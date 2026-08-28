//! The warehouse maintenance thread.
//!
//! # Why this exists
//!
//! Everything a warehouse needs to keep itself in shape already lived in this crate:
//! [`plan_tick`] decides what to compact, [`execute_tick`] merges it, [`commit_tick`] makes
//! the replacement live, [`checkpoint_if_due`] bounds log replay, and [`retire_completed`]
//! removes the inputs a merge replaced once their grace period has run.
//!
//! Five pieces, and no owner. The consequence was not subtle:
//!
//! - Nothing in the server ran maintenance at all. `sankhya-maintenance` was depended on by
//!   its own tests and by the soak, and by nothing that ships.
//! - Every caller therefore had to sequence the five pieces itself and hold the queue of
//!   merges waiting out their grace period. The soak did exactly that, got the last step
//!   wrong, and a run targeting ten gigabytes consumed sixty and died with a full disk.
//!
//! A test getting the sequence wrong is a symptom. The defect is that the sequence was
//! something a caller had to know. Compaction and retirement are the warehouse's business,
//! not its clients' --- so they run here, on a thread the warehouse owns, and a client's
//! only relationship with them is that files quietly get better.
//!
//! # Why retirement is a tick behind
//!
//! A merge commits `Remove` actions for its inputs, which makes them invisible to a reader
//! at the newest version. It does not delete them, deliberately: a reader that listed files
//! just before the merge is still entitled to open what it listed. The inputs outlive their
//! own removal by a stated grace period, and something has to come back afterwards.
//!
//! That is why [`Maintainer`] holds merges in a queue rather than retiring them in the pass
//! that made them. Retiring immediately would delete files out from under readers and make
//! the grace period unobservable --- so the queue is the mechanism, not bookkeeping.

use crate::compaction::{FileStat, PartitionState};
use crate::driver::{
    commit_tick, execute_tick, plan_tick, retire_completed, DriverPolicy, TickReport,
};
use crate::execute::RetentionPolicy;
use crate::schedule::SystemState;
use sankhya_error::{Error, Result};
use sankhya_table::{CompactionOutcome, WriterConfig};
use sankhya_table_delta::live_files;
use sankhya_types::Lsn;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// How the maintenance thread behaves.
#[derive(Clone, Debug)]
pub struct MaintenancePolicy {
    /// How long to wait between ticks.
    ///
    /// A tick is bounded work --- `max_files_per_pass` --- so this is the rate at which the
    /// warehouse catches up, not the duration of a pass.
    pub interval: Duration,
    /// What to compact, and how hard.
    pub driver: DriverPolicy,
    /// When an input a merge replaced may actually be deleted.
    pub retention: RetentionPolicy,
    /// How merged files are written.
    pub writer: WriterConfig,
    /// How much work one tick may do.
    ///
    /// Bounds the pass so maintenance never monopolises the machine: a tick that ran until
    /// the warehouse was perfect would be indistinguishable, from a query's point of view,
    /// from an outage.
    pub duty_cycle_ticks: u64,
}

impl Default for MaintenancePolicy {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(30),
            driver: DriverPolicy::default(),
            retention: RetentionPolicy::default(),
            writer: WriterConfig::default(),
            duty_cycle_ticks: 10_000,
        }
    }
}

/// One warehouse's maintenance, in a form that can be ticked.
///
/// Owns the thing every previous caller had to own by hand: the queue of merges waiting out
/// their grace period. That queue is why retirement is a step behind compaction rather than
/// part of it, and holding it here is the point of this type.
#[derive(Debug)]
pub struct Maintainer {
    policy: MaintenancePolicy,
    /// Merges whose inputs are still on disk, with the tick they were merged at.
    pending: Vec<(CompactionOutcome, u64)>,
    tick: u64,
}

impl Maintainer {
    /// A maintainer for one warehouse.
    #[must_use]
    pub fn new(policy: MaintenancePolicy) -> Self {
        Self {
            policy,
            pending: Vec::new(),
            tick: 0,
        }
    }

    /// How many merges are waiting out their grace period.
    ///
    /// Exposed because it is the difference between "retirement is not working" and
    /// "retirement has not come due yet", and a maintenance thread that cannot tell an
    /// operator which one it is has not explained itself.
    #[must_use]
    pub fn awaiting_retirement(&self) -> usize {
        self.pending.len()
    }

    /// Do one tick of maintenance on one table: compact what is due, retire what has waited.
    ///
    /// # Errors
    ///
    /// Returns an error if the log cannot be read or a commit fails. A merge that fails is
    /// recorded in the report rather than returned as an error: the other partitions are
    /// independent, and one bad partition must not stop maintenance for the warehouse.
    pub fn tick(&mut self, table_root: &Path) -> Result<TickReport> {
        self.tick += 1;
        let tick = self.tick;

        let partitions = partitions_of(table_root)?;
        let mut report = if partitions.is_empty() {
            TickReport::default()
        } else {
            let state = SystemState {
                in_maintenance_window: false,
                queries_running: 0,
                duty_cycle_ticks_remaining: self.policy.duty_cycle_ticks,
            };
            let plan = plan_tick(&partitions, &self.policy.driver, &state);
            let executed = execute_tick(&plan, table_root, tick, self.policy.writer.clone())?;
            if !executed.merged.is_empty() {
                let at = live_files(table_root)
                    .map_err(|e| Error::InvariantViolated(e.to_string()))?;
                let version = at.version.unwrap_or(0).saturating_add(1);
                let now = i64::try_from(tick).unwrap_or(i64::MAX);
                commit_tick(table_root, version, &executed, now)
                    .map_err(|e| Error::InvariantViolated(e.to_string()))?;
            }
            executed
        };

        // Queued rather than retired here. See the module documentation: retiring in the pass
        // that merged deletes files out from under readers that listed before the merge.
        for outcome in &report.merged {
            self.pending.push((outcome.clone(), tick));
        }

        let retired = self.retire_due(tick)?;
        report.bytes_reclaimed = report.bytes_reclaimed.saturating_add(retired.bytes_reclaimed);
        report.files_removed.extend(retired.files_removed);

        // Checkpointing is deliberately *not* done here yet.
        //
        // `checkpoint_if_due` needs the table's `Metadata` --- schema, partition columns ---
        // and there is no reader for it in the log crate today. A checkpoint written from a
        // fabricated default would record a schema the table does not have, which is worse
        // than replaying a long log: one costs startup time, the other tells every external
        // reader something false. Wiring it needs a metadata reader first.

        Ok(report)
    }

    /// Retire the inputs of merges that have served their grace period.
    fn retire_due(&mut self, tick: u64) -> Result<TickReport> {
        // Nothing here pins an older snapshot. When session leases are wired into
        // maintenance this becomes the set of versions they hold; until then the empty set is
        // the honest value, and it is the *conservative* direction only because the grace
        // period is doing the protecting.
        let referenced: BTreeSet<Lsn> = BTreeSet::new();

        let mut due = Vec::new();
        let mut waiting = Vec::new();
        for (outcome, merged_at) in self.pending.drain(..) {
            let age = tick.saturating_sub(merged_at);
            if age >= self.policy.retention.grace_ticks {
                due.push((outcome, age));
            } else {
                waiting.push((outcome, merged_at));
            }
        }
        self.pending = waiting;
        if due.is_empty() {
            return Ok(TickReport::default());
        }
        retire_completed(&due, &referenced, &self.policy.retention)
    }
}

/// Read the log and group the live files into the partitions they sit in.
///
/// Compaction is per-partition by definition: merging across partitions would move rows out
/// of the directory whose value names them.
///
/// # Errors
///
/// Returns an error if the table's log cannot be read.
pub fn partitions_of(table_root: &Path) -> Result<Vec<PartitionState>> {
    let live = live_files(table_root).map_err(|e| Error::InvariantViolated(e.to_string()))?;
    let version = Lsn::new(live.version.unwrap_or(0));
    let table = table_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("table")
        .to_string();

    let mut by_partition: BTreeMap<String, Vec<FileStat>> = BTreeMap::new();
    for file in &live.files {
        let partition = file
            .path
            .rsplit_once('/')
            .map_or_else(String::new, |(directory, _)| directory.to_string());
        by_partition
            .entry(partition)
            .or_default()
            .push(FileStat {
                name: file.path.clone(),
                bytes: file.size,
                rows: file.rows().unwrap_or(0),
                covers_through: version,
            });
    }

    Ok(by_partition
        .into_iter()
        .map(|(partition, files)| PartitionState {
            table: table.clone(),
            partition,
            files,
            ticks_since_write: 0,
        })
        .collect())
}

/// Every table under a warehouse, found by the log that makes a directory a table.
///
/// Discovered rather than configured. A list of tables in a configuration file is a list
/// that goes stale the first time somebody creates one, and the maintenance thread would
/// then quietly not maintain it --- which looks exactly like maintenance working.
#[must_use]
pub fn tables_under(warehouse: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![warehouse.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !entry.metadata().is_ok_and(|m| m.is_dir()) {
                continue;
            }
            if path.join("_delta_log").is_dir() {
                found.push(path);
            } else {
                stack.push(path);
            }
        }
    }
    found.sort();
    found
}

/// A running maintenance thread.
///
/// Dropping this stops the thread. That is deliberate: a maintenance thread outliving the
/// warehouse it maintains would compact a directory nobody is serving from, and on a
/// temporary directory it would race the deletion of the directory itself.
#[derive(Debug)]
pub struct MaintenanceHandle {
    stop: Arc<AtomicBool>,
    ticks: Arc<AtomicU64>,
    reclaimed: Arc<AtomicU64>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl MaintenanceHandle {
    /// How many ticks have run. Used by tests to wait for work rather than sleep for it.
    #[must_use]
    pub fn ticks(&self) -> u64 {
        self.ticks.load(Ordering::Relaxed)
    }

    /// How many bytes retirement has given back.
    #[must_use]
    pub fn bytes_reclaimed(&self) -> u64 {
        self.reclaimed.load(Ordering::Relaxed)
    }

    /// Stop the thread and wait for the tick in progress to finish.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for MaintenanceHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Start maintaining these tables in the background.
///
/// The warehouse maintains itself from here on: clients write, read, and never think about
/// compaction or retirement again. Nothing outside this crate needs to know the order the
/// five steps go in, which is the whole point --- the last caller that had to know got it
/// wrong and filled a disk.
#[must_use]
pub fn spawn(tables: Vec<PathBuf>, policy: MaintenancePolicy) -> MaintenanceHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let ticks = Arc::new(AtomicU64::new(0));
    let reclaimed = Arc::new(AtomicU64::new(0));

    let thread = {
        let stop = Arc::clone(&stop);
        let ticks = Arc::clone(&ticks);
        let reclaimed = Arc::clone(&reclaimed);
        let interval = policy.interval;
        std::thread::Builder::new()
            .name("warehouse-maintenance".to_string())
            .spawn(move || {
                let mut maintainers: BTreeMap<PathBuf, Maintainer> = tables
                    .iter()
                    .map(|table| (table.clone(), Maintainer::new(policy.clone())))
                    .collect();
                while !stop.load(Ordering::Relaxed) {
                    for (table, maintainer) in &mut maintainers {
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        match maintainer.tick(table) {
                            Ok(report) => {
                                reclaimed.fetch_add(report.bytes_reclaimed, Ordering::Relaxed);
                            }
                            // A table that cannot be maintained this tick is not a reason to
                            // stop maintaining the others, or to bring the thread down. The
                            // next tick tries again.
                            Err(_) => continue,
                        }
                    }
                    ticks.fetch_add(1, Ordering::Relaxed);
                    // Woken often so `stop` is honoured promptly rather than after a full
                    // interval: a handle being dropped must not block for thirty seconds.
                    let mut waited = Duration::ZERO;
                    while waited < interval && !stop.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(50));
                        waited += Duration::from_millis(50);
                    }
                }
            })
            .ok()
    };

    MaintenanceHandle {
        stop,
        ticks,
        reclaimed,
        thread,
    }
}

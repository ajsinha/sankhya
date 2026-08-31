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
use crate::orphans::{plan_orphan_cleanup, sweep as sweep_orphans, FileOnDisk, OrphanPolicy};
use crate::schedule::SystemState;
use sankhya_error::{Error, Result};
use sankhya_leases::Leases;
use sankhya_table::{CompactionOutcome, WriterConfig};
use crate::execute::StillReferenced;
use sankhya_table_delta::{live_files, live_files_at};
use sankhya_types::Lsn;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
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
    /// How many ticks between compaction passes.
    ///
    /// One by default: every tick considers every partition, because a pass is already
    /// bounded by `max_files_per_pass` and a partition below the policy's thresholds costs
    /// only the decision not to merge it. Raising this trades promptness for quiet --- a
    /// deployment whose queries are latency-sensitive can compact every fourth tick and
    /// still keep up, provided writes arrive slower than four ticks of merging.
    pub compact_every: u64,
    /// When a file nothing refers to may be removed.
    pub orphans: OrphanPolicy,
    /// How many ticks between orphan sweeps.
    ///
    /// Rarer than compaction --- a hundred and twenty ticks, an hour at the default interval
    /// --- for two reasons. A sweep walks the whole table directory, which costs more the
    /// larger the table and returns nothing most of the time. And what it collects arrives
    /// slowly: an orphan appears only when a merge writes its output and then loses the race
    /// to commit it, which is rare and does not become more urgent by being found sooner.
    ///
    /// The file it collects is a week old by the time the age policy allows removal anyway,
    /// so sweeping more often finds the same files and deletes none of them earlier.
    pub orphan_sweep_every: u64,
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
            compact_every: 1,
            orphans: OrphanPolicy::default(),
            orphan_sweep_every: 120,
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
    /// Merges whose inputs are still on disk, with the tick they were merged at and the lease
    /// epoch at which they stopped being referenced.
    pending: Vec<(CompactionOutcome, u64, u64)>,
    tick: u64,
    /// Readers currently inside the warehouse, when anything is telling us.
    ///
    /// `None` means nobody is publishing lease information --- a maintainer running against a
    /// warehouse no server is serving --- and then the grace period is the only protection, as
    /// it was for every tick before leases existed.
    leases: Option<Arc<Leases>>,
    /// Which tables were cloned from which.
    ///
    /// Empty unless somebody has cloned something, which is the state of every warehouse
    /// today --- and the reason `ADR-0016`'s answer costs nothing until it is used.
    clones: sankhya_clone::Lineages,
}

impl Maintainer {
    /// A maintainer for one warehouse.
    #[must_use]
    pub fn new(policy: MaintenancePolicy) -> Self {
        Self {
            policy,
            pending: Vec::new(),
            tick: 0,
            leases: None,
            clones: sankhya_clone::Lineages::new(),
        }
    }

    /// The same, told which readers are inside.
    ///
    /// With this, a merge's inputs are retired when **every reader that could still name them
    /// has finished** --- not when a number of ticks has gone by. The grace period stays and
    /// becomes a backstop against a leaked announcement rather than the protection itself,
    /// which is the job it is actually good at.
    #[must_use]
    pub fn watching(mut self, leases: Arc<Leases>) -> Self {
        self.leases = Some(leases);
        self
    }

    /// The same, told which tables are clones of which.
    ///
    /// Without this a maintainer reclaims exactly as it always has, which is correct for every
    /// warehouse that has never cloned anything. With it, the two reclamation paths stop
    /// assuming a file belongs to one table --- the premise `ADR-0016` exists because cloning
    /// breaks.
    ///
    /// It is a separate builder rather than a field on the policy because it is **state**, not
    /// configuration: it changes when somebody clones or drops a table, and a reload that reset
    /// it would leave a sweep about to delete a clone's data.
    #[must_use]
    pub fn among(mut self, clones: sankhya_clone::Lineages) -> Self {
        self.clones = clones;
        self
    }

    /// Take a new policy without losing what is already in flight.
    ///
    /// The pending queue and the tick counter survive deliberately. Those are *state*, not
    /// configuration: a merge waiting out its grace period has already happened, and
    /// forgetting it would leave its inputs on disk with nothing left that knows to retire
    /// them. A reload must not leak files as the price of taking effect.
    pub fn reconfigure(&mut self, policy: MaintenancePolicy) {
        self.policy = policy;
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

        let compacting = self.policy.compact_every <= 1 || tick % self.policy.compact_every == 0;
        let partitions = if compacting {
            partitions_of(table_root)?
        } else {
            Vec::new()
        };
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
        //
        // The epoch is taken **now**, at the moment the commit stopped referencing these
        // inputs, and not when retirement is later considered. Taking it later would mark
        // against a clock that has already moved past the readers who are the reason to wait.
        let marked = self.leases.as_ref().map_or(0, |leases| leases.mark());
        for outcome in &report.merged {
            self.pending.push((outcome.clone(), tick, marked));
        }

        let retired = self.retire_due(tick, table_root)?;
        report.bytes_reclaimed = report.bytes_reclaimed.saturating_add(retired.bytes_reclaimed);
        report.files_removed.extend(retired.files_removed);

        // Files nothing refers to, on a slower cycle than compaction.
        if self.policy.orphan_sweep_every > 0 && tick % self.policy.orphan_sweep_every == 0 {
            let swept = self.collect_orphans(table_root);
            report.bytes_reclaimed = report.bytes_reclaimed.saturating_add(swept.bytes_reclaimed);
            report.files_removed.extend(swept.removed);
        }

        // Checkpointing is deliberately *not* done here yet.
        //
        // `checkpoint_if_due` needs the table's `Metadata` --- schema, partition columns ---
        // and there is no reader for it in the log crate today. A checkpoint written from a
        // fabricated default would record a schema the table does not have, which is worse
        // than replaying a long log: one costs startup time, the other tells every external
        // reader something false. Wiring it needs a metadata reader first.

        Ok(report)
    }

    /// Remove files on disk that the log does not refer to.
    ///
    /// # Where an orphan comes from
    ///
    /// Not from compaction succeeding --- the inputs a merge replaces are *retired*, which is
    /// a different mechanism with a different guarantee. An orphan is what a merge leaves
    /// when it writes its output and then **loses the race to commit it**: the writer got the
    /// version first, and maintenance must re-plan against a table state that has changed
    /// rather than take a later version, because the set of files it chose to merge is no
    /// longer the set that is there. The merged file stays on disk with nothing naming it.
    ///
    /// It is invisible to every reader, so it costs only space --- but it costs it once per
    /// lost race, forever, and nothing was collecting it.
    ///
    /// # Why the age threshold does the real work
    ///
    /// A file that is not in the live set is either garbage or a file being written *right
    /// now* by a committer that has not committed yet, and from a directory listing those
    /// two are indistinguishable. The policy's age threshold --- a week by default --- is
    /// what separates them, and it is why this is safe to run beside an active writer.
    ///
    /// Only files old enough to have no plausible commit still in flight are removed.
    fn collect_orphans(&self, table_root: &Path) -> crate::orphans::OrphanReport {
        let Ok(live) = live_files(table_root) else {
            return crate::orphans::OrphanReport::default();
        };
        let named: BTreeSet<String> = live.files.iter().map(|f| f.path.clone()).collect();

        let mut on_disk = Vec::new();
        list_data_files(table_root, table_root, &mut on_disk);
        if on_disk.is_empty() {
            return crate::orphans::OrphanReport::default();
        }

        // Every file a clone of this table still reads. `ADR-0016`: a clone's log does not name
        // the origin's files, it records a version --- so this is resolved from *this table's*
        // log, which the sweep already reads, rather than by normalising another table's paths
        // into this one's naming.
        //
        // Empty for a table nobody has cloned, and then the sweep does exactly what it did
        // before any of this existed.
        let reachable = self.pinned_by_clones(table_root);
        let plan = plan_orphan_cleanup(&on_disk, &named, &reachable, &self.policy.orphans);
        if plan.remove.is_empty() {
            return crate::orphans::OrphanReport::default();
        }
        sweep_orphans(&plan, table_root)
    }

    /// The files a clone of this table still reads, named the way the sweeper names them.
    ///
    /// # Why this reads the origin's own log rather than the clone's
    ///
    /// `ADR-0016`'s Decision 1a: a clone's log names none of the origin's files. It records an
    /// origin and a version, and a read splices the origin's live set at that version with the
    /// clone's own log. So the question *"which of my files does a clone still need?"* is
    /// answered by replaying this table to each pinned version --- which is this table's own
    /// log, in this table's own naming, with nothing to translate.
    ///
    /// A version that cannot be read is **skipped rather than defaulted**, and skipping keeps
    /// files rather than removing them: an unreadable version contributes nothing to the
    /// reachable set, so the sweep falls back to the age threshold that protected everything
    /// before clones existed. That is the safe direction, and it is the only one --- a resolver
    /// that guessed a version's contents would be guessing about what may be deleted.
    fn pinned_by_clones(&self, table_root: &Path) -> BTreeSet<String> {
        if self.clones.is_empty() {
            return BTreeSet::new();
        }
        let Some(table) = table_root.file_name().and_then(|name| name.to_str()) else {
            return BTreeSet::new();
        };

        self.clones
            .pinned_versions(table)
            .into_iter()
            .filter_map(|version| live_files_at(table_root, version).ok())
            .flat_map(|live| live.files.into_iter().map(|file| file.path))
            .collect()
    }

    /// Retire the inputs of merges that no reader can still name.
    ///
    /// **Two conditions, and they protect against different things.** The lease check asks
    /// whether every reader that existed when these inputs stopped being referenced has
    /// finished --- that is the real question, and it is exact. The grace period remains as a
    /// backstop for the case where a lease is leaked and never released, because a registry
    /// with a leak and no backstop reclaims nothing for ever, which is the failure this
    /// warehouse has already met from the other direction.
    fn retire_due(&mut self, tick: u64, table_root: &Path) -> Result<TickReport> {
        // Everything that may still need a file this table would otherwise reclaim, along both
        // axes that do not convert into each other: positions an *arrival buffer* pins, which
        // is a separate mechanism from reader leases and still empty here, and the files a
        // clone reads, which `ADR-0016` made a question about this table's own log.
        let referenced = StillReferenced {
            snapshots: BTreeSet::new(),
            cloned: self
                .pinned_by_clones(table_root)
                .into_iter()
                .map(|path| table_root.join(path))
                .collect(),
        };

        let mut due = Vec::new();
        let mut waiting = Vec::new();
        for (outcome, merged_at, marked) in self.pending.drain(..) {
            let age = tick.saturating_sub(merged_at);
            let old_enough = age >= self.policy.retention.grace_ticks;
            let unreachable = self
                .leases
                .as_ref()
                .is_none_or(|leases| leases.drained(marked));
            if old_enough && unreachable {
                due.push((outcome, age));
            } else {
                waiting.push((outcome, merged_at, marked));
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

/// Every data file under a table, named relative to its root, with its age in seconds.
///
/// The log is skipped rather than aged out. It is not data, and a sweep that reached it
/// would delete the table rather than tidy it.
fn list_data_files(base: &Path, at: &Path, into: &mut Vec<FileOnDisk>) {
    let Ok(entries) = std::fs::read_dir(at) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            if path.file_name().is_some_and(|name| name == "_delta_log") {
                continue;
            }
            list_data_files(base, &path, into);
            continue;
        }
        let Ok(name) = path.strip_prefix(base) else {
            continue;
        };
        // Seconds, which is the unit the default policy is written in: its threshold of
        // 604,800 is a week, chosen to be far beyond any commit still in flight.
        let age = meta
            .modified()
            .ok()
            .and_then(|when| when.elapsed().ok())
            .map_or(0, |since| since.as_secs());
        into.push(FileOnDisk {
            name: name.to_string_lossy().into_owned(),
            bytes: meta.len(),
            age_ticks: age,
        });
    }
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
    /// The policy the thread reads at the top of every cycle.
    ///
    /// Shared rather than copied into the thread, so a setting changed while the server runs
    /// takes effect on the next tick instead of at the next restart. An operator who has to
    /// restart to slow compaction down will not slow compaction down; they will wait for a
    /// window, and the window is when the system is already busy.
    policy: Arc<Mutex<MaintenancePolicy>>,
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

    /// Hand the running thread a new policy.
    ///
    /// Takes effect on the next cycle. The tick in progress finishes under the policy it
    /// started with, which is what makes the change safe rather than merely quick: a pass
    /// that began under one duty-cycle budget is not judged against another halfway through.
    pub fn reconfigure(&self, policy: MaintenancePolicy) {
        if let Ok(mut held) = self.policy.lock() {
            *held = policy;
        }
    }

    /// What the thread is running under right now.
    #[must_use]
    pub fn policy(&self) -> MaintenancePolicy {
        self.policy
            .lock()
            .map_or_else(|_| MaintenancePolicy::default(), |held| held.clone())
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
    spawn_watching(tables, policy, None)
}

/// The same, told which readers are inside the warehouse.
///
/// A server passes the registry its query path pins against, and retirement then waits for
/// readers instead of for a count of ticks. `None` is the honest value for a maintainer with
/// no server beside it, and leaves the grace period doing the protecting as it always did.
#[must_use]
pub fn spawn_watching(
    tables: Vec<PathBuf>,
    policy: MaintenancePolicy,
    leases: Option<Arc<Leases>>,
) -> MaintenanceHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let ticks = Arc::new(AtomicU64::new(0));
    let reclaimed = Arc::new(AtomicU64::new(0));
    let shared = Arc::new(Mutex::new(policy.clone()));

    let thread = {
        let stop = Arc::clone(&stop);
        let ticks = Arc::clone(&ticks);
        let reclaimed = Arc::clone(&reclaimed);
        let shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("warehouse-maintenance".to_string())
            .spawn(move || {
                let mut maintainers: BTreeMap<PathBuf, Maintainer> = tables
                    .iter()
                    .map(|table| {
                        let maintainer = Maintainer::new(policy.clone());
                        let maintainer = match leases.as_ref() {
                            Some(leases) => maintainer.watching(Arc::clone(leases)),
                            None => maintainer,
                        };
                        (table.clone(), maintainer)
                    })
                    .collect();
                while !stop.load(Ordering::Relaxed) {
                    // Read once per cycle, not once at startup. This is the whole of live
                    // reconfiguration: a setting changed while the server runs is picked up
                    // here, and the tick in progress is never re-judged halfway through.
                    let current = shared
                        .lock()
                        .map_or_else(|_| policy.clone(), |held| held.clone());
                    for (table, maintainer) in &mut maintainers {
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        maintainer.reconfigure(current.clone());
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
                    // Re-reading the interval here too, so shortening it takes effect on the
                    // *current* wait rather than after one more of the old one.
                    let mut waited = Duration::ZERO;
                    while !stop.load(Ordering::Relaxed) {
                        let interval = shared
                            .lock()
                            .map_or(current.interval, |held| held.interval);
                        if waited >= interval {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(50));
                        waited += Duration::from_millis(50);
                    }
                }
            })
            .ok()
    };

    MaintenanceHandle {
        policy: shared,
        stop,
        ticks,
        reclaimed,
        thread,
    }
}

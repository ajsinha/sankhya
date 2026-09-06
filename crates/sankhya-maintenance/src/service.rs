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
    /// Versions a named snapshot still reads, by the table root they belong to.
    ///
    /// # Why a map of versions rather than the snapshots themselves
    ///
    /// So this crate needs no notion of what a snapshot *is*. It answers one question ---
    /// *"which versions of this table must survive?"* --- and a caller that knows about
    /// snapshots supplies the answer. A maintenance service that understood snapshots would be
    /// a second place where their expiry is interpreted, and two interpretations of an expiry
    /// eventually disagree about whether a file may be deleted.
    ///
    /// Keyed by **root path** rather than by name, because that is what this already has and it
    /// removes every question about which naming a key is in.
    pinned: std::collections::BTreeMap<PathBuf, Vec<u64>>,
    /// Pins the caller could not establish, each named. Non-empty stops reclamation.
    ///
    /// See [`StillReading::unreadable`]. It is a field rather than a check at one call site
    /// because both deleting paths must honour it, and a rule written at one of them is the
    /// shape `COR-03` already took.
    blind: Vec<String>,
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
            pinned: std::collections::BTreeMap::new(),
            blind: Vec::new(),
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
            // The sequence comes from the **log**, not from this process's tick counter.
            //
            // It was `tick`, which starts at zero every time the server starts. After a
            // restart, tick 1 recomputed a name tick 1 had already used --- and the planner
            // selects any live file under the target size, so a previous output could be
            // chosen as its own input, truncated in place, and then added and removed in one
            // commit, taking the whole merged partition out of the live set.
            //
            // The identical defect had already been found and fixed on the ingest path.
            // Nothing on this path recovered a sequence from anywhere.
            let sequence = next_compaction_sequence(table_root);
            let executed =
                execute_tick(&plan, table_root, sequence, self.policy.writer.clone())?;
            if !executed.merged.is_empty() {
                let at = live_files(table_root)
                    .map_err(|e| Error::InvariantViolated(e.to_string()))?;
                let version = at.version.unwrap_or(0).saturating_add(1);
                // Epoch milliseconds, which is what `deletionTimestamp` means.
                //
                // This was the tick counter --- 1, 2, 3 --- so a removal recorded
                // `deletionTimestamp: 3`, three milliseconds after 1970. Every superseded
                // file was therefore instantly older than any retention interval, and a
                // conformant `VACUUM RETAIN 168 HOURS` run by any other engine would delete
                // the lot immediately: out from under SANKHYA readers holding leases, and
                // reported safe by `DRY RUN` first. The two retention mechanisms could not
                // see each other because one of them was reading a counter as a clock.
                let now = epoch_millis();
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

        // Compaction has run. Reclamation --- and **only** reclamation --- stops here if the
        // pin picture has a hole in it.
        //
        // Compacting under an unknown pin set is safe: a merge adds a file and removes
        // nothing. Deleting under one is the failure, so the two paths that delete are the two
        // paths gated, together, at the one place where the condition is known.
        let (pinned, holes) = self.pins(table_root);
        if !self.blind.is_empty() || !holes.is_empty() {
            report.declined.extend(self.blind.iter().cloned());
            report.declined.extend(holes);
            return Ok(report);
        }

        let retired = self.retire_due(tick, table_root, &pinned)?;
        report.bytes_reclaimed = report.bytes_reclaimed.saturating_add(retired.bytes_reclaimed);
        report.files_removed.extend(retired.files_removed);
        report.presumed_leaked.extend(retired.presumed_leaked);

        // Files nothing refers to, on a slower cycle than compaction.
        if self.policy.orphan_sweep_every > 0 && tick % self.policy.orphan_sweep_every == 0 {
            let swept = self.collect_orphans(table_root, &pinned);
            report.bytes_reclaimed = report.bytes_reclaimed.saturating_add(swept.bytes_reclaimed);
            report.files_removed.extend(swept.removed);
        }

        // Checkpointing, which bounds how far every reader has to replay.
        //
        // `OPS-21`: this was called only from tests, so every log replay in the system --- at
        // startup, on every statement's freshness probe, in `doctor` --- ran from version
        // zero. One `exists()`, one read and one JSON parse per commit, per table, for the
        // life of the warehouse.
        //
        // It used to say it could not be wired because `checkpoint_if_due` needs the table's
        // `Metadata` and the log crate had no reader for it. That reasoning was right and it
        // has expired: `latest_metadata` exists. The point it was protecting still stands ---
        // a checkpoint written from a *fabricated* default records a schema the table does
        // not have and tells every external reader something false --- which is why this
        // reads the metadata and skips the table when there is none to read rather than
        // supplying one.
        match sankhya_table_delta::latest_metadata(table_root) {
            Ok(Some(metadata)) => {
                // A failure to write one is a missed optimisation, never a correctness
                // problem: a checkpoint holds exactly what replay produces. So it is
                // reported through the tick's own error rather than swallowed, and a table
                // that cannot be checkpointed is still compacted, retired and swept.
                crate::driver::checkpoint_if_due(
                    table_root,
                    &metadata,
                    crate::driver::CHECKPOINT_INTERVAL,
                )
                .map_err(|error| Error::InvariantViolated(error.to_string()))?;
            }
            // A table whose log declares no metadata is not one this can summarise. It is
            // also not a table, and `partitions_of` above would already have found nothing.
            Ok(None) => {}
            Err(error) => {
                return Err(Error::InvariantViolated(error.to_string()));
            }
        }

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
    fn collect_orphans(
        &self,
        table_root: &Path,
        reachable: &BTreeSet<String>,
    ) -> crate::orphans::OrphanReport {
        let Ok(live) = live_files(table_root) else {
            return crate::orphans::OrphanReport::default();
        };
        let named: BTreeSet<String> = live.files.iter().map(|f| f.path.clone()).collect();

        let mut on_disk = Vec::new();
        list_data_files(table_root, table_root, &mut on_disk);
        if on_disk.is_empty() {
            return crate::orphans::OrphanReport::default();
        }

        // The pin set is computed once per tick and handed to both deleting paths, which is
        // the whole of the fix for `COR-03`.
        //
        // This path used to compute its own, and computed only the clone half --- while
        // retirement, a hundred lines below, unioned clones *and* snapshots under a comment
        // reading *"two rules disagree eventually, and the one that loses deletes a file
        // somebody is reading"*. This sweep was the second rule.
        //
        // It was not a race, either. Retirement correctly declines a pinned file for ever,
        // which **guarantees** that file crosses this sweep's age threshold: every snapshot
        // older than a week lost its files, on schedule.
        let plan = plan_orphan_cleanup(&on_disk, &named, reachable, &self.policy.orphans);
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
    /// Told, before a tick, what still reads this table.
    ///
    /// Called every cycle rather than at construction, because a clone made or a snapshot taken
    /// while the server runs must be honoured by the next sweep and not by the next restart.
    pub fn told(&mut self, reading: &StillReading, table_root: &Path) {
        self.clones = reading.clones.clone();
        self.blind = reading.unreadable.clone();
        self.pinned = reading
            .snapshots
            .get(table_root)
            .map(|versions| {
                std::collections::BTreeMap::from([(table_root.to_path_buf(), versions.clone())])
            })
            .unwrap_or_default();
    }

    /// The same, told which versions named snapshots still read.
    ///
    /// Supplied per pass rather than at construction, for the reason `among` gives about
    /// clones: it changes when somebody takes, drops or expires a snapshot, and a reload that
    /// reset it would let the next sweep reclaim files a live snapshot protects.
    #[must_use]
    pub fn pinning(mut self, pinned: std::collections::BTreeMap<PathBuf, Vec<u64>>) -> Self {
        self.pinned = pinned;
        self
    }

    /// The files a named snapshot still reads.
    ///
    /// Resolved exactly as a clone's are --- by replaying *this table's own log* to each pinned
    /// version --- because the question is the same question: which of my files does something
    /// else still need? A version that cannot be read is skipped, which keeps files rather than
    /// removing them, and is the only safe direction.
    /// Everything that still reads this table, and everything that could not be established.
    ///
    /// # One question, asked once
    ///
    /// Reclamation has exactly one question --- *does anything still read this?* --- and two
    /// things that can answer yes. They are unioned here, once per tick, and handed to both
    /// paths that delete. Each path computing its own answer is how `COR-03` happened: one of
    /// them read half the union and deleted what the other half was protecting.
    ///
    /// # A version that cannot be read is a hole, not an empty answer
    ///
    /// Skipping it used to be justified as *"the sweep falls back to the age threshold that
    /// protected everything before clones existed"*. That is true of the orphan sweep and
    /// false of retirement, which has no age fallback --- a pinned version that could not be
    /// resolved contributed no paths, so retirement saw no reason to keep the file and removed
    /// it. The two paths had different fallbacks and one paragraph was written for both.
    ///
    /// So it is reported instead, and a hole stops reclamation for the pass.
    fn pins(&self, table_root: &Path) -> (BTreeSet<String>, Vec<String>) {
        let mut pinned = BTreeSet::new();
        let mut holes = Vec::new();

        let mut resolve = |version: u64, who: &str, into: &mut BTreeSet<String>| {
            match live_files_at(table_root, version) {
                Ok(live) => into.extend(live.files.into_iter().map(|file| file.path)),
                Err(error) => holes.push(format!(
                    "{who} pins version {version} of {}, and that version could not be read \
                     ({error}); which files it holds cannot be established",
                    table_root.display()
                )),
            }
        };

        if let Some(versions) = self.pinned.get(table_root) {
            for version in versions {
                resolve(*version, "a snapshot", &mut pinned);
            }
        }

        // `ADR-0016`: a clone's log does not name the origin's files, it records a version ---
        // so this is resolved from *this table's* log rather than by normalising another
        // table's paths into this one's naming.
        if !self.clones.is_empty() {
            if let Some(table) = qualified_name(table_root) {
                for version in self.clones.pinned_versions(&table) {
                    resolve(version, "a clone", &mut pinned);
                }
            }
        }

        (pinned, holes)
    }

    /// Retire the inputs of merges that no reader can still name.
    ///
    /// **Two conditions, and they protect against different things.** The lease check asks
    /// whether every reader that existed when these inputs stopped being referenced has
    /// finished --- that is the real question, and it is exact. The grace period remains as a
    /// backstop for the case where a lease is leaked and never released, because a registry
    /// with a leak and no backstop reclaims nothing for ever, which is the failure this
    /// warehouse has already met from the other direction.
    ///
    /// # The backstop was written as an `and`, and an `and` is not a backstop
    ///
    /// The condition read `old_enough && unreachable`, with both as requirements. A leaked
    /// lease --- a reader that announced itself and whose exit was never announced ---
    /// therefore held every merge input on disk for ever *and* held one entry per merge in
    /// `pending` for ever. The paragraph above described a backstop the code did not have.
    ///
    /// Two thresholds, because one number cannot mean both things. `grace_ticks` is the
    /// **minimum** age and holds even for a reader set that is already drained. `leak_ticks`
    /// is far larger and means *"no lease is legitimately this old"*: past it the registry is
    /// presumed to have leaked and the inputs go.
    ///
    /// It overrides the **lease** check and nothing else. A clone pin and a snapshot pin still
    /// refuse the file, because those are not proxies for anything and there is no timeout at
    /// which they become wrong. And it is reported when it fires, because a backstop firing is
    /// never routine: it says a lease leaked, which is a defect elsewhere that nothing else
    /// here would surface.
    fn retire_due(
        &mut self,
        tick: u64,
        table_root: &Path,
        pinned: &BTreeSet<String>,
    ) -> Result<TickReport> {
        // Everything that may still need a file this table would otherwise reclaim, along both
        // axes that do not convert into each other: positions an *arrival buffer* pins, which
        // is a separate mechanism from reader leases and still empty here, and the files a
        // clone reads, which `ADR-0016` made a question about this table's own log.
        let referenced = StillReferenced {
            snapshots: BTreeSet::new(),
            // Clones and snapshots, unioned by `pins` and resolved once for the tick.
            // Reclamation has exactly **one** question --- *"does anything still read this?"*
            // --- and a second thing that can answer yes is not a second rule. Two rules
            // disagree eventually, and the one that loses deletes a file somebody is reading.
            cloned: pinned.iter().map(|path| table_root.join(path)).collect(),
        };

        let mut due = Vec::new();
        let mut waiting = Vec::new();
        let mut leaked = Vec::new();
        for (outcome, merged_at, marked) in self.pending.drain(..) {
            let age = tick.saturating_sub(merged_at);
            let old_enough = age >= self.policy.retention.grace_ticks;
            let unreachable = self
                .leases
                .as_ref()
                .is_none_or(|leases| leases.drained(marked));
            // `leak_ticks == 0` disables the backstop, for a deployment that would rather fill
            // a disk than ever take the chance. It is not the default, because the default it
            // replaced was a warehouse that reclaimed nothing for ever after one leaked lease
            // and said nothing about why.
            let presumed_leaked = self.policy.retention.leak_ticks > 0
                && age >= self.policy.retention.leak_ticks
                && !unreachable;
            if presumed_leaked {
                leaked.push(format!(
                    "a lease held {} merge input(s) of {} for {age} ticks, past the {}-tick \
                     backstop; the registry has leaked and they were retired anyway",
                    outcome.inputs_retained.len(),
                    table_root.display(),
                    self.policy.retention.leak_ticks,
                ));
            }
            if (old_enough && unreachable) || presumed_leaked {
                due.push((outcome, age));
            } else {
                waiting.push((outcome, merged_at, marked));
            }
        }
        self.pending = waiting;
        if due.is_empty() {
            return Ok(TickReport::default());
        }
        let mut report = retire_completed(&due, &referenced, &self.policy.retention)?;
        report.presumed_leaked = leaked;
        Ok(report)
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
    tables_under_reporting(warehouse).0
}

/// [`tables_under`], and what it could not list while looking.
///
/// `OPS-12` in the one place it decides whether a table is maintained at all. A directory
/// that cannot be listed used to be `continue`, so an unmounted export was a warehouse with
/// no tables in it --- and a maintenance thread with nothing to maintain reports nothing,
/// reclaims nothing, and looks exactly like a warehouse that needs no maintenance.
///
/// Not existing is still silent: a warehouse is created on first use.
#[must_use]
pub fn tables_under_reporting(warehouse: &Path) -> (Vec<PathBuf>, Vec<String>) {
    let mut found = Vec::new();
    let mut unlisted = Vec::new();
    let mut stack = vec![warehouse.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                unlisted.push(format!("{}: {error}", directory.display()));
                continue;
            }
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
    unlisted.sort();
    (found, unlisted)
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
    /// Ticks that declined to reclaim because the pin set could not be established.
    ///
    /// Counted as well as printed, because a log line answers *"did this happen?"* and an
    /// operator looking at a warehouse that is not shrinking needs *"is it still happening?"*.
    declined: Arc<AtomicU64>,
    /// Ticks that failed outright, across every table.
    ///
    /// Separate from `declined`, which is a tick that chose not to reclaim and is the safe
    /// direction. This one is a tick that could not do its work at all, and the two need
    /// different answers from whoever is looking.
    failed: Arc<AtomicU64>,
    /// How many tables are being maintained as of the last cycle.
    maintaining: Arc<std::sync::atomic::AtomicUsize>,
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

    /// How many ticks declined to reclaim because a pin could not be established.
    ///
    /// Non-zero means the warehouse is deliberately not shrinking. That is the safe direction
    /// and it is not a free one: it is a disk filling up while a snapshot document nobody can
    /// read sits in the way, and it needs to be visible without reading a log.
    #[must_use]
    pub fn declined(&self) -> u64 {
        self.declined.load(Ordering::Relaxed)
    }

    /// How many ticks failed outright.
    ///
    /// `OPS-11`. This was not counted and not logged: the tick's error was discarded with
    /// `Err(_) => continue`, so a table whose compaction failed on every tick for ever was
    /// invisible from both a log and a dashboard.
    #[must_use]
    pub fn failed(&self) -> u64 {
        self.failed.load(Ordering::Relaxed)
    }

    /// How many tables this thread is maintaining right now.
    ///
    /// Reported rather than assumed, because the set changes: a table created after the
    /// server started is adopted on the next cycle, and an operator asking "is my new table
    /// being maintained?" has no other way to find out.
    #[must_use]
    pub fn maintaining(&self) -> usize {
        self.maintaining.load(Ordering::Relaxed)
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
/// What still reads a table's files, shared with whoever knows.
///
/// # Why the sweeper is *told* rather than asking
///
/// Because it cannot ask. Clone lineage is read from every table's log and snapshot pins from
/// documents under `_snapshots/`, and both change while the server runs --- somebody clones a
/// table, somebody takes a snapshot, a snapshot expires. A sweeper that read them at startup
/// would honour a warehouse that no longer exists.
///
/// # The defect this exists for
///
/// The running server's sweeper was told **neither**. `Maintainer::among` existed and only a
/// soak test called it, so the maintenance thread ran with an empty lineage set --- and
/// the pin resolver returns nothing for an empty set, which means a clone's files were
/// reclaimable by the sweeper of the table they belong to. The clone would then read a version
/// whose files were gone.
///
/// Found on 2026-09-02 while wiring the same mechanism for snapshots, which is the second thing
/// that answers *"does anything still read this?"*. One question, two answerers, one place they
/// are supplied.
#[derive(Clone, Debug, Default)]
pub struct StillReading {
    /// Which tables are clones of which.
    pub clones: sankhya_clone::Lineages,
    /// Versions a named snapshot still reads, by table root.
    pub snapshots: BTreeMap<PathBuf, Vec<u64>>,
    /// Pins that could not be established, each named.
    ///
    /// # Why an empty list is not the same as an unknown one
    ///
    /// A snapshot document that will not parse, and a snapshot naming a table that resolves to
    /// two, both used to contribute **nothing** --- and contributing nothing is
    /// indistinguishable from *"this pins no files"*. The sweeper then reclaimed exactly the
    /// files it could not see a pin for, which is the unsafe direction and the one that was
    /// taken silently.
    ///
    /// Anything in this list stops reclamation for the pass. Reclamation not running costs
    /// disk and is visible in a report; reclamation running on an unknown pin set costs rows
    /// and is visible when somebody queries.
    pub unreadable: Vec<String>,
}

/// What a maintenance thread maintains, and whether that set can change.
///
/// # Why this is a choice rather than always the warehouse
///
/// `OPS-10`. The table list used to be a `Vec<PathBuf>` taken once at startup, so a table
/// created afterwards was **never maintained** --- its log grew, its small files were never
/// compacted, and nothing said so, because the aggregate reclaimed-bytes figure kept rising
/// from the tables that *were* being maintained. `tables_under`'s own doc warned about a
/// configured list going stale; the caller then froze the discovered one, which is the same
/// staleness arriving a different way.
///
/// A fixed list is still the right thing for a test that wants to maintain one table and
/// nothing else, so it stays --- named, rather than being the only option there is.
#[derive(Debug, Clone)]
pub enum Maintaining {
    /// Exactly these, for as long as the thread runs.
    Only(Vec<PathBuf>),
    /// Every table under this warehouse, re-discovered at the top of every cycle.
    Everything(PathBuf),
}

impl Maintaining {
    /// The tables to maintain this cycle, and what could not be read while looking.
    fn now(&self) -> (Vec<PathBuf>, Vec<String>) {
        match self {
            Self::Only(tables) => (tables.clone(), Vec::new()),
            Self::Everything(warehouse) => tables_under_reporting(warehouse),
        }
    }
}

pub fn spawn_watching(
    tables: Vec<PathBuf>,
    policy: MaintenancePolicy,
    leases: Option<Arc<Leases>>,
) -> MaintenanceHandle {
    spawn_watching_pins(tables, policy, leases, Arc::new(Mutex::new(StillReading::default())))
}

/// Maintain every table under `warehouse`, including the ones that do not exist yet.
#[must_use]
pub fn spawn_over_warehouse(
    warehouse: PathBuf,
    policy: MaintenancePolicy,
    leases: Option<Arc<Leases>>,
    reading: Arc<Mutex<StillReading>>,
) -> MaintenanceHandle {
    spawn_maintaining(Maintaining::Everything(warehouse), policy, leases, reading)
}

/// [`spawn_watching`], told what still reads each table.
///
/// The handle is read **once per cycle**, exactly as the policy is: a clone made or a snapshot
/// taken while the server runs must be honoured by the next sweep, not by the next restart.
#[must_use]
pub fn spawn_watching_pins(
    tables: Vec<PathBuf>,
    policy: MaintenancePolicy,
    leases: Option<Arc<Leases>>,
    reading: Arc<Mutex<StillReading>>,
) -> MaintenanceHandle {
    spawn_maintaining(Maintaining::Only(tables), policy, leases, reading)
}

/// [`spawn_watching_pins`], told whether the set of tables can grow.
#[must_use]
pub fn spawn_maintaining(
    maintaining: Maintaining,
    policy: MaintenancePolicy,
    leases: Option<Arc<Leases>>,
    reading: Arc<Mutex<StillReading>>,
) -> MaintenanceHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let ticks = Arc::new(AtomicU64::new(0));
    let reclaimed = Arc::new(AtomicU64::new(0));
    let declined = Arc::new(AtomicU64::new(0));
    let failed = Arc::new(AtomicU64::new(0));
    let counted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let shared = Arc::new(Mutex::new(policy.clone()));

    let thread = {
        let stop = Arc::clone(&stop);
        let ticks = Arc::clone(&ticks);
        let reclaimed = Arc::clone(&reclaimed);
        let declined = Arc::clone(&declined);
        let failed = Arc::clone(&failed);
        let counted = Arc::clone(&counted);
        let shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("warehouse-maintenance".to_string())
            .spawn(move || {
                // What was last complained about, per table.
                //
                // An unreadable snapshot does not fix itself, so the same complaint is true on
                // every tick --- and a line printed every tick is a line nobody reads. It is
                // printed when the set *changes*, which is once when it starts and once when
                // it stops, and those are the two moments an operator needs.
                let mut complained: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
                // The tick error last reported per table, for the same reason: a table whose
                // compaction fails is a table whose compaction fails every thirty seconds,
                // and a line per tick is a log nobody reads. Reported when it *changes* ---
                // once when it starts and once when it stops.
                let mut failing: BTreeMap<PathBuf, String> = BTreeMap::new();
                // What could not be listed while discovering, for the same hysteresis.
                let mut unlisted: Vec<String> = Vec::new();
                let mut maintainers: BTreeMap<PathBuf, Maintainer> = BTreeMap::new();
                while !stop.load(Ordering::Relaxed) {
                    // The tables, re-read every cycle rather than frozen at startup. A table
                    // created after the server came up used to be maintained by nobody, for
                    // ever, in silence.
                    let (present, could_not_list) = maintaining.now();
                    if could_not_list != unlisted {
                        for why in &could_not_list {
                            tracing::warn!(detail = %why, "maintenance could not list part of the warehouse");
                        }
                        if could_not_list.is_empty() {
                            tracing::info!("maintenance can list the whole warehouse again");
                        }
                        unlisted = could_not_list;
                    }
                    for table in &present {
                        if maintainers.contains_key(table) {
                            continue;
                        }
                        let maintainer = Maintainer::new(policy.clone());
                        let maintainer = match leases.as_ref() {
                            Some(leases) => maintainer.watching(Arc::clone(leases)),
                            None => maintainer,
                        };
                        if !maintainers.is_empty() {
                            // Not on the first cycle, when every table is new and the line
                            // would be one per table on every start.
                            tracing::info!(table = %table.display(), "maintenance adopted a table");
                        }
                        maintainers.insert(table.clone(), maintainer);
                    }
                    // A table that has gone --- dropped, or a warehouse that moved --- stops
                    // being ticked. Kept out of the "adopted" line above so a drop and a
                    // re-create do not read as the same table twice.
                    let gone: Vec<PathBuf> = maintainers
                        .keys()
                        .filter(|table| !present.contains(table))
                        .cloned()
                        .collect();
                    for table in gone {
                        maintainers.remove(&table);
                        complained.remove(&table);
                        failing.remove(&table);
                    }
                    counted.store(maintainers.len(), Ordering::Relaxed);
                    // Read once per cycle, not once at startup. This is the whole of live
                    // reconfiguration: a setting changed while the server runs is picked up
                    // here, and the tick in progress is never re-judged halfway through.
                    let current = shared
                        .lock()
                        .map_or_else(|_| policy.clone(), |held| held.clone());
                    // What still reads these tables, read on the same cadence and for the same
                    // reason. A poisoned handle yields *nothing pinned*, which would be the
                    // unsafe direction --- so it yields the previous cycle's answer instead by
                    // falling back to an empty set only when there has never been one.
                    let reading_now = reading
                        .lock()
                        .map_or_else(|held| held.into_inner().clone(), |held| held.clone());
                    for (table, maintainer) in &mut maintainers {
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        maintainer.reconfigure(current.clone());
                        maintainer.told(&reading_now, table);
                        match maintainer.tick(table) {
                            Ok(report) => {
                                if let Some(previous) = failing.remove(table) {
                                    tracing::info!(
                                        table = %table.display(),
                                        was = %previous,
                                        "maintenance is working for a table again"
                                    );
                                }
                                reclaimed.fetch_add(report.bytes_reclaimed, Ordering::Relaxed);
                                // The complaints, which used to be dropped here.
                                //
                                // `OPS-13` is four ways to delete pinned data and every one of
                                // them ended in a discarded error. A tick that declines to
                                // reclaim looks exactly like a tick with nothing to reclaim
                                // from disk usage alone, so declining silently is the same
                                // failure wearing the safe direction's clothes.
                                if complained.get(table) != Some(&report.declined) {
                                    for why in &report.declined {
                                        tracing::warn!(
                                            table = %table.display(),
                                            detail = %why,
                                            "maintenance is not reclaiming a table"
                                        );
                                    }
                                    if report.declined.is_empty() {
                                        tracing::info!(
                                            table = %table.display(),
                                            "maintenance is reclaiming a table again"
                                        );
                                    }
                                    complained.insert(table.clone(), report.declined.clone());
                                }
                                if !report.declined.is_empty() {
                                    declined.fetch_add(1, Ordering::Relaxed);
                                }
                                // Never routine: it says a lease leaked, which is a defect
                                // somewhere else that nothing else here would surface.
                                for why in &report.presumed_leaked {
                                    // Never routine: it says a lease leaked, which is a
                                    // defect somewhere else that nothing here would surface.
                                    tracing::warn!(detail = %why, "maintenance backstop fired");
                                }
                            }
                            // A table that cannot be maintained this tick is not a reason to
                            // stop maintaining the others, or to bring the thread down. The
                            // next tick tries again.
                            //
                            // `OPS-11`: it also used to be `Err(_) => continue`, so a table
                            // whose compaction failed every thirty seconds was invisible ---
                            // and the aggregate reclaimed-bytes figure kept rising from the
                            // other tables, so the warehouse looked healthy while one of its
                            // tables was not being maintained at all.
                            Err(error) => {
                                let why = error.to_string();
                                if failing.get(table) != Some(&why) {
                                    tracing::warn!(
                                        table = %table.display(),
                                        detail = %why,
                                        "maintenance failed for a table and will try again"
                                    );
                                    failing.insert(table.clone(), why);
                                }
                                failed.fetch_add(1, Ordering::Relaxed);
                                continue;
                            }
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
        declined,
        failed,
        maintaining: counted,
        thread,
    }
}

/// Milliseconds since the epoch, which is what the Delta protocol means by a timestamp.
///
/// Its own function so the two commit paths in this crate cannot disagree about what a
/// timestamp is --- which is exactly what happened when one of them used a tick counter.
fn epoch_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
}

/// One past the highest compaction sequence this table has ever used.
///
/// Recovered from the live set rather than counted in memory, because a counter that starts at
/// zero on every start is a counter that reissues names --- and a reissued data-file name is a
/// file some log still points at.
///
/// Reads the live files, not the directory: a name that has been retired from the table may
/// still exist on disk during its grace period, and reusing it is exactly as wrong as reusing
/// a live one. `live_files` replays the log, which is the record of every name ever committed
/// that still matters.
/// The name this table is known by, in the form the rest of the system records.
///
/// # Why the directory's own name is not it
///
/// `discover` only ever walks `<warehouse>/<schema>/<table>`, so a table's name in a lineage, in
/// a snapshot and in a statement is `schema.table`. A directory carries only its leaf, and the
/// sweeper used the leaf --- so the comparison against a recorded name was false in every
/// deployment, and a clone's files were reclaimed while the clone still read them (`COR-01`).
///
/// A table root with no parent yields the bare leaf, and
/// [`sankhya_clone::Lineages::pinned_versions`] handles the mixed comparison in the direction
/// that keeps files.
fn qualified_name(table_root: &Path) -> Option<String> {
    let table = table_root.file_name().and_then(|name| name.to_str())?;
    match table_root.parent().and_then(Path::file_name).and_then(|name| name.to_str()) {
        Some(schema) => Some(format!("{schema}.{table}")),
        None => Some(table.to_string()),
    }
}

pub fn next_compaction_sequence(table_root: &std::path::Path) -> u64 {
    let Ok(live) = live_files(table_root) else {
        // Unreadable log: refuse to guess low. Zero would collide with the very first
        // compaction this table ever made.
        return u64::from(u32::MAX);
    };
    let highest = live
        .files
        .iter()
        .filter_map(|file| {
            let name = file.path.rsplit('/').next()?;
            let rest = name.strip_prefix("compacted-")?;
            rest.split('-').next()?.parse::<u64>().ok()
        })
        .max()
        .unwrap_or(0);
    highest.saturating_add(1)
}

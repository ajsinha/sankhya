//! The clock. Turning partition state into scheduled, executed maintenance.
//!
//! # What this adds to the pieces beneath it
//!
//! The compaction policy decides *what* is worth merging. The scheduler decides *what
//! may run* against the machine budget. Neither runs anything. This is the loop that
//! joins them, and its whole contribution is two translations and an order:
//!
//! 1. A compaction plan becomes a scheduler [`Job`], which requires deciding its class
//!    and estimating its cost.
//! 2. A scheduled job becomes an executed merge.
//!
//! # Urgency maps onto the class ladder, and this is where it happens
//!
//! An urgent partition is degrading faster than it is being cleared: more files make
//! queries slower, slower queries consume more of the machine, and less capacity
//! remains for compaction. That is self-reinforcing, so it is an **availability**
//! problem and may preempt queries — the architecture's "emergency compaction" — while
//! ordinary compaction is merely a **performance** one and waits its turn.
//!
//! Putting the mapping here rather than in the policy keeps the policy a pure statement
//! about files, and keeps the scheduler ignorant of what compaction is. Only the loop
//! that joins them needs to know both.
//!
//! # A directory listing is not a file set
//!
//! Compaction only ever adds, so between a merge and the retirement of its inputs the
//! directory holds **both** — the same rows twice. Anything that answers "which files
//! belong to this table" by listing the directory is therefore wrong for as long as that
//! window lasts, which is at least a full grace period and by design.
//!
//! Two consequences, and both are easy to get wrong because the naive version works
//! perfectly until the first compaction runs:
//!
//! - **The planner must be given the live set, not a listing.** A planner that re-reads
//!   the directory will happily plan a merge whose inputs include files that a previous
//!   merge already superseded, and the result contains those rows twice.
//! - **A reader must be given the live set too.** Pointing a query engine at the
//!   directory double-counts every row in the window.
//!
//! So the live set is a first-class value the caller carries across ticks. [`apply`]
//! moves it forward: it removes what a merge superseded and adds what the merge wrote,
//! atomically from the caller's point of view. That is the same job a lakehouse table
//! log does, and it is why this system needs one rather than merely liking the idea.
//!
//! # Nothing is removed in the same tick that writes it
//!
//! Retirement runs on a *later* tick, over the outcomes of earlier ones, because its
//! grace period exists precisely to outlast readers that listed files before the merge.
//! A driver that merged and retired in one pass would make the grace period
//! unobservable and the safety it provides theoretical.

use std::collections::BTreeMap;
use sankhya_error::Result;
use sankhya_table::{CompactionOutcome, WriterConfig};
use sankhya_table_delta::{
    commit, latest_checkpoint, live_files, write_checkpoint, Action, AddFile, CheckpointReport,
    CommitError, Metadata as DeltaMetadata, RemoveFile, Version,
};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::compaction::{
    plan_compaction, CompactionPlan, CompactionPolicy, CompactionUrgency, FileStat, PartitionState,
};
use crate::execute::{retire_inputs, run_compaction, RetentionPolicy};
use crate::schedule::{schedule, Class, Job, SystemState};
use sankhya_types::Lsn;

/// How the loop turns file counts into budget.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DriverPolicy {
    pub compaction: CompactionPolicy,
    pub retention: RetentionPolicy,
    /// The clustering each table declares, by table name.
    ///
    /// **Per table, because clustering is a per-table decision.** `CompactionPolicy` carries
    /// one clustering for everything it governs, which is the right shape for thresholds ---
    /// a target file size applies to every table equally --- and the wrong shape for a sort
    /// order, which is a statement about one table's query pattern and means nothing about
    /// another's.
    ///
    /// A table absent from this map falls back to `compaction.clustering`, which is empty by
    /// default. Undeclared means unclustered.
    pub clustering: BTreeMap<String, Vec<String>>,
    /// Bytes a tick of budget is worth.
    ///
    /// Deliberately crude. The estimate exists to stop one enormous merge consuming the
    /// whole duty cycle, not to predict wall-clock time — and an estimate accurate
    /// enough to schedule by would cost more to maintain than it saves.
    pub bytes_per_tick: u64,
}

impl Default for DriverPolicy {
    fn default() -> Self {
        Self {
            compaction: CompactionPolicy::default(),
            retention: RetentionPolicy::default(),
            clustering: BTreeMap::new(),
            bytes_per_tick: 8 * 1024 * 1024,
        }
    }
}

impl DriverPolicy {
    /// The same policy, with each table's declared clustering read from configuration.
    ///
    /// Reads every `table.<schema>.<name>.clustering` under `schema`. A schema nobody has
    /// configured leaves the map empty, which is the same as declaring nothing.
    #[must_use]
    pub fn declaring_clustering(
        mut self,
        config: &sankhya_config::Configuration,
        schema: &str,
    ) -> Self {
        self.clustering = crate::layout::declared(config, schema);
        self
    }

    /// What class of problem this partition has.
    #[must_use]
    pub const fn class_for(urgency: CompactionUrgency) -> Option<Class> {
        match urgency {
            CompactionUrgency::None => None,
            // Self-reinforcing degradation. May preempt queries; audited.
            CompactionUrgency::Urgent => Some(Class::Availability),
            CompactionUrgency::Routine | CompactionUrgency::Elevated => Some(Class::Performance),
        }
    }

    fn estimate_ticks(&self, bytes: u64) -> u64 {
        // At least one tick. A merge that rounds to zero cost would let an unbounded
        // number of them through in a single cycle.
        (bytes / self.bytes_per_tick.max(1)).max(1)
    }

    /// How soon this partition's state becomes visible to someone running a query.
    ///
    /// Coarse on purpose, and monotone in urgency, which is what the scheduler needs:
    /// it orders within a class by this, so only the relative values matter.
    const fn ticks_to_visible(urgency: CompactionUrgency) -> Option<u64> {
        match urgency {
            CompactionUrgency::None => None,
            CompactionUrgency::Urgent => Some(1),
            CompactionUrgency::Elevated => Some(10),
            CompactionUrgency::Routine => Some(100),
        }
    }
}

/// A compaction plan paired with the job that represents it to the scheduler.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PendingCompaction {
    pub plan: CompactionPlan,
    pub job: Job,
    /// The ordering this partition should be written in, from the policy.
    ///
    /// Carried on the pending item rather than looked up at execution time, so the plan
    /// records what it intended: a policy edited between planning and running would
    /// otherwise silently produce a different layout than the plan said.
    pub clustering: Vec<String>,
}

/// What one tick decided.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TickPlan {
    /// In the order they should run.
    pub run: Vec<PendingCompaction>,
    /// Not run this tick, with the reason as the scheduler stated it.
    pub deferred: Vec<(PendingCompaction, String)>,
    /// Whether anything scheduled will take capacity from running queries.
    pub preempts_queries: bool,
}

/// Decide what to compact this tick.
///
/// Pure: it reads partition state and returns decisions, touching nothing.
#[must_use]
pub fn plan_tick(
    partitions: &[PartitionState],
    policy: &DriverPolicy,
    state: &SystemState,
) -> TickPlan {
    let mut pending: Vec<PendingCompaction> = Vec::new();

    for partition in partitions {
        let Some(plan) = plan_compaction(&policy.compaction, partition) else {
            continue;
        };
        let Some(class) = DriverPolicy::class_for(plan.urgency) else {
            continue;
        };

        let job = Job {
            name: format!("compact {}/{}", plan.table, plan.partition),
            class,
            estimated_ticks: policy.estimate_ticks(plan.bytes()),
            // A compaction pass is bounded by max_files_per_pass and leaves its inputs
            // in place, so an interrupted pass loses a bounded amount of work and
            // corrupts nothing. That is what makes it resumable.
            resumable: true,
            ticks_deferred: 0,
            ticks_to_visible: DriverPolicy::ticks_to_visible(plan.urgency),
        };
        pending.push(PendingCompaction {
            plan,
            job,
            // The table's own clustering, falling back to the policy-wide one.
            clustering: policy
                .clustering
                .get(&partition.table)
                .cloned()
                .unwrap_or_else(|| policy.compaction.clustering.clone()),
        });
    }

    let jobs: Vec<Job> = pending.iter().map(|p| p.job.clone()).collect();
    let decided = schedule(&jobs, state);

    // Every scheduled job came from `pending`, so each lookup resolves. `filter_map`
    // rather than an unwrap: if the scheduler ever returned a name that did not, the
    // right outcome is a tick that runs the jobs it can identify, not a panic in the
    // maintenance loop.
    let find = |name: &str| pending.iter().find(|p| p.job.name == name).cloned();

    TickPlan {
        run: decided
            .run
            .iter()
            .filter_map(|s| find(&s.job.name))
            .collect(),
        deferred: decided
            .deferred
            .iter()
            .filter_map(|(job, reason)| Some((find(&job.name)?, reason.to_string())))
            .collect(),
        preempts_queries: decided.preempts_queries(),
    }
}

/// What one tick did.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct TickReport {
    pub merged: Vec<CompactionOutcome>,
    /// Merges that failed, with the reason. A failure does not stop the tick: the other
    /// partitions are independent, and stopping would let one bad partition block
    /// maintenance for the whole warehouse.
    pub failed: Vec<(String, String)>,
    pub files_removed: Vec<PathBuf>,
    pub bytes_reclaimed: u64,
    pub bytes_before: u64,
    pub bytes_after: u64,
}

impl TickReport {
    #[must_use]
    pub fn files_merged(&self) -> usize {
        self.merged.iter().map(|o| o.inputs_retained.len()).sum()
    }
}

/// Run the merges a tick decided on.
///
/// `sequence` distinguishes this tick's outputs from earlier ones. It must increase, or
/// a later merge overwrites an earlier one while readers may still be holding it.
///
/// # Errors
///
/// Never returns an error for a failed merge — those are collected per partition, so
/// one bad partition cannot block maintenance for the rest of the warehouse. An error
/// here means the tick could not be attempted at all.
pub fn execute_tick(
    plan: &TickPlan,
    directory: &Path,
    sequence: u64,
    config: WriterConfig,
) -> Result<TickReport> {
    let mut report = TickReport::default();

    for (index, pending) in plan.run.iter().enumerate() {
        // Into the partition the plan is for, never at the table root.
        //
        // `run_compaction` writes `output_name` relative to `directory`, and the inputs live
        // inside `<partition>/`. A bare name puts the merged file *outside* the partition its
        // rows belong to: the rows' dates no longer match the directory holding them, the
        // add action carries no partition value, and an external reader sees a file that
        // belongs to no partition at all.
        //
        // It also never converges. Each tick removes files from the partition and adds one
        // at the root, so the partition keeps receiving writes and the root accumulates
        // merged files nothing ever touches again.
        // Beside its inputs, whatever directory that is.
        //
        // Derived from the first input's path rather than from `plan.partition`, because a
        // partition *identifier* is not a directory name --- callers legitimately label a
        // partition "all" or "2026-Q3" while the files sit somewhere else entirely. Using
        // the label built a path to a directory that does not exist, the write failed, and
        // the merge was reported as failed rather than done.
        //
        // The inputs' directory is the only thing that is always true, and it is what the
        // output must join: a merge that lands outside the partition its rows belong to
        // leaves their dates disagreeing with the directory holding them.
        let directory_of_inputs = pending
            .plan
            .inputs
            .first()
            .and_then(|file| std::path::Path::new(&file.name).parent())
            .map(|parent| parent.to_string_lossy().into_owned())
            .unwrap_or_default();
        let name = if directory_of_inputs.is_empty() {
            format!("compacted-{sequence:06}-{index:04}.parquet")
        } else {
            format!("{directory_of_inputs}/compacted-{sequence:06}-{index:04}.parquet")
        };
        match run_compaction(&pending.plan, directory, &name, config, &pending.clustering) {
            Ok(outcome) => {
                report.bytes_before = report.bytes_before.saturating_add(outcome.bytes_before);
                report.bytes_after = report.bytes_after.saturating_add(outcome.bytes);
                report.merged.push(outcome);
            }
            Err(e) => report
                .failed
                .push((pending.job.name.clone(), e.to_string())),
        }
    }

    Ok(report)
}

/// Move the live set forward over what a tick merged.
///
/// Removes each merge's inputs and adds its output. The inputs remain **on disk** —
/// retirement is a separate decision — but they are no longer part of the table, which
/// is the distinction a directory listing cannot express.
///
/// Files the tick did not touch are left exactly as they were, including their declared
/// coverage: rewriting coverage the merge did not change would be a chance to get it
/// wrong for no benefit.
pub fn apply(live: &mut Vec<FileStat>, report: &TickReport, table_root: &Path) {
    let relative = |p: &Path| {
        p.strip_prefix(table_root)
            .unwrap_or(p)
            .to_string_lossy()
            .into_owned()
    };
    for outcome in &report.merged {
        // Matched on the path relative to the table root, not the bare file name.
        //
        // The bare name was indistinguishable from correct while every file sat at the
        // table root. Once tables are partitioned, `f.name` is
        // `sank_data_date=2026-08-28/00000000.parquet` and the bare name is
        // `00000000.parquet`, so nothing ever matched: the live set kept every input it was
        // told had been merged, and grew by one phantom entry per tick.
        let superseded: BTreeSet<String> =
            outcome.inputs_retained.iter().map(|p| relative(p)).collect();

        live.retain(|f| !superseded.contains(&f.name));

        live.push(FileStat {
            name: relative(&outcome.output),
            bytes: outcome.bytes,
            rows: outcome.rows,
            covers_through: outcome.covers_through,
        });
    }
}

/// Commit what a tick merged to the table log, as one atomic version.
///
/// Every merge in the tick becomes one `add` and one `remove` per input, in a single
/// commit. Splitting them across versions would publish a state in which the same rows
/// are live twice — briefly, and briefly is enough for a reader to see it.
///
/// The removals declare `dataChange: false`: compaction rewrites files without changing
/// rows, and a reader streaming changes would otherwise see every compacted row as a
/// deletion followed by a re-insertion.
///
/// # Errors
///
/// Returns an error if the version is already taken — the loser must re-read the log and
/// rebase, because a compaction's decisions are exactly "which files to merge" and those
/// were made against a state that no longer exists — or if the log cannot be written.
pub fn commit_tick(
    table_root: &Path,
    version: Version,
    report: &TickReport,
    now: i64,
) -> std::result::Result<Version, CommitError> {
    let mut actions = Vec::new();

    for outcome in &report.merged {
        // Relative to the table root, which is what the Delta protocol means by a file
        // path --- not the bare file name.
        //
        // It was the bare name. That was indistinguishable from correct while every file sat
        // at the table root, and became wrong the moment tables were partitioned: a merge
        // inside `sank_data_date=2026-08-28/` was logged as a file at the root, so the add
        // pointed at a path that does not exist and the removals did not match the
        // partitioned inputs they were meant to retire. The partition then kept every input
        // *and* gained a phantom entry per tick.
        let name = |p: &Path| {
            p.strip_prefix(table_root)
                .unwrap_or(p)
                .to_string_lossy()
                .into_owned()
        };

        // Everything the merge learned, written where other engines can read it too.
        // Statistics that live only in this process are lost on restart and are useless
        // to anyone else reading the table.
        let statistics =
            sankhya_table_delta::from_column_stats(outcome.rows, &outcome.column_stats);
        actions.push(Action::Add(AddFile::with_statistics(
            name(&outcome.output),
            outcome.bytes,
            now,
            &statistics,
        )));
        for input in &outcome.inputs_retained {
            actions.push(Action::Remove(RemoveFile::rewritten(name(input), now)));
        }
    }

    commit(table_root, version, &actions)
}

/// How often a table's log is collapsed into a checkpoint.
///
/// Ten commits is the protocol's usual convention and is not arbitrary: the checkpoint
/// costs one write proportional to the *live file count*, while skipping it costs every
/// cold reader one file open per commit. Ten keeps the write rare and the replay short.
pub const CHECKPOINT_INTERVAL: u64 = 10;

/// Write a checkpoint if enough commits have accumulated since the last one.
///
/// Returns `None` when none was due. A checkpoint is derived state — it holds exactly
/// what replay produces — so failing to write one is a missed optimisation and never a
/// correctness problem. That is why this is a maintenance job rather than part of
/// committing: a commit that had to checkpoint could fail for a reason that does not
/// matter.
///
/// # Errors
///
/// Returns an error if a checkpoint was due and could not be written. The log is
/// untouched either way.
pub fn checkpoint_if_due(
    table_root: &Path,
    metadata: &DeltaMetadata,
    interval: u64,
) -> std::result::Result<Option<CheckpointReport>, CommitError> {
    let live = live_files(table_root)?;
    let Some(version) = live.version else {
        return Ok(None);
    };

    // A table with no checkpoint is measured from version zero rather than treated as
    // infinitely overdue. Otherwise a brand-new table checkpoints on its first commit,
    // writing a file to summarise a log of one — which costs a write and saves nobody
    // anything.
    let since = version.saturating_sub(latest_checkpoint(table_root).unwrap_or(0));
    if since < interval {
        return Ok(None);
    }

    write_checkpoint(table_root, &live, metadata, 1, 2).map(Some)
}

/// Retire inputs from merges completed on earlier ticks.
///
/// Separate from [`execute_tick`] and normally called with outcomes from *previous*
/// ticks. Retiring in the same pass that merged would make the grace period
/// unobservable — see the module documentation.
///
/// # Errors
///
/// Returns an error only if a replacement cannot be verified, which means a compaction
/// did not actually happen and nothing may be removed.
pub fn retire_completed(
    outcomes: &[(CompactionOutcome, u64)],
    referenced: &BTreeSet<Lsn>,
    policy: &RetentionPolicy,
) -> Result<TickReport> {
    let mut report = TickReport::default();

    for (outcome, ticks_since_merge) in outcomes {
        let retirement = retire_inputs(outcome, referenced, *ticks_since_merge, policy)?;
        report.bytes_reclaimed = report
            .bytes_reclaimed
            .saturating_add(retirement.bytes_reclaimed);
        report.files_removed.extend(retirement.removed);
    }

    Ok(report)
}

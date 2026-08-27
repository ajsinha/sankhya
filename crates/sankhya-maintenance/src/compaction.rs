//! Compaction policy.

use sankhya_types::Lsn;
use std::fmt;

/// One file's relevant properties.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FileStat {
    pub name: String,
    pub bytes: u64,
    pub rows: u64,
    /// The position this file is known to contain.
    ///
    /// Compaction must preserve coverage exactly, or the read path's splice would see
    /// a gap where a rewrite occurred.
    pub covers_through: Lsn,
}

/// What a partition currently looks like.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PartitionState {
    pub table: String,
    pub partition: String,
    pub files: Vec<FileStat>,
    /// How long since anything was written here.
    ///
    /// Used to decide whether a partition has settled. Re-clustering a partition still
    /// receiving writes simply means doing it again tomorrow.
    pub ticks_since_write: u64,
}

impl PartitionState {
    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.bytes).sum()
    }

    #[must_use]
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// The median file size, which describes the shape better than the mean.
    ///
    /// A mean is dragged upward by one large file and would report a partition of
    /// mostly-tiny files as healthy.
    #[must_use]
    pub fn median_bytes(&self) -> u64 {
        if self.files.is_empty() {
            return 0;
        }
        let mut sizes: Vec<u64> = self.files.iter().map(|f| f.bytes).collect();
        sizes.sort_unstable();
        sizes.get(sizes.len() / 2).copied().unwrap_or(0)
    }

    /// The furthest position any file here covers.
    #[must_use]
    pub fn covers_through(&self) -> Lsn {
        self.files
            .iter()
            .map(|f| f.covers_through)
            .max()
            .unwrap_or(Lsn::ZERO)
    }
}

/// How badly a partition needs attention.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum CompactionUrgency {
    /// Nothing to do.
    None,
    /// Worth doing when there is capacity.
    Routine,
    /// Query planning is measurably suffering.
    Elevated,
    /// The partition is degrading faster than it is being cleared.
    ///
    /// Left alone this is self-reinforcing: more files make queries slower, slower
    /// queries consume more of the machine, and less capacity remains for compaction.
    Urgent,
}

impl fmt::Display for CompactionUrgency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::None => "none",
            Self::Routine => "routine",
            Self::Elevated => "elevated",
            Self::Urgent => "urgent",
        })
    }
}

/// Thresholds.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CompactionPolicy {
    /// The size a compacted file aims for.
    ///
    /// Large enough that per-file overhead amortizes, small enough to preserve scan
    /// parallelism and pruning granularity.
    pub target_bytes: u64,
    /// Below this, a file is small enough to be worth merging.
    pub small_file_bytes: u64,
    /// File count at which compaction becomes worthwhile.
    pub routine_file_count: usize,
    /// File count at which planning is measurably suffering.
    pub elevated_file_count: usize,
    /// File count at which the partition is degrading faster than it is cleared.
    pub urgent_file_count: usize,
    /// Ticks of quiet after which a partition is considered settled.
    pub settle_ticks: u64,
    /// Columns a settled partition is ordered by, if any.
    ///
    /// Declared rather than inferred. A key chosen from observed queries would change
    /// under a workload shift and rewrite the whole table to follow it, which costs more
    /// than the ordering is worth — and the architecture's rule is to prefer the
    /// reversible decision, which means changing this deliberately at the next
    /// compaction rather than automatically.
    pub clustering: Vec<String>,
    /// Most files to merge in one pass.
    ///
    /// Bounded so a single compaction cannot monopolise the maintenance budget, and so
    /// an interrupted pass loses a bounded amount of work.
    pub max_files_per_pass: usize,
}

impl Default for CompactionPolicy {
    fn default() -> Self {
        Self {
            target_bytes: 256 * 1024 * 1024,
            small_file_bytes: 64 * 1024 * 1024,
            routine_file_count: 8,
            elevated_file_count: 32,
            urgent_file_count: 128,
            settle_ticks: 60,
            clustering: Vec::new(),
            max_files_per_pass: 32,
        }
    }
}

/// What to do about a partition.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CompactionPlan {
    pub table: String,
    pub partition: String,
    pub urgency: CompactionUrgency,
    /// Files to merge, oldest first.
    pub inputs: Vec<FileStat>,
    /// Coverage the merged output must declare.
    ///
    /// Preserved exactly. A rewrite that narrowed coverage would open a gap in the read
    /// path; one that widened it would claim data the file does not hold.
    pub covers_through: Lsn,
    /// Whether the partition has settled enough to be worth sorting as well as merging.
    pub settled: bool,
    pub reason: String,
}

impl CompactionPlan {
    #[must_use]
    pub fn rows(&self) -> u64 {
        self.inputs.iter().map(|f| f.rows).sum()
    }

    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.inputs.iter().map(|f| f.bytes).sum()
    }

    /// Files this pass would replace.
    #[must_use]
    pub fn input_names(&self) -> Vec<&str> {
        self.inputs.iter().map(|f| f.name.as_str()).collect()
    }
}

/// Decide whether and how to compact a partition.
///
/// Pure: it takes the observed state rather than listing a directory, so every policy
/// decision is testable without touching storage.
///
/// Returns `None` when there is nothing worth doing.
#[must_use]
pub fn plan_compaction(
    policy: &CompactionPolicy,
    state: &PartitionState,
) -> Option<CompactionPlan> {
    let count = state.file_count();
    if count < 2 {
        // One file cannot be merged with anything, and zero is nothing.
        return None;
    }

    let small: Vec<&FileStat> = state
        .files
        .iter()
        .filter(|f| f.bytes < policy.small_file_bytes)
        .collect();

    let urgency = if count >= policy.urgent_file_count {
        CompactionUrgency::Urgent
    } else if count >= policy.elevated_file_count {
        CompactionUrgency::Elevated
    } else if count >= policy.routine_file_count
        || (small.len() >= 2 && state.median_bytes() < policy.small_file_bytes / 4)
    {
        CompactionUrgency::Routine
    } else {
        CompactionUrgency::None
    };

    if urgency == CompactionUrgency::None {
        return None;
    }

    // Merge the smallest files first: they carry the most per-file overhead per byte,
    // so they yield the largest planning improvement for the least rewriting.
    let mut candidates: Vec<FileStat> = state.files.clone();
    candidates.sort_by_key(|f| (f.bytes, f.name.clone()));

    let mut inputs = Vec::new();
    let mut accumulated = 0u64;
    for file in candidates {
        if inputs.len() >= policy.max_files_per_pass {
            break;
        }
        // Stop once merging further would overshoot the target: a file larger than the
        // target is worse than two files near it.
        if !inputs.is_empty() && accumulated.saturating_add(file.bytes) > policy.target_bytes {
            break;
        }
        accumulated = accumulated.saturating_add(file.bytes);
        inputs.push(file);
    }

    if inputs.len() < 2 {
        // Nothing can usefully be merged — every file is already at or above target.
        return None;
    }

    let covers_through = inputs
        .iter()
        .map(|f| f.covers_through)
        .max()
        .unwrap_or(Lsn::ZERO);
    let settled = state.ticks_since_write >= policy.settle_ticks;

    Some(CompactionPlan {
        table: state.table.clone(),
        partition: state.partition.clone(),
        urgency,
        reason: format!(
            "{count} files ({} below the small-file threshold), median {} bytes; \
             merging {} of them into approximately {accumulated} bytes",
            small.len(),
            state.median_bytes(),
            inputs.len()
        ),
        inputs,
        covers_through,
        settled,
    })
}

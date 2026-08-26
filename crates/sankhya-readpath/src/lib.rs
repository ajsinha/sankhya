//! Answering a query from several tiers at once.
//!
//! # What this crate is responsible for
//!
//! The splice planner proves that a set of tiers covers a query's span exactly once.
//! This crate turns that proof into an executable query, and its whole job is to not
//! lose the proof on the way.
//!
//! Two things make that non-trivial.
//!
//! **A selected tier may reach past the target.** The planner is greedy from the origin
//! and picks the tier reaching furthest, so a published tier covering `(0, 100]`
//! is a legitimate choice for a query pinned at 60. Coverage is a statement about
//! what a tier *contains*, not about what a query should *see*. So every tier is
//! filtered to `_sankhya_commit_lsn <= target` at the scan, without exception — a tier
//! that "obviously" cannot overshoot is filtered anyway, because the cost is a
//! predicate the engine pushes down and the alternative is a correctness argument that
//! has to be re-made every time a tier is added.
//!
//! **Only selected tiers may be read.** A tier the planner rejected is rejected because
//! reading it would double-count. Registering every available tier and letting the
//! query sort it out would discard the proof entirely.
//!
//! # Refusing rather than approximating
//!
//! If the tiers do not cover the span, the query is refused. This is the read-path half
//! of INV-1: an incomplete answer that looks complete is the worst outcome the system
//! can produce, because nothing downstream can detect it. A refusal is loud, immediate
//! and attributable.

#![doc(html_root_url = "https://docs.rs/sankhya-readpath")]

use datafusion::datasource::MemTable;
use datafusion::prelude::{ParquetReadOptions, SessionContext};
use sankhya_plan::{plan_splice, Splice, SpliceError, TierRef};
use sankhya_table_memory::{ArrivalBuffer, ScanError};
use sankhya_types::{Lsn, LsnRange};
use std::sync::Arc;

/// The commit-position column every tier carries, and the one the target filters on.
pub(crate) const COMMIT_LSN: &str = "_sankhya_commit_lsn";

mod budgeted;
mod merge;
mod predicate;
mod provider;

pub use budgeted::{BudgetedExec, Clock};
pub use merge::{Capability, CapabilityError, ResolvedTable};
pub use predicate::extract;
pub use provider::{resolve, resolve_cached, LoggedFile, SankhyaTable};

/// Published files for one table, and what they cover.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PublishedTier {
    /// The files that are **live**, named individually.
    ///
    /// Not a directory, and this is the whole point. Compaction only ever adds, so
    /// between a merge and the retirement of its inputs the directory holds both the
    /// inputs and the file that replaced them — the same rows twice, by design, for at
    /// least a full grace period. A reader pointed at the directory double-counts every
    /// merged row for that entire window.
    ///
    /// The naive version works perfectly until the first compaction runs, which is what
    /// makes it worth stating here rather than leaving to the caller.
    pub files: Vec<String>,
    pub coverage: LsnRange,
}

impl PublishedTier {
    #[must_use]
    pub fn new(files: Vec<String>, coverage: LsnRange) -> Self {
        Self { files, coverage }
    }

    /// The tier a table's log says is live.
    ///
    /// This is how a reader should normally obtain the file set. Paths in the log are
    /// relative to the table root, as the protocol requires, and are resolved here so
    /// the caller never has to know that.
    ///
    /// # Errors
    ///
    /// Returns an error if the log cannot be read or is malformed. Refusing is correct:
    /// a log that cannot be replayed means the file set is unknown, and guessing it from
    /// the directory is precisely the mistake this type exists to prevent.
    pub fn from_log(
        table_root: &std::path::Path,
        coverage: LsnRange,
    ) -> Result<Self, sankhya_table_delta::CommitError> {
        let live = sankhya_table_delta::live_files(table_root)?;
        Ok(Self {
            files: live
                .files
                .iter()
                .map(|f| table_root.join(&f.path).to_string_lossy().into_owned())
                .collect(),
            coverage,
        })
    }
}

/// The tiers available to answer a query.
#[derive(Clone, Copy, Debug)]
pub struct TierSet<'a> {
    pub published: Option<&'a PublishedTier>,
    pub arrival: Option<&'a ArrivalBuffer>,
}

impl<'a> TierSet<'a> {
    #[must_use]
    pub const fn new(
        published: Option<&'a PublishedTier>,
        arrival: Option<&'a ArrivalBuffer>,
    ) -> Self {
        Self { published, arrival }
    }
}

/// Why a query could not be answered.
#[derive(Debug)]
pub enum ReadError {
    /// The tiers do not cover the span. The query is refused rather than answered
    /// partially.
    Splice(SpliceError),
    /// No tier offered any coverage at all.
    NoTiers,
    Arrival(ScanError),
    /// The table log could not be read or replayed.
    ///
    /// Refusing is correct: a log that cannot be replayed means the file set is
    /// unknown, and falling back to a directory listing is precisely the mistake the
    /// log exists to prevent.
    Log(String),
    Engine(String),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Splice(e) => write!(f, "{e}"),
            Self::NoTiers => write!(
                f,
                "no tier offered coverage, so there is nothing to answer from; this is \
                 an empty table or an unregistered one, not a failure to plan"
            ),
            Self::Arrival(e) => write!(f, "{e}"),
            Self::Log(e) => write!(
                f,
                "the table log could not be replayed, so the file set is unknown: {e}"
            ),
            Self::Engine(e) => write!(f, "the query engine rejected the spliced plan: {e}"),
        }
    }
}

impl std::error::Error for ReadError {}

impl From<SpliceError> for ReadError {
    fn from(e: SpliceError) -> Self {
        Self::Splice(e)
    }
}

impl From<sankhya_table_delta::CommitError> for ReadError {
    fn from(e: sankhya_table_delta::CommitError) -> Self {
        Self::Log(e.to_string())
    }
}

impl From<ScanError> for ReadError {
    fn from(e: ScanError) -> Self {
        Self::Arrival(e)
    }
}

/// Register `name` as a view over exactly the tiers that cover `target`.
///
/// Returns the [`Splice`] so the caller can attach provenance to the response —
/// which tiers answered, over which intervals. That is returned on every read rather
/// than on request, because "which tier answered me" is a question an auditor
/// eventually asks and the system should not have to reconstruct it afterwards.
///
/// # Errors
///
/// Refuses if the tiers do not cover `(0, target]` exactly once, if the arrival tier
/// cannot be scanned, or if the engine rejects the resulting plan.
pub async fn register_spliced(
    ctx: &SessionContext,
    name: &str,
    tiers: &TierSet<'_>,
    target: Lsn,
) -> Result<Splice, ReadError> {
    let mut offered: Vec<TierRef> = Vec::new();
    if let Some(published) = tiers.published {
        offered.push(TierRef::new("published", published.coverage));
    }
    if let Some(arrival) = tiers.arrival {
        if let Some(coverage) = arrival.coverage() {
            offered.push(TierRef::new("arrival", coverage));
        }
    }
    if offered.is_empty() {
        return Err(ReadError::NoTiers);
    }

    // The proof. Everything below reads only what this selected.
    let splice = plan_splice(&offered, target)?;

    let mut parts: Vec<String> = Vec::new();

    for tier in &splice.tiers {
        match tier.name {
            "published" => {
                let published = tiers.published.expect("selected, therefore offered");
                if published.files.is_empty() {
                    return Err(ReadError::Engine(
                        "the published tier declared coverage but named no files".to_string(),
                    ));
                }
                let table = format!("{name}__published");
                let frame = ctx
                    .read_parquet(published.files.clone(), ParquetReadOptions::default())
                    .await
                    .map_err(|e| ReadError::Engine(e.to_string()))?;
                ctx.register_table(&table, frame.into_view())
                    .map_err(|e| ReadError::Engine(e.to_string()))?;
                parts.push(table);
            }
            "arrival" => {
                let arrival = tiers.arrival.expect("selected, therefore offered");
                let batches = arrival.scan(target)?;
                let table = format!("{name}__arrival");
                let provider = MemTable::try_new(arrival.schema(), vec![batches])
                    .map_err(|e| ReadError::Engine(e.to_string()))?;
                ctx.register_table(&table, Arc::new(provider))
                    .map_err(|e| ReadError::Engine(e.to_string()))?;
                parts.push(table);
            }
            other => {
                return Err(ReadError::Engine(format!(
                    "the planner selected a tier named {other}, which this read path \
                     does not know how to read"
                )))
            }
        }
    }

    // Every tier is filtered to the target, including ones that cannot overshoot. See
    // the module documentation: a uniform predicate is cheaper to keep correct than a
    // per-tier argument about why the filter is unnecessary.
    let union = parts
        .iter()
        .map(|t| format!("SELECT * FROM {t} WHERE {COMMIT_LSN} <= {}", target.get()))
        .collect::<Vec<_>>()
        .join(" UNION ALL ");

    ctx.sql(&format!("CREATE OR REPLACE VIEW {name} AS {union}"))
        .await
        .map_err(|e| ReadError::Engine(e.to_string()))?;

    Ok(splice)
}

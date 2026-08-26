//! Every seam in the system, and nothing else.
//!
//! This crate contains trait definitions and the small value types they exchange. It
//! has no implementations, no IO and no async runtime beyond the attribute macro that
//! makes async traits expressible.
//!
//! # Why the seams live in one place
//!
//! Each trait here is a point at which a deterministic fake can be substituted for a
//! real external system. That is what allows the hardest logic in the project — the
//! apply path, the read-path planner, the policy engine — to be tested exhaustively in
//! milliseconds rather than against a live database in minutes.
//!
//! Two seams are load-bearing beyond testing convenience. [`Clock`] and [`IdGen`] are
//! injected everywhere, and enforced by lint, because the determinism guarantee is
//! impossible without them: a wall-clock read or a random identifier anywhere in the
//! commit path makes byte-identical reproduction unachievable.

#![doc(html_root_url = "https://docs.rs/sankhya-ports")]

use sankhya_error::Result;
use sankhya_types::{Lsn, LsnRange, TableId, TableVersion, TenantId, Timestamp};

/// The source of time.
///
/// Never read the wall clock directly. A timestamp that leaks into committed metadata
/// from an ambient clock is the single most common reason a system cannot reproduce
/// its own output byte-for-byte.
pub trait Clock: Send + Sync + std::fmt::Debug {
    /// Civil time, for recording.
    fn now(&self) -> Timestamp;
    /// Monotonic time, for measuring. Never for recording — it has no meaning across
    /// process restarts.
    fn monotonic(&self) -> std::time::Duration;
}

/// The source of identity.
///
/// Injected for the same reason as [`Clock`]: a randomly generated identifier in a
/// commit path defeats reproducibility.
pub trait IdGen: Send + Sync + std::fmt::Debug {
    fn next(&self) -> uuid_like::Id;
}

/// A minimal identifier type, so this crate need not depend on a UUID implementation.
pub mod uuid_like {
    /// A 128-bit opaque identifier.
    #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
    pub struct Id(pub u128);
}

/// What a table format can do.
///
/// Consumers branch on these rather than on a format's identity, which is what keeps
/// the abstraction honest as implementations diverge.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Capabilities {
    /// Can express a row-level deletion without rewriting whole files.
    pub row_level_deletes: bool,
    /// Supports durable named references to a snapshot.
    pub named_refs: bool,
    /// Can change the partitioning of an existing table without a full rewrite.
    pub partition_evolution: bool,
    /// Commits are serialised by the storage layer's own conditional write.
    pub conditional_commit: bool,
    /// Can rewrite files to improve layout without changing content.
    pub compaction: bool,
}

/// A resolved, immutable view of a table.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Snapshot {
    pub table: TableId,
    pub version: TableVersion,
    /// The log position this snapshot is known to contain.
    ///
    /// This is the field the read-path splice depends on. Without it a tier cannot
    /// declare its coverage, and without coverage the splice cannot be proven free of
    /// gaps and double-counting.
    pub covers_through: Lsn,
    pub committed_at: Timestamp,
}

/// How a caller wants to address a version.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AsOf {
    /// The most recent committed version.
    Latest,
    /// A specific version.
    Version(TableVersion),
    /// The last version committed at or before a wall-clock instant.
    ///
    /// Always resolved through the snapshot registry, never by inspecting file
    /// modification times — that is a well-known source of subtly wrong answers.
    Timestamp(Timestamp),
    /// The last version covering a log position.
    Lsn(Lsn),
}

/// Why a commit did not succeed.
///
/// Typed rather than a single error because the correct response differs: a
/// disjoint concurrent write can be rebased and retried, while a conflict on the
/// inputs themselves must abort and reschedule. Collapsing them would make aggressive
/// compaction unsafe.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CommitConflict {
    /// Another writer committed disjoint changes. Rebase onto the new base and retry.
    Retryable { rebase_onto: TableVersion },
    /// The files this commit was built from are gone. Abort and reschedule.
    Fatal { reason: String },
}

/// A table format.
#[async_trait::async_trait]
pub trait TableFormat: Send + Sync + std::fmt::Debug {
    fn capabilities(&self) -> Capabilities;

    /// Resolve a version.
    async fn snapshot(&self, table: TableId, at: AsOf) -> Result<Snapshot>;

    /// Append rows. Idempotent under the given key: applying the same key twice is a
    /// no-op, which is what turns at-least-once delivery into exactly-once effect.
    async fn append(
        &self,
        table: TableId,
        rows: RowSet,
        idempotency_key: &str,
    ) -> Result<TableVersion>;

    /// Rewrite files to improve layout without changing content.
    ///
    /// Compaction only ever *adds*; a separate operation removes. Separating them is
    /// what makes aggressive compaction safe for in-flight readers.
    async fn compact(&self, table: TableId) -> Result<CompactionReport>;

    /// Remove files no longer referenced by any retained version and not covered by a
    /// live lease.
    async fn expire(&self, table: TableId, retain_through: Timestamp) -> Result<ExpiryReport>;
}

/// An opaque batch of rows crossing the format boundary.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct RowSet {
    pub row_count: u64,
    pub byte_estimate: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct CompactionReport {
    pub files_before: u64,
    pub files_after: u64,
    pub bytes_rewritten: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ExpiryReport {
    pub files_removed: u64,
    pub bytes_reclaimed: u64,
    /// Files that could not be removed because the storage layer enforces retention.
    ///
    /// Reported rather than retried indefinitely: under an immutability policy the
    /// deletion will never succeed, and a retry loop would fill the logs while
    /// reclaiming nothing.
    pub blocked_by_retention: u64,
}

/// A source of change events.
#[async_trait::async_trait]
pub trait CaptureSource: Send + std::fmt::Debug {
    /// Begin streaming from a position.
    async fn start(&mut self, from: Lsn) -> Result<()>;

    /// The next batch of raw messages, if any arrive before the deadline.
    async fn next_batch(&mut self) -> Result<Vec<Vec<u8>>>;

    /// Confirm durability up to a position.
    ///
    /// **Only call this after the corresponding commit is durable.** Advancing first
    /// is silent data loss, and it is the most common defect in hand-built capture.
    async fn confirm(&mut self, through: Lsn) -> Result<()>;

    /// How far behind the source this consumer is, in positions and in retained bytes.
    ///
    /// Both are needed: seconds of lag is the operator-facing number, retained bytes
    /// is the one that predicts whether the source is about to run out of disk.
    async fn lag(&self) -> Result<CaptureLag>;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct CaptureLag {
    pub applied_through: Lsn,
    pub source_position: Lsn,
    pub retained_bytes: u64,
}

impl CaptureLag {
    /// Whether the consumer has caught up.
    #[must_use]
    pub const fn is_current(&self) -> bool {
        self.applied_through.get() >= self.source_position.get()
    }
}

/// A tier of the read path.
///
/// Every tier declares the interval it covers. A tier that cannot declare its coverage
/// cannot be spliced safely and must not be readable.
pub trait Tier: Send + Sync + std::fmt::Debug {
    fn name(&self) -> &'static str;
    fn coverage(&self) -> LsnRange;
}

/// Resolution of tables to readable, policy-rewritten sources.
///
/// The only path to a table. It takes a tenant because there is deliberately no
/// constructor that does not — the type system, rather than review, is what guarantees
/// no scan escapes policy.
#[async_trait::async_trait]
pub trait Catalog: Send + Sync + std::fmt::Debug {
    async fn resolve(&self, tenant: TenantId, name: &str, at: AsOf) -> Result<Snapshot>;
}

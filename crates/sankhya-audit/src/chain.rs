//! A tamper-evident record of what each principal was allowed to see.
//!
//! # What an audit record has to contain to be worth keeping
//!
//! Exit criterion 4 of M5 is the demanding one: *audit records reproduce exactly what a
//! principal saw, including data versions*. That rules out the usual shape, which records
//! that a query happened and who ran it. Reproducing what somebody *saw* needs four things:
//!
//! - **Who**, with how they authenticated --- a policy may require a stronger method for a
//!   stronger permission, and an audit that omits the method cannot show it did.
//! - **What they asked for**, verbatim.
//! - **The decision**, including the row filter and column masks that were applied. Without
//!   these the record says access was allowed and cannot say to *what*.
//! - **The data version** --- which snapshot, and for a graph result which epoch. The same
//!   query against the same table returns different rows a day later, so a record without
//!   a version cannot reproduce anything.
//!
//! # Why the chain is cryptographic
//!
//! Each record carries the digest of the one before it, so altering any record invalidates
//! every digest after it. That property comes entirely from the hash being collision- and
//! preimage-resistant. A chain built on a fast non-cryptographic hash is not tamper-evident
//! at all --- an attacker who can write the file can compute a colliding record in
//! milliseconds and the chain still verifies. It would look like a control and be none.
//! See ADR-0003.
//!
//! # What the chain does and does not prove
//!
//! It proves **internal consistency**: nothing has been altered, reordered, inserted or
//! removed from the middle. It does **not** prove that the tail is intact --- an attacker
//! who truncates the log and keeps the prefix leaves a chain that verifies perfectly.
//!
//! Only mirroring somewhere append-only fixes that, by making the true head knowable from
//! outside. [`Chain::head`] exists to be published there. Saying so plainly matters more
//! than the mechanism: an operator who believes a local chain proves completeness has a
//! false sense of a control they do not have.

use sankhya_authz::policy::{Action, Mask, TableRef};
use sankhya_authz::principal::{Authentication, Principal, TenantId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;

/// A SHA-256 digest, as hexadecimal.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct Hash(String);

impl Hash {
    /// The digest that precedes the first record.
    ///
    /// A fixed value rather than an absent one, so the first record is hashed exactly like
    /// every other. An `Option` here would mean a branch, and the branch would be the one
    /// place the chain is computed differently.
    #[must_use]
    pub fn genesis() -> Self {
        Self("0".repeat(64))
    }

    /// The digest as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Which version of the data a result came from.
///
/// Without this a record cannot reproduce anything: the same query against the same table
/// returns different rows a day later, and an audit that cannot say which day it was
/// answers no question anybody asks of it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct DataVersion {
    /// The table log version the scan read.
    pub snapshot: u64,
    /// The graph epoch, when a graph answered part of the query.
    pub graph_epoch: Option<u64>,
}

impl DataVersion {
    /// A version naming only a table snapshot.
    #[must_use]
    pub const fn snapshot(version: u64) -> Self {
        Self {
            snapshot: version,
            graph_epoch: None,
        }
    }

    /// The same, with the graph epoch that also contributed.
    #[must_use]
    pub const fn with_graph_epoch(mut self, epoch: u64) -> Self {
        self.graph_epoch = Some(epoch);
        self
    }
}

/// What the policy decided, in a form that survives to the record.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RecordedDecision {
    /// Whether it was permitted.
    pub allowed: bool,
    /// The row predicate applied, if any.
    pub row_filter: Option<String>,
    /// The columns obscured, and how.
    pub column_masks: BTreeMap<String, String>,
}

impl RecordedDecision {
    /// A permitted decision with its restrictions.
    #[must_use]
    pub fn allowed(row_filter: Option<String>, masks: &BTreeMap<String, Mask>) -> Self {
        Self {
            allowed: true,
            row_filter,
            column_masks: masks
                .iter()
                .map(|(column, mask)| (column.clone(), describe(mask)))
                .collect(),
        }
    }

    /// A refusal.
    ///
    /// Refusals are recorded as carefully as grants. A log containing only successful
    /// access cannot show an attempt to reach something forbidden, which is the pattern an
    /// investigation is usually looking for.
    #[must_use]
    pub fn denied() -> Self {
        Self {
            allowed: false,
            row_filter: None,
            column_masks: BTreeMap::new(),
        }
    }
}

/// How a mask is written in the record.
fn describe(mask: &Mask) -> String {
    match mask {
        Mask::Null => "null".to_string(),
        Mask::Partial { keep } => format!("partial(keep={keep})"),
        Mask::Constant { value } => format!("constant({value})"),
    }
}

/// One thing that happened.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Record {
    /// Position in the chain, from zero.
    pub sequence: u64,
    /// When, as microseconds from the epoch.
    ///
    /// Supplied by the caller rather than read from a clock, so this component stays pure
    /// and a record can be reconstructed exactly during verification. A component that
    /// reads a clock cannot be replayed.
    pub at: i64,
    /// Whose data.
    pub tenant: String,
    /// Who acted.
    pub subject: String,
    /// How they proved it.
    pub authentication: String,
    /// What they touched.
    pub table: String,
    /// What they tried to do.
    pub action: String,
    /// What the policy decided.
    pub decision: RecordedDecision,
    /// Which version of the data answered.
    pub data_version: Option<DataVersion>,
    /// The statement, verbatim.
    pub statement: Option<String>,
    /// How many rows they received.
    pub rows_returned: Option<u64>,
    /// The digest of the record before this one.
    pub previous: Hash,
    /// This record's own digest.
    pub digest: Hash,
}

impl Record {
    /// The bytes this record is hashed over.
    ///
    /// Every field except `digest` itself, in a fixed order. JSON with sorted keys, because
    /// the ordering has to be stable across versions of this crate --- a chain that stops
    /// verifying because a field was reordered is a chain nobody trusts.
    fn canonical(&self) -> String {
        // Serialised without the digest, which is what is being computed.
        let mut without = self.clone();
        without.digest = Hash(String::new());
        serde_json::to_string(&without).unwrap_or_else(|_| {
            // Serialisation of a struct of owned primitives cannot fail; if it somehow
            // does, produce something that will not collide with a real record rather than
            // something that silently hashes to the same value for every record.
            format!("unserialisable:{}:{}", self.sequence, self.at)
        })
    }

    /// Compute this record's digest.
    fn compute_digest(&self) -> Hash {
        let mut hasher = Sha256::new();
        hasher.update(self.canonical().as_bytes());
        Hash(format!("{:x}", hasher.finalize()))
    }
}

/// How many records a chain keeps in memory when it is given a window.
///
/// Enough that `SHOW` and an operator looking at what just happened have something to look at,
/// and small enough that the answer to *"how much memory does the audit use"* is a constant.
pub const WINDOW: usize = 1024;

/// An append-only sequence of records, each carrying the digest of the last.
///
/// # Why the records in memory can be a window onto a longer chain
///
/// Because they were a `Vec` that only ever grew. An entry is appended on every statement
/// **and every catalogue listing** --- every `\dt`, every JDBC metadata call, every
/// tab-completion --- with no cap and no rotation: roughly 3 to 5 GB a day at a hundred
/// statements a second, and 26 GB a day at a thousand. `OPS-04`.
///
/// The file is the chain. Memory holds the most recent [`WINDOW`] records so that something
/// can be shown and the next digest can be linked, and `len` and `head` go on describing the
/// **whole** chain rather than the part still in memory --- which is what makes the count
/// somebody mirrors mean anything.
///
/// A chain built with [`Chain::new`] keeps everything, because that is what a test wants and
/// what verifying a file end to end needs.
#[derive(Debug, Default)]
pub struct Chain {
    records: std::collections::VecDeque<Record>,
    /// How many records have ever been appended, including any no longer held.
    total: u64,
    /// The digest of the most recent record, held separately because the record itself may
    /// have been forgotten.
    head: Option<Hash>,
    /// How many records to keep, or `None` to keep every one.
    window: Option<usize>,
}

impl Chain {
    /// An empty chain that keeps every record.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty chain that keeps only the most recent `window` records.
    ///
    /// What a server uses. The file is where the chain lives; this is the part of it a running
    /// process can be asked about without the answer to *"how much memory does the audit use"*
    /// being *"as much as it has been up for"*.
    #[must_use]
    pub fn keeping(window: usize) -> Self {
        Self {
            window: Some(window.max(1)),
            ..Self::default()
        }
    }

    /// The digest of the most recent record, or the genesis value.
    ///
    /// This is what gets published somewhere append-only. A local chain cannot detect its
    /// own truncation --- an attacker who removes the tail leaves a chain that verifies
    /// perfectly --- and publishing the head is what makes the true length knowable from
    /// outside.
    #[must_use]
    pub fn head(&self) -> Hash {
        self.head.clone().unwrap_or_else(Hash::genesis)
    }

    /// How many records have ever been appended.
    ///
    /// The whole chain, not the part still in memory. A count that shrank when records aged
    /// out would be a count nobody could compare against what they mirrored --- and comparing
    /// it is the only way a truncated chain is ever noticed.
    #[must_use]
    pub fn len(&self) -> usize {
        usize::try_from(self.total).unwrap_or(usize::MAX)
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.total == 0
    }

    /// The records still in memory, oldest first.
    ///
    /// **Not necessarily every record.** See [`Chain::keeping`]; the file is the chain.
    #[must_use]
    pub fn records(&self) -> Vec<&Record> {
        self.records.iter().collect()
    }

    /// The most recently appended record, if it is still held.
    #[must_use]
    pub fn last(&self) -> Option<&Record> {
        self.records.back()
    }

    /// Drop the oldest records until the window is satisfied.
    fn forget_old(&mut self) {
        let Some(window) = self.window else {
            return;
        };
        while self.records.len() > window {
            self.records.pop_front();
        }
    }

    /// Append one entry.
    ///
    /// Returns the digest of the record written, so a caller mirroring to append-only
    /// storage has the value to publish without reaching back into the chain.
    pub fn append(&mut self, entry: Entry) -> Hash {
        let previous = self.head();
        let mut record = Record {
            sequence: self.total,
            at: entry.at,
            tenant: entry.tenant.to_string(),
            subject: entry.subject,
            authentication: describe_authentication(entry.authentication),
            table: entry.table.to_string(),
            action: format!("{:?}", entry.action).to_lowercase(),
            decision: entry.decision,
            data_version: entry.data_version,
            statement: entry.statement,
            rows_returned: entry.rows_returned,
            previous,
            digest: Hash(String::new()),
        };
        record.digest = record.compute_digest();
        let digest = record.digest.clone();
        self.head = Some(digest.clone());
        self.total = self.total.saturating_add(1);
        self.records.push_back(record);
        self.forget_old();
        digest
    }

    /// Append a record exactly as given, without recomputing anything.
    ///
    /// For reconstructing a chain read back from storage --- and for tests that need to
    /// construct a *tampered* one, which is the only way to check that verification
    /// actually detects tampering. A verification routine that has never been shown a
    /// broken chain is a routine nobody has tested.
    pub fn append_raw(&mut self, record: Record) {
        self.head = Some(record.digest.clone());
        self.total = self.total.saturating_add(1);
        self.records.push_back(record);
        self.forget_old();
    }

    /// Check the chain has not been altered.
    ///
    /// Recomputes every digest and every link. Detects alteration, reordering and insertion.
    /// Does **not** detect truncation of the tail, which no local check can --- compare
    /// [`Chain::head`] against what was mirrored for that.
    /// # What a windowed chain can and cannot check
    ///
    /// A chain built with [`Chain::keeping`] holds a window, so this verifies **that window**:
    /// the links between the records it still has, and each record's own digest. It cannot
    /// check a link to a record that has aged out, so the first record held is taken as it
    /// stands. Verifying a whole chain means reading the file, which is what
    /// `sankhya_audit::journal::read` into a [`Chain::new`] does.
    pub fn verify(&self) -> Result<(), Broken> {
        // Where the window begins, so a windowed chain checks the links it has rather than
        // reporting the first record it kept as out of order.
        let first = self
            .records
            .front()
            .map_or(0, |record| record.sequence);
        let mut expected_previous = self
            .records
            .front()
            .map_or_else(Hash::genesis, |record| record.previous.clone());
        for (index, record) in self.records.iter().enumerate() {
            let position = first.saturating_add(u64::try_from(index).unwrap_or(u64::MAX));
            if record.sequence != position {
                return Err(Broken::OutOfOrder {
                    at: position,
                    claims: record.sequence,
                });
            }
            if record.previous != expected_previous {
                return Err(Broken::LinkMismatch { at: position });
            }
            if record.compute_digest() != record.digest {
                return Err(Broken::Altered { at: position });
            }
            expected_previous = record.digest.clone();
        }
        Ok(())
    }

    /// Everything one principal saw, in order.
    ///
    /// The question an investigation actually asks. Filtering by subject *and* tenant
    /// because a subject name is only unique within a tenant, and matching on the name
    /// alone would return another tenant's records to whoever asked.
    #[must_use]
    pub fn what_was_seen_by<'a>(&'a self, tenant: &TenantId, subject: &str) -> Vec<&'a Record> {
        let tenant = tenant.to_string();
        self.records
            .iter()
            .filter(|r| r.tenant == tenant && r.subject == subject)
            .collect()
    }
}

/// What to record.
#[derive(Clone, Debug)]
pub struct Entry {
    /// When, as microseconds from the epoch.
    pub at: i64,
    /// Whose data.
    pub tenant: TenantId,
    /// Who acted.
    pub subject: String,
    /// How they proved it.
    pub authentication: Authentication,
    /// What they touched.
    pub table: TableRef,
    /// What they tried to do.
    pub action: Action,
    /// What the policy decided.
    pub decision: RecordedDecision,
    /// Which version of the data answered.
    pub data_version: Option<DataVersion>,
    /// The statement, verbatim.
    pub statement: Option<String>,
    /// How many rows they received.
    pub rows_returned: Option<u64>,
}

impl Entry {
    /// An entry for an access by this principal.
    #[must_use]
    pub fn by(
        principal: &Principal,
        table: TableRef,
        action: Action,
        decision: RecordedDecision,
        at: i64,
    ) -> Self {
        Self {
            at,
            tenant: principal.tenant().clone(),
            subject: principal.subject().to_string(),
            authentication: principal.authentication(),
            table,
            action,
            decision,
            data_version: None,
            statement: None,
            rows_returned: None,
        }
    }

    /// The same entry, naming the data version that answered.
    #[must_use]
    pub fn from_version(mut self, version: DataVersion) -> Self {
        self.data_version = Some(version);
        self
    }

    /// The same entry, with the statement.
    #[must_use]
    pub fn running(mut self, statement: impl Into<String>) -> Self {
        self.statement = Some(statement.into());
        self
    }

    /// The same entry, with how many rows came back.
    #[must_use]
    pub const fn returning(mut self, rows: u64) -> Self {
        self.rows_returned = Some(rows);
        self
    }
}

fn describe_authentication(method: Authentication) -> String {
    match method {
        Authentication::FederatedToken => "federated-token",
        Authentication::MutualTls => "mutual-tls",
        Authentication::Password => "password",
        Authentication::Internal => "internal",
        Authentication::Unverified => "unverified",
    }
    .to_string()
}

/// How a chain failed to verify.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Broken {
    /// A record's digest does not match its contents.
    Altered {
        /// Which position.
        at: u64,
    },
    /// A record does not carry the previous record's digest.
    LinkMismatch {
        /// Which position.
        at: u64,
    },
    /// A record's sequence number does not match its position.
    OutOfOrder {
        /// Which position.
        at: u64,
        /// What the record claims.
        claims: u64,
    },
}

impl fmt::Display for Broken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Altered { at } => write!(
                f,
                "the audit record at position {at} does not match its own digest: its \
                 contents have been changed since it was written"
            ),
            Self::LinkMismatch { at } => write!(
                f,
                "the audit record at position {at} does not carry the digest of the record \
                 before it: a record has been removed or inserted"
            ),
            Self::OutOfOrder { at, claims } => write!(
                f,
                "the audit record at position {at} claims to be number {claims}: the log \
                 has been reordered"
            ),
        }
    }
}

impl std::error::Error for Broken {}

//! Where a record that does not fit is kept, and for how long.
//!
//! # A table, not a directory of rejected files
//!
//! A side directory beside the warehouse is outside everything this system has built: nothing
//! sweeps it, nothing backs it up, no policy governs who may read it --- and it holds **source
//! data**, which is the most sensitive thing here. It would be a second store with none of the
//! first one's properties, which is the shape `check-writers` exists to refuse.
//!
//! As a table it inherits durability, backup, retention, tiering and policy without any of
//! them being written again. Its schema is fixed and shared by every feed, precisely because
//! the records in it are the ones that did not fit a feed's own schema.
//!
//! # The record is kept whole
//!
//! What is written is the document exactly as it arrived, alongside the reason, the position
//! it arrived at, and a fingerprint of the declaration that refused it. A record reduced to an
//! error message cannot be replayed, and replay is the only actual remedy. The fingerprint is
//! there because a declaration changes: *"why did this fail in March"* is otherwise a question
//! with no answer.

use crate::bind::Unfit;
use crate::declare::Declaration;
use arrow_array::{Int64Array, RecordBatch, StringArray, TimestampMicrosecondArray};
use arrow_schema::{DataType, Field, Schema, TimeUnit};
use std::sync::Arc;

/// The table every feed's refused records land in.
pub const TABLE: &str = "sank_quarantine";

/// One refused record, ready to be written.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Refused {
    /// Which feed refused it.
    pub feed: String,
    /// Which source it came from.
    pub source: String,
    /// Which record of that source it was, counting from zero.
    pub position: i64,
    /// When this system read it, in microseconds since the epoch.
    pub arrived_at: i64,
    /// A stable code for the kind of refusal.
    pub reason_code: &'static str,
    /// The sentence a person acts on.
    pub reason: String,
    /// A fingerprint of the declaration that refused it.
    pub declaration: String,
    /// The record, exactly as it arrived.
    pub payload: String,
}

/// The stable code for a refusal.
///
/// Separate from the sentence, and stable, because a client counting kinds must not be
/// counting substrings of prose --- and because the moment it does, the prose becomes an API
/// nobody meant to publish and nobody may reword.
#[must_use]
pub const fn code(unfit: &Unfit) -> &'static str {
    match unfit {
        Unfit::Unparseable { .. } => "unparseable",
        Unfit::NotADocument => "not-a-document",
        Unfit::MissingKey { .. } => "missing-key",
        Unfit::UnknownKey { .. } => "unknown-key",
        Unfit::WrongKind { .. } => "wrong-kind",
        Unfit::OutOfRange { .. } => "out-of-range",
        Unfit::NullIntoNotNull { .. } => "null-into-not-null",
    }
}

/// The quarantine table's schema.
///
/// Fixed, and the same for every feed. A per-feed quarantine schema would have to change
/// whenever a feed did, which is a migration triggered by the very configuration edit that
/// most needs somewhere to put the records it starts refusing.
#[must_use]
pub fn schema() -> Schema {
    Schema::new(vec![
        Field::new("feed", DataType::Utf8, false),
        Field::new("source", DataType::Utf8, false),
        Field::new("position", DataType::Int64, false),
        Field::new(
            "arrived_at",
            DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
            false,
        ),
        Field::new("reason_code", DataType::Utf8, false),
        Field::new("reason", DataType::Utf8, false),
        Field::new("declaration", DataType::Utf8, false),
        // The payload, verbatim. Text rather than binary because what arrives is a JSON
        // document and rendering it as bytes would make the table unreadable by the person
        // who most needs to read it.
        Field::new("payload", DataType::Utf8, false),
        // No `sank_data_date` here. `sankhya-publish` owns the date axis: it appends the
        // column to the stored schema and stamps it onto every partition file. A
        // hand-written one would be redundant at best and, if it ever disagreed with the
        // axis the table was created under, a table partitioned by one date and carrying
        // another.
    ])
}

/// Why a batch of refused records could not be assembled.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Unassembled {
    /// What Arrow said.
    pub detail: String,
}

impl std::fmt::Display for Unassembled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the quarantine batch could not be assembled: {}", self.detail)
    }
}

impl std::error::Error for Unassembled {}

/// Assemble refused records into a batch.
///
/// # Errors
///
/// [`Unassembled`] when the columns do not make a batch, which means this module and
/// [`schema`] have drifted apart.
pub fn batch(refused: &[Refused]) -> Result<RecordBatch, Unassembled> {
    let feeds = StringArray::from_iter_values(refused.iter().map(|r| r.feed.as_str()));
    let sources = StringArray::from_iter_values(refused.iter().map(|r| r.source.as_str()));
    let positions = Int64Array::from_iter_values(refused.iter().map(|r| r.position));
    let arrived = TimestampMicrosecondArray::from_iter_values(
        refused.iter().map(|r| r.arrived_at),
    )
    .with_timezone("UTC");
    let codes = StringArray::from_iter_values(refused.iter().map(|r| r.reason_code));
    let reasons = StringArray::from_iter_values(refused.iter().map(|r| r.reason.as_str()));
    let declarations =
        StringArray::from_iter_values(refused.iter().map(|r| r.declaration.as_str()));
    let payloads = StringArray::from_iter_values(refused.iter().map(|r| r.payload.as_str()));

    RecordBatch::try_new(
        Arc::new(schema()),
        vec![
            Arc::new(feeds),
            Arc::new(sources),
            Arc::new(positions),
            Arc::new(arrived),
            Arc::new(codes),
            Arc::new(reasons),
            Arc::new(declarations),
            Arc::new(payloads),
        ],
    )
    .map_err(|error| Unassembled { detail: error.to_string() })
}

/// `FNV-1a`, 64-bit — the same one the cube's definition version uses.
const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const PRIME: u64 = 0x0000_0100_0000_01b3;

/// A fingerprint of the declaration that refused a record.
///
/// Derived rather than declared, for the reason the cube's version is: a field somebody edits
/// is a field somebody forgets, and a quarantined record attributed to the wrong configuration
/// is worse than one attributed to none.
///
/// **Not cryptographic.** `FNV-1a` defends against accident. Somebody who can author a second
/// declaration with a colliding fingerprint can already author declarations, which is the
/// larger problem.
#[must_use]
pub fn fingerprint(declaration: &Declaration) -> String {
    let mut hash = OFFSET;
    absorb(&mut hash, declaration.name.as_bytes());
    absorb(&mut hash, declaration.schema.as_bytes());
    absorb(&mut hash, declaration.table.as_bytes());
    for column in &declaration.columns {
        absorb(&mut hash, column.name.as_bytes());
        absorb(&mut hash, column.key().as_bytes());
        absorb(&mut hash, column.written_type.as_bytes());
        absorb(&mut hash, &[u8::from(column.nullable)]);
        absorb(&mut hash, format!("{:?}", column.missing).as_bytes());
    }
    absorb(&mut hash, format!("{:?}", declaration.date).as_bytes());
    absorb(&mut hash, format!("{:?}", declaration.unknown).as_bytes());
    format!("{hash:016x}")
}

/// One field into the hash, length-prefixed.
///
/// Length-prefixed so that two adjacent fields cannot be rearranged into the same bytes ---
/// a column called `ab` reading `c` must not fingerprint the same as one called `a` reading
/// `bc`.
fn absorb(hash: &mut u64, bytes: &[u8]) {
    for byte in (bytes.len() as u64).to_le_bytes().iter().chain(bytes) {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(PRIME);
    }
}

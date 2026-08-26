//! The mutation model, and the withheld-value rule.

use sankhya_cdc_model::{Message, TupleData, TupleValue};
use sankhya_types::Lsn;

/// What happened to a row.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Op {
    Insert,
    Update,
    Delete,
}

/// A row image in the mutation plan, with every value resolved.
///
/// "Resolved" is the important word: no [`TupleValue::Unchanged`] survives into a
/// mutation. Either it was reconciled against the current row, or the mutation was
/// rejected. Letting one through would write a null over real data.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Row {
    pub values: Vec<Option<String>>,
}

/// One resolved change.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Mutation {
    pub relation_id: u32,
    pub op: Op,
    pub row: Row,
    /// The position of the transaction that produced this change.
    ///
    /// Recorded per mutation so the published data carries its own provenance and the
    /// applied position can be recovered from the table's history rather than from
    /// external state.
    pub commit_lsn: Lsn,
}

/// A batch of mutations that will be committed together.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct MutationPlan {
    pub mutations: Vec<Mutation>,
    /// The highest position wholly contained in this plan.
    pub covers_through: Lsn,
    /// Transactions represented, for assertion and reporting.
    pub transaction_count: usize,
}

impl MutationPlan {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.mutations.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.mutations.len()
    }

    /// A stable idempotency key for this plan.
    ///
    /// Derived from the position it covers rather than from a random value or a clock,
    /// so replaying the same range after a crash produces the same key and the commit
    /// becomes a no-op. That property is what converts at-least-once delivery into
    /// exactly-once effect.
    #[must_use]
    pub fn idempotency_key(&self, slot: &str) -> String {
        format!("{slot}:{}", self.covers_through.get())
    }
}

/// Resolve withheld values against the current row.
///
/// # Why this function exists rather than a default
///
/// A large value left untouched by an update is transmitted as a marker, not as data.
/// The only correct resolutions are to substitute the current value or to refuse the
/// mutation. Substituting a null is silent corruption, and the resulting row looks
/// entirely plausible — which is precisely why the decision is made here, explicitly,
/// rather than being allowed to happen by omission.
///
/// Returns `None` when a value was withheld and no current row is available to resolve
/// it against. The caller must then treat the mutation as unresolvable rather than
/// guessing.
#[must_use]
pub fn apply_unchanged(new: &TupleData, current: Option<&Row>) -> Option<Row> {
    let mut values = Vec::with_capacity(new.values.len());
    for (i, value) in new.values.iter().enumerate() {
        values.push(match value {
            TupleValue::Null => None,
            TupleValue::Text(s) => Some(s.clone()),
            TupleValue::Binary(b) => Some(hex(b)),
            // The whole point of this function.
            TupleValue::Unchanged => current?.values.get(i)?.clone(),
        });
    }
    Some(Row { values })
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(char::from(DIGITS[usize::from(b >> 4)]));
        out.push(char::from(DIGITS[usize::from(b & 0x0f)]));
    }
    out
}

/// Whether a message contributes a row change.
#[must_use]
pub const fn op_of(message: &Message) -> Option<Op> {
    match message {
        Message::Insert { .. } => Some(Op::Insert),
        Message::Update { .. } => Some(Op::Update),
        Message::Delete { .. } => Some(Op::Delete),
        _ => None,
    }
}

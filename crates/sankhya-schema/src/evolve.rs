//! Schema evolution.
//!
//! # Why some changes are refused rather than guessed
//!
//! When a table's shape changes, the stream carries the new shape but not the *intent*
//! behind it — and for several changes the intent is genuinely unknowable:
//!
//! - **A dropped column** may mean "stop capturing this from now on" or "erase it from
//!   history". Guessing wrong is either a data-loss incident or a compliance breach,
//!   and both are discovered long afterwards.
//! - **A rename** is indistinguishable from a drop-and-add without tracking column
//!   identity, so treating it as either one silently discards or duplicates a column.
//! - **A narrowing type change** loses data quietly. The values that fit look correct;
//!   the ones that did not are simply gone.
//!
//! So additive and widening changes apply automatically — their intent is unambiguous
//! — and everything else quarantines the table with a named error and an explicit
//! operator action. Quarantine converts an unbounded problem into a bounded one, and is
//! *safer* than the alternative, because a destructive change receives a human
//! decision instead of a default.
//!
//! # The coupled requirement
//!
//! Quarantining a table must not stall capture. With a single replication slot there is
//! one cursor, and if a quarantined table holds it back, retained log grows without
//! bound and eventually fills the source's volume. Quarantined events therefore have to
//! be diverted so the cursor keeps advancing — see the pipeline's dead-letter handling.

use crate::model::{Field, LogicalSchema, LogicalType, Precision};
use std::fmt;

/// What kind of change this is, and therefore what to do about it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Compatibility {
    /// Nothing changed.
    Unchanged,
    /// Safe to apply without operator involvement.
    Compatible { changes: Vec<SchemaChange> },
    /// Requires a decision no algorithm can make on the operator's behalf.
    Incompatible {
        reason: String,
        changes: Vec<SchemaChange>,
    },
}

impl Compatibility {
    /// Whether capture may continue publishing this table.
    #[must_use]
    pub const fn may_continue(&self) -> bool {
        !matches!(self, Self::Incompatible { .. })
    }
}

/// One difference between two shapes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SchemaChange {
    /// A new column. Unambiguous: rows before it simply lack a value.
    ColumnAdded { name: String, logical: LogicalType },
    /// A column widened in a way that loses nothing.
    ColumnWidened {
        name: String,
        from: LogicalType,
        to: LogicalType,
    },
    /// A column became nullable. Existing rows remain valid.
    ColumnRelaxed { name: String },
    /// A column disappeared.
    ColumnDropped { name: String },
    /// A column changed type in a way that may not round-trip.
    ColumnRetyped {
        name: String,
        from: LogicalType,
        to: LogicalType,
    },
    /// A column became mandatory. Existing rows may violate it.
    ColumnTightened { name: String },
    /// Row identity changed, so previously published rows may no longer be addressable.
    IdentityChanged,
    /// Columns were reordered, which is indistinguishable from a rename pair.
    ColumnsReordered,
}

impl fmt::Display for SchemaChange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ColumnAdded { name, .. } => write!(f, "column {name} added"),
            Self::ColumnWidened { name, from, to } => {
                write!(f, "column {name} widened from {from:?} to {to:?}")
            }
            Self::ColumnRelaxed { name } => write!(f, "column {name} became nullable"),
            Self::ColumnDropped { name } => write!(f, "column {name} dropped"),
            Self::ColumnRetyped { name, from, to } => {
                write!(f, "column {name} retyped from {from:?} to {to:?}")
            }
            Self::ColumnTightened { name } => write!(f, "column {name} became mandatory"),
            Self::IdentityChanged => write!(f, "row identity changed"),
            Self::ColumnsReordered => write!(f, "columns reordered"),
        }
    }
}

/// Whether a type change loses nothing.
///
/// Deliberately conservative: only changes whose every value provably survives count as
/// widening. Anything else is a retype, and a retype quarantines.
#[must_use]
fn widens(from: &LogicalType, to: &LogicalType) -> bool {
    use LogicalType::{Decimal, Float32, Float64, Int16, Int32, Int64};
    match (from, to) {
        (Int16, Int32 | Int64) | (Int32, Int64) => true,
        (Float32, Float64) => true,
        // A decimal widens only if it gains room without changing where the point sits.
        // Changing the scale rescales every existing value, which is a retype however
        // it looks.
        (Decimal(a), Decimal(b)) => a.scale == b.scale && b.digits >= a.digits,
        _ => false,
    }
}

/// Compare two shapes and decide what to do.
///
/// Column identity is matched **by name**, because the stream does not carry stable
/// column identifiers. That is precisely why a rename cannot be distinguished from a
/// drop-and-add here, and why both quarantine.
#[must_use]
pub fn classify_change(current: &LogicalSchema, incoming: &LogicalSchema) -> Compatibility {
    if current == incoming {
        return Compatibility::Unchanged;
    }

    let mut changes = Vec::new();
    let mut blocking = Vec::new();

    let find = |schema: &LogicalSchema, name: &str| -> Option<Field> {
        schema.fields.iter().find(|f| f.name == name).cloned()
    };

    // Columns that disappeared.
    for field in &current.fields {
        if find(incoming, &field.name).is_none() {
            let change = SchemaChange::ColumnDropped {
                name: field.name.clone(),
            };
            changes.push(change.clone());
            blocking.push(change);
        }
    }

    // Columns that appeared or changed.
    for field in &incoming.fields {
        match find(current, &field.name) {
            None => changes.push(SchemaChange::ColumnAdded {
                name: field.name.clone(),
                logical: field.logical.clone(),
            }),
            Some(existing) => {
                if existing.logical != field.logical {
                    let change = if widens(&existing.logical, &field.logical) {
                        SchemaChange::ColumnWidened {
                            name: field.name.clone(),
                            from: existing.logical.clone(),
                            to: field.logical.clone(),
                        }
                    } else {
                        let c = SchemaChange::ColumnRetyped {
                            name: field.name.clone(),
                            from: existing.logical.clone(),
                            to: field.logical.clone(),
                        };
                        blocking.push(c.clone());
                        c
                    };
                    changes.push(change);
                }
                if existing.nullable != field.nullable {
                    let change = if field.nullable {
                        SchemaChange::ColumnRelaxed {
                            name: field.name.clone(),
                        }
                    } else {
                        let c = SchemaChange::ColumnTightened {
                            name: field.name.clone(),
                        };
                        blocking.push(c.clone());
                        c
                    };
                    changes.push(change);
                }
                if existing.is_key != field.is_key {
                    let change = SchemaChange::IdentityChanged;
                    if !changes.contains(&change) {
                        changes.push(change.clone());
                        blocking.push(change);
                    }
                }
            }
        }
    }

    if blocking.is_empty() {
        return Compatibility::Compatible { changes };
    }

    let reason = blocking
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ");
    Compatibility::Incompatible {
        reason: format!(
            "{reason}. The intent behind such a change cannot be inferred from the \
             stream, so the table is quarantined rather than guessed at. Its last \
             consistent version remains queryable; resolve with an explicit operation"
        ),
        changes,
    }
}

/// Apply a compatible change, producing the new shape.
///
/// Returns `None` when the change is not compatible — the caller must quarantine
/// rather than proceed, and making that a type-level distinction prevents applying an
/// incompatible change by omission.
#[must_use]
pub fn apply_compatible(
    current: &LogicalSchema,
    incoming: &LogicalSchema,
) -> Option<LogicalSchema> {
    match classify_change(current, incoming) {
        Compatibility::Unchanged => Some(current.clone()),
        Compatibility::Compatible { .. } => Some(incoming.clone()),
        Compatibility::Incompatible { .. } => None,
    }
}

/// Build a widened decimal, for callers constructing schemas.
#[must_use]
pub fn widened_decimal(from: Precision, extra_digits: u8) -> Option<Precision> {
    Precision::new(from.digits.saturating_add(extra_digits), from.scale)
}

//! Automatic table onboarding.
//!
//! # What "automatic" means here, and what it deliberately does not
//!
//! A table created in the source becomes analytically queryable with no configuration
//! step. Onboarding is triggered by the first relation description the stream carries
//! for a relation SANKHYA has not seen, which is exactly when the first write happens.
//!
//! What it does **not** mean is guessing. Every column either maps exactly or the table
//! is refused with the column named. Approximating one column to avoid an operator
//! conversation produces data that reconciles against nothing, and the conversation
//! happens years later with worse information.

use crate::mapping::map_source_type;
use crate::model::{Field, LogicalSchema, LogicalType};
use crate::naming::{NamingError, TableLocation};
use sankhya_cdc_model::{RelationDescriptor, ReplicaIdentity};
use std::fmt;

/// How a table may be written, determined by what identifies its rows.
///
/// This is not a preference. Without row identity the source itself rejects updates
/// and deletes, so an append-only strategy is the only possible one.
///
/// # What `Mergeable` does and does not mean today
///
/// It means the *source* will emit updates and deletes for this table. It does **not** mean
/// they are folded: there is no merge, no upsert and no key-based fold anywhere in this
/// build, and no reader applies `_sankhya_op`. A table receiving updates is therefore stored
/// as every historical version of every row plus tombstones, and `SELECT *` returns all of
/// them.
///
/// This doc comment used to end *"knowing that at onboarding is what lets the storage layer
/// skip merge machinery it will never need"*, which reads as though the other branch has
/// merge machinery. Neither does. What the value actually drives is the onboarding warning
/// below and nothing else. `ING-09`; the fold arrives with the change-capture runtime,
/// `ING-00`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WriteStrategy {
    /// Rows can be identified, so updates and deletes can be applied.
    Mergeable,
    /// No usable identity. Only appends are possible.
    ///
    /// The base table *is* the append target, so there is no merge cost at all and
    /// freshness equals the commit interval. For event and telemetry data this is the
    /// correct model rather than a degradation.
    AppendOnly,
}

/// Something worth telling an operator that does not prevent onboarding.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum OnboardingWarning {
    /// The table has no usable row identity.
    ///
    /// Reported rather than silently accepted: with the default setting the *source*
    /// rejects updates and deletes on such a table, so an operator expecting them to
    /// replicate will otherwise be confused by their absence rather than by an error.
    NoRowIdentity { table: String },
    /// Full before-images are being sent.
    ///
    /// Correct and sometimes necessary, but it multiplies write-ahead log volume, and
    /// log volume is what governs how long a stalled consumer has before it endangers
    /// the source.
    FullReplicaIdentity { table: String },
    /// The path is not byte-identical to the identifier.
    NameTransformed { identifier: String, segment: String },
}

impl fmt::Display for OnboardingWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoRowIdentity { table } => write!(
                f,
                "{table} has no usable row identity, so it is onboarded append-only. \
                 The source will reject updates and deletes on it until a primary key \
                 or a replica identity is set"
            ),
            Self::FullReplicaIdentity { table } => write!(
                f,
                "{table} sends full before-images, which multiplies write-ahead log \
                 volume and shortens the margin before a stalled consumer endangers \
                 the source"
            ),
            Self::NameTransformed {
                identifier,
                segment,
            } => write!(
                f,
                "{identifier:?} is stored at {segment:?}; the original is recorded \
                 alongside the data so the mapping is recoverable"
            ),
        }
    }
}

/// Why a table cannot be onboarded.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum OnboardingError {
    /// A column has no faithful representation.
    UnmappableColumn {
        table: String,
        column: String,
        reason: String,
    },
    /// A column would shadow a reserved provenance column.
    ReservedColumn { table: String, column: String },
    /// The table's name cannot become a storage path.
    Naming(NamingError),
    /// The relation carries no columns.
    NoColumns { table: String },
}

impl fmt::Display for OnboardingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnmappableColumn {
                table,
                column,
                reason,
            } => write!(
                f,
                "{table}.{column} cannot be carried faithfully: {reason}. \
                 The table is not onboarded; exclude the column or change its type"
            ),
            Self::ReservedColumn { table, column } => write!(
                f,
                "{table}.{column} collides with a name SANKHYA reserves for provenance. \
                 Rename it in the source"
            ),
            Self::Naming(e) => write!(f, "{e}"),
            Self::NoColumns { table } => write!(f, "{table} has no columns"),
        }
    }
}

impl std::error::Error for OnboardingError {}

impl From<NamingError> for OnboardingError {
    fn from(e: NamingError) -> Self {
        Self::Naming(e)
    }
}

/// A table ready to receive data.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Onboarded {
    pub location: TableLocation,
    pub schema: LogicalSchema,
    pub strategy: WriteStrategy,
    /// Non-blocking observations, surfaced rather than swallowed.
    pub warnings: Vec<OnboardingWarning>,
}

/// Derive everything needed to store a relation, from its description alone.
///
/// # Errors
///
/// Returns [`OnboardingError`] when any column cannot be carried exactly, when a column
/// would shadow a provenance column, or when the name cannot become a path. The table
/// is then not onboarded at all — a partially onboarded table would accept writes it
/// could not faithfully store.
pub fn onboard_relation(relation: &RelationDescriptor) -> Result<Onboarded, OnboardingError> {
    let qualified = format!("{}.{}", relation.namespace, relation.name);

    if relation.columns.is_empty() {
        return Err(OnboardingError::NoColumns { table: qualified });
    }

    let location = TableLocation::resolve(&relation.namespace, &relation.name)?;

    let mut fields = Vec::with_capacity(relation.columns.len());
    for column in &relation.columns {
        if LogicalSchema::system_column_names().contains(&column.name.as_str()) {
            return Err(OnboardingError::ReservedColumn {
                table: qualified,
                column: column.name.clone(),
            });
        }

        let mapped = map_source_type(column.type_oid, column.type_modifier).map_err(|e| {
            OnboardingError::UnmappableColumn {
                table: qualified.clone(),
                column: column.name.clone(),
                reason: e.to_string(),
            }
        })?;

        fields.push(Field {
            name: column.name.clone(),
            logical: mapped.logical,
            // A key column cannot be null: it would not identify anything.
            nullable: !column.is_key,
            is_key: column.is_key,
        });
    }

    let schema = LogicalSchema::new(fields);

    let strategy = if schema.has_identity() && relation.replica_identity.identifies_rows() {
        WriteStrategy::Mergeable
    } else {
        WriteStrategy::AppendOnly
    };

    let mut warnings = Vec::new();
    if strategy == WriteStrategy::AppendOnly {
        warnings.push(OnboardingWarning::NoRowIdentity {
            table: qualified.clone(),
        });
    }
    if relation.replica_identity == ReplicaIdentity::Full {
        warnings.push(OnboardingWarning::FullReplicaIdentity { table: qualified });
    }
    if !location.is_fully_relatable() {
        warnings.push(OnboardingWarning::NameTransformed {
            identifier: format!("{}.{}", relation.namespace, relation.name),
            segment: location.relative_path(),
        });
    }

    Ok(Onboarded {
        location,
        schema,
        strategy,
        warnings,
    })
}

/// Whether a table's columns are all exact, so exact aggregates over it are meaningful.
#[must_use]
pub fn is_fully_exact(schema: &LogicalSchema) -> bool {
    schema.fields.iter().all(|f| f.logical.is_exact())
}

/// The columns that are not exact, for reporting.
#[must_use]
pub fn inexact_columns(schema: &LogicalSchema) -> Vec<&str> {
    schema
        .fields
        .iter()
        .filter(|f| !f.logical.is_exact())
        .map(|f| f.name.as_str())
        .collect()
}

/// Whether a logical type needs out-of-line storage consideration.
#[must_use]
pub const fn may_be_stored_out_of_line(logical: &LogicalType) -> bool {
    matches!(
        logical,
        LogicalType::Utf8 | LogicalType::Binary | LogicalType::Json
    )
}

//! Turning a declaration into a feed, or into every reason it is not one.

use crate::declare::{DateFrom, Declaration, Microbatch, Missing, Quarantine, Unknown};
use sankhya_schema::{LogicalType, Precision};
use std::collections::BTreeSet;
use std::fmt;

/// A column whose declared type has been read.
///
/// The declaration holds what was written; this holds what it means. Everything downstream
/// takes these, so no binder ever parses a type name.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Shaped {
    /// The column's name in the table.
    pub name: String,
    /// The key it reads from an arriving document.
    pub key: String,
    /// What the value is read as.
    pub logical: LogicalType,
    /// Whether the column accepts nulls.
    pub nullable: bool,
    /// What a document lacking the key means.
    pub missing: Missing,
}

/// Read a written type name.
///
/// The spellings are the logical types' own names, in the case a configuration file is
/// written in. Aliases are deliberately absent: `text` and `string` and `varchar` would each
/// be somebody's habit from another system, and accepting all three means a reader of one
/// configuration cannot tell whether two feeds declare the same thing.
fn read_type(written: &str) -> Option<LogicalType> {
    let trimmed = written.trim();
    if let Some(rest) = trimmed.strip_prefix("decimal(") {
        let inside = rest.strip_suffix(')')?;
        let (digits, scale) = inside.split_once(',')?;
        return Some(LogicalType::Decimal(Precision {
            digits: digits.trim().parse().ok()?,
            scale: scale.trim().parse().ok()?,
        }));
    }
    Some(match trimmed {
        "boolean" => LogicalType::Boolean,
        "int16" => LogicalType::Int16,
        "int32" => LogicalType::Int32,
        "int64" => LogicalType::Int64,
        "float32" => LogicalType::Float32,
        "float64" => LogicalType::Float64,
        "utf8" => LogicalType::Utf8,
        "binary" => LogicalType::Binary,
        "timestamp_utc" => LogicalType::TimestampUtc,
        "timestamp_local" => LogicalType::TimestampLocal,
        "date" => LogicalType::Date,
        "time" => LogicalType::Time,
        "uuid" => LogicalType::Uuid,
        "json" => LogicalType::Json,
        _ => return None,
    })
}

/// Every type name a declaration may write, for saying so in a refusal.
const TYPE_NAMES: &str = "boolean, int16, int32, int64, float32, float64, decimal(digits,scale), \
                          utf8, binary, timestamp_utc, timestamp_local, date, time, uuid, json";

/// A validated feed.
///
/// Constructible only by [`validate`]. Everything downstream --- binding a document, assembling
/// a batch, deciding to stop --- takes one of these, so no code path exists that operates on a
/// declaration nobody checked.
#[derive(Clone, PartialEq, Debug)]
pub struct Feed {
    declaration: Declaration,
    columns: Vec<Shaped>,
    date: DateFrom,
}

impl Feed {
    /// What was declared.
    #[must_use]
    pub const fn declaration(&self) -> &Declaration {
        &self.declaration
    }

    /// This feed's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.declaration.name
    }

    /// The columns, in the order the table has them, with their types read.
    #[must_use]
    pub fn columns(&self) -> &[Shaped] {
        &self.columns
    }

    /// What a key no column claims means.
    #[must_use]
    pub const fn unknown(&self) -> Unknown {
        self.declaration.unknown
    }

    /// When a batch closes.
    #[must_use]
    pub const fn microbatch(&self) -> Microbatch {
        self.declaration.microbatch
    }

    /// Where each row's date comes from.
    ///
    /// Not an `Option`, and not a field with a default. A declaration that does not say is
    /// not a feed, so by the time anything holds one of these the question has an answer
    /// somebody wrote down.
    #[must_use]
    pub const fn date(&self) -> &DateFrom {
        &self.date
    }

    /// What happens to records that do not fit.
    #[must_use]
    pub const fn quarantine(&self) -> Quarantine {
        self.declaration.quarantine
    }
}

/// One reason a declaration is not a feed.
///
/// Every variant names the thing at fault. A validation that said "invalid configuration"
/// would be a check the operator has to repeat by hand.
#[derive(Clone, PartialEq, Debug)]
pub enum Fault {
    /// A required name was empty.
    Empty {
        /// Which one.
        field: &'static str,
    },
    /// The feed declares no columns.
    NoColumns,
    /// Two columns claim the same name.
    DuplicateColumn {
        /// The name they share.
        name: String,
    },
    /// Two columns read the same key.
    ///
    /// Refused rather than allowed. It is occasionally what somebody means --- one value in
    /// two columns --- and far more often a copy-and-paste that silently drops a column's
    /// real source.
    DuplicateKey {
        /// The key they share.
        key: String,
        /// The columns that both read it.
        columns: Vec<String>,
    },
    /// A column is filled with null when its key is absent, and does not accept nulls.
    NullIntoNotNull {
        /// The column.
        name: String,
    },
    /// The feed does not say where its rows' date comes from.
    NoDateAxis,
    /// The date is declared to come from a column the feed does not have.
    DateColumnUnknown {
        /// The column named.
        name: String,
    },
    /// The date is declared to come from a column that is not a date.
    DateColumnNotADate {
        /// The column.
        name: String,
    },
    /// The date is declared to come from a column that may be null.
    DateColumnNullable {
        /// The column.
        name: String,
    },
    /// A column's declared type is not one this system has.
    UnknownType {
        /// The column.
        name: String,
        /// What was written.
        written: String,
    },
    /// A quarantine that never expires.
    QuarantineForever,
    /// A stop rate that cannot fire, or that fires on everything.
    UnusableStopRate {
        /// What was asked for.
        rate: f64,
    },
    /// A stop window of nothing, over which no rate can be measured.
    EmptyStopWindow,
    /// A batch that never closes on one of its two bounds.
    UnboundedBatch {
        /// Which bound is zero.
        bound: &'static str,
    },
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty { field } => write!(f, "`{field}` is empty"),
            Self::NoColumns => write!(
                f,
                "the feed declares no columns. A feed with no columns publishes rows with \
                 nothing in them, which is a table filling up with evidence that something ran"
            ),
            Self::DuplicateColumn { name } => {
                write!(f, "two columns are both called `{name}`")
            }
            Self::DuplicateKey { key, columns } => write!(
                f,
                "`{}` both read the key `{key}`. Refused rather than allowed: it is \
                 occasionally meant and far more often a copy that dropped a column's real \
                 source",
                columns.join("` and `")
            ),
            Self::NullIntoNotNull { name } => write!(
                f,
                "`{name}` is filled with null when its key is absent, and does not accept \
                 nulls. Make the column nullable, or say what a missing key means"
            ),
            Self::NoDateAxis => write!(
                f,
                "the feed does not say where `sank_data_date` comes from. Write `date: \
                 ingest` if it is the moment this system read the record, or `date: {{column: \
                 <name>}}` if the record carries the date it is about --- the two are not \
                 interchangeable, and a table holding a mixture cannot be asked about either"
            ),
            Self::DateColumnUnknown { name } => write!(
                f,
                "the date is declared to come from `{name}`, and this feed has no such column"
            ),
            Self::DateColumnNotADate { name } => write!(
                f,
                "the date is declared to come from `{name}`, which is not a date or a \
                 timestamp. A date derived from something else is a conversion nobody \
                 reviewed"
            ),
            Self::DateColumnNullable { name } => write!(
                f,
                "the date is declared to come from `{name}`, which may be null. A row with no \
                 date belongs to no partition, and answering that with a fallback \
                 reintroduces the mixture one row at a time"
            ),
            Self::UnknownType { name, written } => write!(
                f,
                "`{name}` is declared as `{written}`, which is not a type. The types are: \
                 {TYPE_NAMES}"
            ),
            Self::QuarantineForever => write!(
                f,
                "quarantine.retain_days is zero, so nothing quarantined would ever be \
                 removed. A quarantine that only grows holds exactly the records nobody \
                 looked at, and nobody is responsible for it"
            ),
            Self::UnusableStopRate { rate } => write!(
                f,
                "quarantine.stop_above is {rate}, which is not a fraction above zero and \
                 below one. At zero the feed stops on its first bad record; at one it never \
                 stops at all"
            ),
            Self::EmptyStopWindow => write!(
                f,
                "quarantine.window is zero, and no rate can be measured over no records"
            ),
            Self::UnboundedBatch { bound } => write!(
                f,
                "microbatch.{bound} is zero. A batch bounded only by the other stalls: by \
                 size alone the last records of a quiet hour are never published, and by time \
                 alone a busy feed writes a file per tick"
            ),
        }
    }
}

impl std::error::Error for Fault {}

/// Check a declaration, reporting **every** rule it fails.
///
/// # Why every rule rather than the first
///
/// The same argument the cube's measure validation makes. A validator that stops at the first
/// fault turns fixing a configuration into a sequence of builds, and somewhere around the
/// fourth the person stops fixing rules and starts removing them.
///
/// # Errors
///
/// Every [`Fault`] found, in a stable order.
pub fn validate(declaration: Declaration) -> Result<Feed, Vec<Fault>> {
    let mut faults = Vec::new();

    for (field, value) in [
        ("name", &declaration.name),
        ("from", &declaration.from),
        ("schema", &declaration.schema),
        ("table", &declaration.table),
    ] {
        if value.trim().is_empty() {
            faults.push(Fault::Empty { field });
        }
    }

    if declaration.columns.is_empty() {
        faults.push(Fault::NoColumns);
    }

    let mut seen_names = BTreeSet::new();
    let mut shaped = Vec::with_capacity(declaration.columns.len());
    for column in &declaration.columns {
        if column.name.trim().is_empty() {
            faults.push(Fault::Empty { field: "column name" });
        } else if !seen_names.insert(column.name.clone()) {
            faults.push(Fault::DuplicateColumn { name: column.name.clone() });
        }
        if column.missing == Missing::Null && !column.nullable {
            faults.push(Fault::NullIntoNotNull { name: column.name.clone() });
        }
        match read_type(&column.written_type) {
            Some(logical) => shaped.push(Shaped {
                name: column.name.clone(),
                key: column.key().to_owned(),
                logical,
                nullable: column.nullable,
                missing: column.missing,
            }),
            None => faults.push(Fault::UnknownType {
                name: column.name.clone(),
                written: column.written_type.clone(),
            }),
        }
    }

    // Grouped rather than reported per pair, so a key read by three columns is one fault
    // naming three columns instead of three faults naming two each.
    let mut by_key: std::collections::BTreeMap<&str, Vec<String>> =
        std::collections::BTreeMap::new();
    for column in &declaration.columns {
        by_key.entry(column.key()).or_default().push(column.name.clone());
    }
    for (key, columns) in by_key {
        if columns.len() > 1 {
            faults.push(Fault::DuplicateKey { key: key.to_owned(), columns });
        }
    }

    match &declaration.date {
        None => faults.push(Fault::NoDateAxis),
        Some(DateFrom::Ingest) => {}
        Some(DateFrom::Column { name }) => match shaped.iter().find(|c| &c.name == name) {
            None => faults.push(Fault::DateColumnUnknown { name: name.clone() }),
            Some(column) => {
                if !matches!(
                    column.logical,
                    LogicalType::Date | LogicalType::TimestampUtc | LogicalType::TimestampLocal
                ) {
                    faults.push(Fault::DateColumnNotADate { name: name.clone() });
                }
                if column.nullable || column.missing == Missing::Null {
                    faults.push(Fault::DateColumnNullable { name: name.clone() });
                }
            }
        },
    }

    let quarantine = declaration.quarantine;
    if quarantine.retain_days == 0 {
        faults.push(Fault::QuarantineForever);
    }
    if quarantine.window == 0 {
        faults.push(Fault::EmptyStopWindow);
    }
    if !(quarantine.stop_above > 0.0 && quarantine.stop_above < 1.0) {
        faults.push(Fault::UnusableStopRate { rate: quarantine.stop_above });
    }

    if declaration.microbatch.rows == 0 {
        faults.push(Fault::UnboundedBatch { bound: "rows" });
    }
    if declaration.microbatch.seconds == 0 {
        faults.push(Fault::UnboundedBatch { bound: "seconds" });
    }

    if !faults.is_empty() {
        return Err(faults);
    }
    match declaration.date.clone() {
        Some(date) => Ok(Feed { declaration, columns: shaped, date }),
        // Unreachable: a missing date axis is itself a fault, so this cannot be `None` with
        // no faults. Written as a refusal rather than an `unwrap` because "cannot happen" is
        // a claim, and one the compiler is not keeping is worth less than one it is.
        None => Err(vec![Fault::NoDateAxis]),
    }
}

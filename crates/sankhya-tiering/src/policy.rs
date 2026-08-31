//! What a tiering policy declares, and what makes a table eligible for one.
//!
//! # Why eligibility is decided here and not at purge time
//!
//! `FR-TIER-10` says it outright, and the reason is worth stating in the words of the failure:
//! a type that cannot round-trip faithfully must make a table ineligible **at policy creation,
//! not at purge time**. Discovering it at purge time means discovering it with a partition
//! already detached, a verification that cannot be completed, and an operator holding data that
//! is neither in the source nor provably in the archive.
//!
//! `FR-TIER-07` adds the same shape for the structural rules: eligibility is validated at policy
//! creation **and re-validated before each purge**. Both, because a table's contract can change
//! after a policy is written --- somebody adds an `UPDATE` path to what was an append-only
//! record table --- and a policy that was correct when written is not therefore correct now.
//!
//! # Every reason, never the first
//!
//! [`Policy::eligible`] returns all of them. `FR-TIER-26` requires the planning command to
//! report *"every failing precondition rather than the first"*, and this is where that starts:
//! a table failing on three counts should be fixable in one sitting. Reporting one per attempt
//! is how somebody fixes the float column, re-runs, learns about the mutable contract, and
//! concludes the tool is wasting their time.
//!
//! This is the same choice `Definition::validate` makes for cubes, for the same reason.

use sankhya_schema::LogicalType;
use std::fmt;

/// Why a column's type cannot be archived.
///
/// # What "canonical" has to mean for this to be worth anything
///
/// Verification (`FR-TIER-09`) compares the source against the published tier by per-column
/// checksum over a canonical byte encoding. For that comparison to mean what it says, the
/// encoding needs one property:
///
/// > **Two values are equal if and only if they encode to the same bytes.**
///
/// Both halves matter and they fail independently. If equal values can encode differently, a
/// faithful archive is reported as a mismatch and the purge halts on a defect that is not
/// there. If unequal values can encode identically, a *corrupted* archive is reported as
/// faithful --- and that is the direction that loses data, because the purge then proceeds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NotCanonical {
    /// Equal values can have different bytes, and unequal values the same bytes.
    ///
    /// Floating point breaks the property in both directions at once. `-0.0 == 0.0` is true
    /// and their bit patterns differ; `NaN == NaN` is false and two `NaN`s can share a bit
    /// pattern. A checksum over the bytes and a comparison by value therefore disagree, and
    /// which one is "right" is not a question with an answer.
    ///
    /// This is rejected rather than normalised. A normalisation --- collapse `-0.0`, pick one
    /// `NaN` --- makes the encoding canonical and makes the archive **not byte-faithful to the
    /// source**, which is the property `FR-TIER-09` is actually checking. Choosing convenience
    /// there would mean the verification passes while the archived bytes differ from what was
    /// purged.
    FloatingPoint,
    /// The stored form is text whose byte sequence is not determined by its value.
    ///
    /// Two JSON documents with the same content and different key order, spacing or number
    /// formatting are the same value and different bytes. Canonicalising would require parsing
    /// and re-emitting every document, which changes what was stored --- the same objection as
    /// floating point, arrived at from a different direction.
    ///
    /// A table that needs JSON archived can store it as [`LogicalType::Utf8`] and take
    /// responsibility for its own canonical form, which is an honest thing to ask and a
    /// decision the table's owner is able to make.
    UnstableTextForm,
}

impl fmt::Display for NotCanonical {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FloatingPoint => f.write_str(
                "floating point has no canonical byte encoding: -0.0 and 0.0 are equal with \
                 different bytes, and two NaNs can share bytes while comparing unequal. A \
                 checksum over the bytes and a comparison by value would disagree",
            ),
            Self::UnstableTextForm => f.write_str(
                "its stored text form is not determined by its value --- the same document \
                 with different key order or spacing is the same value and different bytes",
            ),
        }
    }
}

/// Whether this type has a canonical byte encoding, and why not if it has none.
///
/// # Errors
///
/// Returns why the type cannot be encoded canonically.
///
/// # Why this is exhaustive rather than a list of the bad ones
///
/// A `match` over every variant, so adding a logical type to `sankhya-schema` **fails to
/// compile here** until somebody decides what archiving it means. A deny-list would silently
/// admit the new type, and the first evidence would be a checksum mismatch on a purge.
pub const fn canonical_encoding(logical: &LogicalType) -> Result<(), NotCanonical> {
    match *logical {
        // Fixed-width, and the byte sequence is the value.
        LogicalType::Boolean
        | LogicalType::Int16
        | LogicalType::Int32
        | LogicalType::Int64
        | LogicalType::Date
        | LogicalType::Time
        | LogicalType::TimestampUtc
        | LogicalType::Uuid => Ok(()),
        // A wall-clock time with no zone. Distinct from `TimestampUtc` precisely so the
        // absence of a zone is never lost, and its bytes are as determined as any integer's.
        LogicalType::TimestampLocal => Ok(()),
        // Exact by construction. `Precision` fixes the scale, so two equal decimals have the
        // same unscaled value and therefore the same bytes --- which is the whole reason this
        // is the only representation permitted for values that must reconcile.
        LogicalType::Decimal(_) => Ok(()),
        // The bytes *are* the value. UTF-8 is a canonical encoding of a string by definition;
        // an over-long encoding is not valid UTF-8 rather than an alternative spelling.
        LogicalType::Utf8 | LogicalType::Binary => Ok(()),
        LogicalType::Float32 | LogicalType::Float64 => Err(NotCanonical::FloatingPoint),
        LogicalType::Json => Err(NotCanonical::UnstableTextForm),
    }
}

/// A column as a policy sees it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Column {
    /// Its name.
    pub name: String,
    /// Its logical type.
    pub logical: LogicalType,
    /// Whether it is part of the table's primary key.
    ///
    /// # Why a policy has to know this
    ///
    /// `FR-TIER-09` requires verification to establish **primary-key set equality**, and a set
    /// is not a question that can be asked of a table with no key. A policy written against a
    /// keyless table is a policy whose purge would reach verification and find it has nothing
    /// to compare --- with the partition already marked and an operator holding data that is
    /// in neither tier.
    ///
    /// So the key is declared here and its absence is [`Ineligible::NoPrimaryKey`], caught at
    /// policy creation like every other rule. This was missing when the eligibility rules were
    /// first written: they admitted a table that could not be verified.
    pub key: bool,
}

impl Column {
    /// An ordinary column.
    pub fn new(name: impl Into<String>, logical: LogicalType) -> Self {
        Self { name: name.into(), logical, key: false }
    }

    /// A column that is part of the primary key.
    pub fn key(name: impl Into<String>, logical: LogicalType) -> Self {
        Self { name: name.into(), logical, key: true }
    }
}

/// What the source does to a table, as the table's owner declares it.
///
/// # Why this is declared rather than observed
///
/// `sankhya-readpath` settled this for reading and the argument is identical here: inferring
/// *"this table looks append-only because no update has arrived yet"* is correct until the
/// first update. For reading, being wrong means double-counting. For tiering it means having
/// purged rows that were later corrected in place, with the correction now applying to data
/// that is no longer there.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Contract {
    /// Corrections are new rows. Nothing is ever updated or deleted in place.
    AppendOnly,
    /// Rows are updated or deleted in place.
    Mutable,
}

/// The basis on which archived data must be kept.
///
/// Carried on the policy because `FR-TIER-11` gates a purge on the range being *covered* by a
/// retention basis with legal-hold status resolved. A policy with no basis is not a policy with
/// a permissive one --- it is a policy nobody can act on, which is why this has no default.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Retention {
    /// What obliges the data to be kept, in the words of whoever is obliged.
    pub basis: String,
    /// How long, in days from the partition's own date.
    pub days: u32,
}

impl Retention {
    /// A retention basis.
    pub fn new(basis: impl Into<String>, days: u32) -> Self {
        Self { basis: basis.into(), days }
    }
}

/// A tiering policy, as declared.
///
/// Nothing here is checked on construction. Call [`Policy::eligible`], which reports every
/// reason at once.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Policy {
    /// The policy's name, as a command names it.
    pub name: String,
    /// The table it applies to.
    pub table: String,
    /// The column partitions are cut on, and which tiering moves whole.
    pub tiering_key: String,
    /// The table's columns.
    pub columns: Vec<Column>,
    /// What the source does to the table.
    pub contract: Contract,
    /// Whether the table is range-partitioned on [`Self::tiering_key`].
    pub range_partitioned: bool,
    /// Why the archived data must be kept, and for how long.
    pub retention: Retention,
    /// Whether the table's direct identifiers are held elsewhere behind surrogate keys.
    ///
    /// # Why this defaults to the refusing answer
    ///
    /// `FR-TIER-25`: a table carrying unvaulted direct identifiers is **ineligible by default**,
    /// because tiering converts a cheap erasure into an expensive one --- an erasure request
    /// against archived, immutable storage is a different and much harder operation than one
    /// against a live table.
    ///
    /// Nothing in this system classifies a column as a direct identifier, and inventing a
    /// classifier that guesses from column names would be worse than nothing: it would be
    /// confidently wrong about `customer_ref` in both directions. So this is an assertion the
    /// policy's author makes, and the *absence* of the assertion is a refusal rather than a
    /// permission. Somebody has to say it, and saying it is the point.
    pub identifiers_vaulted: bool,
}

/// Why a table cannot be tiered.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Ineligible {
    /// The table is not append-only by contract.
    NotAppendOnly,
    /// No column is declared part of the primary key.
    ///
    /// `FR-TIER-09` verifies primary-key set equality before anything is purged, and a table
    /// without a key has no set to compare. Discovering that at purge time means discovering
    /// it with the verification unable to run and the partition already marked --- which is
    /// the failure `FR-TIER-10` moved every other type rule here to avoid.
    NoPrimaryKey,
    /// The table is not range-partitioned on the tiering key.
    NotRangePartitioned {
        /// The key the policy names.
        key: String,
    },
    /// The tiering key is not a column of the table.
    NoSuchKey {
        /// The key the policy names.
        key: String,
    },
    /// A column's type has no canonical byte encoding.
    UnarchivableColumn {
        /// Which column.
        column: String,
        /// Why its type cannot be encoded.
        why: NotCanonical,
    },
    /// The table may carry direct identifiers that are not held behind surrogate keys.
    IdentifiersNotVaulted,
    /// The policy declares no retention basis, or a zero-day one.
    NoRetentionBasis,
    /// The table has no columns.
    ///
    /// Its own variant rather than folded into the others, because a policy over a table
    /// nobody described is a mistake about the *policy*, and every other rule would pass
    /// vacuously: no column has a bad type when there are no columns.
    NoColumns,
}

impl fmt::Display for Ineligible {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAppendOnly => f.write_str(
                "the table is not append-only by contract. Tiering purges from the source, and \
                 a row updated in place after its partition was archived is a correction \
                 applying to data that is no longer there",
            ),
            Self::NoPrimaryKey => f.write_str(
                "no column is declared part of the primary key. Verification establishes \
                 primary-key set equality before anything is purged, and a table with no key \
                 has no set --- which would be discovered with the partition already marked",
            ),
            Self::NotRangePartitioned { key } => write!(
                f,
                "the table is not range-partitioned on `{key}`. Purge is partition detach, and \
                 a partition that does not correspond to a range of the tiering key cannot be \
                 detached without taking rows nobody archived"
            ),
            Self::NoSuchKey { key } => {
                write!(f, "`{key}` is named as the tiering key and is not a column of the table")
            }
            Self::UnarchivableColumn { column, why } => {
                write!(f, "the column `{column}` cannot be archived faithfully: {why}")
            }
            Self::IdentifiersNotVaulted => f.write_str(
                "the policy does not assert that direct identifiers are held behind surrogate \
                 keys. Tiering turns a cheap erasure into an expensive one, so this is refused \
                 until somebody states otherwise --- the assertion is the point, and its \
                 absence is not a permission",
            ),
            Self::NoRetentionBasis => f.write_str(
                "the policy declares no retention basis. A purge is gated on the archived range \
                 being covered by one, so a policy without it describes a purge that can never \
                 be authorised",
            ),
            Self::NoColumns => f.write_str(
                "the policy describes a table with no columns, so every other rule would pass \
                 by having nothing to check",
            ),
        }
    }
}

/// Whether a table may be tiered, and every reason it may not.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Eligibility {
    /// Empty when the table is eligible.
    pub refusals: Vec<Ineligible>,
}

impl Eligibility {
    /// Whether the table may be tiered.
    #[must_use]
    pub fn is_eligible(&self) -> bool {
        self.refusals.is_empty()
    }
}

impl fmt::Display for Eligibility {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.refusals.is_empty() {
            return f.write_str("eligible");
        }
        let reasons: Vec<String> = self.refusals.iter().map(ToString::to_string).collect();
        write!(f, "ineligible: {}", reasons.join("; "))
    }
}

impl Policy {
    /// Every reason this table may not be tiered.
    ///
    /// Order is fixed --- structural rules, then per-column, then the declarations --- so two
    /// runs over the same policy produce the same list and a diff of two reports means
    /// something.
    #[must_use]
    pub fn eligible(&self) -> Eligibility {
        let mut refusals = Vec::new();

        if self.columns.is_empty() {
            // Reported alone. Every rule below would pass vacuously, and a report saying
            // "eligible except for having no columns" invites somebody to read the rest of it.
            return Eligibility { refusals: vec![Ineligible::NoColumns] };
        }

        if self.contract != Contract::AppendOnly {
            refusals.push(Ineligible::NotAppendOnly);
        }
        if !self.columns.iter().any(|column| column.name == self.tiering_key) {
            refusals.push(Ineligible::NoSuchKey { key: self.tiering_key.clone() });
        }
        if !self.range_partitioned {
            refusals.push(Ineligible::NotRangePartitioned { key: self.tiering_key.clone() });
        }

        // Every column, not the first bad one --- the whole point of the pre-flight is that a
        // schema is fixed once rather than one column per attempt.
        for column in &self.columns {
            if let Err(why) = canonical_encoding(&column.logical) {
                refusals.push(Ineligible::UnarchivableColumn {
                    column: column.name.clone(),
                    why,
                });
            }
        }

        if !self.columns.iter().any(|column| column.key) {
            refusals.push(Ineligible::NoPrimaryKey);
        }
        if !self.identifiers_vaulted {
            refusals.push(Ineligible::IdentifiersNotVaulted);
        }
        if self.retention.basis.trim().is_empty() || self.retention.days == 0 {
            refusals.push(Ineligible::NoRetentionBasis);
        }

        Eligibility { refusals }
    }
}

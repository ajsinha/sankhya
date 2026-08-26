//! Mirror naming: one name across every naming domain.
//!
//! # The requirement
//!
//! A table's origin must be identifiable from its storage path without consulting a
//! lookup table. The same name appears as the source identifier, the directory name,
//! the catalog entry and the name a user types.
//!
//! # Why this is nearly free
//!
//! The source folds unquoted identifiers to lower case, so for the large majority of
//! tables the identifier is *already* a valid path segment and no transformation
//! happens at all. The work is entirely in the tail: quoted identifiers containing
//! characters that are awkward in a path.
//!
//! # Why collisions are refused rather than disambiguated
//!
//! Distinct identifiers can transform to the same segment — differing only by case, or
//! by a character that becomes the separator. The obvious fix is to append a
//! disambiguating suffix. **That is exactly wrong**: a name like `orders-8b1a9953` is
//! not relatable, it is a hash with a prefix, and it destroys the property the whole
//! scheme exists to provide. So a collision is refused, loudly, and an operator
//! resolves it deliberately.

use std::fmt;

/// Segments SANKHYA will not allow a table to occupy.
///
/// The hidden-prefix rule is not cosmetic. Whole families of external readers skip
/// paths beginning with an underscore or a dot — it is precisely the mechanism that
/// makes format metadata directories invisible to them — so a table so named would be
/// unreadable by the very engines the layout exists to serve.
pub const RESERVED_SEGMENTS: &[&str] = &[
    "_delta_log", "metadata", "data", "_sankhya", "_staging", "graveyard",
    // Platform device names, because the local filesystem backend must work everywhere.
    "con", "prn", "aux", "nul",
    "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9",
    "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// The longest segment a path component may be.
const MAX_SEGMENT_BYTES: usize = 63;

/// The character substituted for anything unsafe.
///
/// Chosen because it is **not legal in an unquoted source identifier**, so its presence
/// is itself evidence that a transformation occurred — and because it reads naturally
/// as a word separator rather than as an escape.
const SEPARATOR: char = '-';

/// How much work mapping an identifier required.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum NameClass {
    /// The identifier is already a valid segment. Byte-for-byte identical, no lookup
    /// ever needed. The overwhelmingly common case.
    Identity,
    /// The identifier was transformed. Still legible, but the original must be
    /// recorded alongside the data for exact recovery.
    Transformed,
}

/// A validated path segment.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct PathSegment {
    text: String,
    class: NameClass,
}

impl PathSegment {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub const fn class(&self) -> NameClass {
        self.class
    }

    /// Whether the segment is byte-identical to its source identifier.
    #[must_use]
    pub const fn is_identity(&self) -> bool {
        matches!(self.class, NameClass::Identity)
    }
}

impl fmt::Display for PathSegment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

/// Why an identifier cannot become a path segment.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum NamingError {
    /// Nothing legible survived the transformation.
    Empty { identifier: String },
    /// The segment would occupy a name SANKHYA reserves.
    Reserved { identifier: String, segment: String },
    /// Two distinct identifiers map to one segment.
    ///
    /// Refused rather than disambiguated: a generated suffix destroys the readability
    /// the scheme exists to provide.
    Collision { identifier: String, existing: String, segment: String },
}

impl fmt::Display for NamingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty { identifier } => write!(
                f,
                "identifier {identifier:?} has no legible path representation; \
                 declare an explicit mapping or exclude the table"
            ),
            Self::Reserved { identifier, segment } => write!(
                f,
                "identifier {identifier:?} maps to {segment:?}, which SANKHYA reserves; \
                 rename it in the source or declare an explicit mapping"
            ),
            Self::Collision { identifier, existing, segment } => write!(
                f,
                "identifier {identifier:?} and {existing:?} both map to {segment:?}. \
                 SANKHYA will not disambiguate automatically, because a generated \
                 suffix destroys the naming relationship it exists to preserve. \
                 Rename one in the source, declare an explicit mapping, or exclude one"
            ),
        }
    }
}

impl std::error::Error for NamingError {}

/// Map a source identifier to a path segment.
///
/// # Errors
///
/// Returns [`NamingError`] when no legible segment exists or the result is reserved.
/// Collisions are detected by [`TableLocation`], which sees more than one identifier.
pub fn segment_for(identifier: &str) -> Result<PathSegment, NamingError> {
    if is_already_safe(identifier) {
        return Ok(PathSegment { text: identifier.to_string(), class: NameClass::Identity });
    }

    let transformed = transform(identifier);
    if transformed.is_empty() {
        return Err(NamingError::Empty { identifier: identifier.to_string() });
    }
    if is_reserved(&transformed) {
        return Err(NamingError::Reserved {
            identifier: identifier.to_string(),
            segment: transformed,
        });
    }
    Ok(PathSegment { text: transformed, class: NameClass::Transformed })
}

fn is_already_safe(identifier: &str) -> bool {
    !identifier.is_empty()
        && identifier.len() <= MAX_SEGMENT_BYTES
        && identifier
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        && !identifier.starts_with('_')
        && !is_reserved(identifier)
}

fn is_reserved(candidate: &str) -> bool {
    RESERVED_SEGMENTS.contains(&candidate) || candidate.starts_with('_') || candidate.starts_with('.')
}

/// Lower-case, replace unsafe characters, collapse runs, trim, and bound the length.
fn transform(identifier: &str) -> String {
    let mut out = String::with_capacity(identifier.len());
    let mut last_was_separator = false;

    for ch in identifier.chars() {
        let lowered = ch.to_lowercase().next().unwrap_or(ch);
        let safe = lowered.is_ascii_lowercase() || lowered.is_ascii_digit() || lowered == '_';
        if safe {
            out.push(lowered);
            last_was_separator = false;
        } else if !last_was_separator {
            out.push(SEPARATOR);
            last_was_separator = true;
        }
    }

    let trimmed = out.trim_matches(SEPARATOR);
    let mut result: String = trimmed.chars().take(MAX_SEGMENT_BYTES).collect();
    // Truncation may have left a trailing separator.
    while result.ends_with(SEPARATOR) {
        result.pop();
    }
    // A leading underscore would hide the directory from external readers.
    if result.starts_with('_') {
        result.insert_str(0, "t");
    }
    result
}

/// Where a table lives, and the mapping that put it there.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TableLocation {
    pub source_schema: String,
    pub source_table: String,
    pub schema_segment: PathSegment,
    pub table_segment: PathSegment,
}

impl TableLocation {
    /// Resolve a location, refusing anything unmappable.
    ///
    /// # Errors
    ///
    /// As [`segment_for`].
    pub fn resolve(source_schema: &str, source_table: &str) -> Result<Self, NamingError> {
        Ok(Self {
            source_schema: source_schema.to_string(),
            source_table: source_table.to_string(),
            schema_segment: segment_for(source_schema)?,
            table_segment: segment_for(source_table)?,
        })
    }

    /// The path relative to the warehouse root.
    #[must_use]
    pub fn relative_path(&self) -> String {
        format!("{}/{}", self.schema_segment, self.table_segment)
    }

    /// Whether both segments are byte-identical to their identifiers, so the path
    /// explains itself with no lookup at all.
    #[must_use]
    pub fn is_fully_relatable(&self) -> bool {
        self.schema_segment.is_identity() && self.table_segment.is_identity()
    }
}

/// Detects collisions across a set of tables.
///
/// Kept separate from [`segment_for`] because a collision is a property of a *set*, not
/// of a single identifier, and can only be found by considering them together.
#[derive(Debug, Default)]
pub struct CollisionCheck {
    seen: std::collections::BTreeMap<String, String>,
}

impl CollisionCheck {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a location.
    ///
    /// # Errors
    ///
    /// Returns [`NamingError::Collision`] naming both identifiers, so an operator can
    /// see exactly what conflicts rather than being told only that something did.
    pub fn insert(&mut self, location: &TableLocation) -> Result<(), NamingError> {
        let path = location.relative_path();
        let qualified = format!("{}.{}", location.source_schema, location.source_table);
        match self.seen.get(&path) {
            Some(existing) if existing != &qualified => Err(NamingError::Collision {
                identifier: qualified,
                existing: existing.clone(),
                segment: path,
            }),
            _ => {
                self.seen.insert(path, qualified);
                Ok(())
            }
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

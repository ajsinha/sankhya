//! `CREATE TABLE ... CLONE`.
//!
//! # Why a parser here rather than a DataFusion extension
//!
//! `CREATE TABLE x CLONE y` is not standard SQL, so the engine's parser rejects it before any
//! planning hook can see it, and every extension point sits *downstream* of a successful parse.
//! The statement has to be recognised before the engine is asked. This is the same conclusion
//! `sankhya-cube-sql` reached for `CREATE CUBE`, for the same reason.
//!
//! That places one hard requirement here, and it is why [`parse`] returns an `Option` rather
//! than a `Result`: **it must be able to say "not mine" without opinion.** `CREATE TABLE
//! orders (id BIGINT)` has to reach the engine untouched, and so does malformed SQL — whose
//! error must come from the engine that owns the language rather than from a pre-filter that
//! happened to look first.
//!
//! # Why `DROP TABLE` is not here
//!
//! Dropping a table clones still read is one of `ADR-0016`'s refusals, and the tempting place to
//! enforce it is a parser that intercepts `DROP TABLE`. That would be wrong: `DROP TABLE` is
//! ordinary SQL that the engine owns, and a pre-filter claiming it would have to reimplement
//! everything the engine already does with it — `IF EXISTS`, qualified names, the lot — to hand
//! back the same behaviour in every case it does not care about.
//!
//! The refusal belongs where the drop is *executed*, which is also where the answer to *"is
//! anything still reading this?"* lives. A statement is the wrong layer to ask it at.
//!
//! # Nothing here decides whether the clone may be made
//!
//! This module decides **what was written**. [`crate::refuse::may_clone`] decides whether it may
//! happen, and it needs facts about the origin that a parser has never seen — its versions, its
//! tenant, whether a purge is in flight. Splitting them means a syntax error names a position
//! and a refusal names a reason.

use std::fmt;

/// A clone-DDL statement.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Statement {
    /// The table to create.
    pub table: String,
    /// The table to clone.
    pub origin: String,
    /// The origin version, or `None` for its newest.
    ///
    /// Carried unresolved because *"the newest version"* is a question about a warehouse and
    /// this module has never seen one. Resolving it here would mean resolving it at parse time,
    /// which is a different moment from the one the clone is made at.
    pub version: Option<u64>,
}

/// Why a statement that began as clone DDL could not be read.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DdlError {
    /// A word was expected and something else was there.
    Expected {
        /// What was wanted.
        wanted: &'static str,
        /// What was found, or nothing if the statement ended.
        found: Option<String>,
    },
    /// The version is not a number.
    UnreadableVersion {
        /// What was there.
        found: String,
    },
    /// Something followed a complete statement.
    Trailing {
        /// The first unexpected word.
        found: String,
    },
}

impl fmt::Display for DdlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Expected { wanted, found } => match found {
                Some(found) => write!(f, "expected {wanted}, found `{found}`"),
                None => write!(f, "expected {wanted}, and the statement ended"),
            },
            Self::UnreadableVersion { found } => {
                write!(f, "`{found}` is not a version number")
            }
            Self::Trailing { found } => write!(
                f,
                "`{found}` follows a complete statement. Refused rather than ignored, because a \
                 clause this does not understand may be one that changes what was meant"
            ),
        }
    }
}

/// Read a clone statement, or say it is not one.
///
/// `None` means *"not mine"* and the caller must pass the statement on untouched.
#[must_use]
pub fn parse(sql: &str) -> Option<Result<Statement, DdlError>> {
    let words = words(sql);
    let mut at = 0usize;

    // Three words decide whether this is ours: CREATE, TABLE, and a CLONE somewhere after the
    // name. Anything else — including `CREATE TABLE orders (id BIGINT)` — is the engine's.
    if !matches_word(words.get(at), "CREATE") {
        return None;
    }
    at += 1;
    if !matches_word(words.get(at), "TABLE") {
        return None;
    }
    at += 1;
    // A `CLONE` keyword must appear, or this is an ordinary `CREATE TABLE`. Checked before any
    // error is produced, so a malformed ordinary statement still reaches the engine.
    if !words.iter().skip(at).any(|word| word.eq_ignore_ascii_case("CLONE")) {
        return None;
    }

    Some(read(&words, at))
}

fn read(words: &[String], mut at: usize) -> Result<Statement, DdlError> {
    let table = identifier(words.get(at), "a table name")?;
    at += 1;

    if !matches_word(words.get(at), "CLONE") {
        return Err(DdlError::Expected {
            wanted: "CLONE",
            found: words.get(at).cloned(),
        });
    }
    at += 1;

    let origin = identifier(words.get(at), "the table to clone")?;
    at += 1;

    let mut version = None;
    if matches_word(words.get(at), "AT") {
        at += 1;
        if !matches_word(words.get(at), "VERSION") {
            return Err(DdlError::Expected {
                wanted: "VERSION after AT",
                found: words.get(at).cloned(),
            });
        }
        at += 1;
        let found = words.get(at).ok_or(DdlError::Expected {
            wanted: "a version number",
            found: None,
        })?;
        version = Some(
            found
                .parse::<u64>()
                .map_err(|_| DdlError::UnreadableVersion { found: found.clone() })?,
        );
        at += 1;
    }

    if let Some(found) = words.get(at) {
        return Err(DdlError::Trailing { found: found.clone() });
    }

    Ok(Statement { table, origin, version })
}

fn matches_word(word: Option<&String>, keyword: &str) -> bool {
    word.is_some_and(|word| word.eq_ignore_ascii_case(keyword))
}

/// An identifier, with quotes removed and case preserved inside them.
///
/// Keywords are matched case-insensitively and are **not reserved**: a table called `clone` is
/// a table somebody may already have, and refusing it would be this module deciding what names
/// a warehouse may use.
fn identifier(word: Option<&String>, wanted: &'static str) -> Result<String, DdlError> {
    let found = word.ok_or(DdlError::Expected { wanted, found: None })?;
    let unquoted = found.strip_prefix('"').and_then(|rest| rest.strip_suffix('"'));
    match unquoted {
        Some(inner) if !inner.is_empty() => Ok(inner.to_string()),
        Some(_) => Err(DdlError::Expected { wanted, found: Some(found.clone()) }),
        None if found.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.') => {
            Ok(found.clone())
        }
        None => Err(DdlError::Expected { wanted, found: Some(found.clone()) }),
    }
}

/// Split into words, keeping a quoted identifier whole and dropping a trailing semicolon.
fn words(sql: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quoted = false;

    for character in sql.chars() {
        match character {
            '"' => {
                quoted = !quoted;
                current.push(character);
            }
            ';' if !quoted => break,
            c if c.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

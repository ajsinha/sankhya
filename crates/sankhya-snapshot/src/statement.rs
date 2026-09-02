//! The statements a client uses to take, list and drop a snapshot.
//!
//! # Why the parsing lives here
//!
//! One crate decides what a snapshot statement is. A second parser in the server would be a
//! second answer to what `CREATE SNAPSHOT eod EXPIRE AFTER 90 DAYS` means, and the two would
//! drift on whitespace, on quoting, or on case --- each of which is somebody's Tuesday.
//!
//! This is [`crate::model`]'s shape and `sankhya-clone`'s before it: [`parse`] returns `None`
//! for a statement that is not one of these, so the caller hands it back to the engine
//! untouched rather than having to know what the engine understands.
//!
//! # What is deliberately not here
//!
//! A per-statement `... AS OF SNAPSHOT x`. `ADR-0019` Decision 6 does not decide it: a run
//! reads one instant across many statements, so the session setting is the form that case
//! needs, and a second way to say the same thing is a second thing to keep consistent.

use std::fmt;

use crate::expire::{Expiry, Unaskable};

/// A statement about snapshots.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Statement {
    /// Take one, of every table the caller may read.
    Create {
        /// What to call it.
        name: String,
        /// How long it lives. Mandatory --- see `ADR-0019` Decision 3.
        expiry: Expiry,
    },
    /// List them, with what each pins and when it expires.
    Show,
    /// Remove one, releasing what it pinned.
    Drop {
        /// Which one.
        name: String,
        /// Whether the statement said `IF EXISTS`.
        if_exists: bool,
    },
    /// What a table's log says happened, version by version.
    ///
    /// In this crate rather than a new one because it is the same question in a different
    /// tense: a snapshot names an instant, and this lists the instants there are.
    History {
        /// The table asked about.
        table: String,
    },
    /// Read one table at a version, for the rest of this connection.
    ///
    /// # Why this is per table and a snapshot is not
    ///
    /// A snapshot is a set somebody curated and pinned; this is one table at one number. They
    /// are different acts: the first is *"the instant I named"* and the second is *"that
    /// version, whatever else has moved"*. Conflating them would let a query mix a curated
    /// instant with an arbitrary one and call the result a snapshot.
    ReadVersion {
        /// The table.
        table: String,
        /// The version, or `None` for `RESET VERSION OF`, which reads the present again.
        version: Option<u64>,
    },
}

/// Why a statement that began like a snapshot statement is not one.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum NotAStatement {
    /// A word was expected and something else was there.
    Expected {
        /// What was wanted.
        wanted: &'static str,
        /// What was found, or nothing if the statement ended.
        found: Option<String>,
    },
    /// `CREATE SNAPSHOT` with no expiry.
    ///
    /// Its own variant rather than an `Expected`, because the message has to explain a
    /// *decision* rather than name a missing token. Somebody who omitted it did not forget a
    /// keyword; they expected a default, and there is deliberately none.
    NoExpiry,
    /// The expiry is not a number of days this system will honour.
    Unaskable(Unaskable),
    /// Words after a statement that is already complete.
    Trailing {
        /// What followed.
        after: String,
    },
}

impl fmt::Display for NotAStatement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Expected { wanted, found } => match found {
                None => write!(
                    f,
                    "expected `{wanted}` and the statement ended. It reads `CREATE SNAPSHOT \
                     <name> EXPIRE AFTER <n> DAYS`, `SHOW SNAPSHOTS`, or `DROP SNAPSHOT \
                     <name>`"
                ),
                Some(found) => write!(
                    f,
                    "expected `{wanted}` and found `{found}`. It reads `CREATE SNAPSHOT \
                     <name> EXPIRE AFTER <n> DAYS`, `SHOW SNAPSHOTS`, or `DROP SNAPSHOT \
                     <name>`"
                ),
            },
            Self::NoExpiry => write!(
                f,
                "a snapshot must say how long it lives: `CREATE SNAPSHOT <name> EXPIRE AFTER \
                 <n> DAYS`. There is no default and no unbounded form, because a snapshot \
                 pins files --- one that never expired would hold a whole warehouse's \
                 versions alive, and the cost would fall on somebody who did not ask for it"
            ),
            Self::Unaskable(why) => write!(f, "{why}"),
            Self::Trailing { after } => write!(
                f,
                "unexpected `{after}` after the statement. A snapshot statement takes a name \
                 and an expiry, and nothing else"
            ),
        }
    }
}

impl std::error::Error for NotAStatement {}

/// Read a snapshot statement, or say the statement is not one.
///
/// `None` means *"not mine"* and the caller must pass the statement on untouched. That includes
/// every other `SHOW` and every ordinary `CREATE` and `DROP`.
#[must_use]
pub fn parse(sql: &str) -> Option<Result<Statement, NotAStatement>> {
    let words: Vec<&str> = sql.trim().trim_end_matches(';').split_whitespace().collect();
    let first = words.first().copied()?;
    let second = words.get(1).copied();

    if first.eq_ignore_ascii_case("SHOW")
        && second.is_some_and(|word| word.eq_ignore_ascii_case("SNAPSHOTS"))
    {
        return Some(match words.get(2) {
            None => Ok(Statement::Show),
            Some(after) => Err(NotAStatement::Trailing { after: (*after).to_owned() }),
        });
    }
    if first.eq_ignore_ascii_case("SHOW")
        && second.is_some_and(|word| word.eq_ignore_ascii_case("HISTORY"))
    {
        return Some(read_of(&words, "HISTORY").map(|table| Statement::History { table }));
    }
    if (first.eq_ignore_ascii_case("SET") || first.eq_ignore_ascii_case("RESET"))
        && second.is_some_and(|word| word.eq_ignore_ascii_case("VERSION"))
    {
        return Some(read_version(&words));
    }
    if first.eq_ignore_ascii_case("CREATE")
        && second.is_some_and(|word| word.eq_ignore_ascii_case("SNAPSHOT"))
    {
        return Some(read_create(&words));
    }
    if first.eq_ignore_ascii_case("DROP")
        && second.is_some_and(|word| word.eq_ignore_ascii_case("SNAPSHOT"))
    {
        return Some(read_drop(&words));
    }
    None
}

/// `SHOW <what> OF <table>`.
fn read_of(words: &[&str], what: &'static str) -> Result<String, NotAStatement> {
    match words.get(2) {
        Some(word) if word.eq_ignore_ascii_case("OF") => {}
        found => {
            return Err(NotAStatement::Expected {
                wanted: "OF",
                found: found.map(|word| (*word).to_owned()),
            })
        }
    }
    let _ = what;
    let table = identifier(words.get(3), "a table name")?;
    if let Some(after) = words.get(4) {
        return Err(NotAStatement::Trailing { after: (*after).to_owned() });
    }
    Ok(table)
}

/// `SET VERSION OF <table> = <n>` and `RESET VERSION OF <table>`.
fn read_version(words: &[&str]) -> Result<Statement, NotAStatement> {
    let resetting = words
        .first()
        .is_some_and(|word| word.eq_ignore_ascii_case("RESET"));
    expect(words.get(2), "OF")?;
    let table = identifier(words.get(3), "a table name")?;

    if resetting {
        if let Some(after) = words.get(4) {
            return Err(NotAStatement::Trailing { after: (*after).to_owned() });
        }
        return Ok(Statement::ReadVersion { table, version: None });
    }

    // `= <n>` and `<n>` and `TO <n>` are all spellings a person types.
    let rest: Vec<&str> = words
        .iter()
        .skip(4)
        .filter(|word| **word != "=" && !word.eq_ignore_ascii_case("TO"))
        .copied()
        .collect();
    let asked = rest.first().ok_or(NotAStatement::Expected {
        wanted: "a version number",
        found: None,
    })?;
    let asked = asked.trim_start_matches('=');
    let version: u64 = asked.parse().map_err(|_| NotAStatement::Expected {
        wanted: "a version number",
        found: Some((*rest.first().unwrap_or(&"")).to_owned()),
    })?;
    if let Some(after) = rest.get(1) {
        return Err(NotAStatement::Trailing { after: (*after).to_owned() });
    }
    Ok(Statement::ReadVersion { table, version: Some(version) })
}

/// `CREATE SNAPSHOT <name> EXPIRE AFTER <n> DAYS`.
fn read_create(words: &[&str]) -> Result<Statement, NotAStatement> {
    let name = identifier(words.get(2), "a snapshot name")?;

    // The expiry is checked for *presence* before its shape, so that omitting it gets the
    // message about the decision rather than one about a missing keyword.
    if words.len() <= 3 {
        return Err(NotAStatement::NoExpiry);
    }
    expect(words.get(3), "EXPIRE")?;
    expect(words.get(4), "AFTER")?;

    let days = words.get(5).ok_or(NotAStatement::Expected {
        wanted: "a number of days",
        found: None,
    })?;
    let days: u32 = days.parse().map_err(|_| NotAStatement::Expected {
        wanted: "a number of days",
        found: Some((*days).to_owned()),
    })?;
    // `DAY` as well as `DAYS`, because `EXPIRE AFTER 1 DAYS` reads badly and refusing it would
    // be pedantry with an error message attached.
    match words.get(6) {
        Some(unit) if unit.eq_ignore_ascii_case("DAYS") || unit.eq_ignore_ascii_case("DAY") => {}
        found => {
            return Err(NotAStatement::Expected {
                wanted: "DAYS",
                found: found.map(|word| (*word).to_owned()),
            })
        }
    }
    if let Some(after) = words.get(7) {
        return Err(NotAStatement::Trailing { after: (*after).to_owned() });
    }

    let expiry = Expiry::days(days).map_err(NotAStatement::Unaskable)?;
    Ok(Statement::Create { name, expiry })
}

/// `DROP SNAPSHOT [IF EXISTS] <name>`.
fn read_drop(words: &[&str]) -> Result<Statement, NotAStatement> {
    let mut at = 2usize;
    let if_exists = words.get(at).is_some_and(|word| word.eq_ignore_ascii_case("IF"))
        && words.get(at + 1).is_some_and(|word| word.eq_ignore_ascii_case("EXISTS"));
    if if_exists {
        at += 2;
    }
    let name = identifier(words.get(at), "a snapshot name")?;
    if let Some(after) = words.get(at + 1) {
        return Err(NotAStatement::Trailing { after: (*after).to_owned() });
    }
    Ok(Statement::Drop { name, if_exists })
}

/// A word that must be there, matched without regard to case.
fn expect(word: Option<&&str>, wanted: &'static str) -> Result<(), NotAStatement> {
    match word {
        Some(found) if found.eq_ignore_ascii_case(wanted) => Ok(()),
        found => Err(NotAStatement::Expected {
            wanted,
            found: found.map(|word| (*word).to_owned()),
        }),
    }
}

/// A name, with its quoting stripped.
///
/// Quotes are stripped so that `DROP SNAPSHOT 'eod'` and `DROP SNAPSHOT eod` are one statement.
/// Somebody who has been typing SQL all day will quote it, and refusing that would be pedantry.
fn identifier(word: Option<&&str>, wanted: &'static str) -> Result<String, NotAStatement> {
    let word = word.ok_or(NotAStatement::Expected { wanted, found: None })?;
    let bare = word.trim_matches(|c| c == '\'' || c == '"' || c == '`');
    if bare.is_empty() {
        return Err(NotAStatement::Expected {
            wanted,
            found: Some((*word).to_owned()),
        });
    }
    Ok(bare.to_owned())
}

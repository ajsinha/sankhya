//! The two questions a client may ask about a clone.
//!
//! # Why this exists at all
//!
//! `M10` built the whole of cloning and left it unaskable. [`crate::Lineages`] resolves where a
//! clone came from and what still reads it, and **no client could reach either**. A user could
//! create a clone and never afterwards ask what it was a clone of, which makes its numbers
//! unplaceable: two tables with the same shape and different totals, and nothing on the wire to
//! say that one is a snapshot of the other.
//!
//! The second question is worse. [`crate::refuse::may_drop`] refuses a drop that would strand a
//! clone, and it names the clones --- *after* the attempt. A refusal that names what would break
//! is no use to somebody who had no way to ask beforehand, and an interface that creates clones
//! freely and never surfaces them grows a warehouse nobody is tracking.
//!
//! [ADR-0017](https://github.com/ajsinha/sankhya/blob/main/docs/adr/0017-the-client-contract.md)
//! makes this server work rather than client work: a binding may contain no logic the server
//! does not enforce, so anything a client displays must be something the server can be asked.
//! Three bindings asking three different questions would be three different products.
//!
//! # Why the parsing lives here
//!
//! The same reason [`crate::ddl::parse`] does: one crate decides what a clone statement is. A
//! second parser in the server would be a second answer to what `SHOW LINEAGE OF orders` means,
//! and the two would drift on whitespace, on quoting or on case.
//!
//! [`parse`] returns `None` for a statement that is not one of these, so the caller hands it
//! back to the engine untouched rather than having to know what the engine understands.

use std::fmt;

/// A question about a clone.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Question {
    /// What is this a clone of, all the way up?
    Lineage {
        /// The table asked about.
        table: String,
    },
    /// What still reads this?
    Dependents {
        /// The table asked about.
        table: String,
    },
}

impl Question {
    /// The table the question is about.
    #[must_use]
    pub fn table(&self) -> &str {
        match self {
            Self::Lineage { table } | Self::Dependents { table } => table,
        }
    }
}

/// Why a statement that began like one of these is not one.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum NotAQuestion {
    /// `SHOW LINEAGE` or `SHOW DEPENDENTS` with no table named.
    NoTableNamed {
        /// Which question was being asked.
        question: &'static str,
    },
    /// The `OF` between the question and the table is missing.
    ExpectedOf {
        /// What was there instead, or nothing if the statement ended.
        found: Option<String>,
    },
    /// Words after a statement that is already complete.
    Trailing {
        /// What followed.
        after: String,
    },
}

impl fmt::Display for NotAQuestion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoTableNamed { question } => write!(
                f,
                "SHOW {question} OF needs the name of a table. A question about no table \
                 in particular has no answer this server could give"
            ),
            Self::ExpectedOf { found } => match found {
                None => write!(
                    f,
                    "expected `OF` and the statement ended. It reads `SHOW LINEAGE OF \
                     <table>` or `SHOW DEPENDENTS OF <table>`"
                ),
                Some(found) => write!(
                    f,
                    "expected `OF` and found `{found}`. It reads `SHOW LINEAGE OF <table>` \
                     or `SHOW DEPENDENTS OF <table>`"
                ),
            },
            Self::Trailing { after } => write!(
                f,
                "unexpected `{after}` after the statement. These take one table and nothing \
                 else"
            ),
        }
    }
}

impl std::error::Error for NotAQuestion {}

/// Read a question about a clone, or say the statement is not one.
///
/// `None` means *"not mine"* and the caller must pass the statement on untouched. That
/// includes every other `SHOW`: a catalogue-browsing client sends several on connection, and
/// claiming them here would break the client to answer a question it did not ask.
#[must_use]
pub fn parse(sql: &str) -> Option<Result<Question, NotAQuestion>> {
    let words: Vec<&str> = sql.trim().trim_end_matches(';').split_whitespace().collect();

    if !words.first().is_some_and(|word| word.eq_ignore_ascii_case("SHOW")) {
        return None;
    }
    let question = words.get(1)?;
    let (named, build): (&'static str, fn(String) -> Question) =
        if question.eq_ignore_ascii_case("LINEAGE") {
            ("LINEAGE", |table| Question::Lineage { table })
        } else if question.eq_ignore_ascii_case("DEPENDENTS") {
            ("DEPENDENTS", |table| Question::Dependents { table })
        } else {
            return None;
        };

    // From here the statement is ours, and everything wrong with it is an error rather than a
    // reason to hand it back. `SHOW LINEAGE` cannot be anything else.
    match words.get(2) {
        None => return Some(Err(NotAQuestion::NoTableNamed { question: named })),
        Some(word) if word.eq_ignore_ascii_case("OF") => {}
        Some(found) => {
            return Some(Err(NotAQuestion::ExpectedOf {
                found: Some((*found).to_owned()),
            }))
        }
    }

    let Some(table) = words.get(3) else {
        return Some(Err(NotAQuestion::ExpectedOf { found: None }));
    };
    if let Some(after) = words.get(4) {
        return Some(Err(NotAQuestion::Trailing { after: (*after).to_owned() }));
    }

    // Quotes stripped so `SHOW LINEAGE OF 'orders'` and `SHOW LINEAGE OF orders` are one
    // statement. Somebody who has been typing SQL all day will quote it, and refusing that
    // would be pedantry with an error message attached.
    let table = table.trim_matches(|c| c == '\'' || c == '"' || c == '`');
    Some(Ok(build(table.to_owned())))
}

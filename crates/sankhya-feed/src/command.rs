//! The two statements a feed answers, and everything that is not one.
//!
//! # Why a statement and not a restart
//!
//! [ADR-0018](https://github.com/ajsinha/sankhya/blob/main/docs/adr/0018-a-record-that-does-not-fit.md)
//! requires resuming to be an act somebody performs. The available alternative --- restart the
//! server --- makes an operator who wants to resume *one* feed take an outage on every other
//! one, and on every connection the server is holding.
//!
//! # Why the parsing lives here
//!
//! The same reason [`crate::declare::Declaration::from_document`] does: one crate decides what
//! a feed statement is. A second parser in the server would be a second answer to what
//! `RESUME  FEED   orders` means, and the two would drift on whitespace, on quoting, or on
//! case --- each of which is somebody's Tuesday.
//!
//! This is `sankhya-clone`'s shape, deliberately: [`parse`] returns `None` for a statement that
//! is not a feed command, so the caller hands it back to the engine untouched rather than
//! having to know what the engine understands.

use std::fmt;

/// A statement about feeds.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Command {
    /// List every feed and what it is doing.
    Show,
    /// Set a halted feed running again.
    Resume {
        /// Which feed.
        feed: String,
    },
}

/// Why a statement that began like a feed command is not one.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CommandError {
    /// `RESUME FEED` with nothing after it.
    NoFeedNamed,
    /// Words after a statement that is already complete.
    Trailing {
        /// What followed.
        after: String,
    },
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoFeedNamed => write!(
                f,
                "RESUME FEED needs the name of a feed. `SHOW FEEDS` lists them, with the \
                 reason each halted one stopped"
            ),
            Self::Trailing { after } => write!(
                f,
                "unexpected `{after}` after the statement. A feed command is `SHOW FEEDS` or \
                 `RESUME FEED <name>`, and nothing else"
            ),
        }
    }
}

impl std::error::Error for CommandError {}

/// Read a feed command, or hand the statement back.
///
/// `None` means *not a feed command* --- the caller passes it to the engine. `Some(Err(_))`
/// means it was one and was malformed, which is a refusal rather than something to pass on:
/// handing `RESUME FEED` to a SQL engine produces a parser error about `RESUME`, which sends
/// the reader looking in the wrong place.
#[must_use]
pub fn parse(sql: &str) -> Option<Result<Command, CommandError>> {
    let words: Vec<&str> = sql
        .trim()
        .trim_end_matches(';')
        .split_whitespace()
        .collect();

    let first = words.first().copied()?;
    let second = words.get(1).copied();

    if first.eq_ignore_ascii_case("SHOW") && second.is_some_and(|w| w.eq_ignore_ascii_case("FEEDS"))
    {
        return Some(match words.get(2) {
            None => Ok(Command::Show),
            Some(after) => Err(CommandError::Trailing { after: (*after).to_owned() }),
        });
    }

    if first.eq_ignore_ascii_case("RESUME") && second.is_some_and(|w| w.eq_ignore_ascii_case("FEED"))
    {
        let Some(name) = words.get(2) else {
            return Some(Err(CommandError::NoFeedNamed));
        };
        if let Some(after) = words.get(3) {
            return Some(Err(CommandError::Trailing { after: (*after).to_owned() }));
        }
        // Quotes stripped so `RESUME FEED 'orders'` and `RESUME FEED orders` are one
        // statement. A user who has been typing SQL all day will quote it, and refusing that
        // would be pedantry with a error message attached.
        let feed = name.trim_matches(|c| c == '\'' || c == '"' || c == '`');
        return Some(Ok(Command::Resume { feed: feed.to_owned() }));
    }

    None
}

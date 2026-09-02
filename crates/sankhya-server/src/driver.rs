//! The statements a *driver* sends around a query.
//!
//! # Why these are answered rather than refused
//!
//! A driver's first act on a connection is a handful of statements it does not think of as
//! statements: a float-precision setting, an application name, a transaction it will commit
//! immediately, a `DISCARD ALL` when it returns the connection to a pool. Refusing them is
//! refusing the driver, which makes this door useless to every client it exists for.
//!
//! `SET` was refused as `XX000` --- a *fatal server configuration error* --- which makes a pool
//! discard the connection and try again, forever.
//!
//! # Its own module
//!
//! `wiring.rs` is the composition root and reaches the hard length limit every time a
//! capability lands. The right answer to that is not a larger limit: a file nobody can hold in
//! their head is where a statement comes to be intercepted twice, or not at all.

use sankhya_api_pg::session::{QueryFailure, QueryResult};

use crate::wiring::refusal;

/// Answer a statement a driver sends around a query, or `None` if it is not one.
///
/// # Why these are answered rather than refused
///
/// A driver's first act on a connection is a handful of statements it does not think of as
/// statements: a float-precision setting, an application name, a transaction it will
/// immediately commit. Refusing them is refusing the driver, which makes this door useless
/// to every client it was built for.
///
/// # Why `ROLLBACK` is not among them
///
/// Because it is the one whose meaning we would be faking. This server writes nothing, so
/// `BEGIN` and `COMMIT` are true statements about a transaction of one statement --- but a
/// client that asks to *undo* and is told it worked has been lied to about the only thing
/// it wanted. It is refused, and the refusal says why.
pub(crate) fn run_session_statement(
    sql: &str,
) -> Option<Result<QueryResult, QueryFailure>> {
    let compact = sql.trim().trim_end_matches(';').trim().to_uppercase();
    let first = compact.split_whitespace().next().unwrap_or_default();

    // A transaction of one statement, which is what a read path has.
    let tag = match first {
        "BEGIN" | "START" => "BEGIN",
        "COMMIT" | "END" => "COMMIT",
        "SET" | "RESET" => "SET",
        "DISCARD" => "DISCARD ALL",
        "ROLLBACK" | "ABORT" => {
            return Some(Err(refusal(
                // `25P01`, no_active_sql_transaction: there is nothing to undo, which is
                // the truthful answer rather than a cheerful one.
                "25P01",
                "there is nothing to roll back: this server is a read path over a \
                 published warehouse, so a statement is its own transaction and nothing \
                 it has done can be undone. Answering `ROLLBACK` with success would be \
                 the one lie that matters",
            )))
        }
        _ => return None,
    };
    // `SET` and `RESET` of a real setting are accepted and have no effect, which is true:
    // nothing here reads a session setting. A setting that *did* change an answer would
    // have to be refused instead, and there is none.
    Some(Ok(QueryResult { fields: Vec::new(), rows: Vec::new(), tag: tag.to_owned() }))
}

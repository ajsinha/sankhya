//! Tables that appear while the server is running.
//!
//! # Why this exists
//!
//! It did not, and `CREATE TABLE ... CLONE` was therefore a statement that **succeeded and
//! produced something unreadable**. The clone was committed, `SHOW LINEAGE` and `SHOW
//! DEPENDENTS` saw it, re-issuing the create refused it as already there --- and every
//! `SELECT` against it answered *"table not found"*, until the next restart.
//!
//! Two reviewers found it independently, which is what a statement that succeeds and does
//! nothing looks like from outside.
//!
//! # Why it is its own module
//!
//! `wiring.rs` is the composition root and reaches the hard length limit every time a
//! capability is added. The right answer to that is not a larger limit: a file nobody can hold
//! in their head is where a statement comes to be intercepted twice, or not at all.

use sankhya_authz::policy::TableRef;

use crate::execute::ServableTable;
use crate::wiring::Server;

/// Add tables that have appeared since this server started.
///
/// # Why this exists
///
/// It did not, and `CREATE TABLE ... CLONE` was therefore a statement that **succeeded and
/// produced something unreadable**. The clone was committed, `SHOW LINEAGE` and `SHOW
/// DEPENDENTS` saw it, re-issuing the create refused it as already there --- and every
/// `SELECT` against it answered *"table not found"*, until the next restart.
///
/// Two reviewers found it independently, which is what a statement that succeeds and does
/// nothing looks like from outside.
///
/// # Why a clone is authorized as its root
///
/// A clone has no policy rule of its own and never will --- rules are written about tables
/// somebody designed. `ADR-0016` makes a clone a reference to its origin's files, so its
/// read right *is* the right to read what it references, and `readable` already resolves it
/// that way. This carries the same resolution into session registration, which is the only
/// other place the question is asked.
///
/// Returns how many were added.
pub(crate) fn new_tables(server: &Server, servable: &mut Vec<ServableTable>) -> usize {
    let (found, _) = crate::warehouse::discover(&server.warehouse_path());
    // The ordinary case, and the one that must cost nothing: no table has appeared, so
    // there is no lineage to read and no provider to resolve.
    if found.len() <= servable.len()
        && found.iter().all(|table| {
            servable.iter().any(|open| open.reference == table.reference)
        })
    {
        return 0;
    }

    let unseen: Vec<_> = found
        .into_iter()
        .filter(|table| {
            !servable.iter().any(|open| open.reference == table.reference)
        })
        .collect();
    if unseen.is_empty() {
        return 0;
    }

    let lineages = server.lineages();
    let (opened, _) =
        crate::warehouse::servable(&unseen, server.read_as_of(), &server.log_cache());
    let added = opened.len();
    for mut table in opened {
        table.authorize_as = authority_for(&table.reference, &lineages);
        servable.push(table);
    }
    added
}

/// The table a clone's read right derives from, or `None` for a table that is not one.
///
/// Took a `&Server` and read nothing from it, which the build said and nobody heard under
/// fifty-nine `unreachable_pub` warnings. A parameter a function does not use is a claim that
/// the answer depends on it --- here, that a clone's authority might vary by server --- and it
/// does not: the answer is entirely in the lineage.
fn authority_for(
    reference: &TableRef,
    lineages: &sankhya_clone::Lineages,
) -> Option<TableRef> {
    let qualified = if reference.schema.is_empty() {
        reference.table.to_string()
    } else {
        format!("{}.{}", reference.schema, reference.table)
    };
    // The **root**, not one step up: a clone of a clone references the root's files just as
    // surely. A lineage that cannot be resolved yields nothing, so the table is authorized
    // by its own name and refused --- the conservative answer for a clone nobody can place.
    let root = lineages.ancestors(&qualified).ok()?.last()?.clone();
    let (schema, table) = root.split_once('.')?;
    Some(TableRef::new(schema, table))
}

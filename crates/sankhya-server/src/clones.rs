//! The two questions a client may ask about a clone, answered.
//!
//! # Why this is its own module
//!
//! `wiring.rs` is the composition root and it grows every time a statement is added. It reached
//! the hard length limit here, and the right answer to that is not a larger limit: a file
//! nobody can hold in their head is where a statement comes to be intercepted twice, or not at
//! all. Answering questions about clones is a whole feature with one entry point, so it moves
//! out whole.
//!
//! # What it does not contain
//!
//! Creating and dropping a clone stay in `wiring.rs` beside the rest of the DDL, because they
//! share the resolution and authorization the DDL path uses. Only the *asking* is here.

use sankhya_api_pg::session::{QueryFailure, QueryResult};
use sankhya_authz::principal::Principal;

use crate::wiring::{refusal, Qualified, Server};

/// Answer what a table is a clone of, or what still reads it.
///
/// # Why a client may ask this at all
///
/// `M10` built the whole of cloning and left both questions unaskable. A user could create
/// a clone and never afterwards ask what it was a clone of --- two tables of the same shape
/// with different totals and nothing on the wire to say one is a snapshot of the other.
///
/// And `may_drop` names the clones that would break *after* the drop is attempted, which is
/// no use to somebody who had no way to ask first.
///
/// # Why it is authorized as a read of the table asked about
///
/// Unlike `SHOW FEEDS`, this is a question about *data*: knowing that `q3_frozen` is a
/// clone of `orders` tells you `orders` exists, and to whom. So it goes through the same
/// `readable` check a `SELECT` does, resolved through the clone's root exactly as a read is
/// --- a principal who may not read the table may not learn its family either.
pub(crate) fn answer(
    server: &Server,
    question: Result<sankhya_clone::Question, sankhya_clone::NotAQuestion>,
    principal: &Principal,
) -> Result<QueryResult, QueryFailure> {
    use sankhya_api_pg::message::{oid, FieldDescription};
    use sankhya_error::protocol::sqlstate;

    let question = match question {
        Ok(question) => question,
        Err(error) => {
            return Err(refusal(sqlstate::SYNTAX_ERROR.as_str(), &error.to_string()))
        }
    };

    let lineages = server.lineages();
    let asked = question.table().to_owned();
    let table = match server.qualify(&asked) {
        Qualified::One(name, _) => name,
        Qualified::Absent => {
            // Indistinguishable from a table that exists and may not be read, which is the
            // right answer rather than an accident: saying "you may not ask about that"
            // confirms it exists.
            return Err(refusal(
                "42P01",
                &format!("there is no table called `{asked}` on this server"),
            ));
        }
        Qualified::Ambiguous(candidates) => {
            let named = candidates.join(", ");
            return Err(crate::wiring::refusal_about(
                // `42P09`, ambiguous alias: the name resolves to more than one thing.
                "42P09",
                &format!("`{asked}` names more than one table: {named}"),
                "Qualify it with its schema. Both candidates are in this refusal's `subjects`.",
                candidates,
            ));
        }
    };
    if !server.readable(principal, &table, &lineages) {
        return Err(refusal(
            "42P01",
            &format!("there is no table called `{asked}` on this server"),
        ));
    }

    match question {
        sankhya_clone::Question::Lineage { .. } => {
            // The chain, nearest first, so the first row answers "what is this a clone of?"
            // and the last answers "what is it ultimately a snapshot of?".
            let chain = lineages.ancestors(&table).map_err(|cycle| {
                refusal(
                    sqlstate::DATA_EXCEPTION.as_str(),
                    &format!(
                        "the lineage of `{table}` forms a cycle at `{}`, which cannot                              happen by cloning and can by editing a table's properties.                              Refused rather than followed",
                        cycle.at
                    ),
                )
            })?;
            let rows = chain
                .iter()
                .enumerate()
                .map(|(step, ancestor)| {
                    // The lineage of the table one step *nearer*, because that is the
                    // record naming this ancestor. Step zero's is the table asked about;
                    // every later one is the previous link in the chain.
                    let nearer = step
                        .checked_sub(1)
                        .and_then(|previous| chain.get(previous))
                        .map_or(table.as_str(), String::as_str);
                    let lineage = lineages.of(nearer);
                    vec![
                        Some(step.saturating_add(1).to_string()),
                        Some(ancestor.clone()),
                        lineage.map(|found| found.version.to_string()),
                        lineage.map(|found| found.cloned_at.to_string()),
                    ]
                })
                .collect::<Vec<_>>();
            let tag = format!("SELECT {}", rows.len());
            Ok(QueryResult {
                fields: vec![
                    FieldDescription::text("step", oid::INT8, 8),
                    FieldDescription::text("origin", oid::TEXT, -1),
                    FieldDescription::text("origin_version", oid::INT8, 8),
                    FieldDescription::text("cloned_at", oid::INT8, 8),
                ],
                rows,
                tag,
            })
        }
        sankhya_clone::Question::Dependents { .. } => {
            let dependents = lineages.dependents(&table).map_err(|cycle| {
                refusal(
                    sqlstate::DATA_EXCEPTION.as_str(),
                    &format!(
                        "the lineage of `{table}` forms a cycle at `{}`, which cannot                              happen by cloning and can by editing a table's properties.                              Refused rather than followed",
                        cycle.at
                    ),
                )
            })?;
            let rows = dependents
                .iter()
                .map(|dependent| {
                    // Whether it reads this table itself or reads something that does. A
                    // direct clone is what a drop of this table breaks; an indirect one
                    // breaks when the table between them goes.
                    let direct = lineages
                        .of(dependent)
                        .is_some_and(|lineage| lineage.origin == table);
                    vec![
                        Some(dependent.clone()),
                        Some(if direct { "direct" } else { "indirect" }.to_owned()),
                        lineages.of(dependent).map(|found| found.version.to_string()),
                    ]
                })
                .collect::<Vec<_>>();
            let tag = format!("SELECT {}", rows.len());
            Ok(QueryResult {
                fields: vec![
                    FieldDescription::text("dependent", oid::TEXT, -1),
                    FieldDescription::text("relation", oid::TEXT, -1),
                    FieldDescription::text("reads_version", oid::INT8, 8),
                ],
                rows,
                tag,
            })
        }
    }
}

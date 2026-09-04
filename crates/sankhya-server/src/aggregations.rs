//! Aggregations a user declared: where they live, and the statements that manage them.
//!
//! # What this is for
//!
//! A cube's measures compose along each dimension by a **declared rule** --- sum, last, max ---
//! and what that model cannot express is the rule that is *this* firm's: a weighted average
//! with their weighting, an exposure netted their way, a percentile with their interpolation.
//! [ADR-0010](../../../docs/adr/0010-external-aggregations.md) settles the contract for one;
//! this is the door it arrives through.
//!
//! # Where they live, and why in the warehouse
//!
//! A document per aggregation under `_aggregations/`, beside `_cubes/` and `_snapshots/`. In
//! the warehouse rather than beside it, for the reason every other piece of catalogue state is:
//! a backup that copied the tables and not the aggregations would restore a warehouse whose
//! reports cannot be recomputed.
//!
//! # What is stored, and why the source is part of it
//!
//! The author's Python, as written. [ADR-0023](../../../docs/adr/0023-the-sandbox-a-user-function-runs-in.md)
//! Decision 4 makes creating one a **grant** rather than a right, and a grant nobody can review
//! is a grant nobody should give. It is also what a later reader needs in order to know what
//! the column in front of them was computed by.

use std::path::{Path, PathBuf};

use sankhya_api_pg::session::{QueryFailure, QueryResult};
use sankhya_authz::policy::{Action, TableRef};
use sankhya_authz::principal::Principal;
use sankhya_udf::Aggregation;

use crate::wiring::{acknowledged, refusal, Server};
use datafusion::prelude::SessionContext;
use std::sync::Arc;

/// The bookkeeping schema aggregation documents live under.
///
/// `_`-prefixed, so `warehouse::discover` skips it: an aggregation is not a user table and must
/// not appear in a catalogue somebody browses.
pub(crate) const DIRECTORY: &str = "_aggregations";

/// Where an aggregation of this name is stored.
fn document_at(warehouse: &Path, name: &str) -> PathBuf {
    warehouse.join(DIRECTORY).join(format!("{name}.json"))
}

/// A statement about aggregations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Statement {
    /// `CREATE AGGREGATION <name> LANGUAGE PYTHON AS $$ … $$`
    Create {
        /// What it will be called in a query.
        name: String,
        /// The author's code.
        source: String,
    },
    /// `DROP AGGREGATION [IF EXISTS] <name>`
    Drop {
        /// Which one.
        name: String,
        /// Whether its absence is acceptable.
        if_exists: bool,
    },
    /// `SHOW AGGREGATIONS`
    Show,
}

/// Recognise one, or `None` for every statement that is not ours.
///
/// Written by hand for the same reason `CREATE CUBE` is: this is not SQL, so `sqlparser`
/// rejects it before any engine hook could see it.
pub(crate) fn parse(sql: &str) -> Option<Statement> {
    let trimmed = sql.trim().trim_end_matches(';').trim();
    let upper = trimmed.to_uppercase();

    if upper == "SHOW AGGREGATIONS" {
        return Some(Statement::Show);
    }

    if let Some(rest) = strip_keywords(trimmed, &["DROP", "AGGREGATION"]) {
        let (if_exists, rest) = match strip_keywords(rest, &["IF", "EXISTS"]) {
            Some(after) => (true, after),
            None => (false, rest),
        };
        let name = rest.trim();
        if name.is_empty() || name.contains(char::is_whitespace) {
            return None;
        }
        return Some(Statement::Drop { name: name.to_owned(), if_exists });
    }

    let rest = strip_keywords(trimmed, &["CREATE", "AGGREGATION"])?;
    // `$$ … $$`, PostgreSQL's own dollar quoting. Python is full of quotes and backslashes, and
    // a body written between ordinary quotes would have to be escaped by whoever typed it ---
    // which is how a function comes to differ from the one its author tested.
    let (head, body) = rest.split_once("$$")?;
    let source = body.rsplit_once("$$")?.0;
    let head = head.trim();
    let name = head.split_whitespace().next()?;
    let declared_language = head.to_uppercase();
    if !declared_language.contains("LANGUAGE PYTHON") {
        return None;
    }
    Some(Statement::Create { name: name.to_owned(), source: source.to_owned() })
}

/// The remainder after a run of keywords, or `None` if they are not what this begins with.
fn strip_keywords<'a>(text: &'a str, keywords: &[&str]) -> Option<&'a str> {
    let mut rest = text.trim_start();
    for keyword in keywords {
        let head = rest.get(..keyword.len())?;
        if !head.eq_ignore_ascii_case(keyword) {
            return None;
        }
        let after = rest.get(keyword.len()..)?;
        if !after.starts_with(char::is_whitespace) {
            return None;
        }
        rest = after.trim_start();
    }
    Some(rest)
}

/// Every aggregation this warehouse holds.
pub(crate) fn stored(warehouse: &Path) -> Vec<Aggregation> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(warehouse.join(DIRECTORY)) else {
        return out;
    };
    for entry in entries.flatten() {
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        if let Some(aggregation) = decode(&text) {
            out.push(aggregation);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The document form. Hand-written rather than derived, because `Aggregation` lives in a crate
/// that does not depend on a serialisation library and should not acquire one to be stored.
fn encode(aggregation: &Aggregation) -> String {
    format!(
        "{{\"format\":1,\"name\":{},\"composes\":{},\"source\":{}}}",
        quoted(&aggregation.name),
        aggregation.composes,
        quoted(&aggregation.source)
    )
}

fn decode(text: &str) -> Option<Aggregation> {
    let name = field(text, "\"name\":")?;
    let source = field(text, "\"source\":")?;
    let composes = text.contains("\"composes\":true");
    Some(Aggregation { name, source, composes })
}

/// One JSON string field, unescaped.
fn field(text: &str, key: &str) -> Option<String> {
    let after = text.split_once(key)?.1.trim_start();
    let mut characters = after.strip_prefix('"')?.chars();
    let mut out = String::new();
    while let Some(character) = characters.next() {
        match character {
            '"' => return Some(out),
            '\\' => match characters.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                other => out.push(other),
            },
            other => out.push(other),
        }
    }
    None
}

/// A JSON string.
fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other if (other as u32) < 0x20 => out.push(' '),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// Run one.
pub(crate) fn run(
    server: &Server,
    statement: Statement,
    principal: &Principal,
) -> Result<QueryResult, QueryFailure> {
    use sankhya_error::protocol::sqlstate;

    // The capability check, before the statement is looked at. `CREATE AGGREGATION` runs
    // code the caller supplied, and `ADR-0023` Decision 4 says it "requires a capability
    // that is not granted by default". Nothing granted it and nothing checked it: the arm
    // returned before the zero-role refusal, so a caller holding no roles reached it, and
    // the code then wrote an audit entry asserting the decision had been allowed.
    //
    // A server-wide switch is not the per-principal grant that decision describes, and is
    // not pretended to be. It is the half of it that can be honest today --- closed unless
    // an operator opened it. `SHOW` is not gated: reading which functions exist is not
    // running one, and an operator who has just closed the door needs to see what came in
    // while it was open.
    if !server.settings.user_functions && !matches!(statement, Statement::Show) {
        let named = match &statement {
            Statement::Create { name, .. } | Statement::Drop { name, .. } => name.clone(),
            Statement::Show => String::new(),
        };
        server.record(principal, TableRef::new("", &named), Action::Insert, false);
        return Err(refusal(
            sqlstate::INSUFFICIENT_PRIVILEGE.as_str(),
            "this server does not accept user-supplied aggregations. They run code this              server did not write, so the door is closed unless an operator opens it with              `server.user_functions: true`. `SHOW AGGREGATIONS` still lists what is declared",
        ));
    }

    match statement {
        Statement::Show => show(server),
        Statement::Drop { name, if_exists } => {
            let path = document_at(&server.settings.warehouse, &name);
            if !path.exists() {
                if if_exists {
                    return Ok(acknowledged("DROP AGGREGATION"));
                }
                return Err(refusal(
                    sqlstate::DATA_EXCEPTION.as_str(),
                    &format!("there is no aggregation `{name}`"),
                ));
            }
            std::fs::remove_file(&path).map_err(|error| {
                refusal(sqlstate::IO_ERROR.as_str(), &error.to_string())
            })?;
            server.forget_aggregation(&name);
            server.record(principal, TableRef::new("", &name), Action::Delete, true);
            Ok(acknowledged("DROP AGGREGATION"))
        }
        Statement::Create { name, source } => {
            if document_at(&server.settings.warehouse, &name).exists() {
                return Err(refusal(
                    sqlstate::DATA_EXCEPTION.as_str(),
                    &format!(
                        "the aggregation `{name}` already exists. Drop it first: replacing one \
                         changes every answer computed with it, and a re-run of a script should \
                         not do that silently"
                    ),
                ));
            }

            // Exercised before it is trusted, and before it is stored. `ADR-0010`: the same
            // input accumulated one way and several ways, merged in two groupings, compared by
            // bits. A function that disagrees with itself is refused here, with both answers,
            // rather than found later as two reports differing by a penny.
            let worker = server.worker().map_err(|refused| {
                refusal(sqlstate::DATA_EXCEPTION.as_str(), &refused.to_string())
            })?;
            let declared = worker.declare(&name, &source).map_err(|refused| {
                refusal(sqlstate::DATA_EXCEPTION.as_str(), &refused.to_string())
            })?;

            let directory = server.settings.warehouse.join(DIRECTORY);
            std::fs::create_dir_all(&directory).map_err(|error| {
                refusal(sqlstate::IO_ERROR.as_str(), &error.to_string())
            })?;
            // Published, not written. A reader opening `_aggregations/x.json` while it is being
            // written sees a truncated document --- and a truncated aggregation is one whose
            // source stops mid-function, which is a thing this server would then try to run.
            sankhya_atomicfs::publish(
                &document_at(&server.settings.warehouse, &name),
                encode(&declared).as_bytes(),
            )
            .map_err(|error| refusal(sqlstate::IO_ERROR.as_str(), &error.to_string()))?;

            server.remember_aggregation(declared);
            server.record(principal, TableRef::new("", &name), Action::Insert, true);
            Ok(acknowledged("CREATE AGGREGATION"))
        }
    }
}

/// `SHOW AGGREGATIONS` — name, whether it composes, and the source.
///
/// The source is shown because `ADR-0023` Decision 4 makes creating one a grant: a grant nobody
/// can review is a grant nobody should give.
fn show(server: &Server) -> Result<QueryResult, QueryFailure> {
    use sankhya_api_pg::message::{oid, FieldDescription};

    let rows: Vec<Vec<Option<String>>> = server
        .aggregations()
        .iter()
        .map(|aggregation| {
            vec![
                Some(aggregation.name.clone()),
                Some(if aggregation.composes { "yes" } else { "no" }.to_owned()),
                Some(aggregation.source.clone()),
            ]
        })
        .collect();

    Ok(QueryResult {
        fields: vec![
            FieldDescription::text("aggregation", oid::TEXT, -1),
            FieldDescription::text("composes", oid::TEXT, -1),
            FieldDescription::text("source", oid::TEXT, -1),
        ],
        rows,
        tag: "SHOW".to_owned(),
    })
}

/// How a cube measure declared with `AGGREGATION <name>` is computed.
///
/// A closure over the declared set and the worker, so `sankhya-cube-sql` knows nothing
/// about processes or interpreters --- only that some rules are computed by somebody else,
/// and what to hand them.
///
/// `None` where nothing has been declared or the boundary cannot be built, and a cube that
/// names one then refuses **by name** rather than answering. A measure that quietly
/// answered something else would be a number of the right magnitude and no meaning, which
/// is the failure the whole additivity model exists to prevent.
pub(crate) fn supplied_rules(server: &Server) -> Option<sankhya_cube_sql::functions::Supplied> {
    let declared = server.aggregations();
    if declared.is_empty() {
        return None;
    }
    let worker = server.worker().ok()?;
    Some(Arc::new(move |name: &str, values: &[f64]| {
        let Some(aggregation) = declared.iter().find(|held| held.name == name) else {
            return Err(format!(
                "this server has no aggregation called `{name}`. `SHOW AGGREGATIONS` \
                 lists what it has"
            ));
        };
        let state = worker
            .accumulate(aggregation, &[], values)
            .map_err(|refused| refused.to_string())?;
        worker.finish(aggregation, &state).map_err(|refused| refused.to_string())
    }))
}

/// Register every declared aggregation against a session.
///
/// Registered per session rather than once, because a session is what a statement is planned
/// against and an aggregation declared a moment ago must be callable by the next statement.
pub(crate) fn register(server: &Server, context: &SessionContext) {
    let declared = server.aggregations();
    if declared.is_empty() {
        return;
    }
    let Ok(worker) = server.worker() else {
        // Declared and unrunnable. Not registered, so a statement calling one is refused by
        // name --- which is a better answer than a function that plans and then fails per
        // batch inside a scan.
        return;
    };
    for aggregation in declared.iter() {
        context.register_udaf(sankhya_olap::supplied::Supplied::new(
            Arc::new(aggregation.clone()),
            Arc::clone(&worker),
        ));
    }
}

//! `CREATE CUBE` and `DROP CUBE`, and what a declared fact query obliges.
//!
//! Split out of `wiring.rs` when that file reached its length limit, and it is the right seam:
//! everything here is *catalogue* work --- a definition validated, its dependencies resolved,
//! the file written, the cuboids reclaimed --- and none of it is on the query path.
//!
//! # The one idea the whole file turns on
//!
//! A cube's facts may be a table's name or a **declared query**, and
//! [ADR-0012](../../../docs/adr/0012-open-capabilities.md) makes the second safe with a single
//! rule: *an artefact that cannot say what it needs cannot be cached correctly, checked against
//! policy, or bounded.* A query's text does not say what it needs, so it is planned once, under
//! the caller's own guard, and what it reads is recorded. Everything downstream then treats
//! those tables exactly as it treated the fact table.

use crate::wiring::{acknowledged, refusal, Server};
use sankhya_api_pg::session::{QueryFailure, QueryResult};
use sankhya_authz::policy::{Action, TableRef};
use sankhya_authz::principal::Principal;
use std::sync::Arc;

/// Run a `CREATE CUBE` or `DROP CUBE`.
///
/// # Why the same failure is reported for "no such table" and "you may not read it"
///
/// Because they must be indistinguishable. The query path already refuses to confirm a
/// table's existence to somebody who may not read it, and cube DDL naming a fact table
/// would be a way to ask the same question through a different door: a `CREATE CUBE` that
/// answered *"you may not read `payroll`"* has told you `payroll` exists.
///
/// So the check is [`Self::scope_for`] --- the same authorization the query path uses,
/// with no second implementation to disagree with it --- and both answers are the one
/// sentence below.
pub(crate) fn run_ddl(
    server: &Server,
    statement: Result<sankhya_cube_sql::Statement, sankhya_cube_sql::DdlError>,
    principal: &Principal,
) -> Result<QueryResult, QueryFailure> {
    use sankhya_error::protocol::sqlstate;

    let statement = statement.map_err(|error| {
        refusal(sqlstate::SYNTAX_ERROR.as_str(), &error.to_string())
    })?;

    match statement {
        sankhya_cube_sql::Statement::Create(definition) => {
            create(server, *definition, principal)
        }
        sankhya_cube_sql::Statement::Drop { name, if_exists } => {
            remove(server, &name, if_exists, principal)
        }
    }
}

/// The tables a declared fact query reads, resolved by planning it.
///
/// # Why the planner rather than a scan of the text
///
/// Because the text does not say. `FROM orders` is a different table under a different
/// search path, a CTE named `orders` is not a table at all, and a name inside a string
/// literal is not a reference. The planner already answers all three, and it answers them
/// the same way the query itself will be answered --- which is the property that matters,
/// because a dependency list that disagrees with what the query actually reads is worse
/// than none: it authorizes and invalidates against the wrong tables, confidently.
fn tables_read_by(
    server: &Server,
    query: &str,
    principal: &Principal,
) -> Result<Vec<String>, QueryFailure> {
    use datafusion::common::tree_node::{TreeNode, TreeNodeRecursion};
    use datafusion::logical_expr::LogicalPlan;
    use sankhya_error::protocol::sqlstate;

    let servable = Arc::clone(&server.servable.read());
    let (context, _, _) = crate::execute::session_and_contested(
        principal,
        &server.policy,
        &servable,
    )?;
    let statement = format!("SELECT * FROM {query} AS facts");
    let plan = tokio::task::block_in_place(|| {
        server.runtime.block_on(context.state().create_logical_plan(&statement))
    })
    .map_err(|error| {
        refusal(
            sqlstate::DATA_EXCEPTION.as_str(),
            &format!(
                "this cube's fact query could not be planned, so nothing knows what it \
                 reads: {error}"
            ),
        )
    })?;

    let mut reads: Vec<String> = Vec::new();
    let _ = plan.apply(|node| {
        if let LogicalPlan::TableScan(scan) = node {
            let name = scan.table_name.to_string();
            if !reads.contains(&name) {
                reads.push(name);
            }
        }
        Ok(TreeNodeRecursion::Continue)
    });
    // Sorted, because this is hashed into the definition's fingerprint and the planner's
    // traversal order is not part of what a cube means. Two identical cubes must have the
    // same version whichever order their scans came back in.
    reads.sort();
    Ok(reads)
}

/// Validate a definition, persist it, and start serving it.
fn create(
    server: &Server,
    definition: sankhya_cube::model::Definition,
    principal: &Principal,
) -> Result<QueryResult, QueryFailure> {
    use sankhya_error::protocol::sqlstate;

    let name = definition.name.clone();

    // A name already taken is refused rather than replaced, and there is no
    // `OR REPLACE`. Replacing a cube orphans every cuboid it materialised, and the
    // reclamation of those is a real operation with a real cost --- see
    // `cuboid::retire_cube`. Hiding that inside a `CREATE` would make an expensive,
    // irreversible thing happen because somebody re-ran a script. `DROP` then `CREATE`
    // says it out loud.
    if server.cubes().iter().any(|cube| cube.name() == name) {
        return Err(refusal(
            sqlstate::DATA_EXCEPTION.as_str(),
            &format!(
                "the cube `{name}` already exists. Drop it first: replacing a cube \
                 retires every cuboid it materialised, which is not something a \
                 re-run of a script should do silently"
            ),
        ));
    }

    // A declared query says what it reads by being planned. Resolved here, once, under
    // **this caller's guard** --- the session registers only the tables they may read, so
    // a query naming one they may not fails to resolve, and the dependency list that comes
    // out is by construction a list of tables they were allowed to see.
    //
    // `ADR-0012`: an artefact that cannot say what it needs cannot be cached correctly,
    // checked against policy, or bounded. This is where it says it.
    let mut definition = definition;
    if definition.fact_is_a_query() {
        let reads = tables_read_by(server, &definition.fact_table, principal)?;
        definition.reads = reads;
    }

    // Every table the cube reads, checked against the same authorization the query path
    // uses. A cube whose fact table this principal cannot read would hydrate to nothing
    // anyway; refusing here means the refusal names the statement rather than arriving
    // later as an empty answer nobody can explain.
    let mut tables = definition.reads.clone();
    tables.extend(definition.dimensions.iter().map(|d| d.table.clone()));
    for table in tables {
        if server.scope_for(principal, &table).is_none() {
            return Err(refusal(
                sqlstate::DATA_EXCEPTION.as_str(),
                &format!("there is no table `{table}` to build a cube on"),
            ));
        }
    }

    // The one validator, reporting every rejection rather than the first. A definition
    // fixable in one sitting should be reported in one message.
    let cube = definition.validate().map_err(|rejections| {
        let why: Vec<String> = rejections.iter().map(ToString::to_string).collect();
        refusal(
            sqlstate::DATA_EXCEPTION.as_str(),
            &format!("the cube `{name}` was not created: {}", why.join("; ")),
        )
    })?;

    // Persisted before it is served, so a cube that answers a query is a cube that would
    // survive a restart. The other order produces a cube that works until it does not,
    // and the moment it stops is a restart nobody connects to the statement.
    sankhya_cube::catalogue::save(&server.settings.warehouse, cube.definition()).map_err(
        |error| refusal(sqlstate::IO_ERROR.as_str(), &error.to_string()),
    )?;

    if let Ok(mut cubes) = server.cubes.write() {
        let mut next: Vec<_> = cubes.iter().cloned().collect();
        next.push(cube);
        *cubes = Arc::new(next);
    }

    // Audited as an insert against the cube's own name. There is no `Action` for DDL
    // and inventing one would mean a second vocabulary for the audit reader to learn;
    // creating a cube adds something that was not there, which is what `Insert` says.
    server.record(principal, TableRef::new("", &name), Action::Insert, true);
    Ok(acknowledged("CREATE CUBE"))
}

/// Stop serving a cube, remove its definition, and reclaim what it materialised.
fn remove(
    server: &Server,
    name: &str,
    if_exists: bool,
    principal: &Principal,
) -> Result<QueryResult, QueryFailure> {
    use sankhya_error::protocol::sqlstate;

    let Some(cube) = server.cubes().iter().find(|cube| cube.name() == name).cloned() else {
        if if_exists {
            return Ok(acknowledged("DROP CUBE"));
        }
        return Err(refusal(
            sqlstate::DATA_EXCEPTION.as_str(),
            &format!("there is no cube `{name}`"),
        ));
    };

    // Dropping a cube reads no table, so there is nothing to authorize against a fact
    // table --- but a principal who cannot read what the cube is built on has no business
    // removing it, and the check costs nothing. The refusal is the same sentence as
    // everywhere else, for the same reason.
    if server.scope_across(principal, cube.reads()).is_none() {
        return Err(refusal(
            sqlstate::DATA_EXCEPTION.as_str(),
            &format!("there is no cube `{name}`"),
        ));
    }

    // Out of the served set first, so no statement started after this point can resolve
    // the cube and reach files that are about to go. A statement already running holds
    // its files open and finishes against them.
    if let Ok(mut cubes) = server.cubes.write() {
        let next: Vec<_> =
            cubes.iter().filter(|held| held.name() != name).cloned().collect();
        *cubes = Arc::new(next);
    }

    let definition = sankhya_cube::catalogue::path_of(&server.settings.warehouse, name);
    if let Err(error) = std::fs::remove_file(&definition) {
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(refusal(sqlstate::IO_ERROR.as_str(), &error.to_string()));
        }
    }

    // And the cuboids, which nothing else will ever reclaim: `retire_superseded` keeps a
    // cuboid whose cube has no known current version, deliberately and with a reason, so
    // a dropped cube's materialised storage would otherwise be retained for good.
    let swept = sankhya_maintenance::cuboid::retire_cube(&server.settings.warehouse, name);
    if !swept.removed.is_empty() {
        tracing::info!(
            cube = name,
            cuboids = swept.removed.len(),
            bytes = swept.bytes_reclaimed,
            "retired the cuboids of a dropped cube"
        );
    }

    server.record(principal, TableRef::new("", name), Action::Delete, true);
    Ok(acknowledged("DROP CUBE"))
}

//! Snapshots a deployment has taken: where they live, and the statements that manage them.
//!
//! # Where they live, and why in the warehouse
//!
//! A document per snapshot under `_snapshots/`, beside `_cubes/`. In the warehouse rather than
//! beside it, because a backup that copied the tables and not the snapshots would restore a
//! warehouse whose reports cannot be reproduced --- which is the one thing a snapshot exists
//! for.
//!
//! Durable rather than in memory, for the same reason: the overnight run that quotes a snapshot
//! is not the process that took it.
//!
//! # What is built here and what is not
//!
//! Taking, listing and dropping. **Reading as of one is not built yet**, and `SET SNAPSHOT` is
//! refused rather than accepted, per `ADR-0019` Decision 6 --- accepting it as a no-op would
//! serve the present to a caller who asked for one instant, which is the worst thing this
//! system can do.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sankhya_api_pg::session::{QueryFailure, QueryResult};
use sankhya_snapshot::expire::{standing, Standing};
use sankhya_snapshot::model::{Pinned, Snapshot};
use sankhya_snapshot::statement::Statement;

use sankhya_authz::policy::{Action, TableRef};
use sankhya_authz::principal::Principal;
use sankhya_catalog::guard::Guard;

use crate::wiring::{acknowledged, refusal, Server};

/// The bookkeeping schema snapshot documents live under.
///
/// `_`-prefixed, so `warehouse::discover` skips it: a snapshot is not a user table and must not
/// appear in a catalogue somebody browses.
pub(crate) const DIRECTORY: &str = "_snapshots";

/// Where a snapshot of this name is stored, or a refusal a client can read.
///
/// Fallible, and that is the point rather than an inconvenience. This used to be
/// `warehouse.join(DIRECTORY).join(format!("{name}.json"))` over a name the statement supplied
/// with no constraint beyond being non-empty and whitespace-free, and `Path::join` honours
/// `..` --- so `DROP SNAPSHOT ../_cubes/regional` deleted a cube definition. `SEC-06`.
///
/// **A snapshot document is the worst target of the three**, and not because a snapshot is
/// precious. An absent snapshot pins nothing, so deleting one releases the files the sweeper
/// was holding back: the deletion this whole retention mechanism exists to prevent, arriving
/// through the name of a `DROP` statement.
///
/// Returning a `Result` is what stops that coming back --- there is no way to obtain the path
/// without having asked.
fn document_at(warehouse: &Path, name: &str) -> Result<PathBuf, QueryFailure> {
    let checked = sankhya_atomicfs::name::checked(name).map_err(|refused| {
        refusal(
            sankhya_error::protocol::sqlstate::DATA_EXCEPTION.as_str(),
            &format!("`{name}` is not a usable snapshot name: {refused}"),
        )
    })?;
    Ok(warehouse.join(DIRECTORY).join(format!("{checked}.json")))
}

/// Every snapshot this warehouse holds, with the ones that could not be read.
///
/// A document that cannot be read is **reported, never skipped**. A snapshot nobody can read
/// still pins files, and treating it as absent would let the sweeper reclaim them under a
/// reader --- which is the deletion this whole mechanism is gated on, arriving through a parse
/// failure.
pub(crate) fn load(warehouse: &Path) -> (Vec<Snapshot>, Vec<String>) {
    let directory = warehouse.join(DIRECTORY);
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        // No directory is the ordinary case for every warehouse that has taken none, and it
        // is the *only* case that may be reported as "there are none". A directory that
        // exists and cannot be read is a snapshot set nobody can see, and answering "none"
        // for it is how a pin disappears without anybody being told.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return (Vec::new(), Vec::new())
        }
        Err(error) => {
            return (
                Vec::new(),
                vec![format!("{}: {error}", directory.display())],
            )
        }
    };

    let mut found = Vec::new();
    let mut complaints = Vec::new();
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect();
    paths.sort();

    for path in paths {
        match std::fs::read_to_string(&path)
            .map_err(|error| error.to_string())
            .and_then(|text| Snapshot::from_document(&text).map_err(|error| error.to_string()))
        {
            Ok(snapshot) => found.push(snapshot),
            Err(why) => complaints.push(format!("{}: {why}", path.display())),
        }
    }
    (found, complaints)
}

/// One snapshot document, or `None` if it cannot be read as one.
///
/// Separate from [`load`] because a decision about *this* snapshot must not depend on whether
/// every other document in the directory parses. `load` reports its complaints and a caller
/// dropping one snapshot has no business being refused by another one's corruption.
fn read_one(path: &Path) -> Option<Snapshot> {
    let text = std::fs::read_to_string(path).ok()?;
    Snapshot::from_document(&text).ok()
}

/// The versions snapshots still pin, by qualified table name.
///
/// # Why the sweeper asks this
///
/// Reclamation already has exactly one question --- *"does anything still read this?"* --- which
/// `Lineages::pinned_versions` answers for clones. This is a second thing that can answer yes,
/// unioned with the first. It is deliberately **not** a second reclamation rule: two rules
/// disagree eventually, and the one that loses deletes a file somebody is reading.
///
/// An **expired** snapshot pins nothing. Expiry detaches; retirement reclaims afterwards on its
/// own grace period, so an expiry that should not have happened stays reversible for as long as
/// that period lasts.
#[must_use]
pub(crate) fn pinned_versions(
    snapshots: &[Snapshot],
    today: i32,
) -> BTreeMap<String, Vec<u64>> {
    let mut pinned: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    for snapshot in snapshots {
        if standing(snapshot, today) == Standing::Expired {
            continue;
        }
        for (table, at) in &snapshot.tables {
            let versions = pinned.entry(table.clone()).or_default();
            if !versions.contains(&at.version) {
                versions.push(at.version);
            }
        }
    }
    pinned
}

/// Take a snapshot of every table `readable` names, at the version each stands at.
///
/// `readable` is what the **caller** may read, resolved by the caller: a snapshot that recorded
/// a table its taker could not read would disclose that table's existence to everyone who can
/// list snapshots, which is the leak this system refuses everywhere else arriving through a
/// bookkeeping document.
///
/// # Errors
///
/// [`QueryFailure`] when a snapshot of that name already exists, or when the document cannot be
/// written. An existing name is refused rather than replaced: replacing one would silently move
/// the instant every report quoting it reads from.
/// Every table this principal may read, at versions that were **simultaneously** current.
///
/// # Why one pass is not one instant
///
/// A snapshot's whole claim is that the tables in it agree with each other: two figures quoted
/// from one snapshot describe the same moment. Reading each table's version in a loop cannot
/// deliver that. A writer committing between two of the reads leaves one table recorded before
/// its commit and another after it, and the snapshot then describes a state the warehouse was
/// never in. `COR-22`.
///
/// The old comment called it *"as close to one instant as this can make them"*, which is honest
/// about the mechanism and does not match what the feature says it does.
///
/// # Verified rather than prevented
///
/// There is no warehouse-wide commit sequence to read, and taking a lock over every table would
/// put a writer behind a reader — which this system refuses everywhere else for good reason.
///
/// So the set is **confirmed**: read every version, read them all again, and accept only if
/// nothing moved. A set that is identical across the window was valid throughout it, and that
/// is exactly what "one instant" means. A warehouse busy enough that two consecutive passes
/// never agree gets a refusal rather than a snapshot that is quietly not one — the same choice
/// this system makes wherever it cannot deliver what it says.
fn at_one_instant(
    server: &crate::wiring::Server,
    principal: &Principal,
) -> Result<Vec<(String, u64)>, QueryFailure> {
    /// Enough that an ordinary commit landing mid-pass is retried through, and few enough that
    /// a warehouse under continuous write load is told rather than waited on.
    const ATTEMPTS: usize = 5;

    let once = || -> Vec<(String, u64)> {
        let servable = server.servable_now();
        servable
            .iter()
            .filter(|table| {
                let authority = table.authorize_as.as_ref().unwrap_or(&table.reference);
                Guard::authorize(server.policy_set(), principal, authority, Action::Read).is_some()
            })
            .filter_map(|table| {
                let qualified =
                    crate::warehouse::qualified_name(&server.warehouse_path(), &table.root)?;
                let version = sankhya_table_delta::live_files(&table.root)
                    .ok()
                    .and_then(|live| live.version)?;
                Some((qualified, version))
            })
            .collect()
    };

    let mut moved = 0;
    for _ in 0..ATTEMPTS {
        let first = once();
        let second = once();
        if first == second {
            return Ok(first);
        }
        moved += 1;
    }
    Err(refusal(
        sankhya_error::protocol::sqlstate::SERIALIZATION_FAILURE.as_str(),
        &format!(
            "this warehouse committed during every one of {moved} attempts to read its tables \
             at one instant, so a snapshot taken now would record versions that were never \
             current together. Refused rather than taken: two figures quoted from one snapshot \
             are supposed to describe the same moment. Try again, or take it when writing is \
             quieter"
        ),
    ))
}

pub(crate) fn take(
    warehouse: &Path,
    name: &str,
    taken_by: &str,
    taken_at: i64,
    expires_on: i32,
    readable: &[(String, u64)],
) -> Result<QueryResult, QueryFailure> {
    let path = document_at(warehouse, name)?;
    if path.exists() {
        return Err(refusal(
            "42P07",
            &format!(
                "a snapshot called `{name}` already exists. Refused rather than replaced: \
                 replacing it would silently move the instant every report quoting it reads \
                 from. Drop it first, or take one under another name"
            ),
        ));
    }

    let tables: BTreeMap<String, Pinned> = readable
        .iter()
        .map(|(table, version)| (table.clone(), Pinned { version: *version }))
        .collect();
    let snapshot = Snapshot::new(name, taken_by, taken_at, expires_on, tables);
    let document = snapshot.to_document().map_err(|error| {
        refusal(
            sankhya_error::protocol::sqlstate::INTERNAL_ERROR.as_str(),
            &error.to_string(),
        )
    })?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            refusal(
                sankhya_error::protocol::sqlstate::IO_ERROR.as_str(),
                &format!("the snapshot directory could not be created: {error}"),
            )
        })?;
    }
    // Through the atomic helper, like every other write here: a half-written snapshot document
    // is one the sweeper would refuse to read, which stops reclamation warehouse-wide.
    sankhya_atomicfs::publish(&path, document.as_bytes()).map_err(|error| {
        refusal(
            sankhya_error::protocol::sqlstate::IO_ERROR.as_str(),
            &format!("the snapshot could not be written: {error}"),
        )
    })?;
    Ok(acknowledged("CREATE SNAPSHOT"))
}

/// Remove a snapshot, releasing what it pinned.
///
/// # Errors
///
/// [`QueryFailure`] when there is no such snapshot and the statement did not say `IF EXISTS`,
/// or when the document cannot be removed.
pub(crate) fn drop_it(
    server: &Server,
    principal: &Principal,
    name: &str,
    if_exists: bool,
) -> Result<QueryResult, QueryFailure> {
    let warehouse = &server.warehouse_path();
    let path = document_at(warehouse, name)?;
    let absent = || {
        refusal(
            "42704",
            &format!(
                "there is no snapshot called `{name}`. `SHOW SNAPSHOTS` lists them, with what \
                 each pins and when it expires"
            ),
        )
    };
    if !path.exists() {
        if if_exists {
            return Ok(acknowledged("DROP SNAPSHOT"));
        }
        return Err(absent());
    }

    // Who may drop it.
    //
    // # Why this was worth a finding
    //
    // `DROP SNAPSHOT` took no principal at all, so any caller could drop any snapshot. A
    // snapshot's whole job is to hold files back from the sweeper, so dropping one releases
    // them --- and `docs/INVARIANTS.md` claims the maintenance scheduler is *structurally
    // incapable* of destroying retained history. It is. A statement was doing it instead.
    // `SEC-04`.
    //
    // # The rule
    //
    // The subject who took it, or a caller who may read every table it pins. The second half
    // is what keeps an operator able to clean up after somebody who has left, and it is the
    // same rule `DROP CUBE` follows: a principal who cannot read what a thing is built on has
    // no business removing it.
    //
    // A snapshot document that cannot be read is **refused rather than dropped**. Not being
    // able to tell what it pins is not permission to release it.
    let held = read_one(&path).ok_or_else(|| {
        refusal(
            sankhya_error::protocol::sqlstate::DATA_EXCEPTION.as_str(),
            &format!(
                "the snapshot document for `{name}` could not be read, so what it pins is \
                 unknown. Refused rather than dropped: dropping it would release files whose \
                 readers cannot be identified"
            ),
        )
    })?;
    let pins: Vec<String> = held.tables.keys().cloned().collect();
    let mine = held.taken_by == principal.subject();
    if !mine && server.scope_across(principal, &pins).is_none() {
        // The same sentence as absence, deliberately. Saying "you may not drop that" confirms
        // it exists, and a snapshot's name is a thing somebody chose.
        server.record(principal, TableRef::new("", name), Action::Delete, false);
        return Err(absent());
    }
    server.record(principal, TableRef::new("", name), Action::Delete, true);

    std::fs::remove_file(&path).map_err(|error| {
        refusal(
            sankhya_error::protocol::sqlstate::IO_ERROR.as_str(),
            &format!("the snapshot could not be removed: {error}"),
        )
    })?;
    Ok(acknowledged("DROP SNAPSHOT"))
}

/// Answer `SHOW SNAPSHOTS`.
///
/// Reports what each one pins and when it expires, because a snapshot holds storage on
/// somebody's behalf and a cost with no visible owner is the shape `RSK-35` describes.
pub(crate) fn show(snapshots: &[Snapshot], today: i32) -> QueryResult {
    use sankhya_api_pg::message::{oid, FieldDescription};

    let rows = snapshots
        .iter()
        .map(|snapshot| {
            let state = match standing(snapshot, today) {
                Standing::Live => "live",
                Standing::Expired => "expired",
            };
            vec![
                Some(snapshot.name.clone()),
                Some(state.to_owned()),
                Some(snapshot.taken_by.clone()),
                Some(snapshot.taken_at.to_string()),
                Some(snapshot.expires_on.to_string()),
                Some(snapshot.len().to_string()),
                Some(snapshot.names().collect::<Vec<&str>>().join(" ")),
            ]
        })
        .collect::<Vec<_>>();
    let tag = format!("SELECT {}", rows.len());
    QueryResult {
        fields: vec![
            FieldDescription::text("snapshot", oid::TEXT, -1),
            FieldDescription::text("state", oid::TEXT, -1),
            FieldDescription::text("taken_by", oid::TEXT, -1),
            FieldDescription::text("taken_at", oid::INT8, 8),
            FieldDescription::text("expires_on", oid::INT4, 4),
            FieldDescription::text("tables", oid::INT8, 8),
            FieldDescription::text("pins", oid::TEXT, -1),
        ],
        rows,
        tag,
    }
}

/// Which statement this is, for a caller that has already parsed it.
pub(crate) fn describe(statement: &Statement) -> &'static str {
    match statement {
        Statement::Create { .. } => "CREATE SNAPSHOT",
        Statement::Show => "SHOW SNAPSHOTS",
        Statement::Drop { .. } => "DROP SNAPSHOT",
        Statement::History { .. } => "SHOW HISTORY",
        Statement::Changes { .. } => "SHOW CHANGES",
        Statement::ReadVersion { .. } => "SET VERSION",
    }
}

/// Take, list or drop a snapshot.
///
/// # What a snapshot may name
///
/// Only tables **this caller** may read. A snapshot that recorded a table its taker could
/// not read would disclose that table's existence to everyone who can list snapshots ---
/// the leak this system refuses everywhere else, arriving through a bookkeeping document.
///
/// It follows that two people taking a snapshot of "the warehouse" a moment apart may
/// record different sets, and that is correct: each records the instant *they* could see.
pub(crate) fn run_statement(
    server: &crate::wiring::Server,
    statement: Result<sankhya_snapshot::Statement, sankhya_snapshot::NotAStatement>,
    principal: &Principal,
) -> Result<QueryResult, QueryFailure> {
    use sankhya_error::protocol::sqlstate;
    use sankhya_snapshot::Statement;

    let statement = match statement {
        Ok(statement) => statement,
        Err(error) => {
            return Err(refusal(sqlstate::SYNTAX_ERROR.as_str(), &error.to_string()))
        }
    };
    let warehouse = &server.warehouse_path();

    match statement {
        Statement::Show => {
            let (snapshots, complaints) = load(warehouse);
            // A document that cannot be read is reported, never skipped: a snapshot nobody
            // can read still pins files, and listing the rest as though it were absent
            // would tell an operator the warehouse holds less than it does.
            if let Some(first) = complaints.first() {
                return Err(refusal(
                    sqlstate::DATA_EXCEPTION.as_str(),
                    &format!(
                        "{} snapshot document(s) could not be read, beginning with {first}. \
                         Refused rather than listing the rest: an unreadable snapshot still \
                         pins files, and reporting it as absent would understate what this \
                         warehouse is holding",
                        complaints.len()
                    ),
                ));
            }
            Ok(show(&snapshots, server.today()))
        }
        Statement::Drop { name, if_exists } => {
            drop_it(server, principal, &name, if_exists)
        }
        Statement::History { table } => history_of(server, &table, principal),
        Statement::Changes { table, from, to } => {
            changes_between(server, &table, from, to, principal)
        }
        // Answered as an acknowledgement; the session remembers it and the read path consults
        // it, exactly as `SET SNAPSHOT` does. Checked here so that a version this table can no
        // longer produce is refused at the `SET` rather than at the next query.
        Statement::ReadVersion { table, version } => {
            read_version(server, &table, version, principal)
        }
        Statement::Create { name, expiry } => {
            let readable = at_one_instant(server, principal)?;
            let taken = take(
                warehouse,
                &name,
                principal.subject(),
                server.now_micros(),
                expiry.falls_on(server.today()),
                &readable,
            )?;
            // Recorded as a **read**, which is what it is: taking a snapshot reads the
            // version of every table the caller may read and writes none of them. There is
            // no `Action::Create`, and inventing one for a bookkeeping document would put
            // a second vocabulary in the audit for something the first already describes.
            server.record(principal, TableRef::new("", &name), Action::Read, true);
            Ok(taken)
        }
    }
}

/// What still reads each table: clone lineage, and the versions live snapshots pin.
///
/// # Why both are answered here
///
/// Reclamation asks exactly **one** question --- *"does anything still read this?"* --- and
/// there are two things that can answer yes. Answering them in one place means the sweeper
/// has one input rather than two rules, and two rules disagree eventually: the one that
/// loses deletes a file somebody is reading.
///
/// # What a hole in the answer means, which was the wrong thing
///
/// A snapshot document that will not parse, and a snapshot naming a table that resolves to
/// two, both used to be **dropped with the complaint discarded**. The result was a smaller
/// answer, and a smaller answer here reads to the sweeper as *"fewer files are pinned"* ---
/// so the one case where this code did not know what a snapshot protected was the case in
/// which its files were reclaimed.
///
/// An earlier comment here argued the other way: refusing warehouse-wide for one bad file
/// was called a worse failure than skipping it. That weighed a full disk against lost rows
/// and got the order backwards. Reclamation that stops is visible in a report, costs storage,
/// and is fixed by deleting one file. Reclamation that runs on an unknown pin set is visible
/// when somebody queries, and nothing fixes it.
///
/// So a hole is **carried** rather than dropped: named in
/// [`StillReading::unreadable`](sankhya_maintenance::StillReading::unreadable), where it stops
/// the sweeper's two deleting paths and appears in the tick report. Compaction continues,
/// because compaction adds files and removes none.
#[must_use]
pub(crate) fn still_reading(server: &crate::wiring::Server) -> sankhya_maintenance::StillReading {
    let (snapshots, complaints) = load(server.warehouse_path());
    let pinned = pinned_versions(&snapshots, server.today());

    // Keyed by root path, because that is what the sweeper has and it removes every
    // question about which naming a key is in.
    let mut by_root = std::collections::BTreeMap::new();
    let mut unreadable = complaints;
    for (qualified, versions) in pinned {
        match crate::warehouse::resolve(server.warehouse_path(), &qualified) {
            crate::warehouse::Resolved::One(root) => {
                by_root.insert(root, versions);
            }
            // Not a hole. The table this snapshot named is gone, so there is nothing left for
            // it to pin, and there are no files to protect from anybody.
            crate::warehouse::Resolved::Absent => {}
            crate::warehouse::Resolved::Ambiguous(candidates) => unreadable.push(format!(
                "the snapshot of `{qualified}` names {} tables ({}), so which files it pins                  cannot be established",
                candidates.len(),
                candidates.join(", ")
            )),
        }
    }
    sankhya_maintenance::StillReading {
        clones: server.lineages(),
        snapshots: by_root,
        unreadable,
    }
}

/// What is keeping each version of `root` alive, named.
///
/// # Why both kinds are in one map
///
/// Reclamation asks exactly one question --- *"does anything still read this?"* --- and a
/// snapshot and a clone are two ways of answering yes. Reporting them in two columns would
/// invite a reader to treat one as more binding than the other, and the sweeper does not.
fn keepers_of(
    server: &crate::wiring::Server,
    root: &std::path::Path,
) -> BTreeMap<u64, std::collections::BTreeSet<String>> {
    let mut keepers: BTreeMap<u64, std::collections::BTreeSet<String>> = BTreeMap::new();

    let (snapshots, _complaints) = load(server.warehouse_path());
    let today = server.today();
    for snapshot in &snapshots {
        if standing(snapshot, today) == Standing::Expired {
            continue;
        }
        for (table, at) in &snapshot.tables {
            // Resolved to a root rather than compared by name: the snapshot records a
            // qualified name and the caller may have asked by a bare one, and a column that
            // only worked for one spelling is a column that is wrong half the time.
            if let crate::warehouse::Resolved::One(pinned_root) =
                crate::warehouse::resolve(server.warehouse_path(), table)
            {
                if pinned_root == root {
                    keepers.entry(at.version).or_default().insert(snapshot.name.clone());
                }
            }
        }
    }

    if let Some(qualified) = crate::warehouse::qualified_name(server.warehouse_path(), root) {
        for (version, clones) in server.lineages().keepers_of(&qualified) {
            keepers.entry(version).or_default().extend(clones);
        }
    }

    keepers
}

/// The tables this session sees, when it has asked to read as of a named snapshot.
///
/// `None` when it has not, which is every session until somebody says so --- and the reason
/// this costs nothing until they do.
///
/// # Why a table the snapshot does not name is left out rather than checked
///
/// `ADR-0019` Decision 2 says such a table is refused. It is enforced here by **absence**: the
/// table is simply not registered, so a statement naming it fails to resolve, in the same
/// words as a table that is not there. A check somewhere downstream would be a second place
/// the rule lives, and the day somebody adds a third path into the read path it would be the
/// place they forgot.
///
/// # Errors
///
/// [`QueryFailure`] when the named snapshot does not exist, has expired, or cannot be read.
/// Never a fall back to the present: a caller who asked for one instant and was served *now*
/// has no way to tell.
pub(crate) fn as_of(
    server: &crate::wiring::Server,
    caller: &sankhya_api_pg::session::Caller<'_>,
) -> Result<Option<std::sync::Arc<Vec<crate::execute::ServableTable>>>, QueryFailure> {
    let Some(named) = caller.setting("snapshot") else {
        // No snapshot, but perhaps a version pinned for one table.
        return at_versions(server, caller);
    };
    if named.is_empty() {
        return at_versions(server, caller);
    }

    let (snapshots, complaints) = load(server.warehouse_path());
    if let Some(first) = complaints.first() {
        return Err(refusal(
            sankhya_error::protocol::sqlstate::DATA_EXCEPTION.as_str(),
            &format!(
                "a snapshot document could not be read ({first}), so this session cannot be \
                 sure what `{named}` pins. Refused rather than answered from what is legible"
            ),
        ));
    }
    let Some(snapshot) = snapshots.into_iter().find(|held| held.name == named) else {
        return Err(refusal(
            "42704",
            &format!(
                "there is no snapshot called `{named}`. `SHOW SNAPSHOTS` lists them, with what \
                 each pins and when it expires"
            ),
        ));
    };
    if standing(&snapshot, server.today()) == Standing::Expired {
        return Err(refusal(
            "42704",
            &sankhya_snapshot::expire::expired_message(&snapshot),
        ));
    }

    // Resolved at the pinned version, one table at a time. Not cached: a version is a fixed
    // point, and the cache answers what a table looks like *now*.
    let mut pinned = Vec::new();
    for table in server.servable_now().iter() {
        let Some(qualified) =
            crate::warehouse::qualified_name(server.warehouse_path(), &table.root)
        else {
            continue;
        };
        let Some(at) = snapshot.pins(&qualified) else {
            // Not named by this snapshot: it did not exist when the snapshot was taken, so it
            // is left out and a statement naming it fails to resolve.
            continue;
        };
        let resolved = sankhya_readpath::resolve_as_of(
            std::sync::Arc::clone(&table.schema),
            &table.root,
            at.version,
            sankhya_types::LsnRange::new(sankhya_types::Lsn::new(0), server.read_as_of()),
            server.read_as_of(),
        )
        .map_err(|error| {
            refusal(
                sankhya_error::protocol::sqlstate::DATA_EXCEPTION.as_str(),
                &format!(
                    "`{qualified}` could not be read as of version {} --- the snapshot \
                     `{named}` pins a version this table can no longer produce: {error}",
                    at.version
                ),
            )
        })?;
        pinned.push(crate::execute::ServableTable {
            reference: table.reference.clone(),
            authorize_as: table.authorize_as.clone(),
            inherited: table.inherited.clone(),
            root: table.root.clone(),
            provider: std::sync::Arc::new(resolved),
            schema: std::sync::Arc::clone(&table.schema),
            resolved_at: at.version,
        });
    }
    Ok(Some(std::sync::Arc::new(pinned)))
}

/// Check a `SET SNAPSHOT` before the session remembers it.
///
/// `None` when the statement is not one, so the caller passes it on.
///
/// # Why this is checked here and not at the next statement
///
/// Because the next statement is the wrong place to learn about it. A `SET` that succeeded and
/// a query that then failed sends somebody to look at the query --- the same reasoning
/// `ADR-0017` Decision 5 applies to version skew: fail where a person can act, not where the
/// consequence happens to be noticed.
pub(crate) fn check_setting(
    server: &crate::wiring::Server,
    sql: &str,
) -> Option<Result<QueryResult, QueryFailure>> {
    // The **same** parser the wire layer remembers settings with. There were two, and both
    // split on whitespace, so both read `SET SNAPSHOT='eod'` as a verb and one word --- this
    // one then decided the setting was not called `snapshot` and let the statement fall
    // through to the handler that accepts any `SET` as a no-op. Acknowledged, unvalidated,
    // and with no effect: `CLI-07`.
    let setting = sankhya_api_pg::setting::parse(sql)?;
    if setting.reset || !setting.name.eq_ignore_ascii_case("snapshot") {
        return None;
    }
    let wanted = setting.value;
    if wanted.is_empty() {
        return Some(Err(refusal(
            sankhya_error::protocol::sqlstate::SYNTAX_ERROR.as_str(),
            "`SET SNAPSHOT` needs the name of a snapshot. `SHOW SNAPSHOTS` lists them, and \
             `RESET SNAPSHOT` reads the present again",
        )));
    }

    let (snapshots, complaints) = load(server.warehouse_path());
    if let Some(first) = complaints.first() {
        return Some(Err(refusal(
            sankhya_error::protocol::sqlstate::DATA_EXCEPTION.as_str(),
            &format!("a snapshot document could not be read ({first})"),
        )));
    }
    let Some(snapshot) = snapshots.iter().find(|held| held.name == wanted) else {
        return Some(Err(refusal(
            "42704",
            &format!(
                "there is no snapshot called `{wanted}`. `SHOW SNAPSHOTS` lists them, with \
                 what each pins and when it expires"
            ),
        )));
    };
    if standing(snapshot, server.today()) == Standing::Expired {
        return Some(Err(refusal(
            "42704",
            &sankhya_snapshot::expire::expired_message(snapshot),
        )));
    }

    // Every version this snapshot pins must still **exist**, and that is `COR-21`.
    //
    // `SET VERSION OF` already checks this for the one table it names, under a comment saying
    // exactly why: `live_files_at` replays up to a version and stops, so asking for one beyond
    // the log silently answers with the newest — a version nobody has, served as though they
    // had it. The snapshot path reads the same way and did not check, so a snapshot naming a
    // version its table no longer has read the present and said nothing.
    //
    // Checked here rather than at the next query, for the reason the surrounding function
    // exists: an operator who set a snapshot should learn it is unusable at the `SET`, not
    // three statements later in a number that looks fine.
    for (table, pinned) in &snapshot.tables {
        let Some(root) = server.root_of(table) else {
            return Some(Err(refusal(
                "42P01",
                &format!(
                    "the snapshot `{wanted}` pins `{table}`, and there is no such table on \
                     this server. Reading as of it would quietly leave that table out"
                ),
            )));
        };
        let Ok(commits) = sankhya_table_delta::commits(&root) else {
            return Some(Err(refusal(
                sankhya_error::protocol::sqlstate::DATA_EXCEPTION.as_str(),
                &format!("the log of `{table}`, which `{wanted}` pins, could not be read"),
            )));
        };
        if !commits.iter().any(|(at, _)| *at == pinned.version) {
            let newest = commits.iter().map(|(at, _)| *at).max();
            return Some(Err(refusal(
                "42704",
                &format!(
                    "the snapshot `{wanted}` pins version {} of `{table}`, which that table \
                     no longer has — its newest is {}. Refused rather than served the \
                     present: a report quoting this snapshot would silently be about now",
                    pinned.version,
                    newest.map_or_else(|| "none".to_owned(), |at| at.to_string())
                ),
            )));
        }
    }

    Some(Ok(acknowledged("SET")))
}

/// Answer `SHOW HISTORY OF <table>`.
///
/// # Why there is a column saying what is keeping a version alive
///
/// Because a commit remaining in the log is **not** the same as its data remaining on disk.
/// Retirement deletes the files a merge replaced once nothing references them, so an old
/// version listed here may no longer be readable --- and a history that did not say so would
/// invite somebody to read a version that is gone and be told only at that point.
///
/// Only a snapshot or a clone keeps a version alive. This column is where that becomes visible.
/// `SHOW CHANGES BETWEEN <from> AND <to> FOR <table>`
///
/// [ADR-0024](../../../docs/adr/0024-what-a-difference-between-two-versions-is.md). Every number
/// comes from the log and none of them opens a Parquet file, which is what makes this answerable
/// between two reporting runs on a table nobody would consider scanning.
fn changes_between(
    server: &crate::wiring::Server,
    table: &str,
    from: u64,
    to: u64,
    principal: &Principal,
) -> Result<QueryResult, QueryFailure> {
    use sankhya_api_pg::message::{oid, FieldDescription};

    // The same absence as everywhere else: "you may not ask about that" confirms it is there.
    let absent = || {
        refusal(
            "42P01",
            &format!("there is no table called `{table}` on this server"),
        )
    };
    let Some(root) = server.root_of(table) else {
        return Err(absent());
    };
    if !server.readable_by(principal, table) {
        return Err(absent());
    }

    let difference = sankhya_table_delta::difference(&root, from, to).map_err(|error| {
        refusal(
            sankhya_error::protocol::sqlstate::DATA_EXCEPTION.as_str(),
            &error.to_string(),
        )
    })?;

    Ok(QueryResult {
        fields: vec![
            FieldDescription::text("commits", oid::TEXT, -1),
            FieldDescription::text("rows_added", oid::TEXT, -1),
            FieldDescription::text("rows_removed", oid::TEXT, -1),
            FieldDescription::text("files_added", oid::TEXT, -1),
            FieldDescription::text("files_removed", oid::TEXT, -1),
            // Counted separately and never folded into the numbers beside it. A diff reporting
            // *nothing changed* over a range in which every file was rewritten tells the truth
            // about rows and leaves the reader wondering about the storage.
            FieldDescription::text("compactions", oid::TEXT, -1),
        ],
        rows: vec![vec![
            Some(difference.commits.to_string()),
            Some(difference.rows_added.to_string()),
            Some(difference.rows_removed.to_string()),
            Some(difference.files_added.to_string()),
            Some(difference.files_removed.to_string()),
            Some(difference.compactions.to_string()),
        ]],
        tag: "SHOW".to_owned(),
    })
}

fn history_of(
    server: &crate::wiring::Server,
    table: &str,
    principal: &Principal,
) -> Result<QueryResult, QueryFailure> {
    use sankhya_api_pg::message::{oid, FieldDescription};

    // Indistinguishable from a table that exists and may not be read, which is the right
    // answer: saying "you may not ask about that" confirms it is there.
    let absent = || {
        refusal(
            "42P01",
            &format!("there is no table called `{table}` on this server"),
        )
    };
    let Some(root) = server.root_of(table) else {
        return Err(absent());
    };
    if !server.readable_by(principal, table) {
        return Err(absent());
    }

    let changes = sankhya_table_delta::history(&root).map_err(|error| {
        refusal(
            sankhya_error::protocol::sqlstate::DATA_EXCEPTION.as_str(),
            &format!(
                "the log of `{table}` could not be read: {error}. Refused rather than \
                 summarised as empty --- a table with no history and a history nobody can \
                 read lead to opposite actions"
            ),
        )
    })?;

    // Which versions something is keeping alive, **and what**. The column says *pinned*, which
    // is knowable, rather than *readable*, which is not: a version nothing pins may still be
    // there because nothing has swept yet, and promising that would be a promise nobody keeps.
    //
    // Naming the keeper rather than its kind, because the person reading this column is
    // deciding what to drop to release the storage --- and a column that said only
    // `"snapshot"` sends them to `SHOW SNAPSHOTS` to work out which one, on a warehouse where
    // there may be dozens.
    let keepers = keepers_of(server, &root);

    let rows = changes
        .iter()
        .map(|change| {
            vec![
                Some(change.version.to_string()),
                Some(sankhya_table_delta::describe(change).to_owned()),
                change.at.map(|at| at.to_string()),
                Some(change.added.to_string()),
                Some(change.removed.to_string()),
                Some(change.bytes_added.to_string()),
                Some(if change.changed_data { "yes" } else { "no" }.to_owned()),
                Some(
                    keepers
                        .get(&change.version)
                        .map(|names| {
                            names.iter().cloned().collect::<Vec<_>>().join(", ")
                        })
                        .unwrap_or_default(),
                ),
            ]
        })
        .collect::<Vec<_>>();
    let tag = format!("SELECT {}", rows.len());
    Ok(QueryResult {
        fields: vec![
            FieldDescription::text("version", oid::INT8, 8),
            FieldDescription::text("what", oid::TEXT, -1),
            FieldDescription::text("at", oid::INT8, 8),
            FieldDescription::text("files_added", oid::INT8, 8),
            FieldDescription::text("files_removed", oid::INT8, 8),
            FieldDescription::text("bytes_added", oid::INT8, 8),
            FieldDescription::text("changed_data", oid::TEXT, -1),
            FieldDescription::text("kept_by", oid::TEXT, -1),
        ],
        rows,
        tag,
    })
}

/// Check a `SET VERSION OF <table> = <n>` before the session remembers it.
///
/// # Why the version is checked now rather than at the next query
///
/// Because the log surviving is not the same as the data surviving. A version whose files
/// retirement has reclaimed resolves to a file list naming files that are gone, and the read
/// would fail at the *next* statement --- which is the wrong place to learn it. Checked here,
/// the refusal says the files were reclaimed and that only pinned points survive.
fn read_version(
    server: &crate::wiring::Server,
    table: &str,
    version: Option<u64>,
    principal: &Principal,
) -> Result<QueryResult, QueryFailure> {
    let absent = || {
        refusal(
            "42P01",
            &format!("there is no table called `{table}` on this server"),
        )
    };
    let Some(root) = server.root_of(table) else {
        return Err(absent());
    };
    if !server.readable_by(principal, table) {
        return Err(absent());
    }
    let Some(version) = version else {
        // `RESET VERSION OF` needs nothing checked: reading the present always works.
        return Ok(acknowledged("SET"));
    };

    // The version must **exist**. `live_files_at` replays up to a version and stops, so asking
    // for one beyond the log silently answers with the newest --- a version nobody has, served
    // as though they had it. Refused instead, naming what the table does have.
    let commits = sankhya_table_delta::commits(&root).map_err(|error| {
        refusal(
            sankhya_error::protocol::sqlstate::DATA_EXCEPTION.as_str(),
            &format!("the log of `{table}` could not be read: {error}"),
        )
    })?;
    let newest = commits.iter().map(|(at, _)| *at).max();
    if !commits.iter().any(|(at, _)| *at == version) {
        return Err(refusal(
            "42704",
            &format!(
                "`{table}` has no version {version}. Its newest is {}. `SHOW HISTORY OF \
                 {table}` lists every version it has, and which are pinned",
                newest.map_or_else(|| "none".to_owned(), |at| at.to_string())
            ),
        ));
    }

    let live = sankhya_table_delta::live_files_at(&root, version).map_err(|error| {
        refusal(
            sankhya_error::protocol::sqlstate::DATA_EXCEPTION.as_str(),
            &format!("version {version} of `{table}` could not be resolved: {error}"),
        )
    })?;
    // Every file that version names must still be there. A version whose files are gone is
    // refused rather than answered short: a historical query silently missing whatever had
    // been compacted is the wrong answer that looks most like a right one.
    let missing = live
        .files
        .iter()
        .filter(|file| !root.join(&file.path).exists())
        .count();
    if missing > 0 {
        return Err(refusal(
            "42704",
            &format!(
                "version {version} of `{table}` is in the log and its data is not: {missing} \
                 of its {} file(s) have been reclaimed. A version survives only while a \
                 snapshot or a clone keeps it, and nothing kept this one. `SHOW HISTORY OF \
                 {table}` shows which versions are pinned",
                live.files.len()
            ),
        ));
    }
    Ok(acknowledged("SET"))
}

/// The key a session stores a per-table version under.
///
/// Namespaced so it cannot collide with a setting a driver sends, and lower-cased because that
/// is how the session stores every setting name.
fn version_key(table: &str) -> String {
    format!("version of {}", table.to_lowercase())
}

/// The setting name for `SET VERSION OF <table>`, for the session to record.
#[must_use]
pub(crate) fn version_setting(table: &str) -> String {
    version_key(table)
}

/// The tables this session sees when it has pinned one or more of them to a version.
///
/// # Why this is separate from a snapshot
///
/// A snapshot is a set somebody curated and **pinned**; a version is one table at one number,
/// pinned by nothing. They are different acts --- *"the instant I named"* against *"that
/// version, whatever else has moved"* --- and answering both from one setting would let a query
/// mix a curated instant with an arbitrary one and call the result a snapshot.
///
/// Only the tables named are resolved differently. Everything else reads the present, because
/// that is what the session asked for.
fn at_versions(
    server: &crate::wiring::Server,
    caller: &sankhya_api_pg::session::Caller<'_>,
) -> Result<Option<std::sync::Arc<Vec<crate::execute::ServableTable>>>, QueryFailure> {
    let current = server.servable_now();
    let mut pinned: Vec<crate::execute::ServableTable> = Vec::new();
    let mut changed = false;

    for table in current.iter() {
        let Some(qualified) =
            crate::warehouse::qualified_name(server.warehouse_path(), &table.root)
        else {
            pinned.push(table.clone());
            continue;
        };
        // Both spellings, because a person may have said either and the session recorded what
        // they typed.
        let asked = caller
            .setting(&version_key(&qualified))
            .or_else(|| caller.setting(&version_key(&table.reference.table)));
        let Some(asked) = asked.and_then(|value| value.parse::<u64>().ok()) else {
            pinned.push(table.clone());
            continue;
        };

        let resolved = sankhya_readpath::resolve_as_of(
            std::sync::Arc::clone(&table.schema),
            &table.root,
            asked,
            sankhya_types::LsnRange::new(sankhya_types::Lsn::new(0), server.read_as_of()),
            server.read_as_of(),
        )
        .map_err(|error| {
            refusal(
                sankhya_error::protocol::sqlstate::DATA_EXCEPTION.as_str(),
                &format!("`{qualified}` could not be read at version {asked}: {error}"),
            )
        })?;
        pinned.push(crate::execute::ServableTable {
            reference: table.reference.clone(),
            authorize_as: table.authorize_as.clone(),
            inherited: table.inherited.clone(),
            root: table.root.clone(),
            provider: std::sync::Arc::new(resolved),
            schema: std::sync::Arc::clone(&table.schema),
            resolved_at: asked,
        });
        changed = true;
    }

    // Nothing pinned: hand back the session the server already built, rather than an identical
    // copy. A statement that pins nothing must cost nothing.
    if changed {
        Ok(Some(std::sync::Arc::new(pinned)))
    } else {
        Ok(None)
    }
}

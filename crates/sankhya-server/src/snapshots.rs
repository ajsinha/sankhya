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

use crate::wiring::{acknowledged, refusal};

/// The bookkeeping schema snapshot documents live under.
///
/// `_`-prefixed, so `warehouse::discover` skips it: a snapshot is not a user table and must not
/// appear in a catalogue somebody browses.
pub(crate) const DIRECTORY: &str = "_snapshots";

/// Where a snapshot of this name is stored.
fn document_at(warehouse: &Path, name: &str) -> PathBuf {
    warehouse.join(DIRECTORY).join(format!("{name}.json"))
}

/// Every snapshot this warehouse holds, with the ones that could not be read.
///
/// A document that cannot be read is **reported, never skipped**. A snapshot nobody can read
/// still pins files, and treating it as absent would let the sweeper reclaim them under a
/// reader --- which is the deletion this whole mechanism is gated on, arriving through a parse
/// failure.
pub(crate) fn load(warehouse: &Path) -> (Vec<Snapshot>, Vec<String>) {
    let directory = warehouse.join(DIRECTORY);
    let Ok(entries) = std::fs::read_dir(&directory) else {
        // No directory is the ordinary case for every warehouse that has taken none.
        return (Vec::new(), Vec::new());
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
pub(crate) fn take(
    warehouse: &Path,
    name: &str,
    taken_by: &str,
    taken_at: i64,
    expires_on: i32,
    readable: &[(String, u64)],
) -> Result<QueryResult, QueryFailure> {
    let path = document_at(warehouse, name);
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
    warehouse: &Path,
    name: &str,
    if_exists: bool,
) -> Result<QueryResult, QueryFailure> {
    let path = document_at(warehouse, name);
    if !path.exists() {
        if if_exists {
            return Ok(acknowledged("DROP SNAPSHOT"));
        }
        return Err(refusal(
            "42704",
            &format!(
                "there is no snapshot called `{name}`. `SHOW SNAPSHOTS` lists them, with what \
                 each pins and when it expires"
            ),
        ));
    }
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
            drop_it(warehouse, &name, if_exists)
        }
        Statement::Create { name, expiry } => {
            // Every table this principal may read, at the version it stands at *now*. Read
            // in one pass so the versions are as close to one instant as this can make
            // them; a table that commits between two of these reads is why the position is
            // recorded per table rather than as a single warehouse number.
            let readable: Vec<(String, u64)> = {
                let servable = server.servable_now();
                servable
                    .iter()
                    .filter(|table| {
                        let authority =
                            table.authorize_as.as_ref().unwrap_or(&table.reference);
                        Guard::authorize(server.policy_set(), principal, authority, Action::Read)
                            .is_some()
                    })
                    .filter_map(|table| {
                        let qualified = crate::warehouse::qualified_name(
                            &server.warehouse_path(),
                            &table.root,
                        )?;
                        let version = sankhya_table_delta::live_files(&table.root)
                            .ok()
                            .and_then(|live| live.version)?;
                        Some((qualified, version))
                    })
                    .collect()
            };
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
/// An unreadable snapshot document contributes **nothing** here, and that is the unsafe
/// direction stated plainly: it would let files it protects be reclaimed. `SHOW SNAPSHOTS`
/// refuses outright when one cannot be read, so an operator sees it; making the sweeper
/// refuse as well would stop reclamation warehouse-wide for one bad file, which is a worse
/// failure and a louder one than it deserves.
#[must_use]
pub(crate) fn still_reading(server: &crate::wiring::Server) -> sankhya_maintenance::StillReading {
    let (snapshots, _complaints) = load(server.warehouse_path());
    let pinned = pinned_versions(&snapshots, server.today());

    // Keyed by root path, because that is what the sweeper has and it removes every
    // question about which naming a key is in.
    let mut by_root = std::collections::BTreeMap::new();
    for (qualified, versions) in pinned {
        if let crate::warehouse::Resolved::One(root) =
            crate::warehouse::resolve(server.warehouse_path(), &qualified)
        {
            by_root.insert(root, versions);
        }
    }
    sankhya_maintenance::StillReading {
        clones: server.lineages(),
        snapshots: by_root,
    }
}

//! Feeds a deployment declared, loaded and run.
//!
//! # Where declarations live, and why not in `application.yaml`
//!
//! A feed is a document with a list of columns in it, and the configuration system flattens
//! YAML to dotted keys --- which is right for `server.listen` and wrong for a list of
//! structures. So a feed is its own file under `config/feeds/`, read with `serde`, in the same
//! way a cube's definition is its own file under `_cubes/`.
//!
//! It also puts each feed on its own filesystem object, which is what lets one be added or
//! removed without editing a file three other things read.
//!
//! # What a run does not do
//!
//! It does not create the tables. A feed whose target table does not exist is **refused**
//! rather than creating one from the declaration: the declaration says what a *record* looks
//! like, and a table's schema is a decision with partitioning, a date axis and a class in it.
//! Inferring one from the first feed to mention it is how a warehouse acquires tables nobody
//! designed.

use sankhya_api_pg::session::{QueryFailure, QueryResult};
use sankhya_feed::run::{run, Ran, Running};
use sankhya_feed::validate::{validate, Feed};
use sankhya_feed::{quarantine, Declaration};
use sankhya_publish::Publication;

use crate::wiring::{refusal, Server};
use sankhya_authz::policy::{Action, TableRef};
use sankhya_authz::principal::Principal;
use std::path::{Path, PathBuf};

/// Where feed declarations live, under the configuration directory.
pub(crate) const DIRECTORY: &str = "feeds";

/// A feed this server will run, and where its records come from.
#[derive(Debug)]
pub(crate) struct Declared {
    /// The validated feed.
    pub(crate) feed: Feed,
    /// The directory its sources arrive in.
    pub(crate) from: PathBuf,
    /// Where the declaration was read from, for a complaint to name.
    pub(crate) path: PathBuf,
}

/// Read every feed declared under `config/feeds/`.
///
/// Returns what loaded and a complaint per file that did not. A feed that cannot be read is
/// **not** a reason to refuse to start: the other feeds are somebody else's data, and a
/// server that would not start because one declaration had a typo would take an outage on
/// every table to protect one. The complaints are printed at startup like every other.
#[must_use]
pub(crate) fn load(configuration_dir: &Path) -> (Vec<Declared>, Vec<String>) {
    let directory = configuration_dir.join(DIRECTORY);
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        // No feeds directory is the ordinary case, not a complaint.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return (Vec::new(), Vec::new())
        }
        // `OPS-12`. One that is there and cannot be listed is **every** feed missing, and
        // returning it as "no feeds are declared" is a server that starts cleanly and
        // ingests nothing --- which looks, from every table it should have been filling,
        // exactly like a source that stopped producing.
        Err(error) => {
            return (
                Vec::new(),
                vec![format!(
                    "no feed was loaded: {} could not be read ({error})",
                    directory.display()
                )],
            )
        }
    };

    let mut declared = Vec::new();
    let mut complaints = Vec::new();
    // YAML only. Every file in the directory used to be read as a declaration, so
    // `config/feeds/README.md` --- the file that explains what a feed declaration is ---
    // was parsed as one, and the shipped configuration complained on every single startup:
    //
    //   feed not loaded --- config/feeds/README.md: invalid type: string "One file per
    //   feed. Each declares a source of newline-delimited JSON documents..."
    //
    // A complaint that is always there is a complaint nobody reads, which is worse than
    // none: the next one, about a feed that really is malformed, arrives in the same place
    // and looks the same. A file that is not a declaration is not a broken declaration.
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .filter(|path| {
            path.extension()
                .is_some_and(|kind| kind.eq_ignore_ascii_case("yaml") || kind.eq_ignore_ascii_case("yml"))
        })
        .collect();
    paths.sort();

    for path in paths {
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) => {
                complaints.push(format!("{}: {error}", path.display()));
                continue;
            }
        };
        let declaration: Declaration = match Declaration::from_document(&text) {
            Ok(declaration) => declaration,
            Err(error) => {
                complaints.push(format!("{}: {error}", path.display()));
                continue;
            }
        };
        let from = PathBuf::from(&declaration.from);
        match validate(declaration) {
            Ok(feed) => declared.push(Declared { feed, from, path }),
            Err(faults) => {
                // Every fault, on its own line. Reporting the first would make fixing a
                // declaration a sequence of restarts.
                for fault in faults {
                    complaints.push(format!("{}: {fault}", path.display()));
                }
            }
        }
    }
    (declared, complaints)
}

/// Run one feed once, against the warehouse at `warehouse`.
///
/// # Errors
///
/// The run's own error, as a sentence. A record that does not fit is quarantined rather than
/// raised here; these are the conditions no record can be blamed for.
pub(crate) fn run_once(declared: &Declared, warehouse: &Path) -> Result<Ran, String> {
    let feed = &declared.feed;
    let table_root = warehouse
        .join(feed.declaration().schema.as_str())
        .join(feed.declaration().table.as_str());
    if !sankhya_publish::is_table(&table_root) {
        return Err(format!(
            "`{}` ({}) lands in {}.{}, which is not a table in this warehouse. Refused \
             rather than created: a declaration says what a record looks like, and a table's \
             schema is a decision with a date axis, a class and a partitioning in it",
            feed.name(),
            declared.path.display(),
            feed.declaration().schema,
            feed.declaration().table
        ));
    }
    let quarantine_root = warehouse.join("sank").join(quarantine::TABLE);
    if !sankhya_publish::is_table(&quarantine_root) {
        return Err(format!(
            "`{}` has records to quarantine and sank.{} is not a table in this warehouse. \
             Refused rather than created, and refused rather than dropping them: a record \
             this system cannot keep is one it must not read",
            feed.name(),
            quarantine::TABLE
        ));
    }

    let table = publication(&table_root, &feed.declaration().table, feed);
    let quarantined = Publication::external(&quarantine_root, quarantine::TABLE);
    let now = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| i64::try_from(since.as_micros()).unwrap_or(i64::MAX))
    };
    run(
        feed,
        &declared.from,
        Running {
            table: &table,
            quarantine: &quarantined,
            table_version: table.next_version(),
            quarantine_version: quarantined.next_version(),
            now: &now,
        },
    )
    .map_err(|error| error.to_string())
}

/// Expire quarantined records whose retention has run out.
///
/// # Why the longest retention wins
///
/// One quarantine holds every feed's refused records, and a partition holds whatever arrived
/// that day. Detaching it on the shortest declared retention would let one feed destroy
/// another feed's records by editing its own configuration, which is a feed being given
/// authority over data it did not produce.
///
/// # Why this is detach and not delete
///
/// `DEC-23` gets no exception here. The partition leaves the live set, the files stay on disk,
/// and retirement reclaims them after its grace period --- so an expiry that should not have
/// happened is reversible for as long as that period lasts. That is what makes running this
/// automatically defensible where running a delete would not be.
///
/// Returns what it detached, or the reason it could not. `None` when there is nothing to do.
pub(crate) fn expire_quarantine(
    declared: &[Declared],
    warehouse: &Path,
    today: i32,
    now: i64,
) -> Option<Result<String, String>> {
    let retain = declared
        .iter()
        .map(|feed| feed.feed.quarantine().retain_days)
        .max()?;
    let root = warehouse.join("sank").join(quarantine::TABLE);
    if !sankhya_publish::is_table(&root) {
        return None;
    }
    let live = match sankhya_table_delta::live_files(&root) {
        Ok(live) => live,
        Err(error) => return Some(Err(format!("reading the quarantine: {error}"))),
    };
    let expired = sankhya_maintenance::expire::plan(&live, today, retain);
    if expired.is_empty() {
        return None;
    }
    let version = live.version.map_or(1, |version| version.saturating_add(1));
    // Milliseconds. `now` here is microseconds --- the unit the rest of this module works in
    // --- and `deletionTimestamp` is defined in milliseconds, so passing it straight through
    // dated every detached file about fifty thousand years into the future.
    match sankhya_maintenance::expire::detach(&root, version, &live, &expired, now / 1_000) {
        Ok(_) => Some(Ok(format!(
            "{} partition(s), {} file(s), older than {retain} day(s)",
            expired.partitions.len(),
            expired.files
        ))),
        // Not fatal and not silent. Another committer taking the version is ordinary --- the
        // next tick tries again --- and an operator should still be able to see that expiry
        // is not getting through if it never does.
        Err(error) => Some(Err(format!("detaching expired partitions: {error}"))),
    }
}

/// The publication a feed writes through, with its date axis applied.
fn publication(root: &Path, name: &str, feed: &Feed) -> Publication {
    let publication = Publication::external(root, name);
    match sankhya_feed::run::dated_by(feed) {
        // Ingest date, which is what `Publication::external` already declares. Named here
        // rather than left implicit so the two branches are visibly the same decision.
        None => publication,
        Some(column) => publication.dated_by(column),
    }
}


/// Record which table each declared feed writes into.
///
/// Called once, where the declarations are read, because that is the only place that knows both
/// a feed's name and what it fills. A feed absent from this map has its commands refused rather
/// than allowed: not knowing what a feed writes into is not permission to start it.
///
/// Here rather than on `Server` because this is feed handling, and `wiring.rs` sits at the line
/// limit --- a file that has to be argued down every time it grows is a file that stops being
/// argued with.
pub(crate) fn declare_targets(
    server: &Server,
    targets: std::collections::BTreeMap<String, TableRef>,
) {
    if let Ok(mut held) = server.feed_targets.write() {
        *held = std::sync::Arc::new(targets);
    }
}

/// The table a declared feed writes into.
fn target_of(server: &Server, feed: &str) -> Option<TableRef> {
    server
        .feed_targets
        .read()
        .ok()
        .and_then(|held| held.get(feed).cloned())
}

/// Answer a feed command.
///
/// # What is authorized here, and a correction
///
/// `SHOW FEEDS` was left ungated when `RESUME FEED` was closed, on the grounds that it reports
/// what the *server* is doing rather than what is in any table --- names an operator configured,
/// counts of rows this process moved, and why something stopped --- and that there was no table
/// to check a scope against.
///
/// The second half of that stopped being true in the same change that wrote it. Recording which
/// table each feed writes into, so `RESUME` could be authorized, gave `SHOW` exactly the table
/// it was said to lack. And the first half was never quite right either: a halt reason is
/// whatever the source made of itself, and it carries filesystem paths --- `ADR-0018` halts a
/// feed *because a record did not fit*, and saying so means saying which file. `SEC-18`.
///
/// So it is filtered by the same rule: a feed is listed to a caller who may read the table it
/// fills. A feed whose declaration did not load has no target and is listed to nobody, which is
/// the conservative direction --- an operator who cannot see a feed they expected has a
/// configuration problem to find, and the log said so at startup.
///
/// `RESUME FEED` is a different thing entirely and used to be treated the same way. It restarts
/// an ingest that `ADR-0018` halted **because its source changed shape** --- so resuming one is
/// deciding that records of an unknown shape should start landing in a table again. It took no
/// principal at all, so any caller could make that decision about anybody's table. `SEC-04`.
///
/// It is now authorized against the table the feed writes into, which is the same rule
/// `DROP CUBE` follows: a principal who may not read what a thing writes has no business
/// starting it.
pub(crate) fn run_command(
    server: &Server,
    principal: &Principal,
    command: Result<sankhya_feed::command::Command, sankhya_feed::command::CommandError>,
) -> Result<QueryResult, QueryFailure> {
    let feeds = &*server.feeds();
    use sankhya_api_pg::message::{oid, FieldDescription};
    use sankhya_error::protocol::sqlstate;
    use sankhya_feed::command::Command;
    use sankhya_feed::state::Health;

    let command = match command {
        Ok(command) => command,
        Err(error) => {
            return Err(refusal(sqlstate::SYNTAX_ERROR.as_str(), &error.to_string()))
        }
    };

    match command {
        Command::Show => {
            let standing = feeds.all();
            let rows = standing
                .iter()
                .map(|feed| {
                    // Every feed is listed, and the **reason** is what is withheld.
                    //
                    // Filtering the rows was tried first and was wrong. A feed whose target
                    // table does not exist is unreadable by everybody, so it disappeared from
                    // the listing --- and a feed that halted *because its table is missing* is
                    // precisely the case an operator opens this statement to find. A control
                    // that hides the thing it is meant to report is not a control.
                    //
                    // The name and the state are what the statement is for and are not tenant
                    // data: somebody configured that feed. The reason is what carries the
                    // disclosure, because `ADR-0018` halts a feed when a record does not fit
                    // and saying so means saying which file and what was in it. `SEC-18`.
                    // Unknown target, and the answer here is the **opposite** of the one
                    // `RESUME` gives, deliberately. A feed whose declaration did not load has
                    // no table recorded, so `RESUME` refuses it --- do not start something you
                    // cannot place. A report withholds nothing for the same reason: not knowing
                    // what a feed writes into is a configuration fault, and the operator who
                    // has to fix it is the one asking. Fail safe for an action, fail visible
                    // for a report.
                    let readable = target_of(server, &feed.name).is_none_or(|target| {
                        let table = if target.schema.is_empty() {
                            target.table.clone()
                        } else {
                            format!("{}.{}", target.schema, target.table)
                        };
                        // Or the table is not there at all, in which case there are no rows to
                        // withhold anything about --- and this is the case the statement is
                        // most often opened for. A feed that halted *because its table does
                        // not exist* would otherwise report a redaction where the answer is,
                        // which is the same defect as hiding the row, one level down.
                        server.scope_for(principal, &table).is_some()
                            || matches!(server.qualify(&table), crate::wiring::Qualified::Absent)
                    });
                    let (since, why) = match &feed.health {
                        Health::Running => (None, None),
                        Health::Halted { since, reason } => (
                            Some(since.to_string()),
                            Some(if readable {
                                reason.clone()
                            } else {
                                // Said rather than blanked. An empty reason reads as a feed
                                // that stopped for no reason, which is a different and more
                                // alarming thing to be told than one you may not be told about.
                                "halted; the reason names what was being read and is shown to \
                                 whoever may read the table this feed writes into"
                                    .to_owned()
                            }),
                        ),
                    };
                    vec![
                        Some(feed.name.clone()),
                        Some(feed.health.word().to_owned()),
                        since,
                        why,
                        Some(feed.runs.to_string()),
                        Some(feed.published.to_string()),
                        Some(feed.quarantined.to_string()),
                        Some(feed.skipped.to_string()),
                        Some(feed.halts.to_string()),
                    ]
                })
                .collect::<Vec<_>>();
            let tag = format!("SELECT {}", rows.len());
            Ok(QueryResult {
                fields: vec![
                    FieldDescription::text("feed", oid::TEXT, -1),
                    FieldDescription::text("state", oid::TEXT, -1),
                    FieldDescription::text("halted_since", oid::TEXT, -1),
                    FieldDescription::text("reason", oid::TEXT, -1),
                    FieldDescription::text("runs", oid::INT8, 8),
                    FieldDescription::text("published", oid::INT8, 8),
                    FieldDescription::text("quarantined", oid::INT8, 8),
                    FieldDescription::text("skipped", oid::INT8, 8),
                    FieldDescription::text("halts", oid::INT8, 8),
                ],
                rows,
                tag,
            })
        }
        Command::Resume { feed } => {
            // Refused with the same sentence as an unknown feed, deliberately: a caller who
            // may not touch this feed should not learn from the refusal that it exists.
            let unknown = || {
                refusal(
                    // `42704`, undefined object: this is a name that does not resolve, and
                    // the nearest thing the catalogue has for "no such thing".
                    "42704",
                    &format!(
                        "no feed called `{feed}` is declared on this server. \
                         `SHOW FEEDS` lists them"
                    ),
                )
            };
            // A feed whose declaration did not load has no target here, and is refused rather
            // than resumed: not knowing what it writes into is not permission to start it.
            let Some(target) = target_of(server, &feed) else {
                return Err(unknown());
            };
            let table = if target.schema.is_empty() {
                target.table.clone()
            } else {
                format!("{}.{}", target.schema, target.table)
            };
            if server.scope_for(principal, &table).is_none() {
                server.record(principal, target, Action::Insert, false);
                return Err(unknown());
            }
            server.record(principal, target, Action::Insert, true);
            if feeds.resume(&feed) {
                Ok(QueryResult {
                    fields: Vec::new(),
                    rows: Vec::new(),
                    tag: format!("RESUME FEED {feed}"),
                })
            } else {
                // Named rather than reported as success. An operator who mistypes a feed
                // name and is told it resumed will go away believing it did.
                Err(unknown())
            }
        }
    }
}

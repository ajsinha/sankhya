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

use sankhya_feed::run::{run, Ran, Running};
use sankhya_feed::validate::{validate, Feed};
use sankhya_feed::{quarantine, Declaration};
use sankhya_publish::Publication;
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
    let Ok(entries) = std::fs::read_dir(&directory) else {
        // No feeds directory is the ordinary case, not a complaint.
        return (Vec::new(), Vec::new());
    };

    let mut declared = Vec::new();
    let mut complaints = Vec::new();
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
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

//! `sankhya-server backup` and `sankhya-server drill`.
//!
//! # Why the drill is a first-class command rather than a script
//!
//! `FR-OPS-15` asks for automated, periodic restore drills with retained evidence, and the
//! reason is stated in the requirement itself: *"an untested backup is a rumour"*.
//!
//! A drill that lives in an operator's shell script is a drill that runs on one machine, is
//! not in any repository, and stops running the week that person changes team. Shipping it
//! as a command with an exit status means cron can run it, a monitor can watch it, and the
//! evidence lands in a known place — which is what makes the diagnostic's "no restore drill
//! has ever passed" possible to say at all.
//!
//! # Exit status
//!
//! `0` proven, `1` a table did not verify, `2` the drill could not run. The third is
//! separate for the same reason it is separate in the diagnostic: a monitor treating "I
//! could not look" as "nothing wrong" reports a backup as proven when nothing examined it.

use sankhya_backup::drill::{could_not_start, drill, last_pass, record};
use sankhya_backup::manifest::{KeyGeneration, Manifest, SourceBackup, TableSnapshot};
use sankhya_backup::warehouse::Warehouse;
use sankhya_types::Lsn;
use std::path::Path;

/// Exit status when the backup is proven restorable.
pub(crate) const PROVEN: i32 = 0;
/// Exit status when a table did not verify.
pub(crate) const NOT_PROVEN: i32 = 1;
/// Exit status when the drill could not run.
pub(crate) const COULD_NOT_RUN: i32 = 2;

/// Where a manifest is written, under the data directory.
pub(crate) const MANIFEST_FILE: &str = "backup-manifest.json";

/// Take a backup of every table in the warehouse.
///
/// The transactional half is recorded from what the caller supplies rather than taken here:
/// this system does not back up PostgreSQL, it binds itself to a backup somebody else took.
/// Pretending otherwise would produce a manifest naming an artefact that does not exist.
pub(crate) fn take(warehouse: &Path, data_dir: &Path, now: i64) -> i32 {
    println!("SANKHYA backup {}", env!("CARGO_PKG_VERSION"));

    let (found, refused) = crate::warehouse::discover(warehouse);
    for (path, why) in &refused {
        // Loud, and fatal below. A backup that quietly omits a table it could not read is
        // the worst possible artefact: it restores, it looks complete, and one table is
        // simply absent.
        eprintln!("  COULD NOT READ {}: {why}", path.display());
    }
    if !refused.is_empty() {
        eprintln!("\nRefusing to record a partial backup. A backup missing a table restores \
                   cleanly and is missing a table, and nothing about it says so.");
        return COULD_NOT_RUN;
    }
    if found.is_empty() {
        eprintln!("no tables found under {}", warehouse.display());
        return COULD_NOT_RUN;
    }

    let reader = Warehouse::at(warehouse);
    let mut snapshots = Vec::with_capacity(found.len());
    for table in &found {
        let name = table.reference.to_string();
        match reader.digest_now(&name) {
            Ok((version, digest)) => {
                println!("  {name} at version {version}, {} row(s)", digest.rows());
                snapshots.push(TableSnapshot::new(
                    &name,
                    version,
                    // Coverage is the published position. With no ingest in this process
                    // every table is published to the same point, and a running coordinator
                    // would supply the real per-table figure here.
                    Lsn::new(0),
                    digest,
                ));
            }
            Err(why) => {
                eprintln!("  COULD NOT DIGEST {name}: {why}");
                return COULD_NOT_RUN;
            }
        }
    }

    let source = SourceBackup {
        location: std::env::var("SANKHYA_SOURCE_BACKUP")
            .unwrap_or_else(|_| "unrecorded".to_string()),
        restores_to: Lsn::new(0),
        artefact_digest: std::env::var("SANKHYA_SOURCE_DIGEST")
            .unwrap_or_else(|_| "unrecorded".to_string()),
    };
    let keys = KeyGeneration {
        name: "warehouse".to_string(),
        version: 1,
    };

    let manifest = match Manifest::bind(now, source, snapshots, keys, now + 90 * 86_400_000_000) {
        Ok(manifest) => manifest,
        Err(why) => {
            eprintln!("\n{why}");
            return COULD_NOT_RUN;
        }
    };

    let path = data_dir.join(MANIFEST_FILE);
    if let Err(error) = std::fs::create_dir_all(data_dir)
        .and_then(|()| std::fs::write(&path, manifest.to_json().unwrap_or_default()))
    {
        eprintln!("could not write {}: {error}", path.display());
        return COULD_NOT_RUN;
    }

    println!();
    println!("  {}", manifest.id);
    println!("  queryable at {}", manifest.queryable_at.get());
    println!("  manifest {}", path.display());
    println!();
    println!("This backup is unproven until it has been drilled: `sankhya-server drill`.");
    PROVEN
}

/// Prove the recorded backup restores.
pub(crate) fn run_drill(warehouse: &Path, data_dir: &Path, now: i64) -> i32 {
    println!("SANKHYA restore drill {}", env!("CARGO_PKG_VERSION"));

    let path = data_dir.join(MANIFEST_FILE);
    let manifest = match std::fs::read_to_string(&path)
        .map_err(|error| error.to_string())
        .and_then(|text| Manifest::from_json(&text).map_err(|error| error.to_string()))
    {
        Ok(manifest) => manifest,
        Err(why) => {
            let evidence = could_not_start(
                "unknown",
                now,
                format!("{} could not be read: {why}", path.display()),
            );
            record(data_dir, &evidence).ok();
            eprintln!("  could not read {}: {why}", path.display());
            return COULD_NOT_RUN;
        }
    };

    let evidence = drill(&manifest, &Warehouse::at(warehouse), now);
    if let Err(error) = record(data_dir, &evidence) {
        // Surfaced rather than swallowed. A drill whose result was not written down is
        // indistinguishable from one that never ran, and the requirement asks for evidence.
        eprintln!("  {error}");
        return COULD_NOT_RUN;
    }

    println!("  {}", manifest.id);
    for (table, outcome) in &evidence.tables {
        println!("  {table}: {outcome}");
    }
    println!();
    if evidence.passed() {
        println!("Proven. {} table(s) read back and digested.", evidence.tables.len());
        PROVEN
    } else {
        println!(
            "NOT PROVEN. {} of {} table(s) did not verify — this backup would not restore \
             what it claims to hold.",
            evidence.failures().len(),
            evidence.tables.len()
        );
        NOT_PROVEN
    }
}

/// When the backup was last proven, for the diagnostic.
pub(crate) fn last_proven(data_dir: &Path) -> Option<i64> {
    last_pass(data_dir)
}

//! What a drill catches that a presence check does not, and what it writes down.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use sankhya_backup::drill::{
    could_not_start, drill, last_pass, record, ReadsBack, TableOutcome, EVIDENCE_FILE,
};
use sankhya_backup::manifest::{KeyGeneration, Manifest, SourceBackup, TableSnapshot};
use sankhya_backup::protect::{Protection, Standing, GRACE_MICROS};
use sankhya_ingest::TableDigest;
use sankhya_table_delta::Version;
use sankhya_types::Lsn;
use std::collections::BTreeMap;

/// A stand-in warehouse whose contents the test controls.
#[derive(Default)]
struct Warehouse {
    digests: BTreeMap<(String, Version), Result<TableDigest, String>>,
}

impl Warehouse {
    fn holding(mut self, table: &str, version: Version, rows: u64, checksum: u128) -> Self {
        self.digests.insert(
            (table.to_string(), version),
            Ok(TableDigest::from_parts(rows, checksum)),
        );
        self
    }

    fn refusing(mut self, table: &str, version: Version, why: &str) -> Self {
        self.digests
            .insert((table.to_string(), version), Err(why.to_string()));
        self
    }
}

impl ReadsBack for Warehouse {
    fn digest_of(&self, table: &str, version: Version) -> Result<TableDigest, String> {
        self.digests
            .get(&(table.to_string(), version))
            .cloned()
            .unwrap_or_else(|| Err("no such table at that version".to_string()))
    }
}

fn manifest_of(tables: Vec<(&str, Version, u64, u128)>) -> Manifest {
    Manifest::bind(
        1_000,
        SourceBackup {
            location: "s3://backups/x".to_string(),
            restores_to: Lsn::new(1_000),
            artefact_digest: "sha256:abc".to_string(),
        },
        tables
            .into_iter()
            .map(|(name, version, rows, checksum)| {
                TableSnapshot::new(
                    name,
                    version,
                    Lsn::new(500),
                    TableDigest::from_parts(rows, checksum),
                )
            })
            .collect(),
        KeyGeneration {
            name: "warehouse".to_string(),
            version: 1,
        },
        9_000,
    )
    .expect("consistent")
}

// --- what a presence check would miss -----------------------------------

#[test]
fn a_backup_whose_data_reads_back_correctly_passes() {
    let manifest = manifest_of(vec![("a", 3, 100, 4_242), ("b", 9, 50, 7)]);
    let warehouse = Warehouse::default()
        .holding("a", 3, 100, 4_242)
        .holding("b", 9, 50, 7);

    let evidence = drill(&manifest, &warehouse, 2_000);
    assert!(evidence.passed());
    assert!(evidence.failures().is_empty());
    assert_eq!(evidence.tables.len(), 2);
}

#[test]
fn altered_rows_are_caught_and_named_as_altered() {
    // The row count matches and the data does not. Every file is present and the right
    // length; a presence check passes. This is the failure a drill exists for.
    let manifest = manifest_of(vec![("a", 3, 100, 4_242)]);
    let warehouse = Warehouse::default().holding("a", 3, 100, 9_999);

    let evidence = drill(&manifest, &warehouse, 2_000);
    assert!(!evidence.passed());
    let (table, outcome) = evidence.failures()[0];
    assert_eq!(table, "a");
    let TableOutcome::DigestMismatch {
        checksum_agreed, ..
    } = outcome
    else {
        panic!("expected a mismatch, got {outcome:?}");
    };
    assert!(!checksum_agreed);
    assert!(
        outcome.to_string().contains("altered, not lost"),
        "the two failures mean different things: {outcome}"
    );
}

#[test]
fn lost_or_duplicated_rows_are_caught_and_named_as_lost() {
    let manifest = manifest_of(vec![("a", 3, 100, 4_242)]);
    let warehouse = Warehouse::default().holding("a", 3, 60, 4_242);

    let evidence = drill(&manifest, &warehouse, 2_000);
    let (_, outcome) = evidence.failures()[0];
    assert!(
        outcome.to_string().contains("lost or duplicated"),
        "{outcome}"
    );
}

#[test]
fn a_table_that_cannot_be_read_at_all_is_its_own_outcome() {
    let manifest = manifest_of(vec![("a", 3, 100, 4_242), ("b", 9, 50, 7)]);
    let warehouse = Warehouse::default()
        .holding("a", 3, 100, 4_242)
        .refusing("b", 9, "the file is not there");

    let evidence = drill(&manifest, &warehouse, 2_000);
    assert!(!evidence.passed());
    assert_eq!(evidence.failures().len(), 1);
    let (_, outcome) = evidence.failures()[0];
    assert!(matches!(outcome, TableOutcome::Unreadable { .. }));
}

#[test]
fn one_bad_table_does_not_stop_the_others_being_checked() {
    // A drill that stops at the first failure proves nothing about the rest, and the
    // operator gets one problem at a time across successive drills.
    let manifest = manifest_of(vec![("a", 1, 1, 1), ("b", 1, 1, 1), ("c", 1, 1, 1)]);
    let warehouse = Warehouse::default()
        .refusing("a", 1, "gone")
        .holding("b", 1, 1, 1)
        .refusing("c", 1, "gone");

    let evidence = drill(&manifest, &warehouse, 2_000);
    assert_eq!(evidence.tables.len(), 3);
    assert_eq!(evidence.failures().len(), 2);
}

#[test]
fn a_corrupt_manifest_entry_indicts_the_manifest_not_the_data() {
    // Different artefact, different investigation.
    let mut manifest = manifest_of(vec![("a", 3, 100, 4_242)]);
    manifest.tables[0].checksum = "not a number".to_string();
    let warehouse = Warehouse::default().holding("a", 3, 100, 4_242);

    let evidence = drill(&manifest, &warehouse, 2_000);
    let (_, outcome) = evidence.failures()[0];
    assert_eq!(outcome, &TableOutcome::ManifestUnreadable);
    assert!(outcome.to_string().contains("manifest"));
}

// --- the evidence -------------------------------------------------------

#[test]
fn a_drill_that_never_ran_does_not_read_as_one_that_passed() {
    // Both produce no failures. If they land in the same record, the history says a backup
    // was proven when nothing looked at it.
    let never = could_not_start("backup:x", 2_000, "the warehouse was unreachable");
    assert!(!never.passed());
    assert!(never.to_line().contains("could-not-start"));
    assert!(never.to_line().contains("unreachable"));

    let manifest = manifest_of(vec![("a", 3, 1, 1)]);
    let passed = drill(&manifest, &Warehouse::default().holding("a", 3, 1, 1), 2_000);
    assert!(passed.passed());
    assert!(passed.to_line().contains("\"verdict\": \"pass\""));
}

#[test]
fn a_manifest_with_no_tables_cannot_drill_to_a_pass() {
    // Vacuous truth is how a drill over nothing reports success. The manifest already
    // refuses to be built empty; this is the second door.
    let empty = sankhya_backup::Evidence {
        backup: "backup:x".to_string(),
        at: 1,
        tables: Vec::new(),
        could_not_start: None,
    };
    assert!(!empty.passed(), "no tables checked is not a pass");
}

#[test]
fn the_record_is_append_only_and_keeps_the_failures() {
    // A drill history with no failures in three years describes either a very good system
    // or a drill that does not really run, and nothing in the history says which.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let manifest = manifest_of(vec![("a", 3, 100, 4_242)]);
    let good = Warehouse::default().holding("a", 3, 100, 4_242);
    let bad = Warehouse::default().holding("a", 3, 100, 9_999);

    record(dir.path(), &drill(&manifest, &good, 1_000)).expect("recorded");
    record(dir.path(), &drill(&manifest, &bad, 2_000)).expect("recorded");
    record(dir.path(), &drill(&manifest, &good, 3_000)).expect("recorded");

    let text = std::fs::read_to_string(dir.path().join(EVIDENCE_FILE)).expect("read");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "nothing was overwritten");
    assert!(lines[0].contains("\"pass\""));
    assert!(lines[1].contains("FAIL"), "the failure was kept: {}", lines[1]);
    assert!(lines[1].contains("altered, not lost"), "with its reason");
    assert!(lines[2].contains("\"pass\""));
}

#[test]
fn the_last_pass_is_the_last_pass_not_the_last_attempt() {
    // An operator asking "when did we last prove we could restore" must not be answered
    // with the time of a failed drill.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let manifest = manifest_of(vec![("a", 3, 100, 4_242)]);
    let good = Warehouse::default().holding("a", 3, 100, 4_242);
    let bad = Warehouse::default().holding("a", 3, 100, 9_999);

    record(dir.path(), &drill(&manifest, &good, 1_000)).expect("recorded");
    record(dir.path(), &drill(&manifest, &bad, 5_000)).expect("recorded");
    record(dir.path(), &could_not_start("backup:x", 6_000, "unreachable")).expect("recorded");

    assert_eq!(last_pass(dir.path()), Some(1_000));
}

#[test]
fn a_history_of_nothing_but_failures_has_never_passed() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let manifest = manifest_of(vec![("a", 3, 100, 4_242)]);
    let bad = Warehouse::default().holding("a", 3, 100, 9_999);
    for at in [1_000, 2_000, 3_000] {
        record(dir.path(), &drill(&manifest, &bad, at)).expect("recorded");
    }
    assert_eq!(
        last_pass(dir.path()),
        None,
        "drills have run and none of them proved anything"
    );
}

#[test]
fn no_record_at_all_reads_as_never_passed() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    assert_eq!(last_pass(dir.path()), None);
}

#[test]
fn a_failure_reason_containing_a_quote_does_not_break_the_record() {
    // The record is read by a person during an incident. One malformed line from an awkward
    // table name would take the file with it.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let evidence = could_not_start("backup:x", 1, "the path \"/srv/a\\b\" is not readable");
    record(dir.path(), &evidence).expect("recorded");
    let text = std::fs::read_to_string(dir.path().join(EVIDENCE_FILE)).expect("read");
    assert_eq!(text.lines().count(), 1);
    assert!(text.contains(r#"\"/srv/a\\b\""#), "{text}");
}

// --- protection ---------------------------------------------------------

#[test]
fn deleting_a_backup_does_not_immediately_release_its_files() {
    // The failure: a backup deleted by mistake, the files swept before anybody notices, and
    // no way back even if the manifest is recovered five minutes later.
    let manifest = manifest_of(vec![("a", 3, 1, 1)]);
    let mut protection = Protection::new();
    protection.register(&manifest);
    assert_eq!(protection.standing(manifest.id, 0), Standing::Held);

    let lapses = protection.expire(manifest.id, 1_000);
    assert_eq!(lapses, 1_000 + GRACE_MICROS);
    assert!(matches!(
        protection.standing(manifest.id, 1_001),
        Standing::Grace { .. }
    ));
    assert!(
        protection
            .retained_snapshots(1_001)
            .contains(&("a".to_string(), 3)),
        "the files are still protected during grace"
    );
}

#[test]
fn removal_before_the_grace_has_passed_is_refused_and_protection_stands() {
    let manifest = manifest_of(vec![("a", 3, 1, 1)]);
    let mut protection = Protection::new();
    protection.register(&manifest);
    protection.expire(manifest.id, 1_000);

    assert!(!protection.remove(manifest.id, 1_000 + GRACE_MICROS - 1));
    assert!(!protection.retained_snapshots(1_000).is_empty());

    assert!(protection.remove(manifest.id, 1_000 + GRACE_MICROS));
    assert_eq!(protection.standing(manifest.id, 0), Standing::Released);
    assert!(protection.retained_snapshots(99_999_999).is_empty());
}

#[test]
fn a_backup_past_its_own_horizon_enters_grace_like_any_other() {
    // A policy horizon and an explicit expiry take the same path. Two paths would eventually
    // disagree about which files are safe to remove, and that disagreement deletes data.
    let manifest = manifest_of(vec![("a", 3, 1, 1)]);
    let mut protection = Protection::new();
    protection.register(&manifest);

    assert_eq!(protection.standing(manifest.id, 8_999), Standing::Held);
    assert!(matches!(
        protection.standing(manifest.id, 9_000),
        Standing::Grace { until } if until == 9_000 + GRACE_MICROS
    ));
    assert!(
        !protection.retained_snapshots(9_001).is_empty(),
        "still protected through its grace"
    );
}

#[test]
fn two_backups_referencing_one_snapshot_both_have_to_release_it() {
    let one = manifest_of(vec![("shared", 3, 1, 1)]);
    let two = manifest_of(vec![("shared", 3, 1, 1)]);
    let mut protection = Protection::new();
    protection.register(&one);
    protection.register(&two);

    protection.expire(one.id, 0);
    assert!(protection.remove(one.id, GRACE_MICROS));
    assert!(
        protection
            .retained_snapshots(GRACE_MICROS)
            .contains(&("shared".to_string(), 3)),
        "the second backup still needs it"
    );

    protection.expire(two.id, 0);
    assert!(protection.remove(two.id, GRACE_MICROS));
    assert!(protection.retained_snapshots(GRACE_MICROS).is_empty());
}

#[test]
fn expiring_twice_does_not_extend_the_grace() {
    // Otherwise a sweep that calls expire on every pass never reaches the removal, and the
    // storage is held forever by the thing meant to release it.
    let manifest = manifest_of(vec![("a", 3, 1, 1)]);
    let mut protection = Protection::new();
    protection.register(&manifest);

    assert_eq!(protection.expire(manifest.id, 1_000), 1_000 + GRACE_MICROS);
    assert_eq!(
        protection.expire(manifest.id, 500_000),
        1_000 + GRACE_MICROS,
        "the grace runs from the first expiry"
    );
    assert_eq!(protection.sweepable(1_000 + GRACE_MICROS), vec![manifest.id]);
}

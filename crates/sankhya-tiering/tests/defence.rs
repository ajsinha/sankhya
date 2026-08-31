//! The four layers between an archival purge and a propagated delete.
//!
//! # The trap being defended against
//!
//! `DEC-15`: the capture path replicates deletes. An ordinary `DELETE` used to purge tiered
//! data would faithfully propagate and **erase from the published tier exactly the data the
//! purge was meant to preserve**. Everything would work — the purge, the replication, the
//! applier — and the archive would be gone.
//!
//! Most of what is asserted here is therefore about what happens when something upstream has
//! already gone wrong, because that is the only state in which these layers do anything at all.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_tiering::defence::{At, Change, Extents, Marker, Reason, Verdict};
use sankhya_tiering::machine::Phase;
use sankhya_tiering::policy::Retention;
use sankhya_tiering::registry::{Range, Registry};
use sankhya_tiering::verify::Hash;
use sankhya_tiering::ArchiveEntry;

const T0: i64 = 1_700_000_000_000_000;

fn archived(table: &str, from: i64, until: i64) -> ArchiveEntry {
    ArchiveEntry {
        table: table.to_string(),
        range: Range::new(from, until),
        archive: "s3://archive".to_string(),
        snapshot: "snap-1".to_string(),
        rows: 1_000,
        keys: Hash::from_bytes([3; 32]),
        columns: Vec::new(),
        archived_at: T0,
        retention: Retention::new("7-year statutory record retention", 2557),
        legal_hold: false,
        attribution: Vec::new(),
    }
}

fn extents() -> Extents {
    let registry = Registry::from_entries(vec![
        archived("entries", 0, 100),
        archived("entries", 200, 300),
        archived("others", 0, 100),
    ])
    .unwrap();
    Extents::of(&registry)
}

#[test]
fn a_delete_outside_every_archived_range_is_applied() {
    // The ordinary case, and the one that must not become noisy: a business delete against hot
    // data is exactly what the table is for.
    assert_eq!(
        extents().consider("entries", Change::Delete, At::Ordinal(150)),
        Verdict::Apply
    );
}

#[test]
fn a_delete_inside_an_archived_range_halts_the_applier() {
    // `FR-TIER-06` makes this a fatal alarm rather than a warning or a skipped record. The row
    // it names was purged from the source, so the only copy it can remove is the archived one.
    let verdict = extents().consider("entries", Change::Delete, At::Ordinal(250));
    let Verdict::Halt(alarm) = verdict else { panic!("a delete into an archive must halt") };

    assert_eq!(alarm.reason, Reason::DeleteInArchivedRange);
    assert_eq!(alarm.extent, Some(Range::new(200, 300)));
    assert_eq!(alarm.table, "entries");
}

#[test]
fn the_alarm_is_not_a_skip_and_says_so() {
    // A skip would leave the source and the published tier permanently disagreeing about a row
    // nobody was told about, which is the failure this whole path exists to prevent, arrived at
    // politely.
    let Verdict::Halt(alarm) = extents().consider("entries", Change::Delete, At::Ordinal(50))
    else {
        panic!("must halt")
    };
    let said = alarm.to_string();
    assert!(said.starts_with("fatal:"), "{said}");
    assert!(said.contains("stops here"), "{said}");
    assert!(said.contains("[0, 100)"), "{said}");
}

#[test]
fn a_truncate_is_fatal_whenever_anything_of_the_table_is_archived() {
    // A truncate names no rows, so there is no key to compare and "every row" necessarily
    // includes every archived one. The key it is asked with makes no difference.
    for at in [At::WholeRelation, At::Unknown, At::Ordinal(150)] {
        let Verdict::Halt(alarm) = extents().consider("entries", Change::Truncate, at) else {
            panic!("a truncate against an archive must halt, asked as {at:?}")
        };
        assert_eq!(alarm.reason, Reason::TruncateWithArchive);
    }
}

#[test]
fn a_delete_that_cannot_be_located_fails_closed() {
    // The rule worth stating plainly: "we could not tell" must not become "apply". A delete
    // decoded from a relation with no replica identity carries no key, and it might be in an
    // archived range. The cost of halting on one that was not is an operator's afternoon.
    let Verdict::Halt(alarm) = extents().consider("entries", Change::Delete, At::Unknown) else {
        panic!("an unlocatable delete must halt")
    };
    assert_eq!(alarm.reason, Reason::UnlocatableAgainstArchive);
    assert_eq!(alarm.extent, None, "there is no range to name, and naming one would be a guess");
}

#[test]
fn nothing_is_refused_for_a_table_with_no_archive() {
    // Every deployment today. An applier with no extent map refuses nothing, rather than
    // refusing everything or silently doing neither.
    for change in [Change::Delete, Change::Truncate] {
        for at in [At::WholeRelation, At::Unknown, At::Ordinal(50)] {
            assert_eq!(extents().consider("untouched", change, at), Verdict::Apply);
            assert_eq!(Extents::empty().consider("entries", change, at), Verdict::Apply);
        }
    }
}

#[test]
fn one_tables_archive_does_not_refuse_another_tables_delete() {
    assert_eq!(
        extents().consider("others", Change::Delete, At::Ordinal(250)),
        Verdict::Apply,
        "`others` is archived over [0, 100) and nothing else"
    );
}

#[test]
fn an_insert_or_an_update_is_outside_this_layers_claim() {
    // `FR-TIER-06` names delete and truncate. An update reaching an archived range is a
    // different failure with a different answer --- `FR-TIER-19`'s compensating entry --- and
    // folding it in here would make this check the place somebody argues about corrections.
    for change in [Change::Insert, Change::Update] {
        assert!(extents().consider("entries", change, At::Ordinal(50)).is_apply());
    }
}

#[test]
fn the_extent_map_carries_every_range_of_every_table() {
    let extents = extents();
    assert!(extents.has_archive("entries"));
    assert!(!extents.has_archive("untouched"));
    assert_eq!(extents.covering("entries", 250), Some(Range::new(200, 300)));
    assert_eq!(extents.covering("entries", 150), None, "the gap between two archives");
}

#[test]
fn no_phase_of_a_purge_deletes_a_row() {
    // Layer one, and the only load-bearing one: purge is detach then drop. Enumerating the
    // phases is the audit, the same way enumerating `Authorization`'s constructors is. A phase
    // added later that removed rows would have to pass this.
    for phase in Phase::ALL {
        let name = phase.name().to_ascii_lowercase();
        assert!(!name.contains("delete"), "`{name}` is a phase that removes rows");
        assert!(!name.contains("truncate"), "`{name}` is a phase that removes rows");
    }
    assert!(Phase::ALL.contains(&Phase::Detached));
    assert!(Phase::ALL.contains(&Phase::Dropped));
}

#[test]
fn the_marker_says_it_is_provenance_and_not_a_control() {
    // Layer four is the only one that could be lost without anything noticing, so it says what
    // it is. A design in which deletes are emitted and the applier suppresses them between two
    // markers fails *open* if a marker is lost, reordered, or the applier restarts mid-bracket.
    let marker = Marker {
        purge: "purge-001".to_string(),
        table: "entries".to_string(),
        range: Range::new(0, 100),
        at: T0,
    };
    let said = marker.to_string();
    assert!(said.contains("purge-001"), "{said}");
    assert!(said.contains("[0, 100)"), "{said}");
    assert!(said.contains("provenance only"), "{said}");
    assert!(said.contains("no delete was emitted"), "{said}");
}

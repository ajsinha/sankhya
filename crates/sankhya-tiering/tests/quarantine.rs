//! The week of disk, and the two things the reaper must refuse.
//!
//! # What quarantine insures against, which verification cannot
//!
//! Verification proves the archive matches the source at the moment of the copy. It cannot
//! prove the *policy* was right — that the range was the one somebody meant, that the tiering
//! key meant what its author thought, that a timezone did not move a year's boundary. Those are
//! found days later by a person, and the only thing that helps then is the partition still
//! being on disk.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_tiering::policy::Retention;
use sankhya_tiering::quarantine::{Grace, Held, Kept, NoGrace, Quarantine, Refused};
use sankhya_tiering::registry::{Range, Registry};
use sankhya_tiering::verify::Hash;
use sankhya_tiering::ArchiveEntry;

const DAY: i64 = 86_400 * 1_000_000;
const T0: i64 = 1_700_000_000_000_000;

fn archived(from: i64, until: i64) -> ArchiveEntry {
    ArchiveEntry {
        table: "entries".to_string(),
        range: Range::new(from, until),
        archive: "s3://archive/entries".to_string(),
        snapshot: "snap-1".to_string(),
        rows: 1_000,
        keys: Hash::from_bytes([9; 32]),
        columns: Vec::new(),
        archived_at: T0,
        retention: Retention::new("7-year statutory record retention", 2557),
        legal_hold: false,
        attribution: Vec::new(),
    }
}

fn detained(purge: &str, from: i64, until: i64) -> Held {
    Held {
        purge: purge.to_string(),
        table: "entries".to_string(),
        range: Range::new(from, until),
        storage: format!("/var/lib/sankhya/quarantine/{purge}"),
        detached_at: T0,
        grace: Grace::default(),
    }
}

#[test]
fn a_grace_period_of_zero_cannot_be_asked_for() {
    // A grace period of nothing is `FR-TIER-13` not being implemented rather than being
    // configured, and this removes the setting somebody reaches for when a disk is full at four
    // in the morning.
    assert_eq!(Grace::of(0), Err(NoGrace));
    assert_eq!(Grace::of(1).unwrap().days(), 1);
    assert_eq!(Grace::default().days(), 7, "the default from DEC-24");
    assert!(NoGrace.to_string().contains("not being implemented"));
}

#[test]
fn a_partition_inside_its_grace_period_is_not_reaped() {
    let registry = Registry::from_entries(vec![archived(0, 100)]).unwrap();
    let mut quarantine = Quarantine::new();
    quarantine.hold(detained("purge-001", 0, 100));

    let reaping = quarantine.reap(&registry, T0 + 6 * DAY);
    assert!(reaping.release.is_empty());
    assert_eq!(reaping.kept.len(), 1);
    assert!(matches!(reaping.kept[0].1, Kept::WithinGrace { .. }));
    assert_eq!(quarantine.held().len(), 1, "and it is still held");
}

#[test]
fn a_partition_past_its_grace_period_and_still_archived_is_released() {
    let registry = Registry::from_entries(vec![archived(0, 100)]).unwrap();
    let mut quarantine = Quarantine::new();
    quarantine.hold(detained("purge-001", 0, 100));

    let reaping = quarantine.reap(&registry, T0 + 8 * DAY);
    assert_eq!(reaping.release.len(), 1);
    assert_eq!(reaping.release[0].storage, "/var/lib/sankhya/quarantine/purge-001");
    assert!(quarantine.held().is_empty(), "what it releases, it stops holding");
}

#[test]
fn a_partition_the_registry_no_longer_claims_is_never_reaped() {
    // The property that makes age insufficient on its own. If the archival entry has gone --- a
    // restore that lost it, an entry withdrawn by hand --- the quarantined copy is the only
    // copy, and reaping it on age is the permanent loss quarantine exists to prevent, performed
    // by the machinery meant to prevent it.
    let registry = Registry::new();
    let mut quarantine = Quarantine::new();
    quarantine.hold(detained("purge-001", 0, 100));

    let reaping = quarantine.reap(&registry, T0 + 10_000 * DAY);
    assert!(reaping.release.is_empty(), "no age makes this safe");
    assert_eq!(reaping.kept[0].1, Kept::Unarchived);
    assert!(reaping.kept[0].1.to_string().contains("only one there is"));
    assert_eq!(quarantine.held().len(), 1);
}

#[test]
fn an_archive_of_a_different_range_does_not_authorise_the_reap() {
    // The registry claiming *something* about the table is not the registry claiming *this*.
    let registry = Registry::from_entries(vec![archived(200, 300)]).unwrap();
    let mut quarantine = Quarantine::new();
    quarantine.hold(detained("purge-001", 0, 100));

    let reaping = quarantine.reap(&registry, T0 + 8 * DAY);
    assert!(reaping.release.is_empty());
    assert_eq!(reaping.kept[0].1, Kept::Unarchived);
}

#[test]
fn reattaching_withdraws_the_archival_entry_in_the_same_call() {
    // Re-attaching without withdrawing leaves a range the registry claims and the catalog has
    // attached, which is the disagreement `unify` has to serve hot and flag. Withdrawing
    // without re-attaching leaves the range in neither tier, which is a coverage gap. There is
    // no order to get wrong because there are not two calls.
    let mut registry = Registry::from_entries(vec![archived(0, 100)]).unwrap();
    let mut quarantine = Quarantine::new();
    quarantine.hold(detained("purge-001", 0, 100));

    let put_back = quarantine.reattach(&mut registry, "purge-001", T0 + DAY).unwrap();

    assert!(put_back.entry_withdrawn);
    assert_eq!(put_back.range, Range::new(0, 100));
    assert!(registry.entries().is_empty(), "the registry no longer claims it");
    assert!(quarantine.held().is_empty(), "and it is no longer quarantined");
}

#[test]
fn reattaching_a_range_the_registry_did_not_claim_says_so_rather_than_reporting_success() {
    // Something already withdrew it, and the two facts should be reconciled by somebody.
    let mut registry = Registry::new();
    let mut quarantine = Quarantine::new();
    quarantine.hold(detained("purge-001", 0, 100));

    let put_back = quarantine.reattach(&mut registry, "purge-001", T0 + DAY).unwrap();
    assert!(!put_back.entry_withdrawn);
}

#[test]
fn reattaching_after_the_grace_period_is_refused_and_points_at_rehydration() {
    // `FR-TIER-13` promises re-attachment is *simple* during the grace period. After it, the
    // files are gone and the archive is the copy, so pretending otherwise would be a promise
    // kept in name.
    let mut registry = Registry::from_entries(vec![archived(0, 100)]).unwrap();
    let mut quarantine = Quarantine::new();
    quarantine.hold(detained("purge-001", 0, 100));

    let refusal = quarantine
        .reattach(&mut registry, "purge-001", T0 + 8 * DAY)
        .expect_err("the grace period ended");

    assert!(matches!(refusal, Refused::GraceEnded { .. }));
    assert!(refusal.to_string().contains("rehydration"));
    assert_eq!(registry.entries().len(), 1, "and nothing was withdrawn on the way out");
    assert_eq!(quarantine.held().len(), 1, "nor released");
}

#[test]
fn reattaching_something_never_held_names_what_was_asked_for() {
    let mut registry = Registry::new();
    let mut quarantine = Quarantine::new();
    let refusal =
        quarantine.reattach(&mut registry, "purge-404", T0).expect_err("nothing is held");
    assert_eq!(refusal, Refused::NotHeld { purge: "purge-404".to_string() });
    assert!(refusal.to_string().contains("purge-404"));
}

#[test]
fn the_boundary_of_the_grace_period_is_the_moment_it_ends() {
    // Half-open, like every other range here: the partition is held *until* the instant it is
    // released, not through it.
    let held = detained("purge-001", 0, 100);
    assert_eq!(held.released_at(), T0 + 7 * DAY);
    assert!(held.within_grace(T0 + 7 * DAY - 1));
    assert!(!held.within_grace(T0 + 7 * DAY));
}

#[test]
fn a_sweep_reports_every_partition_it_kept_and_why() {
    // Keeping one is never an error. It costs disk; releasing one too early costs a retained
    // record --- so a sweep that says nothing about what it left behind is a sweep nobody can
    // audit.
    let registry = Registry::from_entries(vec![archived(0, 100)]).unwrap();
    let mut quarantine = Quarantine::new();
    quarantine.hold(detained("purge-001", 0, 100));
    quarantine.hold(detained("purge-002", 200, 300));
    quarantine.hold(Held { detached_at: T0 - 30 * DAY, ..detained("purge-003", 0, 100) });

    let reaping = quarantine.reap(&registry, T0 + DAY);
    assert_eq!(reaping.release.len(), 1, "only purge-003 is both old enough and archived");
    assert_eq!(reaping.release[0].purge, "purge-003");
    assert_eq!(reaping.kept.len(), 2);
    for (held, why) in &reaping.kept {
        assert!(!why.to_string().is_empty(), "{} was kept for no stated reason", held.purge);
    }
}

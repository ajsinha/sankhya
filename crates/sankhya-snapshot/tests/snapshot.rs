//! What a snapshot records, and what it refuses to resolve.
//!
//! # The decision these are about
//!
//! `ADR-0019` Decision 2. A table a snapshot does not name did not exist when it was taken, and
//! every one of the plausible ways to answer that is wrong except refusing. These hold that
//! still, because it is the decision the whole feature turns on and the one that will be
//! "simplified" by somebody who has not read the reasoning.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use std::collections::BTreeMap;

use sankhya_snapshot::expire::{expired_message, standing, unnamed_message, Expiry, Standing, Unaskable, LONGEST_DAYS};
use sankhya_snapshot::model::{Malformed, Pinned, Snapshot};

/// A snapshot of two tables, taken on day 100 and lasting 30 days.
fn snapshot() -> Snapshot {
    Snapshot::new(
        "eod",
        "ana",
        1_756_000_000_000_000,
        130,
        BTreeMap::from([
            ("sales.orders".to_owned(), Pinned { version: 412 }),
            ("ref.fx".to_owned(), Pinned { version: 77 }),
        ]),
    )
}

#[test]
fn a_snapshot_pins_a_version_per_table() {
    let taken = snapshot();
    assert_eq!(taken.pins("sales.orders"), Some(Pinned { version: 412 }));
    assert_eq!(taken.pins("ref.fx"), Some(Pinned { version: 77 }));
    assert_eq!(taken.len(), 2);
    assert!(!taken.is_empty());
}

#[test]
fn a_table_the_snapshot_does_not_name_resolves_to_nothing_rather_than_to_now() {
    // The decision this feature turns on. A table the snapshot does not name **did not exist**
    // when it was taken. Resolving it to the current version would silently mix two instants
    // --- which is exactly what a snapshot exists to prevent, arriving through the mechanism
    // meant to prevent it.
    let taken = snapshot();
    assert_eq!(taken.pins("sales.arrived_later"), None);

    // And the message a reader gets says why, rather than only that.
    let said = unnamed_message(&taken, "sales.arrived_later");
    assert!(said.contains("did not exist"), "{said}");
    assert!(said.contains("confident zero"), "{said}");
    assert!(said.contains("newer snapshot"), "{said}");
}

#[test]
fn the_qualified_name_is_what_is_pinned() {
    // A bare name is unambiguous only until a second schema grows a table of that name, and a
    // snapshot is a record that outlives that moment --- the same reasoning that made a clone's
    // lineage record the qualified name.
    let taken = snapshot();
    assert_eq!(taken.pins("orders"), None, "a bare name is not what was pinned");
    assert!(taken.names().any(|name| name == "sales.orders"));
}

#[test]
fn a_snapshot_of_no_tables_is_legal_and_says_so() {
    // A warehouse where the taker can read nothing produces one. Refusing here would report an
    // entitlement problem as a syntax problem, which sends somebody to fix the wrong thing.
    let empty = Snapshot::new("nothing", "ana", 0, 130, BTreeMap::new());
    assert!(empty.is_empty());
    assert_eq!(empty.len(), 0);
}

// --- the expiry -----------------------------------------------------------

#[test]
fn a_lifetime_of_zero_days_is_refused_as_a_typo() {
    // A snapshot that expires before anything can read it is not a request.
    let refused = Expiry::days(0).expect_err("zero is refused");
    assert_eq!(refused, Unaskable::Immediate);
    assert!(refused.to_string().contains("at least one day"), "{refused}");
}

#[test]
fn a_lifetime_longer_than_the_bound_is_refused_and_says_why_the_bound_exists() {
    // The limit is not technical. A snapshot pins files, and this bounds how far ahead one
    // person may commit storage somebody else will pay for --- so the refusal has to say that,
    // or it reads as an arbitrary number somebody will ask to have raised.
    let refused = Expiry::days(LONGEST_DAYS + 1).expect_err("too long");
    assert_eq!(refused, Unaskable::TooLong { asked: LONGEST_DAYS + 1 });
    assert!(refused.to_string().contains("not technical"), "{refused}");

    // And the bound itself is askable, so the limit is inclusive rather than off by one.
    assert!(Expiry::days(LONGEST_DAYS).is_ok());
}

#[test]
fn there_is_no_way_to_ask_for_a_snapshot_that_never_expires() {
    // Not a test of behaviour --- a test of the *type*. `ADR-0019` Decision 3 spells no
    // unbounded form, and `Snapshot::expires_on` is an `i32` rather than an `Option<i32>` so
    // that "never" is not one word away. If this ever becomes an `Option`, this comment is the
    // reason to argue about it first.
    let taken = snapshot();
    let _: i32 = taken.expires_on;
}

#[test]
fn a_snapshot_is_live_until_its_day_has_passed() {
    let taken = snapshot();
    assert_eq!(standing(&taken, 129), Standing::Live);
    // On the day itself it is still honoured: a lifetime of thirty days that ended on day
    // twenty-nine would be twenty-nine days, and off-by-one in a retention is how somebody
    // loses a report they were told they had.
    assert_eq!(standing(&taken, 130), Standing::Live);
    assert_eq!(standing(&taken, 131), Standing::Expired);
}

#[test]
fn an_expiry_saturates_rather_than_wrapping_into_the_past() {
    // A day near the end of the representable range must not wrap and make a fresh snapshot
    // expired on arrival --- which would be a reproducibility feature that deletes the thing it
    // was reproducing.
    let long = Expiry::days(LONGEST_DAYS).expect("a lifetime");
    assert_eq!(long.falls_on(i32::MAX), i32::MAX);
    assert!(long.falls_on(100) > 100);
}

#[test]
fn the_expired_message_names_the_snapshot_rather_than_only_the_failure() {
    // What turns "my report changed" into "my snapshot expired on Tuesday".
    let said = expired_message(&snapshot());
    assert!(said.contains("`eod`"), "{said}");
    assert!(said.contains("`ana`"), "who took it: {said}");
    assert!(said.contains("2 table(s)"), "what it held: {said}");
    assert!(said.contains("refused rather than answered from the present"), "{said}");
}

// --- the document ---------------------------------------------------------

#[test]
fn a_snapshot_survives_the_trip_to_a_document_and_back() {
    let taken = snapshot();
    let document = taken.to_document().expect("rendering");
    let read = Snapshot::from_document(&document).expect("reading");
    assert_eq!(read, taken);
}

#[test]
fn a_document_that_is_not_a_snapshot_is_refused_rather_than_read_as_empty() {
    // A snapshot nobody can read still protects files. Treating it as pinning nothing would let
    // the sweeper reclaim them under a reader --- which is the deletion this whole mechanism is
    // gated on, arriving through a parse failure.
    let refused = Snapshot::from_document("{ not a snapshot").expect_err("malformed");
    assert!(matches!(refused, Malformed { .. }));
    let said = refused.to_string();
    assert!(said.contains("refused rather than treated as pinning nothing"), "{said}");
    assert!(said.contains("reclaimed under a reader"), "{said}");
}

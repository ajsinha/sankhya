//! Reading archived data again without letting the copy become a second system of record.
//!
//! # The failure with no moment
//!
//! `RSK-35`: *"rehydrated copies accumulate into a shadow system of record"*. Nobody rehydrates
//! a shadow system of record. They rehydrate one range for one investigation, and then another,
//! over a multi-year horizon, and each one is individually reasonable. There is no day on which
//! somebody could have decided otherwise — which is why the expiry cannot be a decision made
//! per rehydration, and why most of these tests are about what cannot be constructed.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_tiering::registry::Range;
use sankhya_tiering::rehydrate::{
    Correction, Expiry, NotBounded, NotSurvivable, ReadOnly, Register, Rehydration, Target,
    Unsuitable,
};

const DAY: i64 = 86_400 * 1_000_000;
const T0: i64 = 1_700_000_000_000_000;

fn target() -> Target {
    Target::loading_into("sankhya_rehydrated", "public", true).expect("excluded and not live")
}

fn copy(name: &str, at: i64, expiry: Expiry) -> Rehydration {
    Rehydration::new(
        name,
        "entries",
        Range::new(0, 100),
        "s3://archive/entries",
        target(),
        "priya",
        at,
        expiry,
    )
}

#[test]
fn a_copy_that_never_expires_cannot_be_asked_for() {
    assert_eq!(Expiry::of(0), Err(NotBounded::Never));
    assert!(NotBounded::Never.to_string().contains("shadow system of record"));
    assert_eq!(Expiry::default().days(), 30);
}

#[test]
fn one_decision_may_not_reach_further_than_the_maximum() {
    // Not a safety property --- a person can rehydrate again --- but a limit on how far a
    // single decision reaches. The risk is accumulation over years, and a copy granted for
    // years is that risk taken in one step.
    let refused = Expiry::of(Expiry::MAXIMUM_DAYS + 1).expect_err("beyond the maximum");
    assert!(matches!(refused, NotBounded::TooLong { .. }));
    assert!(refused.to_string().contains("ask for again"));
    assert!(Expiry::of(Expiry::MAXIMUM_DAYS).is_ok(), "the maximum itself is allowed");
}

#[test]
fn a_schema_a_publication_could_capture_is_refused() {
    // A rehydrated copy in a captured schema is re-captured, and arrives in the published tier
    // as duplicates of rows that are already there.
    let refused = Target::loading_into("reports", "public", false).expect_err("capturable");
    assert_eq!(refused, Unsuitable::MayBeCaptured { schema: "reports".to_string() });
    assert!(refused.to_string().contains("duplicates"));
}

#[test]
fn the_live_schema_is_refused_even_when_asserted_excluded() {
    // A rehydration is a copy to read, never an attachment to the parent. Both assertions have
    // to hold, and the second is not implied by the first: somebody can exclude the live schema
    // from publications and still be wrong to load into it.
    let refused = Target::loading_into("public", "public", true).expect_err("the live schema");
    assert_eq!(refused, Unsuitable::TheLiveSchema { schema: "public".to_string() });
    assert!(refused.to_string().contains("never an attachment"));
}

#[test]
fn a_rehydration_has_one_access_mode_and_it_is_not_writable() {
    // `ReadOnly` is a unit type rather than an enum with one variant used today. An enum invites
    // a second variant, and the second variant is the writable copy `FR-TIER-20` forbids.
    let copy = copy("investigation-4471", T0, Expiry::default());
    assert_eq!(copy.access, ReadOnly);
    assert_eq!(copy.access.to_string(), "read-only");
}

#[test]
fn a_copy_is_live_until_its_expiry_and_not_through_it() {
    let copy = copy("investigation-4471", T0, Expiry::of(30).unwrap());
    assert_eq!(copy.expires_at(), T0 + 30 * DAY);
    assert!(copy.live_at(T0 + 30 * DAY - 1));
    assert!(!copy.live_at(T0 + 30 * DAY));
}

#[test]
fn expiry_drops_the_copies_that_are_due_and_keeps_the_rest() {
    let mut register = Register::new();
    register.record(copy("old", T0 - 40 * DAY, Expiry::of(30).unwrap()));
    register.record(copy("recent", T0 - DAY, Expiry::of(30).unwrap()));

    let dropped = register.expire(T0);
    assert_eq!(dropped.len(), 1);
    assert_eq!(dropped[0].name, "old");
    assert_eq!(register.copies().len(), 1);
    assert_eq!(register.copies()[0].name, "recent");
}

#[test]
fn expiry_is_unconditional_where_the_quarantine_reaper_is_not() {
    // The two reapers look alike and are opposites. A quarantined partition may be the only
    // copy there is, so age alone never authorises releasing it. A rehydrated copy is a copy of
    // an archive that still exists, so dropping it loses nothing and keeping it is the risk.
    let mut register = Register::new();
    register.record(copy("old", T0 - 400 * DAY, Expiry::of(1).unwrap()));

    assert_eq!(register.expire(T0).len(), 1, "nothing weighs against it");
    assert!(register.copies().is_empty());
}

#[test]
fn accumulation_reports_count_and_age_together() {
    // Either alone is uninformative: one copy held for a year and fifty held for a day are
    // different problems, and neither is visible in the other's number.
    let mut register = Register::new();
    register.record(copy("a", T0 - 10 * DAY, Expiry::default()));
    register.record(copy("b", T0 - 2 * DAY, Expiry::default()));

    assert_eq!(register.accumulation(T0), (2, 10));
    assert_eq!(Register::new().accumulation(T0), (0, 0));
}

#[test]
fn a_correction_defaults_to_a_compensating_entry() {
    // How record-keeping already works: a posted entry is reversed, not erased. It touches
    // nothing archived, so it is available whatever the archive's immutability controls say.
    let correction = Correction::compensating("row-9", "row-10");
    assert_eq!(
        correction,
        Correction::Compensating {
            original: "row-9".to_string(),
            entry: "row-10".to_string()
        }
    );
}

#[test]
fn a_rewrite_that_keeps_no_prior_version_is_refused() {
    // Indistinguishable from the archive having always said the new thing, which is the
    // property archives exist to have.
    let refused = Correction::rewrite("row-9", "   ", "CHG-1").expect_err("no prior version");
    assert_eq!(refused, NotSurvivable::NoPriorVersion);
    assert!(refused.to_string().contains("always said the new thing"));
}

#[test]
fn a_rewrite_that_records_no_amendment_is_refused() {
    let refused = Correction::rewrite("row-9", "row-9@v1", "").expect_err("no amendment");
    assert_eq!(refused, NotSurvivable::NoAmendmentLink);
    assert!(refused.to_string().contains("nobody can audit"));
}

#[test]
fn a_rewrite_with_both_is_allowed_and_carries_them() {
    let correction = Correction::rewrite("row-9", "row-9@v1", "CHG-4471").unwrap();
    assert_eq!(
        correction,
        Correction::ControlledRewrite {
            original: "row-9".to_string(),
            prior_version: "row-9@v1".to_string(),
            amendment: "CHG-4471".to_string()
        }
    );
}

#[test]
fn a_rehydration_names_who_asked() {
    // An accumulation with no names against it is an accumulation nobody owns.
    assert_eq!(copy("investigation-4471", T0, Expiry::default()).requested_by, "priya");
    assert_eq!(target().schema(), "sankhya_rehydrated");
}

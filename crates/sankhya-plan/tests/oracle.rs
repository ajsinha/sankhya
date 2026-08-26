//! Testing the oracle.
//!
//! `is_exact_cover` is the function every other splice test leans on: the property test
//! that proves a plan covers a query's span asserts it and nothing more. That makes it
//! load-bearing for INV-1 in a way easy to miss, because it is not called by the library
//! itself — it exists to check the planner's work.
//!
//! An oracle nobody tests is an assertion nobody makes. A mutation audit found exactly
//! that: `is_exact_cover` could be changed to tolerate gaps between tiers, or to stop
//! requiring the cover to reach the target, and the entire suite stayed green. Every
//! splice test would have kept passing over a planner that returned partial covers.
//!
//! So these tests do not check the planner. They feed the oracle covers that are wrong
//! in each way a cover can be wrong, and require it to say so.

use sankhya_plan::{is_exact_cover, TierRef};
use sankhya_types::{Lsn, LsnRange};

fn tier(from: u64, to: u64) -> TierRef {
    TierRef::new(
        "t",
        LsnRange::new(Lsn::new(from), Lsn::new(to)).expect("a non-empty range"),
    )
}

#[test]
fn a_correct_cover_is_accepted() {
    // The oracle must not simply reject everything, or the property tests it backs
    // would be unfalsifiable in the other direction.
    assert!(is_exact_cover(
        &[tier(0, 10), tier(10, 20), tier(20, 30)],
        Lsn::new(30)
    ));
    assert!(is_exact_cover(&[tier(0, 30)], Lsn::new(30)));
}

#[test]
fn a_gap_is_rejected() {
    // The failure the splice exists to prevent. Positions 11..=15 are held by nothing,
    // and a query answered from these tiers would silently omit them.
    assert!(!is_exact_cover(&[tier(0, 10), tier(15, 30)], Lsn::new(30)));
}

#[test]
fn a_gap_of_exactly_one_position_is_rejected() {
    // Guards the boundary specifically. Coverage is half-open — `(start, end]` — so
    // abutting means the next tier starts exactly where the last ended. An oracle
    // written with `<` or `<=` in the wrong place tolerates a single missing position,
    // which is both the likeliest defect and the hardest to notice.
    assert!(!is_exact_cover(&[tier(0, 10), tier(11, 30)], Lsn::new(30)));
}

#[test]
fn an_overlap_is_rejected() {
    // The other direction. These tiers both hold 6..=10, which double-counts every row
    // in that span — a wrong sum rather than a missing one.
    assert!(!is_exact_cover(&[tier(0, 10), tier(5, 30)], Lsn::new(30)));
}

#[test]
fn a_cover_stopping_short_of_the_target_is_rejected() {
    // Contiguous from the origin, with no gap and no overlap, and still wrong: the
    // query asked to see through 30 and these tiers reach 20. This is the mutation that
    // survived the original suite.
    assert!(!is_exact_cover(&[tier(0, 10), tier(10, 20)], Lsn::new(30)));
}

#[test]
fn a_cover_overshooting_the_target_is_rejected() {
    // Overshooting is not harmless. It means the selection was not made for this query,
    // and the caller would be shown positions past the one it pinned.
    assert!(!is_exact_cover(&[tier(0, 10), tier(10, 40)], Lsn::new(30)));
}

#[test]
fn a_cover_not_starting_at_the_origin_is_rejected() {
    // Everything before position 5 is missing. A cover is over `(0, target]`, always.
    assert!(!is_exact_cover(&[tier(5, 30)], Lsn::new(30)));
}

#[test]
fn tiers_out_of_order_are_rejected() {
    // The same intervals in the right order are a correct cover. Order matters because
    // the reader consumes them in order, and the oracle must not silently sort.
    assert!(is_exact_cover(&[tier(0, 10), tier(10, 20)], Lsn::new(20)));
    assert!(!is_exact_cover(&[tier(10, 20), tier(0, 10)], Lsn::new(20)));
}

#[test]
fn nothing_covers_nothing_but_does_not_cover_something() {
    assert!(is_exact_cover(&[], Lsn::ZERO));
    assert!(!is_exact_cover(&[], Lsn::new(1)));
}

#[test]
fn a_cover_of_a_zero_target_must_be_empty() {
    // A query as of the origin sees nothing, so any tier at all is a defect.
    assert!(!is_exact_cover(&[tier(0, 10)], Lsn::ZERO));
}

//! What a feed's state must answer, and what it must not forget.
//!
//! # Why this is not just a boolean
//!
//! ADR-0018 makes "halted" a state somebody has to act on. A state nobody can query exists only
//! in the log line printed at the moment it began, which is no use to an operator arriving an
//! hour later --- and an hour later is when most people arrive.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_feed::state::{Feeds, Health};

const NOW: i64 = 1_756_000_000_000_000;

#[test]
fn a_declared_feed_is_visible_before_it_has_ever_run() {
    // The case an operator most needs to see: a feed that has never managed to run at all.
    // One that only appeared after its first success would be invisible exactly when it
    // mattered.
    let feeds = Feeds::new();
    feeds.declare("orders");

    let all = feeds.all();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].name, "orders");
    assert_eq!(all[0].health, Health::Running);
    assert_eq!(all[0].runs, 0);
    assert!(feeds.should_run("orders"));
}

#[test]
fn a_halted_feed_says_why_and_since_when() {
    let feeds = Feeds::new();
    feeds.declare("orders");
    feeds.halted("orders", "not one of this source's 40 records fitted", NOW);

    let standing = feeds.all().remove(0);
    match standing.health {
        Health::Halted { since, reason } => {
            assert_eq!(since, NOW);
            assert!(reason.contains("40 records"), "{reason}");
        }
        other => panic!("expected a halt, got {other:?}"),
    }
    assert!(!feeds.should_run("orders"), "a halted feed is not run again on a timer");
    assert_eq!(standing.halts, 1);
}

#[test]
fn halting_again_for_the_same_reason_is_not_a_second_halt() {
    // The feed task may notice the halt on more than one tick. Counting each notice would
    // make the number meaningless — it would measure the tick rate, not the source.
    let feeds = Feeds::new();
    feeds.declare("orders");
    feeds.halted("orders", "a reason", NOW);
    feeds.halted("orders", "a reason", NOW + 1_000);

    assert_eq!(feeds.all()[0].halts, 1);
}

#[test]
fn resuming_and_halting_again_is_a_second_halt_and_the_count_remembers() {
    // The distinction that matters: a feed halting again after a resume says the source is
    // still wrong and somebody is resuming rather than fixing. One halt says something
    // happened once.
    let feeds = Feeds::new();
    feeds.declare("orders");
    feeds.halted("orders", "a reason", NOW);
    assert!(feeds.resume("orders"));
    assert!(feeds.should_run("orders"), "a resumed feed runs again");
    feeds.halted("orders", "a reason", NOW + 60_000_000);

    assert_eq!(feeds.all()[0].halts, 2, "resuming does not forget");
}

#[test]
fn resuming_a_feed_nobody_declared_says_so() {
    // Rather than reporting success for a name nobody has, which leaves an operator certain
    // they resumed something.
    let feeds = Feeds::new();
    feeds.declare("orders");

    assert!(!feeds.resume("odrers"), "a typo is not a resume");
    assert!(feeds.resume("orders"));
}

#[test]
fn what_a_run_did_accumulates_across_runs() {
    let feeds = Feeds::new();
    feeds.declare("orders");
    feeds.ran("orders", 100, 5, 2, NOW);
    feeds.ran("orders", 40, 1, 3, NOW + 30_000_000);

    let standing = feeds.all().remove(0);
    assert_eq!(standing.runs, 2);
    assert_eq!(standing.published, 140);
    assert_eq!(standing.quarantined, 6);
    // The number that carries the information: steady for a spool that keeps its files, and
    // growing when sources are arriving behind the mark.
    assert_eq!(standing.skipped, 5);
    assert_eq!(standing.last_run, NOW + 30_000_000);
}

#[test]
fn an_undeclared_feed_is_run_rather_than_skipped() {
    // Failing open here is right: the registry is a record of what has happened, and a feed
    // the task knows about but the registry has not seen yet must not be silently not-run.
    let feeds = Feeds::new();
    assert!(feeds.should_run("never_declared"));
}

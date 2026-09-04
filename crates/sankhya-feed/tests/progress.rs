//! What a restart asks, and what the position answers.
//!
//! # The two failures this is between
//!
//! A feed that restarts and re-reads a source it already published gives a table every row
//! twice. A feed that restarts and skips one leaves a gap. Both are silent, both are
//! discovered by somebody reconciling months later, and which one a system produces depends on
//! the order it happened to write two commits in.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_feed::progress::{key, Partial, Position, Standing};

#[test]
fn a_feed_that_has_never_run_finds_everything_fresh() {
    let position = Position::default();

    assert_eq!(position.standing("2026-08-01.json"), Standing::Fresh);
    assert_eq!(position.standing("2026-08-31.json"), Standing::Fresh);
}

#[test]
fn a_finished_source_and_everything_before_it_is_done() {
    let mut position = Position::default();
    position.finished("2026-08-15.json");

    assert_eq!(position.standing("2026-08-15.json"), Standing::Done);
    assert_eq!(position.standing("2026-08-16.json"), Standing::Fresh);
    // And everything below the mark, which is where the honest limit of a high-water mark
    // is: a source finished last week and a source that has just appeared behind the mark
    // are the same answer, because a mark records where a feed got to and not which files
    // it read. Never re-ingesting is the error this design prefers.
    assert_eq!(position.standing("2026-08-14.json"), Standing::Done);
}

#[test]
fn a_source_part_way_through_is_resumed_from_where_it_stopped() {
    let mut position = Position::default();
    position.finished("2026-08-15.json");
    position.part_way("2026-08-16.json", 1_204);

    assert_eq!(position.standing("2026-08-16.json"), Standing::Resume(1_204));
    // And finishing it clears the partial, so a later restart does not skip its tail.
    position.finished("2026-08-16.json");
    assert_eq!(position.standing("2026-08-16.json"), Standing::Done);
    assert_eq!(position.partial, None);
}

#[test]
fn a_partial_source_is_resumed_even_when_it_sorts_behind_the_mark() {
    // The awkward case: a feed stopped part-way through a file whose name sorts before the
    // last finished one — which happens when files are delivered out of order and the feed
    // was stopped mid-run. Resuming beats refusing here, because the alternative is a
    // half-published source nothing will ever finish.
    let mut position = Position::default();
    position.finished("2026-08-20.json");
    position.part_way("2026-08-02.json", 7);

    assert_eq!(position.standing("2026-08-02.json"), Standing::Resume(7));
}

#[test]
fn the_mark_only_ever_moves_forward() {
    // A source finishing out of order must not move the mark backwards: everything between
    // the old mark and the new one would become eligible again, and a feed would republish
    // whole days.
    let mut position = Position::default();
    position.finished("2026-08-20.json");
    position.finished("2026-08-03.json");

    assert_eq!(position.through, "2026-08-20.json");
    assert_eq!(position.standing("2026-08-10.json"), Standing::Done);
}

#[test]
fn a_position_survives_the_round_trip_through_a_property() {
    let mut position = Position::default();
    position.finished("2026-08-15.json");
    position.part_way("2026-08-16.json", 42);

    let text = position.to_property().expect("a property value");
    assert_eq!(Position::from_property(&text).expect("read back"), position);
}

#[test]
fn an_unreadable_position_is_an_error_and_not_an_empty_one() {
    // The distinction that matters on startup. *No position* means a feed that has not run;
    // *a position nobody can read* means one whose progress is unknown, and treating the
    // second as the first is how a table acquires every row a second time.
    assert!(Position::from_property("{{not json").is_err());
    assert!(Position::from_property("").is_err());

    let empty = Position::from_property("{}").expect("an empty object is a position");
    assert_eq!(empty, Position::default());
}

#[test]
fn two_feeds_landing_in_one_table_do_not_overwrite_each_others_progress() {
    // Allowed, and how a table is fed from two directories. Sharing one property would make
    // each feed's restart depend on the other's, which is a coupling nobody declared.
    assert_ne!(key("orders_eu"), key("orders_us"));
    assert!(key("orders_eu").starts_with("sankhya.feed."));
}

#[test]
fn a_partial_carries_the_count_of_lines_read_because_that_is_what_a_resume_skips_by() {
    // This test was named `a_partial_carries_the_count_of_what_is_published_not_what_is_read`
    // and it locked in `ING-01`: the position held records **published** while the resume
    // skipped by **line index**, and those are the same number only when every line so far
    // fitted and there were no blanks.
    //
    // Its reasoning was sound about the thing it was worried about — a crash between reading
    // and committing must not move the position past rows nobody wrote — and that concern is
    // answered by *when* the position is written, not by what it counts. Rows and position go
    // into one commit, so a crash before it leaves the old position untouched. The name
    // identified the conflation exactly and then asserted it.
    let partial = Partial { source: "a.json".to_owned(), read_through: 10 };
    let position = Position { through: String::new(), partial: Some(partial) };

    assert_eq!(
        position.standing("a.json"),
        Standing::Resume(10),
        "ten lines read, so the eleventh is next"
    );
}

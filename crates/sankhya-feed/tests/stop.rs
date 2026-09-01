//! One bad record, and a run of them.
//!
//! # The case these are really about
//!
//! A source changes shape overnight. Every record now fails to fit, every one is quarantined
//! whole, and the feed keeps running: no error, no gap in the logs, a quarantine table filling
//! up that nobody has a dashboard for. Everything is green and nothing is arriving.
//!
//! That is the outage this control exists to make loud, and it is invisible to every check
//! that looks at whether the process is alive.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use sankhya_feed::declare::Quarantine;
use sankhya_feed::stop::{Outcomes, Span, Verdict};

/// A policy that stops above a fifth of a window of ten.
fn policy() -> Quarantine {
    Quarantine { retain_days: 30, window: 10, stop_above: 0.2 }
}

#[test]
fn one_bad_record_is_an_incident_and_does_not_stop_anything() {
    let mut outcomes = Outcomes::under(policy());

    assert_eq!(outcomes.record(false), Verdict::Continue, "the first record cannot decide");
    for _ in 0..20 {
        assert_eq!(outcomes.record(true), Verdict::Continue);
    }

    assert_eq!(outcomes.totals(), (21, 1));
}

#[test]
fn a_rate_is_not_measured_before_there_is_one() {
    // Three records, all bad, is not "100% of a window" — it is three records. A feed that
    // stopped here would be stopped by its first malformed document, which is the opposite of
    // what a quarantine is for.
    let mut outcomes = Outcomes::under(policy());
    for _ in 0..9 {
        assert_eq!(outcomes.record(false), Verdict::Continue, "the window is not full yet");
    }
}

#[test]
fn a_run_of_bad_records_stops_the_feed_once_the_window_is_full() {
    let mut outcomes = Outcomes::under(policy());
    // Nine good, then the window fills with the tenth.
    for _ in 0..9 {
        assert_eq!(outcomes.record(true), Verdict::Continue);
    }
    assert_eq!(outcomes.record(false), Verdict::Continue, "one in ten is under the threshold");
    // Two in ten is *at* a fifth and not above it. `stop_above` is read as written: the
    // operator who sets 0.2 has said that a fifth is tolerable, not that it is the failure.
    assert_eq!(outcomes.record(false), Verdict::Continue, "at the threshold, not above it");

    let verdict = outcomes.record(false);
    match verdict {
        Verdict::Stop(reason) => {
            assert_eq!(reason.over, Span::Window);
            assert_eq!(reason.quarantined, 3);
            assert_eq!(reason.considered, 10);
            let said = reason.to_string();
            assert!(said.contains("20%"), "{said}");
            assert!(said.contains("nothing arrives"), "the sentence names the outage: {said}");
            assert!(said.contains("3 of the last 10"), "{said}");
        }
        other => panic!("expected a stop, got {other:?}"),
    }
}

#[test]
fn the_window_forgets_so_an_old_bad_afternoon_does_not_stop_a_feed_today() {
    // The reason this is a rate over recent records rather than a total: a total trips
    // eventually, for something that happened last March, and by then it says nothing about
    // what the feed is doing now.
    let mut outcomes = Outcomes::under(policy());
    for _ in 0..2 {
        outcomes.record(false);
    }
    for _ in 0..30 {
        assert_eq!(outcomes.record(true), Verdict::Continue, "the bad pair has fallen out");
    }
    assert_eq!(outcomes.totals(), (32, 2));
}

#[test]
fn a_source_that_produced_nothing_usable_stops_the_feed_however_short_it_was() {
    // The gap the window-based check sleeps through: a file shorter than the window is
    // quarantined in its entirety, the feed moves on to the next one, and nothing anywhere
    // says the source stopped making sense.
    let mut outcomes = Outcomes::under(policy());
    for _ in 0..4 {
        assert_eq!(outcomes.record(false), Verdict::Continue, "the window never fills");
    }

    match outcomes.finish_source() {
        Verdict::Stop(reason) => {
            assert_eq!(reason.over, Span::Source);
            assert_eq!(reason.quarantined, 4);
            assert_eq!(reason.considered, 4);
            assert!(reason.to_string().contains("not one of this source's 4"), "{reason}");
        }
        other => panic!("a source that produced nothing usable is an outage, not {other:?}"),
    }
}

#[test]
fn a_source_with_one_bad_record_in_it_is_not_an_outage() {
    // The mistake this avoids: a two-record file with one bad record trips any fraction worth
    // setting, and stopping a feed for that is the same error as stopping it for its first
    // malformed document. Anything between one bad record and an unusable source is a rate,
    // and the window is what measures rates.
    let mut outcomes = Outcomes::under(policy());
    outcomes.record(true);
    outcomes.record(false);
    assert_eq!(outcomes.finish_source(), Verdict::Continue);

    // And the per-source count resets, so this source's record is not judged with the last.
    outcomes.record(true);
    assert_eq!(outcomes.finish_source(), Verdict::Continue);
}

#[test]
fn a_run_of_half_bad_files_still_stops_the_feed_through_the_window() {
    // The window spans sources, which is what stops the previous rule from being a hole: no
    // single file is wholly bad, and the rate across them is still an outage.
    let mut outcomes = Outcomes::under(policy());
    let mut stopped = false;
    for _ in 0..5 {
        for fitted in [true, false] {
            if matches!(outcomes.record(fitted), Verdict::Stop(_)) {
                stopped = true;
            }
        }
        assert_eq!(outcomes.finish_source(), Verdict::Continue, "no one file is wholly bad");
    }
    assert!(stopped, "half of every file is a rate, and the window measures rates");
}

#[test]
fn a_source_with_no_records_is_not_a_failing_source() {
    // An empty file divided by itself is not a rate. Left as an explicit case because the
    // arithmetic version of this is a division by zero, and the convenient answer to that is
    // whatever the language happens to produce.
    let mut outcomes = Outcomes::under(policy());
    assert_eq!(outcomes.finish_source(), Verdict::Continue);
}

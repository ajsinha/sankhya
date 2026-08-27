//! Session-consistency tests.

// Tests may panic — that is how a test reports a failure. The workspace denies
// `unwrap`, `expect`, `panic` and indexing because a *server* must not do those things
// on data it did not choose; a test chooses all of its data, and an assertion that
// cannot fail loudly is worse than useless.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use proptest::prelude::*;
use sankhya_plan::{
    evaluate_visibility, wait_exhausted, FreshnessError, ReadMode, SessionToken, Visibility,
};
use sankhya_types::Lsn;
use std::time::Duration;

fn token(at: u64) -> SessionToken {
    SessionToken::at(Lsn::new(at))
}

#[test]
fn a_read_after_a_write_waits_for_that_write() {
    // The feature that makes the unified-system claim credible. Without it the first
    // thing anyone tries - write a row, then query it - shows the row missing.
    let visibility =
        evaluate_visibility(ReadMode::ReadYourWrites, Some(token(1000)), Lsn::new(900))
            .expect("waiting is not an error");

    assert_eq!(
        visibility,
        Visibility::Wait {
            until: Lsn::new(1000),
            behind_by: 100
        }
    );
}

#[test]
fn a_read_proceeds_once_capture_has_caught_up() {
    let visibility =
        evaluate_visibility(ReadMode::ReadYourWrites, Some(token(1000)), Lsn::new(1000))
            .expect("proceeds");
    assert!(matches!(visibility, Visibility::Ready { .. }));
}

#[test]
fn a_session_that_has_observed_nothing_never_waits() {
    // The ordinary first request of a session. Waiting here would add latency to every
    // connection for no benefit.
    let visibility =
        evaluate_visibility(ReadMode::ReadYourWrites, None, Lsn::new(500)).expect("proceeds");
    assert_eq!(
        visibility,
        Visibility::Ready {
            target: Lsn::new(500)
        }
    );
}

#[test]
fn eventual_reads_never_wait() {
    let visibility = evaluate_visibility(ReadMode::Eventual, Some(token(u64::MAX)), Lsn::new(1))
        .expect("proceeds");
    assert_eq!(
        visibility,
        Visibility::Ready {
            target: Lsn::new(1)
        }
    );
}

#[test]
fn a_session_token_never_goes_backwards() {
    // A client that has seen position 1000 must not later be served as though it had
    // only seen 500, whatever order its responses arrived in.
    let observed = token(1000).merge(token(500));
    assert_eq!(observed, token(1000));
    assert_eq!(token(500).merge(token(1000)), token(1000));
}

#[test]
fn a_pinned_read_of_an_uncaptured_version_fails_rather_than_approximating() {
    let err = evaluate_visibility(
        ReadMode::Snapshot { at: Lsn::new(2000) },
        None,
        Lsn::new(1000),
    )
    .expect_err("must fail");
    assert!(matches!(err, FreshnessError::SnapshotUnavailable { .. }));
}

#[test]
fn a_pinned_read_targets_its_version_not_the_latest() {
    // Pinning means immutability. Serving the newest data would defeat the entire
    // purpose - a pinned read is what reproducible output uses.
    let visibility = evaluate_visibility(
        ReadMode::Snapshot { at: Lsn::new(500) },
        None,
        Lsn::new(1000),
    )
    .expect("proceeds");
    assert_eq!(
        visibility,
        Visibility::Ready {
            target: Lsn::new(500)
        },
        "a pinned read must not drift forward to newer data"
    );
}

#[test]
fn an_exhausted_wait_becomes_an_explicit_failure() {
    // Serving the older data instead would be a silently wrong answer, and nothing
    // about it would look wrong.
    let err = wait_exhausted(Lsn::new(1000), Lsn::new(900));
    assert!(matches!(err, FreshnessError::NotReached { .. }));
    assert!(
        err.to_string()
            .contains("refused rather than answered with older data"),
        "the message should say why it failed: {err}"
    );
}

#[test]
fn bounded_reads_serve_what_exists() {
    let visibility = evaluate_visibility(
        ReadMode::Bounded {
            max_staleness: Duration::from_secs(5),
        },
        Some(token(2000)),
        Lsn::new(1000),
    )
    .expect("proceeds");
    assert_eq!(
        visibility,
        Visibility::Ready {
            target: Lsn::new(1000)
        }
    );
}

#[test]
fn read_your_writes_is_the_default() {
    // The safe default. Eventual would make write-then-read fail intermittently, which
    // is worse than a small wait because it is not reproducible.
    assert_eq!(ReadMode::default(), ReadMode::ReadYourWrites);
}

proptest! {
    /// Read-your-writes never proceeds before the observed position.
    ///
    /// The whole guarantee, stated as a property: if the client has seen a position,
    /// no read in that session may be served from before it.
    #[test]
    fn never_proceeds_before_what_the_client_has_seen(observed in any::<u64>(), applied in any::<u64>()) {
        let result = evaluate_visibility(
            ReadMode::ReadYourWrites,
            Some(token(observed)),
            Lsn::new(applied),
        );
        match result {
            Ok(Visibility::Ready { .. }) => prop_assert!(
                applied >= observed,
                "proceeded at {applied} having observed {observed}"
            ),
            Ok(Visibility::Wait { until, .. }) => prop_assert_eq!(until.get(), observed),
            Err(e) => prop_assert!(false, "read-your-writes should never fail outright: {:?}", e),
        }
    }

    /// Merging tokens is monotonic, whatever order they arrive in.
    #[test]
    fn merging_is_monotonic(a in any::<u64>(), b in any::<u64>()) {
        let merged = token(a).merge(token(b));
        prop_assert_eq!(merged, token(a.max(b)));
        prop_assert_eq!(token(b).merge(token(a)), merged, "merging must be order-independent");
    }

    /// Evaluation never panics.
    #[test]
    fn evaluation_never_panics(observed in any::<u64>(), applied in any::<u64>(), pinned in any::<u64>()) {
        for mode in [
            ReadMode::Eventual,
            ReadMode::ReadYourWrites,
            ReadMode::Bounded { max_staleness: Duration::from_millis(1) },
            ReadMode::Snapshot { at: Lsn::new(pinned) },
        ] {
            let _ = evaluate_visibility(mode, Some(token(observed)), Lsn::new(applied));
            let _ = evaluate_visibility(mode, None, Lsn::new(applied));
        }
    }
}

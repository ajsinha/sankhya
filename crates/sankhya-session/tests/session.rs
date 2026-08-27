//! Read modes and snapshot leases.
//!
//! The property worth stating first: only a pinned read is reproducible. A report rerun a
//! month later against "now" is a different report, and two people comparing their copies
//! find differences nobody can account for.
//!
//! And the lease rule: there is no unbounded lease, because a forgotten one does not fail
//! loudly. Storage simply grows, compaction accumulates superseded files it may not delete,
//! and the cause is a connection somebody opened last March.

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

use sankhya_session::lease::{LeaseError, Leases};
use sankhya_session::mode::ReadMode;
use sankhya_types::TenantId;

fn tenant(name: &str) -> TenantId {
    let mut bytes = [0u8; 16];
    for (slot, byte) in bytes.iter_mut().zip(name.bytes()) {
        *slot = byte;
    }
    TenantId::from_uuid(uuid::Uuid::from_bytes(bytes))
}

const MINUTE: i64 = 60_000_000;

// --- read modes -----------------------------------------------------------

#[test]
fn only_a_pinned_read_is_reproducible() {
    // The mode people forget, and the only one that makes a result comparable with itself
    // next week. Strong and bounded both mean "current", and current changes.
    assert!(ReadMode::Pinned { snapshot: 41 }.is_reproducible());
    assert!(!ReadMode::Strong.is_reproducible());
    assert!(!ReadMode::BoundedFreshness {
        max_lag_micros: MINUTE
    }
    .is_reproducible());
}

#[test]
fn a_bounded_read_refuses_data_staler_than_its_bound() {
    // The bound is a bound, not a hope. An unbounded "eventually consistent" mode lets a
    // stalled applier serve week-old data with nothing in the result to say so.
    let mode = ReadMode::BoundedFreshness {
        max_lag_micros: MINUTE,
    };
    assert!(mode.accepts_lag(MINUTE - 1));
    assert!(mode.accepts_lag(MINUTE), "the bound itself is acceptable");
    assert!(!mode.accepts_lag(MINUTE + 1));
}

#[test]
fn a_strong_read_accepts_no_lag_at_all() {
    assert!(ReadMode::Strong.accepts_lag(0));
    assert!(!ReadMode::Strong.accepts_lag(1));
}

#[test]
fn a_pinned_read_is_not_stale_by_any_amount() {
    // It asked for a specific version and got it. Measuring its lag against "now" would be
    // measuring the wrong thing entirely.
    let pinned = ReadMode::Pinned { snapshot: 7 };
    assert!(pinned.accepts_lag(i64::MAX));
}

#[test]
fn a_mode_describes_itself_well_enough_to_appear_in_a_result() {
    assert_eq!(ReadMode::Strong.to_string(), "strong");
    assert!(ReadMode::Pinned { snapshot: 41 }.to_string().contains("41"));
    assert!(ReadMode::BoundedFreshness {
        max_lag_micros: 500
    }
    .to_string()
    .contains("500"));
}

// --- leases ---------------------------------------------------------------

#[test]
fn a_lease_holds_a_snapshot_until_it_expires() {
    let mut leases = Leases::with_max_lifetime(10 * MINUTE);
    leases.acquire("s1", tenant("acme"), 41, 0, MINUTE);

    assert!(leases.is_held("s1", 0));
    assert!(leases.is_held("s1", MINUTE - 1));
    assert!(!leases.is_held("s1", MINUTE), "expiry is exclusive");
    assert_eq!(leases.held_snapshots(&tenant("acme"), 0), vec![41]);
    assert!(leases.held_snapshots(&tenant("acme"), MINUTE).is_empty());
}

#[test]
fn a_request_for_longer_than_the_maximum_is_clamped_and_the_grant_is_returned() {
    // Refusing would push callers towards asking for exactly the maximum, which is the same
    // thing with more steps. Silently granting what was asked for would defeat the bound.
    let mut leases = Leases::with_max_lifetime(MINUTE);
    let granted = leases.acquire("s1", tenant("acme"), 41, 0, 100 * MINUTE);

    assert_eq!(
        granted.expires_at, MINUTE,
        "the caller is told what it actually got, not what it asked for"
    );
    assert!(!leases.is_held("s1", MINUTE));
}

#[test]
fn there_is_no_way_to_construct_an_unbounded_registry() {
    // The unbounded case is the one that quietly stops a warehouse reclaiming space, so it
    // is not expressible. Even zero becomes one.
    let leases = Leases::with_max_lifetime(0);
    assert!(leases.max_lifetime_micros() >= 1);
}

#[test]
fn a_live_lease_can_be_renewed() {
    let mut leases = Leases::with_max_lifetime(10 * MINUTE);
    leases.acquire("s1", tenant("acme"), 41, 0, MINUTE);

    let renewed = leases
        .renew("s1", MINUTE / 2, MINUTE)
        .expect("a live lease renews");
    assert_eq!(renewed.expires_at, MINUTE / 2 + MINUTE);
    assert!(
        leases.is_held("s1", MINUTE),
        "it now outlives its first expiry"
    );
}

#[test]
fn an_expired_lease_cannot_be_renewed_only_reacquired() {
    // Renewing an expired lease would let a session hold a snapshot maintenance was already
    // entitled to delete, making it a matter of timing whether the files still exist.
    let mut leases = Leases::with_max_lifetime(10 * MINUTE);
    leases.acquire("s1", tenant("acme"), 41, 0, MINUTE);

    let Err(error) = leases.renew("s1", MINUTE + 1, MINUTE) else {
        panic!("an expired lease must not renew");
    };
    let LeaseError::Expired { expired_at, .. } = error else {
        panic!("expected an expiry error");
    };
    assert_eq!(expired_at, MINUTE);
    assert!(error.to_string().contains("entitled to delete"));
}

#[test]
fn renewing_a_lease_that_was_never_taken_is_refused() {
    let mut leases = Leases::with_max_lifetime(MINUTE);
    assert_eq!(
        leases.renew("nobody", 0, MINUTE),
        Err(LeaseError::NoSuchLease {
            session: "nobody".to_string()
        })
    );
}

#[test]
fn maintenance_is_bounded_by_the_oldest_snapshot_anyone_holds() {
    // The number retention actually consults. A file belonging to any held snapshot must
    // not be deleted, and the oldest bounds how far back retention has to reach.
    let mut leases = Leases::with_max_lifetime(10 * MINUTE);
    leases.acquire("s1", tenant("acme"), 41, 0, MINUTE);
    leases.acquire("s2", tenant("acme"), 12, 0, MINUTE);
    leases.acquire("s3", tenant("other"), 99, 0, MINUTE);

    assert_eq!(leases.oldest_held(0), Some(12));
    assert_eq!(
        leases.oldest_held(MINUTE),
        None,
        "once everything expires, retention applies its ordinary policy"
    );
}

#[test]
fn one_tenants_lease_does_not_appear_in_anothers_held_set() {
    let mut leases = Leases::with_max_lifetime(10 * MINUTE);
    leases.acquire("s1", tenant("acme"), 41, 0, MINUTE);
    leases.acquire("s2", tenant("other"), 99, 0, MINUTE);

    assert_eq!(leases.held_snapshots(&tenant("acme"), 0), vec![41]);
    assert_eq!(leases.held_snapshots(&tenant("other"), 0), vec![99]);
}

#[test]
fn expiring_returns_what_it_removed_so_maintenance_can_act_on_it() {
    let mut leases = Leases::with_max_lifetime(10 * MINUTE);
    leases.acquire("short", tenant("acme"), 41, 0, MINUTE);
    leases.acquire("long", tenant("acme"), 42, 0, 5 * MINUTE);

    let expired = leases.expire(2 * MINUTE);
    assert_eq!(expired.len(), 1);
    assert_eq!(expired.first().map(|l| l.snapshot), Some(41));
    assert_eq!(leases.live_count(2 * MINUTE), 1);
}

#[test]
fn releasing_a_lease_frees_its_snapshot_immediately() {
    // A long job that finishes early should not hold a snapshot for the rest of its lease.
    let mut leases = Leases::with_max_lifetime(10 * MINUTE);
    leases.acquire("s1", tenant("acme"), 41, 0, 10 * MINUTE);
    assert_eq!(leases.oldest_held(0), Some(41));

    let released = leases.release("s1").expect("it was held");
    assert_eq!(released.snapshot, 41);
    assert_eq!(leases.oldest_held(0), None);
}

#[test]
fn two_sessions_pinning_the_same_snapshot_both_hold_it() {
    let mut leases = Leases::with_max_lifetime(10 * MINUTE);
    leases.acquire("s1", tenant("acme"), 41, 0, MINUTE);
    leases.acquire("s2", tenant("acme"), 41, 0, 5 * MINUTE);

    assert_eq!(leases.held_snapshots(&tenant("acme"), 0), vec![41]);
    leases.expire(2 * MINUTE);
    assert_eq!(
        leases.held_snapshots(&tenant("acme"), 2 * MINUTE),
        vec![41],
        "one lease expiring must not release a snapshot another still holds"
    );
}

// --- session tokens and read-your-own-writes ------------------------------

use sankhya_session::token::{
    is_visible, CommitPosition, Contradiction, Session, SessionRequest, SessionToken,
};

#[test]
fn a_write_token_makes_the_write_visible_to_the_next_query() {
    // FR-API-13, and the requirement says plainly why it exists: without it the first
    // demonstration anyone attempts shows their own write missing, and they reasonably
    // conclude the system is broken. Nothing is broken — the write is committed and
    // publication has not reached it.
    let token = SessionToken::issue(CommitPosition(500));
    let session = Session::new();

    let resolved = session
        .resolve(&SessionRequest {
            mode: ReadMode::BoundedFreshness {
                max_lag_micros: MINUTE,
            },
            after: Some(token),
        })
        .expect("no contradiction outside a pin");

    assert_eq!(
        resolved.wait_for,
        Some(CommitPosition(500)),
        "the query must wait for the write rather than answering without it"
    );
    assert!(!is_visible(CommitPosition(499), CommitPosition(500)));
    assert!(is_visible(CommitPosition(500), CommitPosition(500)));
    assert!(is_visible(CommitPosition(501), CommitPosition(500)));
}

#[test]
fn a_query_with_no_token_waits_for_nothing() {
    let resolved = Session::new()
        .resolve(&SessionRequest {
            mode: ReadMode::Strong,
            after: None,
        })
        .expect("no contradiction");
    assert_eq!(resolved.wait_for, None);
}

#[test]
fn a_token_is_opaque_and_round_trips() {
    // FR-API-16. A client that could read a version out of this would depend on it, and the
    // next format change would be a breaking wire change.
    let token = SessionToken::issue(CommitPosition(41));
    let text = token.to_string();

    assert_eq!(SessionToken::parse(&text), Some(token.clone()));
    assert_eq!(token.position(), Some(CommitPosition(41)));
}

#[test]
fn a_token_this_server_did_not_issue_is_not_accepted() {
    for forged in ["", "41", "skhy2-41", "skhy1-", "skhy1-notanumber", "hello"] {
        assert_eq!(
            SessionToken::parse(forged),
            None,
            "'{forged}' must not parse as a token"
        );
    }
}

#[test]
fn asking_for_freshness_inside_a_pinned_session_is_rejected_not_reconciled() {
    // FR-API-14. Reconciling silently means picking one of the two, and whichever is
    // picked, some caller gets the opposite of what they asked for without being told.
    let session = Session::pinned_to(41);

    for mode in [
        ReadMode::Strong,
        ReadMode::BoundedFreshness {
            max_lag_micros: MINUTE,
        },
    ] {
        let Err(contradiction) = session.resolve(&SessionRequest { mode, after: None }) else {
            panic!("{mode} inside a pinned session must be rejected");
        };
        assert!(matches!(
            contradiction,
            Contradiction::FreshnessInsidePin { pinned: 41, .. }
        ));
        assert!(contradiction
            .to_string()
            .contains("cannot both be honoured"));
    }
}

#[test]
fn a_pinned_session_reading_its_own_snapshot_is_fine() {
    let session = Session::pinned_to(41);
    let resolved = session
        .resolve(&SessionRequest {
            mode: ReadMode::Pinned { snapshot: 41 },
            after: None,
        })
        .expect("consistent with the pin");
    assert_eq!(resolved.mode, ReadMode::Pinned { snapshot: 41 });
}

#[test]
fn a_pinned_session_will_not_be_overridden_per_request() {
    // Otherwise repeatable reads are not repeatable.
    let Err(contradiction) = Session::pinned_to(41).resolve(&SessionRequest {
        mode: ReadMode::Pinned { snapshot: 99 },
        after: None,
    }) else {
        panic!("a different snapshot inside a pin must be rejected");
    };
    assert!(matches!(
        contradiction,
        Contradiction::DifferentPin {
            session_pin: 41,
            requested: 99
        }
    ));
    assert!(contradiction.to_string().contains("not repeatable"));
}

#[test]
fn a_pinned_session_does_not_wait_for_a_write_token() {
    // A pinned session reads committed data only. The pinned snapshot either includes the
    // write or predates it, and waiting cannot change which — so waiting would only ever
    // add latency to an answer that was already determined.
    let resolved = Session::pinned_to(41)
        .resolve(&SessionRequest {
            mode: ReadMode::Pinned { snapshot: 41 },
            after: Some(SessionToken::issue(CommitPosition(999))),
        })
        .expect("consistent");
    assert_eq!(resolved.wait_for, None);
}

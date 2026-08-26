//! What the governor refuses, and whether the caller can act on the refusal.

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

use sankhya_governor::{admit, Decision, Demand, PoolState, Posture, Rejection, TenantLimits};
use std::collections::BTreeMap;

const MIB: u64 = 1024 * 1024;

fn pool(in_use: u64, per_tenant: &[(&str, u64)], queued: usize) -> PoolState {
    PoolState {
        total_bytes: 100 * MIB,
        in_use_bytes: in_use,
        per_tenant: per_tenant
            .iter()
            .map(|(t, b)| ((*t).to_string(), *b))
            .collect::<BTreeMap<_, _>>(),
        queued,
        max_queue_depth: 4,
    }
}

fn limits(floor: u64, cap: u64) -> TenantLimits {
    TenantLimits {
        floor_bytes: floor,
        cap_bytes: cap,
    }
}

fn demand(tenant: &str, bytes: u64) -> Demand {
    Demand {
        tenant: tenant.to_string(),
        estimated_bytes: bytes,
    }
}

#[test]
fn a_query_that_fits_is_admitted() {
    let d = admit(
        &demand("a", 10 * MIB),
        &pool(0, &[], 0),
        &limits(20 * MIB, 50 * MIB),
        Posture::Admitting,
    );
    assert_eq!(d, Decision::Admit);
}

#[test]
fn a_query_larger_than_the_pool_is_told_that_waiting_will_not_help() {
    // The distinction the caller most needs. A client told "try again" for a query that
    // can never fit retries forever, and every retry costs another planning pass.
    let d = admit(
        &demand("a", 500 * MIB),
        &pool(0, &[], 0),
        &limits(20 * MIB, 600 * MIB),
        Posture::Admitting,
    );

    let Decision::Reject(r) = d else {
        panic!("expected a refusal");
    };
    assert!(matches!(r, Rejection::ExceedsPool { .. }));
    assert!(!r.retryable());
    assert!(format!("{r}").contains("retrying will not help"));
}

#[test]
fn a_query_larger_than_the_tenants_cap_is_also_permanent() {
    let d = admit(
        &demand("a", 60 * MIB),
        &pool(0, &[], 0),
        &limits(10 * MIB, 50 * MIB),
        Posture::Admitting,
    );
    let Decision::Reject(r) = d else {
        panic!("expected a refusal");
    };
    assert!(matches!(r, Rejection::ExceedsTenantCap { .. }));
    assert!(!r.retryable());
}

#[test]
fn a_permanent_refusal_arrives_before_any_queueing() {
    // A query that can never run must not occupy a queue slot ahead of work that could,
    // and must not wait to be told what is already known.
    let d = admit(
        &demand("a", 500 * MIB),
        &pool(100 * MIB, &[], 4),
        &limits(20 * MIB, 600 * MIB),
        Posture::Admitting,
    );
    assert!(matches!(d, Decision::Reject(Rejection::ExceedsPool { .. })));
}

#[test]
fn a_query_that_does_not_fit_right_now_queues_and_can_retry() {
    let d = admit(
        &demand("a", 40 * MIB),
        &pool(90 * MIB, &[("a", 10 * MIB)], 1),
        &limits(5 * MIB, 60 * MIB),
        Posture::Admitting,
    );
    assert_eq!(d, Decision::Queue { position: 1 });
}

#[test]
fn a_full_queue_refuses_immediately_rather_than_waiting() {
    // Unbounded queueing is rejection with the latency hidden. The same refusal after a
    // timeout has cost a connection, a slot in someone's budget, and the user's
    // patience, to deliver the answer that was available at once.
    let d = admit(
        &demand("a", 40 * MIB),
        &pool(90 * MIB, &[("a", 10 * MIB)], 4),
        &limits(5 * MIB, 60 * MIB),
        Posture::Admitting,
    );
    let Decision::Reject(r) = d else {
        panic!("expected a refusal");
    };
    assert_eq!(r, Rejection::QueueFull { depth: 4 });
    assert!(r.retryable());
    assert!(format!("{r}").contains("arrives immediately"));
}

#[test]
fn a_tenant_within_its_floor_is_admitted_under_global_pressure() {
    // The whole point of a floor. Without it a tenant that submits steadily holds the
    // pool and a tenant that submits occasionally never finds room -- which makes
    // "shared" mean "whoever asks most often".
    let d = admit(
        &demand("quiet", 4 * MIB),
        // The pool is nearly full and none of it is this tenant's.
        &pool(99 * MIB, &[("busy", 99 * MIB)], 0),
        &limits(10 * MIB, 50 * MIB),
        Posture::Admitting,
    );
    assert_eq!(d, Decision::Admit);
}

#[test]
fn a_tenant_past_its_floor_competes_like_everyone_else() {
    // The floor is a guarantee, not an allocation. Beyond it, a tenant takes its turn.
    let d = admit(
        &demand("a", 20 * MIB),
        &pool(95 * MIB, &[("a", 10 * MIB)], 0),
        &limits(10 * MIB, 50 * MIB),
        Posture::Admitting,
    );
    assert_eq!(d, Decision::Queue { position: 0 });
}

#[test]
fn the_cap_binds_even_when_the_pool_is_idle() {
    // An idle pool is otherwise an invitation, and one tenant's bad afternoon becomes
    // everyone's.
    let d = admit(
        &demand("a", 30 * MIB),
        &pool(30 * MIB, &[("a", 30 * MIB)], 0),
        &limits(10 * MIB, 50 * MIB),
        Posture::Admitting,
    );
    assert_eq!(d, Decision::Queue { position: 0 });
}

#[test]
fn a_shedding_system_admits_nothing_however_much_is_free() {
    // Not a capacity question. Under the higher pressure levels everything left goes to
    // whatever is defending the source, and free memory is not the reason to say no.
    let d = admit(
        &demand("a", 1 * MIB),
        &pool(0, &[], 0),
        &limits(50 * MIB, 100 * MIB),
        Posture::Shedding,
    );
    let Decision::Reject(r) = d else {
        panic!("expected a refusal");
    };
    assert_eq!(r, Rejection::NotAdmitting);
    assert!(r.retryable(), "shedding ends; the query can come back");
}

#[test]
fn a_query_too_large_to_ever_run_is_refused_even_while_shedding() {
    // Shedding is temporary and this is not. Telling a client "try later" about a query
    // that will never fit is worse than useless while shedding, because it will.
    let d = admit(
        &demand("a", 500 * MIB),
        &pool(0, &[], 0),
        &limits(50 * MIB, 600 * MIB),
        Posture::Shedding,
    );
    assert!(matches!(d, Decision::Reject(Rejection::ExceedsPool { .. })));
}

#[test]
fn a_query_needing_exactly_what_is_free_is_admitted() {
    // Boundary. Refusing here would leave the last of the pool permanently unusable.
    let d = admit(
        &demand("a", 40 * MIB),
        &pool(60 * MIB, &[("a", 20 * MIB)], 0),
        &limits(10 * MIB, 100 * MIB),
        Posture::Admitting,
    );
    assert_eq!(d, Decision::Admit);
}

#[test]
fn a_query_needing_exactly_the_whole_pool_is_admitted_when_it_is_empty() {
    let d = admit(
        &demand("a", 100 * MIB),
        &pool(0, &[], 0),
        &limits(10 * MIB, 100 * MIB),
        Posture::Admitting,
    );
    assert_eq!(d, Decision::Admit);
}

//! Quotas, and refusals a person can act on.
//!
//! The tests here are as much about the *message* as the decision. A refusal that does not
//! say which limit, what was asked for and what is allowed cannot be acted on, and the
//! recipient opens a ticket instead of fixing their query.

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

use sankhya_governor::quota::{Consumption, Limit, Quota, Quotas, Refusal, Request};
use sankhya_types::TenantId;

fn tenant(name: &str) -> TenantId {
    let mut bytes = [0u8; 16];
    for (slot, byte) in bytes.iter_mut().zip(name.bytes()) {
        *slot = byte;
    }
    TenantId::from_uuid(uuid::Uuid::from_bytes(bytes))
}

fn quotas() -> Quotas {
    let mut q = Quotas::new();
    q.set(
        tenant("acme"),
        Quota {
            max_concurrent_queries: 2,
            max_scan_bytes: 1_000,
            max_result_rows: 100,
            max_storage_bytes: 10_000,
            max_graph_epochs: 2,
        },
    );
    q
}

#[test]
fn a_request_within_every_limit_is_admitted() {
    // The positive case, so the refusals below mean something.
    let q = quotas();
    assert!(q
        .admit(
            &tenant("acme"),
            &Request {
                scan_bytes: 500,
                result_rows: 50,
                ..Request::default()
            }
        )
        .is_ok());
}

#[test]
fn an_unconfigured_tenant_is_refused_rather_than_inheriting() {
    // A tenant nobody configured should not receive whatever the most permissive tenant
    // happens to have.
    let q = quotas();
    let Err(refusal) = q.admit(&tenant("stranger"), &Request::default()) else {
        panic!("a tenant with no quota and no default must be refused");
    };
    assert_eq!(refusal.limit(), None);
    assert!(!refusal.retryable());
    assert!(refusal.to_string().contains("most permissive"));
}

#[test]
fn a_default_applies_only_where_no_quota_was_set() {
    let q = quotas().with_default(Quota {
        max_scan_bytes: 10,
        ..Quota::generous()
    });

    // The stranger gets the default, which is tight.
    let Err(refusal) = q.admit(
        &tenant("stranger"),
        &Request {
            scan_bytes: 50,
            ..Request::default()
        },
    ) else {
        panic!("the default limit must bind");
    };
    assert_eq!(refusal.limit(), Some(Limit::ScanBytes));

    // Acme keeps its own, which is looser.
    assert!(q
        .admit(
            &tenant("acme"),
            &Request {
                scan_bytes: 50,
                ..Request::default()
            }
        )
        .is_ok());
}

#[test]
fn a_refusal_names_the_limit_the_numbers_and_the_setting() {
    // "Quota exceeded" is not an error message, it is a shrug. Three things are needed to
    // act: which limit, what was asked for, what is allowed.
    let q = quotas();
    let Err(refusal) = q.admit(
        &tenant("acme"),
        &Request {
            scan_bytes: 5_000,
            ..Request::default()
        },
    ) else {
        panic!("a scan of 5,000 must not fit a limit of 1,000");
    };

    let Refusal::Exceeded {
        limit,
        requested,
        allowed,
        ..
    } = &refusal
    else {
        panic!("expected an exceeded-limit refusal");
    };
    assert_eq!(*limit, Limit::ScanBytes);
    assert_eq!(*requested, 5_000);
    assert_eq!(*allowed, 1_000);

    let message = refusal.to_string();
    assert!(message.contains("max_scan_bytes"), "{message}");
    assert!(
        message.contains("5000") && message.contains("1000"),
        "{message}"
    );
}

#[test]
fn a_limit_that_frees_on_its_own_says_retrying_may_help() {
    // Being over quota is not being forbidden, and conflating them produces the wrong
    // client behaviour.
    let mut q = quotas();
    q.observe(
        tenant("acme"),
        Consumption {
            running_queries: 2,
            ..Consumption::default()
        },
    );

    let Err(refusal) = q.admit(&tenant("acme"), &Request::default()) else {
        panic!("a third query must not be admitted against a limit of two");
    };
    assert_eq!(refusal.limit(), Some(Limit::ConcurrentQueries));
    assert!(refusal.retryable(), "concurrency frees up as work finishes");
    assert!(refusal.to_string().contains("retrying shortly may succeed"));
}

#[test]
fn a_limit_that_does_not_free_says_retrying_will_not_help() {
    // Telling a client to retry something that can never succeed turns one refusal into a
    // loop.
    let q = quotas();
    let Err(refusal) = q.admit(
        &tenant("acme"),
        &Request {
            result_rows: 1_000,
            ..Request::default()
        },
    ) else {
        panic!("a thousand rows must not fit a limit of a hundred");
    };
    assert_eq!(refusal.limit(), Some(Limit::ResultRows));
    assert!(!refusal.retryable());
    assert!(refusal.to_string().contains("will fail the same way"));
}

#[test]
fn a_scan_bound_and_a_result_bound_are_separate_limits() {
    // They fail differently: a large scan costs the server, a large result costs the
    // client, which is frequently the thing that actually falls over.
    let q = quotas();
    let big_scan = q.admit(
        &tenant("acme"),
        &Request {
            scan_bytes: 5_000,
            result_rows: 1,
            ..Request::default()
        },
    );
    let big_result = q.admit(
        &tenant("acme"),
        &Request {
            scan_bytes: 1,
            result_rows: 5_000,
            ..Request::default()
        },
    );
    assert_eq!(big_scan.unwrap_err().limit(), Some(Limit::ScanBytes));
    assert_eq!(big_result.unwrap_err().limit(), Some(Limit::ResultRows));
}

#[test]
fn storage_is_measured_against_what_is_already_stored() {
    // A write that fits on its own may not fit on top of what is there.
    let mut q = quotas();
    q.observe(
        tenant("acme"),
        Consumption {
            storage_bytes: 9_500,
            ..Consumption::default()
        },
    );

    assert!(q
        .admit(
            &tenant("acme"),
            &Request {
                write_bytes: 400,
                ..Request::default()
            }
        )
        .is_ok());

    let Err(refusal) = q.admit(
        &tenant("acme"),
        &Request {
            write_bytes: 600,
            ..Request::default()
        },
    ) else {
        panic!("9,500 plus 600 exceeds 10,000");
    };
    assert_eq!(refusal.limit(), Some(Limit::StorageBytes));
    let Refusal::Exceeded { requested, .. } = refusal else {
        panic!("expected an exceeded-limit refusal");
    };
    assert_eq!(
        requested, 10_100,
        "the message reports the total, not the delta"
    );
}

#[test]
fn a_graph_epoch_bound_only_applies_to_a_request_that_hydrates_one() {
    // An epoch is large and lives until its last reader releases it. Without a bound, a
    // tenant issuing traversals against successive snapshots holds every one between them.
    let mut q = quotas();
    q.observe(
        tenant("acme"),
        Consumption {
            graph_epochs: 2,
            ..Consumption::default()
        },
    );

    assert!(
        q.admit(&tenant("acme"), &Request::default()).is_ok(),
        "a query that reuses an existing epoch is unaffected"
    );

    let Err(refusal) = q.admit(
        &tenant("acme"),
        &Request {
            hydrates_epoch: true,
            ..Request::default()
        },
    ) else {
        panic!("a third epoch must not be admitted against a limit of two");
    };
    assert_eq!(refusal.limit(), Some(Limit::GraphEpochs));
    assert!(
        refusal.retryable(),
        "an epoch is freed when its readers finish"
    );
}

#[test]
fn the_same_request_always_produces_the_same_refusal() {
    // Several limits are exceeded at once. Reporting an arbitrary one would make a client's
    // retry behaviour depend on iteration order.
    let q = quotas();
    let over_everything = Request {
        scan_bytes: 999_999,
        result_rows: 999_999,
        write_bytes: 999_999,
        hydrates_epoch: true,
    };
    let first = q.admit(&tenant("acme"), &over_everything).unwrap_err();
    for _ in 0..20 {
        assert_eq!(
            q.admit(&tenant("acme"), &over_everything).unwrap_err(),
            first
        );
    }
    assert_eq!(
        first.limit(),
        Some(Limit::ScanBytes),
        "the fixed first check"
    );
}

#[test]
fn one_tenants_consumption_does_not_affect_another() {
    let mut q = quotas();
    q.set(tenant("other"), Quota::generous());
    q.observe(
        tenant("acme"),
        Consumption {
            running_queries: 99,
            storage_bytes: 999_999,
            graph_epochs: 99,
        },
    );

    assert!(q.admit(&tenant("acme"), &Request::default()).is_err());
    assert!(
        q.admit(&tenant("other"), &Request::default()).is_ok(),
        "another tenant's consumption must not be charged to this one"
    );
}

#[test]
fn every_limit_has_a_setting_name_an_operator_can_search_for() {
    for limit in [
        Limit::ConcurrentQueries,
        Limit::ScanBytes,
        Limit::ResultRows,
        Limit::StorageBytes,
        Limit::GraphEpochs,
    ] {
        assert!(!limit.setting().is_empty());
        assert!(
            limit.setting().starts_with("max_"),
            "{} does not look like a setting",
            limit.setting()
        );
    }
}

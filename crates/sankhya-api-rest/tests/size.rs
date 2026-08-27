//! What the gateway hands back instead of bulk JSON.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_api_flight::ticket::Ticket;
use sankhya_api_rest::size::{deliver, Budget, Delivery, Estimate, MAX_BYTES, MAX_ROWS};
use sankhya_types::TenantId;
use std::sync::atomic::{AtomicUsize, Ordering};

fn tenant() -> TenantId {
    TenantId::from_uuid(uuid::Uuid::from_u128(1))
}

fn ticket() -> Ticket {
    Ticket::issue(tenant(), "SELECT * FROM sales.orders", 7, 0, 60_000_000)
}

// --- the decision -------------------------------------------------------

#[test]
fn a_small_result_is_encoded_inline() {
    let delivery = deliver(
        Estimate {
            rows: 20,
            bytes: 4_096,
        },
        tenant(),
        "SELECT * FROM sales.orders LIMIT 20",
        ticket,
    );
    assert_eq!(delivery, Delivery::Inline);
    assert!(delivery.inline());
}

#[test]
fn too_many_rows_comes_back_as_a_ticket_not_an_error() {
    // FR-API-06 says "returning a Flight ticket for anything larger", and the distinction
    // matters: a 413 sends somebody to ask for a bigger cap, and a ticket sends them to the
    // surface built for what they are doing.
    let delivery = deliver(
        Estimate {
            rows: MAX_ROWS + 1,
            bytes: 100,
        },
        tenant(),
        "SELECT * FROM sales.orders",
        ticket,
    );
    let Delivery::Redirect { ticket, why } = delivery else {
        panic!("a large result is redirected, not refused");
    };
    assert_eq!(ticket.statement(), "SELECT * FROM sales.orders");
    assert!(why.contains("Arrow Flight SQL"), "{why}");
    assert!(
        why.contains("not a quota to be raised"),
        "the reason has to be stated or the first thing anybody does is ask for more: {why}"
    );
}

#[test]
fn too_many_bytes_redirects_even_when_the_row_count_is_small() {
    // A row can be arbitrarily large — one row holding a megabyte of payload is exactly the
    // case a row count misses.
    let delivery = deliver(
        Estimate {
            rows: 1,
            bytes: (MAX_BYTES + 1) as u64,
        },
        tenant(),
        "SELECT payload FROM sales.blobs WHERE id = 7",
        ticket,
    );
    assert!(matches!(delivery, Delivery::Redirect { .. }), "{delivery:?}");
}

#[test]
fn a_ticket_is_issued_only_when_one_is_needed() {
    // Issuing a ticket for every request would put a redeemable credential into every small
    // response, which is a larger surface than the feature is worth.
    let issued = AtomicUsize::new(0);
    let counting = || {
        issued.fetch_add(1, Ordering::SeqCst);
        ticket()
    };
    deliver(
        Estimate {
            rows: 5,
            bytes: 10,
        },
        tenant(),
        "SELECT 1",
        counting,
    );
    assert_eq!(issued.load(Ordering::SeqCst), 0, "a small result needs none");

    let counting = || {
        issued.fetch_add(1, Ordering::SeqCst);
        ticket()
    };
    deliver(
        Estimate {
            rows: MAX_ROWS * 10,
            bytes: 10,
        },
        tenant(),
        "SELECT 1",
        counting,
    );
    assert_eq!(issued.load(Ordering::SeqCst), 1);
}

#[test]
fn the_boundary_is_inclusive_on_the_side_that_is_allowed() {
    // Exactly at the cap is allowed; one past it is not. Stated as a test because an
    // off-by-one here is the difference between a documented limit and a lie.
    assert_eq!(
        deliver(
            Estimate {
                rows: MAX_ROWS,
                bytes: MAX_BYTES as u64
            },
            tenant(),
            "SELECT 1",
            ticket
        ),
        Delivery::Inline
    );
    assert!(!deliver(
        Estimate {
            rows: MAX_ROWS + 1,
            bytes: 0
        },
        tenant(),
        "SELECT 1",
        ticket
    )
    .inline());
}

#[test]
fn a_long_statement_is_shortened_before_it_reaches_a_message() {
    // Query text is data. A predicate carrying a customer's identifier does not belong in a
    // message a proxy might log, so even here it is bounded rather than echoed whole.
    let long = format!("SELECT * FROM sales.orders WHERE {}", "x".repeat(500));
    let Delivery::Redirect { why, .. } = deliver(
        Estimate {
            rows: 1,
            bytes: (MAX_BYTES + 1) as u64,
        },
        tenant(),
        &long,
        ticket,
    ) else {
        panic!("redirected");
    };
    assert!(why.contains('…'), "it was shortened: {why}");
    assert!(
        !why.contains(&"x".repeat(200)),
        "and not by very much less than all of it"
    );
}

// --- the second guard ---------------------------------------------------

#[test]
fn encoding_stops_when_the_estimate_was_wrong() {
    // An estimate can be wrong in the direction that matters: a plan expecting a hundred
    // rows can produce a million, and the first guard has already let the request through.
    let mut budget = Budget::new();
    let mut accepted = 0_u64;
    // Each "row" is a kilobyte, so the byte cap is reached long before the row cap.
    while budget.accept(1024) {
        accepted += 1;
        assert!(accepted < MAX_ROWS * 2, "the budget never stopped");
    }
    assert!(budget.exceeded());
    assert!(budget.bytes() > MAX_BYTES);
    assert!(accepted < MAX_ROWS, "it stopped on bytes, not on rows");
}

#[test]
fn the_row_cap_stops_encoding_even_when_rows_are_tiny() {
    let mut budget = Budget::new();
    let mut offered = 0_u64;
    while budget.accept(1) {
        offered += 1;
        assert!(
            offered <= MAX_ROWS * 2,
            "the budget never refused: it accepted {offered} unit(s)"
        );
    }
    assert!(budget.exceeded());
    assert_eq!(budget.rows(), MAX_ROWS + 1, "it stopped one past the cap");
}

#[test]
fn a_budget_that_has_refused_once_keeps_refusing() {
    // A caller that ignores one refusal and asks again about a smaller row must not be told
    // to carry on: the response is already over, and resuming would produce a body that is
    // over the cap and missing the rows in between.
    let mut budget = Budget::new();
    let mut offered = 0_u64;
    while budget.accept(2048) {
        offered += 1;
        assert!(
            offered <= MAX_ROWS * 2,
            "the budget never refused: it accepted {offered} unit(s)"
        );
    }
    let accepted_rows = budget.rows();
    let accepted_bytes = budget.bytes();

    assert!(!budget.accept(1), "a small row does not reopen it");
    assert!(!budget.accept(0));

    // And a refused row changes nothing.
    //
    // This is what makes the early return load-bearing rather than redundant. Without it the
    // counters keep climbing on every rejected call, because bytes and rows only ever grow —
    // so `accept` still returns false and the *behaviour* looks identical. What differs is
    // the number in the overrun message, which would then describe what was **offered**
    // rather than what was **accepted**, and an operator reading "at 41,000 rows" about a
    // response that stopped at 10,001 is being told something untrue.
    assert_eq!(budget.rows(), accepted_rows, "a refused row was counted");
    assert_eq!(budget.bytes(), accepted_bytes, "a refused row's bytes were counted");
    assert!(
        budget
            .overrun()
            .expect("it overran")
            .contains(&format!("{accepted_rows} row(s)")),
        "the message reports what was accepted"
    );
}

#[test]
fn an_overrun_is_abandoned_rather_than_truncated() {
    // A JSON array cut short is either invalid or — worse — valid and silently short, and a
    // client cannot tell the second from a small result.
    let mut budget = Budget::new();
    let mut offered = 0_u64;
    while budget.accept(4096) {
        offered += 1;
        assert!(
            offered <= MAX_ROWS * 2,
            "the budget never refused: it accepted {offered} unit(s)"
        );
    }
    let overrun = budget.overrun().expect("it overran");
    assert!(overrun.contains("estimate was low"), "{overrun}");
    assert!(overrun.contains("discarded rather than sent"), "{overrun}");
    assert!(
        overrun.contains("silently incomplete"),
        "the reason truncation is worse than failure has to be in the message: {overrun}"
    );
}

#[test]
fn a_response_that_fits_has_nothing_to_report() {
    let mut budget = Budget::new();
    for _ in 0..100 {
        assert!(budget.accept(64));
    }
    assert!(!budget.exceeded());
    assert_eq!(budget.overrun(), None);
    assert_eq!(budget.rows(), 100);
    assert_eq!(budget.bytes(), 6_400);
}

// --- the ticket that comes back is usable -------------------------------

#[test]
fn the_ticket_handed_back_is_bound_to_the_tenant_that_asked() {
    // Otherwise the redirection is a privilege escalation: a large result would hand out a
    // credential anybody could redeem.
    let Delivery::Redirect { ticket, .. } = deliver(
        Estimate {
            rows: MAX_ROWS + 1,
            bytes: 0,
        },
        tenant(),
        "SELECT * FROM sales.orders",
        ticket,
    ) else {
        panic!("redirected");
    };
    assert!(ticket.admit(&tenant(), 1_000).is_ok());

    let somebody_else = TenantId::from_uuid(uuid::Uuid::from_u128(2));
    assert!(
        ticket.admit(&somebody_else, 1_000).is_err(),
        "a ticket is redeemable only by whoever it was issued to"
    );
}

#[test]
fn the_ticket_expires() {
    let Delivery::Redirect { ticket, .. } = deliver(
        Estimate {
            rows: MAX_ROWS + 1,
            bytes: 0,
        },
        tenant(),
        "SELECT * FROM sales.orders",
        ticket,
    ) else {
        panic!("redirected");
    };
    assert!(ticket.admit(&tenant(), 59_000_000).is_ok());
    assert!(
        ticket.admit(&tenant(), 61_000_000).is_err(),
        "a redirection that never expires is a permanent credential"
    );
}

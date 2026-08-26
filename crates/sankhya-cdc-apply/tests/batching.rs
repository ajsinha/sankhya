//! The apply path's invariants, exercised without a database.
//!
//! These run in milliseconds because the seam is pure. That is the whole reason the
//! seam is placed here: the same assurance through a live database would be a thousand
//! times slower and non-deterministic, which in practice means far less of it.

use proptest::prelude::*;
use sankhya_cdc_apply::{apply_unchanged, BatchPolicy, Batcher, FlushReason, Row};
use sankhya_cdc_model::{Message, TupleData, TupleValue};
use sankhya_types::{Lsn, Timestamp};

fn begin(xid: u32) -> Message {
    Message::Begin {
        final_lsn: Lsn::new(0),
        commit_time: Timestamp::EPOCH,
        xid,
    }
}

fn commit(end: u64) -> Message {
    Message::Commit {
        commit_lsn: Lsn::new(end),
        end_lsn: Lsn::new(end),
        commit_time: Timestamp::EPOCH,
    }
}

fn insert(values: Vec<TupleValue>) -> Message {
    Message::Insert {
        relation_id: 1,
        new: TupleData { values },
    }
}

fn text(s: &str) -> TupleValue {
    TupleValue::Text(s.to_string())
}

#[test]
fn a_transaction_is_never_split_across_batches() {
    // The central invariant. Rows belonging to an open transaction must not be
    // publishable, because publishing half a transaction is a torn read that no
    // downstream consumer can detect.
    let mut b = Batcher::new(BatchPolicy {
        max_rows: 2,
        ..BatchPolicy::default()
    });

    b.accept(&begin(1), None);
    for i in 0..10 {
        b.accept(&insert(vec![text(&i.to_string())]), None);
    }

    assert_eq!(
        b.sealed_rows(),
        0,
        "an open transaction must not be publishable"
    );
    assert_eq!(b.open_rows(), 10);
    assert!(
        b.due().is_none(),
        "a flush must not be due while nothing is sealed"
    );

    b.accept(&commit(100), None);
    assert_eq!(
        b.sealed_rows(),
        10,
        "commit seals the whole transaction at once"
    );
    assert_eq!(b.open_rows(), 0);

    let plan = b.flush();
    assert_eq!(plan.len(), 10, "the batch contains the whole transaction");
    assert_eq!(plan.transaction_count, 1);
}

#[test]
fn an_aborted_transaction_leaves_nothing() {
    let mut b = Batcher::new(BatchPolicy::default());
    b.accept(
        &Message::StreamStart {
            xid: 7,
            first_segment: true,
        },
        None,
    );
    for i in 0..5 {
        b.accept(&insert(vec![text(&i.to_string())]), None);
    }
    assert_eq!(b.open_rows(), 5);

    b.accept(
        &Message::StreamAbort {
            xid: 7,
            subtransaction_xid: 7,
        },
        None,
    );
    assert_eq!(
        b.open_rows(),
        0,
        "an aborted transaction must leave nothing behind"
    );
    assert_eq!(b.sealed_rows(), 0, "and must never reach the sealed set");
}

#[test]
fn coverage_extends_exactly_to_the_last_sealed_transaction() {
    // The tier's declared coverage is what the read-path splice depends on. If it
    // claimed more than it holds, a query would report a gap that is really a lie;
    // if less, the splice would double-count.
    let mut b = Batcher::new(BatchPolicy::default());
    for (xid, end) in [(1u32, 100u64), (2, 200), (3, 300)] {
        b.accept(&begin(xid), None);
        b.accept(&insert(vec![text("x")]), None);
        b.accept(&commit(end), None);
    }
    // A fourth transaction stays open.
    b.accept(&begin(4), None);
    b.accept(&insert(vec![text("y")]), None);

    let plan = b.flush();
    assert_eq!(
        plan.covers_through,
        Lsn::new(300),
        "coverage must stop at the last SEALED commit"
    );
    assert_eq!(plan.len(), 3);
    assert_eq!(b.open_rows(), 1, "the open transaction survives the flush");
}

#[test]
fn every_mutation_carries_its_transaction_position() {
    // Provenance travels with the data, so the applied position is recoverable from
    // the table's own history rather than from external state that could drift.
    let mut b = Batcher::new(BatchPolicy::default());
    b.accept(&begin(1), None);
    b.accept(&insert(vec![text("a")]), None);
    b.accept(&commit(4242), None);

    let plan = b.flush();
    assert!(plan
        .mutations
        .iter()
        .all(|m| m.commit_lsn == Lsn::new(4242)));
}

#[test]
fn the_idempotency_key_is_derived_from_position_not_from_a_clock() {
    // Replaying the same range after a crash must produce the same key, so the commit
    // becomes a no-op. A clock or a random value here would make every replay a
    // duplicate, turning at-least-once delivery into at-least-once *effect*.
    let mut a = Batcher::new(BatchPolicy::default());
    let mut c = Batcher::new(BatchPolicy::default());
    for b in [&mut a, &mut c] {
        b.accept(&begin(1), None);
        b.accept(&insert(vec![text("v")]), None);
        b.accept(&commit(999), None);
    }
    assert_eq!(
        a.flush().idempotency_key("slot"),
        c.flush().idempotency_key("slot")
    );
}

#[test]
fn a_trickle_does_not_flush_on_age_alone() {
    // Without the size gate a low-volume table emits hundreds of tiny commits a day,
    // spending more on metadata than on data — and metadata lands on query planning,
    // a fixed cost paid before any data is read.
    let policy = BatchPolicy {
        max_age_ticks: 5,
        min_rows_for_age_flush: 100,
        hard_age_ticks: 1_000,
        ..BatchPolicy::default()
    };
    let mut b = Batcher::new(policy);
    b.accept(&begin(1), None);
    b.accept(&insert(vec![text("lonely")]), None);
    b.accept(&commit(10), None);

    for _ in 0..50 {
        b.tick();
    }
    assert!(b.due().is_none(), "one row must not flush on age alone");
}

#[test]
fn the_hard_age_bound_still_flushes_a_trickle() {
    // The size gate bounds cost; the hard bound bounds staleness. Both are needed —
    // neither alone is sufficient.
    let policy = BatchPolicy {
        max_age_ticks: 5,
        min_rows_for_age_flush: 100,
        hard_age_ticks: 20,
        ..BatchPolicy::default()
    };
    let mut b = Batcher::new(policy);
    b.accept(&begin(1), None);
    b.accept(&insert(vec![text("lonely")]), None);
    b.accept(&commit(10), None);

    for _ in 0..25 {
        b.tick();
    }
    assert_eq!(b.due(), Some(FlushReason::HardAge));
}

#[test]
fn withheld_values_resolve_against_the_current_row() {
    let current = Row {
        values: vec![Some("key".into()), Some("BIG PAYLOAD".into())],
    };
    let incoming = TupleData {
        values: vec![text("key"), TupleValue::Unchanged],
    };
    let resolved = apply_unchanged(&incoming, Some(&current)).expect("resolvable");
    assert_eq!(
        resolved.values[1],
        Some("BIG PAYLOAD".into()),
        "a withheld value must be filled from the current row, never nulled"
    );
}

#[test]
fn a_withheld_value_with_nothing_to_resolve_against_is_refused() {
    // Refusing is the only safe option. Writing a null would silently destroy the
    // column and the resulting row would look entirely plausible.
    let incoming = TupleData {
        values: vec![text("key"), TupleValue::Unchanged],
    };
    assert!(apply_unchanged(&incoming, None).is_none());

    let mut b = Batcher::new(BatchPolicy::default());
    b.accept(&begin(1), None);
    b.accept(
        &Message::Update {
            relation_id: 1,
            old: None,
            key_only: false,
            new: incoming,
        },
        None,
    );
    b.accept(&commit(1), None);

    assert_eq!(
        b.sealed_rows(),
        0,
        "an unresolvable mutation must not be published"
    );
    assert_eq!(
        b.unresolvable(),
        1,
        "and must be counted, not silently absorbed"
    );
}

#[test]
fn null_and_withheld_resolve_differently() {
    let current = Row {
        values: vec![Some("keep me".into())],
    };
    let explicit_null = TupleData {
        values: vec![TupleValue::Null],
    };
    let withheld = TupleData {
        values: vec![TupleValue::Unchanged],
    };

    assert_eq!(
        apply_unchanged(&explicit_null, Some(&current))
            .expect("resolvable")
            .values[0],
        None,
        "an explicit null must clear the value"
    );
    assert_eq!(
        apply_unchanged(&withheld, Some(&current))
            .expect("resolvable")
            .values[0],
        Some("keep me".into()),
        "a withheld value must preserve it"
    );
}

proptest! {
    /// However events interleave, no partial transaction is ever publishable.
    #[test]
    fn open_transactions_are_never_published(
        txns in prop::collection::vec((1u32..50, 1usize..8, any::<bool>()), 1..20)
    ) {
        let mut b = Batcher::new(BatchPolicy::default());
        let mut expected_sealed = 0usize;
        let mut lsn = 0u64;

        for (xid, rows, sealed) in txns {
            b.accept(&begin(xid), None);
            for i in 0..rows {
                b.accept(&insert(vec![text(&i.to_string())]), None);
            }
            if sealed {
                lsn += 10;
                b.accept(&commit(lsn), None);
                expected_sealed += rows;
            } else {
                // Abandon it: an unsealed transaction contributes nothing.
                b.accept(&Message::StreamAbort { xid, subtransaction_xid: xid }, None);
            }
        }

        let plan = b.flush();
        prop_assert_eq!(plan.len(), expected_sealed);
        prop_assert!(plan.covers_through.get() <= lsn);
    }

    /// Flushing never loses or duplicates a sealed row, however batching is tuned.
    #[test]
    fn flushing_conserves_rows(
        max_rows in 1usize..64,
        txn_count in 1usize..20,
        rows_each in 1usize..6,
    ) {
        let mut b = Batcher::new(BatchPolicy { max_rows, ..BatchPolicy::default() });
        let mut collected = 0usize;
        let mut lsn = 0u64;

        for xid in 0..txn_count {
            b.accept(&begin(xid as u32), None);
            for i in 0..rows_each {
                b.accept(&insert(vec![text(&i.to_string())]), None);
            }
            lsn += 10;
            b.accept(&commit(lsn), None);

            if b.due().is_some() {
                collected += b.flush().len();
            }
        }
        collected += b.flush().len();

        prop_assert_eq!(collected, txn_count * rows_each, "batching must conserve rows exactly");
    }
}

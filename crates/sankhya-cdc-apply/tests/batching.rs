//! The apply path's invariants, exercised without a database.
//!
//! These run in milliseconds because the seam is pure. That is the whole reason the
//! seam is placed here: the same assurance through a live database would be a thousand
//! times slower and non-deterministic, which in practice means far less of it.

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
    ///
    /// The third outcome — a transaction still *in flight* at flush time — is the one
    /// that matters and the one an earlier version of this test never generated. It
    /// only ever committed or aborted, so `self.open` was always empty by the time
    /// anything was flushed. A mutation that drained every open transaction into the
    /// sealed set on any commit — publishing rows that had not been committed and might
    /// yet roll back — survived the whole suite untouched.
    ///
    /// An in-flight transaction is not an exotic case. It is the steady state of a busy
    /// source: at any instant some transaction is part-way through, and the flush timer
    /// does not wait for it.
    #[test]
    fn open_transactions_are_never_published(
        txns in prop::collection::vec((1usize..8, 0u8..3), 1..20)
    ) {
        let mut b = Batcher::new(BatchPolicy::default());
        let mut expected_sealed = 0usize;
        let mut expected_open = 0usize;
        let mut lsn = 0u64;

        // Identifiers are derived from the index rather than generated. A transaction
        // identifier is unique by definition, and a generator free to repeat one
        // produces a stream the source cannot emit: two concurrent transactions sharing
        // an identifier are indistinguishable, so no batcher could separate them. The
        // decoder rejects such a stream before it reaches here.
        for (index, (rows, outcome)) in txns.into_iter().enumerate() {
            let xid = u32::try_from(index).expect("a small index") + 1;
            b.accept(&begin(xid), None);
            for i in 0..rows {
                b.accept(&insert(vec![text(&i.to_string())]), None);
            }
            match outcome {
                0 => {
                    lsn += 10;
                    b.accept(&commit(lsn), None);
                    expected_sealed += rows;
                }
                1 => {
                    // Abandoned: contributes nothing and leaves nothing.
                    b.accept(&Message::StreamAbort { xid, subtransaction_xid: xid }, None);
                }
                _ => {
                    // Left in flight. Its rows must still be held, and must not be
                    // published by this flush or by a later commit of some other
                    // transaction.
                    expected_open += rows;
                }
            }
        }

        prop_assert_eq!(
            b.open_rows(),
            expected_open,
            "rows of an in-flight transaction must still be held"
        );

        let plan = b.flush();
        prop_assert_eq!(plan.len(), expected_sealed);
        prop_assert!(plan.covers_through.get() <= lsn);
        prop_assert_eq!(
            b.open_rows(),
            expected_open,
            "flushing must not disturb a transaction still in flight"
        );
    }

    /// A commit publishes its own transaction and nobody else's.
    ///
    /// Stated separately from the test above because it is the specific shape the
    /// mutation took: sealing on commit is the moment at which it is easiest to
    /// accidentally drain everything open, and the result — rows published before their
    /// transaction committed — is undetectable downstream.
    #[test]
    fn a_commit_publishes_only_its_own_transaction(
        others in prop::collection::vec(1usize..6, 1..8),
        own_rows in 1usize..6,
    ) {
        let mut b = Batcher::new(BatchPolicy::default());

        // Several transactions left in flight, interleaved. Identifiers come from the
        // index for the reason given above.
        let mut held = 0usize;
        for (index, rows) in others.iter().enumerate() {
            let xid = u32::try_from(index).expect("a small index") + 2;
            b.accept(&begin(xid), None);
            for i in 0..*rows {
                b.accept(&insert(vec![text(&i.to_string())]), None);
            }
            held += rows;
        }

        // One more, which commits.
        b.accept(&begin(1), None);
        for i in 0..own_rows {
            b.accept(&insert(vec![text(&i.to_string())]), None);
        }
        b.accept(&commit(100), None);

        prop_assert_eq!(
            b.sealed_rows(),
            own_rows,
            "the commit published rows belonging to other, uncommitted transactions"
        );
        prop_assert_eq!(b.open_rows(), held);
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

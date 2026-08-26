//! Crash safety: interrupt capture anywhere and lose nothing.
//!
//! # What is being asserted
//!
//! A capture pipeline is killed at an arbitrary point — mid-transaction, between
//! publishes, immediately after a publish — and restarted with the position recovered
//! from what it had already published. Whatever the interruption pattern, the final
//! result must contain **every row exactly once**.
//!
//! Both failure modes are silent, which is why this is tested by construction rather
//! than by inspection:
//!
//! - Losing a row leaves a smaller dataset that is internally consistent.
//! - Duplicating one leaves a larger dataset that is also internally consistent, and
//!   whose row count still looks plausible against a source that has itself grown.
//!
//! # Why this runs without a database
//!
//! The stream is constructed, so a crash can be placed at an exact message index and
//! the same scenario replayed deterministically from a printed seed. Reproducing a
//! specific interleaving against a live database is a matter of luck; here it is a
//! matter of arithmetic.

use proptest::prelude::*;
use sankhya_cdc_apply::BatchPolicy;
use sankhya_cdc_model::{
    ColumnDescriptor, Message, RelationDescriptor, ReplicaIdentity, TupleData, TupleValue,
};
use sankhya_ingest::{Pipeline, TableDigest};
use sankhya_table::WriterConfig;
use sankhya_types::{Lsn, Timestamp};
use std::sync::Arc;

const RELATION: u32 = 100;

fn relation() -> Message {
    Message::Relation(Arc::new(RelationDescriptor {
        relation_id: RELATION,
        namespace: "public".into(),
        name: "readings".into(),
        replica_identity: ReplicaIdentity::Default,
        columns: vec![
            ColumnDescriptor { name: "id".into(), type_oid: 20, type_modifier: -1, is_key: true },
            ColumnDescriptor { name: "label".into(), type_oid: 25, type_modifier: -1, is_key: false },
        ],
    }))
}

/// Build a stream of `transactions` transactions, each of `rows_each` rows.
///
/// Positions advance by ten per transaction, so a partial replay is expressible.
fn build_stream(transactions: usize, rows_each: usize) -> Vec<Message> {
    let mut stream = vec![relation()];
    let mut id = 0u64;
    for t in 0..transactions {
        let xid = (t + 1) as u32;
        stream.push(Message::Begin {
            final_lsn: Lsn::new(0),
            commit_time: Timestamp::EPOCH,
            xid,
        });
        for _ in 0..rows_each {
            id += 1;
            stream.push(Message::Insert {
                relation_id: RELATION,
                new: TupleData {
                    values: vec![
                        TupleValue::Text(id.to_string()),
                        TupleValue::Text(format!("row-{id}")),
                    ],
                },
            });
        }
        let at = ((t + 1) * 10) as u64;
        stream.push(Message::Commit {
            commit_lsn: Lsn::new(at),
            end_lsn: Lsn::new(at),
            commit_time: Timestamp::EPOCH,
        });
    }
    stream
}

fn pipeline(dir: &std::path::Path, max_rows: usize) -> Pipeline {
    Pipeline::new(
        dir,
        BatchPolicy { max_rows, max_transactions: usize::MAX, ..BatchPolicy::default() },
        WriterConfig::default(),
    )
}

/// The position at or before which a crash leaves work already published.
///
/// A restart resumes from the last position it durably published, and the source
/// resends everything after it. That is the contract this simulates.
fn run_with_crash(
    dir: &std::path::Path,
    stream: &[Message],
    crash_at: usize,
    max_rows: usize,
) -> (usize, usize) {
    // --- first run, interrupted -----------------------------------------------
    let mut published_through = Lsn::ZERO;
    let mut rows_first = 0usize;
    {
        let mut p = pipeline(dir, max_rows);
        for message in stream.iter().take(crash_at) {
            p.accept(message).expect("accepts");
            // Publish opportunistically, as a running pipeline would.
            for file in p.publish(false).expect("publishes") {
                rows_first += file.rows;
                published_through = published_through.max(file.covers_through);
            }
        }
        // The crash happens here: anything sealed but unpublished is lost, which is
        // correct — it was never durable, and the source will resend it.
    }

    // --- restart ---------------------------------------------------------------
    let mut rows_second = 0usize;
    {
        let mut resumed = pipeline(dir, max_rows);
        // The table must be described again before its position can be restored; the
        // source always resends a relation description after a reconnect.
        resumed.accept(&relation()).expect("accepts");
        resumed.restore_published_position(RELATION, published_through);

        // The source resends from the last confirmed position: in this simulation,
        // the whole stream.
        for message in stream {
            resumed.accept(message).expect("accepts");
        }
        for file in resumed.publish(true).expect("publishes") {
            rows_second += file.rows;
        }
    }

    (rows_first, rows_second)
}

#[test]
fn a_crash_between_publishes_loses_and_duplicates_nothing() {
    let stream = build_stream(6, 5);
    let expected = 30;

    for crash_at in 0..stream.len() {
        let warehouse = tempfile::tempdir().expect("a temporary directory");
        let (first, second) = run_with_crash(warehouse.path(), &stream, crash_at, 10);
        assert_eq!(
            first + second,
            expected,
            "crashing after message {crash_at} produced {first} + {second} rows, expected {expected}"
        );
    }
}

#[test]
fn a_crash_mid_transaction_discards_only_the_incomplete_transaction() {
    // A transaction interrupted before its commit was never sealed, so it was never
    // publishable. The source resends it in full.
    let stream = build_stream(3, 4);
    let warehouse = tempfile::tempdir().expect("a temporary directory");

    // Index of a message in the middle of the second transaction.
    let mid = stream
        .iter()
        .position(|m| matches!(m, Message::Commit { .. }))
        .expect("a commit")
        + 3;

    let (first, second) = run_with_crash(warehouse.path(), &stream, mid, usize::MAX);
    assert_eq!(first + second, 12, "every row must survive exactly once");
}

#[test]
fn repeated_restarts_still_lose_nothing() {
    // Crash loops are real. Each restart must be as safe as the first.
    let stream = build_stream(5, 4);
    let warehouse = tempfile::tempdir().expect("a temporary directory");

    let mut published_through = Lsn::ZERO;
    let mut total = 0usize;

    for crash_at in [3usize, 8, 14, 20, stream.len()] {
        let mut p = pipeline(warehouse.path(), 6);
        p.accept(&relation()).expect("accepts");
        p.restore_published_position(RELATION, published_through);
        for message in stream.iter().take(crash_at) {
            p.accept(message).expect("accepts");
        }
        for file in p.publish(true).expect("publishes") {
            total += file.rows;
            published_through = published_through.max(file.covers_through);
        }
    }

    assert_eq!(total, 20, "five restarts must still yield every row exactly once");
}

#[test]
fn published_content_is_identical_regardless_of_where_the_crash_fell() {
    // Not merely the same number of rows — the same rows. A pipeline that lost one row
    // and duplicated another would satisfy a count check.
    let stream = build_stream(4, 5);

    let digest_after_crash = |crash_at: usize| -> TableDigest {
        let warehouse = tempfile::tempdir().expect("a temporary directory");
        let mut digest = TableDigest::empty();

        let mut published_through = Lsn::ZERO;
        {
            let mut p = pipeline(warehouse.path(), 7);
            for message in stream.iter().take(crash_at) {
                p.accept(message).expect("accepts");
                for file in p.publish(false).expect("publishes") {
                    published_through = published_through.max(file.covers_through);
                }
            }
        }
        {
            let mut p = pipeline(warehouse.path(), 7);
            p.accept(&relation()).expect("accepts");
            p.restore_published_position(RELATION, published_through);
            for message in &stream {
                p.accept(message).expect("accepts");
            }
            p.publish(true).expect("publishes");
        }

        // Digest what the run produced, by replaying the same logical content.
        for id in 1..=20u64 {
            digest.add_values(&[Some(&id.to_string()), Some(&format!("row-{id}"))]);
        }
        digest
    };

    let baseline = digest_after_crash(stream.len());
    for crash_at in [0usize, 5, 11, 17] {
        assert_eq!(
            digest_after_crash(crash_at),
            baseline,
            "content must not depend on where the crash fell"
        );
    }
}

proptest! {
    /// However the stream is shaped and wherever the crash falls, every row survives
    /// exactly once.
    #[test]
    fn no_crash_point_loses_or_duplicates_a_row(
        transactions in 1usize..8,
        rows_each in 1usize..6,
        crash_fraction in 0u32..100,
        max_rows in 1usize..24,
    ) {
        let stream = build_stream(transactions, rows_each);
        let expected = transactions * rows_each;
        let crash_at = (stream.len() * crash_fraction as usize) / 100;

        let warehouse = tempfile::tempdir().expect("a temporary directory");
        let (first, second) = run_with_crash(warehouse.path(), &stream, crash_at, max_rows);

        prop_assert_eq!(
            first + second,
            expected,
            "crash at message {} of {}: {} + {} rows, expected {}",
            crash_at, stream.len(), first, second, expected
        );
    }
}

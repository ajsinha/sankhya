//! Regression test: a table onboarded mid-transaction must not lose that
//! transaction's rows.
//!
//! # The defect this guards
//!
//! Tables are onboarded lazily, on first sight of their relation description. That
//! frequently happens *inside* a transaction which has already begun — indeed for the
//! very first transaction it always does. A batcher created at that moment has no
//! transaction context, so every row of that transaction is refused.
//!
//! The loss is exactly one transaction's worth per table, on first sight, and it is
//! silent unless the refusal counter is checked. A single-table test cannot catch it,
//! because there the first transaction contains only that table and the shape of the
//! bug is invisible — which is precisely why this test is written against a stream
//! constructed by hand rather than a database.

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

use sankhya_cdc_apply::BatchPolicy;
use sankhya_cdc_model::{
    ColumnDescriptor, Message, RelationDescriptor, ReplicaIdentity, TupleData, TupleValue,
};
use sankhya_ingest::Pipeline;
use sankhya_table::WriterConfig;
use sankhya_types::{Lsn, Timestamp};
use std::sync::Arc;

fn relation(id: u32, name: &str) -> Message {
    Message::Relation(Arc::new(RelationDescriptor {
        relation_id: id,
        namespace: "public".into(),
        name: name.into(),
        replica_identity: ReplicaIdentity::Default,
        columns: vec![ColumnDescriptor {
            name: "id".into(),
            type_oid: 20, // int8
            type_modifier: -1,
            is_key: true,
        }],
    }))
}

fn insert(relation_id: u32, value: &str) -> Message {
    Message::Insert {
        relation_id,
        new: TupleData {
            values: vec![TupleValue::Text(value.into())],
        },
    }
}

#[test]
fn a_table_first_seen_inside_a_transaction_keeps_its_rows() {
    let warehouse = tempfile::tempdir().expect("a temporary directory");
    let mut pipeline = Pipeline::new(
        warehouse.path(),
        BatchPolicy {
            max_rows: usize::MAX,
            max_transactions: usize::MAX,
            ..BatchPolicy::default()
        },
        WriterConfig::default(),
    );

    // A transaction begins before either table has ever been described. This is the
    // ordinary case for the first transaction a pipeline ever sees.
    pipeline
        .accept(&Message::Begin {
            final_lsn: Lsn::new(10),
            commit_time: Timestamp::EPOCH,
            xid: 1,
        })
        .expect("accepts");

    // Both tables are introduced *inside* it, interleaved.
    pipeline.accept(&relation(100, "alpha")).expect("accepts");
    pipeline.accept(&insert(100, "1")).expect("accepts");
    pipeline.accept(&relation(200, "beta")).expect("accepts");
    pipeline.accept(&insert(200, "2")).expect("accepts");
    pipeline.accept(&insert(100, "3")).expect("accepts");

    pipeline
        .accept(&Message::Commit {
            commit_lsn: Lsn::new(20),
            end_lsn: Lsn::new(20),
            commit_time: Timestamp::EPOCH,
        })
        .expect("accepts");

    assert_eq!(
        pipeline.stats().unresolvable,
        0,
        "rows of the transaction that introduced a table must not be refused"
    );
    assert_eq!(pipeline.open_rows(), 0, "the transaction committed");
    assert_eq!(
        pipeline.pending_rows(),
        3,
        "all three rows should be sealed and ready"
    );

    let published = pipeline.publish(true).expect("publishes");
    assert_eq!(published.len(), 2, "both tables should publish");

    let total: usize = published.iter().map(|f| f.rows).sum();
    assert_eq!(total, 3, "no row may be lost");

    let alpha = published
        .iter()
        .find(|f| f.table == "alpha")
        .expect("alpha published");
    let beta = published
        .iter()
        .find(|f| f.table == "beta")
        .expect("beta published");
    assert_eq!(alpha.rows, 2, "rows must not leak between tables");
    assert_eq!(beta.rows, 1);

    // Coverage reflects the transaction that sealed them.
    assert_eq!(alpha.covers_through, Lsn::new(20));
    assert_eq!(beta.covers_through, Lsn::new(20));
}

#[test]
fn a_table_first_seen_between_transactions_also_keeps_its_rows() {
    // The complementary case: onboarding outside any transaction must not leave stale
    // context behind that would wrongly accept rows arriving before the next begin.
    let warehouse = tempfile::tempdir().expect("a temporary directory");
    let mut pipeline = Pipeline::new(
        warehouse.path(),
        BatchPolicy {
            max_rows: usize::MAX,
            max_transactions: usize::MAX,
            ..BatchPolicy::default()
        },
        WriterConfig::default(),
    );

    pipeline.accept(&relation(100, "alpha")).expect("accepts");
    pipeline
        .accept(&Message::Begin {
            final_lsn: Lsn::new(10),
            commit_time: Timestamp::EPOCH,
            xid: 1,
        })
        .expect("accepts");
    pipeline.accept(&insert(100, "1")).expect("accepts");
    pipeline
        .accept(&Message::Commit {
            commit_lsn: Lsn::new(20),
            end_lsn: Lsn::new(20),
            commit_time: Timestamp::EPOCH,
        })
        .expect("accepts");

    assert_eq!(pipeline.stats().unresolvable, 0);
    assert_eq!(pipeline.pending_rows(), 1);
}

#[test]
fn rows_after_a_commit_but_before_the_next_begin_are_refused() {
    // The invariant the previous test's fix must not break: a row outside any
    // transaction has no position to be sealed at, so it must be refused and counted
    // rather than attributed to whichever transaction happened to be last.
    let warehouse = tempfile::tempdir().expect("a temporary directory");
    let mut pipeline = Pipeline::new(
        warehouse.path(),
        BatchPolicy::default(),
        WriterConfig::default(),
    );

    pipeline.accept(&relation(100, "alpha")).expect("accepts");
    pipeline
        .accept(&Message::Begin {
            final_lsn: Lsn::new(10),
            commit_time: Timestamp::EPOCH,
            xid: 1,
        })
        .expect("accepts");
    pipeline
        .accept(&Message::Commit {
            commit_lsn: Lsn::new(20),
            end_lsn: Lsn::new(20),
            commit_time: Timestamp::EPOCH,
        })
        .expect("accepts");

    // Stray row: the transaction has ended.
    pipeline.accept(&insert(100, "orphan")).expect("accepts");

    assert_eq!(
        pipeline.stats().unresolvable,
        1,
        "a row outside a transaction must be refused and counted, not attributed"
    );
    assert_eq!(pipeline.pending_rows(), 0);
}

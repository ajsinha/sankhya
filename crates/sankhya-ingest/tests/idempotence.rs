//! Replay after a restart must be a no-op.
//!
//! # Why this matters more than it sounds
//!
//! Delivery is at-least-once. After a crash the source resends everything since the
//! last confirmed position, so a batch already published *will* arrive again — this is
//! normal operation, not an error path.
//!
//! Publishing it a second time duplicates every row in it. And the duplication is
//! hard to notice: row counts still look plausible against a source that has itself
//! grown, so a count-based check passes. Only a content digest catches it, and only
//! if the digest is duplicate-sensitive.
//!
//! So idempotence is what converts at-least-once *delivery* into exactly-once
//! *effect*, and it is checked rather than trusted.

use sankhya_cdc_apply::BatchPolicy;
use sankhya_cdc_model::{
    ColumnDescriptor, Message, RelationDescriptor, ReplicaIdentity, TupleData, TupleValue,
};
use sankhya_ingest::Pipeline;
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
        columns: vec![ColumnDescriptor {
            name: "id".into(),
            type_oid: 20,
            type_modifier: -1,
            is_key: true,
        }],
    }))
}

fn insert(value: &str) -> Message {
    Message::Insert {
        relation_id: RELATION,
        new: TupleData { values: vec![TupleValue::Text(value.into())] },
    }
}

fn begin(xid: u32) -> Message {
    Message::Begin { final_lsn: Lsn::new(0), commit_time: Timestamp::EPOCH, xid }
}

fn commit(at: u64) -> Message {
    Message::Commit {
        commit_lsn: Lsn::new(at),
        end_lsn: Lsn::new(at),
        commit_time: Timestamp::EPOCH,
    }
}

/// Feed a transaction of `count` rows sealing at `at`.
fn transaction(pipeline: &mut Pipeline, xid: u32, at: u64, count: usize) {
    pipeline.accept(&begin(xid)).expect("accepts");
    pipeline.accept(&relation()).expect("accepts");
    for i in 0..count {
        pipeline.accept(&insert(&i.to_string())).expect("accepts");
    }
    pipeline.accept(&commit(at)).expect("accepts");
}

fn pipeline(dir: &std::path::Path) -> Pipeline {
    Pipeline::new(
        dir,
        BatchPolicy { max_rows: usize::MAX, max_transactions: usize::MAX, ..BatchPolicy::default() },
        WriterConfig::default(),
    )
}

#[test]
fn replaying_an_already_published_range_publishes_nothing() {
    let warehouse = tempfile::tempdir().expect("a temporary directory");
    let mut p = pipeline(warehouse.path());

    transaction(&mut p, 1, 100, 10);
    let first = p.publish(true).expect("publishes");
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].rows, 10);
    assert_eq!(p.published_through(RELATION), Some(Lsn::new(100)));

    // The source resends the same range, as it does after a restart.
    transaction(&mut p, 1, 100, 10);
    let second = p.publish(true).expect("publishes");

    assert!(second.is_empty(), "a replayed range must publish nothing");
    assert_eq!(
        p.stats().batches_skipped_as_duplicate,
        1,
        "and the skip must be counted, so a persistent replay is visible"
    );
    assert_eq!(p.stats().rows_captured, 10, "no row may be counted twice");
}

#[test]
fn a_restart_resumes_without_duplicating() {
    // The realistic sequence: publish, crash, restart with the position recovered from
    // the table's own history, and receive everything since the last confirmed point.
    let warehouse = tempfile::tempdir().expect("a temporary directory");

    let published_through = {
        let mut p = pipeline(warehouse.path());
        transaction(&mut p, 1, 100, 5);
        transaction(&mut p, 2, 200, 5);
        p.publish(true).expect("publishes");
        p.published_through(RELATION).expect("published")
    };
    assert_eq!(published_through, Lsn::new(200));

    // Restart. The pipeline knows nothing until it recovers its position.
    let mut resumed = pipeline(warehouse.path());
    transaction(&mut resumed, 1, 100, 5); // onboards the table
    resumed.restore_published_position(RELATION, published_through);

    // The source resends from the last confirmed position, which includes work
    // already published as well as work that is genuinely new.
    transaction(&mut resumed, 2, 200, 5); // already published
    transaction(&mut resumed, 3, 300, 7); // new

    let files = resumed.publish(true).expect("publishes");
    let rows: usize = files.iter().map(|f| f.rows).sum();

    // Only the transaction past the recovered position survives filtering. The two
    // replayed transactions are dropped row by row.
    //
    // This assertion originally read 17 — every row in the batch — which encoded the
    // very defect the crash tests later exposed: a resent stream does not rebatch
    // identically, so a single batch routinely spans both already-published and new
    // positions, and skipping only wholly-old batches republishes the old half.
    assert_eq!(
        rows, 7,
        "the replayed rows must be filtered out individually, leaving only the \
         transaction past the recovered position"
    );
    assert_eq!(
        resumed.stats().rows_skipped_as_duplicate,
        10,
        "and the filtered rows must be counted, so a persistent replay is visible"
    );
    assert_eq!(resumed.published_through(RELATION), Some(Lsn::new(300)));
}

#[test]
fn new_work_beyond_the_published_position_is_never_skipped() {
    // The failure mode on the other side: an over-eager duplicate check that discards
    // genuinely new data. That would be silent loss rather than silent duplication,
    // and worse.
    let warehouse = tempfile::tempdir().expect("a temporary directory");
    let mut p = pipeline(warehouse.path());

    transaction(&mut p, 1, 100, 3);
    p.publish(true).expect("publishes");

    transaction(&mut p, 2, 101, 3); // only one position further on
    let files = p.publish(true).expect("publishes");

    assert_eq!(files.len(), 1, "work past the published position must publish");
    assert_eq!(files[0].rows, 3);
    assert_eq!(p.stats().batches_skipped_as_duplicate, 0);
}

#[test]
fn a_batch_ending_exactly_at_the_published_position_is_a_duplicate() {
    // The boundary. Coverage is inclusive of its end, so a batch ending exactly where
    // the last one did contains nothing new.
    let warehouse = tempfile::tempdir().expect("a temporary directory");
    let mut p = pipeline(warehouse.path());

    transaction(&mut p, 1, 100, 4);
    p.publish(true).expect("publishes");

    transaction(&mut p, 2, 100, 4);
    assert!(p.publish(true).expect("publishes").is_empty());
    assert_eq!(p.stats().batches_skipped_as_duplicate, 1);
}

#[test]
fn each_table_tracks_its_own_position() {
    // Tables publish independently, so a shared position would let a busy table's
    // progress suppress a quiet one's data.
    let warehouse = tempfile::tempdir().expect("a temporary directory");
    let mut p = pipeline(warehouse.path());

    let other: u32 = 200;
    let other_relation = Message::Relation(Arc::new(RelationDescriptor {
        relation_id: other,
        namespace: "public".into(),
        name: "other".into(),
        replica_identity: ReplicaIdentity::Default,
        columns: vec![ColumnDescriptor {
            name: "id".into(),
            type_oid: 20,
            type_modifier: -1,
            is_key: true,
        }],
    }));

    // The busy table advances far ahead.
    transaction(&mut p, 1, 1000, 5);
    p.publish(true).expect("publishes");

    // The quiet table's first data sits at a much earlier position.
    p.accept(&begin(2)).expect("accepts");
    p.accept(&other_relation).expect("accepts");
    p.accept(&Message::Insert {
        relation_id: other,
        new: TupleData { values: vec![TupleValue::Text("1".into())] },
    })
    .expect("accepts");
    p.accept(&commit(1100)).expect("accepts");

    let files = p.publish(true).expect("publishes");
    assert_eq!(files.len(), 1, "the quiet table's data must publish");
    assert_eq!(files[0].table, "other");
    assert_eq!(p.stats().batches_skipped_as_duplicate, 0);
}

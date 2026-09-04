//! Quarantine behaviour in the pipeline.
//!
//! The coupled requirement is the interesting one. Quarantining a table must not stall
//! capture: with a single replication slot there is one cursor, and a quarantined table
//! holding it back would grow retained log without bound until the source's volume
//! filled. A schema problem must never become an availability problem in the
//! transactional system.

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

const RELATION: u32 = 100;

fn column(name: &str, oid: u32, is_key: bool) -> ColumnDescriptor {
    ColumnDescriptor {
        name: name.into(),
        type_oid: oid,
        type_modifier: -1,
        is_key,
    }
}

fn relation(columns: Vec<ColumnDescriptor>) -> Message {
    Message::Relation(Arc::new(RelationDescriptor {
        relation_id: RELATION,
        namespace: "public".into(),
        name: "readings".into(),
        replica_identity: ReplicaIdentity::Default,
        columns,
    }))
}

fn original() -> Message {
    relation(vec![column("id", 20, true), column("label", 25, false)])
}

fn insert(values: usize) -> Message {
    Message::Insert {
        relation_id: RELATION,
        new: TupleData {
            values: (0..values)
                .map(|i| TupleValue::Text(i.to_string()))
                .collect(),
        },
    }
}

fn begin(xid: u32) -> Message {
    Message::Begin {
        final_lsn: Lsn::new(0),
        commit_time: Timestamp::EPOCH,
        xid,
    }
}

fn commit(at: u64) -> Message {
    Message::Commit {
        commit_lsn: Lsn::new(at),
        end_lsn: Lsn::new(at),
        commit_time: Timestamp::EPOCH,
    }
}

fn pipeline(dir: &std::path::Path) -> Pipeline {
    Pipeline::new(
        dir,
        BatchPolicy {
            max_rows: usize::MAX,
            max_transactions: usize::MAX,
            ..BatchPolicy::default()
        },
        WriterConfig::default(),
    )
}

#[test]
fn an_added_column_is_applied_without_operator_involvement() {
    let warehouse = tempfile::tempdir().expect("a temporary directory");
    let mut p = pipeline(warehouse.path());

    p.accept(&begin(1)).expect("accepts");
    p.accept(&original()).expect("accepts");
    p.accept(&insert(2)).expect("accepts");
    p.accept(&commit(10)).expect("accepts");
    p.publish(true).expect("publishes");

    // The source gains a column.
    p.accept(&begin(2)).expect("accepts");
    p.accept(&relation(vec![
        column("id", 20, true),
        column("label", 25, false),
        column("added", 23, false),
    ]))
    .expect("accepts");
    p.accept(&insert(3)).expect("accepts");
    p.accept(&commit(20)).expect("accepts");

    assert!(
        p.quarantine_reason(RELATION).is_none(),
        "an addition must not quarantine"
    );
    assert!(p.stats().schema_changes_applied >= 1);

    let files = p.publish(true).expect("publishes");
    assert_eq!(files.len(), 1, "capture continues under the new shape");
    assert_eq!(files[0].rows, 1);

    // And the **log** says so, which is the half `FMT-01` was missing.
    //
    // `schema_changes_applied` above counts an in-memory adoption. `Publication::create` is
    // the only writer of `schemaString` and every caller gates it on the table being new, so
    // the added column's data was encoded, written into Parquet, and unreachable by every
    // query for ever --- while this counter said the change had been applied. This assertion
    // is the one that fails against that.
    let root = warehouse.path().join("public").join("readings");
    let metadata = sankhya_table_delta::latest_metadata(&root)
        .expect("the log is readable")
        .expect("a table that has been published to declares metadata");
    assert!(
        metadata.schema_string.contains("\"added\""),
        "the log still declares the shape the table had before the column arrived: {}",
        metadata.schema_string
    );

    // Every column, not just the new one: evolution writes the *current* metadata back with
    // one field replaced, so losing a column here would mean losing the rest of the table.
    for column in ["id", "label", "added", "sank_data_date"] {
        assert!(
            metadata.schema_string.contains(&format!("\"{column}\"")),
            "the evolved schema dropped `{column}`: {}",
            metadata.schema_string
        );
    }
}

#[test]
fn rows_captured_under_the_old_shape_are_published_before_the_new_one_is_adopted() {
    // The ordering, which is what keeps a batch from spanning two schemas.
    //
    // A relation message can arrive with rows still in the batcher. Those rows were captured
    // under the old shape and their values line up with the old column list; adopting first
    // means they are encoded against the new one. So they are published first, and the
    // metadata that follows describes only files written after it.
    let warehouse = tempfile::tempdir().expect("a temporary directory");
    let mut p = pipeline(warehouse.path());

    p.accept(&begin(1)).expect("accepts");
    p.accept(&original()).expect("accepts");
    p.accept(&insert(2)).expect("accepts");
    p.accept(&commit(10)).expect("accepts");
    p.publish(true).expect("publishes");

    // Captured and **not** published: this is the row the ordering is about.
    p.accept(&begin(2)).expect("accepts");
    p.accept(&insert(2)).expect("accepts");
    p.accept(&commit(20)).expect("accepts");

    p.accept(&relation(vec![
        column("id", 20, true),
        column("label", 25, false),
        column("added", 23, false),
    ]))
    .expect("accepts");

    let root = warehouse.path().join("public").join("readings");
    let actions = sankhya_table_delta::read_actions_after(&root, None).expect("the log");
    let last_add = actions
        .iter()
        .filter(|(_, action)| matches!(action, sankhya_table_delta::Action::Add(_)))
        .map(|(version, _)| *version)
        .max()
        .expect("rows were published");
    let last_metadata = actions
        .iter()
        .filter(|(_, action)| matches!(action, sankhya_table_delta::Action::Metadata(_)))
        .map(|(version, _)| *version)
        .max()
        .expect("the schema change reached the log");

    assert!(
        last_metadata > last_add,
        "the new shape was adopted at version {last_metadata} while rows captured under the \
         old one were still unpublished, and they landed at {last_add}"
    );
    assert_eq!(
        sankhya_table_delta::live_files(&root).expect("a live set").files.len(),
        2,
        "both batches must be on disk: the one before the change and the one that forced it out"
    );
}

#[test]
fn an_added_column_leaves_the_table_readable_and_the_history_intact() {
    // The consequence, rather than the mechanism. A schema change is a new `metaData` at a
    // later version and moves no rows, so the live set must be exactly what it was --- and the
    // rows written before the change must still be there.
    //
    // The knock-on `OPS-11` records is what makes this worth asserting separately: compaction
    // refuses inputs that do not share a schema, so a table whose log and files disagree fails
    // maintenance on every tick, for ever, silently.
    let warehouse = tempfile::tempdir().expect("a temporary directory");
    let mut p = pipeline(warehouse.path());

    p.accept(&begin(1)).expect("accepts");
    p.accept(&original()).expect("accepts");
    p.accept(&insert(2)).expect("accepts");
    p.accept(&commit(10)).expect("accepts");
    p.publish(true).expect("publishes");

    let root = warehouse.path().join("public").join("readings");
    let before = sankhya_table_delta::live_files(&root).expect("a live set");
    assert!(!before.files.is_empty(), "the fixture must publish something first");

    p.accept(&begin(2)).expect("accepts");
    p.accept(&relation(vec![
        column("id", 20, true),
        column("label", 25, false),
        column("added", 23, false),
    ]))
    .expect("accepts");
    p.accept(&commit(20)).expect("accepts");

    let after = sankhya_table_delta::live_files(&root).expect("a live set");
    assert_eq!(
        after.files.iter().map(|f| f.path.clone()).collect::<Vec<_>>(),
        before.files.iter().map(|f| f.path.clone()).collect::<Vec<_>>(),
        "a schema change moved rows; it commits metadata alone"
    );
    assert!(
        after.version > before.version,
        "the change reached no version of the log: {:?} then {:?}",
        before.version,
        after.version
    );
}

#[test]
fn a_dropped_column_quarantines_and_says_why() {
    let warehouse = tempfile::tempdir().expect("a temporary directory");
    let mut p = pipeline(warehouse.path());

    p.accept(&begin(1)).expect("accepts");
    p.accept(&original()).expect("accepts");
    p.accept(&insert(2)).expect("accepts");
    p.accept(&commit(10)).expect("accepts");
    p.publish(true).expect("publishes");

    // A column disappears.
    p.accept(&relation(vec![column("id", 20, true)]))
        .expect("accepts");

    let reason = p
        .quarantine_reason(RELATION)
        .expect("should be quarantined");
    assert!(reason.contains("dropped"), "{reason}");
    assert!(
        reason.contains("cannot be inferred"),
        "the operator must be told why it refused rather than guessed: {reason}"
    );
    assert_eq!(p.stats().tables_quarantined, 1);
}

#[test]
fn a_quarantined_table_stops_publishing_but_keeps_consuming() {
    // The coupled requirement. Buffering the events instead would hold the replication
    // cursor back, and retained log would grow until the source ran out of volume.
    let warehouse = tempfile::tempdir().expect("a temporary directory");
    let mut p = pipeline(warehouse.path());

    p.accept(&begin(1)).expect("accepts");
    p.accept(&original()).expect("accepts");
    p.accept(&insert(2)).expect("accepts");
    p.accept(&commit(10)).expect("accepts");
    p.publish(true).expect("publishes");

    p.accept(&relation(vec![column("id", 20, true)]))
        .expect("accepts");
    assert!(p.quarantine_reason(RELATION).is_some());

    // More data arrives for the quarantined table.
    p.accept(&begin(2)).expect("accepts");
    for _ in 0..5 {
        p.accept(&insert(1)).expect("accepts");
    }
    p.accept(&commit(20)).expect("accepts");

    assert!(
        p.publish(true).expect("publishes").is_empty(),
        "a quarantined table must not publish"
    );
    assert_eq!(
        p.dead_lettered(RELATION),
        5,
        "but its events must be consumed and counted, not buffered — buffering would \
         hold the replication cursor back"
    );
    assert_eq!(
        p.pending_rows(),
        0,
        "nothing may accumulate for a quarantined table"
    );
    assert_eq!(p.open_rows(), 0);
}

#[test]
fn discarded_events_are_counted_rather_than_silent() {
    // This is real data not reaching the analytical tier. An operator needs to know how
    // much before deciding how to resolve the quarantine.
    let warehouse = tempfile::tempdir().expect("a temporary directory");
    let mut p = pipeline(warehouse.path());

    p.accept(&original()).expect("accepts");
    p.accept(&relation(vec![column("id", 20, true)]))
        .expect("accepts");

    p.accept(&begin(1)).expect("accepts");
    for _ in 0..12 {
        p.accept(&insert(1)).expect("accepts");
    }
    p.accept(&commit(10)).expect("accepts");

    assert_eq!(p.stats().dead_lettered, 12);
}

#[test]
fn a_quarantine_is_cleared_only_by_an_explicit_action() {
    // The point of quarantine is that a human decides. A pipeline that could clear its
    // own quarantine would simply be guessing on a delay.
    let warehouse = tempfile::tempdir().expect("a temporary directory");
    let mut p = pipeline(warehouse.path());

    p.accept(&original()).expect("accepts");
    p.accept(&relation(vec![column("id", 20, true)]))
        .expect("accepts");
    assert!(p.quarantine_reason(RELATION).is_some());

    // More traffic does not clear it.
    p.accept(&begin(1)).expect("accepts");
    p.accept(&insert(1)).expect("accepts");
    p.accept(&commit(10)).expect("accepts");
    assert!(p.quarantine_reason(RELATION).is_some());

    // Resolving carries the decision: adopt the shape the source moved to. Merely
    // clearing the flag would leave the table expecting the old shape while the source
    // sends the new one, turning a schema problem into an ingest outage.
    assert_eq!(
        p.pending_schema_columns(RELATION),
        Some(1),
        "the new shape is one column"
    );
    assert!(p.adopt_pending_schema(RELATION));
    assert!(p.quarantine_reason(RELATION).is_none());

    p.accept(&begin(2)).expect("accepts");
    p.accept(&insert(1)).expect("accepts");
    p.accept(&commit(20)).expect("accepts");
    assert_eq!(
        p.publish(true).expect("publishes").len(),
        1,
        "capture resumes under the adopted shape"
    );
}

#[test]
fn an_unrepresentable_new_shape_quarantines_rather_than_writing_the_old_one() {
    // Continuing to write the old shape would silently diverge from the source.
    let warehouse = tempfile::tempdir().expect("a temporary directory");
    let mut p = pipeline(warehouse.path());

    p.accept(&original()).expect("accepts");
    // An unconstrained decimal cannot be carried faithfully.
    p.accept(&relation(vec![
        column("id", 20, true),
        column("label", 25, false),
        column("amount", 1700, false),
    ]))
    .expect("accepts");

    let reason = p
        .quarantine_reason(RELATION)
        .expect("should be quarantined");
    assert!(reason.contains("cannot be carried"), "{reason}");
    assert_eq!(
        p.pending_schema_columns(RELATION),
        None,
        "there is nothing to adopt, so the pipeline must not offer a resolution it \
         cannot actually perform"
    );
    assert!(!p.adopt_pending_schema(RELATION));
}

#[test]
fn one_quarantined_table_does_not_stop_the_others() {
    // Tables are independent. A schema problem in one must not halt capture for the
    // rest, or a single bad table would take the whole analytical tier stale.
    let warehouse = tempfile::tempdir().expect("a temporary directory");
    let mut p = pipeline(warehouse.path());

    let other: u32 = 200;
    let other_relation = Message::Relation(Arc::new(RelationDescriptor {
        relation_id: other,
        namespace: "public".into(),
        name: "other".into(),
        replica_identity: ReplicaIdentity::Default,
        columns: vec![column("id", 20, true)],
    }));

    p.accept(&begin(1)).expect("accepts");
    p.accept(&original()).expect("accepts");
    p.accept(&other_relation).expect("accepts");
    p.accept(&insert(2)).expect("accepts");
    p.accept(&Message::Insert {
        relation_id: other,
        new: TupleData {
            values: vec![TupleValue::Text("1".into())],
        },
    })
    .expect("accepts");
    p.accept(&commit(10)).expect("accepts");

    // Quarantine only the first.
    p.accept(&relation(vec![column("id", 20, true)]))
        .expect("accepts");

    let files = p.publish(true).expect("publishes");
    assert_eq!(files.len(), 1, "the healthy table must still publish");
    assert_eq!(files[0].table, "other");
}

#[test]
fn a_truncate_quarantines_the_table_rather_than_being_discarded() {
    // `ING-05`. The batcher's match ended in `_ => {}` and `Truncate` fell into it: the source
    // table is emptied and the analytical copy keeps every row. **Permanent, silent
    // divergence**, with no counter — and a test asserted the decoder *parses* truncate while
    // nothing asserted that anything downstream acted on it.
    //
    // Applying one is not something this pipeline can do. The analytical copy is an unfolded
    // change log with no key-based fold, so there is no "delete every row" to emit, and
    // inventing one would be worse than saying so. What it must not do is carry on.
    let warehouse = tempfile::tempdir().expect("a temporary directory");
    let mut p = pipeline(warehouse.path());

    p.accept(&begin(1)).expect("accepts");
    p.accept(&original()).expect("accepts");
    p.accept(&insert(2)).expect("accepts");
    p.accept(&commit(10)).expect("accepts");
    p.publish(true).expect("publishes");

    assert!(p.quarantine_reason(RELATION).is_none(), "nothing is wrong yet");

    p.accept(&Message::Truncate {
        relation_ids: vec![RELATION],
        cascade: false,
        restart_identity: false,
    })
    .expect("accepts");

    let reason = p
        .quarantine_reason(RELATION)
        .expect("a truncated table must not be left publishing as though nothing happened");
    assert!(
        reason.contains("truncated"),
        "the reason does not say what happened: {reason}"
    );

    // And events keep being consumed, so the replication cursor still advances. A quarantine
    // that stalls capture turns a schema problem into a source-availability problem, which is
    // the coupling this crate's own module doc exists to prevent.
    p.accept(&begin(2)).expect("accepts");
    p.accept(&insert(2)).expect("accepts");
    p.accept(&commit(20)).expect("accepts");
    assert!(p.stats().dead_lettered >= 1, "a quarantined table stopped consuming");
}

#[test]
fn a_truncate_of_another_table_does_not_quarantine_this_one() {
    // The control. A truncation names its own relations, and sending it to every table would
    // quarantine tables the source never truncated — which is the same silent divergence in
    // the opposite direction: a table stops being fed for something that did not happen to it.
    let warehouse = tempfile::tempdir().expect("a temporary directory");
    let mut p = pipeline(warehouse.path());

    p.accept(&begin(1)).expect("accepts");
    p.accept(&original()).expect("accepts");
    p.accept(&insert(2)).expect("accepts");
    p.accept(&commit(10)).expect("accepts");

    p.accept(&Message::Truncate {
        relation_ids: vec![RELATION + 1],
        cascade: false,
        restart_identity: false,
    })
    .expect("accepts");

    assert!(
        p.quarantine_reason(RELATION).is_none(),
        "a truncation of another relation quarantined this one"
    );
}

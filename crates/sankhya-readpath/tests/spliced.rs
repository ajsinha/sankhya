//! One query, answered from memory and Parquet at once.
//!
//! This is where the tiering stops being a design and starts being observable: a single
//! SQL statement whose rows come partly from files and partly from a buffer, summing to
//! exactly what a single-tier system would have returned.

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

use arrow_array::{Int64Array, RecordBatch, UInt64Array};
use arrow_schema::{DataType, Field, Schema};
use datafusion::prelude::SessionContext;
use sankhya_readpath::{register_spliced, PublishedTier, ReadError, TierSet};
use sankhya_table::{write_parquet, WriterConfig};
use sankhya_table_memory::{ArrivalBuffer, MemoryBudget};
use sankhya_types::{Lsn, LsnRange};
use std::sync::Arc;

fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("amount", DataType::Int64, false),
        Field::new("_sankhya_commit_lsn", DataType::UInt64, false),
    ]))
}

/// Rows at positions `(from, to]`, each worth its own position.
fn rows(from: u64, to: u64) -> RecordBatch {
    let lsns: Vec<u64> = (from + 1..=to).collect();
    let amounts: Vec<i64> = lsns
        .iter()
        .map(|l| i64::try_from(*l).expect("small"))
        .collect();
    RecordBatch::try_new(
        schema(),
        vec![
            Arc::new(Int64Array::from(amounts)),
            Arc::new(UInt64Array::from(lsns)),
        ],
    )
    .expect("building a batch")
}

fn path_of(dir: &std::path::Path, name: &str) -> String {
    dir.join(name).to_str().expect("a utf-8 path").to_string()
}

fn range(from: u64, to: u64) -> LsnRange {
    LsnRange::new(Lsn::new(from), Lsn::new(to)).expect("a non-empty range")
}

/// `SUM(1..=n)`, which is what the whole span must add up to.
fn triangular(n: u64) -> i64 {
    i64::try_from(n * (n + 1) / 2).expect("small")
}

struct Fixture {
    _dir: tempfile::TempDir,
    published: PublishedTier,
    arrival: ArrivalBuffer,
}

/// Positions 1..=`durable` published to Parquet; `durable`+1..=`held` still in memory.
fn fixture(durable: u64, held: u64) -> Fixture {
    let dir = tempfile::tempdir().expect("a temp dir");
    write_parquet(
        dir.path(),
        "part-0000.parquet",
        &rows(0, durable),
        Lsn::new(durable),
        WriterConfig::default(),
    )
    .expect("publishing");

    let mut arrival = ArrivalBuffer::new("arrival", schema(), MemoryBudget::default());
    arrival.append(rows(0, held), range(0, held));
    arrival.note_durable(Lsn::new(durable));

    Fixture {
        published: PublishedTier::new(
            vec![path_of(dir.path(), "part-0000.parquet")],
            LsnRange::up_to(Lsn::new(durable)),
        ),
        arrival,
        _dir: dir,
    }
}

async fn sum_of(ctx: &SessionContext, name: &str) -> (i64, i64) {
    let batches = ctx
        .sql(&format!("SELECT COUNT(*) c, SUM(amount) s FROM {name}"))
        .await
        .expect("planning")
        .collect()
        .await
        .expect("executing");
    let b = &batches[0];
    let count = b
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("count");
    let sum = b
        .column(1)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("sum");
    (count.value(0), sum.value(0))
}

#[tokio::test]
async fn one_query_spans_both_tiers() {
    // 700 rows published, 300 still in memory. The answer must be the whole thousand.
    let f = fixture(700, 1_000);
    let ctx = SessionContext::new();

    let splice = register_spliced(
        &ctx,
        "orders",
        &TierSet::new(Some(&f.published), Some(&f.arrival)),
        Lsn::new(1_000),
    )
    .await
    .expect("the two tiers must splice");

    assert_eq!(splice.tier_names(), vec!["published", "arrival"]);

    let (count, sum) = sum_of(&ctx, "orders").await;
    assert_eq!(count, 1_000);
    assert_eq!(sum, triangular(1_000));
}

#[tokio::test]
async fn the_overlap_is_not_double_counted() {
    // The arrival tier physically holds all 1,000 positions, 700 of which are also in
    // Parquet. A naive union returns 1,700 rows and a wrong sum.
    let f = fixture(700, 1_000);
    assert_eq!(
        f.arrival.rows(),
        1_000,
        "the fixture must actually hold the overlap, or this proves nothing"
    );

    let ctx = SessionContext::new();
    register_spliced(
        &ctx,
        "orders",
        &TierSet::new(Some(&f.published), Some(&f.arrival)),
        Lsn::new(1_000),
    )
    .await
    .expect("splicing");

    let (count, sum) = sum_of(&ctx, "orders").await;
    assert_eq!(count, 1_000, "the durable half was counted twice");
    assert_eq!(sum, triangular(1_000));
}

#[tokio::test]
async fn a_pinned_query_does_not_see_past_its_target() {
    // Time travel. Pinned at 400, the query sees 400 rows even though both tiers hold
    // far more -- and the published tier alone overshoots, which is why every tier is
    // filtered rather than only the ones that obviously need it.
    let f = fixture(700, 1_000);
    let ctx = SessionContext::new();

    let splice = register_spliced(
        &ctx,
        "orders",
        &TierSet::new(Some(&f.published), Some(&f.arrival)),
        Lsn::new(400),
    )
    .await
    .expect("splicing");

    assert_eq!(
        splice.tier_names(),
        vec!["published"],
        "the arrival tier contributes nothing below the durable frontier"
    );

    let (count, sum) = sum_of(&ctx, "orders").await;
    assert_eq!(count, 400);
    assert_eq!(sum, triangular(400));
}

#[tokio::test]
async fn a_query_beyond_what_any_tier_holds_is_refused() {
    // Refused, not answered with what happens to be available. An incomplete answer that
    // looks complete is the worst thing this system can produce.
    let f = fixture(700, 1_000);
    let ctx = SessionContext::new();

    let err = register_spliced(
        &ctx,
        "orders",
        &TierSet::new(Some(&f.published), Some(&f.arrival)),
        Lsn::new(5_000),
    )
    .await
    .expect_err("a target beyond the frontier must be refused");

    assert!(matches!(err, ReadError::Splice(_)), "{err}");
}

#[tokio::test]
async fn a_gap_between_the_tiers_is_refused() {
    // The published tier stops at 300; the arrival tier starts at 700. Positions 301
    // through 700 are held by nothing. Answering would silently omit them.
    let dir = tempfile::tempdir().expect("a temp dir");
    write_parquet(
        dir.path(),
        "part-0000.parquet",
        &rows(0, 300),
        Lsn::new(300),
        WriterConfig::default(),
    )
    .expect("publishing");

    let mut arrival = ArrivalBuffer::new("arrival", schema(), MemoryBudget::default());
    arrival.append(rows(700, 1_000), range(700, 1_000));

    let published = PublishedTier::new(
        vec![path_of(dir.path(), "part-0000.parquet")],
        LsnRange::up_to(Lsn::new(300)),
    );

    let ctx = SessionContext::new();
    let err = register_spliced(
        &ctx,
        "orders",
        &TierSet::new(Some(&published), Some(&arrival)),
        Lsn::new(1_000),
    )
    .await
    .expect_err("a gap must be refused");

    assert!(matches!(err, ReadError::Splice(_)), "{err}");
}

#[tokio::test]
async fn a_write_is_visible_before_publication() {
    // Read-your-own-writes, with nothing published at all. This is what the arrival tier
    // buys: the answer does not wait for a Parquet file to appear.
    let mut arrival = ArrivalBuffer::new("arrival", schema(), MemoryBudget::default());
    arrival.append(rows(0, 50), range(0, 50));

    let ctx = SessionContext::new();
    let splice = register_spliced(
        &ctx,
        "orders",
        &TierSet::new(None, Some(&arrival)),
        Lsn::new(50),
    )
    .await
    .expect("the arrival tier alone must answer");

    assert_eq!(splice.tier_names(), vec!["arrival"]);
    let (count, sum) = sum_of(&ctx, "orders").await;
    assert_eq!(count, 50);
    assert_eq!(sum, triangular(50));
}

#[tokio::test]
async fn provenance_names_the_tiers_that_answered() {
    // Returned on every read rather than on request. "Which tier answered me" is a
    // question an auditor eventually asks.
    let f = fixture(700, 1_000);
    let ctx = SessionContext::new();

    let splice = register_spliced(
        &ctx,
        "orders",
        &TierSet::new(Some(&f.published), Some(&f.arrival)),
        Lsn::new(1_000),
    )
    .await
    .expect("splicing");

    let provenance = splice.provenance();
    assert_eq!(provenance.len(), 2);
    assert_eq!(provenance[0].0, "published");
    assert_eq!(provenance[0].1.end_inclusive(), Lsn::new(700));
    assert_eq!(provenance[1].0, "arrival");
    assert_eq!(provenance[1].1.start_exclusive(), Lsn::new(700));
    assert_eq!(provenance[1].1.end_inclusive(), Lsn::new(1_000));
}

#[tokio::test]
async fn an_empty_tier_set_is_reported_as_such() {
    let ctx = SessionContext::new();
    let err = register_spliced(&ctx, "orders", &TierSet::new(None, None), Lsn::new(1))
        .await
        .expect_err("nothing to read from");
    assert!(matches!(err, ReadError::NoTiers), "{err}");
}

#[tokio::test]
async fn a_tier_the_planner_rejected_is_not_read() {
    // Both tiers are *offered*, and the planner selects only the published one, because
    // it reaches the target on its own. The arrival tier is redundant here rather than
    // wrong -- it holds real rows for positions 701..=1000, which the published tier
    // also holds.
    //
    // Reading it anyway would double-count those 300 positions. That is why the
    // planner's selection is authoritative and not merely advisory: registering
    // everything available and letting the query sort it out discards the proof.
    //
    // This case exists because a mutation that read every offered tier instead of every
    // selected one survived the rest of this file. In each of the other tests the two
    // sets happen to coincide, or the extra tier contributes nothing after filtering,
    // so nothing noticed.
    let dir = tempfile::tempdir().expect("a temp dir");
    write_parquet(
        dir.path(),
        "part-0000.parquet",
        &rows(0, 1_000),
        Lsn::new(1_000),
        WriterConfig::default(),
    )
    .expect("publishing");

    let published = PublishedTier::new(
        vec![path_of(dir.path(), "part-0000.parquet")],
        LsnRange::up_to(Lsn::new(1_000)),
    );

    let mut arrival = ArrivalBuffer::new("arrival", schema(), MemoryBudget::default());
    arrival.append(rows(0, 1_000), range(0, 1_000));
    arrival.note_durable(Lsn::new(700));

    // The arrival tier really does hold rows that would be counted twice.
    assert_eq!(
        arrival.scan(Lsn::new(1_000)).expect("scanning").len(),
        1,
        "the fixture must offer a tier with real overlapping rows, or this proves nothing"
    );

    let ctx = SessionContext::new();
    let splice = register_spliced(
        &ctx,
        "orders",
        &TierSet::new(Some(&published), Some(&arrival)),
        Lsn::new(1_000),
    )
    .await
    .expect("splicing");

    assert_eq!(
        splice.tier_names(),
        vec!["published"],
        "the published tier reaches the target alone"
    );

    let (count, sum) = sum_of(&ctx, "orders").await;
    assert_eq!(
        count, 1_000,
        "a rejected tier was read and rows were counted twice"
    );
    assert_eq!(sum, triangular(1_000));
}

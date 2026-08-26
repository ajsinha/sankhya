//! Files the catalogue proves irrelevant are never read.
//!
//! The pair of assertions that matters: pruning **happens** (otherwise the statistics
//! are ornamental) and pruning **never changes an answer** (otherwise they are worse
//! than ornamental).

use arrow_array::{Int64Array, RecordBatch, UInt64Array};
use arrow_schema::{DataType, Field, Schema};
use datafusion::prelude::SessionContext;
use sankhya_plan::{plan_splice, TierRef};
use sankhya_readpath::{LoggedFile, SankhyaTable};
use sankhya_stats::{Bound, ColumnStats};
use sankhya_table::{write_parquet, WriterConfig};
use sankhya_types::{Lsn, LsnRange};
use std::collections::BTreeMap;
use std::sync::Arc;

fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("amount", DataType::Int64, false),
        Field::new("_sankhya_commit_lsn", DataType::UInt64, false),
    ]))
}

/// One file holding `amount` values `from..to`, at positions matching.
fn fragment(dir: &std::path::Path, index: u64, from: i64, to: i64) -> LoggedFile {
    let amounts: Vec<i64> = (from..to).collect();
    let lsns: Vec<u64> = amounts
        .iter()
        .map(|a| u64::try_from(*a).expect("small"))
        .collect();
    let batch = RecordBatch::try_new(
        schema(),
        vec![
            Arc::new(Int64Array::from(amounts.clone())),
            Arc::new(UInt64Array::from(lsns)),
        ],
    )
    .expect("building");

    let name = format!("part-{index:04}.parquet");
    let report = write_parquet(
        dir,
        &name,
        &batch,
        Lsn::new(u64::try_from(to - 1).expect("small")),
        WriterConfig::default(),
    )
    .expect("writing");

    let mut stats = ColumnStats::default();
    for amount in &amounts {
        stats.observe(Some(&amount.to_be_bytes()), Some(Bound::Int(*amount)));
    }
    let mut catalogue = BTreeMap::new();
    catalogue.insert("amount".to_string(), stats);

    LoggedFile::new(
        dir.join(&name).to_str().expect("a utf-8 path").to_string(),
        report.bytes,
        u64::try_from(to - from).expect("small"),
    )
    .with_stats(catalogue)
}

/// Ten files covering amounts 1..1001, in blocks of a hundred.
fn table(dir: &std::path::Path) -> (Vec<LoggedFile>, SankhyaTable) {
    let files: Vec<LoggedFile> = (0..10u64)
        .map(|i| {
            let from = i64::try_from(i).expect("small") * 100 + 1;
            fragment(dir, i, from, from + 100)
        })
        .collect();

    let coverage = LsnRange::up_to(Lsn::new(1_000));
    let splice = plan_splice(&[TierRef::new("published", coverage)], Lsn::new(1_000))
        .expect("a single tier covers it");

    (
        files.clone(),
        SankhyaTable::new(schema(), files, Vec::new(), Lsn::new(1_000), splice, true),
    )
}

async fn measure(table: SankhyaTable, sql: &str) -> (i64, i64) {
    let ctx = SessionContext::new();
    ctx.register_table("orders", Arc::new(table))
        .expect("registering");
    let batches = ctx
        .sql(sql)
        .await
        .expect("planning")
        .collect()
        .await
        .expect("executing");
    let b = &batches[0];
    (
        b.column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .expect("count")
            .value(0),
        b.column(1)
            .as_any()
            .downcast_ref::<Int64Array>()
            .expect("sum")
            .value(0),
    )
}

#[tokio::test]
async fn a_narrow_predicate_prunes_almost_every_file() {
    // Ten files, one of which can hold a match. The statistics prove the other nine
    // irrelevant, so their footers are never read.
    let dir = tempfile::tempdir().expect("a temp dir");
    let (_, provider) = table(dir.path());

    let filters = vec![datafusion::prelude::col("amount").eq(datafusion::prelude::lit(450i64))];
    assert_eq!(provider.prunable(&filters), 9);

    let (count, sum) = measure(
        provider,
        "SELECT COUNT(*) c, SUM(amount) s FROM orders WHERE amount = 450",
    )
    .await;
    assert_eq!(count, 1);
    assert_eq!(sum, 450);
}

#[tokio::test]
async fn pruning_does_not_change_the_answer() {
    // The assertion that makes pruning safe rather than merely fast. Same query, with
    // and without the catalogue.
    let dir = tempfile::tempdir().expect("a temp dir");
    let (files, with_stats) = table(dir.path());

    let stripped: Vec<LoggedFile> = files
        .iter()
        .map(|f| LoggedFile::new(f.path.clone(), f.size, f.rows))
        .collect();
    let coverage = LsnRange::up_to(Lsn::new(1_000));
    let splice = plan_splice(&[TierRef::new("published", coverage)], Lsn::new(1_000))
        .expect("a single tier");
    let without_stats = SankhyaTable::new(
        schema(),
        stripped,
        Vec::new(),
        Lsn::new(1_000),
        splice,
        true,
    );

    const SQL: &str =
        "SELECT COUNT(*) c, SUM(amount) s FROM orders WHERE amount > 250 AND amount <= 640";

    let pruned = measure(with_stats, SQL).await;
    let unpruned = measure(without_stats, SQL).await;

    assert_eq!(pruned, unpruned, "pruning changed the answer");
    assert_eq!(pruned.0, 390);
}

#[tokio::test]
async fn a_range_predicate_prunes_the_blocks_outside_it() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let (_, provider) = table(dir.path());

    // 250 < amount <= 640 touches blocks 201-300, 301-400, 401-500, 501-600, 601-700:
    // five of ten.
    let filters = vec![
        datafusion::prelude::col("amount").gt(datafusion::prelude::lit(250i64)),
        datafusion::prelude::col("amount").lt_eq(datafusion::prelude::lit(640i64)),
    ];
    assert_eq!(provider.prunable(&filters), 5);
}

#[tokio::test]
async fn a_predicate_on_an_uncatalogued_column_prunes_nothing() {
    // A column the catalogue knows nothing about proves nothing. Unknown is not
    // unbounded.
    let dir = tempfile::tempdir().expect("a temp dir");
    let (_, provider) = table(dir.path());

    let filters =
        vec![datafusion::prelude::col("_sankhya_commit_lsn").eq(datafusion::prelude::lit(5u64))];
    assert_eq!(provider.prunable(&filters), 0);
}

#[tokio::test]
async fn a_disjunction_prunes_nothing() {
    // `a OR b` cannot be split and applied independently: a file may fail one side and
    // satisfy the other. Taking either side is the natural implementation and it
    // silently drops rows -- here, a file holding 1..=100 fails `amount > 950` and would
    // be skipped despite satisfying `amount < 50`.
    //
    // Inequalities rather than equalities, deliberately. `a = 50 OR a = 950` is rewritten
    // by the engine into an `IN` list before it ever reaches this code, so a test written
    // that way exercises no disjunction at all -- which is what the first version of this
    // test did, and a mutation that split disjunctions survived it untouched.
    let dir = tempfile::tempdir().expect("a temp dir");
    let (_, provider) = table(dir.path());

    let filters = vec![datafusion::prelude::col("amount")
        .lt(datafusion::prelude::lit(50i64))
        .or(datafusion::prelude::col("amount").gt(datafusion::prelude::lit(950i64)))];

    // The shape really is a disjunction and not something already rewritten.
    assert!(
        matches!(
            &filters[0],
            datafusion::logical_expr::Expr::BinaryExpr(b)
                if b.op == datafusion::logical_expr::Operator::Or
        ),
        "the fixture is not a disjunction: {:?}",
        filters[0]
    );

    assert_eq!(provider.prunable(&filters), 0);

    let (count, sum) = measure(
        provider,
        "SELECT COUNT(*) c, SUM(amount) s FROM orders WHERE amount < 50 OR amount > 950",
    )
    .await;
    // 1..=49 and 951..=1000.
    assert_eq!(count, 99, "a disjunct was dropped");
    assert_eq!(sum, (1..50).sum::<i64>() + (951..=1_000).sum::<i64>());
}

#[tokio::test]
async fn a_reversed_comparison_prunes_the_same_way() {
    // `450 = amount` is `amount = 450`. Not flipping the operator would be a *wrong*
    // skip rather than a missed one, which is why the reversed form is recognised
    // rather than ignored.
    let dir = tempfile::tempdir().expect("a temp dir");
    let (_, provider) = table(dir.path());

    let forward = vec![datafusion::prelude::col("amount").gt(datafusion::prelude::lit(900i64))];
    let reversed = vec![datafusion::prelude::lit(900i64).lt(datafusion::prelude::col("amount"))];

    assert_eq!(provider.prunable(&forward), 9);
    assert_eq!(provider.prunable(&reversed), 9);
}

#[tokio::test]
async fn no_predicate_prunes_nothing() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let (_, provider) = table(dir.path());
    assert_eq!(provider.prunable(&[]), 0);

    let (count, _) = measure(provider, "SELECT COUNT(*) c, SUM(amount) s FROM orders").await;
    assert_eq!(count, 1_000);
}

/// What the catalogue is worth, measured.
///
/// Run with `cargo test -p sankhya-readpath --test pruning --release -- --ignored
/// --nocapture measure`.
///
/// Pruning helps in proportion to how much of the table a predicate can exclude, so a
/// single number would be meaningless. Three selectivities are measured against the same
/// data.
#[tokio::test]
#[ignore = "a measurement, not an assertion"]
async fn measure_what_pruning_saves() {
    const BLOCKS: u64 = 200;
    const PER: i64 = 2_000;

    let dir = tempfile::tempdir().expect("a temp dir");
    let files: Vec<LoggedFile> = (0..BLOCKS)
        .map(|i| {
            let from = i64::try_from(i).expect("small") * PER + 1;
            fragment(dir.path(), i, from, from + PER)
        })
        .collect();
    let total = i64::try_from(BLOCKS).expect("small") * PER;

    let stripped: Vec<LoggedFile> = files
        .iter()
        .map(|f| LoggedFile::new(f.path.clone(), f.size, f.rows))
        .collect();

    let build = |set: Vec<LoggedFile>| {
        let coverage = LsnRange::up_to(Lsn::new(u64::try_from(total).expect("small")));
        let splice = plan_splice(
            &[TierRef::new("published", coverage)],
            coverage.end_inclusive(),
        )
        .expect("a single tier");
        SankhyaTable::new(
            schema(),
            set,
            Vec::new(),
            coverage.end_inclusive(),
            splice,
            true,
        )
    };

    let cases = [
        ("one file in 200", format!("amount = {}", total / 2)),
        (
            "a tenth",
            format!(
                "amount > {} AND amount <= {}",
                total / 2,
                total / 2 + total / 10
            ),
        ),
        ("everything", "amount > 0".to_string()),
    ];

    for (label, predicate) in cases {
        let sql = format!("SELECT COUNT(*) c, SUM(amount) s FROM orders WHERE {predicate}");

        let mut pruned_best = std::time::Duration::MAX;
        let mut plain_best = std::time::Duration::MAX;
        let mut answer = (0, 0);

        for _ in 0..3 {
            let t = std::time::Instant::now();
            answer = measure(build(files.clone()), &sql).await;
            pruned_best = pruned_best.min(t.elapsed());

            let t = std::time::Instant::now();
            let plain = measure(build(stripped.clone()), &sql).await;
            plain_best = plain_best.min(t.elapsed());
            assert_eq!(answer, plain, "pruning changed the answer for {label}");
        }

        let skipped = build(files.clone()).prunable(&[]);
        let _ = skipped;
        println!(
            "{label:>18}: with stats {:>9.2?}   without {:>9.2?}   {:.1}x",
            pruned_best,
            plain_best,
            plain_best.as_secs_f64() / pruned_best.as_secs_f64()
        );
    }
}

#[tokio::test]
async fn the_optimizer_is_given_the_catalogue_rather_than_a_guess() {
    // Distinct-value counts are what an optimizer needs to order a join, and neither the
    // file format nor the table log carries them. Without these it plans on a guess --
    // usually "distinct equals rows", which makes every column look like a key and every
    // join order look equally good.
    use datafusion::catalog::TableProvider;
    use datafusion::common::stats::Precision;

    let dir = tempfile::tempdir().expect("a temp dir");
    let (_, provider) = table(dir.path());

    let stats = provider.statistics().expect("statistics");
    let index = schema().index_of("amount").expect("the amount column");
    let amount = &stats.column_statistics[index];

    // Bounds, exactly, from the catalogue.
    assert_eq!(
        amount.min_value,
        Precision::Exact(datafusion::scalar::ScalarValue::Int64(Some(1)))
    );
    assert_eq!(
        amount.max_value,
        Precision::Exact(datafusion::scalar::ScalarValue::Int64(Some(1_000)))
    );
    assert_eq!(amount.null_count, Precision::Exact(0));

    // A cardinality, and marked inexact. The sketch is accurate to a few percent, which
    // is right for choosing a join order and wrong for concluding a column is unique --
    // and an optimizer told a cardinality is exact may act on the latter.
    let Precision::Inexact(distinct) = amount.distinct_count else {
        panic!(
            "no cardinality reached the optimizer: {:?}",
            amount.distinct_count
        );
    };
    assert!(
        (900..=1_100).contains(&distinct),
        "a thousand distinct values estimated as {distinct}"
    );
}

#[tokio::test]
async fn a_column_the_catalogue_does_not_cover_is_reported_unknown() {
    // Merging only the files that happen to have statistics would produce bounds
    // describing part of the table while claiming to describe all of it -- and an
    // optimizer given a bound that excludes real values plans as though those rows do
    // not exist.
    use datafusion::catalog::TableProvider;
    use datafusion::common::stats::Precision;

    let dir = tempfile::tempdir().expect("a temp dir");
    let (files, _) = table(dir.path());

    // One file loses its statistics, as an unrecognised type or an interrupted
    // compaction would leave it.
    let mut partial = files;
    partial[3] = LoggedFile::new(partial[3].path.clone(), partial[3].size, partial[3].rows);

    let coverage = LsnRange::up_to(Lsn::new(1_000));
    let splice = plan_splice(&[TierRef::new("published", coverage)], Lsn::new(1_000))
        .expect("a single tier");
    let provider = SankhyaTable::new(schema(), partial, Vec::new(), Lsn::new(1_000), splice, true);

    let stats = provider.statistics().expect("statistics");
    let index = schema().index_of("amount").expect("the amount column");
    assert_eq!(stats.column_statistics[index].min_value, Precision::Absent);
    assert_eq!(stats.column_statistics[index].max_value, Precision::Absent);

    // The row count still comes from the log, which every file has.
    assert_eq!(stats.num_rows, Precision::Exact(1_000));
}

#[tokio::test]
async fn a_bound_the_optimizer_cannot_represent_exactly_is_withheld() {
    // An approximate bound handed to an optimizer is worse than none: it will be trusted,
    // and the plan chosen from it is chosen confidently on a wrong number. An infinity
    // and a byte string that is not text both have no exact representation, so both are
    // reported absent rather than coerced into something close.
    use datafusion::catalog::TableProvider;
    use datafusion::common::stats::Precision;
    use sankhya_stats::ColumnStats;

    let dir = tempfile::tempdir().expect("a temp dir");
    let (files, _) = table(dir.path());

    let mut with_infinity = files;
    for file in &mut with_infinity {
        let mut catalogue = BTreeMap::new();
        catalogue.insert(
            "amount".to_string(),
            ColumnStats {
                rows: file.rows,
                nulls: 0,
                min: Some(Bound::Float(f64::NEG_INFINITY)),
                max: Some(Bound::Float(f64::INFINITY)),
                ..ColumnStats::default()
            },
        );
        *file = LoggedFile::new(file.path.clone(), file.size, file.rows).with_stats(catalogue);
    }

    let coverage = LsnRange::up_to(Lsn::new(1_000));
    let splice = plan_splice(&[TierRef::new("published", coverage)], Lsn::new(1_000))
        .expect("a single tier");
    let provider = SankhyaTable::new(
        schema(),
        with_infinity,
        Vec::new(),
        Lsn::new(1_000),
        splice,
        true,
    );

    let stats = provider.statistics().expect("statistics");
    let index = schema().index_of("amount").expect("the amount column");
    assert_eq!(stats.column_statistics[index].min_value, Precision::Absent);
    assert_eq!(stats.column_statistics[index].max_value, Precision::Absent);

    // A finite float, by contrast, goes through.
    let mut finite = (0..1u64)
        .map(|_| {
            let mut catalogue = BTreeMap::new();
            catalogue.insert(
                "amount".to_string(),
                ColumnStats {
                    rows: 10,
                    nulls: 0,
                    min: Some(Bound::Float(1.5)),
                    max: Some(Bound::Float(9.5)),
                    ..ColumnStats::default()
                },
            );
            LoggedFile::new("x.parquet".to_string(), 1, 10).with_stats(catalogue)
        })
        .collect::<Vec<_>>();
    finite.truncate(1);

    let splice = plan_splice(&[TierRef::new("published", coverage)], Lsn::new(1_000))
        .expect("a single tier");
    let provider = SankhyaTable::new(schema(), finite, Vec::new(), Lsn::new(1_000), splice, true);
    let stats = provider.statistics().expect("statistics");
    assert_eq!(
        stats.column_statistics[index].min_value,
        Precision::Exact(datafusion::scalar::ScalarValue::Float64(Some(1.5)))
    );
}

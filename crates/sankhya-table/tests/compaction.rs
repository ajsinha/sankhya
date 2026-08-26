//! Compaction executes, and does not change what a query returns.
//!
//! The policy decides *when* to merge; these tests cover what happens when it does.
//! The property that matters is not that the merge produces fewer files — that is
//! trivially true — but that the data is identical afterwards. A compaction that
//! quietly dropped, duplicated or reordered rows would leave a smaller, internally
//! consistent, entirely wrong table.

use arrow_array::{Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use datafusion::prelude::SessionContext;
use sankhya_table::{WriterConfig, compact_files, read_parquet_stats, write_parquet};
use sankhya_types::Lsn;
use std::path::PathBuf;
use std::sync::Arc;

fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("bucket", DataType::Utf8, false),
        Field::new("amount", DataType::Int64, false),
    ]))
}

/// One small file, of the kind streaming capture actually produces.
fn batch(start: i64, rows: i64) -> RecordBatch {
    let ids: Vec<i64> = (start..start + rows).collect();
    let buckets: Vec<String> = ids.iter().map(|i| format!("b{}", i % 7)).collect();
    let amounts: Vec<i64> = ids.iter().map(|i| i * 3 % 1000).collect();
    RecordBatch::try_new(
        schema(),
        vec![
            Arc::new(Int64Array::from(ids)),
            Arc::new(StringArray::from(buckets)),
            Arc::new(Int64Array::from(amounts)),
        ],
    )
    .expect("building a batch")
}

/// Write `n` small files, as a busy capture stream would.
fn write_fragments(dir: &std::path::Path, n: i64, rows_each: i64) -> Vec<PathBuf> {
    (0..n)
        .map(|i| {
            write_parquet(
                dir,
                &format!("part-{i:04}.parquet"),
                &batch(i * rows_each, rows_each),
                Lsn::new(1000 + u64::try_from(i).unwrap()),
                WriterConfig::default(),
            )
            .expect("writing a fragment")
            .path
        })
        .collect()
}

async fn query_all(dir: &std::path::Path, sql: &str) -> Vec<String> {
    let ctx = SessionContext::new();
    ctx.register_parquet(
        "t",
        dir.to_str().expect("a utf-8 path"),
        datafusion::prelude::ParquetReadOptions::default(),
    )
    .await
    .expect("registering the table");
    let batches = ctx.sql(sql).await.expect("planning").collect().await.expect("executing");
    batches
        .iter()
        .map(|b| {
            arrow::util::pretty::pretty_format_batches(std::slice::from_ref(b))
                .expect("formatting")
                .to_string()
        })
        .collect()
}

#[tokio::test]
async fn compaction_does_not_change_query_results() {
    let before_dir = tempfile::tempdir().expect("a temp dir");
    let after_dir = tempfile::tempdir().expect("a temp dir");

    let inputs = write_fragments(before_dir.path(), 12, 5_000);

    // The same aggregate, over the fragments and then over the merged file. Grouped and
    // ordered so the comparison is not sensitive to scan order, which legitimately
    // differs between one file and twelve.
    const SQL: &str = "SELECT bucket, COUNT(*) c, SUM(amount) s, MIN(id) lo, MAX(id) hi \
                       FROM t GROUP BY bucket ORDER BY bucket";

    let before = query_all(before_dir.path(), SQL).await;

    let outcome = compact_files(
        &inputs,
        after_dir.path(),
        "compacted-0000.parquet",
        Lsn::new(1011),
        WriterConfig::default(),
    )
    .expect("compacting");

    let after = query_all(after_dir.path(), SQL).await;

    assert_eq!(before, after, "compaction changed what the query returns");
    assert_eq!(outcome.rows, 60_000);
}

#[test]
fn compaction_preserves_every_row() {
    let src = tempfile::tempdir().expect("a temp dir");
    let dst = tempfile::tempdir().expect("a temp dir");
    let inputs = write_fragments(src.path(), 8, 1_000);

    let total_before: u64 = inputs
        .iter()
        .map(|p| read_parquet_stats(p).expect("stats").0)
        .sum();

    let outcome = compact_files(
        &inputs,
        dst.path(),
        "out.parquet",
        Lsn::new(1007),
        WriterConfig::default(),
    )
    .expect("compacting");

    assert_eq!(outcome.rows, total_before);
}

#[test]
fn merging_is_smaller_than_the_fragments_it_replaces() {
    // The point of compaction. Not asserted as a fixed ratio — that would encode a
    // property of the fixture, not of the operation — but the merged form must not be
    // *larger*, and in practice per-file overhead alone makes it smaller.
    let src = tempfile::tempdir().expect("a temp dir");
    let dst = tempfile::tempdir().expect("a temp dir");
    let inputs = write_fragments(src.path(), 20, 500);

    let outcome = compact_files(
        &inputs,
        dst.path(),
        "out.parquet",
        Lsn::new(1019),
        WriterConfig::default(),
    )
    .expect("compacting");

    assert!(
        outcome.size_ratio() > 1.0,
        "merging 20 fragments did not save space: {} bytes became {}",
        outcome.bytes_before,
        outcome.bytes
    );
}

#[test]
fn inputs_survive_the_merge() {
    // The safety rule: compaction only ever adds. Every input is still readable
    // afterwards, so a reader that resolved a snapshot before the merge is unaffected.
    let src = tempfile::tempdir().expect("a temp dir");
    let dst = tempfile::tempdir().expect("a temp dir");
    let inputs = write_fragments(src.path(), 4, 100);

    let outcome = compact_files(
        &inputs,
        dst.path(),
        "out.parquet",
        Lsn::new(1003),
        WriterConfig::default(),
    )
    .expect("compacting");

    assert_eq!(outcome.inputs_retained, inputs);
    for path in &inputs {
        assert!(path.exists(), "{} was removed by the merge", path.display());
    }
}

#[test]
fn coverage_is_carried_onto_the_output() {
    // The merged file must declare the same coverage its inputs did, or the read path
    // would see a gap where there is none and refuse to answer.
    let src = tempfile::tempdir().expect("a temp dir");
    let dst = tempfile::tempdir().expect("a temp dir");
    let inputs = write_fragments(src.path(), 3, 50);

    let outcome = compact_files(
        &inputs,
        dst.path(),
        "out.parquet",
        Lsn::new(1002),
        WriterConfig::default(),
    )
    .expect("compacting");

    assert_eq!(outcome.covers_through, Lsn::new(1002));
}

#[test]
fn merging_one_file_is_refused() {
    // Rewriting a single file costs a full read and write and changes nothing. Silently
    // accepting it would let a scheduler burn I/O in a loop.
    let src = tempfile::tempdir().expect("a temp dir");
    let dst = tempfile::tempdir().expect("a temp dir");
    let inputs = write_fragments(src.path(), 1, 10);

    let err = compact_files(
        &inputs,
        dst.path(),
        "out.parquet",
        Lsn::new(1000),
        WriterConfig::default(),
    )
    .expect_err("merging one file should be refused");

    assert!(format!("{err}").contains("at least two"));
}

#[test]
fn merging_across_schemas_is_refused() {
    // Two files with different shapes. Concatenating them would need a decision about
    // the missing column, and every available answer is wrong: refusing is the only
    // honest option, and schema evolution has its own path for the legitimate case.
    let src = tempfile::tempdir().expect("a temp dir");
    let dst = tempfile::tempdir().expect("a temp dir");

    let wide = write_parquet(
        src.path(),
        "wide.parquet",
        &batch(0, 10),
        Lsn::new(1000),
        WriterConfig::default(),
    )
    .expect("writing")
    .path;

    let narrow_schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]));
    let narrow_batch = RecordBatch::try_new(
        narrow_schema,
        vec![Arc::new(Int64Array::from(vec![1i64, 2, 3]))],
    )
    .expect("building");
    let narrow = write_parquet(
        src.path(),
        "narrow.parquet",
        &narrow_batch,
        Lsn::new(1001),
        WriterConfig::default(),
    )
    .expect("writing")
    .path;

    let err = compact_files(
        &[wide, narrow],
        dst.path(),
        "out.parquet",
        Lsn::new(1001),
        WriterConfig::default(),
    )
    .expect_err("merging across schemas should be refused");

    assert!(format!("{err}").contains("schema"));
}

/// What compaction is actually worth, measured rather than asserted.
///
/// Run with `cargo test -p sankhya-table --test compaction --release -- --ignored
/// --nocapture measure`.
///
/// The claim under test is specific: small files cost query **planning** — listing,
/// footer reads, metadata resolution — not scanning. If that is right, the penalty is
/// roughly fixed per query and therefore dominates short queries and disappears into
/// long ones. Both are measured, because a single number would not distinguish the
/// claim from "more files are slower".
#[tokio::test]
#[ignore = "a measurement, not an assertion"]
async fn measure_the_cost_of_small_files() {
    const FRAGMENTS: i64 = 400;
    const ROWS_EACH: i64 = 50_000;

    let many = tempfile::tempdir().expect("a temp dir");
    let one = tempfile::tempdir().expect("a temp dir");

    let inputs = write_fragments(many.path(), FRAGMENTS, ROWS_EACH);
    let outcome = compact_files(
        &inputs,
        one.path(),
        "compacted.parquet",
        Lsn::new(9_999),
        WriterConfig::default(),
    )
    .expect("compacting");

    // A short query: touches one small group. Planning should dominate.
    const SHORT: &str = "SELECT SUM(amount) FROM t WHERE id BETWEEN 10 AND 20";
    // A long query: touches everything. Scanning should dominate.
    const LONG: &str = "SELECT bucket, SUM(amount) FROM t GROUP BY bucket";

    for (label, sql) in [("short", SHORT), ("long", LONG)] {
        let mut fragmented = std::time::Duration::ZERO;
        let mut merged = std::time::Duration::ZERO;

        // Three passes, taking the best of each, so a stray scheduling delay does not
        // become the headline number.
        for _ in 0..3 {
            let t = std::time::Instant::now();
            let _ = query_all(many.path(), sql).await;
            let e = t.elapsed();
            if fragmented.is_zero() || e < fragmented {
                fragmented = e;
            }

            let t = std::time::Instant::now();
            let _ = query_all(one.path(), sql).await;
            let e = t.elapsed();
            if merged.is_zero() || e < merged {
                merged = e;
            }
        }

        println!(
            "{label:>6}: {FRAGMENTS} files {:>8.1?}   1 file {:>8.1?}   {:.2}x",
            fragmented,
            merged,
            fragmented.as_secs_f64() / merged.as_secs_f64()
        );
    }

    println!(
        "  size: {} bytes across {FRAGMENTS} files -> {} bytes in one ({:.2}x)",
        outcome.bytes_before,
        outcome.bytes,
        outcome.size_ratio()
    );
}

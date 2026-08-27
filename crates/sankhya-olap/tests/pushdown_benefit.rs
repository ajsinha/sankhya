//! Measure what the pushdown setting is actually worth.
//!
//! The assertion in `settings.rs` guards a setting whose absence has no symptom but
//! slowness. This quantifies that slowness, so the claim rests on a number rather than
//! on an assumption.
//!
//! Ignored by default: it writes a substantial Parquet file and runs timed queries.
//!
//! ```text
//! cargo test -p sankhya-olap --test pushdown_benefit --release -- --ignored --nocapture
//! ```

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

use datafusion::arrow::array::{Float64Array, Int64Array, RecordBatch, StringArray};
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::parquet::arrow::ArrowWriter;
use datafusion::parquet::basic::{Compression, ZstdLevel};
use datafusion::parquet::file::properties::{EnabledStatistics, WriterProperties};
use datafusion::prelude::{ParquetReadOptions, SessionConfig, SessionContext};
use std::sync::Arc;
use std::time::Instant;

/// A wide-ish table: one selective key column and several payload columns.
///
/// The shape matters. Pushdown pays when a predicate eliminates most rows *and* the
/// surviving rows carry payload that would otherwise be decoded needlessly — which is
/// the ordinary shape of an analytical table, not a contrived one.
fn write_fixture(path: &std::path::Path, rows: usize) -> u64 {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("needle", DataType::Int64, false),
        Field::new("pad_a", DataType::Utf8, false),
        Field::new("pad_b", DataType::Utf8, false),
        Field::new("pad_c", DataType::Utf8, false),
        Field::new("value", DataType::Float64, false),
    ]));

    let properties = WriterProperties::builder()
        .set_compression(Compression::ZSTD(ZstdLevel::try_new(3).unwrap_or_default()))
        .set_statistics_enabled(EnabledStatistics::Page)
        // Without this the page index degenerates to one entry and page pruning stops
        // working — the second silently-disabling default.
        .set_data_page_row_count_limit(20_000)
        .build();

    let file = std::fs::File::create(path).expect("creates");
    let mut writer = ArrowWriter::try_new(file, schema.clone(), Some(properties)).expect("writer");

    const CHUNK: usize = 100_000;
    let mut written = 0usize;
    while written < rows {
        let n = CHUNK.min(rows - written);
        let ids: Vec<i64> = (0..n).map(|i| (written + i) as i64).collect();
        // One row in ten thousand matches, so the predicate is genuinely selective.
        let needles: Vec<i64> = ids.iter().map(|i| i % 10_000).collect();
        // Incompressible padding. A repetitive string dictionary-encodes so well that
        // decoding it costs almost nothing, and avoiding a decode that is already free
        // saves nothing — which is exactly the artefact the first version of this
        // measurement produced.
        let pad: Vec<String> = ids
            .iter()
            .map(|i| {
                let mut h = (*i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
                let mut out = String::with_capacity(64);
                for _ in 0..4 {
                    h ^= h >> 33;
                    h = h.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
                    out.push_str(&format!("{h:016x}"));
                }
                out
            })
            .collect();
        let values: Vec<f64> = ids.iter().map(|i| *i as f64 * 1.5).collect();

        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(Int64Array::from(ids)),
                Arc::new(Int64Array::from(needles)),
                Arc::new(StringArray::from(pad.clone())),
                Arc::new(StringArray::from(pad.clone())),
                Arc::new(StringArray::from(pad)),
                Arc::new(Float64Array::from(values)),
            ],
        )
        .expect("batch");
        writer.write(&batch).expect("writes");
        written += n;
    }
    writer.close().expect("closes");
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

async fn time_query(pushdown: bool, path: &std::path::Path, runs: usize) -> (f64, i64) {
    let config = SessionConfig::new()
        .set_str(
            "datafusion.execution.parquet.pushdown_filters",
            &pushdown.to_string(),
        )
        .set_str(
            "datafusion.execution.parquet.reorder_filters",
            &pushdown.to_string(),
        );
    let ctx = SessionContext::new_with_config(config);
    ctx.register_parquet(
        "t",
        path.to_string_lossy().as_ref(),
        ParquetReadOptions::default(),
    )
    .await
    .expect("registers");

    // Warm the file cache so the comparison is of decode work, not of first-read I/O.
    let _ = ctx
        .sql("SELECT count(*) FROM t")
        .await
        .expect("plans")
        .collect()
        .await;

    let mut best = f64::MAX;
    let mut rows = 0i64;
    for _ in 0..runs {
        let start = Instant::now();
        let batches = ctx
            .sql(
                "SELECT count(*) AS n, sum(length(pad_a)) AS a, sum(length(pad_b)) AS b,
                        sum(length(pad_c)) AS c, sum(value) AS v
                 FROM t WHERE needle = 42",
            )
            .await
            .expect("plans")
            .collect()
            .await
            .expect("executes");
        best = best.min(start.elapsed().as_secs_f64());
        rows = batches[0]
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .expect("a count")
            .value(0);
    }
    (best, rows)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "writes a large file and runs timed queries"]
async fn pushdown_is_measurably_worth_asserting() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let path = dir.path().join("fixture.parquet");

    let rows = 5_000_000usize;
    let bytes = write_fixture(&path, rows);

    let (with, rows_with) = time_query(true, &path, 3).await;
    let (without, rows_without) = time_query(false, &path, 3).await;

    // Correctness first: the setting must not change the answer.
    assert_eq!(
        rows_with, rows_without,
        "pushdown changed the result, which would be far worse than it being slow"
    );
    assert_eq!(rows_with, (rows / 10_000) as i64);

    let speedup = without / with;
    eprintln!(
        "\npushdown measurement\n\
         --------------------\n\
         rows              {rows}\n\
         file              {:.1} MiB\n\
         matching rows     {rows_with} (1 in 10,000)\n\
         pushdown enabled  {with:.3}s\n\
         pushdown disabled {without:.3}s\n\
         speedup           {speedup:.2}x\n",
        bytes as f64 / 1024.0 / 1024.0
    );

    // Deliberately a weak assertion. The point of this test is the measurement; making
    // it a tight performance gate would leave it failing on a loaded machine for
    // reasons unrelated to the code.
    assert!(
        speedup > 1.0,
        "pushdown should not be slower ({with:.3}s enabled vs {without:.3}s disabled)"
    );
}

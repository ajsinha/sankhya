//! Measure what the pushdown setting is actually worth.
//!
//! # What this test is for now, which is not what it was for
//!
//! It was written to quantify a win: `settings.rs` pinned filter pushdown on, on the grounds
//! that leaving a large optimisation switched off has no symptom but slowness, and this file
//! was to put a number under that claim. The number came back neutral, TPC-H then showed it a
//! cost at every selectivity tried, and `session.rs` now leaves the setting at the engine's
//! default with the reasoning recorded there — statistics have already eliminated the rows
//! late materialisation would have saved decoding, so the cheaper mechanism has won before
//! pushdown is reached.
//!
//! So this is no longer evidence for a setting. It is a **standing check that the setting is
//! still a wash on this shape**, and the correctness assertion is the part that matters: a
//! setting that changed the answer would be far worse than one that failed to make it faster.
//!
//! Ignored by default: it writes a substantial Parquet fixture and runs timed queries.
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
use datafusion::prelude::{ParquetReadOptions, SessionConfig, SessionContext};
use sankhya_table::{write_parquet, WriterConfig};
use sankhya_types::Lsn;
use std::sync::Arc;
use std::time::Instant;

/// A wide-ish table: one selective key column and several payload columns.
///
/// The shape matters, and this shape is the wrong one — deliberately left in place, because
/// what it demonstrates is worth keeping. Pushdown pays when a predicate eliminates most
/// *pages*; this one eliminates 99.99% of rows and no pages at all, because `needle` is
/// `id % 10_000` over sequential ids and every page therefore spans the column's whole range.
///
/// The comment that used to sit here claimed this was "the ordinary shape of an analytical
/// table". It is not, and the claim went unchallenged for as long as the fixture was also
/// choosing its own encoding.
///
/// **Written through `sankhya_table::write_parquet`, not through an `ArrowWriter` this test
/// configures itself.** Pushdown is only as good as the page index, and the page index is
/// emitted by writer settings — page statistics and a page row limit. An earlier version of
/// this fixture chose those settings here, which measured pushdown against a file Sankhya
/// would never produce: it duplicated three of `WriterConfig`'s decisions and silently
/// dropped the rest, so a change to the product's layout could not have moved this number.
/// The measurement is worth having only if it is a measurement of what ships.
fn write_fixture(directory: &std::path::Path, rows: usize) -> u64 {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("needle", DataType::Int64, false),
        Field::new("pad_a", DataType::Utf8, false),
        Field::new("pad_b", DataType::Utf8, false),
        Field::new("pad_c", DataType::Utf8, false),
        Field::new("value", DataType::Float64, false),
    ]));

    // One row group per file: `WriterConfig`'s row-group size, so the fixture's layout is
    // decided by the product rather than by a number picked here.
    const CHUNK: usize = 1_000_000;
    let mut written = 0usize;
    let mut part = 0usize;
    while written < rows {
        let n = CHUNK.min(rows - written);
        let ids: Vec<i64> = (0..n).map(|i| (written + i) as i64).collect();
        // One row in ten thousand matches, so the predicate is genuinely selective.
        let needles: Vec<i64> = ids.iter().map(|i| i % 10_000).collect();
        // Incompressible padding. A repetitive string dictionary-encodes so well that
        // decoding it costs almost nothing, and avoiding a decode that is already free
        // saves nothing --- which is exactly the artefact the first version of this
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

        write_parquet(
            directory,
            &format!("part-{part:05}.parquet"),
            &batch,
            Lsn::new(part as u64 + 1),
            WriterConfig::default(),
        )
        .expect("the product's writer writes it");
        written += n;
        part += 1;
    }
    bytes_in(directory)
}

/// How many bytes the fixture occupies.
fn bytes_in(directory: &std::path::Path) -> u64 {
    std::fs::read_dir(directory)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.metadata().ok().map(|meta| meta.len()))
        .sum()
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
async fn pushdown_does_not_change_the_answer_and_is_not_a_win_on_this_shape() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let path = dir.path().join("fixture");

    let rows = 5_000_000usize;
    let bytes = write_fixture(&path, rows);

    let (with, rows_with) = time_query(true, &path, 3).await;
    let (without, rows_without) = time_query(false, &path, 3).await;

    // The claim that survives. Correctness is not a matter of machine load, and a setting
    // that changed the answer would be far worse than one that failed to make it faster.
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
         fixture           {:.1} MiB\n\
         matching rows     {rows_with} (1 in 10,000)\n\
         pushdown enabled  {with:.3}s\n\
         pushdown disabled {without:.3}s\n\
         speedup           {speedup:.2}x\n",
        bytes as f64 / 1024.0 / 1024.0
    );

    // The claim that did not survive, and why the assertion is no longer `speedup > 1.0`.
    //
    // This test used to assert pushdown was faster, and passed --- against a fixture it wrote
    // itself with writer settings it chose itself. Routed through `sankhya_table::write_parquet`
    // the margin collapsed into noise: repeated runs give 1.06x, 0.94x, 0.96x. The old number
    // was the same coin flip wearing a fixture that flattered it.
    //
    // The shape is the reason, and it contradicts this file's own opening comment. `needle` is
    // `id % 10_000` over sequential ids, so every 20,000-row page spans the full range of the
    // column: no page can be pruned by statistics, and no page of payload can be skipped by a
    // row selection either, because each one holds about two matching rows. Decoding is per
    // page. A predicate that eliminates 99.99% of *rows* while eliminating no *pages* saves
    // nothing, and this fixture eliminates no pages by construction.
    //
    // So the honest assertion is the weaker one: enabling pushdown must not cost anything
    // material. Asserting a win here would be asserting a fixture, which is what this test was
    // doing before.
    //
    // `session.rs` reaches the same conclusion from better evidence and says where the setting
    // would look different: data with no useful bounds, decided per query from the statistics
    // rather than pinned on for everyone. That is task #77, and it is a question about the
    // planner, not about finding a fixture that flatters the setting.
    assert!(
        speedup > 0.8,
        "pushdown is materially slower on this shape ({with:.3}s enabled vs {without:.3}s \
         disabled); it is expected to be a wash, not a cost"
    );
}

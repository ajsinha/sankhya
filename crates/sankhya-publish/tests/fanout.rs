//! The guards that keep a partitioned write from becoming ten thousand tiny files.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use arrow_array::{Date32Array, Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use sankhya_publish::{Accumulator, FanOut, Publication};
use sankhya_types::Lsn;
use std::path::Path;
use std::sync::Arc;

const FIRST_DAY: i32 = 19_723;

fn schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("event_date", DataType::Date32, false),
        Field::new("payload", DataType::Utf8, false),
    ]))
}

/// `rows` rows spread evenly across `days` partitions — the shape that produced the defect.
fn spread(from: i64, rows: usize, days: i64) -> RecordBatch {
    let ids: Vec<i64> = (0..rows as i64).map(|i| from + i).collect();
    let dates: Vec<i32> = ids
        .iter()
        .map(|i| FIRST_DAY + i32::try_from(i.rem_euclid(days)).unwrap_or(0))
        .collect();
    let payloads: Vec<String> = ids.iter().map(|i| format!("{i:048x}")).collect();
    RecordBatch::try_new(
        schema(),
        vec![
            Arc::new(Int64Array::from(ids)),
            Arc::new(Date32Array::from(dates)),
            Arc::new(StringArray::from(payloads)),
        ],
    )
    .expect("well-formed")
}

fn parquet_files(root: &Path) -> usize {
    fn walk(dir: &Path, count: &mut usize) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, count);
            } else if path.extension().is_some_and(|e| e == "parquet") {
                *count += 1;
            }
        }
    }
    let mut count = 0;
    walk(root, &mut count);
    count
}

// --- the defect, reproduced ---------------------------------------------

#[test]
fn an_unguarded_append_writes_one_tiny_file_per_partition() {
    // FR-CDC-14's forbidden shape, and what `append` does on its own. Stated as a test so
    // the guarded path below is measured against it rather than against a claim.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("events");
    let publication = Publication::external(&root, "events").dated_by("event_date");
    publication.create(&schema()).expect("created");

    publication
        .append(1, "part-00001.parquet", &spread(0, 5_000, 90), Lsn::new(1))
        .expect("published");

    assert_eq!(
        parquet_files(&root),
        90,
        "one file per partition, each holding about fifty-five rows"
    );
}

// --- the guards ----------------------------------------------------------

#[test]
fn a_partition_too_small_to_be_worth_a_file_waits() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("events");
    let publication = Publication::external(&root, "events").dated_by("event_date");
    publication.create(&schema()).expect("created");

    let mut accumulator = Accumulator::new(&publication, FanOut::default());
    let published = accumulator
        .absorb("part-00001.parquet", &spread(0, 5_000, 90), Lsn::new(1))
        .expect("absorbed");

    assert!(published.is_empty(), "{published:#?}");
    assert_eq!(parquet_files(&root), 0, "nothing was written");
    assert_eq!(accumulator.pending(), 90, "every partition is waiting");
    assert!(accumulator.strain().deferrals > 0);
}

#[test]
fn waiting_ends_in_a_flush_and_each_partition_becomes_one_file() {
    // The bulk-load path: feed everything, then flush. Rows for one partition arriving in
    // five batches become one file, not five.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("events");
    let publication = Publication::external(&root, "events").dated_by("event_date");
    publication.create(&schema()).expect("created");

    let mut accumulator = Accumulator::new(&publication, FanOut::default());
    for round in 0..5_i64 {
        accumulator
            .absorb("part-00001.parquet", &spread(round * 5_000, 5_000, 90), Lsn::new(1))
            .expect("absorbed");
    }
    let published = accumulator
        .flush("part-00001.parquet", Lsn::new(1))
        .expect("flushed");

    assert_eq!(published.len(), 90, "one file per partition");
    assert_eq!(parquet_files(&root), 90);
    assert_eq!(accumulator.pending(), 0);

    // 25,000 rows in 90 files rather than in 450.
    let rows: usize = published.iter().map(|p| p.rows).sum();
    assert_eq!(rows, 25_000);
}

#[test]
fn a_partition_that_has_waited_long_enough_is_written_however_small() {
    // Waiting forever is not deferral, it is loss. A partition receiving one row a day must
    // still become readable within a bounded time.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("events");
    let publication = Publication::external(&root, "events").dated_by("event_date");
    publication.create(&schema()).expect("created");

    let impatient = FanOut {
        max_deferred_batches: 3,
        ..FanOut::default()
    };
    let mut accumulator = Accumulator::new(&publication, impatient);
    let mut written = 0;
    for round in 0..3_i64 {
        written += accumulator
            .absorb("part-00001.parquet", &spread(round * 10, 10, 2), Lsn::new(1))
            .expect("absorbed")
            .len();
    }

    assert!(written > 0, "nothing was ever written despite the deferral limit");
    assert!(accumulator.strain().aged_out > 0, "{:?}", accumulator.strain());
}

#[test]
fn one_commit_writes_no_more_partitions_than_the_cap_allows() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("events");
    let publication = Publication::external(&root, "events").dated_by("event_date");
    publication.create(&schema()).expect("created");

    // Everything is ready at once: no minimum, so only the cap holds it back.
    let capped = FanOut {
        min_file_bytes: 0,
        max_partitions_per_commit: 8,
        ..FanOut::default()
    };
    let mut accumulator = Accumulator::new(&publication, capped);
    let published = accumulator
        .absorb("part-00001.parquet", &spread(0, 5_000, 90), Lsn::new(1))
        .expect("absorbed");

    assert_eq!(published.len(), 8, "the cap held: {}", published.len());
    assert_eq!(accumulator.pending(), 82, "the rest deferred");
    assert!(accumulator.strain().capped > 0);
}

// --- the alarm, which §6.4.2 says is the important one -------------------

#[test]
fn sustained_fan_out_is_reported_rather_than_absorbed() {
    // "The guards buy time; the alarm gets the design fixed. Silently absorbing it would be
    // the failure." A batch touching ninety partitions every time is a partition scheme
    // finer than the arrival pattern, and no amount of deferral fixes that.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("events");
    let publication = Publication::external(&root, "events").dated_by("event_date");
    publication.create(&schema()).expect("created");

    let fan_out = FanOut::default();
    let mut accumulator = Accumulator::new(&publication, fan_out);
    for round in 0..10_i64 {
        accumulator
            .absorb("part-00001.parquet", &spread(round * 100, 100, 90), Lsn::new(1))
            .expect("absorbed");
    }

    let strain = accumulator.strain();
    assert_eq!(strain.widest_batch, 90);
    assert!(strain.wants_attention(&fan_out), "{strain:?}");
    let explanation = strain.explain(&fan_out).expect("an explanation");
    assert!(explanation.contains("partition scheme"), "{explanation}");
}

#[test]
fn a_narrow_workload_raises_no_alarm() {
    // A lint that fires on healthy behaviour gets ignored, and then the real one is too.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("events");
    let publication = Publication::external(&root, "events").dated_by("event_date");
    publication.create(&schema()).expect("created");

    let fan_out = FanOut::default();
    let mut accumulator = Accumulator::new(&publication, fan_out);
    for round in 0..10_i64 {
        accumulator
            .absorb("part-00001.parquet", &spread(round * 100, 100, 2), Lsn::new(1))
            .expect("absorbed");
    }
    assert!(!accumulator.strain().wants_attention(&fan_out));
    assert!(accumulator.strain().explain(&fan_out).is_none());
}

#[test]
fn no_batches_means_no_fan_out_figure_rather_than_a_fan_out_of_zero() {
    // No evidence is not evidence of health.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("events");
    let publication = Publication::external(&root, "events").dated_by("event_date");
    publication.create(&schema()).expect("created");
    let accumulator = Accumulator::new(&publication, FanOut::default());
    assert_eq!(accumulator.strain().average_fan_out(), None);
}

// --- the log stays contiguous, whatever the deferral pattern ------------

/// The versions present in a table's log, in order.
fn log_versions(root: &Path) -> Vec<u64> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root.join("_delta_log")) else {
        return out;
    };
    for entry in entries.flatten() {
        if let Some(stem) = entry.path().file_stem().and_then(|s| s.to_str()) {
            if let Ok(version) = stem.parse::<u64>() {
                out.push(version);
            }
        }
    }
    out.sort_unstable();
    out
}

#[test]
fn deferral_does_not_open_a_gap_in_the_commit_log() {
    // The defect this test exists for, and it reached a running soak before any test saw it.
    //
    // The accumulator used to take a version from the caller. The first caller advanced it
    // once per batch absorbed — reasonable, and wrong, because most batches are deferred.
    // Sixty-three versions passed with no commit and the flush asked for version 64 against
    // a log holding none. The log refused it: "committing version 64 would leave a gap; the
    // next version is 1."
    //
    // Every existing test passed, because each used a single version.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("events");
    let publication = Publication::external(&root, "events").dated_by("event_date");
    publication.create(&schema()).expect("created");

    let mut accumulator = Accumulator::new(&publication, FanOut::default());
    for round in 0..40_i64 {
        accumulator
            .absorb("part.parquet", &spread(round * 100, 100, 90), Lsn::new(1))
            .expect("absorbed");
    }
    accumulator.flush("part.parquet", Lsn::new(1)).expect("flushed");

    let versions = log_versions(&root);
    assert!(versions.len() >= 2, "nothing was committed: {versions:?}");
    for (expected, found) in versions.iter().enumerate() {
        assert_eq!(
            *found, expected as u64,
            "the log has a gap: {versions:?}"
        );
    }
}

#[test]
fn many_flushes_each_take_the_next_version() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("events");
    let publication = Publication::external(&root, "events").dated_by("event_date");
    publication.create(&schema()).expect("created");

    let mut accumulator = Accumulator::new(&publication, FanOut::default());
    for round in 0..4_i64 {
        accumulator
            .absorb("part.parquet", &spread(round * 100, 100, 2), Lsn::new(1))
            .expect("absorbed");
        accumulator.flush("part.parquet", Lsn::new(1)).expect("flushed");
    }
    assert_eq!(log_versions(&root), vec![0, 1, 2, 3, 4], "one commit per flush, in order");
}

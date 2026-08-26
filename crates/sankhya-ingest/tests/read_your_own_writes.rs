//! The demonstration that makes the unified-system claim credible.
//!
//! Write a row through the transactional interface, then immediately query it
//! analytically — and see it. Without this, the first thing anyone tries shows the row
//! missing, and they reasonably conclude the system is broken.
//!
//! The client waits for **capture**, not for publication. That distinction is what
//! allows the commit cadence to be tuned for storage efficiency while a reader still
//! sees its own write promptly.
//!
//! Skipped unless `SANKHYA_PG_BIN` and `SANKHYA_E2E_SOCKET` are set.

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

use datafusion::prelude::{ParquetReadOptions, SessionContext};
use sankhya_cdc_apply::BatchPolicy;
use sankhya_ingest::Pipeline;
use sankhya_plan::{evaluate_visibility, ReadMode, SessionToken, Visibility};
use sankhya_table::WriterConfig;
use sankhya_types::Lsn;
use std::process::Command;
use std::time::Instant;

const MARKER: i64 = 980_000_000;

struct Pg {
    bin: String,
    socket: String,
}

impl Pg {
    fn from_env() -> Option<Self> {
        Some(Self {
            bin: std::env::var("SANKHYA_PG_BIN").ok()?,
            socket: std::env::var("SANKHYA_E2E_SOCKET").ok()?,
        })
    }

    fn sql(&self, statement: &str) -> String {
        let out = Command::new(format!("{}/psql", self.bin))
            .args([
                "-h",
                &self.socket,
                "-U",
                "sankhya",
                "-d",
                "postgres",
                "-tA",
                "-c",
                statement,
            ])
            .output()
            .expect("psql runs");
        assert!(
            out.status.success(),
            "psql failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn drain(&self, slot: &str) -> Vec<Vec<u8>> {
        let hex = self.sql(&format!(
            "SELECT encode(data,'hex') FROM pg_logical_slot_get_binary_changes(
                '{slot}', NULL, NULL, 'proto_version','4','publication_names','sankhya_all')"
        ));
        hex.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                (0..l.len())
                    .step_by(2)
                    .filter_map(|i| u8::from_str_radix(l.get(i..i + 2)?, 16).ok())
                    .collect()
            })
            .collect()
    }

    /// Parse the textual position form the server reports.
    fn lsn(&self, statement: &str) -> Lsn {
        Lsn::parse(&self.sql(statement)).expect("a position")
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_write_is_visible_analytically_to_the_session_that_made_it() {
    let Some(pg) = Pg::from_env() else {
        eprintln!("skipping: set SANKHYA_PG_BIN and SANKHYA_E2E_SOCKET to run");
        return;
    };

    let table = "sensor_calibrations";
    let slot = "sankhya_ryow";

    pg.sql(&format!("DELETE FROM {table} WHERE id > {MARKER}"));
    pg.sql(&format!(
        "SELECT pg_drop_replication_slot('{slot}') WHERE EXISTS
         (SELECT 1 FROM pg_replication_slots WHERE slot_name='{slot}')"
    ));
    pg.sql(&format!(
        "SELECT pg_create_logical_replication_slot('{slot}','pgoutput')"
    ));

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

    // --- the write, and the token it returns -------------------------------------
    pg.sql(&format!(
        "INSERT INTO {table} (id, device_id, offset_value, technician, notes, calibrated_at, calibrated_date)
         VALUES ({}, 'device-ryow', 0.000042, 'tech-1', 'written just now', now(), current_date)",
        MARKER + 1
    ));
    // A real client receives this from the write itself; here it is read back, which is
    // the same position.
    let token = SessionToken::at(pg.lsn("SELECT pg_current_wal_lsn()"));

    // --- before capture, the session must be told to wait -------------------------
    let before = evaluate_visibility(ReadMode::ReadYourWrites, Some(token), Lsn::ZERO)
        .expect("waiting is not an error");
    assert!(
        matches!(before, Visibility::Wait { .. }),
        "with nothing captured the session must wait, not read stale data: {before:?}"
    );

    // --- capture, waiting only as long as the deadline allows ---------------------
    let deadline = std::time::Duration::from_secs(10);
    let started = Instant::now();
    let mut applied;

    loop {
        for bytes in &pg.drain(slot) {
            pipeline.accept_bytes(bytes).expect("accepts");
        }
        pipeline.publish(true).expect("publishes");
        applied = pipeline.stats().applied_through;

        match evaluate_visibility(ReadMode::ReadYourWrites, Some(token), applied)
            .expect("waiting is not an error")
        {
            Visibility::Ready { .. } => break,
            Visibility::Wait { .. } => {
                assert!(
                    started.elapsed() < deadline,
                    "capture did not reach {token} within the deadline; it reached {applied}"
                );
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
    }
    let waited = started.elapsed();

    // --- and now the write is analytically visible --------------------------------
    let published = warehouse.path().join(format!("public/{table}"));
    let ctx = SessionContext::new();
    ctx.register_parquet(
        "captured",
        published.to_string_lossy().as_ref(),
        ParquetReadOptions::default(),
    )
    .await
    .expect("registers");

    // Asserted through SQL rather than by downcasting the result, because the engine
    // is free to choose a physical string representation and the test should not
    // depend on which one it picked.
    for (predicate, why) in [
        (
            "device_id = 'device-ryow'",
            "the session must see the row it just wrote",
        ),
        (
            "notes = 'written just now'",
            "and must see its actual value, not a placeholder",
        ),
        (
            "_sankhya_op = 'I'",
            "recorded with the operation that produced it",
        ),
    ] {
        let batches = ctx
            .sql(&format!(
                "SELECT count(*) AS n FROM captured WHERE {predicate}"
            ))
            .await
            .expect("plans")
            .collect()
            .await
            .expect("executes");
        let n = batches[0]
            .column(0)
            .as_any()
            .downcast_ref::<datafusion::arrow::array::Int64Array>()
            .expect("a count")
            .value(0);
        assert_eq!(n, 1, "{why} (predicate: {predicate})");
    }

    pg.sql(&format!("SELECT pg_drop_replication_slot('{slot}')"));
    pg.sql(&format!("DELETE FROM {table} WHERE id > {MARKER}"));

    eprintln!(
        "read-your-own-writes: wrote at {token}, capture reached {applied} after {:.0}ms, \
         and the analytical query returned the row",
        waited.as_secs_f64() * 1000.0
    );
}

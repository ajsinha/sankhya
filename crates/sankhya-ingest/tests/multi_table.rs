//! Continuous multi-table capture against the real dataset.
//!
//! Runs a workload across several tables in interleaved transactions, feeds the whole
//! stream through one pipeline, and checks that every table lands correctly and
//! independently — then queries the result and reconciles against the source.
//!
//! The interleaving is the point. A pipeline that batched globally, or that let rows
//! from one table reach another's batch, passes a single-table test and fails here.
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
use sankhya_table::WriterConfig;
use std::process::Command;

const MARKER: i64 = 960_000_000;

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
            "psql failed for {statement:?}: {}",
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
}

#[tokio::test(flavor = "multi_thread")]
async fn several_tables_capture_independently_and_reconcile() {
    let Some(pg) = Pg::from_env() else {
        eprintln!("skipping: set SANKHYA_PG_BIN and SANKHYA_E2E_SOCKET to run");
        return;
    };

    let tables = [
        "device_readings",
        "route_legs",
        "shipment_scans",
        "access_events",
    ];
    let per_table = 2_000i64;
    let slot = "sankhya_multi";

    for table in tables {
        pg.sql(&format!("DELETE FROM {table} WHERE id > {MARKER}"));
    }
    pg.sql(&format!(
        "SELECT pg_drop_replication_slot('{slot}') WHERE EXISTS
         (SELECT 1 FROM pg_replication_slots WHERE slot_name='{slot}')"
    ));
    pg.sql(&format!(
        "SELECT pg_create_logical_replication_slot('{slot}','pgoutput')"
    ));

    for round in 0..4i64 {
        pg.sql(&format!(
            "BEGIN;
             INSERT INTO device_readings (id, device_id, metric, value, quality, observed_at, observed_date)
               SELECT {MARKER} + {round} * 500 + g, 'd-' || g, 'm', g::float8, 50, now(), current_date
               FROM generate_series(1, 500) g;
             INSERT INTO route_legs (id, route_ref, from_hub, to_hub, distance_km, departed_at, departed_date)
               SELECT {MARKER} + {round} * 500 + g, 'r-' || g, 'h1', 'h2', (g::numeric/100)::numeric(9,2), now(), current_date
               FROM generate_series(1, 500) g;
             INSERT INTO shipment_scans (id, consignment_ref, hub_code, status, weight_kg, scanned_at, scan_date)
               SELECT {MARKER} + {round} * 500 + g, 'c-' || g, 'h1', 'ok', (g::numeric/10)::numeric(10,3), now(), current_date
               FROM generate_series(1, 500) g;
             INSERT INTO access_events (id, principal_ref, resource, action, allowed, context, occurred_at, occurred_date)
               SELECT {MARKER} + {round} * 500 + g, 'p-' || g, '/r/' || g, 'read', true,
                      ('{{\"seq\":' || g || '}}')::jsonb, now(), current_date
               FROM generate_series(1, 500) g;
             COMMIT;"
        ));
    }

    // Logical decoding reads flushed WAL; make sure everything has reached disk.
    pg.sql("SELECT pg_current_wal_flush_lsn()");

    let messages = pg.drain(slot);
    assert!(!messages.is_empty(), "the stream produced nothing");

    let warehouse = tempfile::tempdir().expect("a temporary directory");
    let mut pipeline = Pipeline::new(
        warehouse.path(),
        // Publish once at the end, so the test can check the table-to-file mapping
        // exactly rather than across many partial files.
        BatchPolicy {
            max_rows: usize::MAX,
            max_transactions: usize::MAX,
            ..BatchPolicy::default()
        },
        WriterConfig::default(),
    );

    for bytes in &messages {
        pipeline
            .accept_bytes(bytes)
            .expect("the pipeline accepts every message");
    }

    assert_eq!(
        pipeline.open_rows(),
        0,
        "every transaction in the stream was committed"
    );
    let published = pipeline.publish(true).expect("publishes");

    let stats = pipeline.stats().clone();
    assert_eq!(
        stats.unresolvable, 0,
        "no mutation should have been unresolvable"
    );
    assert!(
        stats.tables_onboarded >= tables.len(),
        "every table should have onboarded"
    );
    assert_eq!(
        stats.rows_captured as i64,
        per_table * tables.len() as i64,
        "every inserted row should be captured exactly once"
    );
    assert!(
        stats.applied_through.get() > 0,
        "the pipeline must declare coverage"
    );

    for table in tables {
        let file = published
            .iter()
            .find(|f| f.table == table)
            .unwrap_or_else(|| panic!("{table} should have published a file"));
        assert_eq!(file.rows as i64, per_table, "{table} row count");
        assert!(
            file.path
                .to_string_lossy()
                .contains(&format!("public/{table}")),
            "{table} should land at its mirrored path, got {}",
            file.path.display()
        );
        assert!(
            file.covers_through.get() > 0,
            "{table} must declare coverage"
        );
    }

    let ctx = SessionContext::new();
    for table in tables {
        let file = published
            .iter()
            .find(|f| f.table == table)
            .expect("published");
        ctx.register_parquet(
            table,
            file.path.to_string_lossy().as_ref(),
            ParquetReadOptions::default(),
        )
        .await
        .expect("registers");

        let result = ctx
            .sql(&format!("SELECT count(*) AS n FROM {table}"))
            .await
            .expect("plans")
            .collect()
            .await
            .expect("executes");
        let n = result[0]
            .column(0)
            .as_any()
            .downcast_ref::<datafusion::arrow::array::Int64Array>()
            .expect("a count")
            .value(0);

        let source: i64 = pg
            .sql(&format!("SELECT count(*) FROM {table} WHERE id > {MARKER}"))
            .parse()
            .expect("a count");
        assert_eq!(
            n, source,
            "{table}: analytical count disagrees with the source"
        );
        assert_eq!(n, per_table);
    }

    pg.sql(&format!("SELECT pg_drop_replication_slot('{slot}')"));
    for table in tables {
        pg.sql(&format!("DELETE FROM {table} WHERE id > {MARKER}"));
    }

    eprintln!(
        "multi-table: {} messages -> {} tables -> {} rows -> {} files, {} bytes; \
         every table reconciles against the source",
        stats.messages_decoded,
        stats.tables_onboarded,
        stats.rows_captured,
        stats.files_published,
        stats.bytes_published
    );
}

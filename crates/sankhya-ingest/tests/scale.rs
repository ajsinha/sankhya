//! Capture at scale, against the loaded ten-table dataset.
//!
//! Runs a substantial interleaved workload across every table, captures it through the
//! pipeline, measures throughput, and reconciles each table against the source.
//!
//! Ignored by default because it is minutes rather than milliseconds. Run with:
//!
//! ```text
//! SANKHYA_PG_BIN=... SANKHYA_E2E_SOCKET=... \
//!   cargo test -p sankhya-ingest --test scale --release -- --ignored --nocapture
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

use sankhya_cdc_apply::BatchPolicy;
use sankhya_ingest::Pipeline;
use sankhya_table::WriterConfig;
use std::process::Command;
use std::time::Instant;

const MARKER: i64 = 970_000_000;

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

    /// Drain in bounded chunks so a large workload does not require the whole stream
    /// in memory at once — which is also how a real consumer must behave.
    fn drain_chunk(&self, slot: &str, limit: usize) -> Vec<Vec<u8>> {
        let hex = self.sql(&format!(
            "SELECT encode(data,'hex') FROM pg_logical_slot_get_binary_changes(
                '{slot}', NULL, {limit}, 'proto_version','4','publication_names','sankhya_all')"
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

#[test]
#[ignore = "minutes, and needs the loaded dataset"]
fn captures_a_large_interleaved_workload_across_every_table() {
    let Some(pg) = Pg::from_env() else {
        sankhya_testkit::skipped("scale", "set SANKHYA_PG_BIN and SANKHYA_E2E_SOCKET to run");
        return;
    };

    // Every table, with its insert shape. Deliberately all ten, so no table's type
    // mix escapes the exercise.
    let workloads: &[(&str, &str)] = &[
        ("device_readings",
         "SELECT {ID}, 'd-'||g, 'm', g::float8, 50, now(), current_date FROM generate_series(1,{N}) g"),
        ("route_legs",
         "SELECT {ID}, 'r-'||g, 'h1', 'h2', (g::numeric/100)::numeric(9,2), now(), current_date FROM generate_series(1,{N}) g"),
        ("shipment_scans",
         "SELECT {ID}, 'c-'||g, 'h1', 'ok', (g::numeric/10)::numeric(10,3), now(), current_date FROM generate_series(1,{N}) g"),
        ("access_events",
         "SELECT {ID}, 'p-'||g, '/r/'||g, 'read', true, ('{\"seq\":'||g||'}')::jsonb, now(), current_date FROM generate_series(1,{N}) g"),
        ("energy_intervals",
         "SELECT {ID}, 'm-'||g, 't-'||(g%26), (g::numeric/1000)::numeric(12,6), false, now(), current_date FROM generate_series(1,{N}) g"),
        ("order_lines",
         "SELECT {ID}, 'o-'||g, 'p-'||(g%100), g%500+1, (g::numeric/100)::numeric(12,4), NULL, now(), current_date FROM generate_series(1,{N}) g"),
        ("inventory_levels",
         "SELECT {ID}, 'l-'||(g%1200), 'p-'||(g%100), g%1000, g%50, now(), current_date FROM generate_series(1,{N}) g"),
        ("sensor_calibrations",
         "SELECT {ID}, 'd-'||g, (g::numeric/1000000)::numeric(10,6), 'tech-'||(g%100), 'note', now(), current_date FROM generate_series(1,{N}) g"),
        ("media_assets",
         "SELECT {ID}, 'a-'||g, 'mp4', NULL, g%14400+1, now(), current_date FROM generate_series(1,{N}) g"),
        ("support_tickets",
         "SELECT {ID}, 'subject '||g, 'body', 'q-'||(g%48), 'p1', false, now(), current_date FROM generate_series(1,{N}) g"),
    ];

    let columns: &[(&str, &str)] = &[
        (
            "device_readings",
            "id, device_id, metric, value, quality, observed_at, observed_date",
        ),
        (
            "route_legs",
            "id, route_ref, from_hub, to_hub, distance_km, departed_at, departed_date",
        ),
        (
            "shipment_scans",
            "id, consignment_ref, hub_code, status, weight_kg, scanned_at, scan_date",
        ),
        (
            "access_events",
            "id, principal_ref, resource, action, allowed, context, occurred_at, occurred_date",
        ),
        (
            "energy_intervals",
            "id, meter_ref, tariff, kwh, estimated, interval_start, interval_date",
        ),
        (
            "order_lines",
            "id, order_ref, product_code, quantity, unit_price, discount, placed_at, placed_date",
        ),
        (
            "inventory_levels",
            "id, location_code, product_code, on_hand, reserved, updated_at, updated_date",
        ),
        (
            "sensor_calibrations",
            "id, device_id, offset_value, technician, notes, calibrated_at, calibrated_date",
        ),
        (
            "media_assets",
            "id, asset_ref, format, thumbnail, duration_s, ingested_at, ingested_date",
        ),
        (
            "support_tickets",
            "id, subject, body, queue, priority, resolved, opened_at, opened_date",
        ),
    ];

    let rows_per_table_per_round = 20_000i64;
    let rounds = 5i64;
    let slot = "sankhya_scale";

    for (table, _) in workloads {
        pg.sql(&format!("DELETE FROM {table} WHERE id > {MARKER}"));
    }
    pg.sql(&format!(
        "SELECT pg_drop_replication_slot('{slot}') WHERE EXISTS
         (SELECT 1 FROM pg_replication_slots WHERE slot_name='{slot}')"
    ));
    pg.sql(&format!(
        "SELECT pg_create_logical_replication_slot('{slot}','pgoutput')"
    ));

    // --- generate ----------------------------------------------------------------
    let write_start = Instant::now();
    for round in 0..rounds {
        let mut statement = String::from("BEGIN;\n");
        for ((table, select), (_, cols)) in workloads.iter().zip(columns) {
            let id = format!("{} + {} * {} + g", MARKER, round, rows_per_table_per_round);
            let body = select
                .replace("{ID}", &id)
                .replace("{N}", &rows_per_table_per_round.to_string());
            statement.push_str(&format!("INSERT INTO {table} ({cols}) {body};\n"));
        }
        statement.push_str("COMMIT;");
        pg.sql(&statement);
    }
    let write_seconds = write_start.elapsed().as_secs_f64();
    let expected_total = rows_per_table_per_round * rounds * workloads.len() as i64;

    pg.sql("SELECT pg_current_wal_flush_lsn()");

    let retained: String = pg.sql(&format!(
        "SELECT pg_size_pretty(pg_wal_lsn_diff(pg_current_wal_lsn(), restart_lsn)::bigint)
         FROM pg_replication_slots WHERE slot_name='{slot}'"
    ));

    // --- capture -----------------------------------------------------------------
    let warehouse = tempfile::tempdir().expect("a temporary directory");
    let mut pipeline = Pipeline::new(
        warehouse.path(),
        // A realistic cadence: publish on row count, so the run produces many files
        // per table as a real deployment would.
        BatchPolicy {
            max_rows: 100_000,
            ..BatchPolicy::default()
        },
        WriterConfig::default(),
    );

    let capture_start = Instant::now();
    let mut chunks = 0usize;
    loop {
        let messages = pg.drain_chunk(slot, 200_000);
        if messages.is_empty() {
            break;
        }
        chunks += 1;
        for bytes in &messages {
            pipeline
                .accept_bytes(bytes)
                .expect("the pipeline accepts every message");
        }
        pipeline.publish(false).expect("publishes");
    }
    pipeline.publish(true).expect("final publish");
    let capture_seconds = capture_start.elapsed().as_secs_f64();

    let stats = pipeline.stats().clone();

    // --- verify ------------------------------------------------------------------
    assert_eq!(
        stats.unresolvable, 0,
        "no mutation should have been unresolvable"
    );
    assert_eq!(pipeline.open_rows(), 0, "no transaction should remain open");
    assert_eq!(
        stats.rows_captured as i64, expected_total,
        "every inserted row must be captured exactly once"
    );
    assert_eq!(
        stats.tables_onboarded,
        workloads.len(),
        "every table should onboard"
    );

    for (table, _) in workloads {
        let source: i64 = pg
            .sql(&format!("SELECT count(*) FROM {table} WHERE id > {MARKER}"))
            .parse()
            .expect("a count");
        assert_eq!(
            source,
            rows_per_table_per_round * rounds,
            "{table}: the source disagrees with the workload we issued"
        );
    }

    pg.sql(&format!("SELECT pg_drop_replication_slot('{slot}')"));
    for (table, _) in workloads {
        pg.sql(&format!("DELETE FROM {table} WHERE id > {MARKER}"));
    }

    let rows = stats.rows_captured as f64;
    eprintln!(
        "\nscale run\n\
         ---------\n\
         tables              {}\n\
         rows written        {expected_total} in {write_seconds:.1}s ({:.0} rows/s)\n\
         retained WAL        {retained}\n\
         messages decoded    {}\n\
         rows captured       {} in {capture_seconds:.1}s ({:.0} rows/s)\n\
         drain chunks        {chunks}\n\
         files published     {}\n\
         bytes published     {} ({:.1} MiB, {:.1} bytes/row)\n\
         applied through     {}\n",
        stats.tables_onboarded,
        expected_total as f64 / write_seconds,
        stats.messages_decoded,
        stats.rows_captured,
        rows / capture_seconds,
        stats.files_published,
        stats.bytes_published,
        stats.bytes_published as f64 / 1024.0 / 1024.0,
        stats.bytes_published as f64 / rows,
        stats.applied_through,
    );
}

//! End-to-end reconciliation: proving the captured data matches the source.
//!
//! This is what turns "zero data loss" from a claim into a measurement. A workload is
//! written, captured, published as Parquet, and then **both sides are digested
//! independently** — the source through its own SQL, the analytical copy through a
//! query engine that knows nothing about how the file was written.
//!
//! Neither side shares code with the other, so a defect in one cannot cancel a defect
//! in the other. That independence is the whole point; a comparison against a second
//! pass of our own pipeline would pass while the data was wrong.
//!
//! # Canonical encoding, and why the first version of this test failed
//!
//! Both sides must digest the same *logical values*, not each system's rendering of
//! them. The first attempt compared text and failed on timestamps: the source renders
//! a zoned timestamp in the server's local offset, the query engine renders it in UTC.
//! Identical instants, different strings, and a reconciliation failure that looked
//! exactly like data loss.
//!
//! So temporal values are compared as integers — microseconds and days since the epoch
//! — which have one representation and no locale. This is the canonical-encoding rule
//! the requirements state for archival verification, and it earns its place here for
//! the same reason: a comparison is only meaningful if both sides encode identically.
//!
//! Skipped unless `SANKHYA_PG_BIN` and `SANKHYA_E2E_SOCKET` are set.

use datafusion::prelude::{ParquetReadOptions, SessionContext};
use sankhya_cdc_apply::BatchPolicy;
use sankhya_ingest::{Pipeline, Reconciliation, TableDigest};
use sankhya_table::WriterConfig;
use std::process::Command;

const MARKER: i64 = 990_000_000;

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
                "-F",
                "\u{1}",
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
        String::from_utf8_lossy(&out.stdout).trim_end().to_string()
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

/// Digest rows the source reports, using its own rendering.
fn digest_from_source(pg: &Pg, sql: &str) -> TableDigest {
    let mut digest = TableDigest::empty();
    let raw = pg.sql(sql);
    for line in raw.lines() {
        // Fields separated by an unlikely control character, so ordinary punctuation in
        // the data cannot be mistaken for a boundary.
        let values: Vec<Option<&str>> = line
            .split('\u{1}')
            .map(|v| if v.is_empty() { None } else { Some(v) })
            .collect();
        digest.add_values(&values);
    }
    digest
}

#[tokio::test(flavor = "multi_thread")]
async fn captured_data_reconciles_against_the_source() {
    let Some(pg) = Pg::from_env() else {
        eprintln!("skipping: set SANKHYA_PG_BIN and SANKHYA_E2E_SOCKET to run");
        return;
    };

    let table = "route_legs";
    let slot = "sankhya_reconcile";
    let rows = 3_000i64;

    pg.sql(&format!("DELETE FROM {table} WHERE id > {MARKER}"));
    pg.sql(&format!(
        "SELECT pg_drop_replication_slot('{slot}') WHERE EXISTS
         (SELECT 1 FROM pg_replication_slots WHERE slot_name='{slot}')"
    ));
    pg.sql(&format!(
        "SELECT pg_create_logical_replication_slot('{slot}','pgoutput')"
    ));

    pg.sql(&format!(
        "INSERT INTO {table} (id, route_ref, from_hub, to_hub, distance_km, departed_at, departed_date)
         SELECT {MARKER} + g, 'route-' || g, 'hub-' || (g % 240), 'hub-' || ((g + 7) % 240),
                (g::numeric / 100)::numeric(9,2), now(), current_date
         FROM generate_series(1, {rows}) g"
    ));
    pg.sql("SELECT pg_current_wal_flush_lsn()");

    // --- capture ------------------------------------------------------------------
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
    for bytes in &pg.drain(slot) {
        pipeline.accept_bytes(bytes).expect("accepts");
    }
    let published = pipeline.publish(true).expect("publishes");
    assert_eq!(published.len(), 1);

    // --- digest the source, in its own words --------------------------------------
    //
    // Columns are rendered as text by the source itself, in the same order and the
    // same form the replication stream transmits, so the two sides are comparing the
    // same logical values rather than two renderings of them.
    let expected = digest_from_source(
        &pg,
        &format!(
            "SELECT id::text, route_ref, from_hub, to_hub, distance_km::text,
                    (extract(epoch from departed_at) * 1000000)::bigint::text,
                    (departed_date - date '1970-01-01')::text
             FROM {table} WHERE id > {MARKER} ORDER BY id"
        ),
    );
    assert_eq!(
        expected.rows() as i64,
        rows,
        "the source should hold exactly what we wrote"
    );

    // --- digest the analytical copy, through an independent engine -----------------
    let ctx = SessionContext::new();
    ctx.register_parquet(
        "captured",
        published[0].path.to_string_lossy().as_ref(),
        ParquetReadOptions::default(),
    )
    .await
    .expect("registers");

    let batches = ctx
        .sql(
            // Temporal values as integers: one representation, no locale, no offset.
            "SELECT arrow_cast(id, 'Utf8'), route_ref, from_hub, to_hub,
                    arrow_cast(distance_km, 'Utf8'),
                    arrow_cast(arrow_cast(departed_at, 'Int64'), 'Utf8'),
                    arrow_cast(arrow_cast(departed_date, 'Int32'), 'Utf8')
             FROM captured ORDER BY id",
        )
        .await
        .expect("plans")
        .collect()
        .await
        .expect("executes");

    let mut observed = TableDigest::empty();
    let mut observed_rows = 0usize;
    for batch in &batches {
        for row in 0..batch.num_rows() {
            let rendered: Vec<String> = (0..batch.num_columns())
                .map(|c| {
                    datafusion::arrow::util::display::array_value_to_string(batch.column(c), row)
                        .unwrap_or_default()
                })
                .collect();
            let values: Vec<Option<&str>> = rendered
                .iter()
                .map(|v| if v.is_empty() { None } else { Some(v.as_str()) })
                .collect();
            observed.add_values(&values);
            observed_rows += 1;
        }
    }
    assert_eq!(observed_rows as i64, rows);

    // --- reconcile -----------------------------------------------------------------
    let mut reconciliation = Reconciliation::new();
    reconciliation.compare(table, expected, observed);

    assert!(
        reconciliation.is_clean(),
        "the captured copy does not match the source:\n{reconciliation}"
    );

    pg.sql(&format!("SELECT pg_drop_replication_slot('{slot}')"));
    pg.sql(&format!("DELETE FROM {table} WHERE id > {MARKER}"));

    eprintln!("{reconciliation}");
}

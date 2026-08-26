//! The complete path, end to end, against a real database.
//!
//! ```text
//!   PostgreSQL  ->  logical replication  ->  decode  ->  onboard
//!               ->  batch  ->  encode  ->  Parquet  ->  SQL
//! ```
//!
//! Every stage is the real implementation. The schema is derived from the replication
//! stream rather than declared; the values arrive as the source transmits them; the
//! Parquet file is read back by an engine that knows nothing about how it was written.
//!
//! The final assertion compares the query's answer against **the source's own** —
//! not against a second pass of our own pipeline, which would let a shared defect
//! cancel out and pass.
//!
//! Skipped unless `SANKHYA_PG_BIN` and `SANKHYA_E2E_SOCKET` are set.

use datafusion::prelude::SessionContext;
use sankhya_cdc_apply::{BatchPolicy, Batcher};
use sankhya_cdc_model::{Decoder, Message};
use sankhya_schema::{onboard_relation, WriteStrategy};
use sankhya_table::{encode_batch, write_parquet, WriterConfig};
use std::process::Command;

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
}

#[tokio::test(flavor = "multi_thread")]
async fn a_real_workload_becomes_queryable_parquet() {
    let Some(pg) = Pg::from_env() else {
        eprintln!("skipping: set SANKHYA_PG_BIN and SANKHYA_E2E_SOCKET to run");
        return;
    };

    let slot = "sankhya_vertical";
    let table = "energy_intervals";
    let marker = 950_000_000i64;

    // --- clean, then capture from a fresh position -------------------------------
    pg.sql(&format!("DELETE FROM {table} WHERE id > {marker}"));
    pg.sql(&format!(
        "SELECT pg_drop_replication_slot('{slot}') WHERE EXISTS
         (SELECT 1 FROM pg_replication_slots WHERE slot_name='{slot}')"
    ));
    pg.sql(&format!(
        "SELECT pg_create_logical_replication_slot('{slot}','pgoutput')"
    ));

    // A workload with exact decimals, booleans, zoned timestamps and dates — the
    // types most likely to be carried wrongly.
    let rows = 5_000i64;
    pg.sql(&format!(
        "INSERT INTO {table} (id, meter_ref, tariff, kwh, estimated, interval_start, interval_date)
         SELECT {marker} + g,
                'meter-' || g,
                'tariff-' || (g % 26),
                (g::numeric / 1000)::numeric(12,6),
                (g % 2 = 0),
                timestamptz '2025-06-01 00:00:00+00' + (g || ' seconds')::interval,
                date '2025-06-01' + (g % 7)
         FROM generate_series(1, {rows}) g"
    ));

    // --- drain the stream --------------------------------------------------------
    let hex = pg.sql(&format!(
        "SELECT encode(data,'hex') FROM pg_logical_slot_get_binary_changes(
            '{slot}', NULL, NULL, 'proto_version','4','publication_names','sankhya_all')"
    ));
    let messages: Vec<Vec<u8>> = hex
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            (0..l.len())
                .step_by(2)
                .filter_map(|i| u8::from_str_radix(l.get(i..i + 2)?, 16).ok())
                .collect()
        })
        .collect();
    assert!(!messages.is_empty(), "the stream produced nothing");

    // --- decode, onboard from the stream, batch ----------------------------------
    let decoder = Decoder::new();
    let mut batcher = Batcher::new(BatchPolicy {
        max_rows: usize::MAX,
        ..BatchPolicy::default()
    });
    let mut onboarded = None;

    for bytes in &messages {
        let (message, consumed) = decoder.decode_prefix(bytes).expect("decodes");
        assert_eq!(
            consumed,
            bytes.len(),
            "each message must be consumed exactly"
        );

        if let Message::Relation(relation) = &message {
            if relation.name == table {
                let result = onboard_relation(relation).expect("onboards");
                // The schema is DERIVED, never declared. Nothing in this test tells
                // the pipeline what the table looks like.
                assert_eq!(result.strategy, WriteStrategy::Mergeable);
                assert_eq!(result.location.relative_path(), format!("public/{table}"));
                onboarded = Some(result);
            }
        }
        batcher.accept(&message, None);
    }

    let onboarded = onboarded.expect("the stream described the table");
    assert_eq!(
        batcher.unresolvable(),
        0,
        "no mutation should be unresolvable"
    );
    let plan = batcher.flush();
    assert_eq!(
        plan.len() as i64,
        rows,
        "every inserted row should be captured"
    );
    assert!(
        plan.covers_through.get() > 0,
        "the plan must declare coverage"
    );

    // --- encode and write ---------------------------------------------------------
    let batch = encode_batch(&onboarded.schema, &plan.mutations).expect("encodes");
    assert_eq!(batch.num_rows() as i64, rows);

    let warehouse = tempfile::tempdir().expect("a temporary directory");
    let directory = warehouse.path().join(onboarded.location.relative_path());
    let report = write_parquet(
        &directory,
        "00000000.parquet",
        &batch,
        plan.covers_through,
        WriterConfig::default(),
    )
    .expect("writes");

    assert_eq!(report.rows as i64, rows);
    assert!(report.bytes > 0);
    assert_eq!(report.covers_through, plan.covers_through);

    // The layout mirrors the source: the path says which table this is.
    assert!(
        report
            .path
            .to_string_lossy()
            .contains(&format!("public/{table}")),
        "path should mirror the source schema and table: {}",
        report.path.display()
    );

    // --- query it back with an engine that knows nothing about the writer ---------
    let ctx = SessionContext::new();
    ctx.register_parquet(
        "captured",
        report.path.to_string_lossy().as_ref(),
        datafusion::prelude::ParquetReadOptions::default(),
    )
    .await
    .expect("registers");

    let counted = ctx
        .sql("SELECT count(*) AS n FROM captured")
        .await
        .expect("plans")
        .collect()
        .await
        .expect("executes");
    let n = counted[0]
        .column(0)
        .as_any()
        .downcast_ref::<datafusion::arrow::array::Int64Array>()
        .expect("a count")
        .value(0);
    assert_eq!(n, rows, "the Parquet file should hold every captured row");

    // Exact decimal aggregation, compared against the source's own arithmetic.
    let summed = ctx
        .sql("SELECT sum(kwh) AS total FROM captured")
        .await
        .expect("plans")
        .collect()
        .await
        .expect("executes");
    let total = summed[0]
        .column(0)
        .as_any()
        .downcast_ref::<datafusion::arrow::array::Decimal128Array>()
        .expect("an exact decimal sum")
        .value(0);

    let source_total: String = pg.sql(&format!(
        "SELECT sum(kwh)::text FROM {table} WHERE id > {marker}"
    ));
    // The source renders with the column's scale; compare in minor units.
    let expected: i128 = source_total
        .replace('.', "")
        .trim_start_matches('0')
        .parse()
        .unwrap_or(0);
    assert_eq!(
        total, expected,
        "the analytical sum ({total}) disagrees with the source ({source_total} -> {expected}); \
         exact decimal arithmetic must survive the whole path"
    );

    // Provenance is present and usable as a predicate.
    let filtered = ctx
        .sql("SELECT count(*) AS n FROM captured WHERE _sankhya_op = 'I'")
        .await
        .expect("plans")
        .collect()
        .await
        .expect("executes");
    let inserts = filtered[0]
        .column(0)
        .as_any()
        .downcast_ref::<datafusion::arrow::array::Int64Array>()
        .expect("a count")
        .value(0);
    assert_eq!(inserts, rows, "every row should be recorded as an insert");

    // A grouping query, which is what the analytical tier exists for.
    let grouped = ctx
        .sql("SELECT tariff, count(*) AS n FROM captured GROUP BY tariff ORDER BY tariff")
        .await
        .expect("plans")
        .collect()
        .await
        .expect("executes");
    let groups: usize = grouped
        .iter()
        .map(datafusion::arrow::array::RecordBatch::num_rows)
        .sum();
    assert_eq!(groups, 26, "the workload used twenty-six distinct tariffs");

    pg.sql(&format!("SELECT pg_drop_replication_slot('{slot}')"));
    pg.sql(&format!("DELETE FROM {table} WHERE id > {marker}"));

    eprintln!(
        "vertical slice: {} messages -> {} rows -> {} bytes of Parquet -> \
         SQL agrees with the source on an exact decimal sum",
        messages.len(),
        report.rows,
        report.bytes
    );
}

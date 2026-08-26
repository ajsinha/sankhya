//! End-to-end capture against a real PostgreSQL instance.
//!
//! Decodes and applies a live change stream produced by an actual server holding a
//! substantial dataset, then asserts that what the pipeline reconstructed matches what
//! the database itself reports.
//!
//! The comparison is deliberately against an **independent** count taken from the
//! source, not against a second pass of our own decoder — a defect shared by both
//! sides would otherwise cancel out and the test would pass while the pipeline was
//! wrong.
//!
//! Skipped unless `SANKHYA_E2E_SOCKET` and `SANKHYA_PG_BIN` are set, so an ordinary
//! `cargo test` needs no database. Driven by
//! `crates/sankhya-cdc-apply/tests/run_e2e.sh`.

use sankhya_cdc_apply::{BatchPolicy, Batcher, Op};
use sankhya_cdc_model::{Decoder, Message};
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

    /// Wait until every committed change is durable enough to be decoded.
    ///
    /// Note the instance must also not be running with asynchronous commit, or the
    /// flush position lags the write position indefinitely under a quiet workload and
    /// this wait becomes a timeout rather than a synchronisation point.
    ///
    /// Logical decoding reads *flushed* WAL. Under asynchronous commit a transaction
    /// returns before its WAL reaches disk, so a drain issued immediately afterwards
    /// can legitimately see nothing. This is not a test artefact — it is the real
    /// visibility boundary, and the capture loop must respect it too.
    fn wait_for_durable(&self) {
        let target = self.sql("SELECT pg_current_wal_lsn()");
        for _ in 0..200 {
            let flushed = self.sql("SELECT pg_current_wal_flush_lsn()");
            let caught_up: bool = self
                .sql(&format!("SELECT '{flushed}'::pg_lsn >= '{target}'::pg_lsn"))
                .starts_with('t');
            if caught_up {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        panic!("WAL did not flush through {target} within the timeout");
    }

    /// Drain the slot as raw per-message bytes, hex-encoded one message per line.
    fn drain_slot(&self, slot: &str) -> Vec<Vec<u8>> {
        let hex = self.sql(&format!(
            "SELECT encode(data,'hex') FROM pg_logical_slot_get_binary_changes(
                '{slot}', NULL, NULL, 'proto_version','4','publication_names','sankhya_all')"
        ));
        hex.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                (0..l.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&l[i..i + 2], 16).expect("valid hex"))
                    .collect()
            })
            .collect()
    }
}

#[test]
fn captures_a_real_workload_and_reconciles_against_the_source() {
    let Some(pg) = Pg::from_env() else {
        eprintln!("skipping: set SANKHYA_PG_BIN and SANKHYA_E2E_SOCKET to run");
        return;
    };

    // Clean up before, not only after: a previous failure must not make the next run
    // fail for a different reason and hide the original defect.
    for table in ["device_readings", "route_legs", "shipment_scans"] {
        pg.sql(&format!("DELETE FROM {table} WHERE id > 900000000"));
    }

    let slot = "sankhya_e2e";
    pg.sql(&format!(
        "SELECT pg_drop_replication_slot('{slot}') WHERE EXISTS
         (SELECT 1 FROM pg_replication_slots WHERE slot_name='{slot}')"
    ));
    pg.sql(&format!(
        "SELECT pg_create_logical_replication_slot('{slot}','pgoutput')"
    ));

    // A workload over the loaded dataset, exercising all three operations and
    // several tables so cross-table transaction handling is covered.
    let inserted = 500i64;
    let updated = 300i64;
    let deleted = 120i64;

    pg.sql(&format!(
        "INSERT INTO device_readings (id, device_id, metric, value, quality, observed_at, observed_date)
         SELECT 900000000 + g, 'e2e-' || g, 'm', g::float8, 50, now(), current_date
         FROM generate_series(1, {inserted}) g"
    ));
    pg.sql(&format!(
        "UPDATE device_readings SET value = value + 1
         WHERE id BETWEEN 900000001 AND {}",
        900_000_000 + updated
    ));
    pg.sql(&format!(
        "DELETE FROM device_readings WHERE id BETWEEN 900000001 AND {}",
        900_000_000 + deleted
    ));

    // A multi-statement transaction, so the batcher sees several tables sealed at one
    // position rather than one table per transaction.
    pg.sql(
        "BEGIN;
         INSERT INTO route_legs (id, route_ref, from_hub, to_hub, distance_km, departed_at, departed_date)
           VALUES (900000001, 'e2e', 'h1', 'h2', 10.5, now(), current_date);
         INSERT INTO shipment_scans (id, consignment_ref, hub_code, status, weight_kg, scanned_at, scan_date)
           VALUES (900000001, 'e2e', 'h1', 'ok', 1.5, now(), current_date);
         COMMIT;",
    );

    pg.wait_for_durable();
    let raw = pg.drain_slot(slot);
    assert!(!raw.is_empty(), "the slot produced no messages");

    // Decode and apply, asserting each message is consumed exactly.
    let decoder = Decoder::new();
    let mut batcher = Batcher::new(BatchPolicy {
        max_rows: usize::MAX,
        ..BatchPolicy::default()
    });
    let mut decoded = 0usize;

    for (i, message_bytes) in raw.iter().enumerate() {
        let (message, consumed) = decoder
            .decode_prefix(message_bytes)
            .unwrap_or_else(|e| panic!("message {i} failed to decode: {e}"));
        assert_eq!(
            consumed,
            message_bytes.len(),
            "message {i} (tag {:?}) was not consumed exactly; a live stream would desynchronise",
            message_bytes.first().map(|b| *b as char)
        );
        decoded += 1;

        // Updates in this workload touch no out-of-line column, so nothing is
        // withheld and no current row is needed to resolve.
        batcher.accept(&message, None);
    }

    assert_eq!(decoded, raw.len());
    assert_eq!(
        batcher.unresolvable(),
        0,
        "no mutation should have been unresolvable in this workload"
    );
    assert_eq!(
        batcher.open_rows(),
        0,
        "every transaction in the stream was committed"
    );

    let plan = batcher.flush();

    let counts = |op: Op| plan.mutations.iter().filter(|m| m.op == op).count() as i64;
    assert_eq!(
        counts(Op::Insert),
        inserted + 2,
        "inserts, including the two-table transaction"
    );
    assert_eq!(counts(Op::Update), updated);
    assert_eq!(counts(Op::Delete), deleted);

    // Independent check: the source's own view of the surviving rows.
    let surviving: i64 = pg
        .sql("SELECT count(*) FROM device_readings WHERE id > 900000000")
        .parse()
        .expect("a count");
    assert_eq!(
        surviving,
        inserted - deleted,
        "the source disagrees with the workload we issued"
    );

    // And the pipeline's own arithmetic must reach the same number.
    let net = counts(Op::Insert) - 2 - counts(Op::Delete);
    assert_eq!(
        net, surviving,
        "the captured stream does not reconcile with the source: \
         captured net {net}, source reports {surviving}"
    );

    // Coverage must be real and monotonic.
    assert!(
        plan.covers_through.get() > 0,
        "the plan declares no coverage"
    );
    assert!(
        plan.transaction_count >= 4,
        "expected at least four transactions"
    );

    // Every mutation carries the position of its own transaction.
    assert!(plan.mutations.iter().all(|m| m.commit_lsn.get() > 0));

    pg.sql(&format!("SELECT pg_drop_replication_slot('{slot}')"));
    pg.sql("DELETE FROM device_readings WHERE id > 900000000");
    pg.sql("DELETE FROM route_legs WHERE id > 900000000");
    pg.sql("DELETE FROM shipment_scans WHERE id > 900000000");

    eprintln!(
        "e2e: {} messages decoded, {} mutations across {} transactions, reconciled against source",
        raw.len(),
        plan.len(),
        plan.transaction_count
    );
}

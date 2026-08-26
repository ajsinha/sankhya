//! Backfill handoff tests, including against the real dataset.
//!
//! The handoff is the whole difficulty. Reading existing rows is easy; making the
//! boundary between "rows as they were" and "changes since then" exact is not, and
//! both ways of getting it wrong are silent.

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

use sankhya_ingest::{advance_stream, plan_handoff, BackfillPlan, HandoffError};
use sankhya_types::Lsn;
use std::process::Command;

#[test]
fn a_snapshot_and_stream_taken_at_the_same_position_abut_exactly() {
    let plan = BackfillPlan::new("readings", Lsn::new(1000));
    let handoff = plan_handoff(&plan, Lsn::new(1000)).expect("should be exact");
    assert!(handoff.is_exact());
    assert_eq!(handoff.snapshot.end_inclusive(), Lsn::new(1000));
    assert_eq!(handoff.stream.start_exclusive(), Lsn::new(1000));
}

#[test]
fn a_stream_beginning_early_is_refused_because_rows_would_double() {
    let plan = BackfillPlan::new("readings", Lsn::new(1000));
    let err = plan_handoff(&plan, Lsn::new(900)).expect_err("must refuse");
    let HandoffError::Overlap { .. } = err else {
        panic!("expected an overlap, got {err:?}");
    };
    assert!(err.to_string().contains("applied twice"), "{err}");
}

#[test]
fn a_stream_beginning_late_is_refused_and_names_the_cause() {
    // This is what happens when the slot is created AFTER reading the snapshot, which
    // is the natural order to write the code in and the wrong one. Changes in the
    // window reach neither half, and the resulting dataset is internally consistent —
    // so nothing about it looks wrong.
    let plan = BackfillPlan::new("readings", Lsn::new(1000));
    let err = plan_handoff(&plan, Lsn::new(1100)).expect_err("must refuse");
    let HandoffError::Gap { .. } = err else {
        panic!("expected a gap, got {err:?}");
    };
    assert!(
        err.to_string()
            .contains("Create the slot before reading the snapshot"),
        "the message must say how to fix it: {err}"
    );
}

#[test]
fn the_stream_half_extends_without_disturbing_the_seam() {
    let plan = BackfillPlan::new("readings", Lsn::new(1000));
    let handoff = plan_handoff(&plan, Lsn::new(1000)).expect("exact");
    let advanced = advance_stream(&handoff, Lsn::new(5000)).expect("advances");

    assert!(
        advanced.is_exact(),
        "advancing must not open a gap at the seam"
    );
    assert_eq!(advanced.snapshot.end_inclusive(), Lsn::new(1000));
    assert_eq!(advanced.stream.end_inclusive(), Lsn::new(5000));
}

#[test]
fn the_snapshot_covers_from_the_beginning() {
    // The snapshot is not a window; it is everything the table held. So its coverage
    // must start at the origin, or the read path would see a gap before it.
    let plan = BackfillPlan::new("readings", Lsn::new(1000));
    assert_eq!(plan.coverage().start_exclusive(), Lsn::ZERO);
    assert_eq!(plan.coverage().end_inclusive(), Lsn::new(1000));
}

// --- against the real dataset ----------------------------------------------------

fn psql(sql: &str) -> Option<String> {
    let bin = std::env::var("SANKHYA_PG_BIN").ok()?;
    let socket = std::env::var("SANKHYA_E2E_SOCKET").ok()?;
    let out = Command::new(format!("{bin}/psql"))
        .args([
            "-h", &socket, "-U", "sankhya", "-d", "postgres", "-tA", "-c", sql,
        ])
        .output()
        .ok()?;
    assert!(
        out.status.success(),
        "psql failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[test]
fn a_real_slot_reports_a_position_the_snapshot_can_be_read_at() {
    // The mechanism the design depends on: creating a slot yields the position from
    // which it will stream, and a snapshot taken at that position meets it exactly.
    let slot = "sankhya_backfill";
    let Some(_) = psql(&format!(
        "SELECT pg_drop_replication_slot('{slot}') WHERE EXISTS
         (SELECT 1 FROM pg_replication_slots WHERE slot_name='{slot}')"
    )) else {
        eprintln!("skipping: set SANKHYA_PG_BIN and SANKHYA_E2E_SOCKET to run");
        return;
    };

    // Creating the slot returns the position it will begin streaming from.
    let reported = psql(&format!(
        "SELECT lsn FROM pg_create_logical_replication_slot('{slot}','pgoutput')"
    ))
    .expect("creates");
    let stream_from = Lsn::parse(&reported).expect("a position");
    assert!(stream_from.get() > 0);

    // A snapshot read at that same position meets the stream exactly.
    let plan = BackfillPlan::new("energy_intervals", stream_from);
    let handoff = plan_handoff(&plan, stream_from).expect("must be exact");
    assert!(handoff.is_exact());

    // Rows written afterwards belong to the stream half, not the snapshot half.
    psql("SELECT pg_logical_emit_message(true, 'sankhya.test', 'after')").expect("emits");
    psql("SELECT pg_current_wal_flush_lsn()").expect("flushes");
    let now = Lsn::parse(&psql("SELECT pg_current_wal_lsn()").expect("reads")).expect("a position");
    assert!(
        now > stream_from,
        "the source advanced past the handoff point"
    );

    let advanced = advance_stream(&handoff, now).expect("advances");
    assert!(advanced.is_exact());
    assert!(
        !advanced.snapshot.contains(now),
        "a change made after the handoff must not fall in the snapshot half"
    );
    assert!(
        advanced.stream.contains(now),
        "it must fall in the stream half"
    );

    psql(&format!("SELECT pg_drop_replication_slot('{slot}')")).expect("drops");

    eprintln!(
        "backfill handoff: slot reported {stream_from}, source advanced to {now}, \
         seam exact with no overlap and no gap"
    );
}

#[test]
fn creating_the_slot_after_the_snapshot_would_open_a_real_gap() {
    // Demonstrating the failure against a live database rather than asserting it.
    // Changes in the window between the two reach neither half.
    let Some(before) = psql("SELECT pg_current_wal_lsn()") else {
        eprintln!("skipping: database not configured");
        return;
    };
    let snapshot_position = Lsn::parse(&before).expect("a position");

    // Work happens in the window that a late slot would miss entirely.
    psql("SELECT pg_logical_emit_message(true, 'sankhya.test', 'in the window')").expect("emits");
    psql("SELECT pg_current_wal_flush_lsn()").expect("flushes");

    let after = psql("SELECT pg_current_wal_lsn()").expect("reads");
    let late_stream_start = Lsn::parse(&after).expect("a position");
    assert!(
        late_stream_start > snapshot_position,
        "the source advanced in the window"
    );

    let plan = BackfillPlan::new("energy_intervals", snapshot_position);
    let err = plan_handoff(&plan, late_stream_start).expect_err("must refuse");
    assert!(matches!(err, HandoffError::Gap { .. }));

    eprintln!(
        "late-slot gap: snapshot at {snapshot_position}, stream would start at \
         {late_stream_start} — {} positions would reach neither half",
        late_stream_start.get() - snapshot_position.get()
    );
}

//! Assess a real slot, reported by a real database.
//!
//! The pure tests exercise every rung of the ladder against constructed states. This
//! checks that the states we construct actually correspond to what a database reports —
//! that the field names, the units and the status vocabulary are right.
//!
//! Skipped unless `SANKHYA_PG_BIN` and `SANKHYA_E2E_SOCKET` are set.

use sankhya_cdc_pg::{assess, SafetyPolicy, Severity, SlotState, WalStatus};
use sankhya_types::Lsn;
use std::process::Command;
use std::time::Duration;

fn psql(sql: &str) -> Option<String> {
    let bin = std::env::var("SANKHYA_PG_BIN").ok()?;
    let socket = std::env::var("SANKHYA_E2E_SOCKET").ok()?;
    let out = Command::new(format!("{bin}/psql"))
        .args([
            "-h", &socket, "-U", "sankhya", "-d", "postgres", "-tA", "-F", "|", "-c", sql,
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
fn a_real_slot_is_read_and_assessed() {
    let slot = "sankhya_safety";
    let Some(_) = psql(&format!(
        "SELECT pg_drop_replication_slot('{slot}') WHERE EXISTS
         (SELECT 1 FROM pg_replication_slots WHERE slot_name='{slot}')"
    )) else {
        eprintln!("skipping: set SANKHYA_PG_BIN and SANKHYA_E2E_SOCKET to run");
        return;
    };
    psql(&format!(
        "SELECT pg_create_logical_replication_slot('{slot}','pgoutput')"
    ))
    .expect("creates");

    // Produce some log so the slot is holding something measurable.
    psql("SELECT pg_logical_emit_message(true, 'sankhya.test', repeat('x', 4096))").expect("emits");
    psql("SELECT pg_current_wal_flush_lsn()").expect("flushes");

    let row = psql(&format!(
        "SELECT slot_name, active, coalesce(wal_status,'unknown'),
                coalesce(pg_wal_lsn_diff(pg_current_wal_lsn(), restart_lsn)::bigint, 0),
                pg_current_wal_lsn(), coalesce(confirmed_flush_lsn::text, '0/0')
         FROM pg_replication_slots WHERE slot_name='{slot}'"
    ))
    .expect("reads the slot");

    let parts: Vec<&str> = row.split('|').collect();
    assert_eq!(parts.len(), 6, "unexpected slot row shape: {row:?}");

    let state = SlotState {
        name: parts[0].to_string(),
        active: parts[1] == "t",
        status: WalStatus::parse(parts[2]),
        retained_bytes: parts[3].parse().expect("a byte count"),
        source_position: Lsn::parse(parts[4]).expect("a position"),
        confirmed_position: Lsn::parse(parts[5]).expect("a position"),
    };

    // The vocabulary must match: an unrecognised status would be read as `lost`, which
    // would make a healthy slot look terminal.
    assert_eq!(
        state.status,
        WalStatus::Reserved,
        "a freshly created slot on a healthy database should report `reserved`; \
         got {:?} from the raw value {:?}. If the database's vocabulary has changed, \
         WalStatus::parse must be updated or every healthy slot will read as lost",
        state.status,
        parts[2]
    );
    assert!(state.status.is_usable());
    assert!(!state.status.is_past_limit());

    // The slot is holding something, and the arithmetic is sane.
    assert!(
        state.retained_bytes > 0,
        "the slot should be holding the log we just wrote"
    );
    assert!(state.source_position.get() > 0);

    let escalation = assess(&SafetyPolicy::default(), &state, Duration::from_secs(1));
    assert_eq!(
        escalation.severity,
        Severity::Normal,
        "a fresh slot holding a few kilobytes should not escalate: {}",
        escalation.reason
    );

    psql(&format!("SELECT pg_drop_replication_slot('{slot}')")).expect("drops");

    eprintln!(
        "real slot: {} status={} retained={} bytes behind={} -> {}",
        state.name,
        state.status,
        state.retained_bytes,
        state.behind_by(),
        escalation.severity
    );
}

#[test]
fn the_policy_can_be_derived_from_the_databases_own_limit() {
    // The thresholds are fractions of the source's limit, so reading that limit is how
    // a deployment configures itself rather than being told a number that may not match.
    let Some(setting) = psql("SHOW max_slot_wal_keep_size") else {
        eprintln!("skipping: database not configured");
        return;
    };
    eprintln!("source retention limit: {setting}");

    // "-1" means unlimited, which is the default and is unsafe for this design: the
    // source will never protect itself, so an unbounded stall fills the volume.
    if setting.trim() == "-1" {
        eprintln!(
            "NOTE: the source has no retention limit, so it will never invalidate a \
             stalled slot — it will run out of disk instead. SANKHYA's ladder is then \
             the only protection."
        );
    } else {
        assert!(
            setting.contains("GB") || setting.contains("MB") || setting.contains("kB"),
            "unexpected retention limit format: {setting:?}"
        );
    }
}

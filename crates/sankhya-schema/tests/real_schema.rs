//! Validate the type mapping against the real fixture database.
//!
//! The unit tests check the mapping against identifiers written by hand. This checks
//! it against what an actual server reports for the ten-table acceptance schema, so a
//! wrong constant or a misread type modifier is caught rather than agreed with.
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

use sankhya_schema::{map_source_type, LogicalType};
use std::process::Command;

fn query(sql: &str) -> Option<String> {
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
fn every_column_of_the_acceptance_schema_maps() {
    let Some(rows) = query(
        "SELECT c.relname, a.attname, a.atttypid, a.atttypmod, a.attnotnull
         FROM pg_attribute a
         JOIN pg_class c ON c.oid = a.attrelid
         JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE n.nspname = 'public' AND c.relkind = 'r'
           AND a.attnum > 0 AND NOT a.attisdropped
         ORDER BY c.relname, a.attnum",
    ) else {
        eprintln!("skipping: set SANKHYA_PG_BIN and SANKHYA_E2E_SOCKET to run");
        return;
    };

    let mut mapped = 0usize;
    let mut tables = std::collections::BTreeSet::new();
    let mut decimals = 0usize;
    let mut timestamps = 0usize;

    for line in rows.lines().filter(|l| !l.trim().is_empty()) {
        let parts: Vec<&str> = line.split('|').collect();
        assert!(parts.len() >= 5, "unexpected row shape: {line:?}");
        let (table, column) = (parts[0], parts[1]);
        let oid: u32 = parts[2].parse().expect("a type identifier");
        let modifier: i32 = parts[3].parse().expect("a type modifier");

        let result = map_source_type(oid, modifier);
        let m = result.unwrap_or_else(|e| {
            panic!("{table}.{column} (type {oid}, modifier {modifier}) failed to map: {e}")
        });
        assert!(m.lossless, "{table}.{column} must round-trip exactly");

        match m.logical {
            LogicalType::Decimal(p) => {
                decimals += 1;
                // The modifier must have been read correctly, not defaulted.
                assert!(
                    p.digits > 0 && p.digits <= 38,
                    "{table}.{column} precision {p:?}"
                );
                assert!(
                    p.scale <= p.digits,
                    "{table}.{column} scale exceeds precision"
                );
            }
            LogicalType::TimestampUtc => timestamps += 1,
            _ => {}
        }

        tables.insert(table.to_string());
        mapped += 1;
    }

    assert_eq!(
        tables.len(),
        10,
        "expected the ten acceptance tables, saw {tables:?}"
    );
    assert!(
        mapped >= 70,
        "expected at least seventy columns, mapped {mapped}"
    );
    assert!(decimals > 0, "the fixture set must exercise exact decimals");
    assert!(
        timestamps > 0,
        "the fixture set must exercise zoned timestamps"
    );

    eprintln!(
        "real schema: {mapped} columns across {} tables mapped losslessly \
         ({decimals} decimals, {timestamps} zoned timestamps)",
        tables.len()
    );
}

#[test]
fn declared_decimal_precision_matches_the_source() {
    // A misread type modifier would silently change every value's scale. This checks
    // the decoded precision against what the server itself reports in text.
    let Some(rows) = query(
        "SELECT c.relname || '.' || a.attname, a.atttypmod,
                format_type(a.atttypid, a.atttypmod)
         FROM pg_attribute a
         JOIN pg_class c ON c.oid = a.attrelid
         JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE n.nspname = 'public' AND c.relkind = 'r'
           AND a.atttypid = 1700 AND a.attnum > 0 AND NOT a.attisdropped",
    ) else {
        eprintln!("skipping: database not configured");
        return;
    };

    let mut checked = 0usize;
    for line in rows.lines().filter(|l| !l.trim().is_empty()) {
        let parts: Vec<&str> = line.split('|').collect();
        let (name, modifier, declared) = (parts[0], parts[1], parts[2]);
        let modifier: i32 = modifier.parse().expect("a modifier");

        let LogicalType::Decimal(p) = map_source_type(1700, modifier).expect("maps").logical else {
            panic!("{name} should map to a decimal");
        };
        // e.g. "numeric(12,4)"
        let expected = format!("numeric({},{})", p.digits, p.scale);
        assert_eq!(
            declared, expected,
            "{name}: decoded precision disagrees with the source's own rendering"
        );
        checked += 1;
    }
    assert!(checked > 0, "the fixture set must contain decimals");
    eprintln!("decimal precision: {checked} columns agree with the source");
}

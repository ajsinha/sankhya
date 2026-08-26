//! The generator's contract: reproducible, independent, and shaped as claimed.
//!
//! Reconciliation compares what arrives analytically against an *independent* model of
//! the truth. This generator is that model, so if it is not exactly reproducible the
//! whole zero-loss claim rests on nothing.

use sankhya_datagen::{all_schemas, schema_by_name, ColumnKind, Generator, Scale, WriteProfile};

#[test]
fn same_seed_reproduces_identical_rows() {
    let schema = schema_by_name("order_lines").expect("schema exists");
    let a = Generator::new(42).batch(schema, 2, 0, 256);
    let b = Generator::new(42).batch(schema, 2, 0, 256);
    assert_eq!(
        a.rows, b.rows,
        "the same seed must reproduce byte-identical rows"
    );
}

#[test]
fn different_seeds_diverge() {
    let schema = schema_by_name("order_lines").expect("schema exists");
    let a = Generator::new(1).batch(schema, 2, 0, 128);
    let b = Generator::new(2).batch(schema, 2, 0, 128);
    assert_ne!(
        a.rows, b.rows,
        "different seeds must produce different data"
    );
}

#[test]
fn rows_are_independently_reproducible() {
    // Any row can be regenerated without replaying the ones before it. This is what
    // makes a single row's expected value checkable during reconciliation, and it is
    // what allows generation to be parallelised.
    let schema = schema_by_name("device_readings").expect("schema exists");
    let generator = Generator::new(7);

    let whole = generator.batch(schema, 1, 0, 64);
    let middle = generator.batch(schema, 1, 40, 8);

    assert_eq!(
        &whole.rows[40..48],
        &middle.rows[..],
        "row values must not depend on batch boundaries"
    );
}

#[test]
fn tables_do_not_share_a_stream() {
    // Two tables at the same row offset must not produce correlated values, or a
    // defect that swaps tables would be invisible.
    let a = schema_by_name("route_legs").expect("schema exists");
    let b = schema_by_name("energy_intervals").expect("schema exists");
    let generator = Generator::new(9);
    let left = generator.batch(a, 0, 0, 32);
    let right = generator.batch(b, 1, 0, 32);
    // Compare the high-cardinality identifier column present in both.
    let l: Vec<_> = left.rows.iter().map(|r| r[1].clone()).collect();
    let r: Vec<_> = right.rows.iter().map(|r| r[1].clone()).collect();
    assert_ne!(l, r, "distinct tables must draw from distinct streams");
}

#[test]
fn nulls_appear_in_nullable_columns_only() {
    for schema in all_schemas() {
        let batch = Generator::new(11).batch(schema, 0, 0, 128);
        for row in &batch.rows {
            for (value, column) in row.iter().zip(schema.columns) {
                if value.is_none() {
                    assert!(
                        column.nullable,
                        "{}.{} produced a null but is not nullable",
                        schema.name, column.name
                    );
                }
            }
        }
    }
}

#[test]
fn nullable_columns_actually_produce_nulls() {
    // A generator that never emits a null would leave null handling untested while
    // appearing to cover it.
    let mut checked = 0;
    for schema in all_schemas() {
        let batch = Generator::new(13).batch(schema, 0, 0, 256);
        for (i, column) in schema.columns.iter().enumerate() {
            if !column.nullable {
                continue;
            }
            checked += 1;
            let nulls = batch.rows.iter().filter(|r| r[i].is_none()).count();
            assert!(
                nulls > 0,
                "{}.{} is nullable but produced no nulls in 256 rows",
                schema.name,
                column.name
            );
        }
    }
    assert!(checked > 0, "the fixture set must contain nullable columns");
}

#[test]
fn timestamps_increase_with_row_sequence() {
    // Arrival order equals natural sort order, which is what makes sorting by commit
    // position free rather than an extra pass.
    let schema = schema_by_name("access_events").expect("schema exists");
    let batch = Generator::new(5).batch(schema, 0, 0, 512);
    let ts: Vec<&String> = batch.rows.iter().filter_map(|r| r[6].as_ref()).collect();
    let mut sorted = ts.clone();
    sorted.sort();
    assert_eq!(
        ts, sorted,
        "timestamps must be non-decreasing with the row sequence"
    );
}

#[test]
fn large_payloads_resist_compression() {
    // The point of this column is to be stored out-of-line so the source withholds it
    // on an update. A compressible value stays inline and the case is never exercised.
    let schema = schema_by_name("media_assets").expect("schema exists");
    let batch = Generator::new(17).batch(schema, 0, 0, 8);
    let payload = batch.rows[0][3].as_ref().expect("row 0 is not a null row");
    assert!(
        payload.len() >= 8_000,
        "payload is {} bytes, too small to go out-of-line",
        payload.len()
    );

    // A crude compressibility check: a run of one repeated character would have a
    // tiny distinct-character count. Hex digits give sixteen.
    let distinct: std::collections::BTreeSet<char> = payload.chars().collect();
    assert!(
        distinct.len() >= 10,
        "payload uses only {} distinct characters and would compress inline",
        distinct.len()
    );
}

#[test]
fn ten_tables_span_all_write_profiles() {
    let schemas = all_schemas();
    assert_eq!(schemas.len(), 10);
    for profile in [
        WriteProfile::AppendOnly,
        WriteProfile::SlowlyChanging,
        WriteProfile::HotMutable,
    ] {
        assert!(
            schemas.iter().any(|s| s.profile == profile),
            "no fixture covers {profile:?}, so that storage strategy would be untested"
        );
    }
}

#[test]
fn no_schema_is_financial() {
    // The general-purpose claim is tested rather than asserted. If the only fixtures
    // were trading data, a core quietly shaped around one industry would still pass.
    let forbidden = [
        "trade",
        "counterparty",
        "portfolio",
        "position",
        "notional",
        "ledger",
        "settlement",
        "isin",
        "cusip",
        "desk",
        "book",
    ];
    for schema in all_schemas() {
        for word in forbidden {
            assert!(
                !schema.name.contains(word),
                "schema {} contains the domain word '{word}'",
                schema.name
            );
            for column in schema.columns {
                assert!(
                    !column.name.contains(word),
                    "{}.{} contains the domain word '{word}'",
                    schema.name,
                    column.name
                );
            }
        }
    }
}

#[test]
fn every_schema_has_a_partition_candidate() {
    // A date column derived from the timestamp is the natural partition key. Without
    // one the engine has nothing to prune on and every query is a full scan.
    for schema in all_schemas() {
        assert!(
            schema
                .columns
                .iter()
                .any(|c| matches!(c.kind, ColumnKind::Date)),
            "{} has no date column to partition on",
            schema.name
        );
    }
}

#[test]
fn the_acceptance_plan_is_sized_correctly() {
    let plan = Generator::new(1).plan(Scale::acceptance());
    assert_eq!(plan.len(), 10, "the acceptance run uses ten tables");

    let total: u64 = plan
        .iter()
        .map(|(s, rows)| s.approx_row_bytes() * rows)
        .sum();
    let target = Scale::acceptance().total_bytes;
    let ratio = total as f64 / target as f64;
    assert!(
        (0.9..=1.1).contains(&ratio),
        "planned {total} bytes against a {target} target (ratio {ratio:.3})"
    );

    // Sized by width rather than by row count, so a wide table does not dominate.
    let rows: Vec<u64> = plan.iter().map(|(_, r)| *r).collect();
    let widest = plan
        .iter()
        .map(|(s, _)| s.approx_row_bytes())
        .max()
        .unwrap_or(1);
    let narrowest = plan
        .iter()
        .map(|(s, _)| s.approx_row_bytes())
        .min()
        .unwrap_or(1);
    assert!(
        widest > narrowest,
        "the fixture set should vary in row width"
    );
    assert!(rows.iter().all(|r| *r > 0));
}

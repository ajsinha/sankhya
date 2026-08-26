//! Type-mapping tests.
//!
//! The refusals matter as much as the successes. Every rejected type here is one that
//! a more permissive mapping would have carried approximately — producing data that
//! looks correct, reconciles against nothing, and is discovered years later.

use arrow_schema::{DataType, TimeUnit};
use proptest::prelude::*;
use sankhya_schema::{
    map_source_type, numeric_modifier, Field, LogicalSchema, LogicalType, MappingError, Precision,
};

/// The identifiers used by the ten-table fixture set.
const BOOL: u32 = 16;
const INT8: u32 = 20;
const TEXT: u32 = 25;
const FLOAT8: u32 = 701;
const DATE: u32 = 1082;
const TIMESTAMPTZ: u32 = 1184;
const TIMESTAMP: u32 = 1114;
const NUMERIC: u32 = 1700;
const JSONB: u32 = 3802;
const MONEY: u32 = 790;
const INTERVAL: u32 = 1186;
const TIMETZ: u32 = 1266;

#[test]
fn every_type_in_the_fixture_set_maps() {
    // If any of these regressed, the ten-table acceptance load would stop onboarding.
    let cases: &[(u32, i32, LogicalType)] = &[
        (INT8, -1, LogicalType::Int64),
        (TEXT, -1, LogicalType::Utf8),
        (FLOAT8, -1, LogicalType::Float64),
        (BOOL, -1, LogicalType::Boolean),
        (DATE, -1, LogicalType::Date),
        (TIMESTAMPTZ, -1, LogicalType::TimestampUtc),
        (JSONB, -1, LogicalType::Json),
        (
            NUMERIC,
            numeric_modifier(12, 4),
            LogicalType::Decimal(Precision::new(12, 4).expect("valid")),
        ),
    ];
    for (oid, modifier, expected) in cases {
        let mapped = map_source_type(*oid, *modifier)
            .unwrap_or_else(|e| panic!("type {oid} should map: {e}"));
        assert_eq!(&mapped.logical, expected, "type {oid}");
        assert!(mapped.lossless, "type {oid} must round-trip exactly");
    }
}

#[test]
fn zoned_and_unzoned_timestamps_stay_distinct() {
    // Conflating them is how an entire column shifts by hours, silently.
    let zoned = map_source_type(TIMESTAMPTZ, -1).expect("maps").logical;
    let naive = map_source_type(TIMESTAMP, -1).expect("maps").logical;
    assert_ne!(zoned, naive);
    assert_eq!(
        zoned.arrow_type(),
        DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into()))
    );
    assert_eq!(
        naive.arrow_type(),
        DataType::Timestamp(TimeUnit::Microsecond, None)
    );
}

#[test]
fn unconstrained_decimal_is_refused_not_truncated() {
    // The single most dangerous mapping available. An unconstrained numeric truncated
    // to a fixed precision produces values that look right and reconcile against
    // nothing.
    let err = map_source_type(NUMERIC, -1).expect_err("must refuse");
    let MappingError::Unrepresentable { detail, .. } = err else {
        panic!("expected an unrepresentable error, got {err:?}");
    };
    assert!(detail.contains("arbitrary precision"), "{detail}");
    // The message must tell the operator what to do.
    assert!(
        detail.contains("numeric(18,4)") || detail.contains("Constrain"),
        "{detail}"
    );
}

#[test]
fn decimal_beyond_the_exact_range_is_refused() {
    let err = map_source_type(NUMERIC, numeric_modifier(50, 10)).expect_err("must refuse");
    assert!(matches!(err, MappingError::Unrepresentable { .. }));
}

#[test]
fn locale_dependent_and_ambiguous_types_are_refused_with_reasons() {
    for (oid, expect) in [
        (MONEY, "locale"),
        (INTERVAL, "not mutually convertible"),
        (TIMETZ, "cannot be resolved to an instant"),
    ] {
        let err = map_source_type(oid, -1).expect_err("must refuse");
        let MappingError::Unsupported { reason, .. } = err else {
            panic!("expected unsupported for {oid}, got {err:?}");
        };
        assert!(
            reason.contains(expect),
            "the refusal for {oid} should explain why: got {reason:?}"
        );
    }
}

#[test]
fn an_unknown_type_is_refused_with_guidance() {
    let err = map_source_type(999_999, -1).expect_err("must refuse");
    let MappingError::Unsupported { reason, .. } = err else {
        panic!("expected unsupported");
    };
    assert!(
        reason.contains("deliberately"),
        "the refusal should say how to proceed: {reason}"
    );
}

#[test]
fn floats_are_marked_inexact() {
    // Used to refuse an exact aggregate over an inexact column rather than producing a
    // number that cannot be reproduced.
    assert!(!LogicalType::Float64.is_exact());
    assert!(!LogicalType::Float32.is_exact());
    assert!(LogicalType::Decimal(Precision::new(10, 2).expect("valid")).is_exact());
    assert!(LogicalType::Int64.is_exact());
}

#[test]
fn the_physical_schema_carries_provenance() {
    let schema = LogicalSchema::new(vec![
        Field {
            name: "id".into(),
            logical: LogicalType::Int64,
            nullable: false,
            is_key: true,
        },
        Field {
            name: "label".into(),
            logical: LogicalType::Utf8,
            nullable: true,
            is_key: false,
        },
    ]);
    let arrow = schema.arrow_schema();
    let names: Vec<&str> = arrow.fields().iter().map(|f| f.name().as_str()).collect();
    assert_eq!(
        names,
        [
            "id",
            "label",
            "_sankhya_commit_lsn",
            "_sankhya_commit_ts",
            "_sankhya_op"
        ]
    );

    // Provenance columns are never null: a row without a position could not be
    // reconciled or replayed.
    for name in LogicalSchema::system_column_names() {
        let field = arrow.field_with_name(name).expect("present");
        assert!(!field.is_nullable(), "{name} must not be nullable");
    }
}

#[test]
fn a_source_column_shadowing_a_system_column_is_detected() {
    let schema = LogicalSchema::new(vec![Field {
        name: "_sankhya_op".into(),
        logical: LogicalType::Utf8,
        nullable: false,
        is_key: false,
    }]);
    assert_eq!(schema.collides_with_system_column(), Some("_sankhya_op"));
}

#[test]
fn identity_is_reported_accurately() {
    let keyed = LogicalSchema::new(vec![Field {
        name: "id".into(),
        logical: LogicalType::Int64,
        nullable: false,
        is_key: true,
    }]);
    let keyless = LogicalSchema::new(vec![Field {
        name: "v".into(),
        logical: LogicalType::Int64,
        nullable: false,
        is_key: false,
    }]);
    assert!(keyed.has_identity());
    assert!(
        !keyless.has_identity(),
        "a keyless table can be appended to but not updated"
    );
}

proptest! {
    /// Any decimal within the exact range maps, and preserves its precision.
    #[test]
    fn decimals_in_range_round_trip(digits in 1u8..=38, scale in 0u8..=38) {
        prop_assume!(scale <= digits);
        let mapped = map_source_type(NUMERIC, numeric_modifier(digits, scale));
        let Ok(m) = mapped else {
            prop_assert!(false, "numeric({digits},{scale}) should map");
            return Ok(());
        };
        let LogicalType::Decimal(p) = m.logical else {
            prop_assert!(false, "expected a decimal");
            return Ok(());
        };
        prop_assert_eq!(p.digits, digits);
        prop_assert_eq!(p.scale, scale);
    }

    /// Mapping never panics, whatever identifier or modifier arrives.
    ///
    /// These values come off a wire protocol, so hostile or corrupt input is expected.
    #[test]
    fn mapping_never_panics(oid in any::<u32>(), modifier in any::<i32>()) {
        let _ = map_source_type(oid, modifier);
    }

    /// A mapping that succeeds is always lossless. There is no third state.
    #[test]
    fn success_implies_lossless(oid in any::<u32>(), modifier in any::<i32>()) {
        if let Ok(m) = map_source_type(oid, modifier) {
            prop_assert!(m.lossless);
        }
    }
}

//! Schema-evolution tests.
//!
//! The refusals carry the weight. Every quarantined case here is one where a more
//! permissive engine would have applied a change whose intent it could not know, and
//! the consequence would surface much later with no way to reconstruct what was lost.

use sankhya_schema::{
    apply_compatible, classify_change, Compatibility, Field, LogicalSchema, LogicalType, Precision,
    SchemaChange,
};

fn field(name: &str, logical: LogicalType, nullable: bool, is_key: bool) -> Field {
    Field {
        name: name.into(),
        logical,
        nullable,
        is_key,
    }
}

fn schema(fields: Vec<Field>) -> LogicalSchema {
    LogicalSchema::new(fields)
}

fn base() -> LogicalSchema {
    schema(vec![
        field("id", LogicalType::Int64, false, true),
        field("label", LogicalType::Utf8, true, false),
    ])
}

fn dec(digits: u8, scale: u8) -> LogicalType {
    LogicalType::Decimal(Precision::new(digits, scale).expect("valid"))
}

#[test]
fn an_identical_shape_is_unchanged() {
    assert_eq!(classify_change(&base(), &base()), Compatibility::Unchanged);
}

#[test]
fn adding_a_column_is_applied_automatically() {
    // Unambiguous: rows written before it simply lack a value.
    let mut extended = base();
    extended
        .fields
        .push(field("added", LogicalType::Int32, true, false));

    let classification = classify_change(&base(), &extended);
    assert!(classification.may_continue());
    let Compatibility::Compatible { changes } = classification else {
        panic!("adding a column should be compatible");
    };
    assert_eq!(changes.len(), 1);
    assert!(matches!(changes[0], SchemaChange::ColumnAdded { .. }));
    assert_eq!(apply_compatible(&base(), &extended), Some(extended));
}

#[test]
fn widening_an_integer_is_applied_automatically() {
    let widened = schema(vec![
        field("id", LogicalType::Int64, false, true),
        field("label", LogicalType::Utf8, true, false),
    ]);
    let narrow = schema(vec![
        field("id", LogicalType::Int32, false, true),
        field("label", LogicalType::Utf8, true, false),
    ]);
    let classification = classify_change(&narrow, &widened);
    assert!(
        classification.may_continue(),
        "every 32-bit value fits in 64 bits"
    );
}

#[test]
fn narrowing_an_integer_quarantines() {
    // The values that fit look correct; the ones that did not are simply gone.
    let wide = base();
    let narrow = schema(vec![
        field("id", LogicalType::Int32, false, true),
        field("label", LogicalType::Utf8, true, false),
    ]);
    let classification = classify_change(&wide, &narrow);
    assert!(!classification.may_continue());
    let Compatibility::Incompatible { reason, .. } = classification else {
        panic!("narrowing must quarantine");
    };
    assert!(reason.contains("retyped"), "{reason}");
}

#[test]
fn a_decimal_gaining_digits_at_the_same_scale_is_compatible() {
    let before = schema(vec![field("amount", dec(9, 2), false, false)]);
    let after = schema(vec![field("amount", dec(18, 2), false, false)]);
    assert!(classify_change(&before, &after).may_continue());
}

#[test]
fn a_decimal_changing_scale_quarantines_even_when_it_grows() {
    // Changing the scale rescales every existing value. It looks like widening and is
    // not: the stored integers now mean something different.
    let before = schema(vec![field("amount", dec(9, 2), false, false)]);
    let after = schema(vec![field("amount", dec(18, 4), false, false)]);
    let classification = classify_change(&before, &after);
    assert!(
        !classification.may_continue(),
        "a scale change reinterprets every stored value and must not be automatic"
    );
}

#[test]
fn dropping_a_column_quarantines_because_intent_is_unknowable() {
    // "Stop capturing this" and "erase it from history" are both plausible readings,
    // and guessing wrong is either data loss or a compliance breach.
    let reduced = schema(vec![field("id", LogicalType::Int64, false, true)]);
    let classification = classify_change(&base(), &reduced);
    assert!(!classification.may_continue());
    let Compatibility::Incompatible { reason, changes } = classification else {
        panic!("dropping must quarantine");
    };
    assert!(changes
        .iter()
        .any(|c| matches!(c, SchemaChange::ColumnDropped { .. })));
    assert!(
        reason.contains("cannot be inferred"),
        "the message must explain why it refuses: {reason}"
    );
    assert!(
        reason.contains("remains queryable"),
        "and must say the table is not lost: {reason}"
    );
}

#[test]
fn a_rename_quarantines_because_it_looks_like_a_drop_and_an_add() {
    // Column identity is matched by name because the stream carries no stable column
    // identifier. That is exactly why a rename is indistinguishable from a
    // drop-and-add, and why treating it as either would silently discard or duplicate
    // a column.
    let renamed = schema(vec![
        field("id", LogicalType::Int64, false, true),
        field("caption", LogicalType::Utf8, true, false), // was `label`
    ]);
    let classification = classify_change(&base(), &renamed);
    assert!(!classification.may_continue());
    let Compatibility::Incompatible { changes, .. } = classification else {
        panic!("a rename must quarantine");
    };
    assert!(changes
        .iter()
        .any(|c| matches!(c, SchemaChange::ColumnDropped { .. })));
    assert!(changes
        .iter()
        .any(|c| matches!(c, SchemaChange::ColumnAdded { .. })));
}

#[test]
fn relaxing_nullability_is_compatible_but_tightening_is_not() {
    let relaxed = schema(vec![
        field("id", LogicalType::Int64, false, true),
        field("label", LogicalType::Utf8, true, false),
    ]);
    let strict = schema(vec![
        field("id", LogicalType::Int64, false, true),
        field("label", LogicalType::Utf8, false, false),
    ]);

    // Becoming nullable leaves every existing row valid.
    assert!(classify_change(&strict, &relaxed).may_continue());
    // Becoming mandatory may invalidate rows already written.
    assert!(!classify_change(&relaxed, &strict).may_continue());
}

#[test]
fn changing_row_identity_quarantines() {
    // Previously published rows may no longer be addressable, so merge behaviour would
    // silently change meaning.
    let rekeyed = schema(vec![
        field("id", LogicalType::Int64, false, false),
        field("label", LogicalType::Utf8, true, true),
    ]);
    let classification = classify_change(&base(), &rekeyed);
    assert!(!classification.may_continue());
    let Compatibility::Incompatible { changes, .. } = classification else {
        panic!("an identity change must quarantine");
    };
    assert!(changes.contains(&SchemaChange::IdentityChanged));
}

#[test]
fn an_incompatible_change_cannot_be_applied_by_omission() {
    // apply_compatible returns None rather than a best effort, so a caller that
    // forgets to check cannot proceed.
    let reduced = schema(vec![field("id", LogicalType::Int64, false, true)]);
    assert_eq!(apply_compatible(&base(), &reduced), None);
}

#[test]
fn several_compatible_changes_apply_together() {
    let mut evolved = base();
    evolved
        .fields
        .push(field("a", LogicalType::Int32, true, false));
    evolved
        .fields
        .push(field("b", LogicalType::Utf8, true, false));

    let Compatibility::Compatible { changes } = classify_change(&base(), &evolved) else {
        panic!("two additions should be compatible");
    };
    assert_eq!(changes.len(), 2);
}

#[test]
fn one_incompatible_change_among_compatible_ones_still_quarantines() {
    // A permissive engine might apply the safe half and quarantine the rest, leaving
    // the table in a shape that matches neither the old nor the new source.
    let mixed = schema(vec![
        field("id", LogicalType::Int64, false, true),
        // `label` dropped
        field("added", LogicalType::Int32, true, false),
    ]);
    assert!(!classify_change(&base(), &mixed).may_continue());
}

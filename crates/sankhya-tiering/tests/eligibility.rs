//! What makes a table eligible for tiering, decided before a policy exists.
//!
//! # The failure these exist to prevent
//!
//! Every rule here is checked at *policy creation*. `FR-TIER-10` is explicit about why: a type
//! that cannot round-trip must make a table ineligible **at policy creation, not at purge
//! time**. Discovering it at purge time means discovering it with a partition already detached,
//! a verification that cannot complete, and data that is neither in the source nor provably in
//! the archive.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_schema::{LogicalType, Precision};
use sankhya_tiering::policy::{
    canonical_encoding, Column, Contract, Ineligible, NotCanonical, Policy, Retention,
};

/// An immutable record table: append-only, partitioned on its date, nothing exotic in its
/// columns.
///
/// Named without reference to any industry, because `check-vocabulary` refuses a domain word in
/// a core crate --- which is the general-purpose claim being enforced rather than asserted. The
/// first draft of this file used a finance term throughout and the gate rejected all ten uses,
/// including the one in the comment explaining the rename.
fn records() -> Policy {
    Policy {
        name: "records-archive".to_string(),
        table: "entries".to_string(),
        tiering_key: "sank_data_date".to_string(),
        columns: vec![
            Column::new("sank_data_date", LogicalType::Date),
            Column::key("posting_id", LogicalType::Uuid),
            Column::new("account_ref", LogicalType::Int64),
            Column::new("amount", LogicalType::Decimal(Precision { digits: 38, scale: 9 })),
            Column::new("narrative", LogicalType::Utf8),
        ],
        contract: Contract::AppendOnly,
        range_partitioned: true,
        retention: Retention::new("7-year statutory record retention", 2557),
        publication_excludes_deletes: true,
        identifiers_vaulted: true,
    }
}

#[test]
fn a_table_with_no_declared_primary_key_cannot_be_verified_and_is_refused() {
    // `FR-TIER-09` verifies primary-key set equality before anything is purged. A table with no
    // key has no set, and the first draft of these rules admitted one --- the refusal would
    // have arrived at verification time, with the partition already marked.
    let mut policy = records();
    for column in &mut policy.columns {
        column.key = false;
    }
    let eligibility = policy.eligible();
    assert!(eligibility.refusals.contains(&Ineligible::NoPrimaryKey), "{eligibility}");
}

#[test]
fn a_tiering_key_whose_type_has_no_ordinal_is_refused() {
    // Purge is partition detach over a range and the registry records what it covered as
    // `[from, until)`. A key with no ordinal is a range whose coverage cannot be shown, and an
    // undetectable coverage gap is a query that quietly returns fewer rows than exist.
    let mut policy = records();
    policy.tiering_key = "posting_id".to_string();
    let eligibility = policy.eligible();
    assert!(
        eligibility
            .refusals
            .contains(&Ineligible::TieringKeyNotOrdinal { key: "posting_id".to_string() }),
        "{eligibility}"
    );
}

#[test]
fn a_publication_that_would_carry_a_delete_makes_a_table_ineligible() {
    // The second layer. The capture path replicates deletes, so a delete that reaches the
    // publication reaches the published tier --- and against an archived range that is the
    // archive being erased by the machinery meant to preserve it.
    let mut policy = records();
    policy.publication_excludes_deletes = false;
    assert!(
        policy.eligible().refusals.contains(&Ineligible::PublicationPropagatesDeletes),
        "{}",
        policy.eligible()
    );
}

#[test]
fn a_table_that_satisfies_every_rule_is_eligible() {
    let eligibility = records().eligible();
    assert!(eligibility.is_eligible(), "{eligibility}");
    assert_eq!(eligibility.to_string(), "eligible");
}

#[test]
fn every_reason_is_reported_rather_than_the_first() {
    // The property the planning command is built on: a table failing on several counts should
    // be fixable in one sitting. Reporting one per attempt is how somebody fixes the float
    // column, re-runs, learns about the mutable contract, and stops reading the output.
    let mut policy = records();
    policy.contract = Contract::Mutable;
    policy.range_partitioned = false;
    policy.identifiers_vaulted = false;
    policy.columns.push(Column::new("score", LogicalType::Float64));

    let eligibility = policy.eligible();

    assert!(!eligibility.is_eligible());
    assert!(eligibility.refusals.contains(&Ineligible::NotAppendOnly), "{eligibility}");
    assert!(
        eligibility
            .refusals
            .iter()
            .any(|r| matches!(r, Ineligible::NotRangePartitioned { .. })),
        "{eligibility}"
    );
    assert!(
        eligibility.refusals.contains(&Ineligible::IdentifiersNotVaulted),
        "{eligibility}"
    );
    assert!(
        eligibility
            .refusals
            .iter()
            .any(|r| matches!(r, Ineligible::UnarchivableColumn { column, .. } if column == "score")),
        "{eligibility}"
    );
    assert_eq!(eligibility.refusals.len(), 4, "four faults, four refusals: {eligibility}");
}

#[test]
fn a_mutable_table_is_refused() {
    // "Append-only by contract" is the eligibility constraint the whole design rests on. A row
    // updated in place after its partition was archived is a correction applying to data that
    // is no longer in the source.
    let mut policy = records();
    policy.contract = Contract::Mutable;

    let eligibility = policy.eligible();
    assert!(eligibility.refusals.contains(&Ineligible::NotAppendOnly));
    assert!(
        eligibility.to_string().contains("no longer there"),
        "the refusal explains the consequence rather than restating the rule: {eligibility}"
    );
}

#[test]
fn a_table_not_range_partitioned_on_the_key_is_refused() {
    let mut policy = records();
    policy.range_partitioned = false;

    let eligibility = policy.eligible();
    assert_eq!(
        eligibility.refusals,
        vec![Ineligible::NotRangePartitioned { key: "sank_data_date".to_string() }]
    );
}

#[test]
fn a_tiering_key_that_is_not_a_column_is_refused_separately_from_partitioning() {
    // Two different mistakes with two different fixes: one is a typo in the policy, the other
    // is a schema that has to change. Collapsing them would send somebody to alter a table
    // when they meant to correct a name.
    let mut policy = records();
    policy.tiering_key = "booking_date".to_string();

    let eligibility = policy.eligible();
    assert_eq!(
        eligibility.refusals,
        vec![Ineligible::NoSuchKey { key: "booking_date".to_string() }],
        "the table is still partitioned; the key is simply not one of its columns"
    );
}

#[test]
fn a_float_column_makes_a_table_ineligible() {
    // Floating point breaks the canonical-encoding property in both directions at once, and
    // the direction that matters is the one that loses data: two unequal values sharing bytes
    // means a corrupted archive verifies as faithful, and the purge then proceeds.
    let mut policy = records();
    policy.columns.push(Column::new("rate", LogicalType::Float32));

    let eligibility = policy.eligible();
    assert_eq!(
        eligibility.refusals,
        vec![Ineligible::UnarchivableColumn {
            column: "rate".to_string(),
            why: NotCanonical::FloatingPoint,
        }]
    );
    assert!(
        eligibility.to_string().contains("NaN"),
        "the reason names the mechanism, so nobody has to take it on faith: {eligibility}"
    );
}

#[test]
fn a_json_column_makes_a_table_ineligible_and_utf8_does_not() {
    // The two are the same bytes on disk and different claims. JSON's stored text is not
    // determined by its value; a `Utf8` column is whatever the writer put there, and its owner
    // is the one taking responsibility for the canonical form.
    let mut policy = records();
    policy.columns.push(Column::new("payload", LogicalType::Json));
    assert_eq!(
        policy.eligible().refusals,
        vec![Ineligible::UnarchivableColumn {
            column: "payload".to_string(),
            why: NotCanonical::UnstableTextForm,
        }]
    );

    let mut policy = records();
    policy.columns.push(Column::new("payload", LogicalType::Utf8));
    assert!(policy.eligible().is_eligible());
}

#[test]
fn every_bad_column_is_named_and_not_just_the_first() {
    let mut policy = records();
    policy.columns.push(Column::new("a", LogicalType::Float32));
    policy.columns.push(Column::new("b", LogicalType::Json));
    policy.columns.push(Column::new("c", LogicalType::Float64));

    let named: Vec<String> = policy
        .eligible()
        .refusals
        .iter()
        .filter_map(|r| match r {
            Ineligible::UnarchivableColumn { column, .. } => Some(column.clone()),
            _ => None,
        })
        .collect();

    assert_eq!(named, vec!["a", "b", "c"], "a schema is fixed once, not one column per attempt");
}

#[test]
fn identifiers_must_be_asserted_vaulted_and_the_absence_is_a_refusal() {
    // `FR-TIER-25` makes this ineligible *by default*, because tiering converts a cheap
    // erasure into an expensive one. Nothing in this system classifies a column as a direct
    // identifier, so this is an assertion somebody makes — and not making it is a refusal
    // rather than a permission.
    let mut policy = records();
    policy.identifiers_vaulted = false;

    let eligibility = policy.eligible();
    assert_eq!(eligibility.refusals, vec![Ineligible::IdentifiersNotVaulted]);
    assert!(
        eligibility.to_string().contains("not a permission"),
        "the refusal says which way the default falls: {eligibility}"
    );
}

#[test]
fn a_policy_with_no_retention_basis_is_refused() {
    // A purge is gated on the range being covered by a retention basis. A policy without one
    // describes a purge that can never be authorised, which is better said now than at the
    // moment somebody tries to run it.
    let mut policy = records();
    policy.retention = Retention::new("", 2557);
    assert_eq!(policy.eligible().refusals, vec![Ineligible::NoRetentionBasis]);

    let mut policy = records();
    policy.retention = Retention::new("   ", 2557);
    assert_eq!(
        policy.eligible().refusals,
        vec![Ineligible::NoRetentionBasis],
        "whitespace is not a basis"
    );

    let mut policy = records();
    policy.retention = Retention::new("statutory", 0);
    assert_eq!(
        policy.eligible().refusals,
        vec![Ineligible::NoRetentionBasis],
        "nor is a basis that expires immediately"
    );
}

#[test]
fn a_table_with_no_columns_is_refused_alone() {
    // Every other rule passes vacuously over an empty schema — no column has a bad type when
    // there are no columns — and a report saying "eligible except for having no columns"
    // invites somebody to read the rest of it.
    let mut policy = records();
    policy.columns.clear();
    policy.contract = Contract::Mutable;
    policy.identifiers_vaulted = false;

    assert_eq!(
        policy.eligible().refusals,
        vec![Ineligible::NoColumns],
        "the other faults are not reported, because they would be conclusions about nothing"
    );
}

#[test]
fn the_refusal_order_is_fixed_so_two_reports_can_be_compared() {
    let mut policy = records();
    policy.contract = Contract::Mutable;
    policy.identifiers_vaulted = false;
    policy.columns.push(Column::new("score", LogicalType::Float64));

    let first = policy.eligible();
    let second = policy.eligible();
    assert_eq!(first, second, "a diff of two reports has to mean something");
}

#[test]
fn every_logical_type_has_a_decided_answer() {
    // The exhaustive check. `canonical_encoding` matches every variant rather than listing the
    // bad ones, so adding a logical type to `sankhya-schema` fails to compile until somebody
    // decides what archiving it means. This test is the other half: it pins what was decided,
    // so a later change from `Err` to `Ok` for convenience has to argue with a test.
    let archivable = [
        LogicalType::Boolean,
        LogicalType::Int16,
        LogicalType::Int32,
        LogicalType::Int64,
        LogicalType::Decimal(Precision { digits: 38, scale: 9 }),
        LogicalType::Utf8,
        LogicalType::Binary,
        LogicalType::TimestampUtc,
        LogicalType::TimestampLocal,
        LogicalType::Date,
        LogicalType::Time,
        LogicalType::Uuid,
    ];
    for logical in archivable {
        assert!(
            canonical_encoding(&logical).is_ok(),
            "{logical:?} should be archivable"
        );
    }

    assert_eq!(
        canonical_encoding(&LogicalType::Float32),
        Err(NotCanonical::FloatingPoint)
    );
    assert_eq!(
        canonical_encoding(&LogicalType::Float64),
        Err(NotCanonical::FloatingPoint)
    );
    assert_eq!(
        canonical_encoding(&LogicalType::Json),
        Err(NotCanonical::UnstableTextForm)
    );
}

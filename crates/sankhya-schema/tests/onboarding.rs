//! Onboarding tests, including against the real ten-table dataset.

use sankhya_cdc_model::{ColumnDescriptor, Decoder, Message, RelationDescriptor, ReplicaIdentity};
use sankhya_schema::{
    inexact_columns, is_fully_exact, onboard_relation, LogicalType, OnboardingError,
    OnboardingWarning, WriteStrategy,
};
use std::process::Command;

fn column(name: &str, oid: u32, modifier: i32, is_key: bool) -> ColumnDescriptor {
    ColumnDescriptor {
        name: name.into(),
        type_oid: oid,
        type_modifier: modifier,
        is_key,
    }
}

fn relation(
    name: &str,
    identity: ReplicaIdentity,
    columns: Vec<ColumnDescriptor>,
) -> RelationDescriptor {
    RelationDescriptor {
        relation_id: 1,
        namespace: "public".into(),
        name: name.into(),
        replica_identity: identity,
        columns,
    }
}

#[test]
fn a_keyed_table_is_mergeable() {
    let r = relation(
        "orders",
        ReplicaIdentity::Default,
        vec![column("id", 20, -1, true), column("label", 25, -1, false)],
    );
    let onboarded = onboard_relation(&r).expect("onboards");
    assert_eq!(onboarded.strategy, WriteStrategy::Mergeable);
    assert_eq!(onboarded.location.relative_path(), "public/orders");
    assert!(onboarded.warnings.is_empty());
}

#[test]
fn a_key_column_is_never_nullable() {
    // A nullable key would identify nothing.
    let r = relation(
        "t",
        ReplicaIdentity::Default,
        vec![column("id", 20, -1, true)],
    );
    let onboarded = onboard_relation(&r).expect("onboards");
    assert!(!onboarded.schema.fields[0].nullable);
}

#[test]
fn a_keyless_table_is_append_only_and_says_so() {
    // Not a degradation: with no identity the source itself rejects updates and
    // deletes, so append-only is the only possible strategy. Reported so an operator
    // expecting updates to replicate learns why they do not.
    let r = relation(
        "events",
        ReplicaIdentity::Default,
        vec![column("payload", 25, -1, false)],
    );
    let onboarded = onboard_relation(&r).expect("onboards");
    assert_eq!(onboarded.strategy, WriteStrategy::AppendOnly);
    assert!(onboarded
        .warnings
        .iter()
        .any(|w| matches!(w, OnboardingWarning::NoRowIdentity { .. })));
}

#[test]
fn replica_identity_nothing_forces_append_only_even_with_a_key() {
    let r = relation(
        "t",
        ReplicaIdentity::Nothing,
        vec![column("id", 20, -1, true)],
    );
    assert_eq!(
        onboard_relation(&r).expect("onboards").strategy,
        WriteStrategy::AppendOnly
    );
}

#[test]
fn full_replica_identity_warns_about_log_volume() {
    // Correct and sometimes necessary, but log volume governs how long a stalled
    // consumer has before it endangers the source.
    let r = relation("t", ReplicaIdentity::Full, vec![column("id", 20, -1, true)]);
    let onboarded = onboard_relation(&r).expect("onboards");
    let warning = onboarded
        .warnings
        .iter()
        .find(|w| matches!(w, OnboardingWarning::FullReplicaIdentity { .. }))
        .expect("should warn");
    assert!(warning.to_string().contains("write-ahead log volume"));
}

#[test]
fn one_unmappable_column_refuses_the_whole_table() {
    // Partial onboarding would accept writes the table cannot faithfully store.
    let r = relation(
        "t",
        ReplicaIdentity::Default,
        vec![
            column("id", 20, -1, true),
            column("amount", 1700, -1, false), // unconstrained decimal
        ],
    );
    let err = onboard_relation(&r).expect_err("must refuse");
    let OnboardingError::UnmappableColumn { column, reason, .. } = err else {
        panic!("expected an unmappable column, got {err:?}");
    };
    assert_eq!(column, "amount");
    assert!(reason.contains("arbitrary precision"), "{reason}");
}

#[test]
fn a_column_shadowing_provenance_is_refused() {
    let r = relation(
        "t",
        ReplicaIdentity::Default,
        vec![
            column("id", 20, -1, true),
            column("_sankhya_commit_lsn", 20, -1, false),
        ],
    );
    assert!(matches!(
        onboard_relation(&r),
        Err(OnboardingError::ReservedColumn { .. })
    ));
}

#[test]
fn exactness_is_reported_per_column() {
    let r = relation(
        "t",
        ReplicaIdentity::Default,
        vec![
            column("id", 20, -1, true),
            column("measurement", 701, -1, false), // float
        ],
    );
    let onboarded = onboard_relation(&r).expect("onboards");
    assert!(!is_fully_exact(&onboarded.schema));
    assert_eq!(inexact_columns(&onboarded.schema), ["measurement"]);
}

// --- against the real dataset ----------------------------------------------------

/// Capture the relation descriptions the stream carries.
///
/// The slot name is a parameter because these tests run concurrently, and a shared
/// slot means one test drops the slot another is reading — which presents as an
/// unrelated psql failure rather than as the isolation problem it is.
fn stream_relations(slot: &str) -> Option<Vec<RelationDescriptor>> {
    let bin = std::env::var("SANKHYA_PG_BIN").ok()?;
    let socket = std::env::var("SANKHYA_E2E_SOCKET").ok()?;

    let psql = |sql: &str| -> String {
        let out = Command::new(format!("{bin}/psql"))
            .args([
                "-h", &socket, "-U", "sankhya", "-d", "postgres", "-tA", "-c", sql,
            ])
            .output()
            .expect("psql runs");
        assert!(
            out.status.success(),
            "psql failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };

    psql(&format!(
        "SELECT pg_drop_replication_slot('{slot}') WHERE EXISTS
         (SELECT 1 FROM pg_replication_slots WHERE slot_name='{slot}')"
    ));
    psql(&format!(
        "SELECT pg_create_logical_replication_slot('{slot}','pgoutput')"
    ));

    // Touch every table so the stream carries a description of each.
    //
    // A self-assigning update is used deliberately. An `INSERT ... ON CONFLICT DO
    // NOTHING` writes no WAL when the row already exists, so it produces no relation
    // message at all — which is how the first version of this helper silently
    // described nothing. An update writes a new row version regardless of whether the
    // value changed, so the description is always emitted.
    let tables = psql(
        "SELECT c.relname FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE n.nspname='public' AND c.relkind='r' ORDER BY c.relname",
    );
    for table in tables.lines().filter(|l| !l.trim().is_empty()) {
        psql(&format!(
            "UPDATE {table} SET id = id WHERE id = (SELECT min(id) FROM {table})"
        ));
    }

    let hex = psql(&format!(
        "SELECT encode(data,'hex') FROM pg_logical_slot_get_binary_changes(
            '{slot}', NULL, NULL, 'proto_version','4','publication_names','sankhya_all')"
    ));
    psql(&format!("SELECT pg_drop_replication_slot('{slot}')"));

    let decoder = Decoder::new();
    let mut relations = Vec::new();
    for line in hex.lines().filter(|l| !l.trim().is_empty()) {
        let bytes: Vec<u8> = (0..line.len())
            .step_by(2)
            .filter_map(|i| u8::from_str_radix(line.get(i..i + 2)?, 16).ok())
            .collect();
        if let Ok((Message::Relation(r), _)) = decoder.decode_prefix(&bytes) {
            relations.push((*r).clone());
        }
    }
    Some(relations)
}

#[test]
fn every_table_in_the_real_dataset_onboards() {
    let Some(relations) = stream_relations("sankhya_onboard_all") else {
        eprintln!("skipping: set SANKHYA_PG_BIN and SANKHYA_E2E_SOCKET to run");
        return;
    };
    assert!(!relations.is_empty(), "the stream described no relations");

    let mut mergeable = 0usize;
    let mut columns = 0usize;
    let mut seen = std::collections::BTreeSet::new();

    for relation in &relations {
        let onboarded = onboard_relation(relation).unwrap_or_else(|e| {
            panic!(
                "{}.{} failed to onboard: {e}",
                relation.namespace, relation.name
            )
        });

        // Every fixture table has a primary key, so all should be mergeable. If one
        // were not, the storage layer would silently skip merge machinery it needs.
        assert_eq!(
            onboarded.strategy,
            WriteStrategy::Mergeable,
            "{} should be mergeable",
            relation.name
        );
        // And every one should map with no name transformation at all.
        assert!(
            onboarded.location.is_fully_relatable(),
            "{} should need no name transformation",
            relation.name
        );
        assert_eq!(
            onboarded.location.relative_path(),
            format!("public/{}", relation.name)
        );

        // Exactly one key column: the synthetic primary key.
        assert_eq!(onboarded.schema.key_fields().len(), 1, "{}", relation.name);

        mergeable += 1;
        columns += onboarded.schema.fields.len();
        seen.insert(relation.name.clone());
    }

    assert_eq!(
        seen.len(),
        10,
        "expected the ten acceptance tables, saw {seen:?}"
    );
    eprintln!(
        "onboarding: {mergeable} tables, {columns} columns, all mergeable and \
         identity-named, derived from the live stream alone"
    );
}

#[test]
fn the_real_dataset_exercises_both_exact_and_inexact_columns() {
    let Some(relations) = stream_relations("sankhya_onboard_exact") else {
        eprintln!("skipping: database not configured");
        return;
    };
    let mut with_floats = 0usize;
    let mut fully_exact = 0usize;
    for relation in &relations {
        let onboarded = onboard_relation(relation).expect("onboards");
        if is_fully_exact(&onboarded.schema) {
            fully_exact += 1;
        } else {
            with_floats += 1;
        }
        for field in &onboarded.schema.fields {
            if matches!(field.logical, LogicalType::Decimal(_)) {
                assert!(field.logical.is_exact());
            }
        }
    }
    assert!(
        with_floats > 0,
        "the fixture set must contain inexact columns"
    );
    assert!(fully_exact > 0, "and tables that are entirely exact");
    eprintln!("exactness: {fully_exact} fully-exact tables, {with_floats} containing floats");
}

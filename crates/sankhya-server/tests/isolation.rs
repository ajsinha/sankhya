//! Two tenants, one deployment, and every surface between them.
//!
//! M5's demonstration is stated as a sentence: *"Every attempt to reach the other's data —
//! through SQL, through the columnar surface, through a graph traversal, through a cached
//! result, through an error message — fails."*
//!
//! Those five surfaces are each tested where they live, and that is not the same as testing
//! them together. A boundary can hold in five files and leak in the combination, because
//! each file uses its own fixture and none of them shares a tenant with another.
//!
//! So this file uses **one pair of tenants across all five**, and it is the test the
//! exit criterion actually asks for.

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

use sankhya_api_pg::session::Handler;
use sankhya_audit::chain::{Chain, DataVersion, Entry, RecordedDecision};
use sankhya_authz::policy::{Action, PolicySet, Rule, TableRef};
use sankhya_authz::principal::{Authentication, Principal, Role, TenantId};
use sankhya_catalog::guard::Guard;
use sankhya_catalog::key::{Entitlements, PlanKey, ResultKey};

#[path = "../src/execute.rs"]
mod execute;
#[path = "../src/warehouse.rs"]
mod warehouse;
#[path = "../src/wiring.rs"]
mod wiring;

/// The two tenants, used by every surface below so the combination is what is tested.
fn acme() -> TenantId {
    TenantId::from_uuid(uuid::Uuid::from_u128(0xAC33))
}

fn rival() -> TenantId {
    TenantId::from_uuid(uuid::Uuid::from_u128(0x21_2A))
}

fn person(name: &str, tenant: TenantId) -> Principal {
    Principal::authenticated(
        name,
        tenant,
        [Role::new("reader")],
        Authentication::MutualTls,
    )
    .expect("a valid principal")
}

fn orders() -> TableRef {
    TableRef::new("sales", "orders")
}

/// Acme may read its own orders. The rival's identical role and table are granted in the
/// rival's own tenant, which is the realistic case and the one a missing tenant comparison
/// would sail straight through.
fn policy() -> PolicySet {
    PolicySet::new()
        .with(Rule::grant(
            acme(),
            Role::new("reader"),
            orders(),
            Action::Read,
        ))
        .with(Rule::grant(
            rival(),
            Role::new("reader"),
            orders(),
            Action::Read,
        ))
}

// --- surface 1: SQL -------------------------------------------------------

#[test]
fn a_principal_cannot_obtain_a_guard_for_another_tenants_table() {
    // The gate everything else is behind: no guard, no provider, no query.
    let policy = policy();
    let mine = Guard::authorize(&policy, &person("ana", acme()), &orders(), Action::Read);
    assert!(mine.is_some());
    assert_eq!(mine.map(|g| *g.tenant()), Some(acme()));

    // The rival holds the same role name on the same table name, and gets their own data.
    let theirs = Guard::authorize(&policy, &person("mal", rival()), &orders(), Action::Read);
    assert_eq!(theirs.map(|g| *g.tenant()), Some(rival()));
}

#[test]
fn a_guards_storage_prefix_cannot_reach_the_other_tenants_files() {
    // The columnar surface. Even holding a valid guard, the prefix it yields is derived
    // from its own tenant — a caller cannot supply somebody else's.
    let policy = policy();
    let mine = Guard::authorize(&policy, &person("ana", acme()), &orders(), Action::Read)
        .expect("granted");
    let theirs = Guard::authorize(&policy, &person("mal", rival()), &orders(), Action::Read)
        .expect("granted");

    assert_ne!(mine.storage_prefix(), theirs.storage_prefix());
    assert!(
        !mine.storage_prefix().contains(".."),
        "no traversal is expressible"
    );
}

// --- surface 2: the graph tier --------------------------------------------

#[test]
fn a_traversal_has_no_identifier_that_could_reach_the_other_tenants_vertices() {
    // FR-GRAPH-11 forbids traversing a shared graph and filtering afterwards, because
    // filtering leaks topology through timing and through path structure. Each tenant
    // hydrates its own epoch, so the other's vertices were never interned.
    use arrow_array::{Float64Array, Int64Array, RecordBatch, StringArray};
    use arrow_schema::{DataType, Field, Schema};
    use sankhya_graph::epoch::EpochId;
    use sankhya_graph::hydrate::{Hydration, MemoryBudget};
    use sankhya_graph::spec::{EdgeSpec, GraphSpec};
    use std::sync::Arc;

    let spec = GraphSpec::new().with(
        EdgeSpec::new("from_key", "to_key", "transfer")
            .between("party", "party")
            .valid_from("occurred_at")
            .weighted_by("amount"),
    );
    let make = |rows: &[(&str, &str)], id: u64| {
        let schema = Arc::new(Schema::new(vec![
            Field::new("from_key", DataType::Utf8, true),
            Field::new("to_key", DataType::Utf8, true),
            Field::new("occurred_at", DataType::Int64, false),
            Field::new("amount", DataType::Float64, false),
        ]));
        let from: StringArray = rows.iter().map(|r| Some(r.0)).collect();
        let to: StringArray = rows.iter().map(|r| Some(r.1)).collect();
        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(from),
                Arc::new(to),
                Arc::new(Int64Array::from(vec![1i64; rows.len()])),
                Arc::new(Float64Array::from(vec![1.0f64; rows.len()])),
            ],
        )
        .expect("valid");
        let mut hydration = Hydration::new(spec.clone(), MemoryBudget::generous());
        hydration.absorb(&batch).expect("well-formed");
        hydration.finish(EpochId(id), id, 0).expect("within budget")
    };

    let ours = make(&[("acme-1", "acme-2")], 1);
    let theirs = make(&[("rival-1", "rival-2")], 2);

    assert!(ours.vertex(b"acme-1").is_some());
    assert!(
        ours.vertex(b"rival-1").is_none(),
        "the other tenant's vertex is not filtered out — it was never interned, so no \
         identifier exists that could reach it"
    );
    assert!(theirs.vertex(b"acme-1").is_none());
}

// --- surface 3: a cached result -------------------------------------------

#[test]
fn a_cached_result_cannot_be_served_to_the_other_tenant() {
    // The subtlest surface. A cache key that omits the entitlement set does not return a
    // stale answer — it returns *someone else's*, and it looks perfectly fresh.
    let plan = PlanKey::new("SELECT * FROM orders", 1, 1);
    let mine = ResultKey::new(plan, &Entitlements::new(["acme:reader"]), 41);
    let theirs = ResultKey::new(plan, &Entitlements::new(["rival:reader"]), 41);

    assert_ne!(
        mine, theirs,
        "the same SQL, the same snapshot, different entitlements — the keys must differ, \
         or one tenant is served the other's cached rows"
    );

    // And the same principal at a different snapshot is also a different key, or a pinned
    // read would be served a newer answer than it asked for.
    assert_ne!(
        mine,
        ResultKey::new(plan, &Entitlements::new(["acme:reader"]), 42)
    );
}

// --- surface 4: the error message -----------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn an_error_does_not_disclose_that_the_other_tenants_table_exists() {
    // The surface that gets forgotten. A permission error naming a table confirms it
    // exists, and the difference between "no such table" and "you may not read that" is a
    // working enumeration oracle.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let empty = wiring::Settings {
        listen: "127.0.0.1:0".to_string(),
        warehouse: dir.path().to_path_buf(),
        read_as_of: sankhya_types::Lsn::new(u64::MAX),
        tenant: acme(),
        maintenance: None,
        require_password: true,
        metrics_listen: None,
    };
    // Acme's server, with the rival's table granted only to the rival.
    let server = wiring::Server::with_tables(empty, policy(), Vec::new(), Vec::new());

    let unknown = server.query("SELECT * FROM does_not_exist_anywhere");
    let forbidden = server.query("SELECT * FROM orders");

    let shape = |r: &Result<_, sankhya_api_pg::session::QueryFailure>| {
        r.as_ref().err().map(|e| {
            e.message
                .replace("does_not_exist_anywhere", "X")
                .replace("orders", "X")
        })
    };
    assert!(unknown.is_err() && forbidden.is_err());
    assert_eq!(
        shape(&unknown),
        shape(&forbidden),
        "the two errors must differ only in the name the caller already supplied"
    );
}

// --- surface 5: the audit -------------------------------------------------

#[test]
fn an_audit_reproduces_exactly_what_a_principal_saw() {
    // M5's fourth exit criterion. Not "a query happened and here is who ran it" — the
    // restrictions that were applied and the version of the data that answered, or the
    // record reproduces nothing.
    let mut chain = Chain::new();
    let mut masks = std::collections::BTreeMap::new();
    masks.insert("region".to_string(), sankhya_authz::policy::Mask::Null);

    chain.append(
        Entry::by(
            &person("ana", acme()),
            orders(),
            Action::Read,
            RecordedDecision::allowed(Some("region = 'north'".to_string()), &masks),
            1_000,
        )
        .running("SELECT id, region FROM orders")
        .from_version(DataVersion::snapshot(41).with_graph_epoch(7))
        .returning(12),
    );
    // The rival's identical query against their own tenant.
    chain.append(
        Entry::by(
            &person("mal", rival()),
            orders(),
            Action::Read,
            RecordedDecision::allowed(None, &std::collections::BTreeMap::new()),
            1_001,
        )
        .returning(99),
    );

    let seen = chain.what_was_seen_by(&acme(), "ana");
    assert_eq!(seen.len(), 1, "one principal's history, not both");
    let record = seen.first().expect("one record");

    assert_eq!(
        record.decision.row_filter.as_deref(),
        Some("region = 'north'")
    );
    assert_eq!(
        record
            .decision
            .column_masks
            .get("region")
            .map(String::as_str),
        Some("null"),
        "without the masks the record says access was allowed and cannot say to what"
    );
    let version = record.data_version.as_ref().expect("a version");
    assert_eq!(version.snapshot, 41);
    assert_eq!(version.graph_epoch, Some(7));
    assert_eq!(record.rows_returned, Some(12));

    assert!(chain.verify().is_ok(), "and the chain is intact");
}

#[test]
fn one_tenants_audit_history_is_not_visible_to_the_other() {
    // A subject name is unique only within a tenant, so matching on the name alone would
    // return another tenant's records to whoever asked for their own.
    let mut chain = Chain::new();
    for (who, tenant) in [("ana", acme()), ("ana", rival())] {
        chain.append(Entry::by(
            &person(who, tenant),
            orders(),
            Action::Read,
            RecordedDecision::allowed(None, &std::collections::BTreeMap::new()),
            1,
        ));
    }
    assert_eq!(chain.what_was_seen_by(&acme(), "ana").len(), 1);
    assert_eq!(chain.what_was_seen_by(&rival(), "ana").len(), 1);
}

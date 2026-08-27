//! The composition root, driven the way a client drives it.
//!
//! The wiring is a library rather than a `main` precisely so this test can exist. A binary
//! is hard to test and easy to let drift; the thing that decides which policy is in force,
//! which principal a connection becomes and what the audit records is the thing most worth
//! testing, so it lives where a test can reach it.

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
use sankhya_authz::policy::{Action, PolicySet, Rule, TableRef};
use sankhya_authz::principal::{Role, TenantId};

// The wiring module is part of the binary, so the test builds it directly. That is the
// cost of a composition root living in a binary crate, and it is cheaper than the
// alternative of never testing it.
#[path = "../src/wiring.rs"]
mod wiring;

use wiring::{example_tables, permissive_policy, Server, Settings};

fn tenant() -> TenantId {
    TenantId::from_uuid(uuid::Uuid::from_u128(1))
}

fn settings(require_password: bool) -> Settings {
    Settings {
        listen: "127.0.0.1:0".to_string(),
        tenant: tenant(),
        require_password,
    }
}

fn server(policy: PolicySet) -> Server {
    Server::new(settings(true), policy, example_tables())
}

#[test]
fn a_connection_with_no_user_is_refused() {
    // An unattributable connection cannot be audited, and an audit that cannot name who
    // acted is not an audit. A default user would make that hole silent.
    let server = server(PolicySet::new());
    let Err(failure) = server.authenticate(&[], Some(b"anything")) else {
        panic!("a connection with no user must be refused");
    };
    assert!(failure.message.contains("cannot be audited"));
    assert_eq!(failure.sqlstate, "28000");
}

#[test]
fn a_password_is_required_when_configured_and_not_otherwise() {
    let with = server(PolicySet::new());
    assert!(with.requires_password(&[]));
    let user = vec![("user".to_string(), "ana".to_string())];
    assert!(with.authenticate(&user, None).is_err());
    assert!(with.authenticate(&user, Some(b"anything")).is_ok());

    let without = Server::new(settings(false), PolicySet::new(), example_tables());
    assert!(!without.requires_password(&[]));
    assert!(without.authenticate(&user, None).is_ok());
}

#[test]
fn an_insecure_configuration_looks_wrong_in_the_startup_line() {
    // An operator who has accidentally started an open server should see it, not have to
    // go and check.
    let open = Server::new(settings(false), PolicySet::new(), example_tables());
    assert!(open.describe().contains("NO AUTHENTICATION"));

    let closed = server(PolicySet::new());
    assert!(closed.describe().contains("password required"));
    assert!(!closed.describe().contains("NO AUTHENTICATION"));
}

#[test]
fn the_catalogue_lists_only_tables_the_policy_permits() {
    // A schema browser is a back door to the same disclosure the policy component refuses
    // everywhere else: a table name says what a business does.
    let tables = example_tables();
    let permitted = server(permissive_policy(&tenant(), &tables));
    assert_eq!(permitted.visible_tables().len(), tables.len());

    let nothing_granted = server(PolicySet::new());
    assert!(
        nothing_granted.visible_tables().is_empty(),
        "a table nobody was granted must not appear in a schema listing"
    );
}

#[test]
fn a_table_granted_to_another_tenant_is_not_listed() {
    let other = TenantId::from_uuid(uuid::Uuid::from_u128(2));
    let policy = PolicySet::new().with(Rule::grant(
        other,
        Role::new("reader"),
        TableRef::new("public", "example"),
        Action::Read,
    ));
    assert!(server(policy).visible_tables().is_empty());
}

#[test]
fn a_statement_is_refused_with_an_explanation_rather_than_an_empty_result() {
    // An empty result would look like a table with no rows, and a plausible zero would look
    // like an answer. Neither is true, and both would be believed.
    let server = server(permissive_policy(&tenant(), &example_tables()));
    let Err(failure) = server.query("SELECT id FROM example") else {
        panic!("there is no engine behind this yet");
    };
    assert_eq!(failure.sqlstate, "0A000", "feature not supported");
    assert!(failure.message.contains("does not yet execute statements"));
    assert!(
        failure
            .detail
            .as_deref()
            .is_some_and(|d| d.contains("read path exists")),
        "the refusal should say what does work"
    );
}

#[test]
fn everything_that_happens_is_audited_and_the_chain_verifies() {
    let server = server(permissive_policy(&tenant(), &example_tables()));
    assert_eq!(server.audit_len(), 0);
    let empty_head = server.audit_head();

    server.query("SELECT id FROM example").ok();
    server.visible_tables();
    server.query("SELECT 1").ok();

    assert_eq!(server.audit_len(), 3, "refusals are audited too");
    assert!(server.audit_intact(), "the chain must verify");
    assert_ne!(
        server.audit_head(),
        empty_head,
        "the head moves, which is what gets mirrored somewhere append-only"
    );
}

#[test]
fn the_audit_records_the_shape_of_a_statement_and_not_its_values() {
    // A statement can contain the values a query was filtering on. Copying those verbatim
    // into a durable log turns the audit into a second place the data lives, with different
    // retention and different access control from the table it came from.
    let server = server(permissive_policy(&tenant(), &example_tables()));
    server
        .query("SELECT id FROM example WHERE national_id = '123-45-6789'")
        .ok();

    let head = server.audit_head();
    assert!(!head.is_empty());
    assert!(server.audit_intact());
    // The sensitive literal must not be reachable through the audit surface at all.
    assert!(
        !format!("{server:?}").contains("123-45-6789"),
        "a filtered value reached the audit record"
    );
}

#[test]
fn a_tenant_has_a_quota_in_force() {
    // Not a placeholder: a tenant with no quota is refused by the governor, so a server
    // that forgot to set one would refuse every query with a confusing message.
    let server = server(PolicySet::new());
    assert!(
        server.quota().is_some(),
        "a tenant with no quota is refused every query"
    );
}

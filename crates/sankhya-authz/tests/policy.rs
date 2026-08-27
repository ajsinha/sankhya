//! The policy decision, tested from the direction an attacker would come.
//!
//! Most tests here assert something is **refused**. That asymmetry is deliberate: a test
//! that access works fails loudly the first time someone uses the system, and a test that
//! access is *refused* is the only thing standing between a defect and a breach that nobody
//! notices for a year.
//!
//! The two rules everything else rests on are pinned first: absence is denial, and denial
//! beats every grant.

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

use sankhya_authz::policy::{Action, Decision, DenialReason, Mask, PolicySet, Rule, TableRef};
use sankhya_authz::principal::{Authentication, Principal, Role, TenantId};

fn tenant(name: &str) -> TenantId {
    TenantId::new(name).expect("the fixture uses a valid tenant name")
}

fn person(name: &str, in_tenant: &str, roles: &[&str]) -> Principal {
    Principal::authenticated(
        name,
        tenant(in_tenant),
        roles.iter().map(|r| Role::new(*r)),
        Authentication::FederatedToken,
    )
    .expect("the fixture builds a valid principal")
}

fn table() -> TableRef {
    TableRef::new("sales", "orders")
}

// --- the two rules everything rests on ------------------------------------

#[test]
fn a_principal_with_no_matching_rule_is_denied() {
    // Absence is denial. The alternative — allowed until forbidden — means a policy set
    // that fails to load, or a table nobody wrote a rule for, grants everything.
    let policy = PolicySet::new();
    let decision = policy.decide(&person("ana", "acme", &["reader"]), &table(), Action::Read);

    assert!(!decision.is_allowed());
    assert_eq!(
        decision,
        Decision::Denied {
            reason: DenialReason::NoGrant
        }
    );
}

#[test]
fn an_explicit_denial_beats_any_number_of_grants() {
    // Without this, adding a role could only ever widen what someone sees, and an exclusion
    // would be inexpressible.
    let policy = PolicySet::new()
        .with(Rule::grant(
            tenant("acme"),
            Role::new("reader"),
            table(),
            Action::Read,
        ))
        .with(Rule::grant(
            tenant("acme"),
            Role::new("analyst"),
            table(),
            Action::Read,
        ))
        .with(Rule::deny(
            tenant("acme"),
            Role::new("suspended"),
            table(),
            Action::Read,
        ));

    let ordinary = person("ana", "acme", &["reader", "analyst"]);
    assert!(policy
        .decide(&ordinary, &table(), Action::Read)
        .is_allowed());

    let suspended = person("bo", "acme", &["reader", "analyst", "suspended"]);
    let decision = policy.decide(&suspended, &table(), Action::Read);
    assert!(
        !decision.is_allowed(),
        "two grants must not outvote one denial"
    );
}

#[test]
fn the_denial_message_does_not_reveal_whether_a_rule_names_the_principal() {
    // Distinguishing "no rule" from "a rule forbids you" tells a caller something about the
    // policy set they were not granted.
    let denied_by_absence = DenialReason::NoGrant.to_string();
    let denied_by_rule = DenialReason::ExplicitDeny {
        role: Role::new("suspended"),
    }
    .to_string();
    assert_eq!(denied_by_absence, denied_by_rule);
}

// --- the tenant boundary --------------------------------------------------

#[test]
fn a_rule_written_for_one_tenant_never_applies_to_another() {
    // The boundary that is not expressible as a rule. Here the *same* role name and the
    // *same* table exist in both tenants, which is the realistic case and the one where a
    // missing tenant comparison would go unnoticed.
    let policy = PolicySet::new().with(Rule::grant(
        tenant("acme"),
        Role::new("reader"),
        table(),
        Action::Read,
    ));

    assert!(policy
        .decide(&person("ana", "acme", &["reader"]), &table(), Action::Read)
        .is_allowed());
    assert!(
        !policy
            .decide(&person("mal", "other", &["reader"]), &table(), Action::Read)
            .is_allowed(),
        "an identically-named role in another tenant must not inherit the grant"
    );
}

#[test]
fn a_tenant_identifier_that_could_traverse_a_path_is_refused() {
    // The identifier becomes an object-store path prefix. A '/' or '..' in it is a path
    // traversal into another tenant's data.
    for attempt in ["../other", "a/b", "..", "with space", ""] {
        assert!(
            TenantId::new(attempt).is_err(),
            "'{attempt}' must not be accepted as a tenant identifier"
        );
    }
    assert!(TenantId::new("acme-prod_2").is_ok());
}

#[test]
fn internal_work_is_still_scoped_to_a_tenant() {
    // Maintenance that could run without a tenant scope would be the one code path with no
    // boundary, which is exactly what an attacker looks for.
    let system = Principal::internal(tenant("acme"));
    assert_eq!(system.tenant().as_str(), "acme");
    assert!(system.is_internal());

    let policy = PolicySet::new().with(Rule::grant(
        tenant("other"),
        Role::new("reader"),
        table(),
        Action::Read,
    ));
    assert!(
        !policy.decide(&system, &table(), Action::Read).is_allowed(),
        "the internal principal has no special path through the tenant check"
    );
}

// --- how grants combine ---------------------------------------------------

#[test]
fn two_roles_see_the_union_of_their_rows_not_the_intersection() {
    // Granting a role must not be able to *reduce* what someone sees. AND-ing the filters
    // would make a second role narrow the first, which is not what granting means.
    let policy = PolicySet::new()
        .with(
            Rule::grant(tenant("acme"), Role::new("north"), table(), Action::Read)
                .where_rows("region = 'north'"),
        )
        .with(
            Rule::grant(tenant("acme"), Role::new("south"), table(), Action::Read)
                .where_rows("region = 'south'"),
        );

    let both = person("ana", "acme", &["north", "south"]);
    let filter = policy
        .decide(&both, &table(), Action::Read)
        .row_filter()
        .map(str::to_string)
        .expect("a filter is produced");

    assert!(
        filter.contains(" OR "),
        "filters must combine with OR: {filter}"
    );
    assert!(filter.contains("north") && filter.contains("south"));
}

#[test]
fn an_unrestricted_grant_absorbs_a_restricted_one() {
    // A grant with no filter sees every row. Emitting a filter alongside it would narrow
    // what the unrestricted grant already permits.
    let policy = PolicySet::new()
        .with(
            Rule::grant(tenant("acme"), Role::new("north"), table(), Action::Read)
                .where_rows("region = 'north'"),
        )
        .with(Rule::grant(
            tenant("acme"),
            Role::new("auditor"),
            table(),
            Action::Read,
        ));

    let both = person("ana", "acme", &["north", "auditor"]);
    assert_eq!(
        policy.decide(&both, &table(), Action::Read).row_filter(),
        None,
        "an unrestricted grant means every row, and adding a filter would take rows away"
    );
}

#[test]
fn a_column_is_masked_only_when_every_grant_masks_it() {
    // If one role may see a column in the clear, the principal may. Masking on the union
    // would let an added role take visibility away.
    let policy = PolicySet::new()
        .with(
            Rule::grant(tenant("acme"), Role::new("clerk"), table(), Action::Read)
                .masking("email", Mask::Null)
                .masking("total", Mask::Null),
        )
        .with(
            Rule::grant(tenant("acme"), Role::new("finance"), table(), Action::Read)
                .masking("email", Mask::Null),
        );

    let both = person("ana", "acme", &["clerk", "finance"]);
    let masks = policy.decide(&both, &table(), Action::Read).column_masks();

    assert!(masks.contains_key("email"), "both roles mask it");
    assert!(
        !masks.contains_key("total"),
        "finance may see the total in the clear, so the principal may"
    );
}

#[test]
fn one_role_masking_a_column_still_masks_it() {
    let policy = PolicySet::new().with(
        Rule::grant(tenant("acme"), Role::new("clerk"), table(), Action::Read)
            .masking("email", Mask::Partial { keep: 4 }),
    );
    let masks = policy
        .decide(&person("ana", "acme", &["clerk"]), &table(), Action::Read)
        .column_masks();

    assert_eq!(masks.get("email"), Some(&Mask::Partial { keep: 4 }));
}

// --- actions and tables are not interchangeable ---------------------------

#[test]
fn a_grant_to_read_does_not_permit_writing() {
    let policy = PolicySet::new().with(Rule::grant(
        tenant("acme"),
        Role::new("reader"),
        table(),
        Action::Read,
    ));
    let ana = person("ana", "acme", &["reader"]);

    assert!(policy.decide(&ana, &table(), Action::Read).is_allowed());
    for forbidden in [Action::Insert, Action::Update, Action::Delete] {
        assert!(
            !policy.decide(&ana, &table(), forbidden).is_allowed(),
            "{forbidden:?} must not follow from a read grant"
        );
    }
}

#[test]
fn a_grant_on_one_table_does_not_reach_another() {
    let policy = PolicySet::new().with(Rule::grant(
        tenant("acme"),
        Role::new("reader"),
        table(),
        Action::Read,
    ));
    let ana = person("ana", "acme", &["reader"]);

    assert!(policy.decide(&ana, &table(), Action::Read).is_allowed());
    assert!(!policy
        .decide(&ana, &TableRef::new("sales", "customers"), Action::Read)
        .is_allowed());
    assert!(
        !policy
            .decide(&ana, &TableRef::new("hr", "orders"), Action::Read)
            .is_allowed(),
        "the same table name in another schema is another table"
    );
}

#[test]
fn a_role_the_principal_does_not_hold_grants_nothing() {
    let policy = PolicySet::new().with(Rule::grant(
        tenant("acme"),
        Role::new("admin"),
        table(),
        Action::Read,
    ));
    assert!(!policy
        .decide(&person("ana", "acme", &["reader"]), &table(), Action::Read)
        .is_allowed());
}

// --- listing --------------------------------------------------------------

#[test]
fn a_table_a_principal_cannot_read_is_not_even_listed() {
    // A table list is an information leak in its own right: names disclose what a business
    // does, and a permission error on a name they were never meant to know confirms it
    // exists.
    let policy = PolicySet::new()
        .with(Rule::grant(
            tenant("acme"),
            Role::new("reader"),
            table(),
            Action::Read,
        ))
        .with(Rule::grant(
            tenant("acme"),
            Role::new("admin"),
            TableRef::new("hr", "salaries"),
            Action::Read,
        ))
        .with(Rule::grant(
            tenant("other"),
            Role::new("reader"),
            TableRef::new("sales", "secrets"),
            Action::Read,
        ));

    let visible = policy.visible_tables(&person("ana", "acme", &["reader"]));
    assert_eq!(visible.len(), 1);
    assert!(visible.contains(&table()));
    assert!(
        !visible.contains(&TableRef::new("hr", "salaries")),
        "a table only admin may read must not appear"
    );
    assert!(
        !visible.contains(&TableRef::new("sales", "secrets")),
        "another tenant's table must not appear"
    );
}

#[test]
fn a_denied_table_is_removed_from_the_listing_even_when_a_grant_exists() {
    let policy = PolicySet::new()
        .with(Rule::grant(
            tenant("acme"),
            Role::new("reader"),
            table(),
            Action::Read,
        ))
        .with(Rule::deny(
            tenant("acme"),
            Role::new("suspended"),
            table(),
            Action::Read,
        ));

    assert!(policy
        .visible_tables(&person("ana", "acme", &["reader"]))
        .contains(&table()));
    assert!(
        policy
            .visible_tables(&person("bo", "acme", &["reader", "suspended"]))
            .is_empty(),
        "a denial must remove the table from the listing, not merely from the results"
    );
}

// --- identity -------------------------------------------------------------

#[test]
fn a_principal_without_a_subject_cannot_be_built() {
    // An unattributable request cannot be audited, and an audit that cannot name who acted
    // is not an audit.
    let outcome = Principal::authenticated("", tenant("acme"), [], Authentication::Password);
    assert!(outcome.is_err());
}

#[test]
fn how_a_principal_authenticated_is_carried_with_it() {
    // A policy can reasonably require a stronger method for a stronger permission, and an
    // audit that does not record which was used cannot show that it did.
    let ana = person("ana", "acme", &["reader"]);
    assert_eq!(ana.authentication(), Authentication::FederatedToken);
    assert!(!ana.is_internal());
    assert!(Principal::internal(tenant("acme")).is_internal());
}

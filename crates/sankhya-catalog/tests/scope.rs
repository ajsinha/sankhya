//! What a guard permits, as a value you can key a cache on.
//!
//! An aggregate computed over the rows one principal may read is not an answer for another
//! principal. Anything that caches aggregates must therefore key them by the scope they were
//! computed under — [ADR-0008](../../../docs/adr/0008-serving-cubes-under-policy.md) — and
//! `Guard::scope_digest` is that key.
//!
//! Two properties matter and they pull in opposite directions, so both are tested here:
//!
//! - **Sharing.** Two principals with identical entitlements must digest the same, or the
//!   cache holds one copy per user and is worth nothing. A thousand analysts across six roles
//!   should produce six entries.
//! - **Separation.** Any difference in what is *visible* must change the digest. A field that
//!   affects visibility and is not in it would let one principal be served another's
//!   aggregate — a disclosure that leaves no trace in the result to notice.
//!
//! The second is the one that must never regress. A test that only checks sharing would pass
//! on a digest that returned a constant.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use sankhya_authz::policy::{Action, Mask, PolicySet, Rule, TableRef};
use sankhya_authz::principal::{Authentication, Principal, Role, TenantId};
use sankhya_catalog::guard::Guard;

/// A stable tenant identifier for a readable name.
fn tenant(name: &str) -> TenantId {
    let mut bytes = [0u8; 16];
    for (slot, byte) in bytes.iter_mut().zip(name.bytes()) {
        *slot = byte;
    }
    TenantId::from_uuid(uuid::Uuid::from_bytes(bytes))
}

fn person(name: &str, in_tenant: &str, roles: &[&str]) -> Principal {
    Principal::authenticated(
        name,
        tenant(in_tenant),
        roles.iter().map(|r| Role::new(*r)),
        Authentication::MutualTls,
    )
    .expect("a valid principal")
}

fn orders() -> TableRef {
    TableRef::new("sales", "orders")
}

/// A policy allowing `role` in `acme` to read orders, optionally filtered and masked.
fn policy_in(in_tenant: &str, role: &str, filter: Option<&str>, mask: Option<(&str, Mask)>) -> PolicySet {
    let mut rule = Rule::grant(tenant(in_tenant), Role::new(role), orders(), Action::Read);
    if let Some(filter) = filter {
        rule = rule.where_rows(filter);
    }
    if let Some((column, mask)) = mask {
        rule = rule.masking(column, mask);
    }
    PolicySet::new().with(rule)
}

/// The common case: a rule in `acme`.
fn policy(role: &str, filter: Option<&str>, mask: Option<(&str, Mask)>) -> PolicySet {
    policy_in("acme", role, filter, mask)
}

fn digest_for(principal: &Principal, policy: &PolicySet) -> u64 {
    Guard::authorize(policy, principal, &orders(), Action::Read)
        .expect("the principal may read")
        .scope_digest()
}

#[test]
fn two_principals_with_the_same_entitlements_share_a_scope() {
    // The property that makes the cache worth having. Keying on the principal instead would
    // give a thousand analysts a thousand copies of the same six answers.
    let policy = policy("analyst", Some("region = 'north'"), None);
    let one = digest_for(&person("ana", "acme", &["analyst"]), &policy);
    let other = digest_for(&person("bo", "acme", &["analyst"]), &policy);

    assert_eq!(
        one, other,
        "two people with identical entitlements see identical data, so they must share the \
         work of computing it"
    );
}

#[test]
fn a_different_row_filter_is_a_different_scope() {
    // The property that must never regress. These two see different rows, so an aggregate
    // computed for one is a wrong answer for the other — and a wrong answer with no trace,
    // because a total carries no evidence of which rows it came from.
    let north = digest_for(
        &person("ana", "acme", &["analyst"]),
        &policy("analyst", Some("region = 'north'"), None),
    );
    let south = digest_for(
        &person("ana", "acme", &["analyst"]),
        &policy("analyst", Some("region = 'south'"), None),
    );

    assert_ne!(north, south, "different rows, different totals, different key");
}

#[test]
fn an_unfiltered_scope_differs_from_a_filtered_one() {
    let unrestricted = digest_for(
        &person("ana", "acme", &["analyst"]),
        &policy("analyst", None, None),
    );
    let restricted = digest_for(
        &person("ana", "acme", &["analyst"]),
        &policy("analyst", Some("region = 'north'"), None),
    );

    assert_ne!(
        unrestricted, restricted,
        "seeing everything and seeing one region are not the same scope, and the \
         unrestricted total is exactly the one that must not leak"
    );
}

#[test]
fn a_different_tenant_is_a_different_scope() {
    // The one that would be catastrophic. Two tenants must never share an aggregate however
    // identical their policies look.
    let acme = digest_for(
        &person("ana", "acme", &["analyst"]),
        &policy_in("acme", "analyst", None, None),
    );
    let other = digest_for(
        &person("ana", "globex", &["analyst"]),
        &policy_in("globex", "analyst", None, None),
    );

    assert_ne!(acme, other, "a tenant boundary is the strongest boundary there is");
}

#[test]
fn a_column_mask_is_part_of_the_scope() {
    // A masked column changes what the caller may see, so it changes what an aggregate over
    // that column means.
    let plain = digest_for(
        &person("ana", "acme", &["analyst"]),
        &policy("analyst", None, None),
    );
    let masked = digest_for(
        &person("ana", "acme", &["analyst"]),
        &policy("analyst", None, Some(("amount", Mask::Null))),
    );

    assert_ne!(plain, masked, "a masked column is a different view of the same rows");
}

#[test]
fn a_different_mask_of_the_same_column_is_a_different_scope() {
    let nulled = digest_for(
        &person("ana", "acme", &["analyst"]),
        &policy("analyst", None, Some(("amount", Mask::Null))),
    );
    let partial = digest_for(
        &person("ana", "acme", &["analyst"]),
        &policy("analyst", None, Some(("amount", Mask::Partial { keep: 2 }))),
    );

    assert_ne!(nulled, partial, "how much is revealed is part of what is visible");
}

#[test]
fn the_digest_is_stable_across_calls() {
    // A digest that varied between calls would miss every time and quietly turn the cache
    // off — which looks like nothing at all except a system that got slower.
    let policy = policy("analyst", Some("region = 'north'"), None);
    let principal = person("ana", "acme", &["analyst"]);
    assert_eq!(digest_for(&principal, &policy), digest_for(&principal, &policy));
}

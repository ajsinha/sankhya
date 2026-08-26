//! What must change the key, and what must not.
//!
//! The failures here are not stale answers. A key that omits something the answer
//! depended on returns *someone else's* answer, correctly and quickly, which is the
//! worst way for a cache to be wrong.

use proptest::prelude::*;
use sankhya_catalog::{Entitlements, PlanKey, ResultKey};

const SQL: &str = "SELECT SUM(amount) FROM orders";

fn plan() -> PlanKey {
    PlanKey::new(SQL, 1, 1)
}

fn entitled(grants: &[&str]) -> Entitlements {
    Entitlements::new(grants.iter().copied())
}

#[test]
fn different_entitlements_never_share_a_result() {
    // The breach this exists to prevent. Two users run identical SQL against identical
    // data and are entitled to different rows; a shared key serves the first user's rows
    // to the second.
    let a = ResultKey::new(plan(), &entitled(&["region:emea"]), 7);
    let b = ResultKey::new(plan(), &entitled(&["region:apac"]), 7);
    assert_ne!(a, b);

    // Including the case where one is a superset of the other, which is where a naive
    // "close enough" comparison would go wrong.
    let narrow = ResultKey::new(plan(), &entitled(&["region:emea"]), 7);
    let wide = ResultKey::new(plan(), &entitled(&["region:emea", "region:apac"]), 7);
    assert_ne!(narrow, wide);
}

#[test]
fn no_entitlements_is_not_the_same_as_any_entitlements() {
    // An empty set is a real answer -- the user sees nothing -- and must not collide with
    // a user who sees something.
    assert_ne!(
        ResultKey::new(plan(), &Entitlements::default(), 7),
        ResultKey::new(plan(), &entitled(&["region:emea"]), 7)
    );
}

#[test]
fn the_same_entitlements_in_a_different_order_share_a_result() {
    // The other direction, and it is worth having: caching per *user* rather than per
    // entitlement set would throw away every legitimate hit between two users with the
    // same grants.
    assert_eq!(
        ResultKey::new(plan(), &entitled(&["a", "b", "c"]), 7),
        ResultKey::new(plan(), &entitled(&["c", "a", "b"]), 7)
    );
    assert_eq!(
        ResultKey::new(plan(), &entitled(&["a", "b", "a"]), 7),
        ResultKey::new(plan(), &entitled(&["a", "b"]), 7)
    );
}

#[test]
fn a_policy_change_invalidates_every_plan() {
    // A plan cached before a policy tightened is a valid plan for the old policy.
    // Reusing it un-applies the change for everyone whose plan was already cached --
    // silently, and precisely for the users who were already active.
    assert_ne!(PlanKey::new(SQL, 1, 1), PlanKey::new(SQL, 1, 2));
}

#[test]
fn a_schema_change_invalidates_every_plan() {
    assert_ne!(PlanKey::new(SQL, 1, 1), PlanKey::new(SQL, 2, 1));
}

#[test]
fn a_new_snapshot_is_a_new_result_key() {
    // This is what makes invalidation free: a new version is a new key, so nothing has
    // to be found and evicted when the data changes.
    assert_ne!(
        ResultKey::new(plan(), &entitled(&["a"]), 7),
        ResultKey::new(plan(), &entitled(&["a"]), 8)
    );
}

#[test]
fn a_different_plan_is_a_different_result() {
    assert_ne!(
        ResultKey::new(PlanKey::new(SQL, 1, 1), &entitled(&["a"]), 7),
        ResultKey::new(PlanKey::new("SELECT 1", 1, 1), &entitled(&["a"]), 7)
    );
}

#[test]
fn identical_inputs_hit_the_cache() {
    // A key that never repeats is a cache that never works.
    assert_eq!(plan(), plan());
    assert_eq!(
        ResultKey::new(plan(), &entitled(&["a", "b"]), 7),
        ResultKey::new(plan(), &entitled(&["a", "b"]), 7)
    );
}

#[test]
fn the_key_is_the_same_in_every_process() {
    // A randomly-seeded hasher makes a shared cache miss everything, and makes two nodes
    // disagree about what a cache holds. These values were computed once and are pinned
    // so a change to the hash is a deliberate act rather than an upgrade side effect.
    assert_eq!(PlanKey::new(SQL, 1, 1).get(), 0xaf35_88df_d96c_7e8a);
    assert_eq!(
        ResultKey::new(plan(), &entitled(&["region:emea"]), 7).get(),
        0x787b_8b13_8082_7428
    );
}

#[test]
fn components_cannot_be_confused_for_one_another() {
    // Without length prefixing, ("ab", "c") and ("a", "bc") concatenate identically and
    // share a cache entry -- the same failure as omitting a component, arrived at
    // differently.
    assert_ne!(PlanKey::new("ab", 1, 1), PlanKey::new("a", 1, 1));
    assert_ne!(
        ResultKey::new(plan(), &entitled(&["ab", "c"]), 7),
        ResultKey::new(plan(), &entitled(&["a", "bc"]), 7)
    );
}

proptest! {
    /// Any change to any entitlement changes the key.
    #[test]
    fn entitlements_always_matter(
        base in prop::collection::vec("[a-z]{1,6}", 0..6),
        extra in "[a-z]{1,6}",
    ) {
        let before = Entitlements::new(base.clone());
        prop_assume!(!base.contains(&extra));

        let mut after_grants = base;
        after_grants.push(extra);
        let after = Entitlements::new(after_grants);

        prop_assert_ne!(
            ResultKey::new(plan(), &before, 1),
            ResultKey::new(plan(), &after, 1)
        );
    }

    /// Any change to the policy version changes every plan key.
    #[test]
    fn the_policy_version_always_matters(
        sql in "[A-Za-z ]{1,40}",
        schema in 0u64..1000,
        policy in 0u64..1000,
        bump in 1u64..100,
    ) {
        prop_assert_ne!(
            PlanKey::new(&sql, schema, policy),
            PlanKey::new(&sql, schema, policy + bump)
        );
    }
}

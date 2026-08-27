//! The member that must contribute once, and the cycle that must be refused early.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_cube_algo::hierarchy::Hierarchy;

/// A chart of accounts where one cost centre reports two ways.
///
/// ```text
///            total
///           /     \
///     region       business_line
///       /  \        /        \
///   cc_a   cc_shared        cc_b
/// ```
///
/// `cc_shared` is reachable from `total` by two paths. That is ordinary modelling, and it is
/// where a consolidation that walks paths counts it twice.
fn shared_member() -> Hierarchy {
    let mut h = Hierarchy::new();
    h.rolls_up("region", "total");
    h.rolls_up("business_line", "total");
    h.rolls_up("cc_a", "region");
    h.rolls_up("cc_shared", "region");
    h.rolls_up("cc_shared", "business_line");
    h.rolls_up("cc_b", "business_line");
    h
}

// --- the defect this module exists to prevent ---------------------------

#[test]
fn a_member_reachable_by_two_paths_contributes_once() {
    // The classic silent cube defect. The total comes out too large, every constituent is
    // correct, and the discrepancy is a plausible size — so nobody finds it, because nobody
    // reconciles a subtotal.
    let h = shared_member();

    assert_eq!(
        h.paths_to("total", "cc_shared"),
        2,
        "the fixture must genuinely have two paths or this test proves nothing"
    );

    let consolidated = h.consolidates("total").expect("acyclic");
    assert_eq!(
        consolidated,
        ["cc_a", "cc_b", "cc_shared"].into_iter().collect(),
        "three distinct leaves, however many routes reach them"
    );
    assert_eq!(consolidated.len(), 3, "not four");
}

#[test]
fn each_branch_still_sees_the_shared_member() {
    // Contributing once to the *total* must not mean contributing to only one branch. Both
    // sub-totals legitimately include it, and they legitimately do not add up to the total —
    // which is a thing to explain to a user, not a bug to fix.
    let h = shared_member();
    assert!(h.consolidates("region").expect("acyclic").contains("cc_shared"));
    assert!(h
        .consolidates("business_line")
        .expect("acyclic")
        .contains("cc_shared"));

    let region = h.consolidates("region").expect("acyclic").len();
    let line = h.consolidates("business_line").expect("acyclic").len();
    let total = h.consolidates("total").expect("acyclic").len();
    assert_eq!(region + line, 4);
    assert_eq!(total, 3, "the parts overlap, so they exceed the whole");
}

#[test]
fn a_deeper_diamond_still_contributes_once() {
    // Two paths of different lengths, which is where a depth-based deduplication would fail
    // and a set does not.
    let mut h = Hierarchy::new();
    h.rolls_up("mid", "top");
    h.rolls_up("deep_a", "top");
    h.rolls_up("deep_b", "mid");
    h.rolls_up("leaf", "deep_a");
    h.rolls_up("leaf", "deep_b");

    assert_eq!(h.paths_to("top", "leaf"), 2);
    assert_eq!(
        h.consolidates("top").expect("acyclic"),
        ["leaf"].into_iter().collect()
    );
}

// --- ragged, because ragged is the general case -------------------------

#[test]
fn branches_of_different_depths_need_no_padding() {
    // Padding a short branch to match a long one invents members that do not exist. They
    // then appear in results and in member counts, and a user asked why a division shows up
    // at four levels has been handed an implementation detail as their problem.
    let mut h = Hierarchy::new();
    h.rolls_up("shallow_leaf", "top");
    h.rolls_up("mid", "top");
    h.rolls_up("lower", "mid");
    h.rolls_up("deep_leaf", "lower");

    assert_eq!(
        h.consolidates("top").expect("acyclic"),
        ["shallow_leaf", "deep_leaf"].into_iter().collect()
    );
    assert_eq!(h.leaves(), ["shallow_leaf", "deep_leaf"].into_iter().collect());
}

#[test]
fn a_leaf_consolidates_to_itself() {
    // So a caller need not special-case the bottom of the tree, which is where an off-by-one
    // in a consolidation usually is.
    let h = shared_member();
    assert_eq!(
        h.consolidates("cc_a").expect("acyclic"),
        ["cc_a"].into_iter().collect()
    );
}

#[test]
fn a_member_nobody_declared_consolidates_to_nothing() {
    // Empty rather than an error: asking about a member that is not in the hierarchy is a
    // question with an answer, and the answer is that it contributes nothing.
    let h = shared_member();
    assert!(h.consolidates("cc_nowhere").expect("acyclic").is_empty());
}

// --- cycles -------------------------------------------------------------

#[test]
fn a_cycle_is_refused_and_the_path_is_named() {
    // "This hierarchy has a cycle" sends somebody to read the whole definition. A named path
    // sends them to one edge.
    let mut h = Hierarchy::new();
    h.rolls_up("b", "a");
    h.rolls_up("c", "b");
    h.rolls_up("a", "c");

    let refused = h.validate().expect_err("a → b → c → a");
    assert!(refused.cycle.len() >= 3, "{:?}", refused.cycle);
    assert_eq!(
        refused.cycle.first(),
        refused.cycle.last(),
        "the reported path closes"
    );
    assert!(
        refused.to_string().contains("never returns"),
        "the message says what a cycle looks like at query time, because that is how it is \
         otherwise found: {refused}"
    );
}

#[test]
fn a_hierarchy_that_is_entirely_a_cycle_is_still_caught() {
    // It has no roots, so a validation that walks down from the roots visits nothing and
    // reports nothing — a clean bill of health for a definition that cannot be consolidated
    // at all.
    let mut h = Hierarchy::new();
    h.rolls_up("x", "y");
    h.rolls_up("y", "x");
    assert!(h.roots().is_empty(), "the fixture has no root");
    assert!(h.validate().is_err(), "and it is still refused");
}

#[test]
fn a_self_referencing_member_is_a_cycle() {
    let mut h = Hierarchy::new();
    h.rolls_up("a", "a");
    assert!(h.validate().is_err());
}

#[test]
fn consolidating_checks_for_a_cycle_itself() {
    // It should have been refused at definition time. It is checked here too, because a
    // function that recurses over caller-supplied structure must not depend on somebody else
    // having validated it — that dependency is how an unbounded recursion reaches
    // production.
    let mut h = Hierarchy::new();
    h.rolls_up("b", "a");
    h.rolls_up("a", "b");
    assert!(h.consolidates("a").is_err());
}

#[test]
fn a_diamond_is_not_a_cycle() {
    // The distinction that matters: two paths to the same member is ordinary modelling, and
    // a cycle detector that flags it would refuse every real chart of accounts.
    assert!(shared_member().validate().is_ok());
}

// --- the shape of the structure -----------------------------------------

#[test]
fn declaring_the_same_edge_twice_changes_nothing() {
    // A definition that lists a relationship twice means what one listing it once means.
    let mut h = Hierarchy::new();
    h.rolls_up("leaf", "top");
    h.rolls_up("leaf", "top");
    assert_eq!(h.children_of("top"), ["leaf"].into_iter().collect());
    assert_eq!(h.consolidates("top").expect("acyclic").len(), 1);
}

#[test]
fn roots_and_leaves_are_what_they_sound_like() {
    let h = shared_member();
    assert_eq!(h.roots(), ["total"].into_iter().collect());
    assert_eq!(
        h.leaves(),
        ["cc_a", "cc_b", "cc_shared"].into_iter().collect()
    );
}

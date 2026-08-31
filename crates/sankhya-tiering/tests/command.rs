//! Planning, the digest an approval is, and the seven permissions one person may not stack.
//!
//! # What the digest is for, said as the mistake it catches
//!
//! An approval is read by a person and the arguments are typed by a machine, and between those
//! two the ranges are where a mistake hides. A digest that were merely a token would say
//! "somebody planned something recently". This one is taken over the cluster, the policy, the
//! table and every range, so it says "somebody planned **this**" — and a plan approved for one
//! range cannot authorise another by editing the command line.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_tiering::command::{clear, propose, Invocation, Proposal, Rejected, VALID_FOR_HOURS};
use sankhya_tiering::permission::{may, Denied, Permission, Policy, Principal};
use sankhya_tiering::policy::Ineligible;
use sankhya_tiering::registry::Range;

const HOUR: i64 = 3_600 * 1_000_000;
const T0: i64 = 1_700_000_000_000_000;

fn proposal() -> Proposal {
    propose(
        "prod-eu-1",
        "records-archive",
        "entries",
        vec![Range::new(0, 100), Range::new(100, 200)],
        vec![Range::new(200, 300)],
        Vec::new(),
        T0,
    )
}

fn invocation(from: &Proposal) -> Invocation {
    Invocation {
        cluster_asserted: "prod-eu-1".to_string(),
        digest: from.digest(),
        ranges: from.moves.clone(),
        change_reference: "CHG-4471".to_string(),
    }
}

#[test]
fn a_plan_that_meets_every_requirement_is_cleared() {
    let proposal = proposal();
    let cleared = clear(&proposal, "prod-eu-1", &invocation(&proposal), T0 + HOUR).unwrap();
    assert_eq!(cleared.change_reference(), "CHG-4471");
    assert_eq!(cleared.proposal().moves.len(), 2);
    assert!(proposal.is_runnable());
}

#[test]
fn editing_a_range_after_approval_invalidates_the_digest() {
    // The failure the binding exists for. The approver read a plan covering [0, 200); the
    // command line says [0, 300). Nothing about the token changed, and everything about the
    // plan did.
    let proposal = proposal();
    let mut edited = invocation(&proposal);
    edited.ranges = vec![Range::new(0, 100), Range::new(100, 300)];

    assert_eq!(
        clear(&proposal, "prod-eu-1", &edited, T0 + HOUR).err(),
        Some(Rejected::DigestMismatch)
    );
}

#[test]
fn a_digest_from_a_different_plan_does_not_authorise_this_one() {
    let proposal = proposal();
    let other = propose(
        "prod-eu-1",
        "records-archive",
        "entries",
        vec![Range::new(0, 100)],
        Vec::new(),
        Vec::new(),
        T0,
    );
    let mut borrowed = invocation(&proposal);
    borrowed.digest = other.digest();

    assert_eq!(
        clear(&proposal, "prod-eu-1", &borrowed, T0 + HOUR).err(),
        Some(Rejected::DigestMismatch)
    );
}

#[test]
fn two_plans_differing_only_by_cluster_have_different_digests() {
    // Otherwise a plan approved on staging would authorise the same ranges on production, which
    // is the cluster assertion defeated by the thing meant to complement it.
    let here = proposal();
    let there = propose(
        "staging-eu-1",
        "records-archive",
        "entries",
        here.moves.clone(),
        here.remains.clone(),
        Vec::new(),
        T0,
    );
    assert_ne!(here.digest(), there.digest());
}

#[test]
fn the_order_of_the_ranges_is_part_of_the_plan() {
    let forwards = proposal();
    let backwards = propose(
        "prod-eu-1",
        "records-archive",
        "entries",
        vec![Range::new(100, 200), Range::new(0, 100)],
        forwards.remains.clone(),
        Vec::new(),
        T0,
    );
    assert_ne!(forwards.digest(), backwards.digest());
}

#[test]
fn a_runbook_copied_from_another_environment_is_refused() {
    // How the right command gets run against the wrong database.
    let proposal = proposal();
    let mut stale = invocation(&proposal);
    stale.cluster_asserted = "staging-eu-1".to_string();

    let refusal = clear(&proposal, "prod-eu-1", &stale, T0 + HOUR).expect_err("wrong cluster");
    assert_eq!(
        refusal,
        Rejected::WrongCluster {
            asserted: "staging-eu-1".to_string(),
            actual: "prod-eu-1".to_string()
        }
    );
    assert!(refusal.to_string().contains("wrong database"));
}

#[test]
fn an_approval_does_not_survive_the_table_moving_on() {
    // A plan is a statement about a table's contents at a moment. An approval with no expiry
    // approves whatever the table holds when somebody gets round to running it, which is not
    // what the approver read.
    let proposal = proposal();
    let late = T0 + (VALID_FOR_HOURS + 1) * HOUR;

    let refusal = clear(&proposal, "prod-eu-1", &invocation(&proposal), late)
        .expect_err("the digest expired");
    assert!(matches!(refusal, Rejected::Expired { .. }));
    assert!(refusal.to_string().contains("gets round to it"));
}

#[test]
fn the_last_valid_instant_still_clears_and_the_next_does_not() {
    let proposal = proposal();
    let expires = proposal.expires_at();
    assert!(clear(&proposal, "prod-eu-1", &invocation(&proposal), expires - 1).is_ok());
    assert!(clear(&proposal, "prod-eu-1", &invocation(&proposal), expires).is_err());
}

#[test]
fn a_purge_with_no_change_record_is_refused() {
    let proposal = proposal();
    let mut untraceable = invocation(&proposal);
    untraceable.change_reference = "   ".to_string();

    let refusal =
        clear(&proposal, "prod-eu-1", &untraceable, T0 + HOUR).expect_err("no reference");
    assert_eq!(refusal, Rejected::NoChangeReference);
    assert!(refusal.to_string().contains("account for afterwards"));
}

#[test]
fn every_failing_precondition_is_carried_rather_than_counted() {
    // The one place this check reports all of them instead of the first, because these are the
    // ones somebody has to go and fix.
    let mut proposal = proposal();
    proposal.refusals = vec![Ineligible::NotAppendOnly, Ineligible::NoPrimaryKey];

    let refusal = clear(&proposal, "prod-eu-1", &invocation(&proposal), T0 + HOUR)
        .expect_err("two preconditions");
    assert_eq!(
        refusal,
        Rejected::PreconditionsFailed {
            refusals: vec![Ineligible::NotAppendOnly, Ineligible::NoPrimaryKey]
        }
    );
    assert!(refusal.to_string().contains("2 failing precondition"));
    assert!(!proposal.is_runnable());
}

#[test]
fn a_plan_that_moves_nothing_is_not_run() {
    let mut proposal = proposal();
    proposal.moves.clear();
    assert_eq!(
        clear(&proposal, "prod-eu-1", &invocation(&proposal), T0 + HOUR).err(),
        Some(Rejected::NothingToDo)
    );
}

#[test]
fn planning_reports_what_moves_and_what_remains() {
    let said = proposal().to_string();
    assert!(said.contains("2 range(s) move"), "{said}");
    assert!(said.contains("1 remain"), "{said}");
    assert!(said.contains("entries"), "{said}");
}

#[test]
fn a_plan_prints_every_refusal_it_carries() {
    let mut proposal = proposal();
    proposal.refusals = vec![Ineligible::NoColumns, Ineligible::IdentifiersNotVaulted];
    assert_eq!(proposal.to_string().matches("refused:").count(), 2);
}

#[test]
fn the_definer_of_a_policy_may_not_approve_or_purge_under_it() {
    // The person this catches is not a malicious one. It is a competent one working alone at
    // the end of a long day, approving their own work because they are the person who
    // understands it.
    let policy = Policy { name: "records-archive".to_string(), definer: "alex".to_string() };
    let alex = Principal::holding(
        "alex",
        &[Permission::Define, Permission::Approve, Permission::Purge, Permission::Execute],
    );

    for permission in [Permission::Approve, Permission::Purge, Permission::Execute] {
        let refusal = may(&alex, permission, &policy).expect_err("own work");
        assert_eq!(
            refusal,
            Denied::OwnWork {
                principal: "alex".to_string(),
                permission,
                policy: "records-archive".to_string()
            }
        );
    }
    assert!(may(&alex, Permission::Define, &policy).is_ok(), "defining it again is not the risk");
}

#[test]
fn somebody_else_holding_the_same_permissions_may_use_them() {
    // Holding both permissions is normal in a small team. Using both on the *same policy* is
    // what is refused, which is why the check is against the policy rather than in the abstract.
    let policy = Policy { name: "records-archive".to_string(), definer: "alex".to_string() };
    let priya = Principal::holding("priya", &[Permission::Approve, Permission::Purge]);

    assert!(may(&priya, Permission::Approve, &policy).is_ok());
    assert!(may(&priya, Permission::Purge, &policy).is_ok());
}

#[test]
fn a_permission_not_held_is_refused_before_anything_else_is_asked() {
    let policy = Policy { name: "records-archive".to_string(), definer: "alex".to_string() };
    let sam = Principal::holding("sam", &[Permission::Rehydrate]);

    assert_eq!(
        may(&sam, Permission::Purge, &policy),
        Err(Denied::NotHeld { principal: "sam".to_string(), permission: Permission::Purge })
    );
}

#[test]
fn the_seven_permissions_are_distinct_and_named() {
    // `FR-TIER-33` lists seven. Walking them rather than naming them means a permission added
    // later has to pass this, and a duplicate name fails it.
    assert_eq!(Permission::ALL.len(), 7);
    let mut names: Vec<&str> = Permission::ALL.iter().map(Permission::name).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), 7, "two permissions share a name");

    // The rehydrate and retire permissions are distinct from purge, which is the point of
    // listing them separately: reading an archive back is not removing anything.
    for harmless in [Permission::Rehydrate, Permission::Retire, Permission::Drop] {
        assert!(!harmless.conflicts_with_defining(), "{harmless} is not an approval of one's own work");
    }
}

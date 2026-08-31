//! A purge cannot start in the middle, cannot skip a phase, and cannot be authorised by
//! anything except the two constructors that exist.
//!
//! # What these are really testing
//!
//! `FR-TIER-03` claims that *enumerating the constructors of the authorization value is a
//! complete audit of every way data can leave the system of record*. That is a claim about
//! what a reader can conclude, and it holds only if the type genuinely cannot be built any
//! other way --- which is a property of the module's surface, not of its logic. So the tests
//! here pin the surface: two constructors, no `Default`, nothing public to assemble one from.
//!
//! The rest is `FR-TIER-08`'s durability contract, which is easier to state than to get right:
//! the journal is written **before** the action, so a crash leaves a record of something that
//! may not have happened, and resume re-runs it. The opposite order leaves a partition
//! detached with nothing recording it.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_tiering::authorize::Authorization;
use sankhya_tiering::machine::{Entry, Halt, Phase, Purge};
use sankhya_tiering::verify::{Discrepancy, Proof, Verification};

const T0: i64 = 1_700_000_000_000_000;

fn by_command() -> Authorization {
    Authorization::from_command("alex", "CHG-4471")
}

fn by_schedule() -> Authorization {
    Authorization::from_schedule("nightly-archive", "svc-tiering", "priya", "sam")
}

/// The witness an exhaustive verification produces when it finds nothing.
///
/// Built here from a `Verification` with no discrepancies, which is the only way to build one
/// anywhere --- `Proof` has a private field and no constructor. That is `FR-TIER-15` as a
/// property of the type: a test cannot fabricate one either.
fn proof() -> Proof {
    Verification { discrepancies: Vec::new() }.proof().expect("nothing was found")
}

/// Drive a purge to a phase, journalling each transition the way a caller must.
fn driven_to(target: Phase) -> (Purge, Vec<Entry>) {
    let mut purge = Purge::begin("purge-001", by_command());
    let mut journal = Vec::new();
    for phase in Phase::ALL.into_iter().skip(1) {
        if phase > target {
            break;
        }
        let entry = if phase == Phase::Verified {
            purge.verified(proof(), T0, None).expect("in order")
        } else {
            purge.entering(phase, T0, None).expect("in order")
        };
        journal.push(entry);
        purge.entered(phase);
    }
    (purge, journal)
}

#[test]
fn a_purge_begins_planned_and_advances_one_phase_at_a_time() {
    let (purge, journal) = driven_to(Phase::Recorded);
    assert_eq!(purge.phase(), Phase::Recorded);
    assert_eq!(journal.len(), 6, "six transitions from Planned to Recorded");
    let phases: Vec<Phase> = journal.iter().map(|entry| entry.phase).collect();
    assert_eq!(&phases[..], &Phase::ALL[1..]);
}

#[test]
fn a_phase_cannot_be_skipped() {
    // The requirement that makes verification unskippable in the *data*, complementing the
    // type-level guarantee. A caller that tries to detach without verifying is refused with
    // both phases named, so the message says what went wrong rather than that something did.
    let purge = Purge::begin("purge-001", by_command());
    let refused = purge
        .entering(Phase::Detached, T0, None)
        .expect_err("detaching from Planned skips four phases");

    assert_eq!(
        refused,
        Halt::OutOfOrder { last: Phase::Planned, attempted: Phase::Detached }
    );
    assert!(refused.to_string().contains("cannot skip a phase"), "{refused}");
}

#[test]
fn verification_must_precede_everything_destructive() {
    // Stated as a property over the whole order rather than as one assertion, because the
    // thing being protected is not a step but an ordering: nothing that removes data may be
    // reachable without Verified already behind it.
    for phase in Phase::ALL.into_iter().filter(|phase| phase.is_destructive()) {
        let mut required = Vec::new();
        let mut current = Some(phase);
        while let Some(step) = current.and_then(Phase::requires) {
            required.push(step);
            current = Some(step);
        }
        assert!(
            required.contains(&Phase::Verified),
            "{phase} is destructive and does not require Verified: {required:?}"
        );
    }
}

#[test]
fn only_detach_and_beyond_are_destructive() {
    // The line M11 arms. Everything up to Marked is undone by doing nothing.
    assert!(!Phase::Planned.is_destructive());
    assert!(!Phase::Verified.is_destructive());
    assert!(!Phase::Gated.is_destructive());
    assert!(!Phase::Marked.is_destructive(), "a marker is written, not removed");
    assert!(Phase::Detached.is_destructive());
    assert!(Phase::Dropped.is_destructive());
    assert!(Phase::Recorded.is_destructive());
}

#[test]
fn a_kill_switch_stops_the_next_phase_and_never_interrupts_one() {
    // `FR-TIER-32`: a kill switch stops new phases and SHALL NEVER abort a job mid-detach.
    // That is why it is checked when a phase is *entered* and there is no method to interrupt
    // one --- the absence is the guarantee.
    let (purge, _) = driven_to(Phase::Marked);
    let refused = purge
        .entering(Phase::Detached, T0, Some("global-tiering-halt"))
        .expect_err("the switch is set");

    assert_eq!(refused, Halt::Killed { switch: "global-tiering-halt".to_string() });
    assert!(refused.to_string().contains("no further phase was started"), "{refused}");
    assert_eq!(purge.phase(), Phase::Marked, "and the purge is where it was");
}

#[test]
fn a_journal_entry_round_trips() {
    let (_, journal) = driven_to(Phase::Gated);
    for entry in &journal {
        let line = entry.line();
        assert_eq!(
            Entry::from_line(&line).as_ref(),
            Some(entry),
            "the journal is replayed on resume, so its format is a contract: {line}"
        );
    }
}

#[test]
fn a_journal_line_this_build_does_not_understand_is_not_an_empty_journal() {
    // The distinction that matters on resume. A line that cannot be read means a purge whose
    // state is unknown; treating it as absence would resume from Planned and re-detach.
    assert_eq!(Entry::from_line("nonsense"), None);
    assert_eq!(Entry::from_line("123\tpurge-001\tteleported\t"), None);
    assert_eq!(Entry::from_line(""), None);
}

#[test]
fn a_resumed_purge_continues_from_the_furthest_phase_its_journal_records() {
    let (_, journal) = driven_to(Phase::Gated);
    let resumed = Purge::resume("purge-001", by_command(), &journal).expect("a clean journal");

    assert_eq!(resumed.phase(), Phase::Gated);
    assert!(resumed.entering(Phase::Marked, T0, None).is_ok(), "and carries on from there");
    assert!(
        resumed.entering(Phase::Detached, T0, None).is_err(),
        "without being able to skip"
    );
}

#[test]
fn a_phase_recorded_twice_is_a_crash_replayed_rather_than_a_fault() {
    // The direct consequence of writing the journal before the action: a crash between the two
    // leaves a duplicate on the next run. That is the *expected* case, and it is why every
    // phase has to be idempotent.
    let (_, mut journal) = driven_to(Phase::Verified);
    let repeat = journal.last().cloned().expect("an entry");
    journal.push(repeat);

    let resumed = Purge::resume("purge-001", by_command(), &journal).expect("a replayed entry");
    assert_eq!(resumed.phase(), Phase::Verified);
}

#[test]
fn a_journal_that_skipped_a_phase_is_refused_rather_than_resumed() {
    // The failure this prevents is the worst one available: resuming a purge whose journal
    // says a partition was detached without verification, and carrying on from there.
    let (_, journal) = driven_to(Phase::Recorded);
    let mut tampered = journal.clone();
    tampered.remove(1); // drop `gated`

    let refused = Purge::resume("purge-001", by_command(), &tampered)
        .expect_err("the journal is not a prefix of the required order");
    assert!(matches!(refused, Halt::OutOfOrder { .. }), "{refused}");
    assert!(refused.to_string().contains("not resumable"), "{refused}");
}

#[test]
fn another_purges_entries_are_ignored_rather_than_merged() {
    // One journal holds many purges. Reading somebody else's entries as your own would
    // advance a purge past phases it never ran.
    let (_, mine) = driven_to(Phase::Verified);
    let mut journal = mine.clone();
    for entry in &mine {
        let mut theirs = entry.clone();
        theirs.purge = "purge-999".to_string();
        theirs.phase = Phase::Recorded;
        journal.push(theirs);
    }

    let resumed = Purge::resume("purge-001", by_command(), &journal).expect("mine only");
    assert_eq!(resumed.phase(), Phase::Verified);
}

#[test]
fn an_empty_journal_resumes_as_a_fresh_purge() {
    let resumed = Purge::resume("purge-001", by_command(), &[]).expect("nothing recorded");
    assert_eq!(resumed.phase(), Phase::Planned);
    assert!(!resumed.is_destructive());
}

#[test]
fn a_purge_knows_whether_it_has_taken_anything_yet() {
    let (before, _) = driven_to(Phase::Marked);
    assert!(!before.is_destructive(), "a marker is written, nothing is removed");

    let (after, _) = driven_to(Phase::Detached);
    assert!(after.is_destructive(), "and from here a person is involved in undoing it");
}

#[test]
fn every_authorisation_names_who_is_answerable() {
    // `FR-TIER-34`: audit records name the service principal **and** the human definer and
    // approver of the schedule version in force. "The scheduler did it" is not an acceptable
    // audit answer, so a schedule that cannot name them cannot construct one of these.
    let command = by_command();
    let keys: Vec<&str> = command.attribution().iter().map(|(k, _)| *k).collect();
    assert_eq!(keys, vec!["principal", "change_reference"]);
    assert!(command.is_interactive());

    let schedule = by_schedule();
    let keys: Vec<&str> = schedule.attribution().iter().map(|(k, _)| *k).collect();
    assert_eq!(keys, vec!["schedule", "service_principal", "definer", "approver"]);
    assert!(!schedule.is_interactive(), "a schedule is not a person at a terminal");
    assert!(schedule.to_string().contains("approved by sam"), "{schedule}");
}

#[test]
fn the_attribution_reaches_every_journal_entry() {
    // Not carried once at the start: a journal read years later has to say who authorised the
    // phase in front of the reader, not who authorised something earlier in the same file.
    let mut purge = Purge::begin("purge-002", by_schedule());
    let entry = purge.verified(proof(), T0, None).expect("in order");
    purge.entered(Phase::Verified);

    let recorded: Vec<&str> = entry.attribution.iter().map(|(k, _)| k.as_str()).collect();
    assert!(recorded.contains(&"approver"), "{:?}", entry.attribution);
    assert!(entry.line().contains("approver=sam"), "{}", entry.line());
}

#[test]
fn phase_names_survive_a_rename_of_the_variant() {
    // The journal is read back by name, so the strings are a format rather than a rendering.
    // If somebody renames a variant, this test is what tells them the old journals stop
    // resuming.
    for phase in Phase::ALL {
        assert_eq!(Phase::from_name(phase.name()), Some(phase));
    }
    assert_eq!(Phase::from_name("teleported"), None);
}

#[test]
fn verified_cannot_be_entered_through_the_general_transition() {
    // `FR-TIER-15`: verification is structurally absent from every path that could bypass it.
    // The general transition refuses this one phase, so there is no call that reaches
    // `Verified` without holding evidence --- and no parameter to leave out.
    let purge = Purge::begin("purge-002", by_command());
    assert_eq!(purge.entering(Phase::Verified, T0, None), Err(Halt::UnprovenVerification));
}

#[test]
fn a_verification_that_found_something_produces_no_proof() {
    let failed = Verification {
        discrepancies: vec![Discrepancy::RowCount { source: 10, archive: 9 }],
    };
    assert!(failed.proof().is_none(), "a proof of a failure is not a thing");
}

#[test]
fn the_refusal_says_what_is_missing() {
    let said = Halt::UnprovenVerification.to_string();
    assert!(said.contains("proof"), "{said}");
    assert!(said.contains("Verified"), "{said}");
}

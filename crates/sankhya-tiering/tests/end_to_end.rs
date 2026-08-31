//! M9's exit criteria, demonstrated rather than asserted.
//!
//! The plan asks for three things: *"purge demonstrated end to end with verification, quarantine
//! and rollback; the anomaly guard demonstrated halting an intentionally-defective policy; every
//! rejected purge path shown to fail closed."*
//!
//! Each of the eleven pieces has its own tests, and passing all of them is not the same claim.
//! A module can be right and the composition still wrong: a witness that nothing asks for, a
//! phase order nothing walks, a registry entry nothing writes. These three tests walk the whole
//! path with the real types, in the order an operator would.
//!
//! # What is still not demonstrated here, and cannot be
//!
//! **The gate.** `M9`'s third criterion needs an attestation drill run against a real
//! non-production archive, and the first moved to `M11` because it needs a production
//! deployment. Nothing in this file clears either, and **destructive purge against a system of
//! record stays disabled until `M11`** whatever these tests show. Building it and arming it are
//! two decisions and only the first belongs to development.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use arrow_array::{Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use sankhya_schema::{LogicalType, Precision};
use sankhya_tiering::authorize::Authorization;
use sankhya_tiering::command::{clear, propose, Invocation, Rejected};
use sankhya_tiering::defence::{At, Change, Extents, Verdict};
use sankhya_tiering::evidence::Pack;
use sankhya_tiering::machine::{Halt, Phase, Purge};
use sankhya_tiering::migrate::migrate;
use sankhya_tiering::permission::{may, Permission, Principal};
use sankhya_tiering::policy::{Column, Contract, Ineligible, Policy, Retention};
use sankhya_tiering::quarantine::{Grace, Held, Kept, Quarantine};
use sankhya_tiering::registry::{Range, Registry};
use sankhya_tiering::rehydrate::{Expiry, Target};
use sankhya_tiering::schedule::{Halted, Schedule, Stage};
use sankhya_tiering::unify::{mutable, plan, Read, Unservable};
use sankhya_tiering::verify::{compare, Hash, Scan};
use sankhya_tiering::ArchiveEntry;
use std::sync::Arc;

const DAY: i64 = 86_400 * 1_000_000;
const HOUR: i64 = 3_600 * 1_000_000;
const T0: i64 = 1_700_000_000_000_000;
const DOMAIN: Range = Range::new(0, 300);

/// An immutable record table that satisfies every eligibility rule.
fn eligible_policy() -> Policy {
    Policy {
        name: "records-archive".to_string(),
        table: "entries".to_string(),
        tiering_key: "sank_data_date".to_string(),
        columns: vec![
            Column::new("sank_data_date", LogicalType::Date),
            Column::key("posting_id", LogicalType::Uuid),
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

fn batch(ids: &[i64], names: &[&str]) -> RecordBatch {
    let schema = Schema::new(vec![
        Field::new("posting_id", DataType::Int64, false),
        Field::new("narrative", DataType::Utf8, true),
    ]);
    RecordBatch::try_new(
        Arc::new(schema),
        vec![
            Arc::new(Int64Array::from(ids.to_vec())),
            Arc::new(StringArray::from(names.to_vec())),
        ],
    )
    .expect("same length")
}

fn fingerprint(batch: &RecordBatch) -> sankhya_tiering::Fingerprint {
    let mut scan =
        Scan::new(vec!["posting_id".to_string()], vec!["narrative".to_string()]).unwrap();
    scan.absorb(batch).expect("encodes");
    scan.finish()
}

fn archived(from: i64, until: i64, keys: Hash) -> ArchiveEntry {
    ArchiveEntry {
        table: "entries".to_string(),
        range: Range::new(from, until),
        archive: format!("s3://archive/entries/{from}-{until}"),
        snapshot: "snap-1".to_string(),
        rows: 3,
        keys,
        columns: Vec::new(),
        archived_at: T0,
        retention: Retention::new("7-year statutory record retention", 2557),
        legal_hold: false,
        attribution: vec![("principal".to_string(), "priya".to_string())],
    }
}

#[test]
fn a_purge_runs_end_to_end_and_is_rolled_back_out_of_quarantine() {
    // ---- eligibility -------------------------------------------------------------------
    let policy = eligible_policy();
    assert!(policy.eligible().is_eligible(), "{}", policy.eligible());

    // ---- separation of duty ------------------------------------------------------------
    let definer = sankhya_tiering::permission::Policy {
        name: policy.name.clone(),
        definer: "alex".to_string(),
    };
    let priya = Principal::holding("priya", &[Permission::Approve, Permission::Purge]);
    may(&priya, Permission::Purge, &definer).expect("priya did not write it");

    // ---- planning, which does nothing --------------------------------------------------
    let proposal = propose(
        "prod-eu-1",
        &policy.name,
        &policy.table,
        vec![Range::new(0, 100)],
        vec![Range::new(100, 300)],
        Vec::new(),
        T0,
    );
    assert!(proposal.is_runnable());

    // ---- the invocation, bound to the exact ranges -------------------------------------
    let invocation = Invocation {
        cluster_asserted: "prod-eu-1".to_string(),
        digest: proposal.digest(),
        ranges: proposal.moves.clone(),
        change_reference: "CHG-4471".to_string(),
    };
    let cleared = clear(&proposal, "prod-eu-1", &invocation, T0 + HOUR).expect("cleared");
    assert_eq!(cleared.change_reference(), "CHG-4471");

    // ---- verification, which is what produces the proof --------------------------------
    let source = fingerprint(&batch(&[1, 2, 3], &["a", "b", "c"]));
    let archive = fingerprint(&batch(&[1, 2, 3], &["a", "b", "c"]));
    let verification = compare(3, &source, &archive);
    assert!(verification.passed(), "{:?}", verification.discrepancies);
    let proof = verification.proof().expect("nothing was found");

    // ---- the phase chain, journalled before each action --------------------------------
    let mut purge = Purge::begin("purge-001", Authorization::from_command("priya", "CHG-4471"));
    let mut journal = Vec::new();

    journal.push(purge.verified(proof, T0 + HOUR, None).expect("in order"));
    purge.entered(Phase::Verified);
    for phase in [Phase::Gated, Phase::Marked, Phase::Detached, Phase::Dropped, Phase::Recorded] {
        journal.push(purge.entering(phase, T0 + HOUR, None).expect("in order"));
        purge.entered(phase);
    }
    assert_eq!(purge.phase(), Phase::Recorded);
    assert_eq!(journal.len(), 6);
    assert!(purge.is_destructive());

    // Every entry names who authorised it, not just the first.
    for entry in &journal {
        assert!(
            entry.attribution.iter().any(|(key, value)| key == "principal" && value == "priya"),
            "a journal read years later has to say who authorised the phase in front of it"
        );
    }

    // ---- the registry, and the marker written before the detach ------------------------
    let mut registry = Registry::new();
    registry.record(archived(0, 100, source.keys.root)).expect("no overlap");
    let marker = registry.entries()[0].marker();

    // ---- the evidence pack, from the marker and nothing else ---------------------------
    let pack = Pack::from_marker(&marker).expect("readable");
    let seal = pack.seal(b"an operator's key");
    assert!(pack.sealed_by(b"an operator's key", &seal));
    assert_eq!(pack.get("table"), Some("entries"));
    assert!(pack.attribution().contains(&("principal", "priya")));

    // ---- quarantine holds the detached partition ---------------------------------------
    let mut quarantine = Quarantine::new();
    quarantine.hold(Held {
        purge: "purge-001".to_string(),
        table: "entries".to_string(),
        range: Range::new(0, 100),
        storage: "/var/lib/sankhya/quarantine/purge-001".to_string(),
        detached_at: T0 + HOUR,
        grace: Grace::default(),
    });

    // ---- the query path now spans both tiers -------------------------------------------
    let servable = registry.reconcile(&[]).servable("entries").expect("reconciled clean");
    let served = plan(
        DOMAIN,
        &[Range::new(100, 300)],
        &registry,
        &servable,
        "snap-1",
        DOMAIN,
    )
    .expect("no gap");
    assert!(served.spans_tiers());
    assert_eq!(served.segments.len(), 2);
    assert!(matches!(served.segments[0].read, Read::Archive { .. }));
    assert_eq!(served.segments[1].read, Read::Source);

    // and a statement reaching the archived range is refused rather than reporting nothing
    assert!(mutable(&registry, "entries", 50).is_err());

    // ---- rollback ----------------------------------------------------------------------
    let put_back = quarantine
        .reattach(&mut registry, "purge-001", T0 + 2 * HOUR)
        .expect("inside the grace period");
    assert!(put_back.entry_withdrawn, "both halves, or neither");
    assert!(registry.entries().is_empty());
    assert!(quarantine.held().is_empty());

    // and the table is whole again: the whole domain is hot, and nothing is archived
    let after = registry.reconcile(&[]).servable("entries").expect("still clean");
    let whole = plan(DOMAIN, &[DOMAIN], &registry, &after, "snap-2", DOMAIN).expect("no gap");
    assert_eq!(whole.segments.len(), 1);
    assert_eq!(whole.segments[0].read, Read::Source);
    assert!(mutable(&registry, "entries", 50).is_ok(), "and it is writable again");
}

#[test]
fn the_anomaly_guard_halts_an_intentionally_defective_policy() {
    // The defect is deliberately of the kind nothing else catches. The policy is valid, the
    // code is correct and the schedule fires on time --- a boundary moved by a year, so the
    // candidate set is two hundred times what this schedule has ever moved.
    let mut nightly = Schedule::new("nightly-archive");
    nightly.enabled = true;
    nightly.approved = Some(Hash::from_bytes([1; 32]));
    nightly.history = vec![2, 2, 3, 2, 2];
    assert_eq!(nightly.stage, Stage::Archive, "and it was not going to purge anyway");

    // An ordinary night passes.
    assert!(nightly.admit(3, 0, None).is_ok());

    // The night after the boundary moved does not.
    let halted = nightly.admit(400, 0, None).expect_err("a year in one pass");
    let Halted::Anomalous { candidates, median, factor, .. } = halted else {
        panic!("the guard has to be the thing that stops it")
    };
    assert_eq!((candidates, median, factor), (400, 2, 3));

    // And the eligibility rules would have refused the policy behind it in any case, which is
    // the point of having both: the guard catches what a valid policy does, and eligibility
    // catches what an invalid one is.
    let mut defective = eligible_policy();
    defective.tiering_key = "posting_id".to_string();
    defective.contract = Contract::Mutable;
    let refusals = defective.eligible().refusals;
    assert!(refusals.contains(&Ineligible::NotAppendOnly));
    assert!(refusals
        .iter()
        .any(|refusal| matches!(refusal, Ineligible::TieringKeyNotOrdinal { .. })));
}

#[test]
fn every_rejected_purge_path_fails_closed() {
    // One test that walks each way a purge can be refused and asserts that it *is* refused.
    // Individually these are covered elsewhere; together they are the claim the exit criterion
    // makes --- that there is no path where a refusal quietly becomes a permission.
    let registry = Registry::from_entries(vec![archived(0, 100, Hash::from_bytes([2; 32]))])
        .expect("one entry");

    // A table that fails a precondition is not planned around.
    let mut ineligible = eligible_policy();
    ineligible.identifiers_vaulted = false;
    assert!(!ineligible.eligible().is_eligible());

    // `Verified` cannot be journalled without a proof.
    let purge = Purge::begin("purge-002", Authorization::from_command("priya", "CHG-1"));
    assert_eq!(
        purge.entering(Phase::Verified, T0, None),
        Err(Halt::UnprovenVerification)
    );

    // Nor can a destructive phase be reached by skipping to it.
    assert!(matches!(
        purge.entering(Phase::Detached, T0, None),
        Err(Halt::OutOfOrder { .. })
    ));

    // A failed verification yields no proof at all.
    let mismatched = compare(
        3,
        &fingerprint(&batch(&[1, 2, 3], &["a", "b", "c"])),
        &fingerprint(&batch(&[1, 2, 3], &["a", "b", "x"])),
    );
    assert!(!mismatched.passed());
    assert!(mismatched.proof().is_none());

    // A kill switch stops a phase starting.
    assert!(matches!(
        purge.entering(Phase::Gated, T0, Some("ops-freeze")),
        Err(Halt::Killed { .. })
    ));

    // An invocation from the wrong cluster, with no change record, or on an expired digest.
    let proposal = propose(
        "prod-eu-1",
        "records-archive",
        "entries",
        vec![Range::new(0, 100)],
        Vec::new(),
        Vec::new(),
        T0,
    );
    let good = Invocation {
        cluster_asserted: "prod-eu-1".to_string(),
        digest: proposal.digest(),
        ranges: proposal.moves.clone(),
        change_reference: "CHG-4471".to_string(),
    };
    let elsewhere = Invocation { cluster_asserted: "staging".to_string(), ..good.clone() };
    let untraceable = Invocation { change_reference: String::new(), ..good.clone() };
    let widened =
        Invocation { ranges: vec![Range::new(0, 300)], ..good.clone() };

    assert!(matches!(
        clear(&proposal, "prod-eu-1", &elsewhere, T0),
        Err(Rejected::WrongCluster { .. })
    ));
    assert_eq!(
        clear(&proposal, "prod-eu-1", &untraceable, T0).err(),
        Some(Rejected::NoChangeReference)
    );
    assert_eq!(
        clear(&proposal, "prod-eu-1", &widened, T0).err(),
        Some(Rejected::DigestMismatch)
    );
    assert!(matches!(
        clear(&proposal, "prod-eu-1", &good, T0 + 5 * HOUR),
        Err(Rejected::Expired { .. })
    ));

    // A principal acting on their own policy.
    let own = sankhya_tiering::permission::Policy {
        name: "records-archive".to_string(),
        definer: "alex".to_string(),
    };
    let alex = Principal::holding("alex", &[Permission::Define, Permission::Purge]);
    assert!(may(&alex, Permission::Purge, &own).is_err());

    // A delete or truncate reaching an archived range, and one that cannot be placed.
    let extents = Extents::of(&registry);
    for (change, at) in [
        (Change::Delete, At::Ordinal(50)),
        (Change::Delete, At::Unknown),
        (Change::Truncate, At::WholeRelation),
    ] {
        assert!(
            matches!(extents.consider("entries", change, at), Verdict::Halt(_)),
            "a {change} at {at:?} must halt the applier"
        );
    }

    // A query over a range neither tier claims.
    let servable = registry.reconcile(&[]).servable("entries").expect("clean");
    assert!(matches!(
        plan(DOMAIN, &[], &registry, &servable, "snap-1", DOMAIN),
        Err(Unservable::CoverageGap { .. })
    ));

    // A quarantined partition the registry does not claim, at any age.
    let mut quarantine = Quarantine::new();
    quarantine.hold(Held {
        purge: "purge-003".to_string(),
        table: "entries".to_string(),
        range: Range::new(200, 300),
        storage: "/var/lib/sankhya/quarantine/purge-003".to_string(),
        detached_at: T0,
        grace: Grace::default(),
    });
    let reaping = quarantine.reap(&registry, T0 + 10_000 * DAY);
    assert!(reaping.release.is_empty());
    assert_eq!(reaping.kept[0].1, Kept::Unarchived);

    // A whole-table migration with a hole in it.
    assert!(migrate(&registry, "entries", DOMAIN, T0).is_err());

    // A rehydration into a schema a publication could capture, and one that never expires.
    assert!(Target::loading_into("reports", "public", false).is_err());
    assert!(Target::loading_into("public", "public", true).is_err());
    assert!(Expiry::of(0).is_err());

    // A schedule that was never approved.
    let mut unapproved = Schedule::new("nightly-archive");
    unapproved.enabled = true;
    assert!(matches!(
        unapproved.admit(1, 0, None),
        Err(Halted::NotApproved { .. })
    ));

    // And an evidence pack that cannot say what it is evidence of.
    assert!(Pack::from_marker("table=entries").is_err());
}

//! The audit reproduces what a principal saw, and says so when it has been tampered with.
//!
//! Two claims under test, and the second is the one with a caveat worth reading. The chain
//! detects alteration, reordering and insertion. It does **not** detect truncation of the
//! tail — no local check can — and `a_truncated_chain_still_verifies_which_is_why_the_head_is_published`
//! asserts that limitation rather than hiding it.

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

use sankhya_audit::chain::{Broken, Chain, DataVersion, Entry, Hash, Record, RecordedDecision};
use sankhya_authz::policy::{Action, Mask, TableRef};
use sankhya_authz::principal::{Authentication, Principal, Role, TenantId};
use std::collections::BTreeMap;

fn tenant(name: &str) -> TenantId {
    let mut bytes = [0u8; 16];
    for (slot, byte) in bytes.iter_mut().zip(name.bytes()) {
        *slot = byte;
    }
    TenantId::from_uuid(uuid::Uuid::from_bytes(bytes))
}

fn person(name: &str, in_tenant: &str) -> Principal {
    Principal::authenticated(
        name,
        tenant(in_tenant),
        [Role::new("reader")],
        Authentication::MutualTls,
    )
    .expect("a valid principal")
}

fn orders() -> TableRef {
    TableRef::new("sales", "orders")
}

fn read_entry(who: &Principal, at: i64) -> Entry {
    let mut masks = BTreeMap::new();
    masks.insert("email".to_string(), Mask::Null);
    Entry::by(
        who,
        orders(),
        Action::Read,
        RecordedDecision::allowed(Some("region = 'north'".to_string()), &masks),
        at,
    )
    .running("SELECT id, email FROM orders")
    .from_version(DataVersion::snapshot(41).with_graph_epoch(7))
    .returning(12)
}

#[test]
fn a_record_reproduces_what_the_principal_saw() {
    // Exit criterion 4. Not "a query happened and who ran it" — the restrictions that were
    // applied and the version of the data that answered, or the record reproduces nothing.
    let mut chain = Chain::new();
    chain.append(read_entry(&person("ana", "acme"), 1_000));

    let held = chain.records();
    let record = held.first().copied().expect("one record");
    assert_eq!(record.subject, "ana");
    assert_eq!(record.authentication, "mutual-tls");
    assert_eq!(record.table, "sales.orders");
    assert_eq!(record.action, "read");
    assert!(record.decision.allowed);
    assert_eq!(
        record.decision.row_filter.as_deref(),
        Some("region = 'north'"),
        "without the filter the record says access was allowed and cannot say to what"
    );
    assert_eq!(
        record
            .decision
            .column_masks
            .get("email")
            .map(String::as_str),
        Some("null")
    );
    let version = record.data_version.as_ref().expect("a version");
    assert_eq!(version.snapshot, 41);
    assert_eq!(
        version.graph_epoch,
        Some(7),
        "a graph answered part of it, and which epoch is part of what was seen"
    );
    assert_eq!(record.rows_returned, Some(12));
}

#[test]
fn a_refusal_is_recorded_as_carefully_as_a_grant() {
    // A log containing only successful access cannot show an attempt to reach something
    // forbidden, which is the pattern an investigation is usually looking for.
    let mut chain = Chain::new();
    chain.append(Entry::by(
        &person("mal", "acme"),
        TableRef::new("hr", "salaries"),
        Action::Read,
        RecordedDecision::denied(),
        2_000,
    ));

    let held = chain.records();
    let record = held.first().copied().expect("one record");
    assert!(!record.decision.allowed);
    assert_eq!(record.table, "hr.salaries");
    assert_eq!(record.subject, "mal");
}

#[test]
fn an_intact_chain_verifies() {
    let mut chain = Chain::new();
    for at in 0..20 {
        chain.append(read_entry(&person("ana", "acme"), i64::from(at)));
    }
    assert_eq!(chain.len(), 20);
    assert!(chain.verify().is_ok());
}

#[test]
fn the_first_record_links_to_a_fixed_genesis_value() {
    // A fixed value rather than an absent one, so the first record is hashed exactly like
    // every other. An Option would mean a branch, and the branch would be the one place
    // the chain is computed differently.
    let mut chain = Chain::new();
    assert_eq!(chain.head(), Hash::genesis());
    chain.append(read_entry(&person("ana", "acme"), 1));
    let held = chain.records();
    let record = held.first().copied().expect("one record");
    assert_eq!(record.previous, Hash::genesis());
    assert_ne!(chain.head(), Hash::genesis());
}

#[test]
fn altering_a_record_is_detected() {
    let mut chain = Chain::new();
    for at in 0..5 {
        chain.append(read_entry(&person("ana", "acme"), i64::from(at)));
    }
    assert!(chain.verify().is_ok());

    // Reconstruct with one record's contents changed, leaving its digest as written.
    let mut tampered = Chain::new();
    for (index, record) in chain.records().into_iter().enumerate() {
        let mut copy = record.clone();
        if index == 2 {
            copy.rows_returned = Some(999_999);
        }
        tampered.append_raw(copy.clone());
    }

    let Err(broken) = tampered.verify() else {
        panic!("changing a record's contents must invalidate its digest");
    };
    assert_eq!(broken, Broken::Altered { at: 2 });
    assert!(broken.to_string().contains("changed since it was written"));
}

#[test]
fn removing_a_record_from_the_middle_is_detected() {
    let mut chain = Chain::new();
    for at in 0..5 {
        chain.append(read_entry(&person("ana", "acme"), i64::from(at)));
    }

    let mut without = Chain::new();
    for (index, record) in chain.records().into_iter().enumerate() {
        if index == 2 {
            continue;
        }
        without.append_raw((*record).clone());
    }

    let Err(broken) = without.verify() else {
        panic!("removing a record must break the chain");
    };
    // The record that followed the removed one now sits at position 2 and claims 3.
    assert_eq!(broken, Broken::OutOfOrder { at: 2, claims: 3 });
}

#[test]
fn removing_a_record_and_renumbering_the_rest_is_still_detected() {
    // The realistic version of the attack, and the one that isolates the link check. An
    // attacker who deletes a record and leaves the sequence numbers alone is caught by the
    // sequence check; a competent one renumbers. Then the only thing that catches it is
    // each record carrying the digest of the one before.
    //
    // Without this test the link check was never the thing that fired, and the mutation
    // audit said so: removing it entirely left every test green.
    let mut chain = Chain::new();
    for at in 0..5 {
        chain.append(read_entry(&person("ana", "acme"), i64::from(at)));
    }

    let mut forged = Chain::new();
    let mut next = 0u64;
    for (index, record) in chain.records().into_iter().enumerate() {
        if index == 2 {
            continue;
        }
        let mut copy = record.clone();
        copy.sequence = next;
        next += 1;
        forged.append_raw(copy.clone());
    }

    let Err(broken) = forged.verify() else {
        panic!("renumbering after a deletion must not produce a chain that verifies");
    };
    assert_eq!(
        broken,
        Broken::LinkMismatch { at: 2 },
        "the sequence is contiguous, so only the link can detect this"
    );
    assert!(broken.to_string().contains("removed or inserted"));
}

#[test]
fn inserting_a_forged_record_is_detected() {
    // The other direction: a record that never happened, spliced in with a plausible
    // sequence number.
    let mut chain = Chain::new();
    for at in 0..4 {
        chain.append(read_entry(&person("ana", "acme"), i64::from(at)));
    }

    let mut forged = Chain::new();
    let records: Vec<sankhya_audit::Record> = chain.records().into_iter().cloned().collect();
    let mut next = 0u64;
    for (index, record) in records.iter().enumerate() {
        let mut copy = record.clone();
        copy.sequence = next;
        next += 1;
        forged.append_raw(copy.clone());
        if index == 1 {
            let mut fake = record.clone();
            fake.sequence = next;
            fake.subject = "someone-else".to_string();
            next += 1;
            forged.append_raw(fake.clone());
        }
    }

    assert!(
        forged.verify().is_err(),
        "a spliced record must not produce a chain that verifies"
    );
}

#[test]
fn reordering_two_records_is_detected() {
    let mut chain = Chain::new();
    for at in 0..5 {
        chain.append(read_entry(&person("ana", "acme"), i64::from(at)));
    }

    let mut swapped = Chain::new();
    let records: Vec<sankhya_audit::Record> = chain.records().into_iter().cloned().collect();
    for index in [0, 1, 3, 2, 4] {
        if let Some(record) = records.get(index) {
            swapped.append_raw(record.clone());
        }
    }
    assert!(swapped.verify().is_err());
}

#[test]
fn a_truncated_chain_still_verifies_which_is_why_the_head_is_published() {
    // The limitation, asserted rather than hidden. An attacker who removes the tail leaves
    // a chain that verifies perfectly, because every remaining link is intact. Only
    // comparing the head against what was mirrored somewhere append-only detects it.
    //
    // An operator who believes a local chain proves completeness has a false sense of a
    // control they do not have, and this test exists so nobody has to discover that.
    let mut chain = Chain::new();
    for at in 0..10 {
        chain.append(read_entry(&person("ana", "acme"), i64::from(at)));
    }
    let true_head = chain.head();

    let mut truncated = Chain::new();
    for record in chain.records().into_iter().take(6) {
        truncated.append_raw((*record).clone());
    }

    assert!(
        truncated.verify().is_ok(),
        "a truncated chain verifies — this is the known limit of a local check"
    );
    assert_ne!(
        truncated.head(),
        true_head,
        "but its head differs from the one that was mirrored, which is what detects it"
    );
}

#[test]
fn one_principals_history_can_be_read_back() {
    let mut chain = Chain::new();
    chain.append(read_entry(&person("ana", "acme"), 1));
    chain.append(read_entry(&person("bo", "acme"), 2));
    chain.append(read_entry(&person("ana", "acme"), 3));

    let ana = chain.what_was_seen_by(&tenant("acme"), "ana");
    assert_eq!(ana.len(), 2);
    assert_eq!(ana.first().map(|r| r.at), Some(1));
    assert_eq!(ana.get(1).map(|r| r.at), Some(3));
}

#[test]
fn a_subject_name_in_another_tenant_does_not_match() {
    // A subject name is only unique within a tenant. Matching on the name alone would
    // return another tenant's records to whoever asked for their own.
    let mut chain = Chain::new();
    chain.append(read_entry(&person("ana", "acme"), 1));
    chain.append(read_entry(&person("ana", "other"), 2));

    assert_eq!(chain.what_was_seen_by(&tenant("acme"), "ana").len(), 1);
    assert_eq!(chain.what_was_seen_by(&tenant("other"), "ana").len(), 1);
}

#[test]
fn two_records_with_identical_contents_still_have_different_digests() {
    // Because each carries the previous digest. Without that, a duplicate record could be
    // deleted without detection: the chain would still link.
    let mut chain = Chain::new();
    let first = chain.append(read_entry(&person("ana", "acme"), 1_000));
    let second = chain.append(read_entry(&person("ana", "acme"), 1_000));
    assert_ne!(first, second);
}

// --- what a running process keeps ------------------------------------------

#[test]
fn a_chain_with_a_window_stops_growing() {
    // `OPS-04`. The records were a `Vec` that only ever grew, appended on every statement
    // **and every catalogue listing** --- every `\dt`, every JDBC metadata call, every
    // tab-completion. Roughly 3 to 5 GB a day at a hundred statements a second, and 26 GB a
    // day at a thousand, with no cap, no rotation and nowhere for it to go.
    let mut chain = Chain::keeping(8);
    for _ in 0..100 {
        chain.append(read_entry(&person("ana", "acme"), 1));
    }

    assert_eq!(
        chain.records().len(),
        8,
        "a window is a window, whatever it is fed"
    );
    // And the two figures that describe the *whole* chain still do. A count that shrank when
    // records aged out would be a count nobody could compare against what they mirrored ---
    // and comparing it is the only way a truncated chain is ever noticed.
    assert_eq!(chain.len(), 100, "the count is of everything appended");
    assert_ne!(chain.head(), Hash::genesis(), "and the head is the real head");
}

#[test]
fn a_window_verifies_the_records_it_still_has() {
    // A windowed chain cannot check a link to a record that has aged out, so it checks the
    // links it has. What it must not do is report the first record it kept as out of order,
    // which is what a naive index comparison does the moment anything is dropped.
    let mut chain = Chain::keeping(8);
    for _ in 0..100 {
        chain.append(read_entry(&person("ana", "acme"), 1));
    }
    assert!(chain.verify().is_ok(), "the window links to itself");

    // And it still detects tampering inside the window, which is the property that matters.
    let held: Vec<Record> = chain.records().into_iter().cloned().collect();
    let mut tampered = Chain::keeping(8);
    for (index, record) in held.into_iter().enumerate() {
        let mut copy = record;
        if index == 3 {
            copy.subject = "somebody else".to_string();
        }
        tampered.append_raw(copy);
    }
    assert!(
        tampered.verify().is_err(),
        "a record altered inside the window is still caught"
    );
}

#[test]
fn a_chain_that_keeps_everything_still_does() {
    // The default, which is what verifying a whole file end to end needs. A window is a
    // decision a server makes, not something the type does behind a caller's back.
    let mut chain = Chain::new();
    for _ in 0..100 {
        chain.append(read_entry(&person("ana", "acme"), 1));
    }
    assert_eq!(chain.records().len(), 100);
    assert_eq!(chain.len(), 100);
    assert!(chain.verify().is_ok());
}

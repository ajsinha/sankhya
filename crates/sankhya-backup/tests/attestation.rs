//! An archive is proven immutable by failing to damage it.
//!
//! # Why the store here is a real directory and not a mock
//!
//! The defect `RSK-28` describes is *"immutability controls silently removed by a later storage
//! policy change"*, and a mock that returns `Refused` because the test told it to cannot fail
//! that way --- it would attest the mock's honesty and say nothing about a store. So the
//! passing case is enforced by actual filesystem permissions and the refusals are real
//! `EACCES` from real syscalls.
//!
//! POSIX makes the same distinction the requirement does, which is convenient and not a
//! coincidence: a read-only **file** still deletes if its **directory** is writable, because
//! unlink is a property of the directory entry. That is the same shape as an object-lock
//! retention that stops overwrites while a lifecycle rule expires the object, and it is why
//! [`Forbidden`] enumerates the violations separately rather than asking one question.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_backup::attest::{attest, Attestation, Forbidden, Outcome, WriteOnce, PROBE};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// A store backed by a directory, immutable or not as the test asks.
struct Directory {
    root: PathBuf,
    immutable: bool,
    production: bool,
}

impl Directory {
    fn new(root: &Path, immutable: bool) -> Self {
        Self { root: root.to_path_buf(), immutable, production: false }
    }

    fn object(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    /// Make the directory and its contents writable again.
    ///
    /// Called before the temporary directory is dropped: a read-only directory cannot have its
    /// entries removed, so a test that locked one and walked away would leak it.
    fn unlock(&self) {
        let _ = std::fs::set_permissions(&self.root, std::fs::Permissions::from_mode(0o755));
        if let Ok(entries) = std::fs::read_dir(&self.root) {
            for entry in entries.flatten() {
                let _ = std::fs::set_permissions(
                    entry.path(),
                    std::fs::Permissions::from_mode(0o644),
                );
            }
        }
    }
}

impl WriteOnce for Directory {
    fn describe(&self) -> String {
        format!("directory {}", self.root.display())
    }

    fn is_non_production(&self) -> bool {
        !self.production
    }

    fn place(&self, bytes: &[u8]) -> Result<String, String> {
        let name = "probe";
        std::fs::write(self.object(name), bytes).map_err(|error| error.to_string())?;
        if self.immutable {
            // The file first, then the directory. Both are needed: read-only on the file stops
            // an overwrite, and read-only on the directory is what stops the unlink.
            std::fs::set_permissions(self.object(name), std::fs::Permissions::from_mode(0o444))
                .map_err(|error| error.to_string())?;
            std::fs::set_permissions(&self.root, std::fs::Permissions::from_mode(0o555))
                .map_err(|error| error.to_string())?;
        }
        Ok(name.to_string())
    }

    fn attempt(&self, object: &str, violation: Forbidden) -> Outcome {
        let path = self.object(object);
        let result = match violation {
            Forbidden::Overwrite => std::fs::write(&path, b"tampered").map(|()| String::new()),
            Forbidden::Delete => std::fs::remove_file(&path).map(|()| String::new()),
            Forbidden::Truncate => std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&path)
                .map(|_| String::new()),
        };
        match result {
            Ok(_) => Outcome::Allowed {
                detail: format!("the store performed the {violation} without complaint"),
            },
            Err(error) => Outcome::Refused { detail: error.to_string() },
        }
    }

    fn read(&self, object: &str) -> Result<Vec<u8>, String> {
        std::fs::read(self.object(object)).map_err(|error| error.to_string())
    }
}

/// A store that says it refused everything and quietly does not.
///
/// The one thing the contract cannot enforce by types, so it is enforced by the read-back.
struct Liar {
    root: PathBuf,
}

impl WriteOnce for Liar {
    fn describe(&self) -> String {
        "a store that reports refusals it did not make".to_string()
    }
    fn is_non_production(&self) -> bool {
        true
    }
    fn place(&self, bytes: &[u8]) -> Result<String, String> {
        std::fs::write(self.root.join("probe"), bytes).map_err(|error| error.to_string())?;
        Ok("probe".to_string())
    }
    fn attempt(&self, object: &str, _violation: Forbidden) -> Outcome {
        // Reports a refusal, and tampers anyway.
        let _ = std::fs::write(self.root.join(object), b"tampered");
        Outcome::Refused { detail: "object lock in force".to_string() }
    }
    fn read(&self, object: &str) -> Result<Vec<u8>, String> {
        std::fs::read(self.root.join(object)).map_err(|error| error.to_string())
    }
}

fn attested(store: &dyn WriteOnce) -> Attestation {
    attest(store, 1_700_000_000_000_000)
}

#[test]
fn a_store_that_refuses_every_violation_is_attested() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let store = Directory::new(dir.path(), true);

    let attestation = attested(&store);
    store.unlock();

    assert!(
        attestation.passed(),
        "a genuinely immutable store must attest: {attestation:?}"
    );
    assert_eq!(attestation.attempts.len(), 3, "every violation is attempted, not a sample");
    assert!(attestation.allowed().is_empty());
    assert!(attestation.untested().is_empty());
    assert_eq!(attestation.bytes_intact, Some(true));
    assert!(attestation.line().contains("PASS"), "{}", attestation.line());
}

#[test]
fn a_store_whose_control_is_gone_fails_and_says_which_violations_it_allowed() {
    // The load-bearing test. `RSK-28` is about a control that *was* there, and an attestation
    // that cannot fail is the false assurance the risk register is warning about.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let store = Directory::new(dir.path(), false);

    let attestation = attested(&store);

    assert!(!attestation.passed(), "a writable store must not attest");
    assert!(
        attestation.allowed().contains(&Forbidden::Overwrite),
        "and it must name what it allowed: {attestation:?}"
    );
    assert!(attestation.untested().is_empty(), "everything was attempted; it just succeeded");
    assert!(attestation.line().contains("ALLOWED"), "{}", attestation.line());
}

#[test]
fn a_control_that_stops_overwrites_and_not_deletes_is_not_a_pass() {
    // The failure mode that makes three separate questions worth asking. Object-lock retention
    // routinely stops an overwrite while a lifecycle rule expires the object, and a drill that
    // asked "is it immutable" and stopped at the first refusal would call that protected.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let path = dir.path().join("probe");
    std::fs::write(&path, PROBE).expect("placing");
    // The file is read-only; the directory is not. Overwrite fails, unlink succeeds.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).expect("locking");

    struct HalfLocked {
        root: PathBuf,
    }
    impl WriteOnce for HalfLocked {
        fn describe(&self) -> String {
            "a store with retention but no delete protection".to_string()
        }
        fn is_non_production(&self) -> bool {
            true
        }
        fn place(&self, _bytes: &[u8]) -> Result<String, String> {
            Ok("probe".to_string())
        }
        fn attempt(&self, object: &str, violation: Forbidden) -> Outcome {
            let path = self.root.join(object);
            let result = match violation {
                Forbidden::Overwrite => std::fs::write(&path, b"tampered").map(|()| ()),
                Forbidden::Delete => std::fs::remove_file(&path),
                Forbidden::Truncate => std::fs::OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open(&path)
                    .map(|_| ()),
            };
            match result {
                Ok(()) => Outcome::Allowed { detail: violation.name().to_string() },
                Err(error) => Outcome::Refused { detail: error.to_string() },
            }
        }
        fn read(&self, object: &str) -> Result<Vec<u8>, String> {
            std::fs::read(self.root.join(object)).map_err(|error| error.to_string())
        }
    }

    let attestation = attested(&HalfLocked { root: dir.path().to_path_buf() });

    assert!(!attestation.passed(), "half a control is not a control: {attestation:?}");
    assert!(
        attestation.allowed().contains(&Forbidden::Delete),
        "the delete is what it let through: {attestation:?}"
    );
}

#[test]
fn a_production_store_is_refused_before_anything_is_attempted() {
    // Not a policy preference. This drill attempts the violations it is checking for, so
    // against real data a missing control means the drill itself inflicts the loss the control
    // existed to prevent. The gate says "on a non-production archive" for this reason.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let store = Directory { root: dir.path().to_path_buf(), immutable: false, production: true };

    let attestation = attested(&store);

    assert!(!attestation.passed());
    assert!(attestation.could_not_attempt.is_some(), "{attestation:?}");
    assert!(attestation.attempts.is_empty(), "nothing may be attempted against production");
    assert!(
        !dir.path().join("probe").exists(),
        "and nothing may even be written there"
    );
    assert!(attestation.line().contains("NOT ATTEMPTED"), "{}", attestation.line());
}

#[test]
fn an_attestation_that_could_not_place_an_object_is_not_a_pass() {
    // A drill that never touched the store produces no allowed violations, and so does one
    // that ran and passed. If those are the same record, the history says a store was proven
    // when nothing looked at it.
    struct Unreachable;
    impl WriteOnce for Unreachable {
        fn describe(&self) -> String {
            "a store nothing can reach".to_string()
        }
        fn is_non_production(&self) -> bool {
            true
        }
        fn place(&self, _bytes: &[u8]) -> Result<String, String> {
            Err("no credentials for the archive bucket".to_string())
        }
        fn attempt(&self, _object: &str, _violation: Forbidden) -> Outcome {
            panic!("nothing may be attempted when nothing was placed")
        }
        fn read(&self, _object: &str) -> Result<Vec<u8>, String> {
            Err("unreachable".to_string())
        }
    }

    let attestation = attested(&Unreachable);

    assert!(!attestation.passed(), "an untested control is not a proven one");
    assert!(attestation.allowed().is_empty(), "and it must not read as a clean run");
    assert!(
        attestation
            .could_not_attempt
            .as_deref()
            .is_some_and(|why| why.contains("credentials")),
        "the reason survives into the record: {attestation:?}"
    );
}

#[test]
fn a_store_that_reports_refusals_it_did_not_make_is_caught_by_the_read_back() {
    // The contract says an implementation must attempt the violation for real, and a contract
    // is not a mechanism. Reading the object back afterwards is the mechanism: a store that
    // refuses at the API and mutates underneath passes every other assertion and fails this.
    let dir = tempfile::tempdir().expect("a temporary directory");

    let attestation = attested(&Liar { root: dir.path().to_path_buf() });

    assert!(
        attestation.attempts.iter().all(|(_, outcome)| outcome.is_refused()),
        "it claimed a refusal every time"
    );
    assert_eq!(attestation.bytes_intact, Some(false), "and the bytes say otherwise");
    assert!(!attestation.passed(), "so it does not attest: {attestation:?}");
    assert!(attestation.line().contains("OBJECT MODIFIED"), "{}", attestation.line());
}

#[test]
fn an_attempted_delete_that_succeeded_reads_as_damage_rather_than_as_a_missing_file() {
    // After a successful delete the object cannot be read. That is not "inconclusive" — it is
    // the strongest available evidence that the delete went through.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let store = Directory::new(dir.path(), false);

    let attestation = attested(&store);

    assert_eq!(attestation.bytes_intact, Some(false));
    assert!(!attestation.passed());
}

#[test]
fn the_record_line_is_stable_and_names_the_store() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let store = Directory::new(dir.path(), true);
    let attestation = attested(&store);
    store.unlock();

    let line = attestation.line();
    assert!(line.starts_with("1700000000000000\t"), "{line}");
    assert!(line.contains(&dir.path().display().to_string()), "{line}");
    assert_eq!(line.lines().count(), 1, "one attestation is one line: {line}");
}

// --- `passed()` holds for any value of the type, not only ones `attest` built ---

/// An attestation assembled directly, as a reader of the record or a future store would.
///
/// [`Attestation`] is public with public fields, so `attest` is not the only thing that can
/// produce one --- and `passed()` is what everything downstream branches on. Its contract is a
/// property of the type, not of the one constructor that happens to exist today.
fn assembled(attempts: Vec<(Forbidden, Outcome)>, bytes_intact: Option<bool>) -> Attestation {
    Attestation {
        store: "assembled".to_string(),
        at: 1_700_000_000_000_000,
        attempts,
        could_not_attempt: None,
        bytes_intact,
    }
}

#[test]
fn an_attestation_missing_a_violation_does_not_pass() {
    // Found by the mutation audit: replacing the count check with "not empty" survived every
    // test, because `attest` always attempts all three and no test built an attestation any
    // other way. A short list is exactly what a partial run or a truncated record produces,
    // and reading it as a pass would certify a control that was two-thirds tested.
    let short = assembled(
        vec![(
            Forbidden::Overwrite,
            Outcome::Refused { detail: "object lock in force".to_string() },
        )],
        Some(true),
    );

    assert!(
        !short.passed(),
        "one refusal out of three is not an attested store: {short:?}"
    );
}

#[test]
fn a_violation_that_could_not_be_attempted_fails_the_attestation_and_is_named() {
    // The realistic shape: the credentials could overwrite but not delete, so the delete says
    // nothing about immutability. Reporting that as a refusal would be the drill certifying a
    // control it never tested.
    let partial = assembled(
        vec![
            (
                Forbidden::Overwrite,
                Outcome::Refused { detail: "object lock in force".to_string() },
            ),
            (
                Forbidden::Delete,
                Outcome::NotAttempted { why: "no permission to call delete".to_string() },
            ),
            (
                Forbidden::Truncate,
                Outcome::Refused { detail: "object lock in force".to_string() },
            ),
        ],
        Some(true),
    );

    assert!(!partial.passed(), "an untested control is not a proven one");
    assert_eq!(
        partial.untested(),
        vec![Forbidden::Delete],
        "and the record says which one nobody tested"
    );
    assert!(
        partial.allowed().is_empty(),
        "untested is not the same finding as allowed: one is a broken drill, the other is an \
         incident, and they send an operator to different places"
    );
    assert!(partial.line().contains("UNTESTED delete"), "{}", partial.line());
}

#[test]
fn every_violation_refused_but_the_object_changed_does_not_pass() {
    let tampered = assembled(
        Forbidden::ALL
            .iter()
            .map(|violation| {
                (*violation, Outcome::Refused { detail: "refused".to_string() })
            })
            .collect(),
        Some(false),
    );

    assert!(!tampered.passed(), "the bytes are the final word: {tampered:?}");
}

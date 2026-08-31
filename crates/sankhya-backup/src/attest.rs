//! Proving that a write-once store is actually write-once, by trying to violate it.
//!
//! # The risk this exists for
//!
//! `RSK-28`: *"Immutability controls silently removed by a later storage policy change."*
//! Likelihood medium, impact high, and the detection signal the register names is *"attestation
//! check failing"* --- which requires an attestation check to exist. This is it.
//!
//! Three requirements lean on a store that refuses to change what it holds. `FR-TIER-12` mirrors
//! the archival registry to write-once storage; `FR-SEC-12` mirrors the audit chain to immutable
//! storage; `FR-OPS`'s backups are the reason [`protect`](crate::protect) exists at all. Each of
//! them is a durability claim that is true only while somebody else's retention policy stays put,
//! and nothing in this system was watching that.
//!
//! # Why it attempts the violation rather than reading the configuration
//!
//! Because the failure being guarded is **a configuration that says the right thing and no longer
//! does it**. An object-lock flag read back as `enabled` is exactly what a silently-replaced
//! bucket policy still reports. Attestation from configuration would pass in precisely the
//! scenario it was written to catch, which makes it worse than nothing: it converts an unknown
//! into a false assurance.
//!
//! [`drill`](crate::drill) settled the same question for backups and settled it the same way. A
//! backup is proven restorable by **reading it back**, not by checking that the manifest claims a
//! row count. This module is that argument applied to immutability: the only evidence that a
//! store will refuse a write is a refused write.
//!
//! # Why it must never point at production
//!
//! Follows immediately, and is the reason the delivery gate says *"an archive attestation drill
//! has passed **on a non-production archive**"*.
//!
//! This drill is a deliberate, controlled attempt at corruption. If the control is intact the
//! attempt fails and nothing happens. **If the control is gone, the attempt succeeds** --- and a
//! successful attempt against the system of record is the damage the control existed to prevent,
//! inflicted by the thing checking for it. So the target is asserted to be non-production before
//! anything is attempted, and [`Attestation::could_not_attempt`] carries the refusal when it is
//! not.
//!
//! # An attempt that could not be made is not a pass
//!
//! The distinction [`drill`] draws between *"a drill that never ran and a drill that ran and
//! passed"*, which is the same trap wearing different clothes. A write that fails because the
//! path was wrong, the credentials were missing or the directory did not exist has demonstrated
//! nothing about immutability, and recording it as a refusal would let a broken drill certify a
//! store it never touched. Every outcome here says which of the two happened.

use std::fmt;

/// One thing a write-once store must refuse.
///
/// Named individually rather than collapsed into "is it immutable", because they fail
/// separately in the field: object-lock retention stops overwrites while a lifecycle rule
/// happily expires the object, and a legal hold applied to a prefix leaves anything written
/// after it unprotected.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Forbidden {
    /// Writing different bytes to a name that already holds some.
    Overwrite,
    /// Removing the object.
    Delete,
    /// Shortening it in place, which some stores permit while refusing a full overwrite.
    Truncate,
}

impl Forbidden {
    /// Every violation an attestation attempts.
    pub const ALL: [Self; 3] = [Self::Overwrite, Self::Delete, Self::Truncate];

    /// Its name in a record somebody reads years later.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Overwrite => "overwrite",
            Self::Delete => "delete",
            Self::Truncate => "truncate",
        }
    }
}

impl fmt::Display for Forbidden {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// What happened when one violation was attempted.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// The store refused it. This is the only passing outcome.
    Refused {
        /// What the store said, kept verbatim for the evidence pack.
        detail: String,
    },
    /// The store allowed it. **The control is not in force.**
    ///
    /// Carries what the object looks like now, because the drill has just modified something
    /// that was supposed to be unmodifiable and whoever reads this needs to know what state it
    /// was left in.
    Allowed {
        /// What the attempt did.
        detail: String,
    },
    /// The attempt could not be made, so nothing was learned.
    ///
    /// Distinct from `Refused` and it is the distinction the whole module turns on. A store
    /// that could not be reached has not refused anything.
    NotAttempted {
        /// Why.
        why: String,
    },
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused { detail } => write!(f, "refused ({detail})"),
            Self::Allowed { detail } => write!(f, "ALLOWED — {detail}"),
            Self::NotAttempted { why } => write!(f, "not attempted ({why})"),
        }
    }
}

impl Outcome {
    /// Whether this outcome is evidence that the control holds.
    #[must_use]
    pub const fn is_refused(&self) -> bool {
        matches!(self, Self::Refused { .. })
    }
}

/// Whatever is claiming to be write-once.
///
/// A trait rather than a concrete store because three different subsystems mirror to one ---
/// the archival registry, the audit chain and backup manifests --- and an attestation that
/// only understood one of them would leave the other two unattested while looking complete.
///
/// # Contract
///
/// An implementation attempts the violation **for real** and reports what the store did. It
/// must not simulate, short-circuit on a configuration flag, or return `Refused` because it
/// decided not to try: doing any of those reintroduces exactly the defect this module exists
/// to detect, one layer further down where nothing is watching.
pub trait WriteOnce {
    /// A name for this store in the record.
    fn describe(&self) -> String;

    /// Whether this store is safe to attack.
    ///
    /// `false` for anything holding real data. An attestation is a controlled attempt at
    /// corruption and a successful attempt against production is the loss the control exists
    /// to prevent.
    fn is_non_production(&self) -> bool;

    /// Put an object there to be attacked, returning its identifier.
    ///
    /// # Errors
    ///
    /// When the object could not be created, which makes the whole attestation
    /// [`Outcome::NotAttempted`] rather than a failure.
    fn place(&self, bytes: &[u8]) -> Result<String, String>;

    /// Attempt one forbidden operation against a placed object.
    fn attempt(&self, object: &str, violation: Forbidden) -> Outcome;

    /// Read an object back, to check the attempts left it alone.
    ///
    /// # Errors
    ///
    /// When it cannot be read --- which, after an attempted delete, is itself a finding.
    fn read(&self, object: &str) -> Result<Vec<u8>, String>;
}

/// What one attestation found.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Attestation {
    /// Which store was attested.
    pub store: String,
    /// When, in microseconds from the epoch.
    pub at: i64,
    /// One entry per violation attempted, in [`Forbidden::ALL`] order.
    pub attempts: Vec<(Forbidden, Outcome)>,
    /// Why nothing was attempted at all, if nothing was.
    ///
    /// The same guard [`Evidence::could_not_start`](crate::drill::Evidence) draws: an
    /// attestation that never ran produces no violations, and so does one that ran and passed.
    /// If those land in the same record, the history says a store was proven when nothing
    /// touched it.
    pub could_not_attempt: Option<String>,
    /// Whether the object still held its original bytes when the drill finished.
    ///
    /// Checked separately from the attempts, because a store can refuse every operation at the
    /// API and still have changed the object --- and because a *successful* attack must be
    /// reported as having left damage, not merely as a failed refusal.
    pub bytes_intact: Option<bool>,
}

impl Attestation {
    /// Whether the store is proven write-once.
    ///
    /// Requires the drill to have run, to have attempted every violation, for every one to have
    /// been refused, and for the object to be byte-identical afterwards. Any `NotAttempted`
    /// fails it: a control that could not be tested is not a control that was proven.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.could_not_attempt.is_none()
            && self.attempts.len() == Forbidden::ALL.len()
            && self.attempts.iter().all(|(_, outcome)| outcome.is_refused())
            && self.bytes_intact == Some(true)
    }

    /// The violations the store allowed.
    ///
    /// Separate from [`Self::untested`] because they call for different actions: an allowed
    /// violation is an incident, and an untested one is a broken drill.
    #[must_use]
    pub fn allowed(&self) -> Vec<Forbidden> {
        self.attempts
            .iter()
            .filter(|(_, outcome)| matches!(outcome, Outcome::Allowed { .. }))
            .map(|(violation, _)| *violation)
            .collect()
    }

    /// The violations that could not be attempted.
    #[must_use]
    pub fn untested(&self) -> Vec<Forbidden> {
        self.attempts
            .iter()
            .filter(|(_, outcome)| matches!(outcome, Outcome::NotAttempted { .. }))
            .map(|(violation, _)| *violation)
            .collect()
    }

    /// One line for the append-only record.
    ///
    /// Hand-built rather than derived, so the fields are chosen: this is read years later, by
    /// somebody assembling the evidence pack `FR-TIER-35` requires from the write-once manifest
    /// alone, and a serialisation that follows a struct through its refactors is not a format.
    #[must_use]
    pub fn line(&self) -> String {
        let verdict = if self.passed() {
            "PASS".to_string()
        } else if let Some(why) = &self.could_not_attempt {
            format!("NOT ATTEMPTED ({why})")
        } else {
            let allowed = self.allowed();
            let untested = self.untested();
            let mut parts = Vec::new();
            if !allowed.is_empty() {
                parts.push(format!(
                    "ALLOWED {}",
                    allowed.iter().copied().map(Forbidden::name).collect::<Vec<_>>().join(",")
                ));
            }
            if !untested.is_empty() {
                parts.push(format!(
                    "UNTESTED {}",
                    untested.iter().copied().map(Forbidden::name).collect::<Vec<_>>().join(",")
                ));
            }
            if self.bytes_intact == Some(false) {
                parts.push("OBJECT MODIFIED".to_string());
            }
            parts.join(" ")
        };
        format!("{}\t{}\t{}", self.at, self.store, verdict)
    }
}

/// The bytes placed for the drill to attack.
///
/// Recognisable on sight in a store somebody is inspecting, and different from anything the
/// system writes for real, so an object left behind by a drill is never mistaken for an archive.
pub const PROBE: &[u8] = b"sankhya write-once attestation probe; safe to delete once expired\n";

/// Attest a store by attempting every forbidden operation against it.
///
/// Returns what happened. **This function modifies nothing when the control holds and modifies
/// something when it does not** --- which is the whole design, and the reason `store` is
/// required to say it is not production before anything is attempted.
pub fn attest(store: &dyn WriteOnce, now: i64) -> Attestation {
    let described = store.describe();

    // Before anything is attempted. A drill that discovers the control is missing has, by
    // definition, just overwritten and deleted something; against real data that is the loss
    // the control existed to prevent, caused by the check for it.
    if !store.is_non_production() {
        return not_attempted(
            described,
            now,
            "this store is not declared non-production. An attestation attempts the violations \
             it is checking for, so against real data a missing control means the drill itself \
             inflicts the damage",
        );
    }

    let object = match store.place(PROBE) {
        Ok(object) => object,
        Err(why) => {
            return not_attempted(
                described,
                now,
                &format!("nothing could be placed to attack: {why}"),
            )
        }
    };

    let attempts: Vec<(Forbidden, Outcome)> = Forbidden::ALL
        .iter()
        .map(|violation| (*violation, store.attempt(&object, *violation)))
        .collect();

    // Read back last. Every attempt above claimed to have been refused or allowed, and this is
    // the independent check on those claims --- an implementation that reports `Refused`
    // without attempting anything passes every assertion above and fails this one only if it
    // actually changed something, so this is not a substitute for the contract. It is the
    // check that catches a store which refuses at the API and mutates underneath.
    let bytes_intact = match store.read(&object) {
        Ok(bytes) => Some(bytes == PROBE),
        // Unreadable after a delete was attempted is not "intact". It is the strongest possible
        // evidence the delete succeeded.
        Err(_) => Some(false),
    };

    Attestation {
        store: described,
        at: now,
        attempts,
        could_not_attempt: None,
        bytes_intact,
    }
}

/// An attestation that did not happen, and says so.
fn not_attempted(store: String, at: i64, why: &str) -> Attestation {
    Attestation {
        store,
        at,
        attempts: Vec::new(),
        could_not_attempt: Some(why.to_string()),
        bytes_intact: None,
    }
}

// --- a store on a filesystem, and the record of what it was asked ------------

use std::path::{Path, PathBuf};

/// The file an operator creates to declare an archive safe to attack.
///
/// # Why a marker beside the data rather than a flag on the command
///
/// Because the assertion has to travel with the thing it describes. A `--non-production`
/// argument lives in somebody's shell history and in a runbook that gets copied; the copy
/// runs against production eventually, and the flag will still be in it. A file inside the
/// archive is a statement about *that archive*, made once, by whoever had a reason.
///
/// It also fails in the safe direction. Forgetting the marker means the drill refuses to run,
/// which is visible and costs a minute. A flag that defaults wrong means the drill runs.
pub const NON_PRODUCTION_MARKER: &str = "_non_production";

/// Where attestations are recorded, under the data directory.
pub const ATTESTATION_FILE: &str = "attestations.log";

/// A write-once store that is a directory somewhere.
///
/// For an archive on a filesystem, on network-attached storage, or on an object store mounted
/// into the filesystem. An object-store client speaking S3 object-lock is a second
/// implementation of [`WriteOnce`], which is why that is a trait.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Directory {
    root: PathBuf,
}

impl Directory {
    /// A store at this path.
    #[must_use]
    pub fn at(root: &Path) -> Self {
        Self { root: root.to_path_buf() }
    }

    /// The path of the marker that would make this attestable.
    #[must_use]
    pub fn marker(&self) -> PathBuf {
        self.root.join(NON_PRODUCTION_MARKER)
    }
}

impl WriteOnce for Directory {
    fn describe(&self) -> String {
        self.root.display().to_string()
    }

    fn is_non_production(&self) -> bool {
        self.marker().is_file()
    }

    fn place(&self, bytes: &[u8]) -> Result<String, String> {
        // Named for what it is and stamped, so an object a drill left behind in a store
        // somebody is inspecting is never mistaken for an archive.
        let name = "_attestation_probe";
        std::fs::create_dir_all(&self.root).map_err(|error| error.to_string())?;
        std::fs::write(self.root.join(name), bytes).map_err(|error| error.to_string())?;
        Ok(name.to_string())
    }

    fn attempt(&self, object: &str, violation: Forbidden) -> Outcome {
        let path = self.root.join(object);
        let attempted = match violation {
            Forbidden::Overwrite => std::fs::write(&path, b"attestation overwrite attempt"),
            Forbidden::Delete => std::fs::remove_file(&path),
            Forbidden::Truncate => std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&path)
                .map(|_| ()),
        };
        match attempted {
            Ok(()) => Outcome::Allowed {
                detail: format!("the store performed the {violation} without complaint"),
            },
            Err(error) => Outcome::Refused { detail: error.to_string() },
        }
    }

    fn read(&self, object: &str) -> Result<Vec<u8>, String> {
        std::fs::read(self.root.join(object)).map_err(|error| error.to_string())
    }
}

/// Append an attestation to the record.
///
/// # Errors
///
/// When the file cannot be written. Surfaced rather than swallowed, for the reason
/// [`drill::record`](crate::drill::record) gives: evidence that was not recorded is the same
/// as a drill that did not happen, and the caller has to know which it has.
pub fn record(directory: &Path, attestation: &Attestation) -> Result<(), crate::drill::EvidenceError> {
    use std::io::Write as _;
    std::fs::create_dir_all(directory).map_err(|error| crate::drill::EvidenceError {
        path: directory.to_path_buf(),
        why: error.to_string(),
    })?;
    let path = directory.join(ATTESTATION_FILE);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|error| crate::drill::EvidenceError {
            path: path.clone(),
            why: error.to_string(),
        })?;
    writeln!(file, "{}", attestation.line()).map_err(|error| crate::drill::EvidenceError {
        path,
        why: error.to_string(),
    })
}

/// When an attestation last passed, from the record.
///
/// `None` means none ever has --- which includes the case where several have run and every one
/// failed. That is the answer an operator needs, and it is why this reads the record rather
/// than a stored timestamp somebody might update on a failure.
#[must_use]
pub fn last_pass(directory: &Path) -> Option<i64> {
    let text = std::fs::read_to_string(directory.join(ATTESTATION_FILE)).ok()?;
    text.lines()
        .filter(|line| line.ends_with("\tPASS"))
        .filter_map(|line| line.split('\t').next()?.trim().parse::<i64>().ok())
        .max()
}

//! What version something is, and what a reader does about it.
//!
//! # A parse error and "this is from the future" are different facts
//!
//! That distinction is the whole of this crate.
//!
//! An artifact written by a newer release, read by an older one, fails somewhere in the
//! middle of parsing --- an unknown field, a missing key, a number that will not fit. The
//! error says *"invalid type: string, expected u64 at line 14 column 9"*, and an operator
//! reads it as **corruption**. They go looking for a damaged disk, a truncated write, a bad
//! copy. The actual answer is "upgrade the binary", and nothing in front of them says so.
//!
//! So every on-disk format this system writes carries a version, that version is placed
//! where it can be read **before** anything else is understood, and a version from the
//! future is refused **by name**:
//!
//! ```text
//! this backup manifest is format 3 and this build understands up to 2 —
//! it was written by a newer release of SANKHYA, and reading it here would
//! at best fail and at worst misread it
//! ```
//!
//! # Four axes, independently
//!
//! `FR-OPS-11` requires four version axes managed independently: the internal schema, the
//! database major version, the table format protocol, and the wire APIs. Independently is
//! the load-bearing word. A single product version covering all four means every change to
//! any of them is a change to all of them, so an upgrade that only touches the wire protocol
//! reads as a storage-format change and gets the caution one deserves --- and, worse, the
//! reverse: a genuine storage break hides inside a release that looked like a wire change.
//!
//! # Backwards is the direction that matters
//!
//! A new release reading old data is the easy direction and the one everybody tests. The
//! direction that decides whether you can **roll back** is the other one: can the *old*
//! release read what the new one wrote?
//!
//! If it cannot, the upgrade is a one-way door, and the moment to discover that is not after
//! walking through it. So a format bump is a declared, deliberate act with a stated
//! consequence for rollback --- see [`Format::rollback`].

#![doc(html_root_url = "https://docs.rs/sankhya-version")]

use std::fmt;

/// One of the four things that version independently.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Axis {
    /// This system's own on-disk artefacts.
    InternalSchema,
    /// The transactional store's major version.
    Database,
    /// The table format's reader and writer protocol.
    TableProtocol,
    /// What clients speak.
    WireApi,
}

impl Axis {
    /// Its name in the generated documentation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InternalSchema => "internal schema",
            Self::Database => "database major version",
            Self::TableProtocol => "table format protocol",
            Self::WireApi => "wire API",
        }
    }
}

/// Whether an older release could still read what this one writes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Rollback {
    /// The previous release reads it unchanged.
    Safe,
    /// The previous release reads it, ignoring what it does not recognise.
    ///
    /// Safe **only** where the ignored part is not load-bearing. A field the old release
    /// skips is a field whose absence it will act on, so this is a claim about meaning
    /// rather than about parsing.
    Tolerated {
        /// What the older release will not see, and why that is survivable.
        ignoring: &'static str,
    },
    /// The previous release cannot read it. Upgrading is a one-way door.
    ///
    /// Recorded on the format rather than discovered during an incident. An upgrade that
    /// cannot be reversed is a different decision from one that can, and the difference has
    /// to be visible before it is made.
    OneWay {
        /// What breaks, in the words somebody deciding needs.
        because: &'static str,
    },
}

/// An on-disk format this system writes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Format {
    /// What it is called, in an error an operator reads.
    pub name: &'static str,
    /// Where it lives.
    pub path: &'static str,
    /// The highest version this build writes and understands.
    pub current: u32,
    /// The lowest version this build can still read.
    ///
    /// Reading an older artefact is the easy direction and it is not free: every version
    /// still supported is a migration path that has to keep working. Stating the floor makes
    /// dropping one a decision rather than an accident.
    pub oldest_readable: u32,
    /// What an older release does with what this one writes.
    pub rollback: Rollback,
}

/// What a reader should do with an artefact it did not write.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Compatibility {
    /// Read it, and write this format too.
    Full,
    /// Read it and **do not write**.
    ///
    /// `FR-OPS-12`: report read-only degradation rather than silently misreading. Writing
    /// here would produce an artefact claiming a version whose rules this build does not
    /// implement, which is worse than refusing --- it is a lie the next reader believes.
    ReadOnly {
        /// Why writing is refused.
        why: String,
    },
    /// Do not read it.
    Refused {
        /// What to do, which is almost always "upgrade".
        why: String,
    },
}

impl Compatibility {
    /// Whether the artefact may be read at all.
    #[must_use]
    pub const fn readable(&self) -> bool {
        !matches!(self, Self::Refused { .. })
    }

    /// Whether this build may write this format.
    #[must_use]
    pub const fn writable(&self) -> bool {
        matches!(self, Self::Full)
    }
}

impl Format {
    /// What to do with an artefact stamped `found`.
    #[must_use]
    pub fn admits(&self, found: u32) -> Compatibility {
        if found > self.current {
            return Compatibility::Refused {
                why: format!(
                    "this {} is format {found} and this build understands up to {} — it was \
                     written by a newer release of SANKHYA. Reading it here would at best \
                     fail and at worst misread it, so it is refused rather than attempted. \
                     Upgrade this binary, or point it at an artefact written by a build of \
                     its own generation.",
                    self.name, self.current
                ),
            };
        }
        if found < self.oldest_readable {
            return Compatibility::Refused {
                why: format!(
                    "this {} is format {found}, and support for anything below {} was \
                     removed. An artefact this old has to be migrated by a release that \
                     still understood it; this build cannot read it and will not guess.",
                    self.name, self.oldest_readable
                ),
            };
        }
        if found < self.current {
            // Readable and not writable. Writing the older format would mean implementing
            // its rules as well as the current ones, and writing the *newer* format into a
            // file the rest of the system believes is older is how two readers come to
            // disagree about the same bytes.
            return Compatibility::ReadOnly {
                why: format!(
                    "this {} is format {found} and this build writes {}. It is read as-is; \
                     it is not written back, because rewriting it would change its format \
                     without anybody asking for that.",
                    self.name, self.current
                ),
            };
        }
        Compatibility::Full
    }

    /// Whether upgrading past this format can be undone.
    #[must_use]
    pub const fn reversible(&self) -> bool {
        !matches!(self.rollback, Rollback::OneWay { .. })
    }
}

/// Every on-disk format, and what rolling back does to it.
///
/// Declared in one place so that adding a format without stating its rollback consequence is
/// not possible by omission --- and so `cargo xtask check-catalogues` can publish the table
/// rather than somebody maintaining a second copy in prose.
/// The backup manifest's format.
pub static BACKUP_MANIFEST: Format = Format {
    name: "backup manifest",
    path: "<data-dir>/backup-manifest.json",
    current: 1,
    oldest_readable: 1,
    rollback: Rollback::Safe,
};

/// The restore-drill evidence log's format.
pub static DRILL_EVIDENCE: Format = Format {
    name: "restore-drill evidence",
    path: "<data-dir>/restore-drills.jsonl",
    current: 1,
    oldest_readable: 1,
    rollback: Rollback::Safe,
};

/// The diagnostic history's format.
pub static DIAGNOSTIC_HISTORY: Format = Format {
    name: "diagnostic history",
    path: "<data-dir>/diagnostic-history.tsv",
    current: 1,
    oldest_readable: 1,
    rollback: Rollback::Tolerated {
        ignoring: "a build with no version header treats the file as format 1, which it is \
                   --- the header was added after the format, and its absence means the \
                   original",
    },
};

/// The attestation record's format.
///
/// One tab-separated line per attestation: the timestamp, the store, and the verdict. Chosen
/// so that a build which knows nothing about the format still shows a reader the verdict ---
/// this file is read years later, by somebody assembling the evidence pack `FR-TIER-35`
/// requires from the write-once manifest alone.
pub static ATTESTATION_EVIDENCE: Format = Format {
    name: "attestation evidence",
    path: "<data-dir>/attestations.log",
    current: 1,
    oldest_readable: 1,
    rollback: Rollback::Safe,
};

/// Every on-disk format, and what rolling back does to it.
///
/// Declared in one place so that adding a format without stating its rollback consequence is
/// not possible by omission, and so `cargo xtask check-catalogues` can publish the table
/// rather than somebody maintaining a second copy in prose.
///
/// **Callers name the constant rather than looking one up by string.** An earlier version
/// resolved formats through [`format`] and unwrapped the result, which meant every call site
/// asserted at runtime what the compiler already knew --- and did it with an `expect` the
/// workspace's lints correctly refuse. Passing the declaration is the same rule the metric
/// catalogue follows, for the same reason: an undeclared format becomes unrepresentable
/// rather than merely refused.
pub static FORMATS: &[&Format] =
    &[&BACKUP_MANIFEST, &DRILL_EVIDENCE, &ATTESTATION_EVIDENCE, &DIAGNOSTIC_HISTORY];

/// The format by that name, if this build knows one.
///
/// For a caller that genuinely has a name and not a declaration --- a tool inspecting an
/// artefact it was handed. Code that knows which format it is writing names the constant.
#[must_use]
pub fn format(name: &str) -> Option<&'static Format> {
    FORMATS.iter().copied().find(|format| format.name == name)
}

impl fmt::Display for Compatibility {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full => f.write_str("readable and writable"),
            Self::ReadOnly { why } | Self::Refused { why } => f.write_str(why),
        }
    }
}

//! Deciding whether a bundle may be loaded.
//!
//! # What this provides, stated exactly
//!
//! A bundle is loaded from a file, and a file can be edited by anyone who can write to the
//! directory. That makes bundle loading a **code-loading path**, and every code-loading
//! path needs an answer to "why do you trust this?".
//!
//! What ships here is **digest pinning**: the operator configures the digests of the
//! bundles they have reviewed, and anything else is refused. That is a real control --- it
//! makes an unreviewed or modified bundle fail to load --- and it is exactly as strong as
//! the operator's care in maintaining the list.
//!
//! What does **not** ship here is public-key signing. It is the better answer, because it
//! moves trust from a list to a key and lets a vendor sign bundles the operator has never
//! seen. It needs a cryptographic dependency, and this repository pins its dependency set
//! deliberately and by decision record; adding one is that kind of decision, not something
//! to slip in alongside a feature. [`Verifier`] is a trait so that the choice is a
//! deployment matter and adding it later changes no caller.
//!
//! # Why the digest is not called a signature
//!
//! It is not one. A digest proves that the bytes are the bytes you pinned. It proves
//! nothing about who wrote them, and against an attacker who can write the file *and* the
//! configuration it proves nothing at all. Naming it honestly is the difference between a
//! control an operator can reason about and a checkbox that invites them to stop thinking.

use std::collections::BTreeSet;
use std::fmt;

/// A content digest over a bundle's exact bytes.
///
/// FNV-1a, 128-bit. Chosen because it needs no dependency and is entirely adequate for what
/// it is used for --- detecting that a file is not the file that was reviewed. It is **not**
/// collision-resistant against an adversary who can choose both inputs, and it must not be
/// relied on as though it were. Where that matters, configure a [`Verifier`] that uses a
/// real cryptographic hash.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Digest(pub u128);

impl Digest {
    /// The digest of these bytes.
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        // FNV-1a over 128 bits.
        const OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
        const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;
        let mut hash = OFFSET;
        for byte in bytes {
            hash ^= u128::from(*byte);
            hash = hash.wrapping_mul(PRIME);
        }
        Self(hash)
    }

    /// The digest as lower-case hexadecimal, which is how it is written in configuration.
    #[must_use]
    pub fn to_hex(self) -> String {
        format!("{:032x}", self.0)
    }

    /// Read a digest back from hexadecimal.
    #[must_use]
    pub fn from_hex(text: &str) -> Option<Self> {
        u128::from_str_radix(text.trim(), 16).ok().map(Self)
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// Whether a bundle may be loaded.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Trust {
    /// Load it.
    Allowed,
    /// Do not, for this reason.
    Refused {
        /// Why.
        reason: String,
    },
}

impl Trust {
    /// Whether this permits loading.
    #[must_use]
    pub const fn is_allowed(&self) -> bool {
        matches!(self, Self::Allowed)
    }
}

/// Decides whether a bundle's bytes may be loaded.
///
/// A trait so the decision is a deployment matter. An installation with a key-management
/// service verifies a real signature here; one without pins digests; one in a development
/// sandbox trusts everything and says so.
pub trait Verifier: Send + Sync + fmt::Debug {
    /// May these bytes be loaded?
    fn verify(&self, source: &str, bytes: &[u8]) -> Trust;

    /// What this verifier is, for an operator reading a startup log.
    fn describe(&self) -> String;
}

/// Refuses anything whose digest is not on a configured list.
///
/// The default for any installation that has not configured something stronger. An empty
/// list refuses **everything**, which is the correct behaviour for a policy that has not
/// been configured: a trust policy that defaults to trusting is not a policy.
#[derive(Debug, Default)]
pub struct PinnedDigests {
    allowed: BTreeSet<Digest>,
}

impl PinnedDigests {
    /// A policy trusting exactly these digests.
    #[must_use]
    pub fn of(digests: impl IntoIterator<Item = Digest>) -> Self {
        Self {
            allowed: digests.into_iter().collect(),
        }
    }

    /// Trust one more digest.
    pub fn allow(&mut self, digest: Digest) {
        self.allowed.insert(digest);
    }

    /// How many digests are trusted.
    #[must_use]
    pub fn len(&self) -> usize {
        self.allowed.len()
    }

    /// Whether nothing is trusted, in which case everything is refused.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.allowed.is_empty()
    }
}

impl Verifier for PinnedDigests {
    fn verify(&self, source: &str, bytes: &[u8]) -> Trust {
        let digest = Digest::of(bytes);
        if self.allowed.contains(&digest) {
            return Trust::Allowed;
        }
        if self.allowed.is_empty() {
            return Trust::Refused {
                reason: format!(
                    "no bundle digests are pinned, so nothing may be loaded. '{source}' has \
                     digest {digest}; add it to the trusted list after reviewing it. A trust \
                     policy that defaults to trusting is not a policy"
                ),
            };
        }
        Trust::Refused {
            reason: format!(
                "'{source}' has digest {digest}, which is not among the {} pinned. Either \
                 the file has changed since it was reviewed, or it was never reviewed",
                self.allowed.len()
            ),
        }
    }

    fn describe(&self) -> String {
        format!("digest pinning, {} bundle(s) trusted", self.allowed.len())
    }
}

/// Trusts everything, and says loudly that it does.
///
/// For development and for tests. [`Verifier::describe`] returns a sentence intended to
/// appear in a startup log and look wrong in production, because an installation running
/// this by accident has no control on its code-loading path at all.
#[derive(Debug, Default)]
pub struct TrustEverything;

impl Verifier for TrustEverything {
    fn verify(&self, _source: &str, _bytes: &[u8]) -> Trust {
        Trust::Allowed
    }

    fn describe(&self) -> String {
        "NO VERIFICATION — every pack bundle is loaded without being checked. This is a \
         development setting and must not be used where anyone else can write to the bundle \
         directory"
            .to_string()
    }
}

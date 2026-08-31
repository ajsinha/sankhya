//! The evidence pack: what an archive can prove about itself, years after everyone has left.
//!
//! # The requirement is about what is *not* needed to read it
//!
//! `FR-TIER-35`: a signed evidence pack per archive, **generatable years later from the
//! write-once manifest alone**.
//!
//! Every word of "alone" is load-bearing. Not from the registry, which lives in a database that
//! may not exist; not from this software, which may not build; not from a key server, an object
//! catalogue or a runbook. The question being answered is *"here is an archive and a marker; what
//! is this, where did it come from, who authorised it, and is it intact?"* --- asked by somebody
//! who was not there, about a system nobody still runs.
//!
//! So the pack is a projection of [`crate::registry::Entry::marker`] and nothing else. That
//! marker is `key=value` lines: readable by a person without a parser, parseable by a machine
//! without a schema, and forward-compatible because an unknown key is ignored rather than
//! rejected.
//!
//! # The seal, and why it is `HMAC-SHA256` written out here
//!
//! The workspace has no signing dependency and adding one unreviewed for this is a larger
//! decision than it looks. `HMAC` over a hash that is already a dependency --- and already
//! carrying the audit chain and the Merkle tree --- is a standard construction rather than an
//! invention, and the way to make an implementation of it trustworthy is not care but
//! **published test vectors**: `RFC 4231`'s cases are asserted directly, including the one where
//! the key is longer than the block, which is where implementations go wrong.
//!
//! A keyed seal rather than a public-key signature is the honest fit for what this proves. The
//! reader is the organisation that made the archive, checking its own record has not been
//! altered; that is a question about integrity under a key they hold, not about attribution to a
//! third party.

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;

/// `SHA-256`'s block size, in bytes.
const BLOCK: usize = 64;

/// The keys a pack cannot be built without.
const REQUIRED: [&str; 6] = ["table", "range", "archive", "snapshot", "rows", "keys"];

/// Everything an archive's marker says about it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Pack {
    fields: BTreeMap<String, String>,
}

impl Pack {
    /// Read a pack from a write-once marker.
    ///
    /// Blank lines and lines without an `=` are ignored, and so is any key this version does not
    /// know: a reader written today must not fail on a marker written by a later version, which
    /// is the only way *"generatable years later"* survives contact with a schema change.
    ///
    /// # Errors
    ///
    /// [`Unreadable`] naming every required key that is missing, rather than the first, because
    /// somebody holding a damaged marker wants to know how damaged.
    pub fn from_marker(marker: &str) -> Result<Self, Unreadable> {
        let mut fields = BTreeMap::new();
        for line in marker.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                fields.insert(key.trim().to_string(), value.trim().to_string());
            }
        }

        let missing: Vec<String> = REQUIRED
            .iter()
            .filter(|key| !fields.contains_key(**key))
            .map(|key| (*key).to_string())
            .collect();
        if !missing.is_empty() {
            return Err(Unreadable::Missing { keys: missing });
        }

        Ok(Self { fields })
    }

    /// What a key says, if the marker said anything about it.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields.get(key).map(String::as_str)
    }

    /// Who authorised the purge, from the marker's attribution lines.
    #[must_use]
    pub fn attribution(&self) -> Vec<(&str, &str)> {
        self.fields
            .iter()
            .filter_map(|(key, value)| {
                key.strip_prefix("attribution.").map(|name| (name, value.as_str()))
            })
            .collect()
    }

    /// The bytes the seal is taken over.
    ///
    /// Sorted by key and length-prefixed, so the seal is a function of the *content* rather than
    /// of the order the marker happened to be written in --- a marker re-emitted with its lines
    /// rearranged is the same evidence and must seal identically.
    #[must_use]
    pub fn canonical(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        for (key, value) in &self.fields {
            for part in [key.as_bytes(), value.as_bytes()] {
                bytes.extend_from_slice(&(part.len() as u64).to_be_bytes());
                bytes.extend_from_slice(part);
            }
        }
        bytes
    }

    /// Seal the pack under a key.
    #[must_use]
    pub fn seal(&self, key: &[u8]) -> Seal {
        Seal { mac: hmac_sha256(key, &self.canonical()) }
    }

    /// Whether a seal is this pack's, under this key.
    ///
    /// Compared in constant time, because a comparison that returns early tells whoever is
    /// guessing how much of their guess was right.
    #[must_use]
    pub fn sealed_by(&self, key: &[u8], seal: &Seal) -> bool {
        let mine = self.seal(key);
        let mut difference = 0u8;
        for (left, right) in mine.mac.iter().zip(seal.mac.iter()) {
            difference |= left ^ right;
        }
        difference == 0
    }
}

impl fmt::Display for Pack {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for (key, value) in &self.fields {
            if !first {
                f.write_str("\n")?;
            }
            write!(f, "{key}={value}")?;
            first = false;
        }
        Ok(())
    }
}

/// Why a marker could not be read.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Unreadable {
    /// Required keys the marker does not carry.
    Missing {
        /// Every one, not the first.
        keys: Vec<String>,
    },
}

impl fmt::Display for Unreadable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing { keys } => {
                write!(f, "the marker is missing {} required key(s):", keys.len())?;
                for key in keys {
                    write!(f, " {key}")?;
                }
                f.write_str(
                    ". An evidence pack that cannot say what it is evidence of is not evidence",
                )
            }
        }
    }
}

/// A keyed seal over an evidence pack.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Seal {
    mac: [u8; 32],
}

impl Seal {
    /// A seal read back from storage.
    #[must_use]
    pub const fn from_bytes(mac: [u8; 32]) -> Self {
        Self { mac }
    }

    /// Its bytes.
    #[must_use]
    pub const fn to_bytes(&self) -> [u8; 32] {
        self.mac
    }
}

impl fmt::Display for Seal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.mac {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Seal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The whole thing, not a prefix. A truncated digest in somebody's notes is how two
        // different values come to look like the same one.
        write!(f, "{self}")
    }
}

/// `HMAC-SHA256`, as `RFC 2104` defines it.
///
/// Written out rather than depended on, and pinned by `RFC 4231`'s published vectors rather than
/// by inspection. The key-longer-than-block branch is the one that is usually wrong, so it has a
/// vector of its own.
#[must_use]
pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut block = [0u8; BLOCK];
    if key.len() > BLOCK {
        let hashed: [u8; 32] = Sha256::digest(key).into();
        if let Some(slot) = block.get_mut(..hashed.len()) {
            slot.copy_from_slice(&hashed);
        }
    } else {
        if let Some(slot) = block.get_mut(..key.len()) {
            slot.copy_from_slice(key);
        }
    }

    let mut inner_pad = [0x36u8; BLOCK];
    let mut outer_pad = [0x5cu8; BLOCK];
    for index in 0..BLOCK {
        if let (Some(inner), Some(outer), Some(byte)) =
            (inner_pad.get_mut(index), outer_pad.get_mut(index), block.get(index))
        {
            *inner ^= *byte;
            *outer ^= *byte;
        }
    }

    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(message);
    let inner: [u8; 32] = inner.finalize().into();

    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner);
    outer.finalize().into()
}

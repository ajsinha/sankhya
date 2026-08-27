//! Envelope encryption, and rotation that does not rewrite data.
//!
//! # The shape, and why it is this shape
//!
//! Data is encrypted with a **data key**, and the data key is encrypted with a **key
//! encryption key** held elsewhere --- an HSM, a key-management service, a file in a
//! development sandbox. The wrapped data key travels with the data it protects.
//!
//! The whole reason is rotation. If data were encrypted directly under a long-lived key,
//! rotating that key would mean decrypting and re-encrypting every byte, which on a
//! warehouse means a multi-day job that cannot be interrupted and must not be run twice.
//! With an envelope, rotation re-wraps the data keys --- a few thousand small operations ---
//! and **the data is never touched**.
//!
//! # What this module is and is not
//!
//! It is the *envelope*: key identity, wrapping, unwrapping, rotation, and the record of
//! which key protects what. It deliberately does not implement a cipher. A [`KeyProvider`]
//! is a trait so the actual wrapping happens where the key lives --- inside the HSM or the
//! KMS, which is the entire point of having one. A provider that wrapped keys in this
//! process would have the key material in this process, and there would be nothing left to
//! protect.
//!
//! The provider shipped here for tests does not encrypt at all and says so in its name and
//! in its description.

use std::collections::BTreeMap;
use std::fmt;

/// Which key encryption key protects a data key.
///
/// Versioned, because rotation creates a new version rather than replacing the old one:
/// data wrapped under version 3 must stay readable while version 4 becomes current, or
/// rotation is an outage.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct KeyId {
    /// The key's name, stable across rotations.
    pub name: String,
    /// Which version. Increases; old versions stay readable.
    pub version: u32,
}

impl KeyId {
    /// A key version.
    #[must_use]
    pub fn new(name: impl Into<String>, version: u32) -> Self {
        Self {
            name: name.into(),
            version,
        }
    }

    /// The next version of the same key.
    #[must_use]
    pub fn next_version(&self) -> Self {
        Self {
            name: self.name.clone(),
            version: self.version.saturating_add(1),
        }
    }
}

impl fmt::Display for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:v{}", self.name, self.version)
    }
}

/// A data key, encrypted under a key encryption key.
///
/// The plaintext data key is deliberately not a field of anything that derives `Debug` or
/// `Serialize`. Only the wrapped form is storable, which is what stops a key ending up in a
/// log line or a crash dump by accident --- the most common way key material escapes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WrappedKey {
    /// Which key encryption key wrapped it.
    pub wrapped_by: KeyId,
    /// The wrapped bytes.
    pub bytes: Vec<u8>,
}

/// Where wrapping actually happens.
///
/// A trait because it must happen where the key lives. A provider that wrapped keys in this
/// process would hold the key material in this process, and there would be nothing left for
/// the HSM to protect.
pub trait KeyProvider: Send + Sync + fmt::Debug {
    /// The key version new data should be wrapped under.
    fn current(&self) -> KeyId;

    /// Wrap a data key under a specific key version.
    fn wrap(&self, under: &KeyId, data_key: &[u8]) -> Result<WrappedKey, KeyError>;

    /// Unwrap a data key.
    ///
    /// Must succeed for **any** version this provider still holds, not only the current
    /// one. A provider that can only unwrap the current version turns every rotation into
    /// an outage for everything not yet re-wrapped.
    fn unwrap(&self, wrapped: &WrappedKey) -> Result<Vec<u8>, KeyError>;

    /// What this provider is, for an operator reading a startup log.
    fn describe(&self) -> String;
}

/// Why a key operation failed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum KeyError {
    /// The named key version is not known to this provider.
    UnknownKey {
        /// Which one.
        key: KeyId,
    },
    /// The wrapped bytes could not be unwrapped under the key they name.
    Unwrappable {
        /// Which key was named.
        key: KeyId,
    },
    /// The provider refused for a reason of its own.
    Refused {
        /// What it said.
        detail: String,
    },
}

impl fmt::Display for KeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownKey { key } => write!(
                f,
                "the key {key} is not known to this provider. If it was retired, data \
                 wrapped under it is unreadable — retiring a key version before its data \
                 is re-wrapped destroys that data"
            ),
            Self::Unwrappable { key } => write!(
                f,
                "the wrapped key naming {key} could not be unwrapped: it was wrapped under \
                 a different key, or it has been altered"
            ),
            Self::Refused { detail } => write!(f, "the key provider refused: {detail}"),
        }
    }
}

impl std::error::Error for KeyError {}

/// Which key protects which column, and the machinery to rotate them.
#[derive(Debug, Default)]
pub struct Envelope {
    /// The wrapped data key for each protected column.
    columns: BTreeMap<String, WrappedKey>,
}

impl Envelope {
    /// An envelope protecting nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Protect a column with a data key, wrapped under the provider's current key.
    pub fn protect(
        &mut self,
        column: impl Into<String>,
        data_key: &[u8],
        provider: &dyn KeyProvider,
    ) -> Result<(), KeyError> {
        let wrapped = provider.wrap(&provider.current(), data_key)?;
        self.columns.insert(column.into(), wrapped);
        Ok(())
    }

    /// The data key protecting a column.
    pub fn data_key(
        &self,
        column: &str,
        provider: &dyn KeyProvider,
    ) -> Result<Option<Vec<u8>>, KeyError> {
        let Some(wrapped) = self.columns.get(column) else {
            return Ok(None);
        };
        provider.unwrap(wrapped).map(Some)
    }

    /// Whether a column is encrypted.
    #[must_use]
    pub fn protects(&self, column: &str) -> bool {
        self.columns.contains_key(column)
    }

    /// The columns this envelope protects, sorted.
    #[must_use]
    pub fn protected_columns(&self) -> Vec<&str> {
        self.columns.keys().map(String::as_str).collect()
    }

    /// Which key version protects a column.
    #[must_use]
    pub fn wrapped_by(&self, column: &str) -> Option<&KeyId> {
        self.columns.get(column).map(|w| &w.wrapped_by)
    }

    /// Re-wrap every data key under the provider's current key version.
    ///
    /// **The data is not touched.** Each data key is unwrapped under whichever version it
    /// was wrapped with and re-wrapped under the current one; the bytes those data keys
    /// protect are never read, decrypted or written. That is the whole reason for the
    /// envelope: rotating a key that encrypted data directly means re-encrypting every
    /// byte, which on a warehouse is a multi-day job that cannot be interrupted.
    ///
    /// Returns how many keys were re-wrapped. Columns already on the current version are
    /// skipped, so running rotation twice costs nothing the second time --- and rotation is
    /// exactly the kind of job that gets run twice.
    pub fn rotate(&mut self, provider: &dyn KeyProvider) -> Result<Rotation, KeyError> {
        let target = provider.current();
        let mut rewrapped = 0usize;
        let mut skipped = 0usize;

        for wrapped in self.columns.values_mut() {
            if wrapped.wrapped_by == target {
                skipped += 1;
                continue;
            }
            let data_key = provider.unwrap(wrapped)?;
            *wrapped = provider.wrap(&target, &data_key)?;
            rewrapped += 1;
        }

        Ok(Rotation {
            to: target,
            rewrapped,
            already_current: skipped,
        })
    }
}

/// What a rotation did.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Rotation {
    /// The key version everything is now wrapped under.
    pub to: KeyId,
    /// How many data keys were re-wrapped.
    pub rewrapped: usize,
    /// How many were already current.
    pub already_current: usize,
}

impl Rotation {
    /// Whether anything changed.
    #[must_use]
    pub const fn changed_anything(&self) -> bool {
        self.rewrapped > 0
    }

    /// A sentence for a log, saying plainly that no data moved.
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "rotated {} data key(s) to {}, {} already current; no data was read, decrypted \
             or rewritten",
            self.rewrapped, self.to, self.already_current
        )
    }
}

/// A provider that does not encrypt, for tests and development.
///
/// Wrapping is a reversible transformation and nothing here is secret. It exists so the
/// envelope's *mechanics* --- versioning, rotation, unwrapping old versions --- can be
/// tested without a key-management service, and its name and description say what it is so
/// nobody runs it anywhere real.
#[derive(Debug)]
pub struct NoEncryption {
    current: KeyId,
    /// Versions this provider still accepts. Retiring one makes its data unreadable.
    known: Vec<KeyId>,
}

impl NoEncryption {
    /// A provider at version one.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        let current = KeyId::new(name, 1);
        Self {
            known: vec![current.clone()],
            current,
        }
    }

    /// Advance to the next key version, keeping the old one readable.
    pub fn rotate_key(&mut self) -> KeyId {
        self.current = self.current.next_version();
        self.known.push(self.current.clone());
        self.current.clone()
    }

    /// Stop accepting a key version.
    ///
    /// Exists so a test can prove what retiring a key too early costs: data wrapped under
    /// it becomes unreadable, which is data destruction rather than a configuration change.
    pub fn retire(&mut self, key: &KeyId) {
        self.known.retain(|k| k != key);
    }
}

impl KeyProvider for NoEncryption {
    fn current(&self) -> KeyId {
        self.current.clone()
    }

    fn wrap(&self, under: &KeyId, data_key: &[u8]) -> Result<WrappedKey, KeyError> {
        if !self.known.contains(under) {
            return Err(KeyError::UnknownKey { key: under.clone() });
        }
        // Reversible and not secret. The version is mixed in so a key wrapped under one
        // version cannot be unwrapped under another by accident, which is the property the
        // rotation tests actually exercise.
        let marker = u8::try_from(under.version % 251).unwrap_or(0);
        Ok(WrappedKey {
            wrapped_by: under.clone(),
            bytes: data_key.iter().map(|b| b ^ marker).collect(),
        })
    }

    fn unwrap(&self, wrapped: &WrappedKey) -> Result<Vec<u8>, KeyError> {
        if !self.known.contains(&wrapped.wrapped_by) {
            return Err(KeyError::UnknownKey {
                key: wrapped.wrapped_by.clone(),
            });
        }
        let marker = u8::try_from(wrapped.wrapped_by.version % 251).unwrap_or(0);
        Ok(wrapped.bytes.iter().map(|b| b ^ marker).collect())
    }

    fn describe(&self) -> String {
        format!(
            "NO ENCRYPTION — data keys are not protected, only transformed reversibly. \
             This exists to test envelope mechanics and must not be used where the data \
             matters. Current key {}",
            self.current
        )
    }
}

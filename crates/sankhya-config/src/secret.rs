//! Values that must not be printed.
//!
//! # Why redaction is a type and not a convention
//!
//! A password in a log line survives in every backup of that log, on every machine that
//! received it, for as long as the retention policy says — and rotating it does not remove
//! it. The exposure is permanent and the discovery is usually external.
//!
//! Conventions do not prevent this. Somebody adds a debug print during an incident, at the
//! moment when care is scarcest. So a secret is a **type whose `Debug` and `Display` do not
//! reveal it**, and reading the value takes a method whose name says what is happening.
//!
//! `check-logging` already refuses log statements that record what a caller supplied. This
//! is the same rule for the values a *deployment* supplies.

use std::fmt;

/// A configuration value that must not be printed.
///
/// `Debug` and `Display` both render `<redacted>`. Getting at the value requires
/// [`Secret::expose`], which is deliberately ugly to read in a diff.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wrap a value.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The value itself.
    ///
    /// Named so that a reviewer scanning a diff sees the word `expose` at every point where
    /// a secret leaves this type. That is the whole mechanism: the compiler cannot tell a
    /// connection string from a log line, and a person can.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Whether it holds anything.
    ///
    /// Useful for "is this configured at all", which is a question worth answering without
    /// exposing the value to answer it.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Whether a key names something that should never be printed.
///
/// Matched on the key rather than the value, because the value gives no clue and the key
/// almost always does. The list is deliberately short and suffix-or-substring based: a
/// broader rule that redacts ordinary settings trains people to work around it, and a
/// narrower one misses the case it was written for.
#[must_use]
pub fn looks_secret(key: &str) -> bool {
    const MARKERS: &[&str] = &[
        "password", "secret", "token", "credential", "private_key", "passphrase", "api_key",
    ];
    let lower = key.to_lowercase();
    MARKERS.iter().any(|marker| lower.contains(marker))
}

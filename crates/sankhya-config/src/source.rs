//! Where a setting's value came from.
//!
//! # Why every value carries this
//!
//! "The timeout is thirty seconds" is not an answer to "why is the timeout thirty seconds".
//! An operator debugging a deployment needs to know whether a value came from the file they
//! just edited, from an environment variable set three layers up in a container spec, or
//! from a command line somebody typed in a systemd unit two years ago — and the value alone
//! is identical in all three cases.
//!
//! This is the same reason a cube result carries its overlay name and a graph result carries
//! its truncation: a figure without its provenance is a figure nobody can act on.

use std::fmt;

/// Where a value was read from, in precedence order.
///
/// Ordered so that `Ordering` is precedence: a later variant overrides an earlier one. The
/// ordering is the rule, rather than a comment describing a rule implemented elsewhere.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Source {
    /// A configuration file. Among files, the rightmost wins.
    File,
    /// A `.local` overlay beside a configuration file.
    ///
    /// Distinguished from [`Source::File`] because it is the answer to "why does this
    /// machine differ from the others", and that question is asked often enough to deserve
    /// its own answer rather than a path somebody has to notice.
    LocalOverlay,
    /// An environment variable.
    Environment,
    /// A `--key=value` argument.
    CommandLine,
}

impl Source {
    /// What to call it in a message to a person.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::File => "a configuration file",
            Self::LocalOverlay => "a .local overlay",
            Self::Environment => "an environment variable",
            Self::CommandLine => "a command-line argument",
        }
    }

    /// Whether a value from `self` is overridden by one from `other`.
    #[must_use]
    pub fn is_overridden_by(self, other: Self) -> bool {
        other >= self
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A value and where it came from.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Origin {
    /// The value.
    pub value: String,
    /// Where it was read from.
    pub source: Source,
    /// Which file, when it came from one.
    ///
    /// Named rather than indexed: "the third file" requires the reader to reconstruct the
    /// list the process was started with, which they usually cannot.
    pub file: Option<String>,
}

impl Origin {
    /// A value from a source that is not a file.
    #[must_use]
    pub fn from(value: impl Into<String>, source: Source) -> Self {
        Self {
            value: value.into(),
            source,
            file: None,
        }
    }

    /// A value from a named file.
    #[must_use]
    pub fn from_file(value: impl Into<String>, source: Source, file: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            source,
            file: Some(file.into()),
        }
    }

    /// A sentence an operator can act on.
    #[must_use]
    pub fn explain(&self, key: &str) -> String {
        match &self.file {
            Some(file) => format!("`{key}` came from {} ({file})", self.source),
            None => format!("`{key}` came from {}", self.source),
        }
    }
}

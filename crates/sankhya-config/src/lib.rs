//! Typed configuration: several files, an ordered precedence, and a value that knows where
//! it came from.
//!
//! # What this is for
//!
//! A deployment is configured from more than one place, always. A file in the image, a file
//! the operator edited, an environment variable a container spec sets, a flag in a systemd
//! unit. The question that matters at three in the morning is not *what is the timeout* but
//! **why is the timeout that**, and the value alone cannot answer it.
//!
//! So every setting carries its [`Origin`], and [`Configuration::explain`] turns it into a
//! sentence.
//!
//! # Precedence
//!
//! Highest wins:
//!
//! 1. `--key=value` on the command line
//! 2. an environment variable of the same name
//! 3. a `.local` overlay beside a configuration file
//! 4. a configuration file, rightmost of those given
//!
//! The order is [`Source`]'s own ordering rather than a comment describing a rule
//! implemented somewhere else.
//!
//! # Local overlays
//!
//! Each file `x.yaml` is followed by `x.local.yaml` if it exists. This is for values a
//! machine needs in order to start and that must not be committed --- a signing secret in a
//! tracked file is a public signing secret, and signing sessions with a public value lets
//! anyone forge one. A missing overlay changes nothing.
//!
//! # Three refusals
//!
//! This crate refuses where a configuration library usually shrugs, and each refusal is the
//! same principle applied:
//!
//! - **A malformed file fails the load.** Never start half-configured; a process with three
//!   of its four settings behaves plausibly and wrongly.
//! - **An unresolved `${...}` fails the load.** See [`resolve`] --- a placeholder that
//!   reaches a connection string is a configuration error rediscovered as a network one.
//! - **An unparseable typed value fails the read.** `port=eighty` must not quietly become
//!   the default. That is the same defect as an absent cell reading as zero.

#![doc(html_root_url = "https://docs.rs/sankhya-config")]

pub mod parse;
pub mod resolve;
pub mod secret;
pub mod source;

pub use secret::{looks_secret, Secret};
pub use source::{Origin, Source};

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

/// Everything this process is configured with.
#[derive(Clone, Debug, Default)]
pub struct Configuration {
    settings: BTreeMap<String, Origin>,
    files: Vec<PathBuf>,
    stamps: BTreeMap<PathBuf, std::time::SystemTime>,
}

/// Why a configuration could not be loaded.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ConfigError {
    /// A file could not be read or parsed.
    Unreadable(parse::ParseError),
    /// A reference could not be resolved.
    Unresolved(resolve::Unresolved),
    /// A value was asked for as a type it is not.
    NotA {
        /// The setting.
        key: String,
        /// What it was asked for as.
        wanted: &'static str,
        /// What it holds.
        found: String,
        /// Where the value came from, so the fix is in the right file.
        origin: String,
    },
    /// A setting with no value and no default was required.
    Missing {
        /// The setting.
        key: String,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable(error) => write!(f, "{error}"),
            Self::Unresolved(error) => write!(f, "{error}"),
            Self::NotA { key, wanted, found, origin } => write!(
                f,
                "`{key}` must be {wanted} and holds `{found}` — {origin}. Refused rather \
                 than defaulted: a setting that silently becomes something else is a \
                 deployment behaving as though it were configured when it is not"
            ),
            Self::Missing { key } => write!(
                f,
                "`{key}` is required and is not set in any file, environment variable or \
                 argument"
            ),
        }
    }
}

impl std::error::Error for ConfigError {}

/// What a reload changed.
///
/// Returned rather than applied silently. A value changing under a running system is a
/// hazard worth naming: a reload nobody is told about is indistinguishable from a bug.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Changed {
    /// Settings that took a new value, with the old one.
    pub updated: BTreeMap<String, String>,
    /// Settings that appeared.
    pub added: Vec<String>,
    /// Settings that are no longer set anywhere.
    pub removed: Vec<String>,
}

impl Changed {
    /// Whether anything moved.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.updated.is_empty() && self.added.is_empty() && self.removed.is_empty()
    }

    /// A line an operator can read, or `None` if nothing changed.
    ///
    /// Values are omitted for anything [`looks_secret`] recognises: a reload log that prints
    /// the new password is worse than one that prints nothing.
    #[must_use]
    pub fn explain(&self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let mut parts = Vec::new();
        for (key, was) in &self.updated {
            if looks_secret(key) {
                parts.push(format!("{key} changed"));
            } else {
                parts.push(format!("{key} changed from `{was}`"));
            }
        }
        if !self.added.is_empty() {
            parts.push(format!("added {}", self.added.join(", ")));
        }
        if !self.removed.is_empty() {
            parts.push(format!("removed {}", self.removed.join(", ")));
        }
        Some(parts.join("; "))
    }
}

impl Configuration {
    /// Load from files, in order, lowest precedence first.
    ///
    /// Each file is followed by its `.local` overlay if one exists. A file that does not
    /// exist is skipped; a file that exists and cannot be read is an error, because those
    /// are different situations and only one of them is somebody's mistake.
    ///
    /// # Errors
    /// [`ConfigError::Unreadable`] or [`ConfigError::Unresolved`].
    pub fn load<P: AsRef<Path>>(files: &[P]) -> Result<Self, ConfigError> {
        Self::load_with(files, &std::env::vars().collect(), &command_line_args())
    }

    /// The same, taking the environment and arguments explicitly.
    ///
    /// Explicit because a test that has to mutate the real environment is a test that cannot
    /// run beside another one.
    ///
    /// # Errors
    /// As [`Configuration::load`].
    pub fn load_with<P: AsRef<Path>>(
        files: &[P],
        environment: &BTreeMap<String, String>,
        arguments: &BTreeMap<String, String>,
    ) -> Result<Self, ConfigError> {
        let mut ordered: Vec<(PathBuf, Source)> = Vec::new();
        for file in files {
            let path = file.as_ref().to_path_buf();
            ordered.push((path.clone(), Source::File));
            if let Some(overlay) = local_overlay(&path) {
                if overlay.exists() {
                    ordered.push((overlay, Source::LocalOverlay));
                }
            }
        }

        let mut raw: BTreeMap<String, String> = BTreeMap::new();
        let mut origins: BTreeMap<String, Origin> = BTreeMap::new();
        let mut stamps = BTreeMap::new();
        for (path, source) in &ordered {
            if !path.exists() {
                continue;
            }
            if let Ok(stamp) = std::fs::metadata(path).and_then(|m| m.modified()) {
                stamps.insert(path.clone(), stamp);
            }
            let parsed = parse::file(path).map_err(ConfigError::Unreadable)?;
            for (key, value) in parsed {
                raw.insert(key.clone(), value.clone());
                origins.insert(
                    key,
                    Origin::from_file(value, *source, path.display().to_string()),
                );
            }
        }

        // Resolution sees the higher-precedence sources, so `${HOME}` reaches the
        // environment rather than only the files.
        let lookup = |name: &str| {
            arguments
                .get(name)
                .or_else(|| environment.get(name))
                .cloned()
        };
        let resolved = resolve::all(&raw, &lookup).map_err(ConfigError::Unresolved)?;

        let mut settings: BTreeMap<String, Origin> = BTreeMap::new();
        for (key, value) in resolved {
            let mut origin = origins.remove(&key).unwrap_or_else(|| Origin::from(value.clone(), Source::File));
            origin.value = value;
            settings.insert(key, origin);
        }
        // Then the sources that outrank a file, including keys no file mentions.
        for (key, value) in environment {
            settings.insert(key.clone(), Origin::from(value.clone(), Source::Environment));
        }
        for (key, value) in arguments {
            settings.insert(key.clone(), Origin::from(value.clone(), Source::CommandLine));
        }

        Ok(Self {
            settings,
            files: ordered.into_iter().map(|(path, _)| path).collect(),
            stamps,
        })
    }

    /// A setting's value, if it is set.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.settings.get(key).map(|o| o.value.as_str())
    }

    /// A setting's value, or a fallback.
    #[must_use]
    pub fn get_or<'a>(&'a self, key: &str, fallback: &'a str) -> &'a str {
        self.get(key).unwrap_or(fallback)
    }

    /// A required setting.
    ///
    /// # Errors
    /// [`ConfigError::Missing`] naming the key.
    pub fn require(&self, key: &str) -> Result<&str, ConfigError> {
        self.get(key).ok_or_else(|| ConfigError::Missing {
            key: key.to_string(),
        })
    }

    /// A setting as a secret, which will not print.
    #[must_use]
    pub fn secret(&self, key: &str) -> Option<Secret> {
        self.get(key).map(Secret::new)
    }

    /// Where a setting came from.
    #[must_use]
    pub fn origin(&self, key: &str) -> Option<&Origin> {
        self.settings.get(key)
    }

    /// A sentence saying where a setting came from.
    #[must_use]
    pub fn explain(&self, key: &str) -> Option<String> {
        self.settings.get(key).map(|origin| origin.explain(key))
    }

    /// Every key, in order.
    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.settings.keys()
    }

    /// Every key beginning with a prefix, with the prefix removed.
    ///
    /// For a section --- `table.orders.*` --- without the caller writing the prefix logic
    /// again at each site and getting the boundary wrong on one of them.
    #[must_use]
    pub fn section(&self, prefix: &str) -> BTreeMap<String, String> {
        let with_dot = if prefix.ends_with('.') {
            prefix.to_string()
        } else {
            format!("{prefix}.")
        };
        self.settings
            .iter()
            .filter_map(|(key, origin)| {
                key.strip_prefix(&with_dot)
                    .map(|rest| (rest.to_string(), origin.value.clone()))
            })
            .collect()
    }

    /// Whether any file has changed since it was read.
    #[must_use]
    pub fn is_stale(&self) -> bool {
        self.files.iter().any(|path| {
            let now = std::fs::metadata(path).and_then(|m| m.modified()).ok();
            match (now, self.stamps.get(path)) {
                (Some(now), Some(then)) => now > *then,
                // A file that has appeared since the load counts: an overlay somebody just
                // created is exactly the case reloading exists for.
                (Some(_), None) => true,
                _ => false,
            }
        })
    }

    /// Reload, returning what moved.
    ///
    /// # Errors
    /// As [`Configuration::load`]. **The current configuration is kept on failure**: a
    /// process running on a good configuration must not be pushed onto a broken one because
    /// somebody saved a file mid-edit.
    pub fn reload_with(
        &mut self,
        environment: &BTreeMap<String, String>,
        arguments: &BTreeMap<String, String>,
    ) -> Result<Changed, ConfigError> {
        let files: Vec<PathBuf> = self
            .files
            .iter()
            .filter(|path| {
                // Overlays are re-derived by `load_with`; passing them again would load them
                // twice and, worse, at the wrong precedence.
                local_overlay(path).is_none_or(|_| !is_overlay(path))
            })
            .cloned()
            .collect();
        let fresh = Self::load_with(&files, environment, arguments)?;

        let mut changed = Changed::default();
        for (key, origin) in &fresh.settings {
            match self.settings.get(key) {
                Some(before) if before.value != origin.value => {
                    changed.updated.insert(key.clone(), before.value.clone());
                }
                Some(_) => {}
                None => changed.added.push(key.clone()),
            }
        }
        for key in self.settings.keys() {
            if !fresh.settings.contains_key(key) {
                changed.removed.push(key.clone());
            }
        }
        *self = fresh;
        Ok(changed)
    }
}

/// `x.local.yaml` for `x.yaml`.
fn local_overlay(path: &Path) -> Option<PathBuf> {
    if is_overlay(path) {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    let extension = path.extension().and_then(|e| e.to_str());
    let name = match extension {
        Some(extension) => format!("{stem}.local.{extension}"),
        None => format!("{stem}.local"),
    };
    Some(path.with_file_name(name))
}

/// Whether a path is itself an overlay.
fn is_overlay(path: &Path) -> bool {
    path.file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(|stem| stem.ends_with(".local"))
}

/// `--key=value` arguments, by name.
fn command_line_args() -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for argument in std::env::args().skip(1) {
        if let Some(pair) = argument.strip_prefix("--") {
            if let Some((key, value)) = pair.split_once('=') {
                out.insert(key.trim().to_string(), value.trim().to_string());
            }
        }
    }
    out
}

/// Typed reads.
///
/// # Why each of these refuses rather than defaults
///
/// `port=eighty` must not quietly become `8080`. A setting that silently becomes something
/// else is a deployment behaving as though it were configured when it is not, and the
/// failure surfaces wherever the wrong value is used rather than where it was written.
///
/// This is the same rule as an absent cell that is not zero and a truncated result that is
/// not a complete one: **the shape of the answer must not hide the fact that there is no
/// answer.**
impl Configuration {
    /// A setting as an integer.
    ///
    /// # Errors
    /// [`ConfigError::NotA`] when it is set and is not an integer. `Ok(None)` when unset,
    /// which is a different situation and gets a different answer.
    pub fn integer(&self, key: &str) -> Result<Option<i64>, ConfigError> {
        self.parsed(key, "an integer", |text| text.parse::<i64>().ok())
    }

    /// A setting as a number.
    ///
    /// # Errors
    /// As [`Configuration::integer`].
    pub fn number(&self, key: &str) -> Result<Option<f64>, ConfigError> {
        self.parsed(key, "a number", |text| {
            text.parse::<f64>().ok().filter(|value| value.is_finite())
        })
    }

    /// A setting as a boolean.
    ///
    /// Accepts the spellings people actually write. Anything else is refused rather than
    /// read as false --- `enabled=maybe` silently disabling a feature is exactly the kind of
    /// wrong that nobody finds until they need the feature.
    ///
    /// # Errors
    /// As [`Configuration::integer`].
    pub fn boolean(&self, key: &str) -> Result<Option<bool>, ConfigError> {
        self.parsed(key, "true or false", |text| {
            match text.trim().to_lowercase().as_str() {
                "true" | "yes" | "on" | "1" => Some(true),
                "false" | "no" | "off" | "0" => Some(false),
                _ => None,
            }
        })
    }

    /// A setting as a duration, written as `30s`, `5m`, `2h` or a bare number of seconds.
    ///
    /// # Errors
    /// As [`Configuration::integer`].
    pub fn duration(&self, key: &str) -> Result<Option<std::time::Duration>, ConfigError> {
        self.parsed(key, "a duration such as 30s, 5m or 2h", |text| {
            let text = text.trim();
            let (number, multiplier) = match text.chars().last() {
                Some('s') => (&text[..text.len() - 1], 1),
                Some('m') => (&text[..text.len() - 1], 60),
                Some('h') => (&text[..text.len() - 1], 3_600),
                Some('d') => (&text[..text.len() - 1], 86_400),
                _ => (text, 1),
            };
            number
                .trim()
                .parse::<u64>()
                .ok()
                .map(|value| std::time::Duration::from_secs(value.saturating_mul(multiplier)))
        })
    }

    /// A setting as a list, comma-separated.
    ///
    /// An unset key gives `None` rather than an empty list, because "configured to nothing"
    /// and "not configured" are different and a caller may need to tell them apart.
    #[must_use]
    pub fn list(&self, key: &str) -> Option<Vec<String>> {
        self.get(key).map(|text| {
            text.split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(str::to_string)
                .collect()
        })
    }

    /// A setting as a path, resolved against `base` when it is relative.
    ///
    /// Relative paths in configuration are read relative to wherever the process happened to
    /// be started, which is a different directory under systemd, under a container, and in a
    /// developer's shell. Anchoring them is the difference between a setting that means the
    /// same thing everywhere and one that does not.
    #[must_use]
    pub fn path(&self, key: &str, base: &Path) -> Option<PathBuf> {
        self.get(key).map(|text| {
            let path = PathBuf::from(text);
            if path.is_absolute() {
                path
            } else {
                base.join(path)
            }
        })
    }

    /// Read a setting through a parser, refusing what it rejects.
    fn parsed<T>(
        &self,
        key: &str,
        wanted: &'static str,
        parse: impl Fn(&str) -> Option<T>,
    ) -> Result<Option<T>, ConfigError> {
        let Some(origin) = self.settings.get(key) else {
            return Ok(None);
        };
        parse(&origin.value).map(Some).ok_or_else(|| ConfigError::NotA {
            key: key.to_string(),
            wanted,
            // The offending value, unless the key names a secret --- in which case the fact
            // that it is unparseable is reportable and its contents are not.
            found: if looks_secret(key) {
                "<redacted>".to_string()
            } else {
                origin.value.clone()
            },
            origin: origin.explain(key),
        })
    }
}

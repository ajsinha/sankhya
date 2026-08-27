//! Loading bundles, and replacing them while the server runs.
//!
//! # Reload is atomic or it does not happen
//!
//! A reload that half-applies is worse than one that fails: some queries would then see the
//! new definitions and some the old, and which is which depends on timing. So a reload
//! parses, verifies and validates **every** bundle before replacing **any** of them, and a
//! single bad file leaves the previous set entirely in place.
//!
//! The consequence worth stating: a reload can fail, and the server keeps running the
//! definitions it already had. That is the intended behaviour, and the outcome says which
//! files were refused so an operator can fix them rather than guess.

use crate::bundle::{Bundle, BundleError, Validated};
use crate::verify::{Digest, Trust, Verifier};
use std::path::{Path, PathBuf};

/// One bundle that loaded successfully.
#[derive(Clone, Debug)]
pub struct Loaded {
    /// Where it came from.
    pub source: PathBuf,
    /// The digest of the bytes that were loaded.
    ///
    /// Recorded so an operator can pin it, and so a later reload can tell whether the file
    /// actually changed.
    pub digest: Digest,
    /// The checked contents.
    pub bundle: Validated,
}

/// What a reload did.
#[derive(Clone, Debug)]
pub struct ReloadOutcome {
    /// Bundles now in effect.
    pub loaded: Vec<Loaded>,
    /// Files that were refused, and why.
    pub refused: Vec<(PathBuf, String)>,
    /// Whether the loaded set replaced the previous one.
    ///
    /// False when something was refused: the reload is all-or-nothing, so a single bad file
    /// leaves the previous set in place. An operator seeing `false` knows the server is
    /// still serving what it was before.
    pub applied: bool,
}

impl ReloadOutcome {
    /// Whether everything offered was accepted.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.refused.is_empty()
    }

    /// A sentence summarising what happened, for a log.
    #[must_use]
    pub fn summary(&self) -> String {
        if self.applied {
            format!("{} bundle(s) in effect", self.loaded.len())
        } else {
            format!(
                "reload refused: {} file(s) did not load, and the previous {} bundle(s) \
                 remain in effect",
                self.refused.len(),
                self.loaded.len()
            )
        }
    }
}

/// Loads and reloads declarative packs from a directory.
#[derive(Debug)]
pub struct Loader {
    directory: PathBuf,
    verifier: Box<dyn Verifier>,
    current: Vec<Loaded>,
}

impl Loader {
    /// A loader reading `directory`, admitting only what `verifier` allows.
    #[must_use]
    pub fn new(directory: impl Into<PathBuf>, verifier: Box<dyn Verifier>) -> Self {
        Self {
            directory: directory.into(),
            verifier,
            current: Vec::new(),
        }
    }

    /// What the verification policy is, for a startup log.
    #[must_use]
    pub fn policy(&self) -> String {
        self.verifier.describe()
    }

    /// The bundles currently in effect.
    #[must_use]
    pub fn current(&self) -> &[Loaded] {
        &self.current
    }

    /// Read every `.toml` in the directory and, if all of them are good, put them in effect.
    ///
    /// All-or-nothing. Half a reload means some queries see new definitions and some see
    /// old ones, depending on timing, which is a class of bug nobody can reproduce.
    pub fn reload(&mut self) -> ReloadOutcome {
        let mut candidates = Vec::new();
        let mut refused = Vec::new();

        let entries = match std::fs::read_dir(&self.directory) {
            Ok(entries) => entries,
            Err(error) => {
                return ReloadOutcome {
                    loaded: self.current.clone(),
                    refused: vec![(self.directory.clone(), error.to_string())],
                    applied: false,
                };
            }
        };

        // Sorted, so two servers reading the same directory load in the same order and a
        // name collision is reported against the same file on both.
        let mut paths: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "toml"))
            .collect();
        paths.sort();

        for path in paths {
            match self.load_one(&path) {
                Ok(loaded) => candidates.push(loaded),
                Err(reason) => refused.push((path, reason)),
            }
        }

        // Two bundles claiming the same function name is a conflict between files, which
        // neither file can detect on its own.
        if let Some(clash) = first_name_clash(&candidates) {
            refused.push((PathBuf::from("<the bundle set>"), clash));
        }

        if refused.is_empty() {
            self.current = candidates;
            ReloadOutcome {
                loaded: self.current.clone(),
                refused,
                applied: true,
            }
        } else {
            ReloadOutcome {
                loaded: self.current.clone(),
                refused,
                applied: false,
            }
        }
    }

    /// Read, verify and validate one file.
    fn load_one(&self, path: &Path) -> Result<Loaded, String> {
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        let source = path.display().to_string();

        // Verification precedes parsing, deliberately. Parsing untrusted bytes is a smaller
        // attack surface than executing them, but it is not zero, and there is no reason to
        // do it for a file that is going to be refused anyway.
        if let Trust::Refused { reason } = self.verifier.verify(&source, &bytes) {
            return Err(reason);
        }

        let text = String::from_utf8(bytes.clone())
            .map_err(|_| "the bundle is not valid UTF-8".to_string())?;
        let bundle = Bundle::from_toml(&text).map_err(|e: BundleError| e.to_string())?;
        let validated = bundle.validate().map_err(|e| e.to_string())?;

        Ok(Loaded {
            source: path.to_path_buf(),
            digest: Digest::of(&bytes),
            bundle: validated,
        })
    }
}

/// The first function name claimed by two different bundles, if any.
fn first_name_clash(candidates: &[Loaded]) -> Option<String> {
    let mut seen: std::collections::BTreeMap<&str, &Path> = std::collections::BTreeMap::new();
    for loaded in candidates {
        let names = loaded
            .bundle
            .functions
            .iter()
            .map(|f| f.name.as_str())
            .chain(loaded.bundle.queries.iter().map(|q| q.name.as_str()));
        for name in names {
            if let Some(previous) = seen.insert(name, &loaded.source) {
                return Some(format!(
                    "'{name}' is declared by both {} and {}; resolving it by load order \
                     would make the answer depend on directory listing order",
                    previous.display(),
                    loaded.source.display()
                ));
            }
        }
    }
    None
}

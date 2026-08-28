//! Reading a configuration file into a flat namespace.
//!
//! # One namespace, whatever the format
//!
//! YAML nests and a `.properties` file does not. Keeping both shapes would mean precedence,
//! resolution and override logic each handling two cases, and the second case is the one
//! nobody tests. So YAML is **flattened to dotted keys** — `server: {listen: "0.0.0.0"}`
//! becomes `server.listen` — and everything downstream sees one flat map.
//!
//! A list becomes indexed keys, `logging.appenders.0`, plus the joined form at the bare key.
//! Both are provided because both are asked for: code wants the list, and an override wants
//! to replace one element without restating the rest.
//!
//! # A malformed file is fatal
//!
//! Never start half-loaded. A process that comes up with three of its four settings behaves
//! plausibly and wrongly, and the missing one is discovered by whatever it broke.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

/// A file that could not be read as configuration.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ParseError {
    /// Which file.
    pub file: String,
    /// What was wrong with it.
    pub detail: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} could not be read as configuration: {}. Nothing was loaded from it — a \
             process that starts with part of its configuration behaves plausibly and \
             wrongly, and the missing setting is discovered by whatever it breaks",
            self.file, self.detail
        )
    }
}

impl std::error::Error for ParseError {}

/// Read a file into flat, dotted keys.
///
/// The format is chosen by extension: `.yaml` and `.yml` are YAML, everything else is
/// line-oriented `key=value`.
///
/// # Errors
/// [`ParseError`] when the file cannot be read or is not valid in its format.
pub fn file(path: &Path) -> Result<BTreeMap<String, String>, ParseError> {
    let text = std::fs::read_to_string(path).map_err(|error| ParseError {
        file: path.display().to_string(),
        detail: error.to_string(),
    })?;
    let is_yaml = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("yaml") || e.eq_ignore_ascii_case("yml"));

    if is_yaml {
        yaml(&text).map_err(|detail| ParseError {
            file: path.display().to_string(),
            detail,
        })
    } else {
        Ok(properties(&text))
    }
}

/// Flatten YAML into dotted keys.
///
/// # Errors
/// A description of what the YAML parser objected to.
pub fn yaml(text: &str) -> Result<BTreeMap<String, String>, String> {
    let value: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(text).map_err(|error| error.to_string())?;
    let mut out = BTreeMap::new();
    flatten("", &value, &mut out);
    Ok(out)
}

/// Line-oriented `key=value`, with `#` and `!` comments.
#[must_use]
pub fn properties(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        // Split on the first separator only: a value may contain `=`, and a connection
        // string usually does.
        if let Some((key, value)) = line.split_once('=') {
            out.insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    out
}

/// Walk a YAML value, emitting one entry per leaf.
fn flatten(prefix: &str, value: &serde_yaml_ng::Value, out: &mut BTreeMap<String, String>) {
    let join = |key: &str| {
        if prefix.is_empty() {
            key.to_string()
        } else {
            format!("{prefix}.{key}")
        }
    };

    match value {
        serde_yaml_ng::Value::Mapping(map) => {
            for (key, child) in map {
                let Some(name) = scalar(key) else {
                    // A non-scalar key has no dotted form. Skipped rather than guessed at:
                    // inventing a name for it would put a setting under a key nobody can
                    // write in an override.
                    continue;
                };
                flatten(&join(&name), child, out);
            }
        }
        serde_yaml_ng::Value::Sequence(items) => {
            let mut joined = Vec::with_capacity(items.len());
            for (index, item) in items.iter().enumerate() {
                flatten(&join(&index.to_string()), item, out);
                if let Some(text) = scalar(item) {
                    joined.push(text);
                }
            }
            // The joined form as well, so an override can replace the whole list without
            // knowing how long it was — and so a caller that wants a list gets one.
            if !prefix.is_empty() && joined.len() == items.len() {
                out.insert(prefix.to_string(), joined.join(","));
            }
        }
        other => {
            if let Some(text) = scalar(other) {
                if !prefix.is_empty() {
                    out.insert(prefix.to_string(), text);
                }
            }
        }
    }
}

/// A YAML scalar as a string, or `None` for anything else.
///
/// `null` becomes the empty string rather than being dropped. A key present with no value is
/// a deliberate statement --- "this is configured, to nothing" --- and dropping it makes it
/// indistinguishable from a key nobody wrote.
fn scalar(value: &serde_yaml_ng::Value) -> Option<String> {
    match value {
        serde_yaml_ng::Value::Null => Some(String::new()),
        serde_yaml_ng::Value::Bool(b) => Some(b.to_string()),
        serde_yaml_ng::Value::Number(n) => Some(n.to_string()),
        serde_yaml_ng::Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

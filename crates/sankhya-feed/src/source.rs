//! Reading the records a source holds.
//!
//! # One document per line
//!
//! A source is newline-delimited JSON: one dictionary per line. Not a single top-level array,
//! which has to be read whole before any of it can be used and cannot be resumed part-way ---
//! and a feed's whole restart story is about being resumed part-way.
//!
//! It also makes a truncated file readable up to its last complete line, which is the ordinary
//! state of a file somebody is still writing. That last partial line is a record like any
//! other: refused, quarantined whole, and replayable once the file is finished.
//!
//! # Sources are read in name order
//!
//! Because the position is a high-water mark rather than a set (see [`crate::progress`]), the
//! order has to be a total one the feed and a person agree about. Name order is that: it is
//! visible in a directory listing, it is what date-stamped spool files already have, and a
//! feed whose files do not sort meaningfully has a naming problem this makes obvious rather
//! than hides.

use crate::bind::Unfit;
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

/// One record, as it arrived.
#[derive(Clone, PartialEq, Debug)]
pub struct Arrived {
    /// Which record of the source this is, counting from zero.
    pub position: u64,
    /// The line, exactly as it was read. Kept whole so a refusal can be replayed.
    pub text: String,
    /// The parsed document, or why it is not one.
    pub document: Result<Value, Unfit>,
}

/// Why a source could not be read.
#[derive(Debug)]
pub struct Unreadable {
    /// The file.
    pub path: PathBuf,
    /// What the filesystem said.
    pub detail: String,
}

impl std::fmt::Display for Unreadable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "`{}` could not be read: {}", self.path.display(), self.detail)
    }
}

impl std::error::Error for Unreadable {}

/// Every source in a directory, in name order.
///
/// Directories and anything unreadable are skipped; a directory inside a spool is somebody
/// else's business, and this is not a recursive walk.
///
/// # Errors
///
/// [`Unreadable`] when the directory itself cannot be listed --- which is a configuration
/// problem rather than a record problem, and stops the feed rather than being quarantined.
pub fn sources(directory: &Path) -> Result<Vec<PathBuf>, Unreadable> {
    let entries = std::fs::read_dir(directory).map_err(|error| Unreadable {
        path: directory.to_path_buf(),
        detail: error.to_string(),
    })?;
    let mut found: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect();
    found.sort();
    Ok(found)
}

/// Read a source's records, skipping the first `skip` of them.
///
/// Skipping rather than seeking to a byte offset: a byte offset into a file being appended to
/// is only meaningful if nothing before it ever changes, and counting records is what the
/// position records anyway. It costs a scan of what is already published, which is bounded by
/// the source and paid once per restart.
///
/// # Errors
///
/// [`Unreadable`] when the file cannot be opened or a line cannot be read.
pub fn records(path: &Path, skip: u64) -> Result<Vec<Arrived>, Unreadable> {
    let file = std::fs::File::open(path).map_err(|error| Unreadable {
        path: path.to_path_buf(),
        detail: error.to_string(),
    })?;
    let mut arrived = Vec::new();
    for (index, line) in BufReader::new(file).lines().enumerate() {
        let text = line.map_err(|error| Unreadable {
            path: path.to_path_buf(),
            detail: error.to_string(),
        })?;
        let position = index as u64;
        if position < skip {
            continue;
        }
        // A blank line is not a record and is not a refusal either. Files written by hand
        // and files ending with a newline both produce them, and quarantining those would
        // fill the quarantine with nothing.
        if text.trim().is_empty() {
            continue;
        }
        let document = serde_json::from_str::<Value>(&text).map_err(|error| Unfit::Unparseable {
            detail: error.to_string(),
        });
        arrived.push(Arrived { position, text, document });
    }
    Ok(arrived)
}

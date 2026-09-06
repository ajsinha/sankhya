//! The chain, on disk.
//!
//! # What was wrong
//!
//! [`Chain`](crate::Chain) is a `Vec`. The hash-linked, tamper-evident audit that
//! `docs/book/part3/13-security.md` §13.5 describes, and that `docs/STATUS.md` marked as a met
//! criterion, was **erased by a restart** --- so the record of who reached what survived exactly
//! as long as the process did, and the one event most likely to accompany an incident is a
//! server going down. `SEC-07`.
//!
//! # Why a line per record and not a database
//!
//! Because the property the audit needs from its storage is *append-only*, and a file opened for
//! append is the only shape where that is enforced by the thing doing the writing rather than by
//! the code that means to. A row store would let a later version issue an `UPDATE`, and an audit
//! whose storage can be updated is an audit whose storage can be rewritten.
//!
//! It also survives this program. A chain that can only be read by the binary that wrote it is a
//! chain nobody audits: `jq` reads this, and so does a person.
//!
//! # What this is still not
//!
//! **Not a defence against an attacker with the disk.** They can truncate the tail, and a chain
//! cannot detect its own truncation --- removing the last *n* records leaves one that verifies
//! perfectly. Only publishing [`Chain::head`](crate::Chain::head) somewhere append-only makes the
//! true length knowable, which is why the head is printed at every start and why §13.5 says so
//! rather than leaving it to be discovered.

use crate::chain::{Chain, Record};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

/// The file the chain is written to, under the warehouse.
///
/// `_`-prefixed, so `warehouse::discover` skips it: the audit is not a user table and must not
/// appear in a catalogue somebody browses.
pub const DIRECTORY: &str = "_audit";

/// The file inside it.
pub const FILE: &str = "chain.jsonl";

/// Where a warehouse's audit lives.
#[must_use]
pub fn path_of(warehouse: &Path) -> PathBuf {
    warehouse.join(DIRECTORY).join(FILE)
}

/// An open journal, appended to as records are made.
#[derive(Debug)]
pub struct Journal {
    file: File,
}

impl Journal {
    /// Open the journal for this warehouse, creating it if it is not there.
    ///
    /// # Errors
    ///
    /// When the directory cannot be made or the file cannot be opened for appending.
    pub fn open(warehouse: &Path) -> std::io::Result<Self> {
        let at = path_of(warehouse);
        if let Some(parent) = at.parent() {
            std::fs::create_dir_all(parent)?;
            // The directory entry, made durable before anything is written into it. A file
            // whose bytes are synced and whose *name* is not is a file a crash loses whole,
            // which is the failure `sankhya-atomicfs` exists to spell out --- and an audit that
            // loses its first records loses exactly the ones an investigation starts from.
            File::open(parent)?.sync_all()?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&at)?;
        Ok(Self { file })
    }

    /// Write one record and make it durable before returning.
    ///
    /// # Why this syncs every time
    ///
    /// Because the alternative is an audit that is missing precisely the records written in the
    /// seconds before the thing that made the audit interesting. Buffering here would give up the
    /// one property this file exists to provide, for a write rate no audit needs: this is one
    /// line per statement, not one per row.
    ///
    /// # Errors
    ///
    /// When the record cannot be serialised, written or synced.
    pub fn append(&mut self, record: &Record) -> std::io::Result<()> {
        let mut line = serde_json::to_string(record)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        line.push('\n');
        self.file.write_all(line.as_bytes())?;
        self.file.sync_data()
    }
}

/// Read a chain back, with a complaint for every line that is not a record.
///
/// # Why a malformed line is reported rather than skipped
///
/// A skipped line is a record that silently is not in the chain --- and because each record
/// links to the one before it, a gap makes every record after it fail verification. Reporting
/// says *which* line, once, instead of leaving somebody to bisect a file to find out why an
/// audit that was never touched will not verify.
///
/// The records that did parse are returned regardless, because a chain with a hole in it is
/// still the best evidence available and refusing to load it helps nobody.
#[must_use]
pub fn read(warehouse: &Path) -> (Chain, Vec<String>) {
    into(warehouse, Chain::new())
}

/// The same, into a chain that keeps only its most recent records.
///
/// What a server uses at startup. The alternative --- what shipped --- is that a warehouse with
/// a year of audit behind it loads the whole of it into memory before answering anything, which
/// turns `OPS-04`'s unbounded growth into an unbounded *boot*. The file is the chain; a running
/// process holds a window onto it, and `len` and `head` still describe the whole.
#[must_use]
pub fn read_windowed(warehouse: &Path, window: usize) -> (Chain, Vec<String>) {
    into(warehouse, Chain::keeping(window))
}

/// Read every line of the journal into the chain it is given.
///
/// The chain decides what it keeps --- everything, or a window --- and this reads the file the
/// same way either way. A window makes the *memory* bounded, not the reading: every line is
/// still parsed, because a line that does not parse is a hole in the chain and reporting it is
/// the whole point of this function.
fn into(warehouse: &Path, chain: Chain) -> (Chain, Vec<String>) {
    let at = path_of(warehouse);
    let mut chain = chain;
    let Ok(file) = File::open(&at) else {
        // No file is the ordinary case for a warehouse that has answered nothing yet.
        return (chain, Vec::new());
    };
    let mut complaints = Vec::new();
    for (number, line) in BufReader::new(file).lines().enumerate() {
        let Ok(line) = line else {
            complaints.push(format!("{}: line {} could not be read", at.display(), number + 1));
            continue;
        };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Record>(&line) {
            // Appended **raw**: the digests were computed when the record was made and are the
            // evidence. Recomputing them here would make every stored chain verify by
            // construction, which is the one thing a verification must not do.
            Ok(record) => chain.append_raw(record),
            Err(error) => complaints.push(format!(
                "{}: line {} is not a record ({error})",
                at.display(),
                number + 1
            )),
        }
    }
    (chain, complaints)
}

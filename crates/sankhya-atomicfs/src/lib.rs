//! Making a file visible all at once, and claiming a name exclusively.
//!
//! # Why one helper and not four call sites
//!
//! Both operations here were already implemented, correctly, three times: the commit body's
//! staging, the checkpoint parquet's, and `diagnostic::history`'s. They were also implemented
//! wrongly four times --- `catalogue::save`, `_last_checkpoint`, the backup manifest, and the
//! commit's own version claim.
//!
//! Nobody chose to do it wrongly. The technique is three lines long, and three-line techniques
//! get retyped rather than reused, so the copies drift and no one place is obviously the rule.
//! Collecting it here is what lets `check-atomic-writes` say "this path, or explain yourself".
//!
//! # The two properties, which are not the same
//!
//! [`publish`] makes a file **visible all at once**, replacing whatever was there. That is what
//! an update wants: a cube definition, a checkpoint pointer, a manifest. Last writer wins, and
//! no reader ever sees a half-written file.
//!
//! [`claim`] makes a file visible all at once **and fails if the name is taken**. That is what
//! a commit wants, and the difference is the whole of the protocol's concurrency control. Using
//! `publish` where `claim` was meant silently loses the loser's work --- which is exactly the
//! defect this module was written for.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Distinguishes staging files written by this process from any other's.
static NEXT: AtomicU64 = AtomicU64::new(0);

/// A staging path beside `final_path` that no other writer can be using.
///
/// Beside it, not in a temporary directory: `rename` and `link` are only atomic within a
/// filesystem, and the only way to be sure of that is to stay in the same directory.
///
/// The name carries the process id and a per-process counter. A shared staging name is its own
/// defect --- two writers racing for one destination write the same temporary path, and either
/// can then publish the other's bytes --- and it is not hypothetical: the commit path had it.
fn staging_for(final_path: &Path) -> PathBuf {
    let unique = NEXT.fetch_add(1, Ordering::Relaxed);
    let name = final_path
        .file_name()
        .map_or_else(|| "unnamed".to_string(), |n| n.to_string_lossy().into_owned());
    final_path.with_file_name(format!(".{name}.{}.{unique}.tmp", std::process::id()))
}

/// Write `bytes` so that a reader sees all of them or none.
///
/// Replaces whatever is at `final_path`. For a name that must be claimed exclusively, use
/// [`claim`] instead --- this one cannot tell you that somebody else got there first.
///
/// # Errors
///
/// The underlying I/O error, with the path that failed named in it.
pub fn publish(final_path: &Path, bytes: &[u8]) -> io::Result<()> {
    let staging = staging_for(final_path);
    std::fs::write(&staging, bytes)?;
    match std::fs::rename(&staging, final_path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = std::fs::remove_file(&staging);
            Err(error)
        }
    }
}

/// Write `bytes` to a name only if nothing holds it, failing if something does.
///
/// Returns [`io::ErrorKind::AlreadyExists`] when the name is taken. That is not an error
/// condition to be logged and swallowed --- it is the answer, and the callers that matter
/// (`Publication::append_rebasing`, compaction's rebase) are built to receive it.
///
/// # Why `link` and not `create_new`
///
/// `File::create_new` claims the name atomically but then writes into it, so a reader that
/// opens between the claim and the last byte sees a partial file. Linking a fully written
/// staging file gets both properties at once: the bytes are complete before the name exists.
///
/// # Errors
///
/// [`io::ErrorKind::AlreadyExists`] if the name is taken; otherwise the underlying I/O error.
pub fn claim(final_path: &Path, bytes: &[u8]) -> io::Result<()> {
    let staging = staging_for(final_path);
    std::fs::write(&staging, bytes)?;
    let claimed = std::fs::hard_link(&staging, final_path);
    // Removed either way. After a successful link the bytes are reachable through
    // `final_path`, so the staging name is litter; after a failed one it is a body nobody
    // wants. Leaking it would put debris in directories that get replayed.
    let _ = std::fs::remove_file(&staging);
    claimed
}

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
//!
//! # Visibility is not durability, and this module used to deliver only the first
//!
//! `rename` is atomic with respect to other *readers*. It says nothing about power.
//!
//! Until 2026-09-03 nothing in this workspace called `fsync` --- `grep -rn "sync_all|sync_data"
//! crates/*/src` returned nothing at all --- so a `publish` that returned had put bytes in the
//! page cache and a name in a directory entry, both of which a crash discards. A commit could
//! be acknowledged and then, after power loss, be a log entry pointing at a Parquet file that
//! is short or zero length. `docs/INVARIANTS.md` read as a durability claim.
//!
//! Two syncs are needed and the second is the one that gets forgotten:
//!
//! - **The file**, before the rename. Otherwise the name is durable and the contents are not,
//!   which is the worst of the three outcomes: a file that exists, is the right size, and is
//!   full of nothing.
//! - **The directory**, after it. A rename is a directory modification, and an unsynced
//!   directory can lose the entry while the file's own blocks survive --- a complete file
//!   nothing refers to.
//!
//! This costs a real disk round trip per publish and it is not optional. Every guarantee this
//! system makes about a commit is a guarantee that the commit is still there afterwards.

mod exclusive;
pub mod name;

pub use exclusive::{Holder, NotLocked, WarehouseLock};

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
    write_durably(&staging, bytes)?;
    match std::fs::rename(&staging, final_path) {
        Ok(()) => sync_parent(final_path),
        Err(error) => {
            let _ = std::fs::remove_file(&staging);
            Err(error)
        }
    }
}

/// Write bytes and wait for the device to say it has them.
///
/// `std::fs::write` returns when the kernel has accepted the bytes, not when they are on the
/// medium. For a staging file that is about to be renamed into place, the difference is the
/// whole of durability: rename the name over unsynced contents and a crash leaves a file of
/// the right size holding whatever the block was before.
fn write_durably(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::File::create(path)?;
    file.write_all(bytes)?;
    // `sync_all` rather than `sync_data`: the length is metadata, and a file whose data is
    // durable while its length is not is a file that reads as empty.
    file.sync_all()
}

/// Make a directory's own modifications durable.
///
/// The step that is almost always missing. A `rename` or a `link` changes the *directory*, and
/// that change lives in the page cache like any other write until the directory itself is
/// synced. Without this, a crash can leave the file's blocks on disk and no name pointing at
/// them --- or, for `claim`, a commit that succeeded and then did not exist.
///
/// A failure to open the parent is not fatal: on a filesystem that does not permit opening a
/// directory this is unavailable rather than wrong, and the bytes are still synced. Reported
/// as success for that reason, and only for that reason.
fn sync_parent(path: &Path) -> io::Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    match std::fs::File::open(parent) {
        Ok(directory) => directory.sync_all(),
        Err(_) => Ok(()),
    }
}

/// Create a directory, and make the entry naming it durable.
///
/// # Why this is not `create_dir_all`
///
/// Creating a directory modifies its **parent**, and that modification lives in the page cache
/// like any other write. Every fsync in this workspace syncs the directory holding the file
/// just written; none synced the directory holding the newly created *directory*.
///
/// So a first commit into a new partition could be acknowledged and then, after power loss,
/// come back with the fully-synced Parquet and `_delta_log` inodes present and unreferenced ---
/// in `lost+found` --- because the table root's directory block never reached the medium.
/// Losing the partition entry leaves a commit pointing at a path that does not exist; losing
/// `_delta_log` leaves the table reading as absent.
///
/// Each level is synced as it is created, so a path several levels deep is durable throughout
/// rather than at its leaf. A parent that cannot be opened is not fatal, for the reason
/// [`sync_parent`] gives.
///
/// # Errors
///
/// The underlying I/O error from creating a directory.
pub fn create_dir_durably(path: &Path) -> io::Result<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            create_dir_durably(parent)?;
        }
    }
    match std::fs::create_dir(path) {
        Ok(()) => sync_parent(path),
        // Somebody else created it between the check and the call, which is the same outcome.
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
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
    write_durably(&staging, bytes)?;
    let claimed = std::fs::hard_link(&staging, final_path);
    // The directory entry the link created, made durable before the caller is told the name
    // is theirs. A commit that reports success and is not there after a power loss is worse
    // than one that reports failure.
    //
    // Held rather than returned with `?`. The `?` was here, before the `remove_file` below, so
    // a parent-directory sync that failed returned `Err` **after the hard link was already in
    // place** --- the caller was told its claim failed while the name was taken, and the
    // staging file leaked into a directory that gets replayed. The two are a bad pair: a
    // writer that believes it lost the race rebases, and `Publication::append_rebasing` gives
    // the rebase a new file name, so the same rows are committed twice under different paths
    // and replay dedups by path.
    let synced = if claimed.is_ok() { sync_parent(final_path) } else { Ok(()) };
    // Removed either way. After a successful link the bytes are reachable through
    // `final_path`, so the staging name is litter; after a failed one it is a body nobody
    // wants. Leaking it would put debris in directories that get replayed.
    let _ = std::fs::remove_file(&staging);
    // The claim first: it is what the caller acts on, and `AlreadyExists` must reach them
    // unchanged. A durability failure over a link that did happen is reported only when the
    // link itself succeeded, which is the only case where it means anything.
    claimed.and(synced)
}

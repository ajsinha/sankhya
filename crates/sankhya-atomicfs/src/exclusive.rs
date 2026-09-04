//! One writer per warehouse, enforced rather than assumed.
//!
//! # What the commit protocol does not cover
//!
//! [`claim`](crate::claim) serialises two committers **at a version**: one of them gets the
//! name and the other is refused and rebases. That is the whole of the Delta protocol's
//! concurrency control and it is correct, but its scope is a single log entry.
//!
//! Everything outside a log entry is unprotected. Two servers on one warehouse each run their
//! own maintenance: each plans compactions against a live set the other is changing, each
//! retires inputs against its own lease registry --- which cannot see the other's readers ---
//! and each sweeps orphans against an age threshold that has no idea a file belongs to a
//! commit the other process has not written yet. None of that races at a version, so none of
//! it is caught. The audit recorded it as `COR-15`, and the two name-reuse defects it lists
//! beside it (`COR-02`, `COR-06`) are what leaks through the gap.
//!
//! # Why a lock file and not an advisory lock
//!
//! `flock(2)` is the better primitive and this workspace cannot reach it: `unsafe_code` is
//! `forbid` at the workspace root, `libc` is confined to `sankhya-sandbox` by `check-layers`,
//! and adding a locking crate here would give the one dependency-free crate a dependency.
//!
//! So the lock is a file, claimed with the same exclusive-create this module already provides,
//! naming the process that holds it. What a file cannot do by itself is notice that its holder
//! died --- a crash leaves the name behind, and a lock that refuses for ever after one crash is
//! a lock an operator learns to delete on sight, which is no lock at all.
//!
//! Liveness is therefore established from `/proc`, and established **exactly**:
//!
//! - The holder's pid has no `/proc` entry --- it is gone, the lock is stale, and it is taken.
//! - The pid exists but its start time differs from the one recorded --- the pid was reused by
//!   an unrelated process, the holder is gone, and the lock is taken.
//! - The pid exists with the recorded start time --- the holder is **running**, and startup is
//!   refused naming it.
//! - The file cannot be parsed, or this is not Linux and liveness cannot be established at all
//!   --- startup is refused, and the operator is told to remove the file.
//!
//! The last case is deliberately the unhelpful one. Every automatic way out of it ends in two
//! servers on one warehouse, which is the failure being prevented.

use std::fmt;
use std::path::{Path, PathBuf};

/// The process a warehouse lock names.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Holder {
    /// The process id recorded in the lock.
    pub pid: u32,
    /// That process's start time, in clock ticks since boot, as `/proc/<pid>/stat` reports it.
    ///
    /// Recorded because a pid alone cannot answer *"is that still the same process?"* --- pids
    /// are reused, and a lock broken on a reused pid is two servers on one warehouse.
    pub started: u64,
}

/// Why a warehouse could not be locked.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum NotLocked {
    /// Another process holds it, and that process is running.
    Held {
        /// Where the lock file is.
        file: PathBuf,
        /// Who holds it.
        holder: Holder,
    },
    /// A lock file exists that cannot be read or understood, so its holder cannot be checked.
    ///
    /// Refused rather than broken. A lock file this process cannot interpret may name a
    /// running server, and guessing is how the guarantee is lost.
    Unreadable {
        /// Where the lock file is.
        file: PathBuf,
        /// What went wrong.
        detail: String,
    },
    /// The lock could not be written --- a missing directory, no permission, a full disk.
    Failed {
        /// Where the lock file would be.
        file: PathBuf,
        /// What went wrong.
        detail: String,
    },
}

impl fmt::Display for NotLocked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Held { file, holder } => write!(
                f,
                "this warehouse is already served by process {} --- {} says so, and that \
                 process is running. Two servers on one warehouse corrupt it: each retires \
                 files against readers the other cannot see. Stop that server first.",
                holder.pid,
                file.display()
            ),
            Self::Unreadable { file, detail } => write!(
                f,
                "the warehouse lock {} cannot be read, so whether a server still holds it \
                 cannot be established ({detail}). Confirm no SANKHYA process is serving this \
                 warehouse and remove that file.",
                file.display()
            ),
            Self::Failed { file, detail } => {
                write!(f, "the warehouse lock {} could not be taken: {detail}", file.display())
            }
        }
    }
}

impl std::error::Error for NotLocked {}

/// An exclusive hold on one warehouse, released when this value is dropped.
#[derive(Debug)]
pub struct WarehouseLock {
    file: PathBuf,
    holder: Holder,
}

impl WarehouseLock {
    /// Take the lock at `file`, or say who has it.
    ///
    /// # Errors
    ///
    /// [`NotLocked::Held`] when a running process holds it, [`NotLocked::Unreadable`] when an
    /// existing lock cannot be interpreted, and [`NotLocked::Failed`] when the file could not
    /// be written.
    pub fn take(file: &Path) -> Result<Self, NotLocked> {
        let holder = Self::me(file)?;

        // Bounded, because each pass either takes the lock or names a live holder. The loop
        // exists for one case only: this process finds a stale lock, removes it, and another
        // process claims the freed name first. Then the second pass reads *that* process's
        // entry, finds it alive, and refuses --- which is the right answer.
        for _ in 0..3 {
            match Self::claim_file(file, &holder) {
                Ok(()) => return Ok(Self { file: file.to_path_buf(), holder }),
                Err(TakeFailure::Taken) => {}
                Err(TakeFailure::Io(detail)) => {
                    return Err(NotLocked::Failed { file: file.to_path_buf(), detail })
                }
            }

            let existing = Self::read(file)?;
            if alive(&existing) {
                return Err(NotLocked::Held { file: file.to_path_buf(), holder: existing });
            }
            // Its holder is provably gone. Removing is safe in a way that overwriting is not:
            // if another process claims the name between the removal and the next attempt,
            // the next pass sees a live holder and refuses instead of replacing it.
            if let Err(error) = std::fs::remove_file(file) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    return Err(NotLocked::Failed {
                        file: file.to_path_buf(),
                        detail: error.to_string(),
                    });
                }
            }
        }

        Err(NotLocked::Failed {
            file: file.to_path_buf(),
            detail: "the lock changed hands on every attempt".to_string(),
        })
    }

    /// The process this lock names, which is this one.
    #[must_use]
    pub fn holder(&self) -> &Holder {
        &self.holder
    }

    /// Where the lock file is.
    #[must_use]
    pub fn file(&self) -> &Path {
        &self.file
    }

    /// Give the lock up early. Dropping does the same thing.
    pub fn release(self) {
        drop(self);
    }

    fn me(file: &Path) -> Result<Holder, NotLocked> {
        let pid = std::process::id();
        let started = start_time(pid).ok_or_else(|| NotLocked::Unreadable {
            file: file.to_path_buf(),
            detail: "this platform does not expose process start times, so a stale lock \
                     cannot be told from a live one"
                .to_string(),
        })?;
        Ok(Holder { pid, started })
    }

    fn claim_file(file: &Path, holder: &Holder) -> Result<(), TakeFailure> {
        let body = format!("sankhya-warehouse-lock 1\npid {}\nstarted {}\n", holder.pid, holder.started);
        // The same exclusive create `claim` is built on, and for the same reason: the name is
        // the thing being claimed, and a claim that replaces is not a claim.
        match std::fs::File::create_new(file) {
            Ok(mut handle) => {
                use std::io::Write;
                handle
                    .write_all(body.as_bytes())
                    .and_then(|()| handle.sync_all())
                    .map_err(|error| TakeFailure::Io(error.to_string()))?;
                crate::sync_parent(file).map_err(|error| TakeFailure::Io(error.to_string()))
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(TakeFailure::Taken)
            }
            Err(error) => Err(TakeFailure::Io(error.to_string())),
        }
    }

    fn read(file: &Path) -> Result<Holder, NotLocked> {
        let text = std::fs::read_to_string(file).map_err(|error| NotLocked::Unreadable {
            file: file.to_path_buf(),
            detail: error.to_string(),
        })?;
        parse(&text).ok_or_else(|| NotLocked::Unreadable {
            file: file.to_path_buf(),
            detail: "it does not look like a warehouse lock".to_string(),
        })
    }
}

impl Drop for WarehouseLock {
    fn drop(&mut self) {
        // Only if it is still ours. An operator who removed the file by hand and started a
        // second server would otherwise have this process delete the second server's lock on
        // the way out --- turning one mistake into an unprotected warehouse.
        if let Ok(holder) = Self::read(&self.file) {
            if holder == self.holder {
                let _ = std::fs::remove_file(&self.file);
            }
        }
    }
}

enum TakeFailure {
    Taken,
    Io(String),
}

fn parse(text: &str) -> Option<Holder> {
    let mut pid = None;
    let mut started = None;
    for line in text.lines() {
        let (key, value) = line.split_once(' ')?;
        match key {
            "sankhya-warehouse-lock" => {
                if value.trim() != "1" {
                    return None;
                }
            }
            "pid" => pid = value.trim().parse().ok(),
            "started" => started = value.trim().parse().ok(),
            _ => return None,
        }
    }
    Some(Holder { pid: pid?, started: started? })
}

/// Whether the process a lock names is still the process that took it.
///
/// False for a pid with no process and for a pid that has been **reused**, which is the case a
/// pid check alone gets wrong. It is only ever consulted after an exclusive create has already
/// failed, so a wrong `true` costs a refused startup and a wrong `false` costs the guarantee.
fn alive(holder: &Holder) -> bool {
    start_time(holder.pid).is_some_and(|started| started == holder.started)
}

/// A process's start time in clock ticks since boot, from `/proc/<pid>/stat`.
///
/// `None` when there is no such process, and on any platform without `/proc` --- where the
/// caller refuses rather than guesses.
#[cfg(target_os = "linux")]
fn start_time(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // `comm` is the second field, it is parenthesised, and it may contain both spaces and
    // close parentheses --- `(my )prog)` is a legal name. Splitting on whitespace from the
    // left is therefore wrong; every parser that does it is wrong for one process name.
    // Everything after the *last* close parenthesis is unambiguous.
    let rest = stat.rsplit_once(')')?.1;
    // Fields resume at 3 (`state`), so field 22 (`starttime`) is index 19 here.
    rest.split_whitespace().nth(19)?.parse().ok()
}

#[cfg(not(target_os = "linux"))]
fn start_time(_pid: u32) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

    use super::*;

    /// Put a lock file there that this module did not write.
    ///
    /// Not `fs::write`. `check-atomic-writes` forbids it across the workspace, and this crate
    /// is the helper that exists so nobody needs it --- an exemption for its own tests would
    /// be the first of the exemptions the rule was written to stop.
    fn put(file: &Path, body: &str) {
        use std::io::Write;
        let mut handle = std::fs::File::create_new(file).expect("a lock file that is not there");
        handle.write_all(body.as_bytes()).expect("writing it");
    }

    #[test]
    fn a_second_server_on_one_warehouse_is_refused() {
        let home = tempfile::tempdir().expect("tempdir");
        let file = home.path().join("warehouse.lock");

        let first = WarehouseLock::take(&file).expect("the first server takes the lock");
        let second = WarehouseLock::take(&file);

        match second {
            Err(NotLocked::Held { holder, .. }) => {
                assert_eq!(holder, *first.holder(), "it named a holder that is not the holder");
            }
            other => panic!("a second server was allowed onto the warehouse --- {other:?}"),
        }
    }

    #[test]
    fn the_lock_is_released_when_the_server_stops() {
        let home = tempfile::tempdir().expect("tempdir");
        let file = home.path().join("warehouse.lock");

        drop(WarehouseLock::take(&file).expect("the first server takes the lock"));
        assert!(!file.exists(), "the lock file outlived the process that held it");
        WarehouseLock::take(&file).expect("a restart could not retake its own warehouse");
    }

    #[test]
    fn a_lock_left_by_a_crash_is_taken_rather_than_obeyed_for_ever() {
        let home = tempfile::tempdir().expect("tempdir");
        let file = home.path().join("warehouse.lock");

        // A crashed server: the file is there, and nothing is running behind it. Pid 1 exists
        // on every Linux system, so the *pid* test alone would call this live --- the recorded
        // start time is what distinguishes a reused pid from the process that took the lock.
        put(&file, "sankhya-warehouse-lock 1\npid 1\nstarted 999999999999\n");

        WarehouseLock::take(&file).expect("a crashed server locked the warehouse for ever");
    }

    #[test]
    fn a_lock_that_cannot_be_understood_is_refused_rather_than_broken() {
        let home = tempfile::tempdir().expect("tempdir");
        let file = home.path().join("warehouse.lock");
        put(&file, "not a lock file at all\n");

        match WarehouseLock::take(&file) {
            Err(NotLocked::Unreadable { .. }) => {}
            other => panic!("an unreadable lock was broken instead of refused --- {other:?}"),
        }
    }

    #[test]
    fn a_holder_is_read_back_as_it_was_written() {
        let holder = Holder { pid: 4242, started: 8_837_412 };
        let text = format!(
            "sankhya-warehouse-lock 1\npid {}\nstarted {}\n",
            holder.pid, holder.started
        );
        assert_eq!(parse(&text), Some(holder));
    }

    #[test]
    fn this_process_is_alive_and_a_reused_pid_is_not() {
        let pid = std::process::id();
        let started = start_time(pid).expect("this process has a start time");
        assert!(alive(&Holder { pid, started }));
        assert!(
            !alive(&Holder { pid, started: started.wrapping_add(1) }),
            "a pid whose start time differs was called the same process"
        );
    }
}

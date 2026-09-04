//! Every writer that a commit can point at waits for the medium.
//!
//! # Why this is a source check and not a test
//!
//! `fsync` cannot be observed from inside the process that calls it. A test can prove that a
//! reader never sees a torn write --- `sankhya-atomicfs/tests/races.rs` does --- but nothing
//! short of cutting the power distinguishes bytes that reached the disk from bytes the kernel
//! has merely accepted. Three mutations were written for these calls and all three survived,
//! correctly: there is no test that could have caught them.
//!
//! `check-atomic-writes` already takes this shape for the same reason, and for the same kind
//! of property. This is its durability half.
//!
//! # What it is checking
//!
//! Until 2026-09-03 `grep -rn "sync_all\|sync_data" crates/*/src` returned **nothing**. Every
//! publish put bytes in the page cache and a name in a directory entry, and a crash discards
//! both --- so a commit could be acknowledged and then, after power loss, be a log entry
//! pointing at a Parquet file that is short, zero length, or full of whatever those blocks
//! held before. `docs/INVARIANTS.md` read as a durability claim.
//!
//! Two syncs are needed per published file and the second is the one that gets forgotten: the
//! **file** before the rename, and the **directory** after it.

use std::path::Path;

/// The writers a commit can end up pointing at, and what each must call.
///
/// Named individually rather than discovered, because "every file write must fsync" is false
/// --- a scratch file, a log line and a test fixture must not pay for a disk round trip. What
/// must be durable is what a commit refers to, and that is a short list somebody has to keep.
const MUST_SYNC: &[(&str, &str)] = &[
    (
        "crates/sankhya-atomicfs/src/lib.rs",
        "the helper 299 source files publish through",
    ),
    (
        "crates/sankhya-table/src/write.rs",
        "the Parquet a commit references",
    ),
    (
        "crates/sankhya-table-delta/src/checkpoint.rs",
        "the checkpoint readers use instead of replaying the log",
    ),
];

/// Every durable writer syncs its file and its directory.
#[must_use]
pub fn check(root: &Path) -> bool {
    println!("== check-durability ==");
    let mut ok = true;
    let mut checked = 0usize;

    for (relative, what) in MUST_SYNC {
        let Ok(text) = std::fs::read_to_string(root.join(relative)) else {
            eprintln!("  MISSING  {relative} is named as a durable writer and is not there");
            ok = false;
            continue;
        };
        checked += 1;

        // The file's own bytes.
        if !text.contains("sync_all()") && !text.contains("sync_data()") {
            eprintln!(
                "  NOT DURABLE  {relative} ({what}) never waits for the medium. A rename over \
                 unsynced bytes leaves a file that exists, is the right length, and holds what \
                 those blocks held before"
            );
            ok = false;
        }

        // The directory entry, which is a separate write and survives separately.
        let syncs_a_directory = text.contains("File::open(parent)")
            || text.contains("File::open(directory)")
            || text.contains("sync_parent");
        if !syncs_a_directory {
            eprintln!(
                "  NO DIRECTORY SYNC  {relative} ({what}) syncs its bytes and not the directory \
                 entry that names them. A crash can leave the blocks and lose the name --- a \
                 complete file nothing refers to, or a commit that was acknowledged and is gone"
            );
            ok = false;
        }
    }

    if ok {
        println!("   {checked} durable writer(s) sync both their bytes and their directory");
    }
    ok
}

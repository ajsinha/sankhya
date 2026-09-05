//! What the build tree costs, and collecting what it no longer needs.
//!
//! Cargo names every artefact by a hash of its inputs and never removes the one a rebuild
//! supersedes. Nothing collects them, nothing reports them, and `target/` reached 482 GB
//! here --- 13,877 files in `deps` alone, most unreachable by any build --- taking the disk
//! to 95% full. That is how a forty-five-minute soak came to die at t+2833s and write a
//! zero-byte report explaining why: it ran out of room to say what had gone wrong.
//!
//! Two halves, deliberately separate. [`check`] says the number out loud on every run and
//! fails when the tree has taken the machine hostage; [`sweep`] collects the superseded
//! generations. A check that silently deleted a developer's build cache would be making a
//! decision that is not a check's to make.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The ceiling bounds cognitive load. It is not satisfied structurally: a split that
/// widens visibility or separates an invariant from its enforcement is a violation of
/// this rule, not compliance with it, and must be rejected in review.
/// How many generations of an artefact are worth keeping.
///
/// One is the build you just did. The second exists so that flipping between two branches,
/// or between `--all-features` and not, does not relink the world every time. A third buys
/// very little and costs another whole copy of every test binary in the workspace.
const SWEEP_KEEP: usize = 2;


/// Delete the build artefacts that no longer belong to any recent build.
///
/// # The problem this exists for
///
/// Cargo names every artefact by a hash of its inputs, and it never removes the artefact
/// that a rebuild superseded. Edit a crate that everything depends on, and the workspace's
/// test binaries --- around 700 MB each here, because they statically link arrow, parquet
/// and delta-kernel --- are written again under a new hash while the old ones stay. Nothing
/// in cargo collects them. Three days of ordinary work grew `target/` to 482 GB and took
/// the disk to 95% full, which is how a forty-five-minute soak came to die at t+2833s and
/// write a zero-byte report explaining why.
///
/// # What it keeps, and why that is safe
///
/// Artefacts are grouped by name, and the newest [`SWEEP_KEEP`] hashes of each are kept.
/// The grouping is a heuristic, so it can in principle delete something the next build
/// wants --- and the cost of being wrong is that cargo rebuilds it. That is a minute, set
/// against a build tree that otherwise grows without bound. Being slightly too eager here
/// is the cheap direction to be wrong in.
pub fn sweep(root: &Path, dry_run: bool) -> bool {
    println!("== sweep ==");
    let target = root.join("target");
    if !target.exists() {
        println!("   no build tree, so nothing to sweep");
        return true;
    }
    let (before, _) = tree_size(&target);
    let mut removed_files = 0_u64;
    let mut removed_bytes = 0_u64;
    // `lints/` too: clippy builds into a directory of its own so that it and `cargo test`
    // stop invalidating each other, and a directory nobody sweeps is a directory that grows
    // until it takes the machine hostage --- which is what this whole file exists to stop.
    for root in [target.clone(), target.join("lints")] {
        for profile in ["debug", "release"] {
            for dir in ["deps", "examples"] {
                let at = root.join(profile).join(dir);
                if !at.exists() {
                    continue;
                }
                let (f, b) = sweep_dir(&at, dry_run);
                removed_files += f;
                removed_bytes += b;
            }
        }
    }
    let gb = removed_bytes as f64 / 1024.0 / 1024.0 / 1024.0;
    let left = (before - removed_bytes.min(before)) as f64 / 1024.0 / 1024.0 / 1024.0;
    if dry_run {
        println!("   would remove {removed_files} superseded file(s), {gb:.1} GB");
    } else {
        println!("   removed {removed_files} superseded file(s), {gb:.1} GB");
    }
    println!("   target/ now {left:.1} GB, keeping {SWEEP_KEEP} generation(s) of each artefact");
    true
}


/// Sweep one artefact directory, keeping the newest generations of each name.
fn sweep_dir(at: &Path, dry_run: bool) -> (u64, u64) {
    // Grouped by what the artefact *is*, across the hashes that are versions of it:
    // `sankhya_server-9e7713a1.d` and `sankhya_server-b5fac89b.d` are one group, and
    // `libsankhya_config-c0ffee.rlib` is another. The hash is the generation; everything
    // either side of it is the identity.
    let mut groups: BTreeMap<(String, String), Vec<(std::time::SystemTime, PathBuf, u64)>> =
        BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(at) else {
        return (0, 0);
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some((stem, ext)) = split_artefact(name) else {
            continue;
        };
        let when = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
        groups
            .entry((stem, ext))
            .or_default()
            .push((when, path, meta.len()));
    }
    let mut files = 0_u64;
    let mut bytes = 0_u64;
    for (_, mut generation) in groups {
        if generation.len() <= SWEEP_KEEP {
            continue;
        }
        // Newest first, so the tail is what the newest builds have already replaced.
        generation.sort_by(|a, b| b.0.cmp(&a.0));
        for (_, path, len) in generation.into_iter().skip(SWEEP_KEEP) {
            if dry_run || std::fs::remove_file(&path).is_ok() {
                files += 1;
                bytes += len;
            }
        }
    }
    (files, bytes)
}


/// Split an artefact filename into what it is and the extension, discarding the hash.
///
/// Returns `None` for anything not carrying a cargo metadata hash, which is left alone:
/// a file this cannot parse is a file it has no business deleting.
fn split_artefact(name: &str) -> Option<(String, String)> {
    let (before_ext, ext) = match name.find('.') {
        Some(i) => (&name[..i], name[i..].to_string()),
        None => (name, String::new()),
    };
    let cut = before_ext.rfind('-')?;
    let hash = &before_ext[cut + 1..];
    let hashish = hash.len() >= 8 && hash.chars().all(|c| c.is_ascii_hexdigit());
    if !hashish {
        return None;
    }
    Some((before_ext[..cut].to_string(), ext))
}


/// When the build tree stops being a cache and starts being a problem.
///
/// # Why there is a number here at all
///
/// `target/` reached 482 GB and took the disk to 95% full. That is not a build; that is
/// three days of builds, none of which cleaned up after the one before. Cargo names each
/// artefact by a hash of its inputs and never collects the old ones, so every edit to a
/// widely-depended-on crate leaves another ~700 MB test binary behind forever. There were
/// 13,877 files in `deps` and the great majority could not be reached by any build.
///
/// Nothing reported this. It was found because a forty-five-minute soak died at t+2833s
/// with a full disk, and the report it tried to write to explain itself is zero bytes ---
/// the same failure this project already fixed once, in the soak, and had in its own
/// build tree the whole time.
const BUILD_TREE_WARN: u64 = 50 * 1024 * 1024 * 1024;

/// The size at which a build tree has taken the machine hostage.
const BUILD_TREE_HARD: u64 = 150 * 1024 * 1024 * 1024;


/// Report what the build tree is consuming, and fail when it is consuming the machine.
///
/// A guard rather than a cleanup: deleting a developer's build cache is not a check's
/// decision to make, and `cargo clean` is one command. What was missing was anybody saying
/// the number out loud before the disk said it instead.
pub fn check(root: &Path) -> bool {
    println!("== check-build-tree ==");
    let target = root.join("target");
    if !target.exists() {
        println!("   no build tree yet, so nothing to report");
        return true;
    }
    let (bytes, files) = tree_size(&target);
    let gb = bytes as f64 / 1024.0 / 1024.0 / 1024.0;
    println!("   target/ holds {gb:.1} GB across {files} file(s)");
    if bytes > BUILD_TREE_HARD {
        let hard = BUILD_TREE_HARD as f64 / 1024.0 / 1024.0 / 1024.0;
        eprintln!("  TOO LARGE    target/ is {gb:.1} GB against a limit of {hard:.0} GB.");
        eprintln!("               Cargo never collects the artefacts a rebuild supersedes, so");
        eprintln!("               this grows without bound until something on this machine");
        eprintln!("               runs out of disk --- and the thing that runs out is usually");
        eprintln!("               not the build. Run `cargo clean`.");
        return false;
    }
    if bytes > BUILD_TREE_WARN {
        let warn = BUILD_TREE_WARN as f64 / 1024.0 / 1024.0 / 1024.0;
        println!("  approaching  {gb:.1} GB, warning at {warn:.0} GB; `cargo clean` reclaims it");
    }
    true
}


/// Bytes and file count under a directory, following no symlinks.
fn tree_size(dir: &Path) -> (u64, u64) {
    let mut bytes = 0_u64;
    let mut files = 0_u64;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (bytes, files);
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            let (b, f) = tree_size(&entry.path());
            bytes += b;
            files += f;
        } else if meta.is_file() {
            bytes += meta.len();
            files += 1;
        }
    }
    (bytes, files)
}


#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::{split_artefact, sweep_dir};
    use std::path::Path;

    /// The hash is the generation; everything either side of it is the identity.
    #[test]
    fn artefacts_of_one_crate_are_recognised_as_generations_of_the_same_thing() {
        let first = split_artefact("sankhya_server-9e7713a1be4acc1d").expect("a hashed binary");
        let second = split_artefact("sankhya_server-b5fac89b087a095c").expect("a hashed binary");
        assert_eq!(first, second, "two builds of one test binary must group together, or the sweep keeps every generation of everything and reclaims nothing");
        let rlib = split_artefact("libsankhya_config-c0ffeec0ffeec0ff.rlib").expect("an rlib");
        assert_eq!(rlib, ("libsankhya_config".to_string(), ".rlib".to_string()));
    }


    /// A file whose name it cannot parse is a file it has no business deleting.
    #[test]
    fn a_file_without_a_metadata_hash_is_left_alone() {
        assert!(split_artefact("sankhya-server.d").is_none(), "`sankhya-server.d` has no hash, so it is a current output rather than a superseded generation");
        assert!(split_artefact("CACHEDIR.TAG").is_none());
        assert!(split_artefact("build").is_none());
        assert!(split_artefact("libfoo-notahexhash.rlib").is_none(), "the segment after the last dash must actually be a hash, or ordinary dashed names get swept");
    }


    /// The newest generations survive and the superseded ones go.
    ///
    /// Written because the sweep deletes files, and the only thing worse than a build tree
    /// that grows without bound is a cleanup that removes the build you are standing on.
    #[test]
    fn a_sweep_keeps_the_newest_generations_and_removes_what_they_replaced() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let deps = dir.path();
        // Four generations of one binary, oldest first, with distinct modification times.
        let mut made = Vec::new();
        for (n, hash) in ["aaaaaaaaaaaaaaaa", "bbbbbbbbbbbbbbbb", "cccccccccccccccc", "dddddddddddddddd"].iter().enumerate() {
            let path = deps.join(format!("soak-{hash}"));
            std::fs::write(&path, vec![b'x'; 1024]).expect("a written artefact");
            let when = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000 + n as u64 * 60);
            filetime_set(&path, when);
            made.push(path);
        }
        // And something the sweep must not touch.
        let keep = deps.join("soak.d");
        std::fs::write(&keep, b"current").expect("a written output");

        let (files, bytes) = sweep_dir(deps, false);
        assert_eq!(files, 2, "four generations with two kept leaves two to remove");
        assert_eq!(bytes, 2048);
        assert!(!made[0].exists(), "the oldest generation must be gone");
        assert!(!made[1].exists());
        assert!(made[2].exists(), "the second-newest is kept so branch switching does not relink the world");
        assert!(made[3].exists(), "the newest generation is the build you are standing on and must survive");
        assert!(keep.exists(), "an unhashed file is a current output, not a superseded generation");
    }


    /// A dry run reports exactly what a real run would remove, and removes none of it.
    #[test]
    fn a_dry_run_removes_nothing() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let deps = dir.path();
        for hash in ["aaaaaaaaaaaaaaaa", "bbbbbbbbbbbbbbbb", "cccccccccccccccc"] {
            std::fs::write(deps.join(format!("soak-{hash}")), vec![b'x'; 512]).expect("an artefact");
        }
        let (files, _) = sweep_dir(deps, true);
        assert_eq!(files, 1, "three generations with two kept leaves one");
        assert_eq!(std::fs::read_dir(deps).expect("a readable directory").count(), 3, "a dry run that deletes something is not a dry run");
    }


    /// Set a file's modification time, so generation order is stated rather than raced for.
    fn filetime_set(path: &Path, when: std::time::SystemTime) {
        let file = std::fs::OpenOptions::new().write(true).open(path).expect("an openable artefact");
        file.set_modified(when).expect("a settable modification time");
    }

}

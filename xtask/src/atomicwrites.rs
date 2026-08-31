use std::collections::BTreeSet;
use std::path::Path;

/// Files allowed to write to a live path, with the reason.
///
/// Two kinds of entry, and they are not the same. The publishing helper *is* the
/// implementation, so it must be able to do the thing it exists to encapsulate. Everything
/// else here is a path a reader can never name --- a scratch file, a report beside a log ---
/// and each says why.
const MAY_WRITE_DIRECTLY: &[(&str, &str)] = &[
    (
        "crates/sankhya-atomicfs/src/lib.rs",
        "the publishing helper itself: this is the one place staging, linking and renaming are \
         implemented, and it cannot route through itself",
    ),
    (
        "crates/sankhya-table/src/write.rs",
        "a Parquet data file, which no reader can name until the commit that adds it lands. \
         Safety here comes from the *ordering* rather than from the write being atomic: the \
         path appears in no log until the file is closed, and a partial file left by a crash \
         is unreferenced and collected by the orphan sweep. Routing it through `publish` would \
         also mean buffering a whole Parquet file in memory, since `ArrowWriter` streams into \
         a handle rather than producing bytes",
    ),
    (
        "crates/sankhya-backup/src/attest.rs",
        "the attestation drill, whose whole job is to attempt the writes a write-once store \
         must refuse. Routing them through `publish` would test the wrong thing entirely: \
         `publish` stages and renames, and a store that refuses an in-place overwrite may \
         well permit a rename --- so an attestation built on it could report a control in \
         force that is not. The probe object is named `_attestation_probe` and no reader \
         names it",
    ),
];

/// The shapes that are refused, and what to do instead.
const REFUSED: &[(&str, &str)] = &[
    (
        "std::fs::write(",
        "publishes bytes onto whatever path it is given. A reader opening that path mid-write \
         sees a truncated file. Use `atomic::publish`, or `atomic::claim` when the name must \
         be claimed exclusively",
    ),
    (
        "File::create(",
        "creates and then writes, so the name exists before the bytes do. Use \
         `atomic::publish`",
    ),
];

/// No writer may make a file visible by writing to the path a reader will open.
///
/// # Why a gate and not a convention
///
/// This exact technique was already implemented correctly three times in this repository --- the
/// commit body's staging, the checkpoint parquet's, and `diagnostic::history`'s --- and
/// incorrectly four: `catalogue::save`, `_last_checkpoint`, the backup manifest, and the commit's
/// own version claim.
///
/// Nobody chose the wrong one. The technique is three lines long, and three-line techniques get
/// retyped rather than reused, so the copies drift and no single place is obviously the rule.
/// A convention that holds in three places and lapses in four is not a convention; it is an
/// average.
///
/// # What this cannot catch
///
/// A write through a helper of somebody's own making, or through a crate this scan does not
/// read. The gate narrows the ways to get it wrong; it does not make them impossible, and
/// saying so is better than implying a guarantee it cannot give.
pub(crate) fn check(root: &Path) -> bool {
    println!("== check-atomic-writes ==");
    let mut files = Vec::new();
    crate::rust_files(&root.join("crates"), &mut files);

    let mut ok = true;
    let mut checked = 0usize;
    let mut excused: BTreeSet<String> = BTreeSet::new();

    for file in &files {
        let rel = file.strip_prefix(root).unwrap_or(file).display().to_string();
        // Tests may write freely: a test writes fixtures and injects damage, and neither is a
        // publication a reader of this system will ever open.
        if rel.contains("/tests/") || rel.contains("/benches/") {
            continue;
        }
        if let Some((path, why)) = MAY_WRITE_DIRECTLY.iter().find(|(p, _)| rel.ends_with(p)) {
            assert!(why.len() > 30, "{path} is excused without a usable reason");
            excused.insert(rel);
            continue;
        }
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        for (shape, why) in REFUSED {
            for (number, line) in text.lines().enumerate() {
                if !line.contains(shape) {
                    continue;
                }
                // A staging write is the first half of the correct pattern, not a violation.
                if line.contains("staging") || line.contains("temporary") || line.contains(".tmp")
                {
                    continue;
                }
                eprintln!("  DIRECT WRITE   {rel}:{}: `{shape}` {why}", number + 1);
                ok = false;
            }
        }
        checked += 1;
    }

    // An excuse for a file that no longer writes directly must be removed, or the list only
    // ever grows and stops describing anything.
    for (path, _) in MAY_WRITE_DIRECTLY {
        if !excused.iter().any(|seen| seen.ends_with(path)) {
            eprintln!("  STALE EXCUSE   {path} is allowed to write directly and does not exist");
            ok = false;
        }
    }

    println!("   {checked} source file(s) publish through the helper, {} excused", excused.len());
    ok
}

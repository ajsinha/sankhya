//! `M10`'s soak: a clone under maintenance, and whether it still reads what it could.
//!
//! # A different question from the other soak
//!
//! `soak_run` asks *"does any bounded measure grow without bound?"* over forty-five minutes. This
//! asks a correctness question that a long run is not needed to answer and a **hostile** one is:
//! after the origin has been compacted, retired and swept, can the clone still read every row it
//! could read before?
//!
//! That is `M10`'s exit criterion, and the failure it guards against has no symptom at the time.
//! From the origin's point of view a file only the clone still names is on disk, absent from the
//! live set and past its threshold --- indistinguishable from debris. Nothing errors. The clone
//! is simply missing rows the next time somebody reads that range of it, possibly months later.
//!
//! # Why it runs every time rather than deliberately
//!
//! The other soak is `#[ignore]`d because forty-five minutes is not something a suite can spend.
//! This one is seconds, and a correctness property that runs only when somebody remembers is a
//! correctness property nobody is checking. `SANKHYA_CLONE_SOAK_ROUNDS` makes it longer for
//! anybody who wants a harder version of the same question.
//!
//! # Everything goes through the product's own paths
//!
//! Writes through `Publication`, maintenance through `Maintainer`, reads through
//! `resolve_clone_cached`. A soak that writes through its own code measures its own code --- the
//! lesson `soak_run`'s header records, learned when a fixture went on encoding a layout the
//! product had stopped producing.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::print_stdout
)]

use arrow_array::{Int64Array, RecordBatch};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use sankhya_clone::{Lineage, Lineages};
use sankhya_maintenance::{Maintainer, MaintenancePolicy, OrphanPolicy, RetentionPolicy};
use sankhya_publish::Publication;
use sankhya_readpath::{resolve_clone_cached, Inherited};
use sankhya_table_delta::LogCache;
use sankhya_types::{Lsn, LsnRange};
use std::collections::BTreeSet;
use std::sync::Arc;

/// Rows per published file. Small, because the question is about file *lifetime* rather than
/// about volume, and a hundred files of ten rows exercises compaction harder than ten of a
/// hundred.
const ROWS: i64 = 10;

fn schema() -> SchemaRef {
    Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]))
}

fn batch(from: i64) -> RecordBatch {
    let ids: Vec<i64> = (from..from + ROWS).collect();
    RecordBatch::try_new(schema(), vec![Arc::new(Int64Array::from(ids))]).expect("a batch")
}

/// Maintenance that does everything, every tick, with nothing held back.
///
/// Deliberately hostile. The shipping defaults compact every tick, sweep every hundred and
/// twenty, and keep an input for a week --- which on a test's timescale means retirement and
/// orphan collection never happen and the soak proves nothing about either. Zeroed here so both
/// run on every tick, which is the condition the clone has to survive.
fn relentless() -> MaintenancePolicy {
    MaintenancePolicy {
        compact_every: 1,
        orphan_sweep_every: 1,
        orphans: OrphanPolicy { min_age_ticks: 0 },
        retention: RetentionPolicy { grace_ticks: 0, verify_replacement: true },
        ..MaintenancePolicy::default()
    }
}

/// Every `id` a table's plan can actually read, by opening every file the plan names.
///
/// Rows rather than counts, and read from the files rather than from the log. A row count comes
/// from the log, so a plan naming files that are not there reports the right number --- which is
/// exactly the failure this soak exists to catch, and it would pass a count.
fn ids_readable(root: &std::path::Path, inherited: &Inherited) -> BTreeSet<i64> {
    let cache = LogCache::new();
    let table = resolve_clone_cached(
        schema(),
        root,
        inherited,
        Some(LsnRange::up_to(Lsn::new(u64::MAX))),
        Lsn::new(u64::MAX),
        &cache,
    )
    .expect("the clone resolves");

    let mut ids = BTreeSet::new();
    for file in table.published_files() {
        let handle = std::fs::File::open(&file.path)
            .unwrap_or_else(|error| panic!("the plan named `{}`: {error}", file.path));
        let reader = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(handle)
            .expect("a parquet file")
            .build()
            .expect("a reader");
        for batch in reader {
            let batch = batch.expect("a batch");
            let column = batch
                .column(0)
                .as_any()
                .downcast_ref::<Int64Array>()
                .expect("the id column");
            for row in 0..batch.num_rows() {
                ids.insert(column.value(row));
            }
        }
    }
    ids
}

/// Run the soak, and report which inherited rows survived.
///
/// `told` decides whether the maintainer is told about the clone. That is the only difference
/// between the test and its control, which is the point: **a soak that only ever runs the
/// protected case cannot show the protection is what protected it.**
fn soak(rounds: i64, told: bool) -> (BTreeSet<i64>, BTreeSet<i64>) {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let origin_root = dir.path().join("entries");
    let clone_root = dir.path().join("staging");

    // --- an origin with enough files that compaction has something to do ---------------
    let origin = Publication::external(&origin_root, "entries");
    origin.create(&schema()).expect("creating the origin");
    for round in 0..rounds {
        origin
            .append(
                u64::try_from(round + 1).unwrap_or(1),
                &format!("part-{round:04}.parquet"),
                &batch(round * ROWS),
                Lsn::new(u64::try_from((round + 1) * ROWS).unwrap_or(1)),
            )
            .expect("publishing to the origin");
    }
    let cloned_at = u64::try_from(rounds).unwrap_or(1);

    // --- the clone, taken here and never updated ---------------------------------------
    let clone = Publication::external(&clone_root, "staging");
    let lineage = Lineage::new("entries", cloned_at, 0);
    clone
        .create_clone(
            &sankhya_table_delta::schema_string(&schema()).expect("a schema string"),
            &lineage.to_properties(),
        )
        .expect("creating the clone");

    let inherited = Inherited { origin_root: origin_root.clone(), version: cloned_at };
    let before = ids_readable(&clone_root, &inherited);
    assert_eq!(
        before.len(),
        usize::try_from(rounds * ROWS).unwrap_or(0),
        "the clone starts by reading everything the origin had"
    );

    // --- both sides diverge, and the origin is maintained relentlessly -----------------
    let mut clones = Lineages::new();
    if told {
        clones.record("staging", lineage);
    }
    let mut maintainer = Maintainer::new(relentless()).among(clones);

    for round in rounds..rounds * 2 {
        // The origin moves on. **Rebasing**, because compaction is committing to this same log
        // and takes versions of its own --- a writer that assumed it owned the sequence would
        // lose the race, which is the contention `ADR-0013`'s C3 is about and not this test's
        // subject.
        origin
            .append_rebasing(
                origin.next_version(),
                8,
                &format!("part-{round:04}.parquet"),
                &batch(round * ROWS),
                Lsn::new(u64::try_from((round + 1) * ROWS).unwrap_or(1)),
            )
            .expect("publishing to the origin");

        // And so does the clone, into its own log.
        clone
            .append_rebasing(
                clone.next_version(),
                8,
                &format!("own-{round:04}.parquet"),
                &batch(1_000_000 + round * ROWS),
                Lsn::new(u64::try_from((round + 1) * ROWS).unwrap_or(1)),
            )
            .expect("publishing to the clone");

        // Compaction, retirement and orphan collection, every round, with nothing waived.
        maintainer.tick(&origin_root).expect("a maintenance tick");
    }

    // A plan that names a file which is gone is a failure to *read*, not an empty read --- so
    // the control, which expects exactly that, must not die inside the helper. Caught here and
    // reported as "nothing survived", which is the honest reading.
    //
    // The default hook is silenced for the attempt, because a panic printed during a passing
    // run reads as a fault. What it would have said is what the control asserts anyway.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let after = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        ids_readable(&clone_root, &inherited)
    }))
    .unwrap_or_default();
    std::panic::set_hook(previous);

    (before, after)
}

#[test]
fn a_clone_reads_every_row_it_could_after_the_origin_is_fully_maintained() {
    let rounds: i64 = std::env::var("SANKHYA_CLONE_SOAK_ROUNDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(12);

    let (before, after) = soak(rounds, true);

    let lost: Vec<i64> = before.difference(&after).copied().collect();
    assert!(
        lost.is_empty(),
        "maintenance on the origin removed {} row(s) the clone could read before: {:?}",
        lost.len(),
        &lost[..lost.len().min(10)]
    );

    // The clone's own writes are there too, and the origin's later ones are not.
    let own: BTreeSet<i64> = (rounds..rounds * 2)
        .flat_map(|round| (0..ROWS).map(move |i| 1_000_000 + round * ROWS + i))
        .collect();
    assert!(own.is_subset(&after), "the clone cannot read what it wrote itself");
    let origins_later: BTreeSet<i64> = (rounds..rounds * 2)
        .flat_map(|round| (0..ROWS).map(move |i| round * ROWS + i))
        .collect();
    assert!(
        origins_later.is_disjoint(&after),
        "the clone is reading rows the origin wrote after it was taken"
    );

    println!(
        "clone soak: {rounds} rounds, {} inherited row(s) intact, {} of its own",
        before.len(),
        own.len()
    );
}

#[test]
fn the_same_soak_without_the_lineage_loses_what_the_clone_could_read() {
    // **The control, and the reason the test above means anything.**
    //
    // Told about the clone, maintenance keeps the versions it pins. Not told, it sees files that
    // are on disk, absent from the live set and past their threshold --- debris by every measure
    // it has --- and reclaims them. Nothing errors either way. The difference is whether the
    // clone can still be read, and asserting the loss here is what proves the protection above
    // is doing the protecting rather than the policy being too gentle to reclaim anything.
    let (before, after) = soak(12, false);

    assert!(
        !before.is_empty(),
        "the clone could read nothing to begin with, so this proves nothing"
    );
    assert!(
        after != before,
        "an unprotected clone survived relentless maintenance, so the protected one proves \
         nothing --- either the policy reclaims nothing or the inherited files were never \
         touchable"
    );
    println!(
        "clone soak control: {} row(s) readable before, {} after",
        before.len(),
        after.len()
    );
}

#[test]
fn the_origin_is_actually_maintained_which_is_what_makes_the_other_test_mean_something() {
    // The control. Without it the first test would pass just as happily against a maintainer
    // that did nothing at all --- and "the clone still reads everything" is trivially true when
    // nothing was ever reclaimed. This asserts the hostile policy is hostile.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let origin_root = dir.path().join("entries");

    let origin = Publication::external(&origin_root, "entries");
    origin.create(&schema()).expect("creating");
    for round in 0..12i64 {
        origin
            .append(
                u64::try_from(round + 1).unwrap_or(1),
                &format!("part-{round:04}.parquet"),
                &batch(round * ROWS),
                Lsn::new(u64::try_from((round + 1) * ROWS).unwrap_or(1)),
            )
            .expect("publishing");
    }

    let before = sankhya_table_delta::live_files(&origin_root).expect("its log").files.len();

    // No clones at all: the ordinary case, where maintenance is free to reclaim everything.
    let mut maintainer = Maintainer::new(relentless());
    for _ in 0..12 {
        maintainer.tick(&origin_root).expect("a tick");
    }
    let after = sankhya_table_delta::live_files(&origin_root).expect("its log").files.len();

    assert!(
        after < before,
        "the maintenance policy reclaimed nothing ({before} files before, {after} after), so \
         the clone soak would have proven nothing"
    );
}

/// Bytes on disk under a directory, and how many files.
fn footprint(root: &std::path::Path) -> (u64, usize) {
    fn walk(at: &std::path::Path, bytes: &mut u64, files: &mut usize) {
        let Ok(entries) = std::fs::read_dir(at) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, bytes, files);
            } else if let Ok(meta) = entry.metadata() {
                *bytes += meta.len();
                *files += 1;
            }
        }
    }
    let (mut bytes, mut files) = (0, 0);
    walk(root, &mut bytes, &mut files);
    (bytes, files)
}

/// An origin of `files` published fragments.
fn origin_of(root: &std::path::Path, name: &str, files: i64) -> Publication {
    let publication = Publication::external(root.join(name), name);
    publication.create(&schema()).expect("creating");
    for file in 0..files {
        publication
            .append(
                u64::try_from(file + 1).unwrap_or(1),
                &format!("part-{file:04}.parquet"),
                &batch(file * ROWS),
                Lsn::new(u64::try_from((file + 1) * ROWS).unwrap_or(1)),
            )
            .expect("publishing");
    }
    publication
}

/// Clone `origin` and report what the clone cost on disk.
fn clone_cost(root: &std::path::Path, origin: &str, at: u64, name: &str) -> (u64, usize) {
    let clone_root = root.join(name);
    let publication = Publication::external(&clone_root, name);
    publication
        .create_clone(
            &sankhya_table_delta::schema_string(&schema()).expect("a schema string"),
            &Lineage::new(origin, at, 0).to_properties(),
        )
        .expect("creating the clone");
    footprint(&clone_root)
}

#[test]
fn a_clone_costs_the_same_against_a_large_table_as_against_a_small_one() {
    // `M10`'s first exit criterion: a clone demonstrated at **constant cost** against a large
    // table.
    //
    // # Why this is proved structurally rather than timed
    //
    // The tempting demonstration is a stopwatch: clone a small table, clone a large one, compare.
    // That would be a *throughput measurement*, and this repository has spent a day learning what
    // those cost — five guards, four failed gate runs, and the eventual answer that such
    // measurements have to run alone on a quiet machine. Inventing a sixth would be poor
    // value for a property that is not statistical.
    //
    // Because it is not. `ADR-0016` Decision 1a makes a clone's log hold **no `Add` actions at
    // all** — the rows it starts with are the origin's and stay where they are. So the clone's
    // cost is one commit whatever the origin holds, and that is an exact claim about bytes and
    // files rather than a distribution over runs. Proving the exact thing exactly is better than
    // measuring a proxy badly.
    let dir = tempfile::tempdir().expect("a temporary directory");

    origin_of(dir.path(), "small", 2);
    origin_of(dir.path(), "large", 60);

    let (small_bytes, small_files) = footprint(&dir.path().join("small"));
    let (large_bytes, large_files) = footprint(&dir.path().join("large"));
    assert!(
        large_files > small_files * 10 && large_bytes > small_bytes * 5,
        "the two origins are not different enough for this to mean anything: \
         {small_files} files/{small_bytes} bytes against {large_files}/{large_bytes}"
    );

    let (from_small, files_small) = clone_cost(dir.path(), "small", 2, "of_small");
    let (from_large, files_large) = clone_cost(dir.path(), "large", 60, "of_large");

    assert_eq!(
        files_small, files_large,
        "cloning a {large_files}-file table wrote a different number of files than cloning a \
         {small_files}-file one"
    );
    // Within a few bytes rather than identical, and the difference is worth naming because the
    // first version of this test asserted equality and failed by **one byte**: the lineage
    // records the origin version as text, so cloning at version 60 writes one character more
    // than cloning at version 2. That is the only thing about a clone that varies with anything,
    // and it varies with the *number*, not with the table.
    let difference = from_large.abs_diff(from_small);
    assert!(
        difference <= 16,
        "cloning a {large_files}-file table cost {from_large} bytes and a {small_files}-file \
         one cost {from_small}, a difference of {difference}; a clone that grows with its \
         origin is a copy"
    );
    assert!(
        from_large < large_bytes / 10,
        "the clone cost {from_large} bytes against an origin of {large_bytes}, which is not \
         constant space by any reading"
    );

    println!(
        "clone cost: {files_small} file(s), {from_small} and {from_large} bytes, from origins \
         of {small_files} and {large_files} files"
    );
}

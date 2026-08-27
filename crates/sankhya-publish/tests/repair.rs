//! Repairing a table, and refusing to.
//!
//! The tests that matter most here are the **refusals**. A repair tool that guesses is
//! worse than no repair tool: it writes a plausible invented value into the table
//! permanently, with an operator's confidence attached, because a tool said it was fixed.
//!
//! So roughly half of this file checks that things are *not* repaired, and that the
//! explanation says what a person would have to decide.

// Tests may panic — that is how a test reports a failure. The workspace denies
// `unwrap`, `expect`, `panic` and indexing because a *server* must not do those things
// on data it did not choose; a test chooses all of its data, and an assertion that
// cannot fail loudly is worse than useless.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use arrow_array::{Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use sankhya_publish::repair::{apply, plan, Repair};
use sankhya_publish::verify::{verify, Finding};
use sankhya_table::{write_parquet, WriterConfig};
use sankhya_table_delta::{commit, create, Action, AddFile, Metadata};
use sankhya_types::Lsn;
use std::sync::Arc;

fn schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("label", DataType::Utf8, true),
    ]))
}

fn batch(from: i64, rows: i64) -> RecordBatch {
    let ids: Vec<i64> = (from..from + rows).collect();
    let labels: Vec<Option<String>> = ids.iter().map(|i| Some(format!("row-{i}"))).collect();
    RecordBatch::try_new(
        schema(),
        vec![
            Arc::new(Int64Array::from(ids)),
            Arc::new(StringArray::from(labels)),
        ],
    )
    .expect("a valid batch")
}

/// A table whose files are real but whose log records no statistics for them.
///
/// Exactly what a hand-written publisher produces: the data is fine, and every query
/// against it reads every file because nothing can be pruned.
fn table_missing_statistics(files: usize) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("orders");
    std::fs::create_dir_all(&root).expect("creating");

    let json = sankhya_table_delta::schema_string(&schema()).expect("representable");
    commit(&root, 0, &create(Metadata::new("orders", json, 0))).expect("creating");

    let mut adds = Vec::new();
    for index in 0..files {
        let name = format!("part-{index:04}.parquet");
        let rows = batch(i64::try_from(index).unwrap_or(0) * 100, 100);
        let report = write_parquet(&root, &name, &rows, Lsn::new(1), WriterConfig::default())
            .expect("writing");
        // `AddFile::new` carries no statistics at all — the hand-written case.
        adds.push(Action::Add(AddFile::new(name, report.bytes, 0)));
    }
    commit(&root, 1, &adds).expect("adding");
    (dir, root)
}

// --- what it repairs ------------------------------------------------------

#[test]
fn missing_statistics_are_recomputed_from_the_files_themselves() {
    // Derived, not guessed. The file is the truth and reading it invents nothing.
    let (_dir, root) = table_missing_statistics(3);
    assert_eq!(verify(&root).findings.len(), 3);

    let plan = plan(&root);
    assert_eq!(plan.actions.len(), 3);
    assert!(plan.would_fully_repair());
    assert!(plan.refused.is_empty());

    let outcome = apply(&plan).expect("the repair commits");
    assert_eq!(outcome.repaired.len(), 3);
    assert!(outcome.failed.is_empty());
    assert!(
        outcome.is_clean_now(),
        "afterwards: {:?}",
        outcome.after.findings
    );
}

#[test]
fn the_recomputed_statistics_are_the_real_ones() {
    // Not merely present. A tool that wrote a placeholder to silence the finding would be
    // the worst possible outcome: pruning would then skip files that hold matching rows.
    let (_dir, root) = table_missing_statistics(1);
    apply(&plan(&root)).expect("repairing");

    let live = sankhya_table_delta::live_files(&root).expect("readable");
    let file = live.files.first().expect("one file");
    let stats = file.stats.as_ref().expect("statistics now present");

    assert!(stats.contains("\"numRecords\":100"), "{stats}");
    // The fixture's first file holds ids 0..99.
    assert!(stats.contains("minValues"), "{stats}");
    assert!(stats.contains("maxValues"), "{stats}");
    assert!(
        stats.contains("99"),
        "the true maximum must be recorded: {stats}"
    );
}

#[test]
fn a_repair_is_appended_as_a_new_version_and_never_rewrites_one() {
    // The log is append-only, so the broken state stays readable for forensics, the repair
    // is revertible, and time travel to before it still works.
    let (_dir, root) = table_missing_statistics(1);
    let before = std::fs::read_to_string(root.join("_delta_log/00000000000000000001.json"))
        .expect("version 1 exists");

    let outcome = apply(&plan(&root)).expect("repairing");
    assert_eq!(outcome.version, Some(2), "a new version, not a rewrite");

    let after = std::fs::read_to_string(root.join("_delta_log/00000000000000000001.json"))
        .expect("version 1 still exists");
    assert_eq!(before, after, "the broken commit is left exactly as it was");
    assert!(root.join("_delta_log/00000000000000000002.json").exists());
}

#[test]
fn a_repair_never_deletes_a_file() {
    // A repair that removes data is not a repair.
    let (_dir, root) = table_missing_statistics(2);
    let before: Vec<_> = std::fs::read_dir(&root)
        .expect("readable")
        .filter_map(Result::ok)
        .map(|e| e.file_name())
        .collect();

    apply(&plan(&root)).expect("repairing");

    let after: Vec<_> = std::fs::read_dir(&root)
        .expect("readable")
        .filter_map(Result::ok)
        .map(|e| e.file_name())
        .collect();
    assert_eq!(before.len(), after.len(), "no data file was removed");
}

#[test]
fn planning_changes_nothing() {
    // Plan reads and decides; apply writes. Doing nothing with a plan is always an option.
    let (_dir, root) = table_missing_statistics(2);
    let before = verify(&root);
    let _ = plan(&root);
    assert_eq!(verify(&root), before, "planning must not touch the table");
}

#[test]
fn repairing_an_already_clean_table_does_nothing() {
    // Including writing an empty commit, which would grow the log for no reason and make
    // every subsequent replay slightly slower.
    use sankhya_publish::publish::{publish_table, Publication};
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("clean");
    publish_table(
        &Publication::external(&root, "clean"),
        &schema(),
        &[batch(0, 10)],
    )
    .expect("publishing");

    let plan = plan(&root);
    assert!(plan.is_empty());
    assert!(!plan.would_fully_repair(), "there is nothing to repair");

    let outcome = apply(&plan).expect("nothing to do");
    assert_eq!(outcome.version, None, "no commit was written");
    assert!(outcome.is_clean_now());
}

#[test]
fn the_outcome_is_verified_rather_than_assumed() {
    // A repair tool that reports success without looking is one nobody should trust.
    let (_dir, root) = table_missing_statistics(1);
    let outcome = apply(&plan(&root)).expect("repairing");
    assert_eq!(outcome.after, verify(&root));
}

// --- what it refuses ------------------------------------------------------

#[test]
fn a_missing_schema_is_refused_because_it_cannot_be_inferred() {
    // A table with a column added after its files were written would infer a schema missing
    // it, and an empty table has nothing to infer from at all.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("no-metadata");
    std::fs::create_dir_all(root.join("_delta_log")).expect("creating");

    let plan = plan(&root);
    assert!(plan.actions.is_empty(), "nothing here can be derived");
    let refusal = plan.refused.first().expect("a refusal");
    assert_eq!(refusal.finding, Finding::NoMetadata);
    assert!(refusal.why.contains("cannot be inferred"));
    assert!(
        refusal.decision.contains("Supply the schema"),
        "the refusal must say what a person has to decide: {}",
        refusal.decision
    );
    assert!(!plan.would_fully_repair());
}

#[test]
fn a_key_column_that_does_not_exist_is_refused_because_only_a_person_knows_what_it_was() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("bad-key");
    std::fs::create_dir_all(&root).expect("creating");

    let json = sankhya_table_delta::schema_string(&schema()).expect("representable");
    let mut metadata = Metadata::new("bad-key", json, 0);
    metadata.configuration = sankhya_publish::class::configuration(
        sankhya_publish::class::TableClass::External,
        &["account_ref".to_string()],
    );
    commit(&root, 0, &[Action::Metadata(metadata)]).expect("creating");

    let plan = plan(&root);
    let refusal = plan
        .refused
        .iter()
        .find(|r| matches!(r.finding, Finding::KeyColumnMissing { .. }))
        .expect("a refusal about the key");
    assert!(refusal.why.contains("Only a person knows"));
    assert!(
        refusal.why.contains("resolves distinct rows into one"),
        "the refusal must say what guessing wrong costs: {}",
        refusal.why
    );
}

#[test]
fn a_directory_that_is_not_a_table_is_refused_rather_than_created() {
    // A repair tool that helpfully creates the table it was pointed at, on a mistyped path,
    // is how a warehouse acquires an empty table nobody meant.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let plan = plan(&dir.path().join("does-not-exist"));

    assert!(plan.actions.is_empty());
    let refusal = plan.refused.first().expect("a refusal");
    assert!(refusal.why.contains("nothing here to repair"));
    assert!(
        !dir.path().join("does-not-exist").exists(),
        "and nothing was created"
    );
}

#[test]
fn a_partial_repair_reports_what_it_could_not_do() {
    // Three files fixed and one unreadable is more useful than a single failure that hides
    // the three.
    let (_dir, root) = table_missing_statistics(3);
    // Remove one file's data, leaving its log entry.
    std::fs::remove_file(root.join("part-0001.parquet")).expect("removing");

    let outcome = apply(&plan(&root)).expect("the repair still commits");
    assert_eq!(outcome.repaired.len(), 2);
    assert_eq!(outcome.failed.len(), 1);
    let (file, reason) = outcome.failed.first().expect("one failure");
    assert_eq!(file, "part-0001.parquet");
    assert!(reason.contains("opening"), "{reason}");
    assert!(
        !outcome.is_clean_now(),
        "the file with no data still has no statistics"
    );
}

#[test]
fn the_plan_says_whether_it_is_the_whole_fix() {
    // An operator needs to know before they start whether this is all of it.
    let (_dir, root) = table_missing_statistics(1);
    assert!(plan(&root).would_fully_repair());

    let dir = tempfile::tempdir().expect("a temporary directory");
    let empty = dir.path().join("no-metadata");
    std::fs::create_dir_all(empty.join("_delta_log")).expect("creating");
    assert!(!plan(&empty).would_fully_repair());
}

#[test]
fn the_only_derivable_repair_is_named_in_the_plan() {
    // So a reviewer of the plan sees what will happen to which file, not a count.
    let (_dir, root) = table_missing_statistics(1);
    let plan = plan(&root);
    let Repair::RecomputeStatistics { file } = plan.actions.first().expect("one action");
    assert_eq!(file, "part-0000.parquet");
    assert!(plan.summary().contains("1 action"));
}

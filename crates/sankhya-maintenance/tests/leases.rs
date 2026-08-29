//! Retirement waits for readers, not for a number of ticks.
//!
//! Compaction publishes a merge and its inputs stop being referenced. They cannot be deleted
//! immediately: a reader that listed the table before the merge holds their paths and will open
//! them. Until leases existed, what protected that reader was `grace_ticks` --- a count of
//! maintenance passes, chosen generously, and a proxy for "a reader might still be here".
//!
//! These tests are about the difference between a proxy and an answer.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use arrow_array::{Int64Array, RecordBatch};
use arrow_schema::{DataType, Field, Schema};
use sankhya_leases::Leases;
use sankhya_maintenance::{Maintainer, MaintenancePolicy, OrphanPolicy};
use sankhya_table::{write_parquet, WriterConfig};
use sankhya_table_delta::{commit, create, Action, AddFile, Metadata};
use sankhya_types::Lsn;
use std::sync::Arc;

const SCHEMA: &str = r#"{"type":"struct","fields":[{"name":"id","type":"long","nullable":false,"metadata":{}}]}"#;

fn arrow_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]))
}

/// A table of eight small files in one partition, which compaction will merge.
fn a_compactable_table(root: &std::path::Path) {
    commit(root, 0, &create(Metadata::new("orders", SCHEMA.to_string(), 0))).expect("creating");
    let mut adds = Vec::new();
    for i in 0..8_u64 {
        let name = format!("part-{i:04}.parquet");
        let batch = RecordBatch::try_new(
            arrow_schema(),
            vec![Arc::new(Int64Array::from((0..100_i64).collect::<Vec<i64>>()))],
        )
        .expect("a batch");
        let report = write_parquet(
            root,
            &name,
            &batch,
            Lsn::new(i * 100 + 100),
            WriterConfig::default(),
        )
        .expect("writing");
        adds.push(Action::Add(AddFile::with_rows(name, report.bytes, 0, 100)));
    }
    commit(root, 1, &adds).expect("publishing");
}

/// Compacting every tick, with the shortest grace period that still leaves a window.
///
/// One tick, not zero. At zero, `retire_due` runs in the same pass that queued the merge and
/// the inputs are gone before anything else can happen --- which leaves no moment in which a
/// reader can arrive, and so nothing for these tests to observe. The point is to make the
/// *lease* the thing being tested, not to remove every other protection.
fn retiring_next_tick() -> MaintenancePolicy {
    let mut policy = MaintenancePolicy {
        compact_every: 1,
        orphan_sweep_every: 0,
        orphans: OrphanPolicy { min_age_ticks: 0 },
        ..MaintenancePolicy::default()
    };
    policy.retention.grace_ticks = 1;
    policy
}

#[test]
fn a_reader_that_arrived_before_the_merge_keeps_its_inputs_alive() {
    // The whole point. `grace_ticks` is zero, so nothing but the lease is holding these files
    // --- which is what makes this a test of the lease rather than of the grace period.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path();
    a_compactable_table(root);

    let leases = Arc::new(Leases::new());
    let reader = leases.pin();
    let mut maintainer = Maintainer::new(retiring_next_tick()).watching(Arc::clone(&leases));

    let merged = maintainer.tick(root).expect("a tick");
    assert!(!merged.merged.is_empty(), "the fixture must actually compact");

    let after = maintainer.tick(root).expect("another tick");
    assert!(
        after.files_removed.is_empty(),
        "a reader that started before the merge is still holding these paths: {:?}",
        after.files_removed
    );
    assert!(
        root.join("part-0000.parquet").exists(),
        "and the file it would open is still there"
    );

    drop(reader);
    let released = maintainer.tick(root).expect("a tick after the reader left");
    assert!(
        !released.files_removed.is_empty(),
        "once the reader has finished the inputs are collectable"
    );
    assert!(
        !root.join("part-0000.parquet").exists(),
        "and they are actually gone"
    );
}

#[test]
fn a_reader_that_arrived_after_the_merge_does_not_delay_it() {
    // The other direction, and the one that matters for a busy warehouse. A reader that
    // started after the inputs stopped being referenced resolved a log that does not name
    // them, so waiting for it would mean a warehouse under continuous read load never
    // reclaims anything --- which is a disk filling up, and this warehouse has met that once.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path();
    a_compactable_table(root);

    let leases = Arc::new(Leases::new());
    let mut maintainer = Maintainer::new(retiring_next_tick()).watching(Arc::clone(&leases));

    let merged = maintainer.tick(root).expect("a tick");
    assert!(!merged.merged.is_empty(), "the fixture must actually compact");

    // Arrives only now, after the merge has been committed.
    let _latecomer = leases.pin();

    let after = maintainer.tick(root).expect("another tick");
    assert!(
        !after.files_removed.is_empty(),
        "a reader that arrived after the merge cannot be holding its inputs"
    );
}

#[test]
fn a_maintainer_nobody_is_watching_falls_back_to_the_grace_period() {
    // A maintainer running against a warehouse no server is serving has no lease information,
    // and must behave exactly as it did before leases existed. Treating "nobody told me" as
    // "nobody is reading" would be right here and catastrophic in the case where the
    // information simply failed to arrive --- so it is the *absence of a source* that is
    // checked, not an empty registry.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path();
    a_compactable_table(root);

    let mut maintainer = Maintainer::new(retiring_next_tick());
    let merged = maintainer.tick(root).expect("a tick");
    assert!(!merged.merged.is_empty(), "the fixture must actually compact");

    let after = maintainer.tick(root).expect("another tick");
    assert!(
        !after.files_removed.is_empty(),
        "with no lease source, retirement proceeds on the grace period as it always did"
    );
}

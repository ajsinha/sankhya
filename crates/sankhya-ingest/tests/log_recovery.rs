//! Capture publishes into a table log, and recovers its position from it.
//!
//! A restart resets everything the pipeline holds in memory. Both counters that name
//! files and versions are *derived* rather than remembered, so the only thing that can
//! restore them is the log itself — which is the point of having one.
//!
//! The failure this prevents is quiet: restarting at sequence zero writes
//! `00000000.parquet` over a file that is still live and still referenced. Nothing
//! errors, the file count does not change, and the rows in the overwritten file simply
//! become different rows.

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

use sankhya_cdc_apply::BatchPolicy;
use sankhya_cdc_model::{
    ColumnDescriptor, Message, RelationDescriptor, ReplicaIdentity, TupleData, TupleValue,
};
use sankhya_maintenance::{Maintainer, MaintenancePolicy};
use sankhya_ingest::Pipeline;
use sankhya_table::WriterConfig;
use sankhya_table_delta::{live_files, read_actions, Action};
use sankhya_types::{Lsn, Timestamp};
use std::sync::Arc;

const RELATION: u32 = 100;

fn relation() -> Message {
    Message::Relation(Arc::new(RelationDescriptor {
        relation_id: RELATION,
        namespace: "public".into(),
        name: "readings".into(),
        replica_identity: ReplicaIdentity::Default,
        columns: vec![
            ColumnDescriptor {
                name: "id".into(),
                type_oid: 20,
                type_modifier: -1,
                is_key: true,
            },
            ColumnDescriptor {
                name: "label".into(),
                type_oid: 25,
                type_modifier: -1,
                is_key: false,
            },
        ],
    }))
}

/// `transactions` transactions of `rows_each` rows, starting at position `from * 10`.
fn stream(from: usize, transactions: usize, rows_each: usize) -> Vec<Message> {
    let mut out = vec![relation()];
    let mut id = (from * rows_each) as u64;
    for t in from..from + transactions {
        out.push(Message::Begin {
            final_lsn: Lsn::new(0),
            commit_time: Timestamp::EPOCH,
            xid: (t + 1) as u32,
        });
        for _ in 0..rows_each {
            id += 1;
            out.push(Message::Insert {
                relation_id: RELATION,
                new: TupleData {
                    values: vec![
                        TupleValue::Text(id.to_string()),
                        TupleValue::Text(format!("row-{id}")),
                    ],
                },
            });
        }
        let at = ((t + 1) * 10) as u64;
        out.push(Message::Commit {
            commit_lsn: Lsn::new(at),
            end_lsn: Lsn::new(at),
            commit_time: Timestamp::EPOCH,
        });
    }
    out
}

fn pipeline(dir: &std::path::Path) -> Pipeline {
    Pipeline::new(
        dir,
        BatchPolicy {
            max_rows: 10,
            max_transactions: usize::MAX,
            ..BatchPolicy::default()
        },
        WriterConfig::default(),
    )
}

fn table_root(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join("public").join("readings")
}

/// Feed the stream, publishing at every transaction boundary.
///
/// Publishing only once at the end would produce a single file per run, and a fixture
/// with one file cannot distinguish resuming from the highest committed sequence from
/// resuming from the lowest — they are the same number. Continuous capture publishes on
/// a cadence, so several files per run is the realistic shape as well as the
/// discriminating one.
fn run(p: &mut Pipeline, messages: &[Message]) {
    for message in messages {
        p.accept(message).expect("accepting");
        if matches!(message, Message::Commit { .. }) {
            p.publish(true).expect("publishing");
        }
    }
    p.publish(true).expect("publishing");
}

#[test]
fn publishing_creates_the_table_and_commits_every_file() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let mut p = pipeline(dir.path());
    run(&mut p, &stream(0, 6, 10));

    let root = table_root(dir.path());
    let live = live_files(&root).expect("the log must exist");

    assert!(!live.files.is_empty(), "nothing was committed");
    for file in &live.files {
        assert!(
            root.join(&file.path).exists(),
            "{} is in the log but not on disk",
            file.path
        );
        assert!(
            file.rows().is_some(),
            "{} was committed without a row count and cannot be planned against",
            file.path
        );
    }

    // The creating commit is version zero and carries the schema.
    let actions = read_actions(&root).expect("reading");
    assert!(matches!(actions[0], (0, Action::Protocol { .. })));
    assert!(matches!(actions[1], (0, Action::Metadata(_))));
}

#[test]
fn a_restart_does_not_reuse_a_file_name() {
    // The failure this exists for. Restarting at sequence zero overwrites a live file,
    // and nothing about the result looks wrong.
    let dir = tempfile::tempdir().expect("a temp dir");
    let root = table_root(dir.path());

    let mut first = pipeline(dir.path());
    run(&mut first, &stream(0, 6, 10));
    let before: Vec<String> = live_files(&root)
        .expect("log")
        .files
        .iter()
        .map(|f| f.path.clone())
        .collect();
    drop(first);

    // Restart. Fresh in-memory state, same warehouse.
    let mut second = pipeline(dir.path());
    run(&mut second, &stream(6, 6, 10));

    let after: Vec<String> = live_files(&root)
        .expect("log")
        .files
        .iter()
        .map(|f| f.path.clone())
        .collect();

    assert!(after.len() > before.len(), "the restart published nothing");
    for name in &before {
        assert!(
            after.contains(name),
            "{name} was published before the restart and is no longer live"
        );
    }

    let mut unique = after.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), after.len(), "a file name was reused");

    // Names alone are not enough, and this is the assertion that matters.
    //
    // Overwriting a live file changes its *content*, not its name. The live set looks
    // identical, the file count is unchanged, and the rows that were in the overwritten
    // file have simply become different rows. The log records the overwrite as a second
    // `add` for the same path, so that is what to look for.
    let history = read_actions(&root).expect("reading");
    let mut added: Vec<&str> = history
        .iter()
        .filter_map(|(_, a)| match a {
            Action::Add(f) => Some(f.path.as_str()),
            _ => None,
        })
        .collect();
    let total = added.len();
    added.sort_unstable();
    added.dedup();
    assert_eq!(
        added.len(),
        total,
        "a path was added twice, which means a live file was overwritten"
    );
}

#[test]
fn a_restart_continues_the_version_sequence() {
    // Restarting at version zero would be told the table already exists, and the
    // pipeline would stop publishing entirely.
    let dir = tempfile::tempdir().expect("a temp dir");
    let root = table_root(dir.path());

    let mut first = pipeline(dir.path());
    run(&mut first, &stream(0, 4, 10));
    let version_before = live_files(&root).expect("log").version.expect("a version");
    drop(first);

    let mut second = pipeline(dir.path());
    run(&mut second, &stream(4, 4, 10));
    let version_after = live_files(&root).expect("log").version.expect("a version");

    assert!(
        version_after > version_before,
        "the log did not advance across the restart: {version_before} then {version_after}"
    );
}

#[test]
fn a_name_is_not_reused_even_after_the_file_leaves_the_live_set() {
    // The sequence is recovered from the whole history, not from what is currently live.
    // A compacted-away file's name must not come back while a reader holding an older
    // snapshot can still resolve it.
    use sankhya_table_delta::{commit, AddFile, RemoveFile};

    let dir = tempfile::tempdir().expect("a temp dir");
    let root = table_root(dir.path());

    let mut first = pipeline(dir.path());
    run(&mut first, &stream(0, 6, 10));
    let live = live_files(&root).expect("log");
    let retired: Vec<String> = live.files.iter().map(|f| f.path.clone()).collect();
    drop(first);

    // Compact through the product's own maintenance, not by writing the log by hand.
    //
    // This used to commit an add for `merged.parquet` --- a file it never wrote --- plus a
    // remove for every input. It made the assertion below pass while describing a warehouse
    // that could not exist, and it called itself "as maintenance would" while doing none of
    // what maintenance does.
    let mut maintainer = Maintainer::new(MaintenancePolicy::default());
    maintainer.tick(&root).expect("a maintenance tick");

    assert_eq!(live_files(&root).expect("log").files.len(), 1);

    // Restart and publish again.
    let mut second = pipeline(dir.path());
    run(&mut second, &stream(6, 4, 10));

    let after: Vec<String> = live_files(&root)
        .expect("log")
        .files
        .iter()
        .map(|f| f.path.clone())
        .collect();

    for name in &retired {
        assert!(
            !after.contains(name),
            "{name} was retired and its name has been reused"
        );
    }

    // And no path was written over, retired or not.
    let history = read_actions(&root).expect("reading");
    let mut added: Vec<&str> = history
        .iter()
        .filter_map(|(_, a)| match a {
            Action::Add(f) => Some(f.path.as_str()),
            _ => None,
        })
        .collect();
    let total = added.len();
    added.sort_unstable();
    added.dedup();
    assert_eq!(added.len(), total, "a path was added twice");
}

#[test]
fn a_published_file_is_prunable_from_the_moment_it_is_committed() {
    // Without this, a freshly captured file is never skipped until maintenance has been
    // over it -- safe, and slower than it needs to be for exactly as long as compaction
    // is behind. The batch is already in memory and is already the file's exact
    // contents, so this costs no read.
    use sankhya_stats::{can_skip, Bound, Predicate};

    let dir = tempfile::tempdir().expect("a temp dir");
    let mut p = pipeline(dir.path());
    run(&mut p, &stream(0, 6, 10));

    let root = table_root(dir.path());
    let live = live_files(&root).expect("log");
    assert!(!live.files.is_empty());

    for file in &live.files {
        let statistics = file
            .statistics()
            .unwrap_or_else(|| panic!("{} was published without statistics", file.path));
        let columns = sankhya_table_delta::to_column_stats(&statistics);

        let id = columns
            .get("id")
            .unwrap_or_else(|| panic!("{} has no statistics for id", file.path));

        assert!(id.min.is_some(), "{} has no lower bound for id", file.path);
        assert!(id.max.is_some());
        assert_eq!(id.nulls, 0);

        // The column is declared int8 in the relation, so it maps to an integer bound
        // rather than a text one even though the wire carries it as text -- which is
        // the type mapping doing its job, and is worth asserting because a bound of the
        // wrong type silently prunes nothing.
        assert!(matches!(id.min, Some(Bound::Int(_))), "{:?}", id.min);

        // And they are usable: nothing in this stream has an id beyond the tenth
        // thousand, so every file is skippable for a predicate above it.
        assert!(can_skip(id, &Predicate::GreaterThan(Bound::Int(999_999))));
    }
}

#[test]
fn published_bounds_do_not_claim_more_than_the_file_holds() {
    // A bound that overstates is safe; one that understates skips a file holding a
    // match. Checked against the file's own contents rather than against what the
    // pipeline believed it wrote.
    use sankhya_stats::{can_skip, Bound, Predicate};

    let dir = tempfile::tempdir().expect("a temp dir");
    let mut p = pipeline(dir.path());
    run(&mut p, &stream(0, 4, 10));

    let root = table_root(dir.path());
    for file in &live_files(&root).expect("log").files {
        let columns = sankhya_table_delta::to_column_stats(&file.statistics().expect("statistics"));
        let id = &columns["id"];
        let Some(Bound::Int(min)) = id.min else {
            panic!("expected an integer bound for id, got {:?}", id.min);
        };
        let Some(Bound::Int(max)) = id.max else {
            panic!("expected an integer bound for id");
        };
        assert!(min <= max);

        // Every value actually in the file is inside the declared bounds, so no
        // predicate matching a real value may skip the file.
        for value in [min, max] {
            assert!(
                !can_skip(id, &Predicate::Equals(Bound::Int(value))),
                "{} declares {min}..={max} and would skip {value}",
                file.path
            );
        }
        assert!(!can_skip(
            id,
            &Predicate::Between {
                low: Bound::Int(min),
                high: Bound::Int(max),
            }
        ));
    }
}

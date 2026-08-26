//! Compaction keeps up while capture is still writing.
//!
//! Every other compaction test starts from a warehouse that has stopped changing. That
//! is the easy case and it is not the case that matters: the system's whole difficulty
//! is that capture creates the mess *while* maintenance clears it, so the question is
//! whether the loop converges against a moving target or merely against a stationary one.
//!
//! The failure this would catch is a compaction that is always one tick behind — file
//! counts that grow slowly and forever, which looks fine for an hour and is a
//! self-reinforcing collapse over a week, because more files make queries slower, slower
//! queries take more of the machine, and less is left for compaction.

use sankhya_cdc_apply::BatchPolicy;
use sankhya_cdc_model::{
    ColumnDescriptor, Message, RelationDescriptor, ReplicaIdentity, TupleData, TupleValue,
};
use sankhya_ingest::Pipeline;
use sankhya_maintenance::{
    apply, commit_tick, execute_tick, plan_tick, CompactionPolicy, DriverPolicy, FileStat,
    PartitionState, SystemState,
};
use sankhya_table::WriterConfig;
use sankhya_table_delta::live_files;
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

/// One transaction of `rows` rows, committing at `at`.
fn transaction(start: u64, rows: u64, at: u64) -> Vec<Message> {
    let mut out = Vec::new();
    out.push(Message::Begin {
        final_lsn: Lsn::new(0),
        commit_time: Timestamp::EPOCH,
        xid: u32::try_from(at).expect("a small position"),
    });
    for i in 0..rows {
        let id = start + i;
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
    out.push(Message::Commit {
        commit_lsn: Lsn::new(at),
        end_lsn: Lsn::new(at),
        commit_time: Timestamp::EPOCH,
    });
    out
}

fn policy() -> DriverPolicy {
    DriverPolicy {
        compaction: CompactionPolicy {
            small_file_bytes: 1024 * 1024,
            routine_file_count: 4,
            elevated_file_count: 8,
            urgent_file_count: 16,
            ..CompactionPolicy::default()
        },
        ..DriverPolicy::default()
    }
}

#[test]
fn file_counts_stay_within_policy_while_capture_keeps_writing() {
    const TICKS: u64 = 40;
    const TXNS_PER_TICK: u64 = 3;
    const ROWS_PER_TXN: u64 = 40;
    /// Sweeps are rarer than publishes, so a backlog actually forms.
    const MAINTENANCE_EVERY: u64 = 3;

    let dir = tempfile::tempdir().expect("a temp dir");
    let table_root = dir.path().join("public").join("readings");

    let mut pipeline = Pipeline::new(
        dir.path(),
        BatchPolicy {
            max_rows: ROWS_PER_TXN as usize,
            max_transactions: usize::MAX,
            ..BatchPolicy::default()
        },
        WriterConfig::default(),
    );
    pipeline.accept(&relation()).expect("onboarding");

    let state = SystemState {
        in_maintenance_window: false,
        queries_running: 0,
        duty_cycle_ticks_remaining: 10_000,
    };
    let policy = policy();
    let urgent = policy.compaction.urgent_file_count;

    let mut id = 0u64;
    let mut position = 0u64;
    let mut version = 0u64;
    let mut peak_live = 0usize;

    for tick in 1..=TICKS {
        // Capture, as it happens in production: several transactions, published on a
        // cadence rather than all at once.
        for _ in 0..TXNS_PER_TICK {
            position += 10;
            for message in transaction(id, ROWS_PER_TXN, position) {
                pipeline.accept(&message).expect("accepting");
            }
            id += ROWS_PER_TXN;
            pipeline.publish(true).expect("publishing");
        }

        // Maintenance runs on a duty cycle, not after every publish. Compacting the
        // instant anything accumulates is not the situation worth testing: files pile up
        // between sweeps, and the question is whether a sweep can clear a backlog that
        // grew while it was not running.
        if tick % MAINTENANCE_EVERY != 0 {
            let logged = live_files(&table_root).expect("log");
            peak_live = peak_live.max(logged.files.len());
            assert!(
                logged.files.len() <= urgent,
                "at tick {tick} the partition held {} live files between sweeps and the \
                 urgent threshold is {urgent}",
                logged.files.len()
            );
            continue;
        }

        // Maintenance runs against what the log says, not a listing.
        let logged = live_files(&table_root).expect("log");
        version = logged.version.expect("a version");
        peak_live = peak_live.max(logged.files.len());

        let files: Vec<FileStat> = logged
            .files
            .iter()
            .map(|f| FileStat {
                name: f.path.clone(),
                bytes: f.size,
                rows: f.rows().expect("a row count"),
                covers_through: Lsn::new(position),
            })
            .collect();

        let plan = plan_tick(
            &[PartitionState {
                table: "public.readings".to_string(),
                partition: "all".to_string(),
                files: files.clone(),
                ticks_since_write: 0,
            }],
            &policy,
            &state,
        );

        if !plan.run.is_empty() {
            let report =
                execute_tick(&plan, &table_root, tick, WriterConfig::default()).expect("ticking");
            assert!(report.failed.is_empty(), "{:?}", report.failed);
            version += 1;
            commit_tick(
                &table_root,
                version,
                &report,
                i64::try_from(tick).expect("small"),
            )
            .expect("committing the tick");
            let mut carried = files;
            apply(&mut carried, &report);
        }

        // The claim: the loop holds the count, rather than falling steadily behind.
        let after = live_files(&table_root).expect("log");
        assert!(
            after.files.len() <= urgent,
            "at tick {tick} the partition held {} live files and the urgent threshold is \
             {urgent}; compaction is falling behind ingest",
            after.files.len()
        );
    }

    // And nothing was lost while all that was happening.
    let final_live = live_files(&table_root).expect("log");
    let rows: u64 = final_live
        .files
        .iter()
        .map(|f| f.rows().expect("a row count"))
        .sum();
    assert_eq!(rows, TICKS * TXNS_PER_TICK * ROWS_PER_TXN);

    // The fixture must actually have created pressure, or this proves nothing.
    assert!(
        peak_live > policy.compaction.routine_file_count,
        "capture never produced enough files to need compacting: peak was {peak_live}"
    );
    assert!(version > TICKS, "maintenance never committed anything");
}

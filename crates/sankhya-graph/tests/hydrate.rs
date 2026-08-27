//! Building an epoch from a scan, and refusing to build one that will not fit.
//!
//! The tests that matter here are the ones about what hydration *refuses*. A graph built
//! from a scan whose join produced nulls is quietly missing most of its edges and looks
//! entirely healthy; a graph that exceeds its memory budget takes the process down at query
//! time rather than failing as a build job.

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

use arrow_array::{Float64Array, Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use sankhya_graph::epoch::EpochId;
use sankhya_graph::hydrate::{Hydration, HydrationError, MemoryBudget};
use sankhya_graph::spec::{EdgeSpec, GraphSpec};
use sankhya_graph_algo::budget::Budget;
use sankhya_graph_algo::ids::EdgeMask;
use sankhya_graph_algo::traverse::reachable;
use std::sync::Arc;

/// A batch of transfers: who, to whom, when, how much.
fn transfers(rows: &[(Option<&str>, Option<&str>, i64, f64)]) -> RecordBatch {
    let schema = Arc::new(Schema::new(vec![
        Field::new("from_key", DataType::Utf8, true),
        Field::new("to_key", DataType::Utf8, true),
        Field::new("occurred_at", DataType::Int64, false),
        Field::new("amount", DataType::Float64, false),
    ]));
    let from: StringArray = rows.iter().map(|r| r.0).collect();
    let to: StringArray = rows.iter().map(|r| r.1).collect();
    let at = Int64Array::from(rows.iter().map(|r| r.2).collect::<Vec<_>>());
    let amount = Float64Array::from(rows.iter().map(|r| r.3).collect::<Vec<_>>());
    RecordBatch::try_new(
        schema,
        vec![Arc::new(from), Arc::new(to), Arc::new(at), Arc::new(amount)],
    )
    .expect("test fixture builds a valid batch")
}

fn spec() -> GraphSpec {
    GraphSpec::new().with(
        EdgeSpec::new("from_key", "to_key", "transfer")
            .between("party", "party")
            .valid_from("occurred_at")
            .weighted_by("amount"),
    )
}

#[test]
fn a_scan_becomes_a_graph_that_can_be_traversed() {
    let batch = transfers(&[
        (Some("alice"), Some("bob"), 100, 50.0),
        (Some("bob"), Some("carol"), 200, 40.0),
    ]);
    let mut hydration = Hydration::new(spec(), MemoryBudget::generous());
    hydration.absorb(&batch).expect("a well-formed batch");
    let epoch = hydration
        .finish(EpochId(1), 7, 1_000)
        .expect("within budget");

    assert_eq!(epoch.vertex_count(), 3, "alice, bob, carol");
    assert_eq!(epoch.edge_count(), 2);
    assert_eq!(epoch.snapshot(), 7, "the epoch says what it was built from");
    assert_eq!(epoch.lag(1_500), 500);

    let alice = epoch.vertex(b"alice").expect("alice was interned");
    let found = reachable(
        epoch.adjacency(),
        &[alice],
        &EdgeMask::all(epoch.adjacency().edge_type_count()),
        &Budget::generous(),
    );
    assert_eq!(found.found.len(), 3, "alice reaches everyone");
}

#[test]
fn a_row_with_a_null_endpoint_is_counted_rather_than_silently_dropped() {
    // The failure this guards: a scan whose join produced nulls hydrates a graph missing
    // most of its edges, and nothing about the result looks wrong. The count is the only
    // thing that reveals it.
    let batch = transfers(&[
        (Some("alice"), Some("bob"), 100, 50.0),
        (Some("alice"), None, 150, 10.0),
        (None, Some("carol"), 160, 10.0),
    ]);
    let mut hydration = Hydration::new(spec(), MemoryBudget::generous());
    hydration.absorb(&batch).expect("a well-formed batch");

    assert_eq!(hydration.rows_read(), 3);
    assert_eq!(
        hydration.rows_skipped(),
        2,
        "two rows had a null endpoint and contributed no edge"
    );
    let epoch = hydration.finish(EpochId(1), 1, 0).expect("within budget");
    assert_eq!(epoch.edge_count(), 1);
}

#[test]
fn a_graph_too_large_for_its_budget_is_refused_while_it_is_still_a_build_job() {
    // FR-GRAPH-19. An allocation failure at query time takes the process down, and in
    // managed mode the database with it. The refusal has to say how large it was getting
    // or nobody can size the budget.
    let rows: Vec<(Option<String>, Option<String>, i64, f64)> = (0..5_000)
        .map(|i| {
            (
                Some(format!("party-{i}")),
                Some(format!("party-{}", i + 1)),
                i64::from(i),
                1.0,
            )
        })
        .collect();
    let borrowed: Vec<(Option<&str>, Option<&str>, i64, f64)> = rows
        .iter()
        .map(|r| (r.0.as_deref(), r.1.as_deref(), r.2, r.3))
        .collect();
    let batch = transfers(&borrowed);

    let mut hydration = Hydration::new(spec(), MemoryBudget::of(8 * 1024));
    let outcome = hydration.absorb(&batch);

    let Err(HydrationError::OverBudget {
        budget_bytes,
        reached_bytes,
        edges_so_far,
        ..
    }) = outcome
    else {
        panic!("a 5,000-edge graph must not fit in 8 KiB: {outcome:?}");
    };
    assert_eq!(budget_bytes, 8 * 1024);
    assert!(reached_bytes > budget_bytes);
    assert!(edges_so_far > 0, "the error says how far it got");

    let message = HydrationError::OverBudget {
        budget_bytes,
        reached_bytes,
        edges_so_far,
        vertices_so_far: 1,
    }
    .to_string();
    assert!(message.contains("takes the process down"));
}

#[test]
fn an_epoch_publishes_what_it_costs_per_vertex_and_per_edge() {
    // The M4 exit criterion: memory per vertex and per edge published, so hardware can be
    // sized before it is bought. Measured from a real epoch rather than estimated from
    // type sizes, because the interner's key storage dominates and depends on key length.
    let rows: Vec<(Option<String>, Option<String>, i64, f64)> = (0..1_000)
        .map(|i| {
            (
                Some(format!("party-{i}")),
                Some(format!("party-{}", (i + 1) % 1_000)),
                i64::from(i),
                1.0,
            )
        })
        .collect();
    let borrowed: Vec<(Option<&str>, Option<&str>, i64, f64)> = rows
        .iter()
        .map(|r| (r.0.as_deref(), r.1.as_deref(), r.2, r.3))
        .collect();
    let mut hydration = Hydration::new(spec(), MemoryBudget::generous());
    hydration
        .absorb(&transfers(&borrowed))
        .expect("a well-formed batch");
    let epoch = hydration.finish(EpochId(1), 1, 0).expect("within budget");

    let footprint = epoch.footprint();
    assert!(footprint.total_bytes > 0);
    assert!(
        footprint.bytes_per_vertex > 0.0 && footprint.bytes_per_vertex < 10_000.0,
        "an implausible per-vertex figure means the measurement is wrong: {}",
        footprint.bytes_per_vertex
    );
    assert!(footprint.bytes_per_edge > 0.0);
}

#[test]
fn keys_arriving_in_two_batches_are_one_vertex_not_two() {
    // The interner has to persist across batches. A scan arrives in pieces, and a fresh
    // interner per batch would split every vertex that appears in more than one.
    let mut hydration = Hydration::new(spec(), MemoryBudget::generous());
    hydration
        .absorb(&transfers(&[(Some("alice"), Some("bob"), 100, 1.0)]))
        .expect("first batch");
    hydration
        .absorb(&transfers(&[(Some("bob"), Some("carol"), 200, 1.0)]))
        .expect("second batch");
    let epoch = hydration.finish(EpochId(1), 1, 0).expect("within budget");

    assert_eq!(epoch.vertex_count(), 3, "bob appears in both batches, once");
    assert_eq!(epoch.edge_count(), 2);
}

#[test]
fn an_epoch_says_whether_a_temporal_query_over_it_means_anything() {
    // FR-GRAPH-03 forbids offering static traversal for flow analysis. A graph mixing timed
    // and untimed edges answers a time-respecting query by routing freely through the
    // untimed ones, so the caller has to be able to find out.
    let timed = spec();
    assert!(!timed.has_untimed_edges());

    // Same endpoints and same vertex types — only the edge kind differs, and this one
    // carries no validity column.
    let mixed = timed
        .clone()
        .with(EdgeSpec::new("from_key", "to_key", "knows").between("party", "party"));
    assert!(
        mixed.has_untimed_edges(),
        "one untimed edge kind is enough to make the guarantee not hold"
    );

    let mut hydration = Hydration::new(mixed, MemoryBudget::generous());
    hydration
        .absorb(&transfers(&[(Some("a"), Some("b"), 1, 1.0)]))
        .expect("batch");
    let epoch = hydration.finish(EpochId(1), 1, 0).expect("within budget");
    assert!(!epoch.is_fully_temporal());
}

#[test]
fn a_batch_that_matches_no_spec_contributes_nothing_and_is_not_an_error() {
    // A scan may deliver several tables and each spec reads the ones it recognises.
    let schema = Arc::new(Schema::new(vec![Field::new(
        "unrelated",
        DataType::Int64,
        false,
    )]));
    let batch = RecordBatch::try_new(schema, vec![Arc::new(Int64Array::from(vec![1, 2, 3]))])
        .expect("valid batch");

    let mut hydration = Hydration::new(spec(), MemoryBudget::generous());
    hydration.absorb(&batch).expect("not an error");
    let epoch = hydration.finish(EpochId(1), 1, 0).expect("within budget");
    assert_eq!(epoch.edge_count(), 0);
}

#[test]
fn type_identifiers_come_from_sorted_names_not_encounter_order() {
    // Two hydrations of the same spec must assign the same identifiers even if the data
    // arrives differently, or the incremental-equals-full property fails for a reason that
    // is not a bug.
    let s = GraphSpec::new()
        .with(EdgeSpec::new("a", "b", "zebra"))
        .with(EdgeSpec::new("c", "d", "alpha"));
    let ids = s.edge_type_ids();
    assert_eq!(ids.get("alpha"), Some(&0), "sorted, not declaration order");
    assert_eq!(ids.get("zebra"), Some(&1));
}

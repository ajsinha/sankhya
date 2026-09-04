//! Incremental application and full rehydration produce **identical** graphs.
//!
//! `FR-GRAPH-09` calls this the single most valuable test in the graph tier, and the reason
//! is that its failures are invisible. An overlay that drops an edge produces a graph that
//! is smaller than it should be and wrong in no way anyone can see: every query still
//! returns a well-formed answer, just a slightly emptier one. Nothing crashes, no bound is
//! exceeded, and the only symptom is a conclusion that quietly understates a connection.
//!
//! So the comparison is edge by edge rather than by counts. Two graphs with the same vertex
//! and edge counts can still differ in which vertices those edges join, and a count check
//! passes for exactly the defect this test exists to catch.

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
use proptest::prelude::*;
use sankhya_graph::epoch::{Epoch, EpochId, EpochSlot};
use sankhya_graph::hydrate::{Hydration, MemoryBudget};
use sankhya_graph::overlay::{Overlay, RebuildThreshold};
use sankhya_graph::spec::{EdgeSpec, GraphSpec};
use sankhya_graph_algo::ids::{EdgeType, VertexId};
use std::sync::Arc;

fn spec() -> GraphSpec {
    GraphSpec::new().with(
        EdgeSpec::new("from_key", "to_key", "transfer")
            .between("party", "party")
            .valid_from("occurred_at")
            .weighted_by("amount"),
    )
}

fn batch(rows: &[(String, String, i64, f64)]) -> RecordBatch {
    let schema = Arc::new(Schema::new(vec![
        Field::new("from_key", DataType::Utf8, true),
        Field::new("to_key", DataType::Utf8, true),
        Field::new("occurred_at", DataType::Int64, false),
        Field::new("amount", DataType::Float64, false),
    ]));
    let from: StringArray = rows.iter().map(|r| Some(r.0.as_str())).collect();
    let to: StringArray = rows.iter().map(|r| Some(r.1.as_str())).collect();
    let at = Int64Array::from(rows.iter().map(|r| r.2).collect::<Vec<_>>());
    let amount = Float64Array::from(rows.iter().map(|r| r.3).collect::<Vec<_>>());
    RecordBatch::try_new(
        schema,
        vec![Arc::new(from), Arc::new(to), Arc::new(at), Arc::new(amount)],
    )
    .expect("fixture builds a valid batch")
}

fn hydrate_all(batches: &[RecordBatch]) -> Epoch {
    let mut hydration = Hydration::new(spec(), MemoryBudget::generous());
    for b in batches {
        hydration.absorb(b).expect("well-formed batch");
    }
    hydration.finish(EpochId(1), 1, 0).expect("within budget")
}

/// Every edge of a graph, expressed in external keys so two epochs are comparable.
///
/// Dense ids are per-epoch and mean nothing across two, so comparing them would compare
/// interning order rather than structure. The external keys are what the source data says,
/// and they are the same in both graphs or the graphs genuinely differ.
fn edges_by_key(epoch: &Epoch) -> Vec<(String, String, u16, i64, i64, u64)> {
    let mut out = Vec::new();
    for index in 0..epoch.vertex_count() {
        let source = VertexId(u32::try_from(index).unwrap_or(u32::MAX));
        let Some(source_key) = epoch.key(source) else {
            continue;
        };
        let source_key = String::from_utf8_lossy(source_key).into_owned();
        for kind in 0..epoch.adjacency().edge_type_count() {
            for arc in epoch.adjacency().out_edges(source, EdgeType(kind)).iter() {
                let Some(target_key) = epoch.key(arc.target) else {
                    continue;
                };
                out.push((
                    source_key.clone(),
                    String::from_utf8_lossy(target_key).into_owned(),
                    kind,
                    arc.validity.from,
                    arc.validity.until,
                    // Bit pattern, so the comparison is exact and needs no float equality.
                    arc.weight.to_bits(),
                ));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn a_rebuild_through_the_overlay_equals_hydrating_everything_at_once() {
    let base_rows = vec![
        ("alice".to_string(), "bob".to_string(), 100, 50.0),
        ("bob".to_string(), "carol".to_string(), 200, 40.0),
    ];
    let new_rows = vec![
        ("carol".to_string(), "dave".to_string(), 300, 30.0),
        ("alice".to_string(), "dave".to_string(), 350, 5.0),
    ];
    let base_batch = batch(&base_rows);
    let new_batch = batch(&new_rows);

    let full = hydrate_all(&[base_batch.clone(), new_batch.clone()]);

    let mut overlay = Overlay::new(
        spec(),
        MemoryBudget::generous(),
        RebuildThreshold::default(),
    );
    overlay.apply(&new_batch).expect("valid changes");
    let incremental = overlay
        .rebuild(&[base_batch], EpochId(1), 1, 0)
        .expect("within budget");

    assert_eq!(
        edges_by_key(&incremental),
        edges_by_key(&full),
        "an incrementally-built graph must be indistinguishable from a fully rehydrated one"
    );
    assert_eq!(incremental.vertex_count(), full.vertex_count());
}

#[test]
fn an_overlay_asks_for_a_rebuild_once_it_stops_being_cheaper_than_one() {
    let base = hydrate_all(&[batch(&[
        ("a".to_string(), "b".to_string(), 1, 1.0),
        ("b".to_string(), "c".to_string(), 2, 1.0),
        ("c".to_string(), "d".to_string(), 3, 1.0),
        ("d".to_string(), "e".to_string(), 4, 1.0),
        ("e".to_string(), "f".to_string(), 5, 1.0),
    ])]);
    assert_eq!(base.edge_count(), 5);

    let mut overlay = Overlay::new(
        spec(),
        MemoryBudget::generous(),
        RebuildThreshold::default(),
    );
    assert!(!overlay.needs_rebuild(&base), "an empty overlay is cheap");

    // One edge against a base of five is a fifth, which is the default threshold.
    overlay
        .apply(&batch(&[("f".to_string(), "g".to_string(), 6, 1.0)]))
        .expect("valid");
    assert_eq!(overlay.pending_edges(), 1);
    assert!(
        overlay.needs_rebuild(&base),
        "at a fifth of the base, reading two structures costs more than the rebuild saves"
    );
}

#[test]
fn a_rebuild_clears_what_was_pending() {
    let base_batch = batch(&[("a".to_string(), "b".to_string(), 1, 1.0)]);
    let mut overlay = Overlay::new(
        spec(),
        MemoryBudget::generous(),
        RebuildThreshold::default(),
    );
    overlay
        .apply(&batch(&[("b".to_string(), "c".to_string(), 2, 1.0)]))
        .expect("valid");
    assert_eq!(overlay.pending_batches(), 1);

    let rebuilt = overlay
        .rebuild(&[base_batch], EpochId(2), 2, 0)
        .expect("within budget");
    assert_eq!(rebuilt.edge_count(), 2);
    assert_eq!(
        overlay.pending_batches(),
        0,
        "replayed rows are no longer pending"
    );
    assert_eq!(overlay.pending_edges(), 0);
}

#[test]
fn publishing_a_new_epoch_does_not_disturb_a_reader_holding_the_old_one() {
    // The property that lets a rebuild run without blocking queries. A reader that has
    // cloned the handle owns its epoch until it drops it, and a swap during that time
    // leaves the old one alive rather than pulling it away.
    let first = Arc::new(hydrate_all(&[batch(&[(
        "a".to_string(),
        "b".to_string(),
        1,
        1.0,
    )])]));
    let slot = EpochSlot::holding(Arc::clone(&first));

    let reader = slot.current().expect("something is published");
    assert_eq!(reader.edge_count(), 1);

    let second = Arc::new(hydrate_all(&[batch(&[
        ("a".to_string(), "b".to_string(), 1, 1.0),
        ("b".to_string(), "c".to_string(), 2, 1.0),
    ])]));
    let displaced = slot.publish(Arc::clone(&second));

    assert_eq!(
        reader.edge_count(),
        1,
        "the in-flight reader still sees the graph it started with"
    );
    assert_eq!(slot.current().map(|e| e.edge_count()), Some(2));
    assert_eq!(displaced.map(|e| e.id()), Some(first.id()));
}

#[test]
fn a_query_before_anything_is_hydrated_gets_a_named_refusal() {
    // FR-GRAPH-20. The caller's correct response — wait and retry — differs from their
    // response to a real failure, so a generic error is the wrong answer and a hang is
    // worse.
    let slot = EpochSlot::empty();
    assert!(slot.is_empty());

    let Err(not_hydrated) = slot.require() else {
        panic!("an empty slot must refuse rather than return something");
    };
    assert!(not_hydrated.to_string().contains("wait-and-retry"));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// However the same rows are split into batches, the graph is the same.
    ///
    /// The generator deliberately produces repeated keys and repeated instants, because
    /// both are where ordering bugs hide: a vertex seen in two batches must intern once,
    /// and two edges at the same instant must still sort totally or the arrays differ
    /// between runs of identical data.
    #[test]
    fn any_split_of_the_same_rows_builds_the_same_graph(
        rows in prop::collection::vec(
            (0u8..6, 0u8..6, 0i64..5, 1u32..4),
            1..40,
        ),
        split in 1usize..8,
    ) {
        let rows: Vec<(String, String, i64, f64)> = rows
            .into_iter()
            .map(|(a, b, t, w)| {
                (format!("v{a}"), format!("v{b}"), t, f64::from(w))
            })
            .collect();

        let whole = hydrate_all(&[batch(&rows)]);

        let pieces: Vec<RecordBatch> = rows
            .chunks(split.max(1))
            .filter(|c| !c.is_empty())
            .map(batch)
            .collect();
        let assembled = hydrate_all(&pieces);

        prop_assert_eq!(edges_by_key(&assembled), edges_by_key(&whole));
        prop_assert_eq!(assembled.vertex_count(), whole.vertex_count());
    }

    /// Applying rows through the overlay equals hydrating them all at once.
    #[test]
    fn incremental_application_equals_full_rehydration(
        base in prop::collection::vec((0u8..5, 0u8..5, 0i64..4, 1u32..3), 1..20),
        added in prop::collection::vec((0u8..5, 0u8..5, 0i64..4, 1u32..3), 1..20),
    ) {
        let to_rows = |v: Vec<(u8, u8, i64, u32)>| -> Vec<(String, String, i64, f64)> {
            v.into_iter()
                .map(|(a, b, t, w)| (format!("v{a}"), format!("v{b}"), t, f64::from(w)))
                .collect()
        };
        let base_rows = to_rows(base);
        let added_rows = to_rows(added);
        let base_batch = batch(&base_rows);
        let added_batch = batch(&added_rows);

        let full = hydrate_all(&[base_batch.clone(), added_batch.clone()]);

        let mut overlay =
            Overlay::new(spec(), MemoryBudget::generous(), RebuildThreshold::default());
        overlay.apply(&added_batch).map_err(|e| TestCaseError::fail(e.to_string()))?;
        let incremental = overlay
            .rebuild(&[base_batch], EpochId(1), 1, 0)
            .map_err(|e| TestCaseError::fail(e.to_string()))?;

        prop_assert_eq!(edges_by_key(&incremental), edges_by_key(&full));
    }
}

/// The absolute edge bound rebuilds even when the fraction says not to.
///
/// # Why this was missing
///
/// A mutation that made `max_edges` unreachable survived the suite. The one test of
/// `needs_rebuild` uses a base of five edges and one pending, which is a fifth --- the
/// *fraction* path. Nothing exercised the absolute bound at all, so it could have been any
/// number, or absent, and every test would still pass.
///
/// The two thresholds guard different failures and the second is the one that matters at
/// scale. On a graph with a hundred million edges, a fifth is twenty million edges held in a
/// second structure that every query reads and merges. `fraction_of_base` never fires there;
/// the absolute bound is the only thing that does, which is exactly why its own doc comment
/// says it exists "on a graph whose base is enormous".
#[test]
fn the_absolute_edge_bound_rebuilds_where_the_fraction_never_would() {
    let base = hydrate_all(&[batch(&[
        ("a".to_string(), "b".to_string(), 1, 1.0),
        ("b".to_string(), "c".to_string(), 2, 1.0),
        ("c".to_string(), "d".to_string(), 3, 1.0),
        ("d".to_string(), "e".to_string(), 4, 1.0),
        ("e".to_string(), "f".to_string(), 5, 1.0),
    ])]);

    // A fraction that can never trigger, so only the absolute bound can.
    let threshold = RebuildThreshold {
        fraction_of_base: f64::INFINITY,
        max_edges: 2,
    };
    let mut overlay = Overlay::new(spec(), MemoryBudget::generous(), threshold);

    overlay
        .apply(&batch(&[("f".to_string(), "g".to_string(), 6, 1.0)]))
        .expect("valid");
    assert_eq!(overlay.pending_edges(), 1);
    assert!(
        !overlay.needs_rebuild(&base),
        "one pending edge is below an absolute bound of two"
    );

    overlay
        .apply(&batch(&[("g".to_string(), "h".to_string(), 7, 1.0)]))
        .expect("valid");
    assert_eq!(overlay.pending_edges(), 2);
    assert!(
        overlay.needs_rebuild(&base),
        "the absolute bound is reached and the fraction can never fire, so nothing else \
         would ever call for a rebuild"
    );
}

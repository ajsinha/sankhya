//! A traversal cannot leave its tenant's graph, because there is nothing to leave into.
//!
//! `FR-GRAPH-11` is unusually strong about this: *a traversal leaving the tenant's
//! identifier space SHALL be an invariant violation, not a filtered result*. Traversing a
//! shared graph and filtering afterwards is explicitly forbidden, and the reason is worth
//! stating because it is not obvious --- filtering after the fact leaks topology through
//! **timing** and through **path structure**. A traversal that takes longer when the other
//! tenant's subgraph is large has disclosed its size; one that returns a three-hop path
//! where a two-hop one exists has disclosed that the two-hop route passes through somebody
//! else.
//!
//! So each tenant gets its own epoch, hydrated from its own rows. The other tenant's
//! vertices are not filtered out of the traversal --- they were never interned, so there is
//! no identifier that could reach them.

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
use sankhya_graph::epoch::{Epoch, EpochId, EpochSlot};
use sankhya_graph::hydrate::{Hydration, MemoryBudget};
use sankhya_graph::spec::{EdgeSpec, GraphSpec};
use sankhya_graph_algo::budget::Budget;
use sankhya_graph_algo::ids::EdgeMask;
use sankhya_graph_algo::traverse::reachable;
use std::sync::Arc;

fn spec() -> GraphSpec {
    GraphSpec::new().with(
        EdgeSpec::new("from_key", "to_key", "transfer")
            .between("party", "party")
            .valid_from("occurred_at")
            .weighted_by("amount"),
    )
}

fn edges(rows: &[(&str, &str)]) -> RecordBatch {
    let schema = Arc::new(Schema::new(vec![
        Field::new("from_key", DataType::Utf8, true),
        Field::new("to_key", DataType::Utf8, true),
        Field::new("occurred_at", DataType::Int64, false),
        Field::new("amount", DataType::Float64, false),
    ]));
    let from: StringArray = rows.iter().map(|r| Some(r.0)).collect();
    let to: StringArray = rows.iter().map(|r| Some(r.1)).collect();
    let at = Int64Array::from(vec![1i64; rows.len()]);
    let amount = Float64Array::from(vec![1.0f64; rows.len()]);
    RecordBatch::try_new(
        schema,
        vec![Arc::new(from), Arc::new(to), Arc::new(at), Arc::new(amount)],
    )
    .expect("a valid batch")
}

fn hydrate(rows: &[(&str, &str)], id: u64) -> Epoch {
    let mut hydration = Hydration::new(spec(), MemoryBudget::generous());
    hydration.absorb(&edges(rows)).expect("well-formed");
    hydration.finish(EpochId(id), id, 0).expect("within budget")
}

/// Acme's rows, and another tenant's, kept apart at hydration.
fn acme() -> Epoch {
    hydrate(&[("a1", "a2"), ("a2", "a3")], 1)
}

fn other() -> Epoch {
    hydrate(&[("b1", "b2"), ("b2", "a1")], 2)
}

#[test]
fn one_tenants_vertex_has_no_identifier_in_another_tenants_epoch() {
    // Not filtered out — never interned. There is no identifier that could reach it, which
    // is a stronger statement than "the traversal excludes it".
    let acme = acme();
    let other = other();

    assert!(acme.vertex(b"a1").is_some());
    assert!(
        acme.vertex(b"b1").is_none(),
        "another tenant's vertex must not exist in this epoch at all"
    );
    assert!(other.vertex(b"b1").is_some());
}

#[test]
fn a_traversal_cannot_reach_across_epochs_even_when_the_source_rows_connect() {
    // The other tenant's data *does* contain an edge into acme's key space — `b2 -> a1`.
    // In a shared graph that edge would be traversable and would then have to be filtered
    // out. Here acme's epoch simply does not contain it.
    let acme = acme();
    let a1 = acme.vertex(b"a1").expect("acme's own vertex");

    let found = reachable(
        acme.adjacency(),
        &[a1],
        &EdgeMask::all(acme.adjacency().edge_type_count()),
        &Budget::generous(),
    );

    let keys: Vec<String> = found
        .found
        .iter()
        .filter_map(|r| acme.key(r.vertex))
        .map(|k| String::from_utf8_lossy(k).into_owned())
        .collect();
    assert_eq!(keys.len(), 3, "a1, a2, a3");
    assert!(
        keys.iter().all(|k| k.starts_with('a')),
        "the traversal reached a foreign key: {keys:?}"
    );
}

#[test]
fn the_reverse_direction_is_closed_too() {
    // The edge in the fixture points *into* acme's key space, so the interesting direction
    // is the other one: from the tenant that owns the edge. It reaches its own copy of the
    // key, which is a different vertex in a different epoch, not acme's.
    let other = other();
    let b1 = other.vertex(b"b1").expect("its own vertex");
    let found = reachable(
        other.adjacency(),
        &[b1],
        &EdgeMask::all(other.adjacency().edge_type_count()),
        &Budget::generous(),
    );
    assert_eq!(found.found.len(), 3, "b1, b2, and its own 'a1'");

    // And that 'a1' is emphatically not acme's. A dense identifier is an index into one
    // epoch's arrays and means nothing outside it: here the *same key* has different
    // indices in the two epochs, and the *same index* names different keys. Either way,
    // carrying an identifier across is meaningless — which is why the interner is owned by
    // the epoch rather than shared.
    let acme = acme();
    let in_acme = acme.vertex(b"a1").expect("acme has it");
    let in_other = other.vertex(b"a1").expect("the other tenant has its own");
    assert_ne!(
        in_acme.0, in_other.0,
        "the same key indexes differently in each epoch"
    );
    assert_ne!(
        acme.key(in_other).map(<[u8]>::to_vec),
        other.key(in_other).map(<[u8]>::to_vec),
        "the same index names a different vertex in each epoch"
    );
}

#[test]
fn each_tenant_gets_its_own_slot_and_publishing_one_does_not_touch_the_other() {
    let acme_slot = EpochSlot::holding(Arc::new(acme()));
    let other_slot = EpochSlot::holding(Arc::new(other()));

    assert_eq!(acme_slot.current().map(|e| e.snapshot()), Some(1));
    assert_eq!(other_slot.current().map(|e| e.snapshot()), Some(2));

    // Rebuild one.
    other_slot.publish(Arc::new(hydrate(&[("b1", "b9")], 7)));
    assert_eq!(
        acme_slot.current().map(|e| e.snapshot()),
        Some(1),
        "the other tenant's rebuild must not disturb this one"
    );
    assert_eq!(other_slot.current().map(|e| e.snapshot()), Some(7));
}

#[test]
fn an_epoch_reports_a_footprint_that_can_be_charged_to_its_tenant() {
    // Per-tenant memory accounting needs a number to charge. An epoch that could not say
    // what it costs would be exempt from every quota.
    let epoch = acme();
    let footprint = epoch.footprint();
    assert!(footprint.total_bytes > 0);
    assert_eq!(footprint.total_bytes, epoch.heap_bytes());
}

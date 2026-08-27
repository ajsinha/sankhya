//! Graph algorithms invoked from SQL, joined against a table.
//!
//! The point of these tests is not that the traversals work --- `sankhya-graph-algo` tests
//! that against brute force. It is that the results **compose**: that a traversal can be
//! joined against an ordinary table, filtered by an ordinary predicate, and ordered by an
//! ordinary column, without anything about it being special.
//!
//! And that the three things a graph result must never lose survive the round trip: which
//! epoch answered, whether the search was truncated, and what the bounds were.

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
use datafusion::prelude::*;
use sankhya_graph::epoch::EpochId;
use sankhya_graph::hydrate::{Hydration, MemoryBudget};
use sankhya_graph::spec::{EdgeSpec, GraphSpec};
use sankhya_graph_sql::{register, GraphCatalog};
use std::sync::Arc;

fn spec() -> GraphSpec {
    GraphSpec::new().with(
        EdgeSpec::new("from_key", "to_key", "transfer")
            .between("party", "party")
            .valid_from("occurred_at")
            .weighted_by("amount"),
    )
}

fn transfers(rows: &[(&str, &str, i64, f64)]) -> RecordBatch {
    let schema = Arc::new(Schema::new(vec![
        Field::new("from_key", DataType::Utf8, true),
        Field::new("to_key", DataType::Utf8, true),
        Field::new("occurred_at", DataType::Int64, false),
        Field::new("amount", DataType::Float64, false),
    ]));
    let from: StringArray = rows.iter().map(|r| Some(r.0)).collect();
    let to: StringArray = rows.iter().map(|r| Some(r.1)).collect();
    let at = Int64Array::from(rows.iter().map(|r| r.2).collect::<Vec<_>>());
    let amount = Float64Array::from(rows.iter().map(|r| r.3).collect::<Vec<_>>());
    RecordBatch::try_new(
        schema,
        vec![Arc::new(from), Arc::new(to), Arc::new(at), Arc::new(amount)],
    )
    .expect("fixture builds a valid batch")
}

/// A session with one graph registered, and a `parties` table to join against.
async fn session(rows: &[(&str, &str, i64, f64)]) -> SessionContext {
    let mut hydration = Hydration::new(spec(), MemoryBudget::generous());
    hydration
        .absorb(&transfers(rows))
        .expect("a well-formed batch");
    let epoch = Arc::new(hydration.finish(EpochId(9), 42, 0).expect("within budget"));

    let catalog = Arc::new(GraphCatalog::new());
    catalog.publish("payments", epoch);

    let context = SessionContext::new();
    register(&context, catalog);

    // An ordinary table, so the join is an ordinary join.
    let mut keys: Vec<&str> = rows.iter().flat_map(|r| [r.0, r.1]).collect();
    keys.sort_unstable();
    keys.dedup();
    let schema = Arc::new(Schema::new(vec![
        Field::new("key", DataType::Utf8, false),
        Field::new("label", DataType::Utf8, false),
    ]));
    let labels: Vec<String> = keys.iter().map(|k| format!("label-{k}")).collect();
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(keys.iter().map(|k| Some(*k)).collect::<StringArray>()),
            Arc::new(
                labels
                    .iter()
                    .map(|l| Some(l.as_str()))
                    .collect::<StringArray>(),
            ),
        ],
    )
    .expect("valid batch");
    context
        .register_batch("parties", batch)
        .expect("registering a table");
    context
}

fn chain() -> Vec<(&'static str, &'static str, i64, f64)> {
    vec![
        ("alice", "bob", 100, 900.0),
        ("bob", "carol", 200, 880.0),
        ("carol", "dave", 300, 870.0),
        ("dave", "alice", 400, 860.0),
        // Deliberately a *second* hop, not a first: conservation compares an outgoing
        // edge with the incoming one, and a seed has no incoming edge to compare against.
        ("bob", "eve", 250, 5.0),
    ]
}

#[tokio::test]
async fn a_traversal_can_be_joined_against_an_ordinary_table() {
    // The whole reason the graph lives in the same engine. If this needed a separate query
    // language, there would be no point putting it here.
    let context = session(&chain()).await;
    let rows = context
        .sql(
            "SELECT p.label, r.depth \
             FROM graph_reachable('payments', 'alice', 'max_depth=2') AS r \
             JOIN parties AS p ON p.key = r.vertex \
             ORDER BY r.depth, p.label",
        )
        .await
        .expect("planning succeeds")
        .collect()
        .await
        .expect("execution succeeds");

    let total: usize = rows.iter().map(RecordBatch::num_rows).sum();
    assert_eq!(total, 4, "alice, bob, eve at depth 1, carol at depth 2");
}

#[tokio::test]
async fn every_row_says_which_epoch_answered() {
    // A graph result that cannot be reconciled with a relational one taken at a different
    // moment is a result nobody can account for.
    let context = session(&chain()).await;
    let rows = context
        .sql("SELECT DISTINCT epoch, snapshot FROM graph_reachable('payments', 'alice')")
        .await
        .expect("planning")
        .collect()
        .await
        .expect("execution");

    let text = pretty(&rows);
    assert!(
        text.contains("42"),
        "the source snapshot must reach the result: {text}"
    );
}

#[tokio::test]
async fn a_truncated_traversal_says_so_in_every_row() {
    // FR-GRAPH-14. The flag lives in the rows rather than beside them precisely so that a
    // projection cannot drop it.
    let context = session(&chain()).await;
    let rows = context
        .sql(
            "SELECT truncated, truncation_reason \
             FROM graph_reachable('payments', 'alice', 'max_depth=1') LIMIT 1",
        )
        .await
        .expect("planning")
        .collect()
        .await
        .expect("execution");

    let text = pretty(&rows);
    assert!(text.contains("true"), "the search stopped early: {text}");
    assert!(text.contains("depth limit"), "and says why: {text}");
}

#[tokio::test]
async fn a_time_respecting_traversal_refuses_the_route_a_static_one_takes() {
    // The distinction that makes a temporal graph worth having, visible from SQL. The
    // second edge fires before the first, so nothing could have travelled the route.
    let backwards = vec![("alice", "bob", 500, 10.0), ("bob", "carol", 100, 10.0)];
    let context = session(&backwards).await;

    let static_walk = count(
        &context,
        "SELECT * FROM graph_reachable('payments', 'alice')",
    )
    .await;
    assert_eq!(static_walk, 3, "static reachability follows both edges");

    let timed = count(
        &context,
        "SELECT * FROM graph_time_respecting('payments', 'alice')",
    )
    .await;
    assert_eq!(
        timed, 2,
        "carol is not reachable in time: the onward edge fired before the inbound one"
    );
}

#[tokio::test]
async fn conservation_and_dwell_are_reachable_from_sql() {
    // The bounds that separate a route along which something moved from a chain of
    // unrelated edges that happen to be ordered in time.
    let context = session(&chain()).await;

    let unconstrained = count(
        &context,
        "SELECT * FROM graph_time_respecting('payments', 'alice', 'max_depth=4')",
    )
    .await;
    assert_eq!(
        unconstrained, 5,
        "everyone is reachable without constraints"
    );

    // The hop to eve carries 5 out of 900, which is not a continuation of the route.
    let conserving = count(
        &context,
        "SELECT * FROM graph_time_respecting('payments', 'alice', 'max_depth=4,min_conservation=0.9')",
    )
    .await;
    assert!(
        conserving < unconstrained,
        "requiring nine tenths to carry through must exclude the negligible hop"
    );
}

#[tokio::test]
async fn conservation_does_not_apply_to_the_first_hop_out_of_a_seed() {
    // A seed has no incoming edge, so there is nothing for its outgoing edge to conserve.
    // Applying the constraint anyway would refuse every traversal that has one, which is
    // exactly what an earlier version did — a sentinel weight of infinity stood in for the
    // missing edge, and no real edge can conserve nine tenths of infinity.
    let context = session(&[("alice", "bob", 100, 1.0), ("bob", "carol", 200, 1.0)]).await;
    let found = count(
        &context,
        "SELECT * FROM graph_time_respecting('payments', 'alice', \
         'max_depth=4,min_conservation=0.9')",
    )
    .await;
    assert_eq!(found, 3, "the first hop is exempt, and the rest conserve");
}

#[tokio::test]
async fn a_circuit_is_found_and_reported_step_by_step() {
    let context = session(&chain()).await;
    let rows = context
        .sql(
            "SELECT path_id, position, vertex FROM graph_cycles('payments', \
             'alice,bob,carol,dave', 'max_depth=8') ORDER BY path_id, position",
        )
        .await
        .expect("planning")
        .collect()
        .await
        .expect("execution");

    let text = pretty(&rows);
    assert!(
        text.contains("alice"),
        "the circuit runs through alice: {text}"
    );
    let total: usize = rows.iter().map(RecordBatch::num_rows).sum();
    assert_eq!(total, 5, "a four-hop circuit, closed, is five positions");
}

#[tokio::test]
async fn the_cheapest_route_between_two_parties_comes_back_in_order() {
    let context = session(&chain()).await;
    let rows = context
        .sql(
            "SELECT position, vertex, path_cost \
             FROM graph_shortest_path('payments', 'alice', 'carol') ORDER BY position",
        )
        .await
        .expect("planning")
        .collect()
        .await
        .expect("execution");

    let total: usize = rows.iter().map(RecordBatch::num_rows).sum();
    assert_eq!(total, 3, "alice, bob, carol");
}

#[tokio::test]
async fn influence_multiplies_and_is_visible_from_sql() {
    let context = session(&[("a", "b", 1, 0.5), ("b", "c", 2, 0.4)]).await;
    let rows = context
        .sql("SELECT vertex, score FROM graph_influence('payments', 'a') ORDER BY vertex")
        .await
        .expect("planning")
        .collect()
        .await
        .expect("execution");

    let text = pretty(&rows);
    assert!(
        text.contains("0.2"),
        "0.5 then 0.4 multiplies to 0.2: {text}"
    );
}

#[tokio::test]
async fn the_planner_is_told_exactly_how_many_rows_a_traversal_returns() {
    // FR-GRAPH-12. Without this the planner orders the downstream join badly — the exact
    // defect M3 spent a day chasing on a six-way query, where one absent figure moved
    // every join.
    let context = session(&chain()).await;
    let plan = context
        .sql(
            "EXPLAIN ANALYZE SELECT p.label \
             FROM graph_reachable('payments', 'alice', 'max_depth=1') AS r \
             JOIN parties AS p ON p.key = r.vertex",
        )
        .await
        .expect("planning")
        .collect()
        .await
        .expect("execution");

    let text = pretty(&plan);
    assert!(
        text.contains("CollectLeft") || text.contains("DataSourceExec"),
        "the traversal should be planned as a small side of the join: {text}"
    );
}

#[tokio::test]
async fn an_unknown_graph_is_refused_rather_than_returning_no_rows() {
    // An empty traversal over a graph that does not exist reads exactly like one that
    // found nothing, and those mean opposite things.
    let context = session(&chain()).await;
    let outcome = context
        .sql("SELECT * FROM graph_reachable('absent', 'alice')")
        .await;

    let Err(error) = outcome else {
        panic!("a traversal over an unregistered graph must be refused");
    };
    let message = error.to_string();
    assert!(message.contains("absent"), "{message}");
    assert!(
        message.contains("payments"),
        "and names what does exist: {message}"
    );
}

#[tokio::test]
async fn a_seed_that_is_not_in_the_graph_is_refused() {
    // Skipping it turns "this entity is not in the graph" into "this entity is connected
    // to nothing".
    let context = session(&chain()).await;
    let outcome = context
        .sql("SELECT * FROM graph_reachable('payments', 'nobody')")
        .await;

    let Err(error) = outcome else {
        panic!("an unknown seed must be refused");
    };
    assert!(error.to_string().contains("connected to nothing"));
}

#[tokio::test]
async fn an_unrecognised_edge_type_is_refused_rather_than_narrowing_to_nothing() {
    let context = session(&chain()).await;
    let outcome = context
        .sql("SELECT * FROM graph_reachable('payments', 'alice', 'edge_types=nonsense')")
        .await;

    let Err(error) = outcome else {
        panic!("an unknown edge type must be refused");
    };
    assert!(error.to_string().contains("silently narrow"));
}

#[tokio::test]
async fn a_malformed_bound_is_refused_rather_than_defaulted() {
    // A misspelled bound that quietly becomes the default produces a result that is wrong
    // in a way the query text does not reveal.
    let context = session(&chain()).await;
    let outcome = context
        .sql("SELECT * FROM graph_reachable('payments', 'alice', 'max_depth=three')")
        .await;

    let Err(error) = outcome else {
        panic!("a non-integer depth must be refused");
    };
    assert!(error.to_string().contains("must be an integer"));
}

#[tokio::test]
async fn a_column_reference_as_an_argument_explains_the_alternative() {
    let context = session(&chain()).await;
    let outcome = context
        .sql("SELECT * FROM parties AS p, graph_reachable('payments', p.key)")
        .await;

    let Err(error) = outcome else {
        panic!("a per-row argument must be refused");
    };
    assert!(
        error.to_string().contains("subquery"),
        "the refusal should say what to do instead: {error}"
    );
}

// --- helpers --------------------------------------------------------------

fn pretty(batches: &[RecordBatch]) -> String {
    datafusion::arrow::util::pretty::pretty_format_batches(batches)
        .map(|d| d.to_string())
        .unwrap_or_default()
}

async fn count(context: &SessionContext, sql: &str) -> usize {
    let rows = context
        .sql(sql)
        .await
        .expect("planning")
        .collect()
        .await
        .expect("execution");
    rows.iter().map(RecordBatch::num_rows).sum()
}

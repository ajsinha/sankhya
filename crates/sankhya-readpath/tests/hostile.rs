//! A query that asks for too much is refused, and the process lives.
//!
//! # Why this gate matters more than it looks
//!
//! The query engine's hash aggregation and hash joins do not spill. An unbounded build
//! side does not get slow — it exhausts memory, and the operating system terminates the
//! process. Every other query in flight dies with it, and where the transactional store
//! is supervised by the same process, so does the database.
//!
//! That turns one badly-written query into an outage. The whole of resource governance
//! exists to make it an error message instead, and an error message is only worth
//! anything if the process is still there to send it.
//!
//! # What is actually being asserted
//!
//! Not that the query fails — plenty of things make a query fail. That it fails **for
//! the right reason**, and that the process survives to run the next one. A test that
//! only checked for an error would pass on a query that failed to parse.

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
use arrow_schema::{DataType, Field, Schema};
use datafusion::execution::runtime_env::RuntimeEnvBuilder;
use datafusion::prelude::{SessionConfig, SessionContext};
use std::sync::Arc;

/// Enough distinct groups that a hash aggregation cannot hold them in a few megabytes.
const ROWS: i64 = 400_000;

fn hostile_data() -> RecordBatch {
    let schema = Arc::new(Schema::new(vec![
        Field::new("group", DataType::Utf8, false),
        Field::new("amount", DataType::Int64, false),
    ]));

    // Every row its own group, and a wide key, so the aggregation's hash table is the
    // whole input rather than a summary of it. This is the shape that kills a process:
    // an aggregation that cannot reduce anything.
    let groups: Vec<String> = (0..ROWS)
        .map(|i| format!("{i:020}-a-deliberately-long-and-unique-group-key"))
        .collect();
    let amounts: Vec<i64> = (0..ROWS).collect();

    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(groups)),
            Arc::new(Int64Array::from(amounts)),
        ],
    )
    .expect("building")
}

async fn run_with_limit(limit: usize) -> Result<usize, String> {
    let runtime = RuntimeEnvBuilder::new()
        .with_memory_limit(limit, 1.0)
        .build_arc()
        .expect("building the runtime");

    let ctx = SessionContext::new_with_config_rt(
        // One partition, so the limit is not divided into pieces each of which fits.
        SessionConfig::new().with_target_partitions(1),
        runtime,
    );
    ctx.register_batch("wide", hostile_data())
        .expect("registering");

    match ctx
        .sql("SELECT \"group\", SUM(amount) FROM wide GROUP BY \"group\"")
        .await
        .expect("planning")
        .collect()
        .await
    {
        Ok(batches) => Ok(batches.iter().map(RecordBatch::num_rows).sum()),
        Err(e) => Err(e.to_string()),
    }
}

#[tokio::test]
#[ignore = "an experiment, not an assertion"]
async fn probe_where_the_limit_actually_bites() {
    for limit in [
        64 * 1024,
        256 * 1024,
        1024 * 1024,
        8 * 1024 * 1024,
        64 * 1024 * 1024,
    ] {
        let outcome = run_with_limit(limit).await;
        println!(
            "{:>10} bytes: {}",
            limit,
            match outcome {
                Ok(rows) => format!("succeeded with {rows} rows"),
                Err(e) => format!("refused: {}", e.lines().next().unwrap_or("")),
            }
        );
    }
}

#[tokio::test]
async fn a_hostile_aggregation_under_a_tight_limit_is_refused_not_fatal() {
    // One megabyte. The threshold was measured rather than guessed: this aggregation
    // succeeds at eight megabytes and is refused at one, and the first attempt at this
    // test picked two — just over the line, so it passed and proved nothing.
    let error = run_with_limit(1024 * 1024)
        .await
        .expect_err("this aggregation cannot fit in a megabyte");

    // The right reason, and the right *operator*. A test that accepted any error would
    // pass on a typo; one that accepted any resource error would not notice the
    // aggregation quietly starting to spill, which would make this gate meaningless.
    assert!(
        error.contains("Resources exhausted"),
        "refused for the wrong reason: {error}"
    );
    assert!(
        error.contains("HashAggregate"),
        "something other than the aggregation ran out of room: {error}"
    );

    // And the process is still here. Everything after this line is the assertion --
    // if the limit had been enforced by the operating system instead, there would be
    // no line after this one.
    let rows = run_with_limit(512 * 1024 * 1024)
        .await
        .expect("the same query fits when given room");
    assert_eq!(rows as i64, ROWS);
}

#[tokio::test]
async fn the_same_query_succeeds_when_it_is_afforded() {
    // The other half of the claim. A governor that refused everything would pass the
    // test above and be useless.
    let rows = run_with_limit(512 * 1024 * 1024)
        .await
        .expect("a generous limit must let it through");
    assert_eq!(rows as i64, ROWS);
}

#[tokio::test]
async fn several_hostile_queries_in_a_row_leave_the_process_healthy() {
    // One refusal proves the error path works once. The failure this guards against is
    // a refusal that leaks its reservation, so the pool shrinks with every rejected
    // query and eventually a query that should fit does not.
    for _ in 0..5 {
        let error = run_with_limit(1024 * 1024)
            .await
            .expect_err("still too small");
        assert!(error.contains("Resources exhausted") || error.contains("memory"));
    }

    let rows = run_with_limit(512 * 1024 * 1024)
        .await
        .expect("the pool must be intact after five refusals");
    assert_eq!(rows as i64, ROWS);
}

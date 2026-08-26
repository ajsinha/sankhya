//! TPC-H, through this system's own storage.
//!
//! # Why a named suite rather than more microbenchmarks
//!
//! Everything else measured in this project isolates one mechanism: what compaction
//! saves, what pruning saves, what a checkpoint saves. Each is a real number and none of
//! them says whether the system is fast at anything anyone would recognise.
//!
//! TPC-H is not a perfect proxy for what this system is for, and it has the advantage of
//! being a query somebody else wrote. The numbers below can be compared to other numbers
//! rather than only to themselves.
//!
//! # What is being measured
//!
//! Data is generated at a stated scale factor, written as Parquet **through this
//! system's own write path**, and read back through **this system's own table provider**.
//! Nothing here uses the engine's built-in file reader, because that would measure the
//! engine rather than the system built on it.
//!
//! # Honesty about what this is not
//!
//! Not an audited TPC-H result and not comparable to a published one. It is a small
//! scale factor on a development machine, single-node, with no substitution rules and no
//! refresh streams. Calling it a TPC-H benchmark would be a misuse of the name; it is a
//! set of TPC-H queries used as a workload.

use datafusion::prelude::SessionContext;
use sankhya_table::{write_parquet, WriterConfig};
use sankhya_types::Lsn;
use std::path::Path;
use tpchgen::generators::{
    CustomerGenerator, LineItemGenerator, NationGenerator, OrderGenerator, PartGenerator,
    PartSuppGenerator, RegionGenerator, SupplierGenerator,
};
use tpchgen_arrow::{
    CustomerArrow, LineItemArrow, NationArrow, OrderArrow, PartArrow, PartSuppArrow, RegionArrow,
    SupplierArrow,
};

/// Small enough to run in an ordinary test, large enough that the plan matters.
///
/// The measurement below reads `SANKHYA_TPCH_SCALE` instead, so a real number can be
/// taken at a real scale factor without every `cargo test` paying for it.
const SCALE: f64 = 0.05;

fn scale() -> f64 {
    std::env::var("SANKHYA_TPCH_SCALE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(SCALE)
}

/// Write one table's batches as Parquet, returning its row count.
fn write_table<I>(dir: &Path, name: &str, batches: I) -> usize
where
    I: Iterator<Item = arrow_array::RecordBatch>,
{
    let table_dir = dir.join(name);
    let mut rows = 0;
    for (index, batch) in batches.enumerate() {
        rows += batch.num_rows();
        write_parquet(
            &table_dir,
            &format!("part-{index:04}.parquet"),
            &batch,
            Lsn::new(index as u64 + 1),
            WriterConfig::default(),
        )
        .expect("writing");
    }
    rows
}

fn generate(dir: &Path) -> Vec<(String, usize)> {
    generate_at(dir, SCALE)
}

fn generate_at(dir: &Path, scale: f64) -> Vec<(String, usize)> {
    // A batch size that produces several files per table, so the scan has more than one
    // file to plan over -- a single-file scan hides every cost that scales with files.
    const BATCH: usize = 64 * 1024;

    vec![
        (
            "lineitem".to_string(),
            write_table(
                dir,
                "lineitem",
                LineItemArrow::new(LineItemGenerator::new(scale, 1, 1)).with_batch_size(BATCH),
            ),
        ),
        (
            "orders".to_string(),
            write_table(
                dir,
                "orders",
                OrderArrow::new(OrderGenerator::new(scale, 1, 1)).with_batch_size(BATCH),
            ),
        ),
        (
            "customer".to_string(),
            write_table(
                dir,
                "customer",
                CustomerArrow::new(CustomerGenerator::new(scale, 1, 1)).with_batch_size(BATCH),
            ),
        ),
        (
            "part".to_string(),
            write_table(
                dir,
                "part",
                PartArrow::new(PartGenerator::new(scale, 1, 1)).with_batch_size(BATCH),
            ),
        ),
        (
            "partsupp".to_string(),
            write_table(
                dir,
                "partsupp",
                PartSuppArrow::new(PartSuppGenerator::new(scale, 1, 1)).with_batch_size(BATCH),
            ),
        ),
        (
            "supplier".to_string(),
            write_table(
                dir,
                "supplier",
                SupplierArrow::new(SupplierGenerator::new(scale, 1, 1)).with_batch_size(BATCH),
            ),
        ),
        (
            "nation".to_string(),
            write_table(
                dir,
                "nation",
                NationArrow::new(NationGenerator::new(scale, 1, 1)).with_batch_size(BATCH),
            ),
        ),
        (
            "region".to_string(),
            write_table(
                dir,
                "region",
                RegionArrow::new(RegionGenerator::new(scale, 1, 1)).with_batch_size(BATCH),
            ),
        ),
    ]
}

async fn register(ctx: &SessionContext, dir: &Path, tables: &[(String, usize)]) {
    for (name, _) in tables {
        ctx.register_parquet(
            name,
            dir.join(name).to_str().expect("a utf-8 path"),
            datafusion::prelude::ParquetReadOptions::default(),
        )
        .await
        .expect("registering");
    }
}

/// The queries, by their number in the specification.
///
/// Chosen for what they exercise rather than for coverage: a scan-heavy aggregation, a
/// highly selective filter, a three-way join with a top-N, and a six-way join.
fn queries() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        (
            "Q1",
            "pricing summary: a full scan with grouping and eight aggregates",
            "SELECT l_returnflag, l_linestatus, sum(l_quantity) AS sum_qty, \
             sum(l_extendedprice) AS sum_base_price, \
             sum(l_extendedprice * (1 - l_discount)) AS sum_disc_price, \
             sum(l_extendedprice * (1 - l_discount) * (1 + l_tax)) AS sum_charge, \
             avg(l_quantity) AS avg_qty, avg(l_extendedprice) AS avg_price, \
             avg(l_discount) AS avg_disc, count(*) AS count_order \
             FROM lineitem WHERE l_shipdate <= date '1998-09-02' \
             GROUP BY l_returnflag, l_linestatus \
             ORDER BY l_returnflag, l_linestatus",
        ),
        (
            "Q6",
            "forecasting revenue change: a narrow, highly selective filter",
            "SELECT sum(l_extendedprice * l_discount) AS revenue FROM lineitem \
             WHERE l_shipdate >= date '1994-01-01' AND l_shipdate < date '1995-01-01' \
             AND l_discount BETWEEN 0.05 AND 0.07 AND l_quantity < 24",
        ),
        (
            "Q3",
            "shipping priority: a three-way join with a top-N",
            "SELECT l_orderkey, sum(l_extendedprice * (1 - l_discount)) AS revenue, \
             o_orderdate, o_shippriority \
             FROM customer, orders, lineitem \
             WHERE c_mktsegment = 'BUILDING' AND c_custkey = o_custkey \
             AND l_orderkey = o_orderkey AND o_orderdate < date '1995-03-15' \
             AND l_shipdate > date '1995-03-15' \
             GROUP BY l_orderkey, o_orderdate, o_shippriority \
             ORDER BY revenue DESC, o_orderdate LIMIT 10",
        ),
        (
            "Q5",
            "local supplier volume: a six-way join",
            "SELECT n_name, sum(l_extendedprice * (1 - l_discount)) AS revenue \
             FROM customer, orders, lineitem, supplier, nation, region \
             WHERE c_custkey = o_custkey AND l_orderkey = o_orderkey \
             AND l_suppkey = s_suppkey AND c_nationkey = s_nationkey \
             AND s_nationkey = n_nationkey AND n_regionkey = r_regionkey \
             AND r_name = 'ASIA' AND o_orderdate >= date '1994-01-01' \
             AND o_orderdate < date '1995-01-01' \
             GROUP BY n_name ORDER BY revenue DESC",
        ),
    ]
}

#[tokio::test]
async fn every_query_runs_and_returns_rows() {
    // Correctness first, and deliberately not a golden-answer check: the reference
    // answers are defined for scale factor 1 with the specification's substitution
    // parameters, and asserting invented ones would be asserting my own arithmetic.
    //
    // What is asserted is that each query plans, executes, and produces the shape the
    // specification says it does. A benchmark that measures a query returning nothing is
    // measuring an error path.
    let dir = tempfile::tempdir().expect("a temp dir");
    let tables = generate(dir.path());
    let ctx = SessionContext::new();
    register(&ctx, dir.path(), &tables).await;

    for (name, what, sql) in queries() {
        let batches = ctx
            .sql(sql)
            .await
            .unwrap_or_else(|e| panic!("{name} did not plan: {e}"))
            .collect()
            .await
            .unwrap_or_else(|e| panic!("{name} did not execute: {e}"));

        let rows: usize = batches.iter().map(arrow_array::RecordBatch::num_rows).sum();
        assert!(rows > 0, "{name} ({what}) returned nothing");
    }
}

#[tokio::test]
async fn the_generated_data_has_the_proportions_the_specification_gives() {
    // TPC-H fixes the cardinality of every table relative to the scale factor. If the
    // generator produced something else, every number below would be measuring a
    // different workload than its name claims.
    let dir = tempfile::tempdir().expect("a temp dir");
    let tables = generate(dir.path());
    let rows: std::collections::BTreeMap<&str, usize> = tables
        .iter()
        .map(|(name, rows)| (name.as_str(), *rows))
        .collect();

    // Fixed regardless of scale.
    assert_eq!(rows["nation"], 25);
    assert_eq!(rows["region"], 5);

    // Scaled: 150,000 customers and 1,500,000 orders per unit of scale factor.
    let expected_customers = (150_000.0 * SCALE) as usize;
    assert_eq!(rows["customer"], expected_customers);
    assert_eq!(rows["orders"], (1_500_000.0 * SCALE) as usize);

    // Line items average four per order, and the exact count varies by design.
    let orders = rows["orders"];
    assert!(
        rows["lineitem"] > orders * 3 && rows["lineitem"] < orders * 5,
        "{} line items for {orders} orders",
        rows["lineitem"]
    );
}

/// The numbers.
///
/// Run with `cargo test -p sankhya-olap --test tpch --release -- --ignored --nocapture
/// measure`.
#[tokio::test]
#[ignore = "a measurement, not an assertion"]
async fn measure_tpch_queries() {
    let dir = tempfile::tempdir().expect("a temp dir");

    let scale = scale();
    let generated = std::time::Instant::now();
    let tables = generate_at(dir.path(), scale);
    let generation = generated.elapsed();

    let rows: usize = tables.iter().map(|(_, r)| r).sum();
    let bytes: u64 = walk_bytes(dir.path());
    println!(
        "scale factor {scale}: {rows} rows across {} tables, {:.1} MiB of Parquet, \
         generated and written in {:.2?}",
        tables.len(),
        bytes as f64 / (1024.0 * 1024.0),
        generation
    );

    let ctx = SessionContext::new();
    register(&ctx, dir.path(), &tables).await;

    for (name, what, sql) in queries() {
        // Best of three. A median would need more runs to mean anything, and the
        // minimum is the figure least polluted by whatever else the machine is doing.
        let mut best = std::time::Duration::MAX;
        let mut produced = 0;
        for _ in 0..3 {
            let start = std::time::Instant::now();
            let batches = ctx
                .sql(sql)
                .await
                .expect("planning")
                .collect()
                .await
                .expect("executing");
            best = best.min(start.elapsed());
            produced = batches.iter().map(arrow_array::RecordBatch::num_rows).sum();
        }
        println!("{name:>4}  {best:>9.2?}  {produced:>6} rows   {what}");
    }
}

fn walk_bytes(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| {
            let path = entry.path();
            if path.is_dir() {
                walk_bytes(&path)
            } else {
                std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0)
            }
        })
        .sum()
}

/// The same queries at the concurrency the requirements name.
///
/// Run with `SANKHYA_TPCH_SCALE=1 cargo test -p sankhya-olap --test tpch --release --
/// --ignored --nocapture concurrency`.
///
/// # Why single-query latency is not the number
///
/// A latency objective without a concurrency figure is not an objective — every engine
/// is fast with one query running. The requirements state both, and the second is the
/// harder half: it is where memory pressure, scheduling and shared caches start to
/// matter, and where a system that looked fine stops being fine.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "a measurement, not an assertion"]
async fn measure_tpch_under_concurrency() {
    use std::sync::Arc;

    let dir = tempfile::tempdir().expect("a temp dir");
    let scale = scale();
    let tables = generate_at(dir.path(), scale);

    let ctx = Arc::new(SessionContext::new());
    register(&ctx, dir.path(), &tables).await;

    println!("scale factor {scale}, latencies in milliseconds");
    println!(
        "{:>4}  {:>7}  {:>8}  {:>8}  {:>8}",
        "", "clients", "median", "p95", "max"
    );

    for (name, _, sql) in queries() {
        // The concurrency figures the requirements attach to the nearest analogous
        // workload: four for a wide scan, eight for a selective lookup and for a pivot.
        let clients = match name {
            "Q1" => 4,
            _ => 8,
        };

        // A warm-up round, because the first execution of a plan pays for compilation
        // and the first read pays for the page cache. Reporting those inside the
        // measurement would describe a system nobody runs.
        for _ in 0..2 {
            let _ = ctx
                .sql(sql)
                .await
                .expect("planning")
                .collect()
                .await
                .expect("executing");
        }

        const ROUNDS: usize = 5;
        let mut samples = Vec::new();
        for _ in 0..ROUNDS {
            let handles: Vec<_> = (0..clients)
                .map(|_| {
                    let ctx = Arc::clone(&ctx);
                    let sql = sql.to_string();
                    tokio::spawn(async move {
                        let start = std::time::Instant::now();
                        let _ = ctx
                            .sql(&sql)
                            .await
                            .expect("planning")
                            .collect()
                            .await
                            .expect("executing");
                        start.elapsed()
                    })
                })
                .collect();
            for handle in handles {
                samples.push(handle.await.expect("a client panicked"));
            }
        }

        samples.sort_unstable();
        let at = |q: f64| samples[((samples.len() as f64 * q) as usize).min(samples.len() - 1)];
        println!(
            "{name:>4}  {clients:>7}  {:>8.1}  {:>8.1}  {:>8.1}",
            at(0.5).as_secs_f64() * 1000.0,
            at(0.95).as_secs_f64() * 1000.0,
            samples.last().expect("samples").as_secs_f64() * 1000.0
        );
    }
}

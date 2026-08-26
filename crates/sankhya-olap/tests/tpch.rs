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
//! Data is generated at a stated scale factor and written as Parquet **through this
//! system's own write path**, then read back through **this system's own configured
//! session** — the one that asserts filter pushdown, filter reordering and bloom filters
//! at startup.
//!
//! The first version of this file used a bare `SessionContext`, which is to say it
//! measured the engine's defaults rather than this system. Two of those defaults are off
//! and switch off the mechanism they belong to, which is the exact reason the settings
//! are asserted at startup in the first place. Benchmarking around that assertion made
//! the numbers describe a system nobody runs.
//!
//! # Honesty about what this is not
//!
//! Not an audited TPC-H result and not comparable to a published one. It is a small
//! scale factor on a development machine, single-node, with no substitution rules and no
//! refresh streams. Calling it a TPC-H benchmark would be a misuse of the name; it is a
//! set of TPC-H queries used as a workload.

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

/// Write one table's batches as Parquet **and commit them to a table log**, returning its
/// row count.
///
/// A commit-position column is added as the batches are written, because that is what
/// capture produces and what the read path resolves against. Without it these tables
/// could only be read through the engine's own file listing, which would make a benchmark
/// of this system a benchmark of the engine underneath it.
fn write_table<I>(dir: &Path, name: &str, batches: I) -> usize
where
    I: Iterator<Item = arrow_array::RecordBatch>,
{
    use arrow_array::UInt64Array;
    use arrow_schema::{DataType, Field, Schema};
    use sankhya_table::column_stats;
    use sankhya_table_delta::{commit, create, schema_string, Action, AddFile, Metadata};
    use std::sync::Arc;

    let table_dir = dir.join(name);
    let mut rows = 0u64;
    let mut adds = Vec::new();
    let mut delta_schema: Option<String> = None;

    for (index, batch) in batches.enumerate() {
        // The position column, one value per row and increasing across the table.
        let positions: Vec<u64> = (rows..rows + batch.num_rows() as u64).collect();
        let mut fields: Vec<Arc<Field>> = batch.schema().fields().iter().cloned().collect();
        fields.push(Arc::new(Field::new(
            "_sankhya_commit_lsn",
            DataType::UInt64,
            false,
        )));
        let mut columns = batch.columns().to_vec();
        columns.push(Arc::new(UInt64Array::from(positions)));

        let with_position =
            arrow_array::RecordBatch::try_new(Arc::new(Schema::new(fields)), columns)
                .expect("adding the position column");

        rows += with_position.num_rows() as u64;
        if delta_schema.is_none() {
            delta_schema =
                Some(schema_string(&with_position.schema()).expect("a representable schema"));
        }

        let file = format!("part-{index:04}.parquet");
        let report = write_parquet(
            &table_dir,
            &file,
            &with_position,
            Lsn::new(rows),
            WriterConfig::default(),
        )
        .expect("writing");

        // The statistics compaction would have produced, computed from the batch that was
        // just written -- which is what makes the provider able to prune and the
        // optimizer able to plan.
        let statistics = sankhya_table_delta::from_column_stats(
            with_position.num_rows() as u64,
            &column_stats(&with_position),
        );
        adds.push(Action::Add(AddFile::with_statistics(
            file,
            report.bytes,
            0,
            &statistics,
        )));
    }

    let schema = delta_schema.expect("at least one batch");
    commit(&table_dir, 0, &create(Metadata::new(name, schema, 0))).expect("creating");
    commit(&table_dir, 1, &adds).expect("publishing");

    usize::try_from(rows).expect("a sane row count")
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

/// Register every table **through this system's own provider**.
///
/// Not through the engine's file listing. The provider is what resolves the file set from
/// the log, prunes on the statistics catalogue and hands the optimizer its cardinalities —
/// registering around it would make this a benchmark of the engine rather than of the
/// system built on it.
async fn register(ctx: &SessionContext, dir: &Path, tables: &[(String, usize)]) {
    use sankhya_readpath::resolve;
    use sankhya_types::LsnRange;
    use std::sync::Arc;

    for (name, rows) in tables {
        let table_root = dir.join(name);
        let target = Lsn::new(u64::try_from(*rows).expect("a sane row count"));

        // The schema as written, which is the generated schema plus the position column.
        let live = sankhya_table_delta::live_files(&table_root).expect("the log");
        let first = table_root.join(&live.files[0].path);
        let file = std::fs::File::open(&first).expect("opening");
        let schema = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(file)
            .expect("reader")
            .schema()
            .clone();

        let provider = resolve(
            schema,
            &table_root,
            Some(LsnRange::up_to(target)),
            None,
            target,
        )
        .expect("resolving");

        ctx.register_table(name, Arc::new(provider))
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
    let ctx = sankhya_olap::session().expect("the required settings must apply");
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

    let ctx = sankhya_olap::session().expect("the required settings must apply");
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

    let ctx = Arc::new(sankhya_olap::session().expect("the required settings must apply"));
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

/// Which of the asserted settings is responsible for what.
///
/// Run with `SANKHYA_TPCH_SCALE=1 cargo test -p sankhya-olap --test tpch --release --
/// --ignored --nocapture attribute`.
///
/// The settings are asserted at startup as a group, on the reasoning that each switches
/// off a mechanism it belongs to. That reasoning deserves a measurement per setting
/// rather than per group, because a group that is net positive can still contain
/// something that costs more than it saves.
#[tokio::test]
#[ignore = "a measurement, not an assertion"]
async fn attribute_each_required_setting() {
    use datafusion::prelude::SessionConfig;

    let dir = tempfile::tempdir().expect("a temp dir");
    let scale = scale();
    let tables = generate_at(dir.path(), scale);

    // Every combination of the three, so an interaction between two of them is visible
    // rather than attributed to whichever was toggled last.
    let settings = [
        "datafusion.execution.parquet.pushdown_filters",
        "datafusion.execution.parquet.reorder_filters",
        "datafusion.execution.parquet.bloom_filter_on_read",
    ];

    println!("scale factor {scale}, best of three, milliseconds");
    print!("{:>28}", "pushdown/reorder/bloom");
    for (name, _, _) in queries() {
        print!("{name:>10}");
    }
    println!();

    for mask in 0..8u8 {
        let mut config = SessionConfig::new();
        for (bit, key) in settings.iter().enumerate() {
            let on = mask & (1 << bit) != 0;
            config = config.set_str(key, if on { "true" } else { "false" });
        }
        let ctx = SessionContext::new_with_config(config);
        register(&ctx, dir.path(), &tables).await;

        let label = format!(
            "{}/{}/{}",
            if mask & 1 != 0 { "on " } else { "off" },
            if mask & 2 != 0 { "on " } else { "off" },
            if mask & 4 != 0 { "on " } else { "off" },
        );
        print!("{label:>28}");

        for (_, _, sql) in queries() {
            let mut best = std::time::Duration::MAX;
            for _ in 0..3 {
                let start = std::time::Instant::now();
                let _ = ctx
                    .sql(sql)
                    .await
                    .expect("planning")
                    .collect()
                    .await
                    .expect("executing");
                best = best.min(start.elapsed());
            }
            print!("{:>10.1}", best.as_secs_f64() * 1000.0);
        }
        println!();
    }
}

/// Where filter pushdown earns its place, and where it does not.
///
/// Run with `SANKHYA_TPCH_SCALE=1 cargo test -p sankhya-olap --test tpch --release --
/// --ignored --nocapture selectivity`.
///
/// Pushdown evaluates predicates inside the decoder so payload columns are materialized
/// only for surviving rows. That bargain is obviously good when almost nothing survives and
/// obviously bad when almost everything does — the bookkeeping is paid either way and the
/// saving is proportional to what it avoids. What matters is where the crossover falls on
/// real data, because "it depends" is not a setting.
#[tokio::test]
#[ignore = "a measurement, not an assertion"]
async fn measure_pushdown_against_selectivity() {
    use datafusion::prelude::SessionConfig;

    let dir = tempfile::tempdir().expect("a temp dir");
    let scale = scale();
    let tables = generate_at(dir.path(), scale);

    // The same shape at four selectivities: a whole-table sum with a filter that keeps
    // progressively less. The payload column is deliberately not the filter column, so
    // there is something for late materialization to avoid fetching.
    let cases: Vec<(&str, String)> = vec![
        (
            "one row in ~6,000,000",
            "SELECT sum(l_extendedprice) FROM lineitem WHERE l_orderkey = 1 AND l_linenumber = 1"
                .to_string(),
        ),
        (
            "one row in ~1,500",
            "SELECT sum(l_extendedprice) FROM lineitem WHERE l_orderkey < 1000".to_string(),
        ),
        (
            "about one row in 60",
            "SELECT sum(l_extendedprice) FROM lineitem WHERE l_quantity < 1.0".to_string(),
        ),
        (
            "about one row in 7",
            "SELECT sum(l_extendedprice) FROM lineitem WHERE l_shipdate >= date '1994-01-01' \
             AND l_shipdate < date '1995-01-01'"
                .to_string(),
        ),
        (
            "every row",
            "SELECT sum(l_extendedprice) FROM lineitem WHERE l_quantity > 0".to_string(),
        ),
    ];

    println!("scale factor {scale}, best of three, milliseconds");
    println!(
        "{:>24}{:>10}{:>10}{:>10}",
        "selectivity", "off", "on", "ratio"
    );

    for (label, sql) in cases {
        let mut timings = Vec::new();
        for on in [false, true] {
            let config = SessionConfig::new().set_str(
                "datafusion.execution.parquet.pushdown_filters",
                if on { "true" } else { "false" },
            );
            let ctx = SessionContext::new_with_config(config);
            register(&ctx, dir.path(), &tables).await;

            let mut best = std::time::Duration::MAX;
            for _ in 0..3 {
                let start = std::time::Instant::now();
                let _ = ctx
                    .sql(&sql)
                    .await
                    .expect("planning")
                    .collect()
                    .await
                    .expect("executing");
                best = best.min(start.elapsed());
            }
            timings.push(best.as_secs_f64() * 1000.0);
        }
        println!(
            "{label:>24}{:>10.1}{:>10.1}{:>10.2}",
            timings[0],
            timings[1],
            timings[0] / timings[1]
        );
    }
}

/// What sorting the data by the column a query filters on is worth.
///
/// Run with `SANKHYA_TPCH_SCALE=1 cargo test -p sankhya-olap --test tpch --release --
/// --ignored --nocapture clustering`.
///
/// # Why this is the lever rather than pushdown
///
/// Q6 filters `l_shipdate` to one year of seven. Written in generation order, every row
/// group holds the whole date range, so the bounds exclude nothing and the scan reads
/// everything. Written in date order, most row groups fall entirely outside the year and
/// are skipped on their statistics — before any decoding, which is the only saving that
/// is free.
///
/// The architecture's rule is to prefer the reversible decision: sorting is cheap to
/// change at the next compaction, partitioning is a physical commitment. This measures
/// what the reversible one buys.
#[tokio::test]
#[ignore = "a measurement, not an assertion"]
async fn measure_what_clustering_by_the_filter_column_is_worth() {
    use datafusion::prelude::ParquetReadOptions;

    let scale = scale();
    let generated = tempfile::tempdir().expect("a temp dir");
    let tables = generate_at(generated.path(), scale);

    // Read the generated lineitem back and write it again in date order. Doing it
    // through a query rather than by hand is the point: this is what a compaction that
    // sorts would produce.
    let ctx = std::sync::Arc::new(sankhya_olap::session().expect("settings"));
    register(&ctx, generated.path(), &tables).await;

    let sorted_dir = tempfile::tempdir().expect("a temp dir");
    let batches = ctx
        .sql("SELECT * FROM lineitem ORDER BY l_shipdate")
        .await
        .expect("planning")
        .collect()
        .await
        .expect("executing");

    // Re-batched to the same size as the unsorted files, so the comparison is between
    // orderings rather than between file layouts.
    const ROWS_PER_FILE: usize = 64 * 1024;
    let schema = batches[0].schema();
    let combined = arrow::compute::concat_batches(&schema, &batches).expect("concat");
    let mut index = 0;
    let mut file = 0;
    while index < combined.num_rows() {
        let len = ROWS_PER_FILE.min(combined.num_rows() - index);
        write_parquet(
            &sorted_dir.path().join("lineitem"),
            &format!("part-{file:04}.parquet"),
            &combined.slice(index, len),
            Lsn::new(file as u64 + 1),
            WriterConfig::default(),
        )
        .expect("writing");
        index += len;
        file += 1;
    }

    let sorted_ctx = std::sync::Arc::new(sankhya_olap::session().expect("settings"));
    sorted_ctx
        .register_parquet(
            "lineitem",
            sorted_dir
                .path()
                .join("lineitem")
                .to_str()
                .expect("a utf-8 path"),
            ParquetReadOptions::default(),
        )
        .await
        .expect("registering");

    let q6 = queries()
        .into_iter()
        .find(|(name, _, _)| *name == "Q6")
        .map(|(_, _, sql)| sql)
        .expect("Q6");

    println!("scale factor {scale}, best of three, milliseconds");
    for (label, context) in [
        ("generation order", &ctx),
        ("sorted by ship date", &sorted_ctx),
    ] {
        let mut best = std::time::Duration::MAX;
        for _ in 0..3 {
            let start = std::time::Instant::now();
            let _ = context
                .sql(q6)
                .await
                .expect("planning")
                .collect()
                .await
                .expect("executing");
            best = best.min(start.elapsed());
        }
        // And at eight clients, which is what NFR-PERF-02 states its 250 ms against.
        let shared = std::sync::Arc::clone(context);
        let mut samples = Vec::new();
        for _ in 0..3 {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let ctx = std::sync::Arc::clone(&shared);
                    let sql = q6.to_string();
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
        let p95 = samples[(samples.len() * 95) / 100].as_secs_f64() * 1000.0;

        println!(
            "{label:>22}  {:>8.1} single  {p95:>8.1} p95 at 8 clients",
            best.as_secs_f64() * 1000.0
        );
    }
}

/// What this system's storage costs against bare Parquet.
///
/// Run with `SANKHYA_TPCH_SCALE=1 cargo test -p sankhya-olap --test tpch --release --
/// --ignored --nocapture overhead`.
///
/// Every row carries a commit position. That is what makes a read at a pinned position
/// possible, and it is not free: the column is written for every row of every table, and
/// scans pay for the bytes whether or not the query mentions it.
///
/// The number belongs in the open rather than inside a claim that the storage is
/// "efficient". A cost that buys something is a design; a cost nobody measured is a
/// surprise.
#[tokio::test]
#[ignore = "a measurement, not an assertion"]
async fn measure_what_the_commit_position_costs() {
    let scale = scale();

    let with_position = tempfile::tempdir().expect("a temp dir");
    let tables = generate_at(with_position.path(), scale);

    // The same data written without the position column, which is what the generator
    // produces and what a bare Parquet layout would hold.
    let bare = tempfile::tempdir().expect("a temp dir");
    const BATCH: usize = 64 * 1024;
    let mut index = 0;
    for batch in LineItemArrow::new(LineItemGenerator::new(scale, 1, 1)).with_batch_size(BATCH) {
        write_parquet(
            &bare.path().join("lineitem"),
            &format!("part-{index:04}.parquet"),
            &batch,
            Lsn::new(index as u64 + 1),
            WriterConfig::default(),
        )
        .expect("writing");
        index += 1;
    }

    let ours = walk_bytes(&with_position.path().join("lineitem"));
    let theirs = walk_bytes(&bare.path().join("lineitem"));
    let lineitem_rows = tables
        .iter()
        .find(|(name, _)| name == "lineitem")
        .map(|(_, rows)| *rows)
        .expect("lineitem");

    println!(
        "scale factor {scale}, lineitem only: {lineitem_rows} rows\n  \
         bare Parquet          {:>8.1} MiB\n  \
         with commit position  {:>8.1} MiB  ({:+.1}%)",
        theirs as f64 / (1024.0 * 1024.0),
        ours as f64 / (1024.0 * 1024.0),
        (ours as f64 / theirs as f64 - 1.0) * 100.0
    );
}

/// The plan for Q5, through the provider and through the engine's own file listing.
///
/// Run with `cargo test -p sankhya-olap --test tpch --release -- --ignored --nocapture
/// diff_plans`.
///
/// A latency difference between two ways of reading the same data is either the reading
/// or the plan. Printing both plans settles which, and guessing at it costs more than
/// looking.
#[tokio::test]
#[ignore = "a diagnostic, not an assertion"]
async fn diff_plans_for_the_six_way_join() {
    use datafusion::physical_plan::displayable;
    use datafusion::prelude::ParquetReadOptions;

    let dir = tempfile::tempdir().expect("a temp dir");
    let tables = generate_at(dir.path(), scale());

    let q5 = queries()
        .into_iter()
        .find(|(name, _, _)| *name == "Q5")
        .map(|(_, _, sql)| sql)
        .expect("Q5");

    // Through the provider.
    let provider_ctx = sankhya_olap::session().expect("settings");
    register(&provider_ctx, dir.path(), &tables).await;

    // Through the engine's file listing, over the same files.
    let listing_ctx = sankhya_olap::session().expect("settings");
    for (name, _) in &tables {
        listing_ctx
            .register_parquet(
                name,
                dir.path().join(name).to_str().expect("a utf-8 path"),
                ParquetReadOptions::default(),
            )
            .await
            .expect("registering");
    }

    for (label, ctx) in [("provider", &provider_ctx), ("listing", &listing_ctx)] {
        let plan = ctx
            .sql(q5)
            .await
            .expect("planning")
            .create_physical_plan()
            .await
            .expect("physical plan");

        // Join order is what a size or cardinality difference changes, so that is what
        // is printed: the joins in the order the optimizer nested them.
        let text = displayable(plan.as_ref()).indent(false).to_string();
        for line in text
            .lines()
            .filter(|l| l.contains("FilterExec") || l.contains("DataSourceExec"))
        {
            let t = line.trim();
            println!("    | {}", &t[..200.min(t.len())]);
        }
        let joins: Vec<&str> = text
            .lines()
            .filter(|l| l.contains("HashJoinExec"))
            .map(str::trim)
            .collect();
        println!("--- {label}: {} joins", joins.len());
        for join in joins {
            println!("    {}", &join[..160.min(join.len())]);
        }

        // Best of five. The interest is the difference between the two paths, and a
        // mean carries whatever else the machine was doing into that difference.
        let mut best = std::time::Duration::MAX;
        for _ in 0..5 {
            let start = std::time::Instant::now();
            ctx.sql(q5)
                .await
                .expect("planning")
                .collect()
                .await
                .expect("running");
            best = best.min(start.elapsed());
        }
        println!("    {label}: {best:>9.2?}");
    }
}

/// `NFR-PERF-02` measured against a needle lookup, which is what it asks for.
///
/// Run with `SANKHYA_TPCH_SCALE=1 cargo test -p sankhya-olap --test tpch --release --
/// --ignored --nocapture needle`.
///
/// This exists because the objective was being read against Q6, and Q6 is not a needle
/// lookup. Q6 applies three range predicates and returns something under two percent of
/// six million rows — around a hundred thousand of them, touched across every file in
/// the table. `NFR-PERF-02` describes finding *one* row in a large table, which is a
/// different mechanism entirely: it is answered by pruning almost every file unread,
/// not by scanning quickly.
///
/// Reporting Q6 against it therefore failed an objective that had never been tested.
/// The public suite has no needle lookup — TPC-H has no point query at all — so the
/// requirement's own words are used instead, and the mismatch is recorded rather than
/// the number being quietly re-mapped to a friendlier objective.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "a measurement, not an assertion"]
async fn measure_a_needle_lookup() {
    use std::sync::Arc;

    let dir = tempfile::tempdir().expect("a temp dir");
    let scale = scale();
    let tables = generate_at(dir.path(), scale);
    let ctx = Arc::new(sankhya_olap::session().expect("the required settings must apply"));
    register(&ctx, dir.path(), &tables).await;

    // A key from the middle of the table. The first or last would prune to the first or
    // last file and flatter the result; the middle is the ordinary case.
    let sql = "SELECT l_orderkey, l_partkey, l_extendedprice \
               FROM lineitem WHERE l_orderkey = 3000001";

    for _ in 0..2 {
        let _ = ctx.sql(sql).await.expect("planning").collect().await;
    }

    const CLIENTS: usize = 8;
    let mut samples = Vec::new();
    for _ in 0..5 {
        let handles: Vec<_> = (0..CLIENTS)
            .map(|_| {
                let ctx = Arc::clone(&ctx);
                tokio::spawn(async move {
                    let start = std::time::Instant::now();
                    let rows = ctx
                        .sql(sql)
                        .await
                        .expect("planning")
                        .collect()
                        .await
                        .expect("executing");
                    (
                        start.elapsed(),
                        rows.iter().map(|b| b.num_rows()).sum::<usize>(),
                    )
                })
            })
            .collect();
        for handle in handles {
            samples.push(handle.await.expect("a client panicked"));
        }
    }

    // A lookup that found nothing would be fast for the wrong reason.
    let found = samples[0].1;
    assert!(
        found > 0,
        "the needle key matched no rows; the measurement is meaningless"
    );

    let mut times: Vec<_> = samples.iter().map(|(t, _)| *t).collect();
    times.sort_unstable();
    let at = |q: f64| times[((times.len() as f64 * q) as usize).min(times.len() - 1)];
    println!(
        "scale factor {scale}, {CLIENTS} clients, {found} row(s) matched\n\
         needle lookup: median {:.1} ms, p95 {:.1} ms  (NFR-PERF-02 target: p95 < 250 ms)",
        at(0.5).as_secs_f64() * 1000.0,
        at(0.95).as_secs_f64() * 1000.0,
    );
}

/// The performance objectives, asserted rather than printed.
///
/// Run with `SANKHYA_TPCH_SCALE=1 cargo test -p sankhya-olap --test tpch --release --
/// --ignored --nocapture objectives`, which is what `cargo xtask check-performance`
/// invokes.
///
/// Every other performance test in this file prints a number and passes regardless.
/// That is right for a measurement and wrong for an objective: `NFR-PERF-*` are
/// requirements, and a requirement that cannot fail a build is a preference. This is
/// the only test here that fails when the system gets slower.
///
/// Each objective is measured against the workload its own text describes, not against
/// whichever public-suite query is nearest:
///
/// - **`NFR-PERF-02`** says *selective needle lookup*. TPC-H contains no point query, so
///   this uses one. It was previously read against Q6, which returns about a hundred
///   thousand rows from three range predicates — a different mechanism, and one that
///   failed an objective never actually tested. See `measure_a_needle_lookup`.
/// - **`NFR-PERF-03`** says *multi-dimensional pivot, warm, pruned*. Q3 is that shape.
///   Q5 is a six-way join, and it is measured and published by
///   `measure_tpch_under_concurrency` — but it is not gated here, because it does not
///   satisfy the objective's stated precondition that a partition predicate be present.
///   Partitioning is not built, so no query can currently satisfy it. That is recorded
///   as an open item in `docs/STATUS.md`, not hidden by a passing test.
/// - **`NFR-PERF-04`** says *wide scan, warm, local cache*. Q1 is that shape.
///
/// The margins are wide enough that this should not be flaky. If it starts failing
/// intermittently, the correct response is to find what regressed, not to raise the
/// bound — the bounds are the requirements, and they are already being met on hardware
/// well below the reference node.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "the performance gate: minutes, and needs a quiet machine"]
async fn the_performance_objectives_are_met() {
    use std::sync::Arc;

    let dir = tempfile::tempdir().expect("a temp dir");
    let scale = scale();
    let tables = generate_at(dir.path(), scale);
    let ctx = Arc::new(sankhya_olap::session().expect("the required settings must apply"));
    register(&ctx, dir.path(), &tables).await;

    let needle = "SELECT l_orderkey, l_partkey, l_extendedprice \
                  FROM lineitem WHERE l_orderkey = 3000001";
    let pivot = queries()
        .into_iter()
        .find(|(n, _, _)| *n == "Q3")
        .map(|(_, _, sql)| sql)
        .expect("Q3");
    let scan = queries()
        .into_iter()
        .find(|(n, _, _)| *n == "Q1")
        .map(|(_, _, sql)| sql)
        .expect("Q1");

    let cases = [
        (
            "NFR-PERF-02",
            "selective needle lookup",
            needle,
            8usize,
            250u128,
        ),
        ("NFR-PERF-03", "multi-dimensional pivot", pivot, 8, 1_000),
        ("NFR-PERF-04", "wide scan", scan, 4, 3_000),
    ];

    let mut over = Vec::new();
    for (id, what, sql, clients, budget_ms) in cases {
        let p95 = percentile_under_load(&ctx, sql, clients).await;
        let ms = p95.as_millis();
        println!("{id}  {what:<24} {clients} clients  p95 {ms:>5} ms  budget {budget_ms} ms");
        if ms >= budget_ms {
            over.push(format!(
                "{id} ({what}): p95 {ms} ms against a {budget_ms} ms budget"
            ));
        }
    }

    assert!(
        over.is_empty(),
        "performance objectives not met on this machine:\n  {}",
        over.join("\n  ")
    );
}

/// p95 across five rounds of `clients` concurrent executions, after two warm-ups.
async fn percentile_under_load(
    ctx: &std::sync::Arc<datafusion::prelude::SessionContext>,
    sql: &str,
    clients: usize,
) -> std::time::Duration {
    use std::sync::Arc;

    for _ in 0..2 {
        let _ = ctx.sql(sql).await.expect("planning").collect().await;
    }

    // Eight clients on a current-thread runtime are eight clients taking turns, and the
    // resulting number describes nothing the requirement is about. This was not
    // hypothetical: the first version of the gate inherited the default flavour and
    // reported a pivot three times over its budget.
    assert!(
        tokio::runtime::Handle::current().metrics().num_workers() > 1,
        "this measurement needs a multi-threaded runtime; on a current-thread one the \
         clients serialize and the figure is meaningless"
    );

    let mut samples = Vec::new();
    for _ in 0..5 {
        let handles: Vec<_> = (0..clients)
            .map(|_| {
                let ctx = Arc::clone(ctx);
                let sql = sql.to_string();
                tokio::spawn(async move {
                    let start = std::time::Instant::now();
                    ctx.sql(&sql)
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
    samples[((samples.len() as f64 * 0.95) as usize).min(samples.len() - 1)]
}

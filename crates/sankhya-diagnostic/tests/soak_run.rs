//! The scheduled soak: a real warehouse, real load, sampled and judged.
//!
//! # A test, and only a test
//!
//! It was a binary in a crate of its own, with its own writer and its own compaction. That
//! shape had two consequences and both were bad. A soak that writes through its own code is
//! measuring its own code --- it went near neither `sankhya-publish` nor
//! `sankhya-maintenance`, which is why the publish path could declare a partition column it
//! never wrote and no run ever noticed. And a test does not ship, so a test has no business
//! being a binary.
//!
//! So: every write here goes through [`sankhya_publish::Publication`], every merge through
//! [`sankhya_maintenance::run_compaction`], and this file owns nothing either of them owns.
//! A defect in the write path now fails the soak.
//!
//! # Running it
//!
//! Ignored by default, because it takes forty-five minutes:
//!
//! ```text
//! SANKHYA_SOAK_MINUTES=45 \
//!   cargo test -p sankhya-diagnostic --test soak_run -- --ignored --nocapture
//! ```
//!
//! Configured by environment rather than argv, because a test harness has no argv of its
//! own. `SANKHYA_SOAK_GB`, `SANKHYA_SOAK_TABLES`, `SANKHYA_SOAK_MINUTES`,
//! `SANKHYA_SOAK_SCHEMA`, `SANKHYA_SOAK_AT` --- the last defaulting inside the project root
//! and refused if it points outside it.

#![allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]


// A binary may print. That is what it is for.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use arrow_array::{Date32Array, Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use sankhya_diagnostic::soak::measure::{Bound, Scale, Watched};
use sankhya_diagnostic::soak::report::supported_horizon;
use sankhya_diagnostic::soak::sample::{file_bytes, open_files, resident_bytes, Samples};
use sankhya_diagnostic::soak::Report;
use sankhya_maintenance::{spawn_maintenance, MaintenancePolicy};
use sankhya_publish::{Accumulator, FanOut, Publication, Strain};
use sankhya_table::{scan_parquet, Scanned};
use sankhya_table_delta::live_files;
use sankhya_types::Lsn;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

/// Where a warehouse goes unless the caller says otherwise.
///
/// Relative to the project root, so it is inside the repository by construction rather than
/// by the caller remembering. `.build` is already where generated artefacts live and is
/// already ignored by version control.
///
/// The warehouse *is* this directory --- tables live at `<warehouse>/<schema>/<table>`. An
/// earlier version put the warehouse under a per-run directory and produced
/// `.build/soak/warehouse/soak/records`, which names the schema twice and buries the
/// warehouse a level deeper than anything needs.
const DEFAULT_WAREHOUSE: &str = ".build/warehouse";

/// How often a reading is taken.
const SAMPLE_EVERY: Duration = Duration::from_secs(15);
/// How often progress is printed and the report rewritten.
const REPORT_EVERY: Duration = Duration::from_secs(120);

/// Where a run's own files go: beside the warehouse, never inside it.
///
/// The warehouse is emptied before each fill. A report written into it is a report the next
/// run deletes, and the evidence a soak produces has to outlive the data it produced it from.
fn artefacts(warehouse: &Path) -> PathBuf {
    warehouse.parent().map_or_else(|| warehouse.to_path_buf(), Path::to_path_buf)
}

/// The project root: the nearest ancestor holding the workspace manifest.
///
/// Found rather than assumed, so the default warehouse is the same directory whether the
/// binary is run from the root, from a crate, or from a test.
fn project_root() -> Option<PathBuf> {
    let mut here = std::env::current_dir().ok()?;
    loop {
        let manifest = here.join("Cargo.toml");
        if manifest.exists()
            && std::fs::read_to_string(&manifest)
                .map(|text| text.contains("[workspace]"))
                .unwrap_or(false)
        {
            return Some(here);
        }
        if !here.pop() {
            return None;
        }
    }
}

/// The path, if it is inside the project root.
///
/// # Why this is a refusal rather than a convention
///
/// The rule that this project writes nothing outside its own root was breached twice in one
/// session --- once by a default, once by a redirect --- by somebody who knew the rule. A rule
/// that depends on being remembered is a rule with a failure rate. This makes the breach
/// impossible instead: a warehouse outside the root is not a warehouse this binary will use.
///
/// The check is on the *resolved* path, so `..` and a symlink cannot walk out of it. The
/// parent is resolved rather than the path itself, because the warehouse usually does not
/// exist yet.
fn confined(root: &Path, at: &Path) -> Result<PathBuf, String> {
    let absolute = if at.is_absolute() {
        at.to_path_buf()
    } else {
        root.join(at)
    };
    let anchor = absolute
        .ancestors()
        .find(|candidate| candidate.exists())
        .unwrap_or(root);
    let resolved = anchor
        .canonicalize()
        .map_err(|error| format!("cannot resolve {}: {error}", anchor.display()))?;
    let root = root
        .canonicalize()
        .map_err(|error| format!("cannot resolve the project root: {error}"))?;
    if !resolved.starts_with(&root) {
        return Err(format!(
            "{} is outside the project root {} — refused. This binary writes gigabytes, \
             and a path outside the repository puts them somewhere nobody will think to \
             look for them",
            absolute.display(),
            root.display()
        ));
    }
    Ok(absolute)
}


/// What makes one fan-out alarm the same condition as another.
///
/// # Why this is not the message
///
/// It was the message, and the message reads "...across {batches} batches". That count rises
/// every round, so every rendering was a new string, so the set that exists to report a
/// standing condition **once** reported it every round --- 168 times in a forty-five-minute
/// run, which is exactly what its own comment says not to do.
///
/// The condition is the shape of the strain, not the tally of how long it has been observed:
/// which table, how wide the average batch is, and the widest seen. Rounding the average to
/// a whole partition is deliberate --- an alarm that re-fires because a mean moved by a
/// hundredth is the same defect in slower motion.
///
/// A *worsening* condition is a different condition and is reported again, which is why the
/// widest batch is part of the key rather than only the table.
fn fan_out_condition(table: usize, strain: &Strain) -> String {
    let average = strain.average_fan_out().unwrap_or(0.0);
    format!("{table}:{average:.0}:{}", strain.widest_batch)
}

/// Run a soak.
///
/// **A test, not a binary.** A soak is a test of the product, so it must not add
/// infrastructure of its own: it drives `sankhya-publish` for every write and
/// `sankhya-maintenance` for every merge, and owns nothing that could drift from them. An
/// earlier version shipped as `sankhya-soak` with its own writer --- and the consequence was
/// a harness that could not have caught the defect living in the write path it bypassed.
///
/// Ignored by default, because it takes forty-five minutes. Run it deliberately:
///
/// ```text
/// SANKHYA_SOAK_MINUTES=45 cargo test -p sankhya-soak --test soak_run -- --ignored --nocapture
/// ```
///
/// Configured by environment rather than by arguments, because a test harness has no argv of
/// its own.
#[test]
#[ignore = "a soak takes forty-five minutes; run it deliberately"]
fn soak() {
    let env = |name: &str| std::env::var(name).ok();
    // Twenty gigabytes by default, doubled from ten by owner decision on 2026-08-29.
    //
    // The figure is a floor on how much data the run cannot hold in page cache, which is what
    // makes a read a read. Ten gigabytes was chosen when the soak did not read the data at
    // all; now that it does --- 3.01 billion rows in the run that prompted this --- the
    // working set matters and a bigger one is a harder test of the same machinery.
    //
    // Cost is linear and mostly in the fill: roughly forty-five seconds per gigabyte, so the
    // preamble goes from about seven minutes to about fifteen. The judged window is unchanged,
    // because it is counted from when measurement starts.
    //
    // Read from the library rather than parsed here, because two of the thresholds a run is
    // judged against are arithmetic on this figure. When the harness kept its own copy the
    // doubling landed here and nowhere else, and the next run aborted against a budget
    // written for half the data.
    let Scale { gb, tables } = Scale::declared();
    let minutes: u64 = env("SANKHYA_SOAK_MINUTES")
        .and_then(|v| v.parse().ok())
        .unwrap_or(45);
    let schema = env("SANKHYA_SOAK_SCHEMA").unwrap_or_else(|| "soak".to_string());

    let Some(root) = project_root() else {
        panic!("cannot find the project root from here");
    };
    // Defaulted inside the project, and confined to it.
    let at = env("SANKHYA_SOAK_AT").map_or_else(|| root.join(DEFAULT_WAREHOUSE), PathBuf::from);
    let at = match confined(&root, &at) {
        Ok(path) => path,
        Err(why) => panic!("{why}"),
    };

    println!("{}  soak starting", stamp());
    println!("{}    target {gb} GB across {tables} table(s), {minutes} minute(s)", stamp());
    println!("{}    warehouse {} (schema {schema})", stamp(), at.display());

    // Emptied first. A run that inherits the previous run's files starts with a file count
    // and a page cache it did not create, and the memory reading in the first minutes then
    // describes the last run as much as this one. Only the warehouse goes --- the log and the
    // report beside it are the evidence.
    if at.exists() {
        if let Err(error) = std::fs::remove_dir_all(&at) {
            eprintln!("{}  cannot clear {}: {error}", stamp(), at.display());
            std::process::exit(2);
        }
        println!("{}    cleared the previous warehouse", stamp());
    }
    if let Err(error) = std::fs::create_dir_all(&at) {
        eprintln!("{}  cannot use {}: {error}", stamp(), at.display());
        std::process::exit(2);
    }

    // --- fill ------------------------------------------------------------
    let filling = Instant::now();
    let per_table_bytes = (gb * 1024.0 * 1024.0 * 1024.0) / tables as f64;
    let mut roots = Vec::new();
    for table in 0..tables {
        let name = table_name(table);
        let root = at.join(&schema).join(&name);
        create_table(&root);
        let written = fill(&root, per_table_bytes, &filling);
        println!(
            "{}    {name} filled, {:.2} GB",
            stamp(),
            written as f64 / (1024.0 * 1024.0 * 1024.0)
        );
        roots.push(root);
    }
    println!(
        "{}  fill complete in {:.0}s",
        stamp(),
        filling.elapsed().as_secs_f64()
    );

    // --- the cube path ---------------------------------------------------
    //
    // Declared over the first table, so the run exercises what M7 built rather than
    // reporting on M6's surface and calling it M7. A soak that never hydrates a cube says
    // nothing about whether cells leak, whether cuboids accumulate, or whether hydrating
    // under sustained write load competes with compaction --- and those are exactly the
    // questions a soak exists to answer.
    //
    // Declared through the catalogue, hydrated through `publish_from_fact_table`, queried
    // through the registered table functions. Every one is the product's own API: a soak is
    // a client, and a client that reimplements the thing it is testing tests its own copy.
    if let Some(first) = roots.first() {
        declare_soak_cube(&at, first);
    }

    // --- maintenance -----------------------------------------------------
    //
    // Started here and then left alone. The warehouse compacts and retires on its own
    // thread; this harness writes and reads and has no idea when either happens, which is
    // exactly the relationship a client has with a real deployment.
    //
    // The interval is the harness's --- a soak that waited thirty seconds between ticks
    // would spend most of forty-five minutes not maintaining anything --- and every
    // *decision* inside the tick is the product's.
    let maintenance = spawn_maintenance(
        roots.clone(),
        MaintenancePolicy {
            interval: Duration::from_secs(2),
            ..MaintenancePolicy::default()
        },
    );

    // --- run -------------------------------------------------------------
    let started = Instant::now();
    let deadline = Duration::from_secs(minutes * 60);
    let mut samples = Samples::new();
    let mut last_report = Instant::now();
    let mut round = 0_u64;

    let mut planned = 0_u64;
    let mut published = 0_u64;
    let mut refused = 0_u64;
    let mut scanned = Scanned::default();
    let mut unread = 0_u64;
    // How many cube navigations answered.
    let mut cube_rounds = 0_u64;
    // One runtime for the whole run. Built per navigation, it created and dropped
    // thread-local state forty times over forty-five minutes, and the allocator kept the
    // high-water mark --- which reads, from outside, exactly like a leak in the product.
    let cube_runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime for the cube path");
    // One accumulator per table, living for the whole run: deferral only works if what was
    // deferred is still there next round.
    let publications: Vec<Publication> = roots.iter().map(|root| publication(root)).collect();
    // Reported once each, not once a round: a standing condition printed every fifteen
    // seconds is a condition nobody reads. Keyed by [`fan_out_condition`], because the
    // rendered message carries a running batch count and so is never the same twice.
    let mut fan_out_reported: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();
    let mut accumulators: Vec<Accumulator<'_>> = publications
        .iter()
        .map(|publication| Accumulator::new(publication, FanOut::default()))
        .collect();

    while started.elapsed() < deadline {
        round += 1;

        // Writes: one batch per table per round, through the fan-out accumulator.
        //
        // Through the *same* guard the fill uses. An earlier version had the fill guarded
        // and the measured loop unguarded, so the run that mattered wrote one file per
        // partition per round --- ninety files a round --- and measured a path production
        // does not use. A harness that guards the setup and not the measurement is measuring
        // the wrong thing.
        for (index, root) in roots.iter().enumerate() {
            let _ = root;
            let Some(accumulator) = accumulators.get_mut(index) else {
                continue;
            };
            if append_one(accumulator, round) {
                published += 1;
            } else {
                refused += 1;
            }
        }

        // Planning: replay every table's log, which is what planning actually costs.
        for root in &roots {
            if live_files(root).is_ok() {
                planned += 1;
            }
        }

        // Reads: actually decode data.
        //
        // The first version of this loop counted a log replay as a "query" and never read a
        // row. The dataset was ten gigabytes on disk that nothing scanned, so the run
        // measured the append and log paths and reported a figure that sounded like it
        // measured the read path too. A soak that names its workload after work it does not
        // do is worse than one that measures less and says so.
        //
        // Bounded per round, and rotating, so the whole dataset is covered many times over a
        // long run without any single round taking minutes.
        let read = roots
            .get(round as usize % roots.len())
            .map_or_else(Scanned::default, |root| scan_some(root, round));
        if read.rows == 0 {
            // "Could not read" is not "read nothing": a scan that silently returned no rows
            // would make every throughput figure below a report about an empty loop.
            unread += 1;
        }
        scanned = scanned.and(read);

        // Hydrate and navigate the cube, on the same rotation as the scan.
        //
        // Its cost lands in measures that already exist: cells are held in this process, so
        // a cube-side leak shows in `resident_bytes`; cuboids are written under the
        // warehouse, so their population shows in `warehouse_bytes`. No new measure is
        // needed, and adding one nothing distinguishes would be a measure to keep supplied
        // for nothing.
        if round % 4 == 0 {
            if let Some(first) = roots.first() {
                cube_rounds += u64::from(navigate_the_cube(&cube_runtime, first));
            }
        }

        // The fan-out alarm, which ARCHITECTURE §6.4.2 calls the important one: "the guards
        // buy time; the alarm gets the design fixed. Silently absorbing it would be the
        // failure." A soak that runs the guards and never reports the strain is doing the
        // absorbing.
        for (table, accumulator) in accumulators.iter().enumerate() {
            if let Some(why) = accumulator.strain().explain(&FanOut::default()) {
                if fan_out_reported.insert(fan_out_condition(table, accumulator.strain())) {
                    println!("{}  FAN-OUT  {why}", stamp());
                }
            }
        }

        // No maintenance here. The warehouse maintains itself.
        //
        // This harness used to group files by partition, choose a compaction policy, merge,
        // and commit the `Remove` actions by hand --- and it forgot to retire the inputs
        // afterwards, which is how a run targeting ten gigabytes consumed sixty. That was
        // the predictable end of a test performing surgery on a warehouse: a soak is a
        // client, and a client that knows the order the maintenance steps go in is a client
        // that can get it wrong.
        //
        // `sankhya_maintenance::spawn_maintenance` runs above, on the warehouse's own thread.
        // What the soak does now is what a soak should do: write, read, and watch.

        // A run that is not writing is not soaking anything. Reported at once rather than
        // discovered four hours later in a report full of steady measures.
        if refused > 0 {
            eprintln!(
                "{}  ABORTING: {refused} write(s) refused and {published} published — this \
                 run would measure an idle warehouse and report it healthy",
                stamp()
            );
            std::process::exit(2);
        }

        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        let at_micros = started.elapsed().as_micros() as i64;
        samples.record("resident_bytes", at_micros, resident_bytes());
        samples.record("open_files", at_micros, open_files());
        samples.record("metric_series", at_micros, Some(0.0));
        samples.record(
            "history_bytes",
            at_micros,
            Some(file_bytes(&artefacts(&at).join("diagnostic-history.tsv")).unwrap_or(0.0)),
        );
        // What this run is consuming. The previous run filled the disk and died writing its
        // own log; nothing was watching the one resource it actually ran out of.
        samples.record(
            "warehouse_bytes",
            at_micros,
            sankhya_diagnostic::soak::sample::tree_bytes(&at),
        );
        // Stop before the budget is spent, not after.
        //
        // A judged report is written every fifteen minutes, so a breach is *reported* --- but
        // the run that discovered this was writing its report onto a full disk, and a report
        // written onto a full disk is zero bytes. Being right about the finding is worth
        // nothing if recording it is the operation that fails.
        //
        // So the budget is enforced here, in the loop, against the same declaration the
        // report judges against. The run ends, the report is written while there is still
        // room to write it, and the evidence survives the finding.
        if let Some(consumed) = sankhya_diagnostic::soak::sample::tree_bytes(&at) {
            if consumed > warehouse_budget() {
                emit(
                    &samples, &at, started.elapsed(), round, planned, worst_table(&roots),
                    published, &scanned, unread,
                );
                eprintln!(
                    "{}  ABORTING: the warehouse holds {:.1} GB against a budget of {:.1} GB. \
                     Reclamation is not keeping up with what this run writes --- which is the \
                     finding, and it is recorded above rather than lost to a full disk.",
                    stamp(),
                    consumed / 1024.0 / 1024.0 / 1024.0,
                    warehouse_budget() / 1024.0 / 1024.0 / 1024.0,
                );
                std::process::exit(3);
            }
        }

        #[allow(clippy::cast_precision_loss)]
        samples.record("queries", at_micros, Some(planned as f64));
        samples.record("scanned_rows", at_micros, Some(scanned.rows as f64));
        #[allow(clippy::cast_precision_loss)]
        samples.record("audit_records", at_micros, Some(planned as f64));
        let live = worst_table(&roots);
        #[allow(clippy::cast_precision_loss)]
        samples.record("live_files", at_micros, Some(live as f64));

        if last_report.elapsed() >= REPORT_EVERY {
            emit(&samples, &at, started.elapsed(), round, planned, live, published, &scanned, unread);
            last_report = Instant::now();
        }
        std::thread::sleep(SAMPLE_EVERY);
    }

    let live = worst_table(&roots);
    // Reported, and asserted. A soak that declared a cube and never navigated it says
    // nothing about the cube path --- which is the failure of reporting on one milestone's
    // surface while calling it another's, and it would look exactly like a clean run.
    println!("{}  the cube answered {cube_rounds} time(s)", stamp());
    // Asserted rather than merely printed, because a zero here is invisible in a report
    // full of healthy measures.
    //
    // No mutation entry claims coverage of it: this test is `#[ignore]`d, so the suite the
    // audit runs never reaches the assertion, and an entry saying otherwise would be a claim
    // nothing checks. The guard fires when somebody runs the soak, which is when it matters.
    assert!(
        cube_rounds > 0,
        "the cube was declared and never navigated; this run judges nothing about it"
    );
    println!(
        "{}  maintenance ran {} tick(s) and reclaimed {:.2} GB",
        stamp(),
        maintenance.ticks(),
        maintenance.bytes_reclaimed() as f64 / (1024.0 * 1024.0 * 1024.0)
    );
    maintenance.stop();
        emit(&samples, &at, started.elapsed(), round, planned, live, published, &scanned, unread);
    println!("{}  soak finished after {minutes} minute(s)", stamp());
}

/// The live file count of the table that has the most.
///
/// # The maximum, not the sum, and the difference is a false finding
///
/// `live_files` is declared with a limit of a thousand and a meaning — *a scan pays per file*
/// — that is a property of **one table**. Summing across ten tables produces a number in
/// different units from its own threshold, and the first judged report of the first real run
/// duly reported `BREACHED — 4900 count is already past the limit` when every table held
/// about four hundred and ninety, comfortably under.
///
/// The maximum is what the limit is about: a query reads one table, and pays for that
/// table's files. It also still catches the failure the measure exists for, because a table
/// falling behind raises the maximum whether or not the others do — where a sum can hide one
/// table's ramp inside nine tables' noise.
/// The space this run is entitled to, read from the declaration rather than restated.
///
/// Stated in one place so the loop that enforces the budget and the report that judges it
/// can never disagree. A harness carrying its own copy of a threshold is a harness that
/// will one day abort at a figure the report calls healthy.
fn warehouse_budget() -> f64 {
    sankhya_diagnostic::soak::measure::WATCHED
        .iter()
        .find_map(|w| match w {
            Watched { name: "warehouse_bytes", bound: Bound::Steady { limit }, .. } => Some(*limit),
            _ => None,
        })
        .expect("warehouse_bytes is declared as a steady measure with a budget")
}

fn worst_table(roots: &[PathBuf]) -> usize {
    roots
        .iter()
        .filter_map(|root| live_files(root).ok())
        .map(|set| set.files.len())
        .max()
        .unwrap_or(0)
}

/// Print progress and write the judged report.
///
/// Written every interval rather than only at the end: a run killed at hour nine should leave
/// hour eight's verdict behind rather than nothing at all.
fn emit(
    samples: &Samples,
    at: &Path,
    elapsed: Duration,
    round: u64,
    queries: u64,
    live: usize,
    published: u64,
    scanned: &Scanned,
    unread: u64,
) {
    let horizon = supported_horizon(samples);
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    let now = elapsed.as_micros() as i64;
    let report = Report::of(samples, horizon, now);
    let rss = resident_bytes().unwrap_or(0.0) / (1024.0 * 1024.0);
    println!(
        "{}  t+{:>5.0}s  round {round:<5} published {published:<6} planned {queries:<7} \
         scanned {:>6.1} GB/{:<9} live_files {live:<6} rss {rss:>6.0} MB  {}{}  \
         (horizon {}s)",
        stamp(),
        elapsed.as_secs_f64(),
        scanned.bytes as f64 / (1024.0 * 1024.0 * 1024.0),
        scanned.rows,
        if report.passed() { "PASS" } else { "watching" },
        // A round that read nothing is reported rather than averaged away. Throughput over
        // an empty loop is the figure this whole harness exists not to produce.
        if unread == 0 {
            String::new()
        } else {
            format!("  UNREAD {unread}")
        },
        horizon
    );
    for (measure, verdict) in &report.verdicts {
        if !verdict.passed() {
            println!("{}      {:<16} {verdict:?}", stamp(), measure.name);
        }
    }
    std::fs::write(artefacts(at).join("soak-report.txt"), report.describe()).ok();
}

/// Seconds since the epoch, and a readable clock time beside it.
fn stamp() -> String {
    let since = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let seconds_today = since % 86_400;
    format!(
        "[{:02}:{:02}:{:02}Z]",
        seconds_today / 3600,
        (seconds_today % 3600) / 60,
        seconds_today % 60
    )
}

/// Print why the run cannot proceed, and stop.
///
/// Rather than `expect`. A soak that panics on a setup failure dies with a stack trace and a
/// thread name — which is the mirror of the mistake this binary already made in the other
/// direction, where swallowing a write error let it soak an idle warehouse and call it
/// healthy. Neither extreme says what went wrong; both waste the run.
#[allow(clippy::panic)]
fn die(what: &str) -> ! {
    eprintln!("{}  cannot start: {what}", stamp());
    std::process::exit(2)
}

/// The soak's table, which is a *conforming* analytical table.
///
/// `FR-STORE-20` requires every analytical table to carry `sank_data_date` and be
/// partitioned on it, with no exemption for size or purpose. An earlier version of this
/// harness had `id, region, payload` and no date at all --- a warehouse that could not have
/// passed the product's own requirement, which is exactly why it never exercised the
/// partitioning code and never caught the defect that lived there.
///
/// `event_date` is the source column the axis is declared on. The `sank_data_date` column
/// itself is added by `sankhya-publish`, not here: a harness that writes it by hand would be
/// asserting against its own arithmetic rather than the product's.
fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("region", DataType::Utf8, true),
        Field::new("event_date", DataType::Date32, false),
        Field::new("payload", DataType::Utf8, false),
    ]))
}

/// How many days of data a table spreads across.
///
/// Ninety, so the run exercises a realistic number of partitions --- enough that partition
/// directories, per-partition compaction and the log's partition-value records all carry
/// weight, and not so many that the fill spends its time on directory creation.
const DAYS: i64 = 90;

/// The first day of the soak's date range, as days since the Unix epoch.
///
/// Fixed rather than derived from the clock, so two runs produce the same partitions and a
/// comparison between them is a comparison of the system rather than of the calendar.
const FIRST_DAY: i32 = 19_723;

fn create_table(root: &Path) {
    // Only if it is not already a table. Resuming a soak against a warehouse that is
    // already filled has to be cheap, or fixing the harness costs three and a half minutes
    // of regeneration every time — and paying that makes the tempting move "do not fix it".
    if root.join("_delta_log").is_dir() {
        return;
    }
    if let Err(error) = std::fs::create_dir_all(root) {
        die(&format!("{} could not be created: {error}", root.display()));
    }
    if let Err(error) = publication(root).create(&schema()) {
        die(&format!("{} could not be created: {error}", root.display()));
    }
}

/// The publication a soak table is written through.
///
/// **Through `sankhya-publish`, not through `write_parquet` directly.** The harness used to
/// own its write path, and the consequence was a soak that never ran the code that ships:
/// the publish path declared a partition column it never wrote for months, and no soak could
/// have noticed because no soak went near it. A harness that exercises its own writer is
/// measuring the harness.
fn publication(root: &Path) -> Publication {
    Publication::external(root, "soak").dated_by("event_date")
}

/// How the rows in a batch are dated.
///
/// # Why one harness needs both
///
/// The soak used the same shape for the fill and for the steady-state rounds: every row's
/// date was `id % 90`, so **every** batch touched all ninety partitions, for the whole run.
/// That is a bulk backfill, and it is a real workload --- it is what the fill is. It is not
/// what arrival looks like afterwards.
///
/// A source feeding a warehouse continuously produces rows dated *now*. A batch of those
/// touches one partition, or two across a midnight. Modelling arrival as a uniform spread
/// over ninety days made the fan-out alarm fire for all 168 rounds of a forty-five-minute
/// run --- correctly, given what it was shown, and about a workload no source produces.
///
/// The alarm's advice is "the partition scheme is the thing to change". Shown a real arrival
/// pattern it would say nothing, and the daily axis `FR-STORE-20` mandates would be exactly
/// right. So the harness models both and the alarm becomes informative rather than constant.
#[derive(Clone, Copy)]
enum Dating {
    /// Spread uniformly across the whole range: a backfill, and what the fill genuinely is.
    Backfill,
    /// Concentrated on the newest day, which is what a live source produces.
    ///
    /// Not *only* the newest: a small tail lands on the day before, because a real source
    /// has rows in flight across midnight and a harness that never produces one would not
    /// exercise the two-partition commit at all.
    Arriving,
}

impl Dating {
    /// The day offset within the range for one row.
    fn day_for(self, row: i64, newest: i64) -> i32 {
        match self {
            Self::Backfill => i32::try_from(row.rem_euclid(DAYS)).unwrap_or(0),
            // One row in thirty-two lands on the previous day.
            Self::Arriving => {
                let day = if row.rem_euclid(32) == 0 {
                    newest.saturating_sub(1)
                } else {
                    newest
                };
                i32::try_from(day.rem_euclid(DAYS)).unwrap_or(0)
            }
        }
    }
}

/// Declare a cube over the soak's own table.
///
/// The soak's schema is `id`, `region`, `event_date`, `payload`, and a cube needs a numeric
/// measure --- so `id` is the measure, summed. It is a meaningless total and an entirely
/// real exercise of hydration, consolidation and the roll-up path, which is what a soak is
/// for.
fn declare_soak_cube(warehouse: &Path, table_root: &Path) {
    use sankhya_cube::algo::{Along, Measure, Rule};
    use sankhya_cube::model::{Definition, Dimension, Level};

    let table = table_root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("soak")
        .to_string();
    let definition = Definition::new(
        "soak_cube",
        table.clone(),
        vec![Dimension {
            name: "region".to_string(),
            table,
            joins_on: "region".to_string(),
            levels: vec![Level::new("area", "region")],
            rollups: None,
            parent_child: None,
        }],
        vec![Measure::new("id", vec![Along::new("region", Rule::Sum)])],
    );
    if let Err(error) = sankhya_cube::catalogue::save(warehouse, &definition) {
        eprintln!("{}  the cube could not be declared: {error}", stamp());
    }
}

/// Hydrate the cube from the table and roll it up, returning whether it answered.
///
/// The runtime is the caller's. Building one **per call** --- which this did --- creates and
/// drops thread-local state forty times over a run, and an allocator does not return that
/// promptly. It is a test doing infrastructure work, which is the thing the golden rule
/// exists to catch, and it was in the harness rather than the product.
///
/// # Why this is worth doing every few rounds
///
/// Hydration reads the whole fact table, so doing it every round would make the soak a
/// measurement of hydration rather than of the system. Every fourth round exercises the path
/// --- cells built, held, dropped --- often enough that a leak in it accumulates visibly over
/// forty-five minutes, and rarely enough that the write and compaction paths still dominate.
fn navigate_the_cube(runtime: &tokio::runtime::Runtime, table_root: &Path) -> bool {
    use datafusion::prelude::SessionContext;

    let Ok(definition) = sankhya_cube::catalogue::load(
        table_root.parent().and_then(Path::parent).unwrap_or(table_root),
        "soak_cube",
    ) else {
        return false;
    };
    let Ok(cube) = definition.validate() else {
        return false;
    };
    let Some(measure) = cube.measures().first().cloned() else {
        return false;
    };

    let Ok(table) = sankhya_readpath::resolve(
        schema(),
        table_root,
        sankhya_types::LsnRange::new(Lsn::new(0), Lsn::new(u64::MAX)),
        None,
        Lsn::new(u64::MAX),
    ) else {
        return false;
    };

    let context = SessionContext::new();
    let name = table_root
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("soak");
    if context.register_table(name, Arc::new(table)).is_err() {
        return false;
    }
    let catalog = Arc::new(sankhya_cube_sql::catalog::CubeCatalog::new());
    // A log per navigation, discarded with it. The soak measures the cube path rather than
    // driving selection, and a log that outlived the call would be state the harness holds
    // on the product's behalf --- which is what a test must not do.
    sankhya_cube_sql::functions::register(
        &context,
        Arc::clone(&catalog),
        Arc::new(sankhya_cube::querylog::QueryLog::new()),
    );

    runtime.block_on(async {
        if sankhya_cube_sql::publish::publish_from_fact_table(
            &context,
            &catalog,
            "soak_cube",
            Arc::new(cube),
            &measure,
            1,
        )
        .await
        .is_err()
        {
            return false;
        }
        context
            .sql("SELECT * FROM cube_rollup('soak_cube', 'id', 'by=region')")
            .await
            .ok()
            .is_some()
    })
}

/// One batch of rows, sized so a file is a few megabytes.
fn batch(from: i64, rows: usize) -> RecordBatch {
    batch_dated(from, rows, Dating::Backfill, 0)
}

/// One batch of rows, dated by the given policy.
fn batch_dated(from: i64, rows: usize, dating: Dating, newest: i64) -> RecordBatch {
    let ids: Vec<i64> = (0..rows as i64).map(|i| from + i).collect();
    let regions: Vec<Option<&str>> = ids
        .iter()
        .map(|i| match i % 4 {
            0 => Some("north"),
            1 => Some("south"),
            2 => Some("east"),
            _ => None,
        })
        .collect();
    // Incompressible enough that the file size tracks the row count rather than the
    // compressor's mood, which is what makes a byte target predictable.
    let payloads: Vec<String> = ids
        .iter()
        .map(|i| format!("{:x}{:x}{:x}", i.wrapping_mul(2_654_435_761), i, i ^ 0x5A5A))
        .collect();
    // Spread across the range rather than all on one day, so a batch genuinely spans
    // partitions and the write path has to split it.
    let dates: Vec<i32> = ids
        .iter()
        .map(|i| FIRST_DAY + dating.day_for(*i, newest))
        .collect();
    match RecordBatch::try_new(
        schema(),
        vec![
            Arc::new(Int64Array::from(ids)),
            Arc::new(StringArray::from(regions)),
            Arc::new(Date32Array::from(dates)),
            Arc::new(StringArray::from(payloads)),
        ],
    ) {
        Ok(batch) => batch,
        // Every array here is built from the same schema at the same length, so this cannot
        // arise from the data. It would mean the schema and the builders disagree, which is
        // a defect rather than a condition, and the run cannot continue past it.
        Err(error) => die(&format!("the generated batch does not match its schema: {error}")),
    }
}

/// A name for the nth soak table.
///
/// Named rather than numbered, because `t07` in a failure report tells you nothing and a
/// warehouse of `t00..t09` is indistinguishable from a scratch directory somebody forgot to
/// delete. The names are deliberately ordinary English --- this is a core crate, and
/// `check-vocabulary` refuses domain vocabulary here so that any one industry stays a use
/// case rather than leaking into the engine. (The first version of this comment named two
/// such industries and was refused by that check, which is the check being exactly right.)
///
/// Beyond the list the suffix carries the number, so a run of five hundred tables still has
/// names a person can read and a machine can order.
fn table_name(index: usize) -> String {
    const NAMES: [&str; 10] = [
        "events",
        "sessions",
        "requests",
        "documents",
        "readings",
        "samples",
        "entries",
        "items",
        "records",
        "batches",
    ];
    match NAMES.get(index) {
        Some(name) => (*name).to_string(),
        None => {
            let stem = NAMES.get(index % NAMES.len()).copied().unwrap_or("table");
            format!("{stem}_{:03}", index / NAMES.len())
        }
    }
}

/// Decode a bounded, rotating slice of one table's live files.
///
/// **Bounded**, because scanning ten gigabytes every round would make a round take minutes
/// and the sampling useless. **Rotating**, because scanning the same slice every round would
/// exercise one file and the page cache, which is not the read path.
///
/// The window advances with the round, so over a long run every file is read many times and
/// the whole dataset is covered rather than the newest corner of it.
fn scan_some(root: &Path, round: u64) -> Scanned {
    const BYTES_PER_ROUND: u64 = 192 * 1024 * 1024;

    let Ok(set) = live_files(root) else {
        return Scanned::default();
    };
    if set.files.is_empty() {
        return Scanned::default();
    }
    let mut names: Vec<&str> = set.files.iter().map(|f| f.path.as_str()).collect();
    names.sort_unstable();

    let start = (round as usize).wrapping_mul(7) % names.len();
    let mut total = Scanned::default();
    for offset in 0..names.len() {
        if total.bytes >= BYTES_PER_ROUND {
            break;
        }
        let Some(name) = names.get((start + offset) % names.len()) else {
            break;
        };
        if let Ok(one) = scan_parquet(&root.join(name)) {
            total = total.and(one);
        }
    }
    total
}

/// Write files until the table holds roughly `target_bytes`.
///
/// Resumes rather than restarts. The count starts from what the table already holds and the
/// version from its log, so a run against an already-filled warehouse writes nothing — which
/// is what makes it cheap to restart a soak after fixing the harness, rather than paying
/// three and a half minutes to regenerate ten gigabytes that are already there.
fn fill(root: &Path, target_bytes: f64, since: &Instant) -> u64 {
    const ROWS_PER_FILE: usize = 200_000;
    let publication = publication(root);
    let mut accumulator = Accumulator::new(&publication, FanOut::default());
    let existing = live_files(root).ok();
    let mut written: u64 = existing
        .as_ref()
        .map_or(0, sankhya_table_delta::LiveSet::total_bytes);
    let mut version = next_version(root).max(1);
    #[allow(clippy::cast_possible_wrap)]
    let mut row = existing
        .as_ref()
        .map_or(0_i64, |set| (set.files.len() * ROWS_PER_FILE) as i64);
    if written as f64 >= target_bytes {
        println!(
            "{}      already holds {:.2} GB, not refilling",
            stamp(),
            written as f64 / (1024.0 * 1024.0 * 1024.0)
        );
    }
    while (written as f64) < target_bytes {
        let name = format!("part-{version:06}.parquet");
        // Through `sankhya-publish`, which splits the batch across the partitions its dates
        // fall in and commits every file in one version. A batch of two hundred thousand
        // rows spread over ninety days becomes ninety files, which is what a real fill does.
        // Through the fan-out accumulator, not straight at `append`.
        //
        // A batch of two hundred thousand rows spread over ninety days touches ninety
        // partitions, and an unguarded append writes ninety files for it. The first run of
        // this harness against the partitioned write path reached **32,279 live files in
        // four minutes**, averaging 37 KB, and the judge breached `live_files` --- correctly,
        // and about a defect this harness had just been given the ability to see.
        let published = match accumulator.absorb(
            &name,
            &batch(row, ROWS_PER_FILE),
            Lsn::new(version),
        ) {
            Ok(published) => published,
            Err(error) => die(&format!("{} could not be filled: {error}", root.display())),
        };
        written += published.iter().map(|p| p.bytes).sum::<u64>();
        row += ROWS_PER_FILE as i64;
        version += 1;
        if version % 5 == 0 {
            println!(
                "{}      {:.2} GB after {:.0}s",
                stamp(),
                written as f64 / (1024.0 * 1024.0 * 1024.0),
                since.elapsed().as_secs_f64()
            );
        }
    }
    // Whatever is still deferred must be written before the run starts measuring, or the
    // warehouse is short by however much was waiting and every later figure is about a
    // smaller dataset than the one asked for.
    if let Ok(flushed) = accumulator.flush(&format!("part-{version:06}.parquet"), Lsn::new(version)) {
        written += flushed.iter().map(|p| p.bytes).sum::<u64>();
    }
    written
}

/// The version a table's next commit must take.
///
/// Read from the log rather than counted in this process. Commit versions are contiguous by
/// protocol, and a global counter starting at some round number is rejected by every table
/// — which is precisely what happened: the first run of this binary wrote Parquet files for
/// three minutes and published **none** of them, because every commit was refused and the
/// error was thrown away.
fn next_version(root: &Path) -> u64 {
    live_files(root)
        .ok()
        .and_then(|set| set.version)
        .map_or(0, |version| version + 1)
}

/// One more file, published the way anything else publishes.
///
/// Returns whether it landed. **Nothing here is swallowed**: a harness that discards write
/// errors measures an idle system and reports that it is healthy, which is the exact class
/// of failure a soak exists to find, committed in the soak.
fn append_one(accumulator: &mut Accumulator<'_>, sequence: u64) -> bool {
    let name = format!("live-{sequence:06}.parquet");
    let from = i64::try_from(sequence).unwrap_or(0) * 10_000;
    // Arriving, not backfilling. The fill above lays down ninety days of history; what
    // happens *after* it is a source feeding rows dated now, and dating those across ninety
    // partitions modelled a workload nothing produces --- while making the fan-out alarm
    // fire every round about it.
    //
    // The newest day advances with the run, so the hot partition moves and compaction has to
    // keep up with a partition that is being appended to rather than one that is finished.
    let newest = i64::try_from(sequence).unwrap_or(0) / ROUNDS_PER_DAY;
    accumulator
        .absorb(
            &name,
            &batch_dated(from, 5_000, Dating::Arriving, newest),
            Lsn::new(sequence.saturating_add(1)),
        )
        .is_ok()
}

/// How many rounds of arrival make up a day of the soak's calendar.
///
/// The clock is the round counter, not the wall clock: a forty-five-minute run has to cross
/// a day boundary several times or it never exercises a partition going cold, and it must
/// not cross one every round or every partition is cold immediately.
const ROUNDS_PER_DAY: i64 = 24;

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::{confined, table_name};
    use sankhya_publish::Strain;
    use std::path::Path;

    /// A warehouse outside the project root is refused.
    ///
    /// Tested here rather than by running the binary. The guard exists because this project
    /// writes nothing outside its own root, and the way that rule got broken a third time
    /// was somebody checking the guard by invoking the thing that writes --- before it had
    /// compiled in. A guard against writing must never be verified by writing.
    /// Arrival touches one partition, or two across a midnight.
    ///
    /// The soak dated every row `id % 90` for the whole run, so every batch touched all
    /// ninety partitions and the fan-out alarm fired 168 times in forty-five minutes. It was
    /// right about what it was shown; what it was shown was a backfill labelled as arrival.
    #[test]
    fn arriving_rows_land_on_the_newest_day_and_the_one_before() {
        let days: std::collections::BTreeSet<i32> = (0..10_000)
            .map(|row| super::Dating::Arriving.day_for(row, 40))
            .collect();
        assert_eq!(
            days.len(),
            2,
            "a live source produces rows dated now, and a few in flight across midnight: {days:?}"
        );
        assert!(days.contains(&40) && days.contains(&39));
    }

    /// And a backfill genuinely spans the range, which is what the fill is.
    #[test]
    fn a_backfill_spans_every_partition() {
        let days: std::collections::BTreeSet<i32> = (0..10_000)
            .map(|row| super::Dating::Backfill.day_for(row, 0))
            .collect();
        assert_eq!(
            days.len(),
            usize::try_from(super::DAYS).expect("small"),
            "the fill lays down history and must touch every partition, or per-partition \
             compaction is never exercised"
        );
    }

    /// The newest day advances, so the hot partition moves rather than growing forever.
    #[test]
    fn the_hot_partition_moves_as_the_run_goes_on() {
        let early = super::Dating::Arriving.day_for(1, 0);
        let later = super::Dating::Arriving.day_for(1, 5);
        assert_ne!(
            early, later,
            "a partition that is appended to for the whole run is never compacted as a cold \
             one, and the two paths behave differently"
        );
    }

    /// A standing condition is one condition however long it stands.
    ///
    /// The alarm keyed itself on its own message, and the message counts batches, so the
    /// count made every rendering unique and the "report once" set reported 168 times in a
    /// forty-five-minute run. Asserted on the *key* rather than on captured output, because
    /// the defect was in the key and output capture would have hidden it behind formatting.
    #[test]
    fn an_alarm_that_stands_for_longer_is_still_the_same_alarm() {
        let early = Strain {
            batches: 3,
            partitions_touched: 270,
            widest_batch: 90,
            ..Strain::default()
        };
        let later = Strain {
            batches: 168,
            partitions_touched: 15_120,
            ..early
        };
        assert_eq!(
            super::fan_out_condition(0, &early),
            super::fan_out_condition(0, &later),
            "the same strain observed for longer must key the same, or the alarm fires every \
             round and stops being read"
        );
    }

    /// A worse condition is a different condition, and is worth saying again.
    #[test]
    fn a_widening_fan_out_is_reported_again() {
        let before = Strain {
            batches: 10,
            partitions_touched: 300,
            widest_batch: 30,
            ..Strain::default()
        };
        let worse = Strain {
            widest_batch: 90,
            ..before
        };
        assert_ne!(
            super::fan_out_condition(0, &before),
            super::fan_out_condition(0, &worse),
            "a fan-out that got wider is news"
        );
    }

    /// Two tables straining independently are two alarms.
    #[test]
    fn each_table_reports_its_own_strain() {
        let strain = Strain {
            batches: 10,
            partitions_touched: 900,
            widest_batch: 90,
            ..Strain::default()
        };
        assert_ne!(
            super::fan_out_condition(0, &strain),
            super::fan_out_condition(1, &strain),
            "one table's alarm must not silence another's"
        );
    }

    #[test]
    fn a_warehouse_outside_the_project_root_is_refused() {
        let root = Path::new("/tmp/some-project");
        assert!(confined(root, Path::new("/var/tmp/elsewhere")).is_err());
        assert!(confined(root, Path::new("/etc")).is_err());
    }

    #[test]
    fn a_relative_warehouse_resolves_under_the_root() {
        let root = std::env::current_dir().expect("a working directory");
        let at = confined(&root, Path::new(".build/soak")).expect("inside the root");
        assert!(at.starts_with(&root), "{}", at.display());
    }

    #[test]
    fn dot_dot_cannot_walk_out_of_the_root() {
        // The check is on the resolved path, so a relative escape is caught rather than
        // being taken literally.
        let root = std::env::current_dir().expect("a working directory");
        assert!(confined(&root, Path::new("../../../var/tmp/escaped")).is_err());
    }

    #[test]
    fn the_root_itself_is_inside_itself() {
        let root = std::env::current_dir().expect("a working directory");
        assert!(confined(&root, &root).is_ok());
    }

    #[test]
    fn tables_are_named_and_the_names_extend_past_the_list() {
        assert_eq!(table_name(0), "events");
        assert_eq!(table_name(9), "batches");
        // Five hundred tables still have readable, orderable names.
        assert_eq!(table_name(10), "events_001");
        assert_eq!(table_name(499), "batches_049");
    }
}

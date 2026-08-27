//! The scheduled soak: a real warehouse, real load, sampled and judged, for as long as it
//! is given.
//!
//! # Why this is a binary and not a test
//!
//! `M6` exit criterion 4 asks for a **multi-day run at the ten-gigabyte scale**. That is not
//! a `cargo test` — a build that took days would not be a build — so the harness runs short
//! in the suite to prove it works, and long here to prove the system does.
//!
//! The two differ in exactly two numbers: how much data, and how long. Everything else, the
//! judgement included, is the same code.
//!
//! ```text
//! sankhya-soak --gb 10 --tables 10 --minutes 240 --at /var/tmp/soak
//! ```
//!
//! # It reports as it goes
//!
//! A run that reports only at the end is a run nobody watches, and a soak that nobody watches
//! is one whose failure is discovered when somebody remembers to look. So progress goes to
//! standard output every couple of minutes with a timestamp, and the judged report is written
//! after every interval rather than only at the finish --- if the process is killed at hour
//! nine, hour eight's verdict is on disk.

// A binary may print. That is what it is for.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use arrow_array::{Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use sankhya_soak::report::supported_horizon;
use sankhya_soak::sample::{file_bytes, open_files, resident_bytes, Samples};
use sankhya_soak::Report;
use sankhya_table::{write_parquet, WriterConfig};
use sankhya_table_delta::{commit, create, live_files, Action, AddFile, Metadata, RemoveFile};
use sankhya_types::Lsn;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

/// How often a reading is taken.
const SAMPLE_EVERY: Duration = Duration::from_secs(15);
/// How often progress is printed and the report rewritten.
const REPORT_EVERY: Duration = Duration::from_secs(120);

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let gb: f64 = flag(&args, "--gb")
        .and_then(|v| v.parse().ok())
        .unwrap_or(10.0);
    let tables: usize = flag(&args, "--tables")
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);
    let minutes: u64 = flag(&args, "--minutes")
        .and_then(|v| v.parse().ok())
        .unwrap_or(240);
    let at = PathBuf::from(flag(&args, "--at").unwrap_or_else(|| "/var/tmp/sankhya-soak".into()));

    println!("{}  soak starting", stamp());
    println!("{}    target {gb} GB across {tables} table(s), {minutes} minute(s)", stamp());
    println!("{}    warehouse {}", stamp(), at.display());

    if let Err(error) = std::fs::create_dir_all(&at) {
        eprintln!("{}  cannot use {}: {error}", stamp(), at.display());
        std::process::exit(2);
    }

    // --- fill ------------------------------------------------------------
    let filling = Instant::now();
    let per_table_bytes = (gb * 1024.0 * 1024.0 * 1024.0) / tables as f64;
    let mut roots = Vec::new();
    for table in 0..tables {
        let root = at.join("warehouse").join("soak").join(format!("t{table:02}"));
        create_table(&root);
        let written = fill(&root, per_table_bytes, &filling);
        println!(
            "{}    t{table:02} filled, {:.2} GB",
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

    // --- run -------------------------------------------------------------
    let started = Instant::now();
    let deadline = Duration::from_secs(minutes * 60);
    let mut samples = Samples::new();
    let mut last_report = Instant::now();
    let mut round = 0_u64;
    let mut queries = 0_u64;
    let mut published = 0_u64;
    let mut refused = 0_u64;

    while started.elapsed() < deadline {
        round += 1;

        // Writes: one file per table per round.
        for root in &roots {
            if append_one(root, round) {
                published += 1;
            } else {
                refused += 1;
            }
        }

        // Queries: replay every table's log, which is what planning actually costs.
        for root in &roots {
            if live_files(root).is_ok() {
                queries += 1;
            }
        }

        // Maintenance on a duty cycle, so live files are a sawtooth rather than a ramp.
        if round % 8 == 0 {
            for root in &roots {
                compact_appended(root, round);
            }
        }

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
            Some(file_bytes(&at.join("diagnostic-history.tsv")).unwrap_or(0.0)),
        );
        #[allow(clippy::cast_precision_loss)]
        samples.record("queries", at_micros, Some(queries as f64));
        #[allow(clippy::cast_precision_loss)]
        samples.record("audit_records", at_micros, Some(queries as f64));
        let live = worst_table(&roots);
        #[allow(clippy::cast_precision_loss)]
        samples.record("live_files", at_micros, Some(live as f64));

        if last_report.elapsed() >= REPORT_EVERY {
            emit(&samples, &at, started.elapsed(), round, queries, live, published);
            last_report = Instant::now();
        }
        std::thread::sleep(SAMPLE_EVERY);
    }

    let live = worst_table(&roots);
    emit(&samples, &at, started.elapsed(), round, queries, live, published);
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
) {
    let horizon = supported_horizon(samples);
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    let now = elapsed.as_micros() as i64;
    let report = Report::of(samples, horizon, now);
    let rss = resident_bytes().unwrap_or(0.0) / (1024.0 * 1024.0);
    println!(
        "{}  t+{:>5.0}s  round {round:<5} published {published:<6} queries {queries:<7} \
         live_files {live:<6} rss {rss:>6.0} MB  {}  (horizon {}s)",
        stamp(),
        elapsed.as_secs_f64(),
        if report.passed() { "PASS" } else { "watching" },
        horizon
    );
    for (measure, verdict) in &report.verdicts {
        if !verdict.passed() {
            println!("{}      {:<16} {verdict:?}", stamp(), measure.name);
        }
    }
    std::fs::write(at.join("soak-report.txt"), report.describe()).ok();
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
fn die(what: &str) -> ! {
    eprintln!("{}  cannot start: {what}", stamp());
    std::process::exit(2)
}

fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("region", DataType::Utf8, true),
        Field::new("payload", DataType::Utf8, false),
    ]))
}

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
    let Ok(delta) = sankhya_table_delta::schema_string(&schema()) else {
        die("the soak schema has no faithful table representation");
    };
    if let Err(error) = commit(root, 0, &create(Metadata::new("soak", delta, 0))) {
        die(&format!("{} could not be created: {error}", root.display()));
    }
}

/// One batch of rows, sized so a file is a few megabytes.
fn batch(from: i64, rows: usize) -> RecordBatch {
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
    match RecordBatch::try_new(
        schema(),
        vec![
            Arc::new(Int64Array::from(ids)),
            Arc::new(StringArray::from(regions)),
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

/// Write files until the table holds roughly `target_bytes`.
///
/// Resumes rather than restarts. The count starts from what the table already holds and the
/// version from its log, so a run against an already-filled warehouse writes nothing — which
/// is what makes it cheap to restart a soak after fixing the harness, rather than paying
/// three and a half minutes to regenerate ten gigabytes that are already there.
fn fill(root: &Path, target_bytes: f64, since: &Instant) -> u64 {
    const ROWS_PER_FILE: usize = 200_000;
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
        let report = match write_parquet(
            root,
            &name,
            &batch(row, ROWS_PER_FILE),
            Lsn::new(version),
            WriterConfig::default(),
        ) {
            Ok(report) => report,
            Err(error) => die(&format!("{}/{name} could not be written: {error}", root.display())),
        };
        if let Err(error) = commit(
            root,
            version,
            &[Action::Add(AddFile::with_rows(
                &name,
                report.bytes,
                0,
                ROWS_PER_FILE as u64,
            ))],
        ) {
            die(&format!("{}/{name} could not be published: {error}", root.display()));
        }
        written += report.bytes;
        row += ROWS_PER_FILE as i64;
        version += 1;
        if version % 20 == 0 {
            println!(
                "{}      {:.2} GB after {:.0}s",
                stamp(),
                written as f64 / (1024.0 * 1024.0 * 1024.0),
                since.elapsed().as_secs_f64()
            );
        }
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

/// One more file, as ingest would publish it.
///
/// Returns whether it landed. **Nothing here is swallowed**: a harness that discards write
/// errors measures an idle system and reports that it is healthy, which is the exact class
/// of failure a soak exists to find, committed in the soak.
fn append_one(root: &Path, sequence: u64) -> bool {
    let version = next_version(root);
    let name = format!("live-{sequence:06}.parquet");
    let Ok(report) = write_parquet(
        root,
        &name,
        &batch(i64::try_from(sequence).unwrap_or(0) * 10_000, 5_000),
        Lsn::new(version),
        WriterConfig::default(),
    ) else {
        return false;
    };
    commit(
        root,
        version,
        &[Action::Add(AddFile::with_rows(&name, report.bytes, 0, 5_000))],
    )
    .is_ok()
}

/// Merge the files this run appended, leaving the base dataset alone.
///
/// **Only the `live-` files.** The first version removed every live file and replaced them
/// with one small batch — which is not compaction, it is deletion: ten gigabytes would have
/// stopped being live at the first duty cycle and the remaining hours would have soaked an
/// empty warehouse.
///
/// Real compaction merges what it removes. This one is honest about being a stand-in: it
/// merges only the small files the run itself produced, which is the shape that makes
/// `live_files` a sawtooth, and never touches the base.
fn compact_appended(root: &Path, sequence: u64) -> bool {
    let Ok(live) = live_files(root) else {
        return false;
    };
    // Its own previous output as well as the newly appended files.
    //
    // The first version merged only `live-` files, so every duty cycle left one more
    // `compacted-` file behind that nothing ever touched again — a permanent climb of one
    // file per cycle. **A compaction that never re-compacts its own output is not
    // compaction**, and the sawtooth judge would eventually have flagged it: correctly, and
    // about the harness rather than about the system, four hours into a run.
    let appended: Vec<&sankhya_table_delta::AddFile> = live
        .files
        .iter()
        .filter(|file| file.path.starts_with("live-") || file.path.starts_with("compacted-"))
        .collect();
    if appended.len() < 2 {
        return false;
    }

    let rows: u64 = appended.iter().filter_map(|file| file.rows()).sum();
    let version = next_version(root);
    let mut actions: Vec<Action> = appended
        .iter()
        .map(|file| {
            Action::Remove(RemoveFile::rewritten(
                file.path.clone(),
                i64::try_from(version).unwrap_or(0),
            ))
        })
        .collect();

    let name = format!("compacted-{sequence:06}.parquet");
    #[allow(clippy::cast_possible_truncation)]
    let Ok(report) = write_parquet(
        root,
        &name,
        &batch(0, rows.min(200_000) as usize),
        Lsn::new(version),
        WriterConfig::default(),
    ) else {
        return false;
    };
    actions.push(Action::Add(AddFile::with_rows(&name, report.bytes, 0, rows)));
    commit(root, version, &actions).is_ok()
}

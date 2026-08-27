//! The quickstart, executed and timed.
//!
//! # Why this is a test and not a document
//!
//! `M6`'s first exit criterion, and the implementation plan is explicit about the reason:
//! *"it must be a test so it cannot rot"*. A getting-started guide is prose about a sequence
//! of commands, and prose does not fail when the seventh command stops working. This file is
//! that sequence, run on every build.
//!
//! **The timing is the smaller half of its value.** The larger half is that the documented
//! path is exercised end to end: generate a warehouse, start the real binary as a
//! subprocess, connect over the real wire protocol, run a query, take a backup, prove it.
//! Any of those breaking breaks this test.
//!
//! # What the five minutes does and does not include
//!
//! It measures from **a built binary** to a successful query. That is a deliberate choice
//! and it is worth stating rather than leaving implied, because the alternative reading is
//! the one a first-time user has:
//!
//! | Excluded | Why |
//! |---|---|
//! | `git clone` | Network, not this system's |
//! | `cargo build` | Several minutes on a cold machine, dominated by dependencies, and **removed entirely by a released artifact**. `QUICKSTART.md` says so plainly |
//! | The vendored PostgreSQL build | Roughly two minutes, and needed only for the transactional half |
//!
//! **So the five-minute promise as written cannot be met from source**, and this test does
//! not pretend otherwise: the Rust compile alone exceeds it. It is met from a packaged
//! artifact, which makes exit criterion 1 depend on `§10.4`. Recording that here is more
//! useful than a green test measuring the wrong thing.
//!
//! # What the timing can and cannot catch
//!
//! Measured, the whole journey takes about **forty milliseconds**. Asserting that against
//! five minutes is asserting against a number four orders of magnitude larger, and a budget
//! that can never be reached is documentation with a `#[test]` attribute on it.
//!
//! But tightening the budget does not rescue it either, and it is worth being plain about
//! why: this warehouse holds a thousand rows in four files. **No plausible scaling
//! regression is visible at that size.** Somebody making the read path open every Parquet
//! footer would still finish in milliseconds here. The budgets below catch a phase
//! *breaking* or slowing by two orders of magnitude — a deadlock, a retry loop, a sleep
//! somebody left in — and nothing subtler.
//!
//! Scaling belongs to `cargo xtask check-performance`, which generates a scale-factor-1
//! dataset and is deliberately outside `check-all` for exactly that reason. Splitting them
//! is the point: this test runs on every build and proves the path works; that one runs on a
//! quiet machine and proves the path is fast.
//!
//! So the budgets here are set for a contended runner with a cold cache, the five-minute
//! figure is checked as the promise of record, and the real deliverable is that seven
//! documented steps ran end to end.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use arrow_array::{Float64Array, Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use sankhya_table::{write_parquet, WriterConfig};
use sankhya_table_delta::{commit, create, Action, AddFile, Metadata};
use sankhya_types::Lsn;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The promise of record, from `ROADMAP.md`.
const FIVE_MINUTES: Duration = Duration::from_secs(300);

/// How long the whole journey may take from a built binary.
///
/// Ten seconds, against an observed forty milliseconds. The gap is not generosity about
/// performance — it is the range a shared runner with a cold page cache genuinely spans, and
/// a timing test that flakes is a timing test that gets deleted.
const JOURNEY_BUDGET: Duration = Duration::from_secs(10);

/// A phase, and what it cost.
struct Phase {
    what: &'static str,
    took: Duration,
    budget: Duration,
}

impl Phase {
    fn check(&self) {
        assert!(
            self.took <= self.budget,
            "{} took {:.2}s, and its budget is {:.0}s",
            self.what,
            self.took.as_secs_f64(),
            self.budget.as_secs_f64()
        );
    }
}

/// Time one step.
fn timed<T>(what: &'static str, budget: Duration, step: impl FnOnce() -> T) -> (T, Phase) {
    let start = Instant::now();
    let value = step();
    (
        value,
        Phase {
            what,
            took: start.elapsed(),
            budget,
        },
    )
}

/// Write the sample warehouse a first-time user is told to generate.
fn write_warehouse(root: &std::path::Path) {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("region", DataType::Utf8, true),
        Field::new("amount", DataType::Float64, false),
    ]));
    let table_root = root.join("sales").join("orders");
    std::fs::create_dir_all(&table_root).expect("creating the table directory");
    let delta = sankhya_table_delta::schema_string(&schema).expect("representable");
    commit(&table_root, 0, &create(Metadata::new("orders", delta, 0))).expect("creating");

    let mut adds = Vec::new();
    for file in 0..4u64 {
        let ids: Vec<i64> = (0..250)
            .map(|i| i64::try_from(file * 250 + i).unwrap_or(0))
            .collect();
        let regions: Vec<Option<&str>> = ids
            .iter()
            .map(|i| match i % 3 {
                0 => Some("north"),
                1 => Some("south"),
                _ => None,
            })
            .collect();
        #[allow(clippy::cast_precision_loss)]
        let amounts: Vec<f64> = ids.iter().map(|i| *i as f64 * 1.5).collect();
        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![
                Arc::new(Int64Array::from(ids)),
                Arc::new(StringArray::from(regions)),
                Arc::new(Float64Array::from(amounts)),
            ],
        )
        .expect("a valid batch");
        let name = format!("part-{file:04}.parquet");
        let report = write_parquet(
            &table_root,
            &name,
            &batch,
            Lsn::new(file + 1),
            WriterConfig::default(),
        )
        .expect("writing");
        adds.push(Action::Add(AddFile::with_rows(&name, report.bytes, 0, 250)));
    }
    commit(&table_root, 1, &adds).expect("publishing");
}

/// The server, and the port it actually bound.
struct Running {
    child: Child,
    port: u16,
}

impl Drop for Running {
    fn drop(&mut self) {
        // Killed rather than signalled: a test that leaves a server behind poisons every
        // later run on the same machine, and this one has already proven the shutdown path
        // elsewhere.
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

/// How long to wait for the server to announce its port before giving up.
///
/// Bounded, and the bound is not decoration. An earlier version read the banner on this
/// thread until the line appeared — so a server that started and printed *nothing* left the
/// test blocked forever, which took the whole build with it and produced no message. A test
/// that hangs is strictly worse than one that fails: a failure names what broke.
const BANNER_TIMEOUT: Duration = Duration::from_secs(30);

/// Start the binary and wait until it says which port it took.
///
/// Reading the banner rather than sleeping. A sleep long enough to be reliable dominates the
/// measurement; one short enough not to is a flaky test.
fn start(warehouse: &std::path::Path, data: &std::path::Path) -> Running {
    let mut child = Command::new(env!("CARGO_BIN_EXE_sankhya-server"))
        .env("SANKHYA_NO_PASSWORD", "1")
        .env("SANKHYA_LISTEN", "127.0.0.1:0")
        .env("SANKHYA_METRICS_LISTEN", "127.0.0.1:0")
        .env("SANKHYA_WAREHOUSE", warehouse)
        .env("SANKHYA_DATA_DIR", data)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the server binary starts");

    let stdout = child.stdout.take().expect("piped");
    // Read on another thread and wait with a deadline, because `read_line` has none. The
    // thread is left to finish on its own: it ends when the child does, and the child is
    // killed by `Running`'s `Drop` on every path out of this test.
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        while reader.read_line(&mut line).unwrap_or(0) > 0 {
            if let Some(address) = line.trim().strip_prefix("listening on ") {
                let port = address
                    .rsplit_once(':')
                    .and_then(|(_, port)| port.parse::<u16>().ok());
                sender.send(port).ok();
                return;
            }
            line.clear();
        }
        // Ended without announcing. Reported rather than left to time out, so the failure
        // says "it never said" instead of "something took too long".
        sender.send(None).ok();
    });

    let announced = receiver.recv_timeout(BANNER_TIMEOUT);
    let port = match announced {
        Ok(Some(port)) => port,
        Ok(None) => {
            let mut running = Running { child, port: 0 };
            running.child.kill().ok();
            panic!("the server exited without announcing a port");
        }
        Err(_) => {
            let mut running = Running { child, port: 0 };
            running.child.kill().ok();
            panic!(
                "the server did not announce a port within {}s",
                BANNER_TIMEOUT.as_secs()
            );
        }
    };
    Running { child, port }
}

/// Run one simple query over the wire and return the rows it produced.
///
/// A hand-written client rather than `psql`, because `psql` needs the vendored PostgreSQL
/// build and this test must run on a machine that has not done it. It speaks the real
/// protocol, which is the half that can regress here.
fn query(port: u16, sql: &str) -> usize {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connecting");
    stream.set_nodelay(true).ok();

    let mut startup = Vec::new();
    let mut body = 196_608i32.to_be_bytes().to_vec();
    body.extend_from_slice(b"user\0quickstart\0\0");
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    startup.extend_from_slice(&((body.len() + 4) as i32).to_be_bytes());
    startup.extend_from_slice(&body);
    stream.write_all(&startup).expect("startup");

    let mut buffer = Vec::new();
    read_until_ready(&mut stream, &mut buffer);

    let mut message = vec![b'Q'];
    let payload = format!("{sql}\0");
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    message.extend_from_slice(&((payload.len() + 4) as i32).to_be_bytes());
    message.extend_from_slice(payload.as_bytes());
    stream.write_all(&message).expect("query");

    buffer.clear();
    read_until_ready(&mut stream, &mut buffer);
    // `D` is a DataRow. Counting tags rather than parsing the whole stream: the assertion is
    // that rows came back, and a parser here would be a second protocol implementation to
    // keep correct.
    count_tags(&buffer, b'D')
}

/// Read until the server says it is ready for the next statement.
fn read_until_ready(stream: &mut TcpStream, buffer: &mut Vec<u8>) {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("a read timeout");
    let mut chunk = [0u8; 4096];
    loop {
        let read = stream.read(&mut chunk).expect("the server answers");
        assert!(read > 0, "the server closed the connection");
        buffer.extend_from_slice(&chunk[..read]);
        if count_tags(buffer, b'Z') > 0 {
            return;
        }
    }
}

/// How many messages of this tag the buffer holds.
///
/// Walks the framing rather than searching for the byte, because a row's *data* can contain
/// any byte and a search would count values as messages.
fn count_tags(buffer: &[u8], tag: u8) -> usize {
    let mut at = 0usize;
    let mut found = 0usize;
    while at + 5 <= buffer.len() {
        let length = i32::from_be_bytes([
            buffer[at + 1],
            buffer[at + 2],
            buffer[at + 3],
            buffer[at + 4],
        ]);
        let Ok(length) = usize::try_from(length) else {
            return found;
        };
        if length < 4 || at + 1 + length > buffer.len() {
            return found;
        }
        if buffer[at] == tag {
            found += 1;
        }
        at += 1 + length;
    }
    found
}

/// Run a subcommand of the binary and return its exit status.
fn subcommand(what: &str, warehouse: &std::path::Path, data: &std::path::Path) -> i32 {
    Command::new(env!("CARGO_BIN_EXE_sankhya-server"))
        .arg(what)
        .env("SANKHYA_WAREHOUSE", warehouse)
        .env("SANKHYA_DATA_DIR", data)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("the subcommand runs")
        .code()
        .unwrap_or(-1)
}

#[test]
fn a_first_time_user_reaches_a_successful_query() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let warehouse = dir.path().join("warehouse");
    let data = dir.path().join(".sankhya");
    let journey = Instant::now();
    let mut phases = Vec::new();

    let ((), phase) = timed("generate a warehouse", Duration::from_secs(5), || {
        write_warehouse(&warehouse);
    });
    phases.push(phase);

    let (server, phase) = timed("start the server", Duration::from_secs(5), || {
        start(&warehouse, &data)
    });
    phases.push(phase);

    let (rows, phase) = timed("connect and query", Duration::from_secs(5), || {
        query(server.port, "SELECT id, region, amount FROM orders LIMIT 5")
    });
    phases.push(phase);
    assert_eq!(rows, 5, "the query returned rows over the real wire protocol");

    let (aggregate, phase) = timed("run an aggregate", Duration::from_secs(5), || {
        query(server.port, "SELECT region, count(*) FROM orders GROUP BY region")
    });
    phases.push(phase);
    assert!(aggregate >= 2, "north, south and the nulls");

    let (status, phase) = timed("run the diagnostic", Duration::from_secs(5), || {
        subcommand("doctor", &warehouse, &data)
    });
    phases.push(phase);
    // 0 clean, 1 findings — either is a working diagnostic. 2 means it could not look.
    assert_ne!(status, 2, "the diagnostic could not run");

    let (status, phase) = timed("take a backup", Duration::from_secs(8), || {
        subcommand("backup", &warehouse, &data)
    });
    phases.push(phase);
    assert_eq!(status, 0, "the backup was recorded");

    let (status, phase) = timed("prove the backup", Duration::from_secs(8), || {
        subcommand("drill", &warehouse, &data)
    });
    phases.push(phase);
    assert_eq!(status, 0, "the backup is proven restorable");

    let total = journey.elapsed();

    // Printed whether or not it passes. A timing test that reports only a failure gives
    // nobody the trend, and the trend is how a slow drift gets noticed before it is a
    // failure.
    println!("\n  the five-minute experience, from a built binary");
    for phase in &phases {
        println!(
            "  {:>22}  {:>7.2}s   (budget {:.0}s)",
            phase.what,
            phase.took.as_secs_f64(),
            phase.budget.as_secs_f64()
        );
    }
    println!("  {:>22}  {:>7.2}s\n", "total", total.as_secs_f64());

    for phase in &phases {
        phase.check();
    }
    assert!(
        total <= JOURNEY_BUDGET,
        "the whole journey took {:.2}s against a budget of {:.0}s",
        total.as_secs_f64(),
        JOURNEY_BUDGET.as_secs_f64()
    );
    // The promise of record. It should never be the assertion that fires — if it does, the
    // per-phase budgets above have been raised past the point of meaning anything.
    assert!(total <= FIVE_MINUTES);
}

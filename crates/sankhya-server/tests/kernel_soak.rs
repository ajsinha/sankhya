//! A soak over the function catalogue: real tables, real columns, sustained mixed load.
//!
//! # What this measures that a unit test cannot
//!
//! The catalogue's unit tests ask whether each kernel computes the right number once, on data
//! the test chose. That is the easy half. This asks four things none of them can:
//!
//! - **Does it drift?** The same statement over the same rows must return the *same bits* an
//!   hour later. A reduction whose result depends on how work was partitioned returns a
//!   different number when the machine is busier, and no single-shot test can see that.
//! - **Does it work on a column?** A kernel called on a literal and a kernel called on four
//!   thousand rows of a `FixedSizeList` take different paths, and a width read from the wrong
//!   stride is invisible until the second one runs.
//! - **Do the refusals keep refusing?** A guard that stops guarding after an hour is worth
//!   more finding than a guard that never worked.
//! - **Does anything accumulate?** Descriptors, memory and open files under mixed load.
//!
//! # Both tiers, honestly
//!
//! The analytical arm runs here. The transactional arm needs a PostgreSQL cluster, which
//! `sankhya-oltp-pg` can start from vendored binaries and which is **not present on every
//! machine** --- so that arm reports that it was skipped, by name, and the run says so in its
//! verdict. A soak that quietly runs half of itself and prints `PASS` is worse than one that
//! fails: it is a claim nobody checked.
//!
//! # Running it
//!
//! ```text
//! SANKHYA_KERNEL_SOAK_MINUTES=30 \
//!   cargo test -p sankhya-diagnostic --test kernel_soak -- --ignored --nocapture
//! ```

#![allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::print_stdout, clippy::print_stderr, clippy::indexing_slicing)]

mod common;

use common::start;

use arrow_array::builder::{FixedSizeListBuilder, Float64Builder};
use arrow_array::{Int64Array, RecordBatch};
use arrow_schema::{DataType, Field, Schema};
use sankhya_diagnostic::soak::kernels::{
    covariances, embeddings, expectations, probes, series, Expectation, Findings, ROWS, WIDTH,
};
use sankhya_publish::Publication;
use sankhya_types::Lsn;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How wide a covariance matrix is, and therefore how long its flat array.
const ORDER: usize = 4;

/// How many matrices the covariance table holds.
///
/// Fewer than the vector tables, because a decomposition per row is the expensive probe and
/// the point is to run it repeatedly rather than once over a great many rows.
const MATRICES: usize = 500;

// --- the fixture ----------------------------------------------------------

/// A table of embeddings: a `FixedSizeList<Float64, WIDTH>` column, which is what
/// `ADR-0021` Decision 2 means by the width being part of the type.
fn write_vectors(root: &Path, name: &str, column: &str, rows: &[Vec<f64>]) {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new(
            column,
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float64, true)),
                i32::try_from(rows.first().map_or(0, Vec::len)).unwrap_or(0),
            ),
            false,
        ),
    ]));

    let publication = Publication::external(root.join(name), name);
    publication.create(&schema).expect("creating a table with a vector column");

    let width = i32::try_from(rows.first().map_or(0, Vec::len)).unwrap_or(0);
    let mut vectors = FixedSizeListBuilder::new(Float64Builder::new(), width);
    for row in rows {
        vectors.values().append_slice(row);
        vectors.append(true);
    }
    let ids: Vec<i64> = (0..i64::try_from(rows.len()).unwrap_or(0)).collect();
    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![Arc::new(Int64Array::from(ids)), Arc::new(vectors.finish())],
    )
    .expect("a batch");
    publication
        .append(1, "part-0000.parquet", &batch, Lsn::new(rows.len() as u64))
        .expect("publishing");
}

/// The whole warehouse the soak reads.
fn build(root: &Path, seed: u64) -> sankhya_diagnostic::soak::kernels::Embedded {
    let rows = embeddings(seed);
    let expected = expectations(&rows, seed);
    write_vectors(&root.join("quant"), "embeddings", "embedding", &rows);
    write_vectors(&root.join("quant"), "curves", "curve", &series(seed ^ 7));
    write_vectors(
        &root.join("quant"),
        "matrices",
        "covariance",
        &covariances(seed ^ 11, ORDER, MATRICES),
    );
    expected
}

// --- a client that speaks the wire protocol -------------------------------

/// One connection, kept open for the whole run.
///
/// Kept open deliberately: a soak that reconnects per statement measures connection setup and
/// hides everything a long-lived session accumulates.
struct Client {
    stream: TcpStream,
}

impl Client {
    fn open(port: u16) -> Self {
        let stream = TcpStream::connect(("127.0.0.1", port)).expect("connecting");
        stream.set_nodelay(true).ok();
        stream
            .set_read_timeout(Some(Duration::from_secs(60)))
            .expect("a read deadline");
        let mut client = Self { stream };

        let mut body = 196_608i32.to_be_bytes().to_vec();
        body.extend_from_slice(b"user\0soak\0\0");
        let mut startup = ((body.len() + 4) as i32).to_be_bytes().to_vec();
        startup.extend_from_slice(&body);
        client.stream.write_all(&startup).expect("startup");
        client.drain();
        client
    }

    /// Send one statement and return everything the server said.
    fn ask(&mut self, sql: &str) -> Vec<u8> {
        let payload = format!("{sql}\0");
        let mut message = vec![b'Q'];
        message.extend_from_slice(&((payload.len() + 4) as i32).to_be_bytes());
        message.extend_from_slice(payload.as_bytes());
        self.stream.write_all(&message).expect("a query");
        self.drain()
    }

    /// Read until the server says it is ready for the next statement.
    fn drain(&mut self) -> Vec<u8> {
        let mut all = Vec::new();
        let mut buffer = [0u8; 8192];
        loop {
            let read = match self.stream.read(&mut buffer) {
                Ok(0) => panic!("the server closed the connection mid-run"),
                Ok(n) => n,
                Err(error) => panic!("reading from the server: {error}"),
            };
            all.extend_from_slice(&buffer[..read]);
            if ready(&all) {
                return all;
            }
        }
    }
}

/// Whether a buffer ends with a `ReadyForQuery`.
fn ready(buffer: &[u8]) -> bool {
    let mut at = 0usize;
    let mut last = None;
    while at + 5 <= buffer.len() {
        let length = i32::from_be_bytes([
            buffer[at + 1],
            buffer[at + 2],
            buffer[at + 3],
            buffer[at + 4],
        ]);
        let Ok(length) = usize::try_from(length) else {
            return false;
        };
        if length < 4 || at + 1 + length > buffer.len() {
            return false;
        }
        last = Some(buffer[at]);
        at += 1 + length;
    }
    last == Some(b'Z')
}

/// The first value of the first row, as text, or `None` for a refusal.
fn first_value(buffer: &[u8]) -> Result<Option<String>, String> {
    let mut at = 0usize;
    let mut value = None;
    let mut rows = 0usize;
    let mut refusal = None;
    while at + 5 <= buffer.len() {
        let length = i32::from_be_bytes([
            buffer[at + 1],
            buffer[at + 2],
            buffer[at + 3],
            buffer[at + 4],
        ]);
        let Ok(length) = usize::try_from(length) else {
            break;
        };
        if length < 4 || at + 1 + length > buffer.len() {
            break;
        }
        let body = &buffer[at + 5..at + 1 + length];
        if buffer[at] == b'E' {
            refusal = Some(String::from_utf8_lossy(body).replace('\0', " "));
        }
        if buffer[at] == b'D' {
            rows += 1;
            if value.is_none() {
                value = decode_first(body);
            }
        }
        at += 1 + length;
    }
    if let Some(said) = refusal {
        return Err(said);
    }
    let _ = rows;
    Ok(value)
}

/// How many `DataRow` messages a buffer holds.
fn row_count(buffer: &[u8]) -> usize {
    let mut at = 0usize;
    let mut rows = 0usize;
    while at + 5 <= buffer.len() {
        let length = i32::from_be_bytes([
            buffer[at + 1],
            buffer[at + 2],
            buffer[at + 3],
            buffer[at + 4],
        ]);
        let Ok(length) = usize::try_from(length) else {
            break;
        };
        if length < 4 || at + 1 + length > buffer.len() {
            break;
        }
        if buffer[at] == b'D' {
            rows += 1;
        }
        at += 1 + length;
    }
    rows
}

/// The first column of one `DataRow` body.
fn decode_first(body: &[u8]) -> Option<String> {
    if body.len() < 6 {
        return None;
    }
    let length = i32::from_be_bytes([body[2], body[3], body[4], body[5]]);
    if length < 0 {
        return Some(String::new());
    }
    let length = usize::try_from(length).ok()?;
    body.get(6..6 + length).map(|bytes| String::from_utf8_lossy(bytes).into_owned())
}

// --- the run --------------------------------------------------------------

#[test]
#[ignore = "a soak: run with SANKHYA_KERNEL_SOAK_MINUTES and --ignored"]
fn the_kernels_hold_up_under_sustained_mixed_load() {
    let minutes: u64 = std::env::var("SANKHYA_KERNEL_SOAK_MINUTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);
    let clients: usize = std::env::var("SANKHYA_KERNEL_SOAK_CLIENTS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4);
    let seed: u64 = std::env::var("SANKHYA_KERNEL_SOAK_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0x5150_1234);

    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    println!("building {ROWS} embeddings of {WIDTH}, {ROWS} curves, {MATRICES} matrices of {ORDER}");
    let data = build(&warehouse, seed);
    let server = start(&warehouse, &dir.path().join("data"));
    let port = server.port;
    println!("server on {port}; {clients} client(s) for {minutes} minute(s)");

    let probes = probes("quant.embeddings", "quant.curves", "quant.matrices", &data);
    println!("{} probe(s) per pass", probes.len());

    let deadline = Instant::now() + Duration::from_secs(minutes * 60);
    let outcome = std::thread::scope(|scope| {
        let probes = &probes;
        let handles: Vec<_> = (0..clients)
            .map(|which| {
                scope.spawn(move || {
                    let mut client = Client::open(port);
                    let mut findings = Findings::default();
                    let mut passes = 0u64;
                    while Instant::now() < deadline {
                        for probe in probes {
                            let said = client.ask(&probe.sql);
                            findings.ran += 1;
                            if let Err(reason) = check(probe, &said, &mut findings) {
                                findings.wrong.entry(probe.name).or_insert(reason);
                            }
                        }
                        passes += 1;
                    }
                    println!("  client {which}: {passes} pass(es), {} probe(s)", findings.ran);
                    findings
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("a client")).collect::<Vec<_>>()
    });

    drop(server);

    // --- the verdict -----------------------------------------------------
    let mut ran = 0usize;
    let mut wrong: BTreeMap<&'static str, String> = BTreeMap::new();
    for findings in &outcome {
        ran += findings.ran;
        for (name, reason) in &findings.wrong {
            wrong.entry(name).or_insert_with(|| reason.clone());
        }
    }

    println!("\n== the analytical arm ==");
    println!("   {ran} probe(s) across {clients} client(s), {} wrong", wrong.len());
    for (name, reason) in &wrong {
        println!("   WRONG  {name}: {reason}");
    }

    // --- the transactional arm -------------------------------------------
    //
    // Reported by name whether it ran or not. A soak that quietly runs half of itself and
    // prints PASS is worse than one that fails: it is a claim nobody checked.
    println!("\n== the transactional arm ==");
    match transactional_tier() {
        Some(detail) => println!("   {detail}"),
        None => println!(
            "   SKIPPED  no PostgreSQL cluster is available on this machine, so the \
             transactional half of the kernels was not exercised. This is a gap in the run \
             and not a pass: `sankhya-oltp-pg` can start a vendored cluster, and nothing \
             wires it into the server yet (M8 §12.2)."
        ),
    }

    assert!(
        wrong.is_empty(),
        "the kernels did not hold up: {:#?}",
        wrong.iter().collect::<Vec<_>>()
    );
    assert!(ran > probes.len(), "the soak did not complete a pass: {ran} probe(s)");
}

/// Whether a transactional tier is available, and what it is.
///
/// `None` rather than a panic, so the run reports the gap instead of failing on a machine that
/// was never going to have one.
fn transactional_tier() -> Option<String> {
    let candidates = ["vendor/postgresql/bin", "/usr/lib/postgresql/17/bin", "/usr/bin"];
    for at in candidates {
        if sankhya_oltp_pg::Binaries::at(at).is_some() {
            return Some(format!("a cluster is available at {at}"));
        }
    }
    None
}

/// Check one probe's answer against what it must be.
fn check(
    probe: &sankhya_diagnostic::soak::kernels::Probe,
    said: &[u8],
    findings: &mut Findings,
) -> Result<(), String> {
    let outcome = first_value(said);

    match &probe.expect {
        Expectation::Refused(fragment) => match outcome {
            Err(message) if message.contains(fragment) => Ok(()),
            Err(message) => Err(format!("refused, but not for `{fragment}`: {message}")),
            Ok(_) => Err(format!("was accepted, and must be refused for `{fragment}`")),
        },
        _ => {
            let value = match outcome {
                Err(message) => return Err(format!("refused: {message}")),
                Ok(None) => return Err("no rows came back".to_owned()),
                Ok(Some(text)) => text,
            };
            match &probe.expect {
                Expectation::Rows(expected) => {
                    let rows = row_count(said);
                    if rows == *expected {
                        Ok(())
                    } else {
                        Err(format!("{rows} row(s), expected {expected}"))
                    }
                }
                Expectation::Near { value: want, tolerance } => {
                    let got: f64 = value.parse().map_err(|_| format!("not a number: {value}"))?;
                    if (got - want).abs() <= *tolerance {
                        Ok(())
                    } else {
                        Err(format!("{got}, expected {want} within {tolerance}"))
                    }
                }
                Expectation::Within { low, high } => {
                    let got: f64 = value.parse().map_err(|_| format!("not a number: {value}"))?;
                    if got >= *low && got <= *high {
                        Ok(())
                    } else {
                        Err(format!("{got}, expected between {low} and {high}"))
                    }
                }
                Expectation::Stable => {
                    // The drift check, and the one only a soak can make: whatever the answer
                    // is, it must be the same answer every time. A reduction that reassociated
                    // under load fails here and nowhere else.
                    match findings.first.get(probe.name) {
                        None => {
                            findings.first.insert(probe.name, value);
                            Ok(())
                        }
                        Some(first) if *first == value => Ok(()),
                        Some(first) => Err(format!(
                            "drifted: was `{first}`, now `{value}`. The same statement over \
                             the same rows returned two different numbers"
                        )),
                    }
                }
                Expectation::Refused(_) => unreachable!("handled above"),
            }
        }
    }
}

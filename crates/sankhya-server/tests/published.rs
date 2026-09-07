//! What the running binary publishes, scraped from the port it announces.
//!
//! # What an in-process test cannot check here
//!
//! `sankhya-metrics` has its own tests for the registry, and `observability.rs` drives the
//! scrape endpoint against a `Server` built in the test process. Neither can check that the
//! **binary** publishes what it has counted --- and this is the defect those tests missed:
//! `MaintenanceHandle` counted ticks, reclaimed bytes, declines and failures from the day it
//! was written, and nothing outside two unit tests and a soak run ever read them. A counter
//! that is maintained and a counter that is exported look identical from inside the type.
//!
//! One server, one temporary warehouse, killed at the end. Nothing here writes to a warehouse
//! another process is writing to.

// `indexing_slicing` because `common` builds its own fixtures and indexes them, and a module
// included with `mod` inherits the including file's crate attributes. Every other test that
// includes it allows the same lint, for the same reason: a test chooses all of its data.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// A child that is killed however the test leaves.
///
/// # Why this type exists
///
/// `std::process::Child` does **not** kill on drop, and this test's assertions are between
/// the spawn and the kill. A panic anywhere in that stretch --- a failed assertion, a scrape
/// that cannot connect, the deadline --- unwinds straight past `child.kill()` and leaves a
/// `sankhya-server` running for the rest of the machine's uptime, one per failed run,
/// ticking maintenance every second.
///
/// Worse than a stray process: `TempDir` **is** dropped on unwind, so the warehouse, the data
/// directory and the configuration are deleted out from under a process that still holds the
/// warehouse lock and is still writing. That is a live writer against a warehouse that no
/// longer exists --- the second-writer failure this repository has a build check for, arriving
/// through a test whose own subject is a lock.
struct Killed(Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Start the binary and return it alongside the metrics address it announced.
///
/// The port is read from the banner rather than fixed, because a fixed port makes two tests
/// running at once fail on `AddrInUse` --- for a reason that has nothing to do with either.
fn started(warehouse: &std::path::Path, data: &std::path::Path) -> (Killed, String) {
    started_with(warehouse, data, true)
}

/// The same, with maintenance configured off.
fn started_without_maintenance(
    warehouse: &std::path::Path,
    data: &std::path::Path,
) -> (Killed, String) {
    started_with(warehouse, data, false)
}

fn started_with(
    warehouse: &std::path::Path,
    data: &std::path::Path,
    maintains: bool,
) -> (Killed, String) {
    let config = warehouse.parent().expect("a parent").join("application.yaml");
    let mut yaml = String::new();
    yaml.push_str(&format!("warehouse:\n  path: {}\n", warehouse.display()));
    // Every door on an ephemeral port, and every one of them under `server:` --- which is the
    // key the loader reads. A top-level `listen:` is read by nothing, so a test that writes one
    // binds the compiled-in `127.0.0.1:5433`, and two of them at once fail with `AddrInUse` for
    // a reason that has nothing to do with what they test.
    yaml.push_str("server:\n");
    yaml.push_str("  listen: 127.0.0.1:0\n");
    yaml.push_str("  metrics_listen: 127.0.0.1:0\n");
    yaml.push_str("  flight_listen: 127.0.0.1:0\n");
    // The shortest cadence the configuration accepts. `200ms` is refused, and rightly: a
    // duration is written in the units an operator writes, and a setting that silently became
    // something else would be a deployment behaving as though it were configured.
    yaml.push_str(if maintains {
        "maintenance:\n  interval: 1s\n"
    } else {
        // `0` disables it, said in the configuration rather than by deleting the setting, so
        // a deployment that turns it off leaves a record of having decided to.
        "maintenance:\n  interval: 0\n"
    });
    std::fs::write(&config, yaml).expect("writing the configuration");

    let mut child = Command::new(env!("CARGO_BIN_EXE_sankhya-server"))
        .arg("start")
        .env("SANKHYA_CONFIG", &config)
        .env("SANKHYA_DATA_DIR", data)
        .env_remove("SANKHYA_WAREHOUSE")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the server binary starts");
    // Wrapped before anything can fail, so every path below --- including the two panics in
    // this function --- goes through `Drop` rather than through an explicit kill somebody has
    // to remember at each `return`.
    let mut child = Killed(child);

    // Read the banner line by line rather than to end-of-file: the server does not exit, so
    // `read_to_string` would block for ever.
    let stdout = child.0.stdout.take().expect("piped stdout");
    let mut lines = BufReader::new(stdout).lines();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut banner = String::new();
    let address = loop {
        if Instant::now() >= deadline {
            panic!("the server never announced a metrics port. It printed:\n{banner}");
        }
        let Some(Ok(line)) = lines.next() else {
            let mut why = String::new();
            if let Some(mut err) = child.0.stderr.take() {
                err.read_to_string(&mut why).ok();
            }
            panic!("the server stopped before announcing a metrics port:\n{banner}\n{why}");
        };
        banner.push_str(&line);
        banner.push('\n');
        if let Some(rest) = line.trim().strip_prefix("metrics on http://") {
            break rest.trim_end_matches("/metrics").to_string();
        }
    };
    (child, address)
}

/// One `GET /metrics`, over a plain socket, because that is what a collector does.
fn scrape(address: &str) -> String {
    let mut socket =
        std::net::TcpStream::connect(address).expect("the announced port accepts a connection");
    socket
        .write_all(b"GET /metrics HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .expect("writing the request");
    let mut body = String::new();
    socket.read_to_string(&mut body).expect("reading the response");
    body
}

/// Scrape until `wanted` holds, or give up and show what was last seen.
fn until(address: &str, what: &str, wanted: impl Fn(&str) -> bool) -> String {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let last = scrape(address);
        if wanted(&last) {
            return last;
        }
        assert!(Instant::now() < deadline, "{what} never happened. Last scrape:\n{last}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The value of an unlabelled series, if the scrape carries one.
fn value(body: &str, metric: &str) -> Option<f64> {
    body.lines()
        .find_map(|line| line.strip_prefix(metric)?.strip_prefix(' ')?.trim().parse().ok())
}

#[test]
fn the_running_server_publishes_what_maintenance_has_done() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let warehouse = dir.path().join("warehouse");
    std::fs::create_dir_all(&warehouse).expect("a warehouse");
    // Real tables, because the assertion that pins the publisher is a **count** of them.
    // An empty warehouse leaves every maintenance number at zero, and zero is what the
    // metrics registry renders for an unlabelled metric nothing has recorded --- so on an
    // empty warehouse this whole test passes with the publisher deleted.
    common::write_warehouse(&warehouse);
    let tables = sankhya_maintenance::tables_under(&warehouse).len();
    assert!(tables > 0, "the fixture must give maintenance something to look after");

    let data = dir.path().join("data");
    let (_child, address) = started(&warehouse, &data);

    // The tick counter rising proves a cycle happened. It does not prove this server
    // published anything: the metric is unlabelled, so it reads zero from startup either
    // way, and only the **rise** is evidence.
    let later = until(&address, "the tick counter rose", |body| {
        value(body, "sankhya_maintenance_ticks_total").unwrap_or(0.0) > 0.0
    });

    // And this is what pins the rest of the block. All five values are published together;
    // four of them are legitimately zero on a healthy idle warehouse and therefore
    // indistinguishable from the zero series, but the table count is not. A publisher that
    // sets only the tick counter --- or none of them --- fails here.
    assert_eq!(
        value(&later, "sankhya_maintenance_tables"),
        Some(tables as f64),
        "the warehouse holds {tables} table(s) and the scrape does not say so:\n{later}"
    );
    assert_eq!(
        value(&later, "sankhya_maintenance_failures_total"),
        Some(0.0),
        "a healthy warehouse must not fail a pass:\n{later}"
    );
}

#[test]
fn a_server_that_maintains_nothing_exports_no_maintenance_metrics() {
    // `maintenance.interval: 0` is a supported configuration --- a deployment whose warehouse
    // another process maintains --- and on one of those a flat `sankhya_maintenance_ticks_total 0`
    // reads exactly like a thread that died on its first cycle. `absent()` cannot tell them
    // apart while the series is there, so it is not there.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let warehouse = dir.path().join("warehouse");
    std::fs::create_dir_all(&warehouse).expect("a warehouse");
    let data = dir.path().join("data");
    let (_child, address) = started_without_maintenance(&warehouse, &data);

    let body = scrape(&address);
    for metric in [
        "sankhya_maintenance_ticks_total",
        "sankhya_maintenance_bytes_reclaimed_total",
        "sankhya_maintenance_declined_total",
        "sankhya_maintenance_failures_total",
        "sankhya_maintenance_tables",
    ] {
        assert!(
            !body.contains(metric),
            "{metric} was exported at zero by a server that maintains nothing, which is what a \
             dead maintainer looks like:\n{body}"
        );
    }
    // And the one in the same group that is still meaningful is still there: it is computed
    // from the servable set at the moment of the scrape, not by the maintenance thread.
    assert!(
        body.contains("sankhya_table_live_files_max"),
        "filtering by group rather than by producer would have removed the metric that pages"
    );
}

#[test]
fn every_maintenance_counter_carries_a_series_before_anything_happens() {
    // A separate test, because it is a separate claim and the one above used to make both
    // --- badly. Asserting that the four counters carry *a* series proves the metrics are
    // declared and scrape-visible; it says **nothing** about the publisher, because an
    // unlabelled metric is rendered at zero from startup whether or not anything records
    // into it. Keeping the two apart is what stops one of them quietly standing in for the
    // other.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let warehouse = dir.path().join("warehouse");
    std::fs::create_dir_all(&warehouse).expect("a warehouse");
    let data = dir.path().join("data");
    let (_child, address) = started(&warehouse, &data);

    let first = scrape(&address);
    for metric in [
        "sankhya_maintenance_ticks_total",
        "sankhya_maintenance_bytes_reclaimed_total",
        "sankhya_maintenance_declined_total",
        "sankhya_maintenance_failures_total",
        "sankhya_maintenance_tables",
    ] {
        assert!(
            value(&first, metric).is_some(),
            "{metric} carried no series at all, so `absent()` cannot mean the exporter is \
             broken:\n{first}"
        );
    }
}

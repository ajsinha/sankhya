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

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Start the binary and return it alongside the metrics address it announced.
///
/// The port is read from the banner rather than fixed, because a fixed port makes two tests
/// running at once fail on `AddrInUse` --- for a reason that has nothing to do with either.
fn started(warehouse: &std::path::Path, data: &std::path::Path) -> (Child, String) {
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
    yaml.push_str("maintenance:\n  interval: 1s\n");
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

    // Read the banner line by line rather than to end-of-file: the server does not exit, so
    // `read_to_string` would block for ever.
    let stdout = child.stdout.take().expect("piped stdout");
    let mut lines = BufReader::new(stdout).lines();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut banner = String::new();
    let address = loop {
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("the server never announced a metrics port. It printed:\n{banner}");
        }
        let Some(Ok(line)) = lines.next() else {
            let _ = child.kill();
            let mut why = String::new();
            if let Some(mut err) = child.stderr.take() {
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
    let data = dir.path().join("data");
    let (mut child, address) = started(&warehouse, &data);

    // The counters exist from the first scrape --- they are unlabelled, so a zero series is
    // knowable before anything has happened, and `absent()` therefore means the exporter is
    // broken rather than that maintenance is healthy.
    let first = scrape(&address);
    for metric in [
        "sankhya_maintenance_ticks_total",
        "sankhya_maintenance_bytes_reclaimed_total",
        "sankhya_maintenance_declined_total",
        "sankhya_maintenance_failures_total",
    ] {
        assert!(
            value(&first, metric).is_some(),
            "{metric} carried no series at all:\n{first}"
        );
    }

    // And then they move, which is the half a declaration cannot prove. A tick counter that
    // stays at zero for ever is exactly what a maintainer that never ran looks like, and it
    // is what this server exported before the publisher existed.
    let later = until(&address, "the tick counter rose", |body| {
        value(body, "sankhya_maintenance_ticks_total").unwrap_or(0.0) > 0.0
    });
    assert_eq!(
        value(&later, "sankhya_maintenance_failures_total"),
        Some(0.0),
        "an empty warehouse must not fail a pass:\n{later}"
    );

    let _ = child.kill();
    let _ = child.wait();
}


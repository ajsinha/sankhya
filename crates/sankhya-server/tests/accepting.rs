//! What a real server does when it runs out of file descriptors.
//!
//! `OPS-08`. The accept loop was `accepted?`: the error propagated out of the serve loop
//! and out of `main`, so the process exited. `EMFILE` is the reachable case --- there was
//! no connection cap and the shipped unit set no `LimitNOFILE=` --- and `ECONNABORTED`,
//! which any load balancer produces on every health check, would have done it too.
//!
//! # Why this is not a unit test
//!
//! `setrlimit` is process-wide. A test binary cannot lower its own descriptor limit without
//! lowering it for every other test in the same binary, so the limit is set in a shell that
//! then `exec`s the server --- which is also how an init system does it, and the reason the
//! unit now says `LimitNOFILE=65535`.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use common::{start_under_descriptor_limit, start_with, write_warehouse, Session};
use std::net::TcpStream;
use std::time::{Duration, Instant};

/// Few enough that connections exhaust it, and enough that the server starts on it.
///
/// The server opens the warehouse, its audit chain and its two listening sockets before it
/// serves anything, so a limit that only a running server could exceed is one it never
/// reaches. Sixty-four is comfortably above what starting costs and comfortably below what
/// a hundred callers do.
const DESCRIPTORS: u32 = 64;

/// More than the limit, so the shortage is certain rather than likely.
const CALLERS: usize = 120;

#[test]
fn running_out_of_descriptors_does_not_end_the_server() {
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let server = start_under_descriptor_limit(
        &warehouse,
        &dir.path().join("data"),
        &[],
        DESCRIPTORS,
    );

    // It answers before the shortage, so a failure afterwards is the shortage rather than a
    // server that was never working.
    let before = Session::open(server.port)
        .run("SELECT region FROM orders")
        .expect("the server answers before the shortage");
    assert!(before > 0, "the fixture must have rows for this to mean anything");

    // Held open all at once. Closing each before opening the next would never exhaust
    // anything --- the descriptor is returned as fast as it is taken.
    let address = format!("127.0.0.1:{}", server.port);
    let mut held: Vec<TcpStream> = Vec::with_capacity(CALLERS);
    for _ in 0..CALLERS {
        // A refused connection is the *expected* outcome once the server stops accepting,
        // so a failure here is not a test failure. What matters is what the server does.
        if let Ok(stream) = TcpStream::connect(&address) {
            held.push(stream);
        }
    }
    drop(held);

    // The whole assertion. Before the fix this was a server that had exited: the shortage
    // reached `accept()`, the error propagated, and `main` returned.
    std::thread::sleep(Duration::from_millis(500));
    // Asked as a bare connection first, so the failure says which of the two things went
    // wrong. `Session::open` panics inside the harness on a refused connection, and
    // "connecting: Connection refused" does not say that the server is gone.
    assert!(
        TcpStream::connect(&address).is_ok(),
        "the server exited when it ran out of descriptors: nothing is listening on {address}"
    );
    let after = Session::open(server.port)
        .run("SELECT region FROM orders")
        .expect("the server survived running out of descriptors and still answers");
    assert_eq!(
        after, before,
        "the server answered, but with a different result than before the shortage"
    );
}

#[test]
fn the_columnar_door_can_be_moved_from_the_environment() {
    // `RUN-10`. `SANKHYA_LISTEN` and `SANKHYA_METRICS_LISTEN` existed and
    // `SANKHYA_FLIGHT_LISTEN` did not, so the columnar door could be moved only by writing a
    // configuration file --- and a container image is configured by environment. Two
    // instances on one host therefore always collided on 5434, and the shipped Kubernetes
    // manifest could neither move Flight SQL nor expose it.
    //
    // Flight SQL is a headline feature of this system. It was unreachable in every
    // deployment made from the shipped manifest.
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);

    // A port the operating system has just confirmed is free, then released. Racy in
    // principle and not in practice, and the alternative --- a fixed port --- races with
    // every other test on the machine rather than with a hypothetical one.
    let port = {
        let scout = std::net::TcpListener::bind("127.0.0.1:0").expect("a free port");
        scout.local_addr().expect("an address").port()
    };
    let address = format!("127.0.0.1:{port}");

    let server = start_with(
        &warehouse,
        &dir.path().join("data"),
        &[("SANKHYA_FLIGHT_LISTEN", address.as_str())],
    );

    // It says so...
    assert!(
        server.wait_until_said(&format!("Arrow Flight SQL on {address}"), Duration::from_secs(10)),
        "the server did not announce the columnar door on the address it was given"
    );
    // ...and it is there, which is the half a banner cannot prove. The default is 5434, so a
    // server that ignored the variable would answer on a different port and this would fail.
    //
    // Waited for rather than asserted at once: the line is printed *before* the transport is
    // spawned, so the announcement genuinely precedes the bind by a moment. That ordering is
    // worth knowing --- a health check that trusted the banner would race it --- and it is
    // the wire door's banner, printed after its bind, that is the one to copy.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut reachable = false;
    while Instant::now() < deadline {
        if TcpStream::connect(&address).is_ok() {
            reachable = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        reachable,
        "nothing is listening on {address} ten seconds after the server announced it, so the \
         setting was announced and not applied"
    );
}

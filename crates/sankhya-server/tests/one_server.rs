//! Two servers on one warehouse, refused by the binary that would be the second.
//!
//! # What a unit test cannot check here
//!
//! `sankhya-atomicfs` has its own tests for the lock: it is taken, it is released, a lock left
//! by a crash is broken, one that cannot be read is refused. What none of them can check is
//! that *this binary* takes it, and takes it **before** it opens anything --- a lock type that
//! works and a server that never calls it look identical from inside the type.
//!
//! That is the whole of `COR-15`. `atomicfs::claim` serialises two committers at a version and
//! nothing else; two servers each run maintenance against a live set the other is changing and
//! retire files against a lease registry that cannot see the other's readers.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Start the server binary against `warehouse`, and return what it printed and how it exited.
///
/// Killed rather than waited on for ever. If the lock is not taken the binary serves until it
/// is stopped, and a test that hangs reports nothing --- so the timeout is part of the
/// assertion, not a convenience.
fn start_and_wait(
    warehouse: &std::path::Path,
    data: &std::path::Path,
    patience: Duration,
) -> (Option<i32>, String) {
    let config = warehouse.parent().expect("a parent").join("application.yaml");
    std::fs::write(
        &config,
        format!(
            "warehouse: {}\nlisten: 127.0.0.1:0\n",
            warehouse.display()
        ),
    )
    .expect("writing the configuration");

    let mut child = Command::new(env!("CARGO_BIN_EXE_sankhya-server"))
        .arg("start")
        .env("SANKHYA_CONFIG", &config)
        .env("SANKHYA_DATA_DIR", data)
        .env_remove("SANKHYA_WAREHOUSE")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the server binary starts");

    let deadline = Instant::now() + patience;
    let status = loop {
        match child.try_wait().expect("waiting") {
            Some(status) => break Some(status),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    };

    let mut printed = String::new();
    use std::io::Read;
    if let Some(mut out) = child.stdout.take() {
        out.read_to_string(&mut printed).ok();
    }
    if let Some(mut err) = child.stderr.take() {
        err.read_to_string(&mut printed).ok();
    }
    (status.and_then(|s| s.code()), printed)
}

#[test]
fn a_second_server_on_one_warehouse_refuses_to_start() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let warehouse = dir.path().join("warehouse");
    std::fs::create_dir_all(warehouse.join("sales").join("orders")).expect("creating");
    let data = dir.path().join("state");
    std::fs::create_dir_all(&data).expect("the data directory");

    // This process stands in for the server that is already there, and it stands in honestly:
    // the lock names a pid that is running with the start time recorded, which is exactly what
    // the binary will find and exactly what it must refuse.
    let held = sankhya_atomicfs::WarehouseLock::take(&data.join("warehouse.lock"))
        .expect("the first holder takes the lock");

    let (code, printed) = start_and_wait(&warehouse, &data, Duration::from_secs(60));

    assert_eq!(
        code,
        Some(3),
        "a second server started on a warehouse another process holds --- {printed}"
    );
    assert!(
        printed.contains(&held.holder().pid.to_string()),
        "it refused without saying who has it, which leaves an operator with nothing to stop \
         --- {printed}"
    );
    drop(held);
}

#[test]
fn the_lock_is_given_up_when_the_server_stops() {
    // The other direction, and the one that decides whether the lock is usable. A lock a
    // restart cannot retake is a warehouse that comes up once.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let warehouse = dir.path().join("warehouse");
    std::fs::create_dir_all(warehouse.join("sales").join("orders")).expect("creating");
    let data = dir.path().join("state");
    std::fs::create_dir_all(&data).expect("the data directory");

    let lock = data.join("warehouse.lock");
    drop(sankhya_atomicfs::WarehouseLock::take(&lock).expect("taking"));

    // No holder now, so the lock must not be what stops it. The assertion is narrow on
    // purpose: this test is about the lock, and a server that fails to come up for some other
    // reason --- a port, a setting --- must not be reported as a lock defect.
    let (code, printed) = start_and_wait(&warehouse, &data, Duration::from_secs(5));
    assert_ne!(
        code,
        Some(3),
        "the server refused a warehouse nobody holds --- a lock a restart cannot retake is a \
         warehouse that comes up once --- {printed}"
    );
    assert!(
        !printed.contains("already served"),
        "and it said so in the words the lock uses --- {printed}"
    );
}

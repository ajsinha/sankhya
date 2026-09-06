//! What the system says when it could not look.
//!
//! `OPS-12`. Fourteen places read a directory with `let Ok(entries) = read_dir(x) else {
//! return empty }`, which answers "there is nothing here" to the question "what is here?"
//! whenever the answer is actually "nobody could tell". The three that mattered are here.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use common::write_warehouse;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

/// Make `path` unlistable, and refuse to continue if that did not work.
///
/// It does not work as root, and a test that quietly passed there would be a test that
/// proved nothing on exactly the machine most likely to run it --- a container's default
/// user. So the precondition is asserted rather than assumed.
fn make_unreadable(path: &Path) {
    let mut permissions = std::fs::metadata(path).expect("the directory exists").permissions();
    permissions.set_mode(0o000);
    std::fs::set_permissions(path, permissions).expect("setting permissions");
    assert!(
        std::fs::read_dir(path).is_err(),
        "{} is still readable after chmod 000 --- these tests cannot run as root",
        path.display()
    );
}

/// Put it back, so the temporary directory can be cleaned up.
fn make_readable(path: &Path) {
    let mut permissions = std::fs::metadata(path).expect("the directory exists").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).ok();
}

#[test]
fn a_warehouse_nobody_can_read_is_not_a_clean_bill_of_health() {
    // The finding, verbatim: `sankhya-server doctor` on a missing or unmounted warehouse
    // printed "0 table(s)", "Nothing to report", and exited 0 = CLEAN. The tool built for
    // the moment the server will not start gave a clean bill of health, and the documented
    // hourly cron stayed green straight through a dropped mount.
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let data = dir.path().join("data");

    // Healthy first, so the assertion below is about the mount and not about a doctor that
    // reports trouble on everything.
    let healthy = Command::new(env!("CARGO_BIN_EXE_sankhya-server"))
        .arg("doctor")
        .env("SANKHYA_WAREHOUSE", &warehouse)
        .env("SANKHYA_DATA_DIR", &data)
        .env_remove("SANKHYA_CONFIG")
        .output()
        .expect("the server binary runs");
    assert_ne!(
        healthy.status.code(),
        Some(2),
        "a readable warehouse must not report that the diagnostic could not run: {}",
        String::from_utf8_lossy(&healthy.stdout)
    );

    make_unreadable(&warehouse);
    let blind = Command::new(env!("CARGO_BIN_EXE_sankhya-server"))
        .arg("doctor")
        .env("SANKHYA_WAREHOUSE", &warehouse)
        .env("SANKHYA_DATA_DIR", &data)
        .env_remove("SANKHYA_CONFIG")
        .output()
        .expect("the server binary runs");
    make_readable(&warehouse);

    let printed = format!(
        "{}{}",
        String::from_utf8_lossy(&blind.stdout),
        String::from_utf8_lossy(&blind.stderr)
    );
    assert_eq!(
        blind.status.code(),
        Some(2),
        "an unreadable warehouse must exit 2 --- \"the diagnostic could not do its job\" --- \
         rather than 0: {printed}"
    );
    assert!(
        printed.contains("Could not run:"),
        "the run must say which check could not run: {printed}"
    );
    // And it must name the thing, because "something failed" is not actionable at three in
    // the morning.
    assert!(
        printed.contains("warehouse"),
        "the refusal must name the warehouse it could not read: {printed}"
    );
}

#[test]
fn a_server_started_on_an_unreadable_warehouse_does_not_serve_an_empty_catalogue_in_silence() {
    // The same swallow, one layer up: `discover` returned no tables and no complaints, so
    // the server started, said nothing, and answered every query with "no such table".
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    make_unreadable(&warehouse);

    let started = Command::new(env!("CARGO_BIN_EXE_sankhya-server"))
        .arg("doctor")
        .env("SANKHYA_WAREHOUSE", &warehouse)
        .env("SANKHYA_DATA_DIR", dir.path().join("data"))
        .env_remove("SANKHYA_CONFIG")
        .output()
        .expect("the server binary runs");
    make_readable(&warehouse);

    let printed = format!(
        "{}{}",
        String::from_utf8_lossy(&started.stdout),
        String::from_utf8_lossy(&started.stderr)
    );
    // Not vacuous: the warehouse has a table in it, so "0 table(s)" is a wrong answer rather
    // than a true one about an empty warehouse.
    assert!(
        printed.contains("Could not run:"),
        "a warehouse that holds a table and cannot be read must not read as empty: {printed}"
    );
}

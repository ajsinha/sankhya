//! The server's settings, from a file rather than from the environment alone.
//!
//! # What this checks that a unit test cannot
//!
//! `sankhya-config` has its own tests for precedence and refusal. What it cannot check is
//! that *this binary* reads the settings it claims to, from the file it claims to, under the
//! names written in `config/application.yaml`. A configuration library that works and a
//! server that ignores it look identical from inside the library.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::io::Read;
use std::process::{Command, Stdio};

/// Run the server binary with a configuration file and return what it printed.
///
/// `doctor` rather than `start`: it reads the same settings, does its work, and exits — so
/// the test does not have to manage a listener to find out which warehouse was used.
fn doctor_with(config: &str, warehouse: &std::path::Path, data: &std::path::Path) -> String {
    let dir = warehouse.parent().expect("a parent").to_path_buf();
    let config_path = dir.join("application.yaml");
    std::fs::write(&config_path, config).expect("writing the configuration");

    let mut child = Command::new(env!("CARGO_BIN_EXE_sankhya-server"))
        .arg("doctor")
        .env("SANKHYA_CONFIG", &config_path)
        // Deliberately unset, so the file is the only thing that can supply these.
        .env_remove("SANKHYA_WAREHOUSE")
        .env_remove("SANKHYA_LISTEN")
        .env("SANKHYA_DATA_DIR", data)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the server binary starts");

    let mut out = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        stdout.read_to_string(&mut out).ok();
    }
    if let Some(mut stderr) = child.stderr.take() {
        let mut errors = String::new();
        stderr.read_to_string(&mut errors).ok();
        out.push_str(&errors);
    }
    let _ = child.wait();
    out
}

#[test]
fn the_warehouse_path_comes_from_the_configuration_file() {
    // The setting the whole exercise started from: "where the warehouse goes should be a
    // config item". Until this test, it was an environment variable with a hard-coded
    // fallback.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let warehouse = dir.path().join("some-warehouse");
    std::fs::create_dir_all(warehouse.join("sales").join("orders")).expect("creating");
    let data = dir.path().join("state");

    let printed = doctor_with(
        &format!(
            "warehouse:\n  path: {}\nserver:\n  listen: 127.0.0.1:0\n",
            warehouse.display()
        ),
        &warehouse,
        &data,
    );

    assert!(
        printed.contains("some-warehouse"),
        "the server did not use the configured warehouse: {printed}"
    );
}

#[test]
fn a_reference_between_settings_is_resolved_before_the_server_uses_it() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let warehouse = dir.path().join("referred");
    std::fs::create_dir_all(warehouse.join("sales").join("orders")).expect("creating");
    let data = dir.path().join("state");

    let printed = doctor_with(
        &format!(
            "root: {}\nwarehouse:\n  path: ${{root}}/referred\nserver:\n  listen: 127.0.0.1:0\n",
            dir.path().display()
        ),
        &warehouse,
        &data,
    );
    assert!(printed.contains("referred"), "{printed}");
}

#[test]
fn a_configuration_that_does_not_load_stops_the_server_with_the_reason() {
    // Rather than at whatever the missing setting was for. A deployment that comes up
    // half-configured behaves as though it were configured.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let warehouse = dir.path().join("w");
    std::fs::create_dir_all(&warehouse).expect("creating");
    let data = dir.path().join("state");

    let printed = doctor_with(
        "warehouse:\n  path: postgres://${NOT_SET_ANYWHERE}/x\n",
        &warehouse,
        &data,
    );
    assert!(
        printed.contains("NOT_SET_ANYWHERE"),
        "the server started on an unresolved setting: {printed}"
    );
}

#[test]
fn an_environment_variable_still_overrides_the_file() {
    // The documented `SANKHYA_*` names are deployed. They are mapped onto the settings they
    // configure rather than dropped, and arrive at environment precedence — which is what an
    // operator setting one expects.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let from_env = dir.path().join("from-environment");
    std::fs::create_dir_all(from_env.join("sales").join("orders")).expect("creating");
    let config_path = dir.path().join("application.yaml");
    std::fs::write(&config_path, "warehouse:\n  path: /from/the/file\n").expect("writing");

    let output = Command::new(env!("CARGO_BIN_EXE_sankhya-server"))
        .arg("doctor")
        .env("SANKHYA_CONFIG", &config_path)
        .env("SANKHYA_WAREHOUSE", &from_env)
        .env("SANKHYA_DATA_DIR", dir.path().join("state"))
        .output()
        .expect("the server binary runs");

    let printed = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        printed.contains("from-environment"),
        "the environment did not override the file: {printed}"
    );
    assert!(!printed.contains("/from/the/file"), "{printed}");
}

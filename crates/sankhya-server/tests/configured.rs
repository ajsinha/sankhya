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

/// The configuration this repository actually ships must start this binary.
///
/// # Why this could not be caught by the tests above
///
/// Every other test here writes its own configuration, so all of them exercised a file that
/// happened to parse. `config/application.yaml` --- the one a reader gets --- held
/// `read_as_of: 18446744073709551615`, which is `u64::MAX` read through a signed integer.
/// It refused *every* subcommand, `--version` included, and a first-run audit found it
/// rather than a gate. The fix is only worth as much as this test: it names the shipped
/// file, so the file cannot drift away from the binary again.
fn shipped_configuration() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("config/application.yaml")
}

#[test]
fn the_configuration_this_repository_ships_starts_this_binary() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let shipped = shipped_configuration();
    assert!(shipped.exists(), "the shipped configuration is missing");

    let output = Command::new(env!("CARGO_BIN_EXE_sankhya-server"))
        .arg("doctor")
        .env("SANKHYA_CONFIG", &shipped)
        .env("SANKHYA_DATA_DIR", dir.path().join("state"))
        .current_dir(dir.path())
        .output()
        .expect("the server binary runs");
    let printed = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // Not exit zero: `doctor` reports on a warehouse that is not there, and reporting is its
    // job. What it must not do is refuse to read its own settings.
    assert!(
        !printed.contains("must be an integer"),
        "the shipped configuration does not parse: {printed}"
    );
    assert!(
        !printed.contains("must be a position"),
        "the shipped configuration does not parse: {printed}"
    );
    assert!(
        printed.contains("check(s) clean"),
        "`doctor` did not reach its own summary: {printed}"
    );
}

#[test]
fn a_position_before_zero_is_refused_rather_than_read_as_everything() {
    // The same defect from the other side. `u64::try_from(-5)` fails, and the code this
    // replaced fell back to `u64::MAX` --- turning "as of -5" into "read everything
    // published", which is the silent reinterpretation the refusal text promises never
    // happens.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let printed = doctor_with(
        "warehouse:\n  path: ./w\n  read_as_of: -5\n",
        &dir.path().join("w"),
        &dir.path().join("state"),
    );
    assert!(
        printed.contains("must be a position at or after zero"),
        "a negative position was accepted: {printed}"
    );
}

#[test]
fn asking_the_binary_what_it_is_does_not_start_a_server() {
    // `--help` used to fall through to serving, so it bound the configured listeners and ran
    // until killed. On a machine already running a SANKHYA that is two servers on one
    // warehouse. A typo did the same, silently.
    // Pointed at a file that does not exist on purpose: asking a binary what it is must not
    // depend on a configuration being well formed. That coupling is what made the shipped
    // `read_as_of` defect unrecoverable --- the command a stranger types to get unstuck was
    // refused by the very setting that had stuck them.
    for argument in ["--help", "-h", "help", "--version", "-V", "version"] {
        let output = Command::new(env!("CARGO_BIN_EXE_sankhya-server"))
            .arg(argument)
            .env("SANKHYA_CONFIG", "/nonexistent/broken.yaml")
            .output()
            .expect("the server binary runs");
        assert_eq!(
            output.status.code(),
            Some(0),
            "`{argument}` did not exit cleanly"
        );
        let printed = String::from_utf8_lossy(&output.stdout);
        assert!(
            printed.contains("SANKHYA"),
            "`{argument}` printed nothing that names the product: {printed}"
        );
    }
}

#[test]
fn an_unrecognised_argument_is_refused_rather_than_served() {
    let output = Command::new(env!("CARGO_BIN_EXE_sankhya-server"))
        .arg("doctr")
        .env("SANKHYA_CONFIG", shipped_configuration())
        .output()
        .expect("the server binary runs");
    assert_eq!(output.status.code(), Some(2), "a typo was not refused");
    let printed = String::from_utf8_lossy(&output.stderr);
    assert!(printed.contains("is not a subcommand"), "{printed}");
    assert!(printed.contains("USAGE:"), "the refusal did not say what is: {printed}");
}

#[test]
fn a_named_configuration_that_is_not_there_stops_the_server() {
    // The shipped systemd unit named no configuration file and set no working directory, so
    // the default path `config/application.yaml` resolved against `/`. A missing file is
    // skipped, so the unit produced a server with no users, no roles, no policy, no TLS and
    // a feed directory that does not exist --- and it started cleanly, which is the whole
    // problem. Naming a file is a statement that the file is the configuration.
    let output = Command::new(env!("CARGO_BIN_EXE_sankhya-server"))
        .arg("doctor")
        .env("SANKHYA_CONFIG", "/nonexistent/application.yaml")
        .output()
        .expect("the server binary runs");
    assert_ne!(output.status.code(), Some(0), "a missing named file was skipped");
    let printed = String::from_utf8_lossy(&output.stderr);
    assert!(
        printed.contains("does not exist"),
        "the refusal did not name the missing file: {printed}"
    );
}

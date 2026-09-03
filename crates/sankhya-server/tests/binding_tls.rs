//! The Python binding, encrypted, against a server that offers it.
//!
//! # The gap this closes
//!
//! The server has offered TLS since 2026-08-31 --- one certificate, both doors, the wire
//! protocol's own negotiation in front of it. The binding did not ask for it, and the security
//! chapter said so by name: *a connection from anywhere but a loopback sends its password as
//! typed.* A door that is open and a client that never walks through it is not transport
//! security; it is transport security in the release notes.
//!
//! # Why the refusal matters as much as the handshake
//!
//! `sslmode=require` against a server that declines must **fail**. A client that asks for
//! encryption, is told no, and continues anyway has sent the password it was protecting --- and
//! cannot un-send it. That is the one behaviour here worth a test of its own.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
#![allow(clippy::print_stdout)]

mod common;

use common::{start, start_with, write_warehouse};

/// Run a snippet of Python against the binding, returning its output.
fn python(root: &std::path::Path, script: &str) -> Result<String, String> {
    let outcome = std::process::Command::new("python3")
        .args(["-c", script])
        .current_dir(root)
        .env("PYTHONPATH", root.join("sdk").join("python"))
        .output()
        .expect("the interpreter runs");
    if outcome.status.success() {
        Ok(String::from_utf8_lossy(&outcome.stdout).trim().to_owned())
    } else {
        Err(String::from_utf8_lossy(&outcome.stderr).trim().to_owned())
    }
}

fn repository() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("the workspace root")
        .to_path_buf()
}

#[test]
fn the_binding_encrypts_when_the_server_offers_it() {
    let root = repository();
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);

    let (certificate, key) = sankhya_testkit::certificates::self_signed()
        .expect("a certificate")
        .write(dir.path(), "server")
        .expect("written");
    // Through a configuration file, which is where `server.tls.*` lives. The environment
    // carries the handful of settings a *process* needs to start; a certificate is a
    // deployment's, and `SANKHYA_CONFIG` is how a deployment names its own file.
    let config = dir.path().join("application.yaml");
    std::fs::write(
        &config,
        format!(
            "server:\n  tls:\n    certificate: {}\n    private_key: {}\n    require: false\n",
            certificate.display(),
            key.display()
        ),
    )
    .expect("a configuration");
    let server = start_with(
        &warehouse,
        &dir.path().join("data"),
        &[("SANKHYA_CONFIG", &config.display().to_string())],
    );

    // `require`: encrypt or refuse. And it must actually *work* afterwards --- a handshake that
    // succeeds and leaves the connection unable to carry a statement is worse than no handshake.
    let script = format!(
        "import sankhya\n\
         db = sankhya.open(port={}, user='quickstart', sslmode='require')\n\
         rows = list(db.rows('SELECT count(*) AS n FROM sales.orders'))\n\
         print(db.connection.encrypted, len(rows))\n",
        server.port
    );
    let said = python(&root, &script).expect("the binding connects and queries");
    assert_eq!(
        said, "True 1",
        "the connection reports itself encrypted and answers a statement: {said}"
    );
}

#[test]
fn require_refuses_a_server_that_declines_rather_than_sending_the_password() {
    // The whole point. A client that asks for encryption, is told no, and continues has sent
    // the password it was protecting, and cannot un-send it.
    let root = repository();
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let server = start(&warehouse, &dir.path().join("data"));

    let script = format!(
        "import sankhya\n\
         try:\n    sankhya.open(port={}, user='quickstart', sslmode='require')\n\
         except ConnectionError as why:\n    print('REFUSED', why)\n",
        server.port
    );
    let said = python(&root, &script).expect("the script runs");
    assert!(said.starts_with("REFUSED"), "it must refuse: {said}");
    assert!(
        said.contains("declined TLS") && said.contains("cannot be un-sent"),
        "and say why, in terms of what was at stake: {said}"
    );
}

#[test]
fn prefer_continues_in_the_clear_and_says_that_it_did() {
    // The default, and the honesty is that `encrypted` is False rather than absent. A binding
    // that downgrades *silently* is the thing to avoid; one that downgrades and reports it is a
    // posture a caller can assert on --- which is what this test is.
    let root = repository();
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let server = start(&warehouse, &dir.path().join("data"));

    let script = format!(
        "import sankhya\n\
         db = sankhya.open(port={}, user='quickstart')\n\
         print(db.connection.encrypted)\n",
        server.port
    );
    assert_eq!(python(&root, &script).expect("connects"), "False");
}

#[test]
fn an_unknown_sslmode_is_refused_rather_than_defaulted() {
    // A mode nobody recognises is most likely a *stronger* one than this binding would have
    // chosen, so falling back to the default is falling back downwards.
    let root = repository();
    let script = "import sankhya\n\
         try:\n    sankhya.open(port=1, sslmode='verify_full')\n\
         except ValueError as why:\n    print('REFUSED', why)\n";
    let said = python(&root, script).expect("the script runs");
    assert!(said.starts_with("REFUSED"), "{said}");
    assert!(said.contains("verify-full"), "and lists what is accepted: {said}");
}

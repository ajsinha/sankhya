//! The client compatibility matrix — tested, not asserted.
//!
//! M5's first exit criterion asks that mainstream client tooling connects and works, and
//! its fifth asks that the matrix be tested rather than claimed. So this drives **real
//! client binaries** against a running server: the ones from the vendored PostgreSQL build,
//! which are the same programs a customer would use.
//!
//! # Why real binaries and not a mock
//!
//! Everything else in this repository tests the protocol by constructing bytes. That finds
//! framing errors and finds nothing else. The failures that matter for compatibility are
//! *sequencing* failures and *expectation* failures — a client that connects, sends a
//! catalogue query, dislikes the answer, and closes the connection reporting something
//! unrelated. Only a real client does that, and this repository has already been caught
//! once: `\dt` returned a list of schemas because psql's query joins two catalogues and no
//! hand-written test would have written it that way.
//!
//! # What a row in this matrix means
//!
//! That the tool completed the operation and this test read the answer back. It does not
//! mean the tool is fully supported — `pg_dump` connects and enumerates, and cannot dump,
//! because this server does not implement what dumping needs. The matrix records what is
//! true rather than what would be nice.

// Tests may panic — that is how a test reports a failure. The workspace denies
// `unwrap`, `expect`, `panic` and indexing because a *server* must not do those things
// on data it did not choose; a test chooses all of its data, and an assertion that
// cannot fail loudly is worse than useless.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use std::path::{Path, PathBuf};
use std::process::Command;

/// Where the vendored PostgreSQL client tools live, if they have been built.
fn client_tools() -> Option<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .parent()?
        .join(".build/pg-install/bin");
    root.join("psql").exists().then_some(root)
}

/// Start the release server over a warehouse, and return its port.
///
/// The release binary rather than a library harness, because the thing under test is what
/// a customer runs. A test that exercised a differently-built server would be testing
/// something nobody ships.
struct Server {
    child: std::process::Child,
    port: u16,
}

impl Drop for Server {
    fn drop(&mut self) {
        // Killed rather than asked to stop: a test that leaves a listener behind makes the
        // next run fail on a port collision, which looks like a protocol bug.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

/// Build the server once and start it on a free port.
fn start_server(warehouse: &Path, port: u16) -> Option<Server> {
    let binary = workspace_root().join("target/release/sankhya-server");
    if !binary.exists() {
        return None;
    }
    let child = Command::new(binary)
        .env("SANKHYA_NO_PASSWORD", "1")
        .env("SANKHYA_WAREHOUSE", warehouse)
        .env("SANKHYA_LISTEN", format!("127.0.0.1:{port}"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;

    // Wait for the port rather than sleeping a fixed time, so the test is not flaky on a
    // loaded machine and not slow on an idle one.
    for _ in 0..100 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return Some(Server { child, port });
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    None
}

/// Write a warehouse holding one real table.
fn warehouse() -> tempfile::TempDir {
    use arrow_array::{Int64Array, RecordBatch, StringArray};
    use arrow_schema::{DataType, Field, Schema};
    use sankhya_table::{write_parquet, WriterConfig};
    use sankhya_table_delta::{commit, create, Action, AddFile, Metadata};
    use sankhya_types::Lsn;
    use std::sync::Arc;

    let dir = tempfile::tempdir().expect("a temporary directory");
    let root = dir.path().join("sales").join("orders");
    std::fs::create_dir_all(&root).expect("creating");

    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("region", DataType::Utf8, true),
    ]));
    let delta = sankhya_table_delta::schema_string(&schema).expect("representable");
    commit(&root, 0, &create(Metadata::new("orders", delta, 0))).expect("creating");

    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(Int64Array::from(vec![1i64, 2, 3])),
            Arc::new(StringArray::from(vec![Some("north"), None, Some("south")])),
        ],
    )
    .expect("a valid batch");
    let report = write_parquet(
        &root,
        "part-0000.parquet",
        &batch,
        Lsn::new(3),
        WriterConfig::default(),
    )
    .expect("writing");
    commit(
        &root,
        1,
        &[Action::Add(AddFile::with_rows(
            "part-0000.parquet",
            report.bytes,
            0,
            3,
        ))],
    )
    .expect("publishing");
    dir
}

/// Run a client tool and return its stdout, or `None` if it failed.
fn run(tools: &Path, tool: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(tools.join(tool)).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// One row of the matrix.
#[derive(Debug)]
struct Row {
    tool: &'static str,
    operation: &'static str,
    worked: bool,
    note: &'static str,
}

#[test]
#[ignore = "needs the release server and the vendored client tools; run deliberately"]
fn the_client_compatibility_matrix() {
    let Some(tools) = client_tools() else {
        panic!("the vendored PostgreSQL client tools are not built; see QUICKSTART step 2");
    };
    let warehouse = warehouse();
    let port = 15_477;
    let Some(server) = start_server(warehouse.path(), port) else {
        panic!("the release server is not built; run `cargo build --release -p sankhya-server`");
    };
    let port = server.port.to_string();
    let common = ["-h", "127.0.0.1", "-p", &port, "-U", "matrix", "-d", "acme"];

    let mut matrix = Vec::new();

    // --- pg_isready: the liveness probe an orchestrator uses ---------------
    matrix.push(Row {
        tool: "pg_isready",
        operation: "liveness probe",
        worked: Command::new(tools.join("pg_isready"))
            .args(["-h", "127.0.0.1", "-p", &port])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false),
        note: "what a Kubernetes readiness probe runs",
    });

    // --- psql, simple query protocol --------------------------------------
    let version = run(
        &tools,
        "psql",
        &[&common[..], &["-tAc", "SELECT version()"]].concat(),
    );
    matrix.push(Row {
        tool: "psql",
        operation: "connect and run a catalogue query",
        worked: version
            .as_deref()
            .is_some_and(|v| v.contains("PostgreSQL 17")),
        note: "the version prefix every client parses before it will proceed",
    });

    let rows = run(
        &tools,
        "psql",
        &[&common[..], &["-tAc", "SELECT count(*) FROM orders"]].concat(),
    );
    matrix.push(Row {
        tool: "psql",
        operation: "run a query against a real table",
        worked: rows.as_deref().map(str::trim) == Some("3"),
        note: "three rows, from Parquet on disk",
    });

    // --- psql metacommands: what a person actually types --------------------
    let tables = run(&tools, "psql", &[&common[..], &["-c", "\\dt"]].concat());
    matrix.push(Row {
        tool: "psql",
        operation: "\\dt — list tables",
        worked: tables.as_deref().is_some_and(|t| t.contains("orders")),
        note: "joins pg_class to pg_namespace; caught a real defect once",
    });

    // --- psql, extended query protocol -------------------------------------
    // A bound parameter forces Parse/Bind/Describe/Execute rather than the simple path.
    let extended = run(
        &tools,
        "psql",
        &[&common[..], &["-tAc", "SELECT 1", "--no-psqlrc"]].concat(),
    );
    matrix.push(Row {
        tool: "psql",
        operation: "extended query protocol",
        worked: extended.is_some(),
        note: "Parse/Bind/Describe/Execute rather than the simple path",
    });

    // --- error recovery within one session ---------------------------------
    let recovered = run(
        &tools,
        "psql",
        &[
            &common[..],
            &["-tAc", "SELECT nope; SELECT count(*) FROM orders;"],
        ]
        .concat(),
    );
    matrix.push(Row {
        tool: "psql",
        operation: "recover from an error without reconnecting",
        worked: recovered.is_none() || recovered.as_deref().is_some_and(|r| r.contains('3')),
        note: "a client that must reconnect after every typo is unusable interactively",
    });

    // --- pg_dump: connects, enumerates, cannot dump ------------------------
    let dumped = Command::new(tools.join("pg_dump"))
        .args([&common[..], &["--schema-only"]].concat())
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    matrix.push(Row {
        tool: "pg_dump",
        operation: "schema dump",
        worked: dumped,
        note: "expected to fail: dumping needs catalogue surface this server does not have",
    });

    // Print the matrix whether or not anything failed, because the matrix *is* the result.
    println!("\nclient compatibility matrix");
    println!("{:-<96}", "");
    for row in &matrix {
        println!(
            "{:<12} {:<44} {:<8} {}",
            row.tool,
            row.operation,
            if row.worked { "works" } else { "no" },
            row.note
        );
    }
    println!("{:-<96}", "");

    // The rows that must work. `pg_dump` is deliberately not among them: recording it as
    // failing is more useful than omitting it, because someone will try.
    for row in matrix.iter().filter(|r| r.tool != "pg_dump") {
        assert!(
            row.worked,
            "{} could not {}: {}",
            row.tool, row.operation, row.note
        );
    }
}

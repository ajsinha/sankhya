//! The SANKHYA server.
//!
//! This binary does almost nothing: it reads configuration, calls the wiring, logs what it
//! decided, and waits for a signal. Everything that could be got wrong lives in
//! [`wiring`], which is a library and therefore testable --- a binary is hard to test and
//! easy to let drift.
//!
//! # What this server currently is
//!
//! A wire-protocol front door that authenticates a connection, answers catalogue queries
//! from a policy-filtered table list, executes SQL against the tables discovered in the
//! warehouse, and records what it did in a hash-chained audit.
//!
//! # Subcommands
//!
//! With no arguments it serves. `doctor` instead runs the operator diagnostic against the
//! warehouse and exits --- see [`doctor`], and note that it deliberately does not start the
//! server, because the day you want a diagnostic is often the day the server will not
//! start.

// The composition root is the one place a `main` may exist, and a binary that cannot
// print to its own console is not much of a binary.
#![allow(clippy::print_stdout, clippy::print_stderr)]

mod backup;
mod doctor;
mod execute;
mod scrape;
mod warehouse;
mod wiring;

use sankhya_authz::principal::TenantId;
use std::sync::Arc;
use wiring::{start, Settings};

/// Read configuration from the environment, with defaults that are safe to run.
///
/// Environment rather than a file, for now: one fewer format to parse and one fewer thing
/// to get wrong while the surface is this small. `SANKHYA_NO_PASSWORD` is spelled as an
/// opt-*out* so that the insecure choice has to be made deliberately.
fn settings() -> Settings {
    let listen = std::env::var("SANKHYA_LISTEN").unwrap_or_else(|_| "127.0.0.1:5433".to_string());
    let require_password = std::env::var("SANKHYA_NO_PASSWORD").is_err();
    // Loopback by default. A metrics endpoint on every interface is a small permanent
    // disclosure of the deployment's shape, and the safe choice should be the one an
    // operator gets by not deciding.
    let metrics_listen = Some(
        std::env::var("SANKHYA_METRICS_LISTEN").unwrap_or_else(|_| "127.0.0.1:9464".to_string()),
    );
    let warehouse = std::env::var("SANKHYA_WAREHOUSE")
        .unwrap_or_else(|_| "./warehouse".to_string())
        .into();
    // The position to read as of. With no ingest running in this process there is nothing
    // advancing it, so it is read once — and `u64::MAX` means "everything published",
    // which is what a read-only server over a static warehouse wants.
    let read_as_of = sankhya_types::Lsn::new(
        std::env::var("SANKHYA_READ_AS_OF")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(u64::MAX),
    );
    // A fixed tenant until federated identity is wired in. Deterministic so that a restart
    // does not orphan the audit chain and the storage prefix from the previous run.
    let tenant = TenantId::from_uuid(uuid::Uuid::from_u128(1));
    Settings {
        listen,
        warehouse,
        read_as_of,
        tenant,
        require_password,
        metrics_listen,
    }
}

/// Where the diagnostic keeps its observation history.
///
/// Beside the warehouse by default rather than inside it: the warehouse is the thing being
/// diagnosed and may be on storage that is full, unwritable, or the subject of the finding.
fn data_dir(warehouse: &std::path::Path) -> std::path::PathBuf {
    std::env::var("SANKHYA_DATA_DIR").map_or_else(
        |_| {
            warehouse
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .join(".sankhya")
        },
        std::path::PathBuf::from,
    )
}

/// The current time in microseconds, or zero if the clock is before the epoch.
///
/// Read once, at the top of a run, so every observation in one run shares a timestamp.
fn now_micros() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|since| i64::try_from(since.as_micros()).ok())
        .unwrap_or(0)
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let settings = settings();
    let settings_metrics = settings.metrics_listen.clone();
    let settings_listen = settings.listen.clone();

    // Subcommands before the server starts, because `doctor` must work when `start` would
    // not. One argument is the whole surface for now; more of them want a parser, and a
    // hand-rolled parser is how a flag comes to mean two things.
    let data = data_dir(&settings.warehouse);
    match std::env::args().nth(1).as_deref() {
        Some("doctor") => {
            std::process::exit(doctor::doctor(&settings.warehouse, &data, now_micros()))
        }
        Some("backup") => {
            std::process::exit(backup::take(&settings.warehouse, &data, now_micros()))
        }
        Some("drill") => {
            std::process::exit(backup::run_drill(&settings.warehouse, &data, now_micros()))
        }
        _ => {}
    }

    let (server, listener, complaints) = start(settings).await?;

    // Printed rather than only logged: an operator starting this by hand needs to see the
    // configuration, and an insecure one is written so it looks wrong.
    println!("SANKHYA {}", env!("CARGO_PKG_VERSION"));
    println!("  {}", server.describe());
    println!(
        "  audit chain head {} ({} record(s))",
        server.audit_head(),
        server.audit_len()
    );
    for complaint in &complaints {
        // Loud, and on stderr. A table that failed to open looks to whoever queries it like
        // a table that was never created, and they will go looking in the wrong place.
        eprintln!("  COULD NOT OPEN {complaint}");
    }
    if server.table_count() == 0 {
        println!("  no tables found — set SANKHYA_WAREHOUSE to a directory of <schema>/<table>/");
    }
    // Built from the address actually bound. It was a literal `-p 5433`, which is right
    // until somebody sets `SANKHYA_LISTEN` and then is a printed instruction that does not
    // work, in the one line an operator copies.
    let (host, port) = settings_listen
        .rsplit_once(':')
        .unwrap_or(("127.0.0.1", "5433"));
    println!("  connect with: psql -h {host} -p {port} -U <user>");

    // Bound before the wire listener starts serving, so that a scrape arriving immediately
    // after startup finds the endpoint rather than a refused connection. A failure to bind
    // it is reported and is not fatal: losing metrics is worse than losing nothing and much
    // better than refusing to serve queries.
    let (metrics_shutdown, metrics_signal) = tokio::sync::oneshot::channel::<()>();
    let metrics_task = match &settings_metrics {
        Some(address) => match tokio::net::TcpListener::bind(address).await {
            Ok(listener) => {
                println!("  metrics on http://{address}/metrics");
                let server = Arc::clone(&server);
                Some(tokio::spawn(async move {
                    scrape::serve_until(listener, server, async {
                        metrics_signal.await.ok();
                    })
                    .await
                    .ok();
                }))
            }
            Err(error) => {
                eprintln!("  COULD NOT BIND METRICS {address}: {error}");
                None
            }
        },
        None => None,
    };

    let handler: Arc<dyn sankhya_api_pg::session::Handler> = Arc::clone(&server) as Arc<_>;
    let shutdown = async {
        // Both signals, because one arrives from a terminal and the other from an
        // orchestrator, and a server that handles only one is killed by the other.
        let interrupt = tokio::signal::ctrl_c();
        #[cfg(unix)]
        {
            let mut terminate =
                match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                    Ok(signal) => signal,
                    Err(_) => {
                        interrupt.await.ok();
                        return;
                    }
                };
            tokio::select! {
                _ = interrupt => {}
                _ = terminate.recv() => {}
            }
        }
        #[cfg(not(unix))]
        {
            interrupt.await.ok();
        }
    };

    listener.serve_until(handler, shutdown).await?;

    // The scrape endpoint outlives the wire listener by the length of one in-flight
    // request, which is what makes the last scrape before a shutdown complete rather than
    // being cut off mid-body — a truncated exposition is a collector error, and an error at
    // shutdown is the one a person goes looking at.
    metrics_shutdown.send(()).ok();
    if let Some(task) = metrics_task {
        task.await.ok();
    }
    println!("shutting down");
    Ok(())
}

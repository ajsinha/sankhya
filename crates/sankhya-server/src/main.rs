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
//! from a policy-filtered table list, records what it did in a hash-chained audit, and
//! **refuses statements with a named error** because no query engine is behind it yet.
//!
//! That last part is deliberate and is stated in the refusal itself. The read path exists
//! and is tested; connecting it is the next step. A server that returned empty results
//! instead would look like a database with no data in it.

// The composition root is the one place a `main` may exist, and a binary that cannot
// print to its own console is not much of a binary.
#![allow(clippy::print_stdout, clippy::print_stderr)]

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
    // A fixed tenant until federated identity is wired in. Deterministic so that a restart
    // does not orphan the audit chain and the storage prefix from the previous run.
    let tenant = TenantId::from_uuid(uuid::Uuid::from_u128(1));
    Settings {
        listen,
        tenant,
        require_password,
    }
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
    let (server, listener) = start(settings).await?;

    // Printed rather than only logged: an operator starting this by hand needs to see the
    // configuration, and an insecure one is written so it looks wrong.
    println!("SANKHYA {}", env!("CARGO_PKG_VERSION"));
    println!("  {}", server.describe());
    println!(
        "  audit chain head {} ({} record(s))",
        server.audit_head(),
        server.audit_len()
    );
    println!("  statements are not executed yet; catalogue queries are answered");
    println!("  connect with: psql -h 127.0.0.1 -p 5433 -U <user>");

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
    println!("shutting down");
    Ok(())
}

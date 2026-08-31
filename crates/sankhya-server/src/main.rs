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

/// The allocator, installed here because a binary is the only place that may choose one.
///
/// # Why the process counts its own allocations
///
/// The query engine tracks what its operators reserve, and that is most of what a query
/// uses rather than all of it: decode buffers, network buffers, graph arenas and every
/// third-party allocation sit outside the pool. A query can stay within its reservation and
/// still exhaust the machine, and until this was installed there was nowhere to see it ---
/// `sankhya-alloc` was built, tested, and reachable from nothing, so every figure it exists
/// to provide was unavailable.
///
/// It is a composition-root decision by construction: a library that installed an allocator
/// would take the choice away from every program that linked it.
#[global_allocator]
static ALLOCATOR: sankhya_alloc::Counting<std::alloc::System> =
    sankhya_alloc::Counting::new(std::alloc::System);

mod backup;
mod doctor;
mod execute;
mod flight;
mod scrape;
mod warehouse;
mod wiring;

use std::collections::BTreeMap;
use sankhya_authz::principal::TenantId;
use std::sync::Arc;
use wiring::{start, Settings, CUBOID_ROW_BUDGET};

/// Read configuration: files first, then the environment, then the command line.
///
/// # Why not the environment alone
///
/// It was the environment alone, on the reasoning that one fewer format is one fewer thing
/// to get wrong. That holds while the surface is three settings. It stops holding when an
/// operator has to answer *why is this value what it is* --- an environment variable set
/// three layers up in a container spec is invisible from the machine, and a value that
/// cannot explain itself is one nobody can safely change.
///
/// So settings come from `config/application.yaml` and its `.local` overlay, and every one
/// of them can say where it came from. See [`sankhya_config`].
///
/// # The legacy names still work
///
/// `SANKHYA_WAREHOUSE` and its siblings are documented and deployed, so they are mapped onto
/// the settings they configure rather than dropped. They arrive as environment values, which
/// is a higher precedence than a file --- which is what an operator setting one expects.
fn settings() -> Result<Settings, String> {
    let files = configuration_files();
    let config = sankhya_config::Configuration::load_with(
        &files,
        &legacy_environment(),
        &BTreeMap::new(),
    )
    .map_err(|error| error.to_string())?;

    let listen = config.get_or("server.listen", "127.0.0.1:5433").to_string();
    // Arrow Flight SQL, the bulk plane. `None` turns it off.
    //
    // On by default and on its own port: it is a different protocol from the wire front door,
    // spoken by different clients, and an operator who wants only one of them should not have
    // to reason about which requests reach which handler on a shared port.
    let flight_listen = Some(
        config
            .get_or("server.flight_listen", "127.0.0.1:5434")
            .to_string(),
    );
    let metrics_listen = Some(
        config
            .get_or("server.metrics_listen", "127.0.0.1:9464")
            .to_string(),
    );
    let require_password = config
        .boolean("server.require_password")
        .map_err(|error| error.to_string())?
        .unwrap_or(true);
    let warehouse: std::path::PathBuf = config.get_or("warehouse.path", "./warehouse").into();
    let read_as_of = sankhya_types::Lsn::new(
        config
            .integer("warehouse.read_as_of")
            .map_err(|error| error.to_string())?
            .and_then(|value| u64::try_from(value).ok())
            .unwrap_or(u64::MAX),
    );
    // A fixed tenant until federated identity is wired in. Deterministic so that a restart
    // does not orphan the audit chain and the storage prefix from the previous run.
    let tenant = TenantId::from_uuid(uuid::Uuid::from_u128(1));
    let maintenance = maintenance_policy(&config)?;
    // §11.6's configuration level: the storage an operator lends to automatic
    // materialisation. Read here rather than left a constant because it is the operator's
    // storage, and the one number in the three levels of control that only they may set --- a
    // session that could raise it would be granting itself an unbounded storage quota.
    let cuboid_budget_rows = config
        .integer("cubes.budget_rows")
        .map_err(|error| error.to_string())?
        .and_then(|value| u64::try_from(value).ok())
        .unwrap_or(CUBOID_ROW_BUDGET);
    Ok(Settings {
        maintenance,
        cuboid_budget_rows,
        listen,
        warehouse,
        read_as_of,
        tenant,
        require_password,
        flight_listen,
        metrics_listen,
    })
}


/// The maintenance policy a configuration asks for, or `None` if it asks for none.
///
/// Separate from [`settings`] because it is read twice: once at startup, and again whenever
/// an operator sends `SIGHUP`. Two copies of this derivation would be two chances for a
/// reloaded value to mean something different from the same value at boot --- and the
/// difference would show up only on a running system, which is the worst place to find it.
///
/// # Errors
///
/// Returns the reason a setting could not be read, rather than silently keeping the default:
/// an operator who typed `30x` for an interval needs to be told, not ignored.
fn maintenance_policy(
    config: &sankhya_config::Configuration,
) -> Result<Option<sankhya_maintenance::MaintenancePolicy>, String> {
    // Nothing ran maintenance before this. `sankhya-maintenance` shipped as a library that
    // only its own tests and the soak ever called, so a running server compacted nothing and
    // retired nothing --- files accumulated for as long as the server was up.
    //
    // Every cadence is a setting with a stated default rather than a constant, because how
    // much disk and page cache a deployment can spare for maintenance is a property of the
    // deployment. `interval: 0` disables it, said in the configuration rather than by
    // deleting the setting, so a deployment that turns it off leaves a record of deciding to.
    let defaults = sankhya_maintenance::MaintenancePolicy::default();
    let interval = config
        .duration("maintenance.interval")
        .map_err(|error| error.to_string())?
        .unwrap_or(defaults.interval);
    let ticks = |key: &str, fallback: u64| -> Result<u64, String> {
        Ok(config
            .integer(key)
            .map_err(|error| error.to_string())?
            .and_then(|value| u64::try_from(value).ok())
            .unwrap_or(fallback))
    };
    if interval.is_zero() {
        return Ok(None);
    }
    Ok(Some(sankhya_maintenance::MaintenancePolicy {
        interval,
        compact_every: ticks("maintenance.compact_every", defaults.compact_every)?,
        orphan_sweep_every: ticks("maintenance.orphan_sweep_every", defaults.orphan_sweep_every)?,
        ..defaults
    }))
}

/// The configuration files, lowest precedence first.
///
/// `SANKHYA_CONFIG` names an explicit file, which is what a deployment with several
/// instances on one machine needs. Otherwise the file beside the binary's working directory.
fn configuration_files() -> Vec<std::path::PathBuf> {
    std::env::var("SANKHYA_CONFIG").map_or_else(
        |_| vec![std::path::PathBuf::from("config/application.yaml")],
        |named| named.split(',').map(std::path::PathBuf::from).collect(),
    )
}

/// The documented `SANKHYA_*` variables, under the settings they configure.
///
/// Listed rather than derived. A rule that turns `SANKHYA_FOO_BAR` into `foo.bar` would also
/// turn every unrelated variable into a setting, and a deployment's environment holds a great
/// many unrelated variables.
fn legacy_environment() -> BTreeMap<String, String> {
    const MAPPED: &[(&str, &str)] = &[
        ("SANKHYA_LISTEN", "server.listen"),
        ("SANKHYA_METRICS_LISTEN", "server.metrics_listen"),
        ("SANKHYA_WAREHOUSE", "warehouse.path"),
        ("SANKHYA_READ_AS_OF", "warehouse.read_as_of"),
        ("SANKHYA_DATA_DIR", "data.dir"),
    ];
    let mut out = BTreeMap::new();
    for (variable, setting) in MAPPED {
        if let Ok(value) = std::env::var(variable) {
            out.insert((*setting).to_string(), value);
        }
    }
    // Spelled as an opt-out, so the insecure choice is deliberate: the variable's presence
    // is the signal, whatever it holds.
    if std::env::var("SANKHYA_NO_PASSWORD").is_ok() {
        out.insert("server.require_password".to_string(), "false".to_string());
    }
    out
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
    // Installing the allocator is a `#[global_allocator]` attribute and reaches nothing;
    // announcing it is what lets the metrics endpoint read the counters without naming a
    // static that only this binary has. Done first, before anything can be scraped.
    sankhya_alloc::announce(&ALLOCATOR);

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // A configuration that does not load stops the server here, with the reason, rather
    // than at whatever the missing setting was for. `sankhya-config` refuses a malformed
    // file, an unresolved reference and an unparseable value; each of those is a deployment
    // that would otherwise come up behaving as though it were configured.
    let settings = match settings() {
        Ok(settings) => settings,
        Err(why) => {
            eprintln!("sankhya: {why}");
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, why));
        }
    };
    let settings_flight = settings.flight_listen.clone();
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
        Some("attest") => std::process::exit(backup::run_attestation(
            std::env::args().nth(2).as_deref(),
            &data,
            now_micros(),
        )),
        _ => {}
    }

    let configured_maintenance = settings.maintenance.clone();
    let warehouse_root = settings.warehouse.clone();
    let (server, listener, complaints) = start(settings).await?;

    // The warehouse maintains itself from here, on its own thread, for as long as the server
    // runs. Held in a binding rather than dropped: dropping the handle stops the thread, and
    // `let _ = ...` would stop it immediately --- maintenance that runs for the length of one
    // statement is worse than none, because the log would say it started.
    let maintenance = configured_maintenance.map(|policy| {
        let tables = sankhya_maintenance::tables_under(&warehouse_root);
        println!(
            "  maintaining {} table(s) every {:?}, compacting every {} tick(s), sweeping every {}",
            tables.len(),
            policy.interval,
            policy.compact_every,
            policy.orphan_sweep_every
        );
        // The *same* registry the query path pins. Building a second one here would leave the
        // sweeper watching a registry nobody announces into.
        std::sync::Arc::new(sankhya_maintenance::spawn_maintenance_watching(
            tables,
            policy,
            Some(server.leases()),
        ))
    });

    // Arrow Flight SQL, served on its own listener.
    //
    // The protocol has been complete and tested since M6 and **nothing served it** ---
    // `GUIDE.md` §7a documented a bulk plane with nowhere to send a `GetFlightInfo`. This is
    // the line that made the difference, and it is worth how little it is: the surface was
    // built, the transport was not, and no test could tell because every test of the protocol
    // constructed the service directly.
    if let Some(address) = settings_flight {
        match address.parse::<std::net::SocketAddr>() {
            Ok(socket) => {
                let flying = flight::Flying::new(Arc::clone(&server));
                println!("  Arrow Flight SQL on {socket}");
                tokio::spawn(async move {
                    let transport = sankhya_api_grpc::Transport::new(socket);
                    let service = sankhya_api_flight::SankhyaFlight::new(std::sync::Arc::new(
                        flying,
                    ));
                    // Served until the process ends. A bulk plane that stopped on its own
                    // would be indistinguishable, to a client, from one that was never there.
                    if let Err(error) = transport
                        .serve_until(service, std::future::pending::<()>())
                        .await
                    {
                        eprintln!("  Arrow Flight SQL stopped: {error}");
                    }
                });
            }
            Err(error) => {
                eprintln!("  server.flight_listen is not an address: {address}: {error}");
            }
        }
    }

    // Maintained cubes, built on the same cadence and for the same reason.
    //
    // A cube marked maintained is maintained whether or not whoever declared it is logged in
    // --- a dashboard is fast at nine because something built its cells at four. The refresher
    // has no principal, so it builds the *unrestricted* cuboid, which per ADR-0008 may serve
    // only an unrestricted caller: this helps dashboards and service accounts and does
    // nothing for a restricted analyst, whose cuboids are built by their own queries.
    //
    // Gated on maintenance being enabled, because it is maintenance: an operator who turned
    // the thread off did not ask for a different background writer to keep going.
    if let Some(every) = maintenance.as_ref().map(|handle| handle.policy().interval) {
        let cubes = Arc::clone(&server);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(every).await;
                let built = tokio::task::block_in_place(|| cubes.refresh_maintained_cubes());
                if !built.is_empty() {
                    println!("  materialised {} cuboid(s): {}", built.len(), built.join(", "));
                }
            }
        });
    }

    // Reconfiguration without a restart.
    //
    // An operator who has to restart the server to slow compaction down will not slow
    // compaction down. They will wait for a maintenance window --- and the window is when
    // the system is already busy, which is exactly when the setting needed changing.
    //
    // SIGHUP is the conventional signal for it and this process already handles SIGTERM, so
    // an operator has one habit rather than two. The configuration is read again from the
    // same files in the same precedence order, so a reloaded value cannot mean something a
    // booted one would not.
    if let Some(handle) = maintenance.as_ref().map(std::sync::Arc::clone) {
        tokio::spawn(async move {
            let Ok(mut hangup) =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
            else {
                eprintln!("  cannot listen for SIGHUP; maintenance settings need a restart");
                return;
            };
            while hangup.recv().await.is_some() {
                let reloaded = sankhya_config::Configuration::load_with(
                    &configuration_files(),
                    &legacy_environment(),
                    &BTreeMap::new(),
                )
                .map_err(|error| error.to_string())
                .and_then(|config| maintenance_policy(&config));

                match reloaded {
                    // Refused rather than half-applied. A reload that took the readable
                    // settings and left the rest is a configuration nobody wrote.
                    Err(why) => eprintln!("  reload refused, keeping the running settings: {why}"),
                    // Disabling maintenance on a reload is not honoured by stopping the
                    // thread: stopping is not reversible without a restart, which is the
                    // thing this exists to avoid. Said plainly rather than ignored.
                    Ok(None) => eprintln!(
                        "  reload asks to disable maintenance, which needs a restart; the \
                         thread keeps running under the settings it has"
                    ),
                    Ok(Some(policy)) => {
                        let was = handle.policy();
                        handle.reconfigure(policy.clone());
                        if was.interval == policy.interval
                            && was.compact_every == policy.compact_every
                            && was.orphan_sweep_every == policy.orphan_sweep_every
                        {
                            println!("  reloaded; maintenance settings are unchanged");
                        } else {
                            println!(
                                "  maintenance reloaded: every {:?} (was {:?}), compacting \
                                 every {} tick(s) (was {}), sweeping every {} (was {})",
                                policy.interval,
                                was.interval,
                                policy.compact_every,
                                was.compact_every,
                                policy.orphan_sweep_every,
                                was.orphan_sweep_every
                            );
                        }
                    }
                }
            }
        });
    }

    // Printed rather than only logged: an operator starting this by hand needs to see the
    // configuration, and an insecure one is written so it looks wrong.
    // The address actually bound, not the one configured. Told to bind port 0 the
    // configured value is literally ":0", so the line that exists to tell an operator where
    // to connect told them nothing — and a test wanting an ephemeral port had no way to
    // learn which one it got.
    let bound = listener
        .local_addr()
        .map_or_else(|_| settings_listen.clone(), |address| address.to_string());

    println!("SANKHYA {}", env!("CARGO_PKG_VERSION"));
    println!("  {}", server.describe());
    println!("  listening on {bound}");
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
    // Bound, because `cubes()` now returns a snapshot rather than a borrow --- cube DDL can
    // change the set between statements, so there is nothing stable to borrow from.
    let cubes = server.cubes();
    if !cubes.is_empty() {
        let named: Vec<&str> = cubes.iter().map(sankhya_cube::model::Cube::name).collect();
        println!("  {} cube(s): {}", named.len(), named.join(", "));
    }
    if server.table_count() == 0 {
        println!("  no tables found — set SANKHYA_WAREHOUSE to a directory of <schema>/<table>/");
    }
    // Built from the address actually bound. It was a literal `-p 5433`, which is right
    // until somebody sets `SANKHYA_LISTEN` and then is a printed instruction that does not
    // work, in the one line an operator copies.
    let (host, port) = bound.rsplit_once(':').unwrap_or(("127.0.0.1", "5433"));
    println!("  connect with: psql -h {host} -p {port} -U <user>");

    // Bound before the wire listener starts serving, so that a scrape arriving immediately
    // after startup finds the endpoint rather than a refused connection. A failure to bind
    // it is reported and is not fatal: losing metrics is worse than losing nothing and much
    // better than refusing to serve queries.
    let (metrics_shutdown, metrics_signal) = tokio::sync::oneshot::channel::<()>();
    let metrics_task = match &settings_metrics {
        Some(address) => match tokio::net::TcpListener::bind(address).await {
            Ok(listener) => {
                // The bound address again, for the same reason: `address` may name port 0.
                let on = listener
                    .local_addr()
                    .map_or_else(|_| address.clone(), |bound| bound.to_string());
                println!("  metrics on http://{on}/metrics");
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

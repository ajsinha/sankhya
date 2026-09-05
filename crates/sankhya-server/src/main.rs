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
mod adopt;
mod aggregations;
mod clones;
mod cubes;
mod driver;
mod feeds;
mod snapshots;
mod flight;
mod scrape;
mod warehouse;
mod wiring;

use std::collections::BTreeMap;
use sankhya_authz::principal::TenantId;
use std::sync::Arc;
use wiring::{start, Posture, Settings, TransportSecurity, CUBOID_ROW_BUDGET};

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
/// What the binary prints when asked, and when told something it does not understand.
///
/// Written out rather than generated, because the environment variables below are otherwise
/// documented in exactly one place --- a book chapter nothing links to --- and the first
/// thing a stranger types when a binary refuses is `--help`.
const USAGE: &str = "\
SANKHYA --- an analytical warehouse that refuses rather than guesses.

USAGE:
    sankhya-server [start]     serve; the default when no subcommand is given
    sankhya-server doctor      report on the warehouse and exit non-zero on a finding
    sankhya-server backup      take a backup
    sankhya-server drill       restore a backup and prove it reads
    sankhya-server attest <store>
                               attest a declared backup store
    sankhya-server hash-password
                               read a password and print a verifier for
                               server.credentials.<user>
    sankhya-server --help | --version

ENVIRONMENT:
    SANKHYA_CONFIG             configuration file(s), comma-separated
                               (default: config/application.yaml, relative to the working
                               directory --- a unit with no WorkingDirectory= gets none)
    SANKHYA_WAREHOUSE          warehouse.path
    SANKHYA_LISTEN             server.listen
    SANKHYA_METRICS_LISTEN     server.metrics_listen
    SANKHYA_READ_AS_OF         warehouse.read_as_of
    SANKHYA_DATA_DIR           data.dir
    SANKHYA_NO_PASSWORD        serve with no authentication; presence is the signal
    SANKHYA_USER_FUNCTIONS     accept CREATE AGGREGATION, which runs supplied code
                               (default: false)

Configuration is refused rather than defaulted: a setting that silently becomes something
else is a deployment behaving as though it were configured when it is not.";

fn settings() -> Result<Settings, String> {
    let files = configuration_files();
    // A file the operator *named* and that is not there stops the server.
    //
    // A missing file is otherwise skipped, which is right for the default path --- a server
    // with no configuration file is a server run from a shell, and it works. It is wrong for
    // `SANKHYA_CONFIG`: naming a file is a statement that the file is the configuration, so
    // skipping it silently produces a server with no users, no roles, no policy, no TLS and
    // no feeds that believes it is configured. The shipped systemd unit named no file at all
    // and inherited `/` as its working directory, which is exactly that server.
    if std::env::var("SANKHYA_CONFIG").is_ok() {
        for file in &files {
            if !file.exists() {
                return Err(format!(
                    "the configuration file `{}` named by SANKHYA_CONFIG does not exist.                      Refused rather than skipped: a named file that is not read is a server                      with no users, no roles and no policy that believes it is configured",
                    file.display()
                ));
            }
        }
    }
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
    // Off unless an operator says otherwise. `CREATE AGGREGATION` runs code the caller
    // supplied, and until the per-principal grant `ADR-0023` Decision 4 describes exists,
    // the only honest default is a closed door.
    let user_functions = config
        .boolean("server.user_functions")
        .map_err(|error| error.to_string())?
        .unwrap_or(false);
    let warehouse: std::path::PathBuf = config.get_or("warehouse.path", "./warehouse").into();
    // Absent means everything published, which is what a server running no ingest wants.
    // A position that will not convert is refused rather than defaulted: falling back to
    // `u64::MAX` would read "as of -5" as "read everything", which is the silent
    // reinterpretation every other setting here refuses by name.
    let read_as_of = sankhya_types::Lsn::new(
        match config
            .integer("warehouse.read_as_of")
            .map_err(|error| error.to_string())?
        {
            None => u64::MAX,
            Some(value) => u64::try_from(value).map_err(|_| {
                format!(
                    "`warehouse.read_as_of` must be a position at or after zero and holds \
                     `{value}`. Leave it unset to read everything published"
                )
            })?,
        },
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
    // `server.users.<name>: role, role` --- the roles each user holds.
    //
    // Read as a section rather than as a list of known names, because the names are the
    // operator's and this file has never seen them. Roles are comma-separated for the same
    // reason every other list in this configuration is: a YAML sequence and a scalar are
    // different shapes to read, and one shape is fewer.
    let roles: std::collections::BTreeMap<String, Vec<String>> = config
        .section("server.users")
        .into_iter()
        .map(|(user, named)| {
            let held: Vec<String> = named
                .split(',')
                .map(|role| role.trim().to_owned())
                .filter(|role| !role.is_empty())
                .collect();
            (user, held)
        })
        .collect();
    // `server.credentials.<name>: <verifier>` --- what each user's password is checked against.
    //
    // A separate section from `server.users`, which holds roles. One section holding two kinds
    // of thing would mean a typo in a role name silently becoming a credential, or the reverse.
    //
    // Read the same way and for the same reason: the names are the operator's. A malformed
    // verifier is refused **here**, at startup, with the line named --- not at the first login
    // attempt, where the operator is not looking and the client is told only that its password
    // was wrong.
    let mut credentials: std::collections::BTreeMap<String, sankhya_credential::Verifier> =
        std::collections::BTreeMap::new();
    for (user, stored) in config.section("server.credentials") {
        match sankhya_credential::Verifier::parse(&stored) {
            Ok(verifier) => {
                credentials.insert(user, verifier);
            }
            Err(why) => {
                return Err(format!(
                    "`server.credentials.{user}` is not a password verifier: {why}. Make one \
                     with `sankhya-server hash-password`"
                ))
            }
        }
    }
    let transport_security = transport_security(&config)?;
    Ok(Settings {
        roles,
        credentials,
        maintenance,
        transport_security,
        cuboid_budget_rows,
        listen,
        warehouse,
        read_as_of,
        tenant,
        require_password,
        user_functions,
        flight_listen,
        metrics_listen,
    })
}


/// What a configuration says about encryption, or `None` if it says nothing.
///
/// # Half a configuration is refused rather than completed
///
/// A certificate with no key is not a server that is nearly encrypted. It is a server whose
/// operator believes it is encrypted, and starting in the clear there is the most expensive
/// default this file could offer: everything works, nothing complains, and the mistake is
/// discovered by somebody else.
///
/// So the pair is read as one thing, and either half without the other stops startup, naming
/// the setting that is missing.
fn transport_security(
    config: &sankhya_config::Configuration,
) -> Result<Option<TransportSecurity>, String> {
    let certificate = config.get("server.tls.certificate").map(str::to_owned);
    let private_key = config.get("server.tls.private_key").map(str::to_owned);
    match (certificate, private_key) {
        (None, None) => Ok(None),
        (present, absent) => {
            if present.is_none() || absent.is_none() {
                let (set, missing) = if present.is_some() {
                    ("server.tls.certificate", "server.tls.private_key")
                } else {
                    ("server.tls.private_key", "server.tls.certificate")
                };
                return Err(format!(
                    "{set} is set and {missing} is not. Refused rather than started in the \
                     clear: a half-configured door is not a server that is nearly encrypted, \
                     it is a server whose operator believes it is encrypted"
                ));
            }
            let (certificate, private_key) = (
                present.unwrap_or_default(),
                absent.unwrap_or_default(),
            );
            Ok(Some(TransportSecurity {
                certificate: certificate.into(),
                private_key: private_key.into(),
                client_ca: config.get("server.tls.client_ca").map(Into::into),
                // Requiring by default. An operator who has gone to the trouble of
                // configuring a certificate did not do it so that a client could decline to
                // use it, and the permissive setting is the one to ask for by name.
                require: config
                    .boolean("server.tls.require")
                    .map_err(|error| error.to_string())?
                    .unwrap_or(true),
            }))
        }
    }
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

/// How often each declared feed looks at its spool directory, in seconds.
///
/// # Why this is configurable and why it is bounded below
///
/// A spool is somebody else's schedule. One that lands a file a minute is served badly by a
/// half-hourly scan, and one that lands a file a day is served badly by scanning it every
/// thirty seconds --- the cost of a scan is paid whether or not anything arrived. So the
/// cadence belongs to the deployment.
///
/// **Zero is not accepted**, because a zero cadence is not "as fast as possible": it is a
/// loop with no sleep in it, which is a busy wait that takes a core and starves the tasks it
/// competes with. Zero and an unparseable value both fall back to the default and **say so on
/// the way past**. Not silently, because an operator who set it believes it took effect; and
/// not by refusing to start, because that takes an outage on every table over one knob --- the
/// same reasoning that keeps one malformed feed declaration from stopping the server.
fn feed_interval_seconds() -> u64 {
    /// What a deployment gets by not thinking about it: often enough that a file is picked
    /// up while somebody is still watching for it, rarely enough to cost nothing.
    const DEFAULT: u64 = 30;

    match std::env::var("SANKHYA_FEED_INTERVAL_SECONDS") {
        Err(_) => DEFAULT,
        Ok(value) => match value.trim().parse::<u64>() {
            Ok(0) | Err(_) => {
                eprintln!(
                    "  SANKHYA_FEED_INTERVAL_SECONDS is `{value}`, which is not a number of \
                     seconds greater than zero — using {DEFAULT}"
                );
                DEFAULT
            }
            Ok(seconds) => seconds,
        },
    }
}

/// The configuration files, lowest precedence first.
///
/// `SANKHYA_CONFIG` names an explicit file, which is what a deployment with several
/// instances on one machine needs. Otherwise the file beside the binary's working directory.
/// The directory configuration is read from, which is where `feeds/` sits beside it.
///
/// Derived from the first configuration file rather than configured separately: two settings
/// that must agree are two settings that will one day not.
fn configuration_dir() -> std::path::PathBuf {
    configuration_files()
        .first()
        .and_then(|file| file.parent().map(std::path::Path::to_path_buf))
        .unwrap_or_else(|| std::path::PathBuf::from("config"))
}

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
        ("SANKHYA_USER_FUNCTIONS", "server.user_functions"),
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

/// Read a password from standard input and print a verifier for `server.credentials.<user>`.
///
/// # Why this exists at all
///
/// Because without it the credential store is unusable. `SEC-01` is not closed by a server that
/// *can* verify a password if somebody hand-derives PBKDF2 with the right parameters; it is
/// closed by an operator being able to produce a line and paste it into a file.
///
/// # Read from standard input rather than taken as an argument
///
/// A password on a command line is in the shell history, in `ps` output for every user on the
/// machine, and in whatever collects process telemetry. None of those are places a credential
/// stops being a credential.
///
/// # Errors
///
/// If the password cannot be read, or the system cannot supply randomness for a salt — which is
/// a machine that must not be used to make a credential rather than one that should get a
/// predictable one.
fn hash_password() -> std::io::Result<()> {
    use std::io::BufRead;

    eprintln!(
        "Type the password and press enter. It is read from standard input rather than taken \
         as an argument, because an argument is in your shell history and in `ps`."
    );
    let mut password = String::new();
    std::io::stdin().lock().read_line(&mut password)?;
    // The trailing newline only. A password may legitimately begin or end with a space, and
    // trimming it here would produce a verifier for a password nobody can type again.
    let password = password.strip_suffix('\n').unwrap_or(&password);
    let password = password.strip_suffix('\r').unwrap_or(password);
    if password.is_empty() {
        eprintln!("sankhya: an empty password is not one");
        std::process::exit(2);
    }

    let Some(salt) = sankhya_credential::fresh_salt() else {
        eprintln!(
            "sankhya: this machine could not supply randomness for a salt, so it must not be \
             used to make a credential"
        );
        std::process::exit(1);
    };
    let Some(verifier) =
        sankhya_credential::make(password.as_bytes(), &salt, sankhya_credential::ITERATIONS)
    else {
        eprintln!("sankhya: the verifier could not be derived");
        std::process::exit(1);
    };

    // The line, and only the line, on standard output --- so it can be redirected into a file
    // or piped without the explanation coming with it.
    println!("{verifier}");
    eprintln!(
        "\nPut it in your configuration under `server.credentials`:\n\n  \
         server:\n    credentials:\n      <user>: {verifier}\n"
    );
    Ok(())
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

/// Today, as days since the epoch.
///
/// Whole days, so expiry is decided on the same boundary the partitions are written on.
/// Anything finer would make a partition's fate depend on the time of day a tick happened to
/// run, which is a thing nobody chose.
fn today() -> i32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| i32::try_from(since.as_secs() / 86_400).unwrap_or(0))
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

    // What the binary is, before the configuration is consulted. Asking a program its
    // version must not depend on a file being well formed --- when the shipped
    // `config/application.yaml` held an unparseable position, the refusal reached `--help`
    // too, so the one command a stranger types to get unstuck was the one that could not
    // answer.
    match std::env::args().nth(1).as_deref() {
        Some("--help" | "-h" | "help") => {
            println!("{USAGE}");
            return Ok(());
        }
        Some("--version" | "-V" | "version") => {
            println!("SANKHYA {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        // Answered here, beside `--help`, because it reads no configuration. An operator whose
        // configuration is broken is exactly the operator who may need to write a credential
        // into it, and a subcommand that refused until the file parsed would be useless at the
        // one moment it is wanted --- which is the failure `RUN-06` recorded for `--help`.
        Some("hash-password") => return hash_password(),
        _ => {}
    }

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
    let settings_warehouse = settings.warehouse.clone();
    // How often a feed looks at its spool directory. Its own cadence rather than
    // maintenance's: an operator who turned maintenance off did not ask for ingest to stop.
    let feed_interval_seconds = feed_interval_seconds();
    let settings_metrics = settings.metrics_listen.clone();
    let settings_listen = settings.listen.clone();

    // Subcommands before the server starts, because `doctor` must work when `start` would
    // not. One argument is the whole surface for now; more of them want a parser, and a
    // hand-rolled parser is how a flag comes to mean two things.
    //
    // Every arm is named, and the unnamed ones are refused. The fall-through this replaced
    // served on any unrecognised argument, so `--help` bound the configured listeners and
    // ran until killed --- a typo starting a second server on one warehouse, which is the
    // two-committers race §15.3 names.
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
        // Serving is what no argument means, and `start` says it out loud. `--help` and
        // `--version` returned above, before the configuration was read.
        None | Some("start" | "--help" | "-h" | "help" | "--version" | "-V" | "version") => {}
        Some(unrecognised) => {
            eprintln!("sankhya: `{unrecognised}` is not a subcommand of this binary\n\n{USAGE}");
            std::process::exit(2)
        }
    }

    // One server per warehouse, decided here, before anything is opened.
    //
    // # What the commit protocol does not cover
    //
    // `atomicfs::claim` serialises two committers *at a version* and that is the whole of its
    // scope. Maintenance lives outside it: two servers each plan compactions against a live
    // set the other is changing, each retire merge inputs against a lease registry that cannot
    // see the other's readers, and each sweep orphans against an age threshold that has no
    // idea a file belongs to a commit the other has not written yet. Nothing there races at a
    // version, so nothing there was caught --- `COR-15`, and the two name-reuse defects beside
    // it are what leaked through the gap.
    //
    // In the data directory rather than the warehouse root, because the warehouse root holds
    // published tables and refuses a foreign object at startup. A lock file there would be the
    // foreign object.
    if let Err(why) = std::fs::create_dir_all(&data) {
        eprintln!("sankhya: the data directory {} could not be created: {why}", data.display());
        return Err(why);
    }
    let held = match sankhya_atomicfs::WarehouseLock::take(&data.join("warehouse.lock")) {
        Ok(lock) => lock,
        Err(why) => {
            eprintln!("sankhya: {why}");
            std::process::exit(3);
        }
    };
    // Bound for the length of `main`, not dropped. Dropping it here would release the lock
    // immediately and leave the warehouse unprotected for the whole of the run, which is worse
    // than never having taken it: the log would say it was locked.
    let _warehouse_lock = held;

    let configured_maintenance = settings.maintenance.clone();
    // The cadence the pin refresh runs on, taken before the policy is moved into the sweeper.
    let configured_interval = configured_maintenance
        .as_ref()
        .map_or_else(|| std::time::Duration::from_secs(30), |policy| policy.interval);
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
        // What still reads each table, refreshed on its own cadence below.
        //
        // The sweeper was told **nothing**: `Maintainer::among` existed and only a soak test
        // called it, so the maintenance thread ran with an empty lineage set --- and an empty
        // set pins nothing, which means a clone's files were reclaimable by the sweeper of the
        // table they belong to. Snapshots would have arrived into the same hole.
        let reading = std::sync::Arc::new(std::sync::Mutex::new(
            sankhya_maintenance::StillReading::default(),
        ));
        // The *same* registry the query path pins. Building a second one here would leave the
        // sweeper watching a registry nobody announces into.
        let handle = std::sync::Arc::new(sankhya_maintenance::spawn_maintenance_watching_pins(
            tables,
            policy,
            Some(server.leases()),
            std::sync::Arc::clone(&reading),
        ));
        (handle, reading)
    });
    let (maintenance, reading) = match maintenance {
        Some((handle, reading)) => (Some(handle), Some(reading)),
        None => (None, None),
    };

    // Refresh what still reads each table, so the sweeper honours a clone made or a snapshot
    // taken while this server runs rather than one made before it started.
    //
    // On the maintenance cadence, because that is the only consumer: refreshing faster would be
    // work nobody reads, and refreshing slower would leave a window in which a freshly taken
    // snapshot protects nothing.
    if let Some(reading) = reading.clone() {
        let refreshing = Arc::clone(&server);
        let every = configured_interval;
        tokio::spawn(async move {
            loop {
                let current = tokio::task::block_in_place(|| crate::snapshots::still_reading(&refreshing));
                // Poison is recovered from rather than skipped.
                //
                // `if let Ok(..)` here meant that one panic while this lock was held stopped
                // the refresh **for ever**, silently, and the sweeper went on reclaiming
                // against whatever pin set was current at the moment of the panic --- so a
                // snapshot taken afterwards protected nothing. The maintenance side of this
                // same lock already recovers; only the writing side did not.
                //
                // What poison means is that some thread panicked while holding this lock. The
                // value under it is a whole `StillReading` written by a single assignment, so
                // there is no half-updated state to inherit, and the assignment below replaces
                // it outright.
                //
                // The braces are load-bearing: a `MutexGuard` alive across the `await` below
                // makes this future `!Send`, and `tokio::spawn` requires `Send`. The block
                // ends the guard's scope where the assignment ends, which is also where it
                // ought to end.
                {
                    let mut held = reading
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    *held = current;
                }
                tokio::time::sleep(every).await;
            }
        });
    }

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
                let acceptor = server.columnar_acceptor();
                println!(
                    "  Arrow Flight SQL on {socket}{}",
                    if acceptor.is_some() { " (TLS)" } else { "" }
                );
                tokio::spawn(async move {
                    let transport = sankhya_api_grpc::Transport::new(socket);
                    // The same certificate the wire door presents, loaded once at startup.
                    let transport = match acceptor {
                        None => transport,
                        Some(acceptor) => transport.encrypted(acceptor),
                    };
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

    // Declared feeds, run on their own cadence.
    //
    // Not on the maintenance thread. Maintenance is work the warehouse does to itself and an
    // operator turns it off when another process is doing it; a feed is somebody's data
    // arriving, and stopping ingest because compaction was disabled would be a surprise
    // nobody asked for.
    //
    // A feed that **stops** stays stopped. `ADR-0018` is explicit that a run of records which
    // do not fit means a source has changed shape, and that retrying on a timer rediscovers
    // the same outage every few minutes and is acted on by nobody. So the task drops it and
    // says so once.
    let (declared, feed_complaints) = feeds::load(&configuration_dir());
    for complaint in &feed_complaints {
        eprintln!("  feed not loaded — {complaint}");
    }
    if !declared.is_empty() {
        let cadence = std::time::Duration::from_secs(feed_interval_seconds);
        let warehouse = settings_warehouse.clone();
        // Declared up front, so a feed that has never managed to run is still visible to
        // `SHOW FEEDS` --- which is the case an operator most needs to see.
        let standing = server.feeds();
        let mut targets = std::collections::BTreeMap::new();
        for feed in &declared {
            standing.declare(feed.feed.name());
            // What each one writes into, so `RESUME FEED` can be authorized against the same
            // table a query would be. Recorded here because this is the only place that knows
            // both the feed's name and its declaration.
            let declaration = feed.feed.declaration();
            targets.insert(
                feed.feed.name().to_owned(),
                sankhya_authz::policy::TableRef::new(&declaration.schema, &declaration.table),
            );
        }
        feeds::declare_targets(&server, targets);
        println!(
            "  {} feed(s) declared: {}",
            declared.len(),
            declared
                .iter()
                .map(|feed| feed.feed.name().to_owned())
                .collect::<Vec<_>>()
                .join(", ")
        );
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(cadence).await;

                // Quarantine expiry, on the same tick as the feeds themselves. `ADR-0018`
                // makes a retention mandatory precisely so that this exists: a quarantine
                // that only grows holds exactly the records nobody looked at, and nobody is
                // responsible for it.
                match tokio::task::block_in_place(|| {
                    feeds::expire_quarantine(&declared, &warehouse, today(), now_micros())
                }) {
                    None => {}
                    Some(Ok(said)) => println!("  quarantine expired {said}"),
                    Some(Err(why)) => eprintln!("  quarantine could not be expired — {why}"),
                }

                for feed in &declared {
                    let name = feed.feed.name().to_owned();
                    // Asked of the registry rather than a local set, so `RESUME FEED` takes
                    // effect on the next tick without this task knowing the command exists.
                    if !standing.should_run(&name) {
                        continue;
                    }
                    let warehouse = warehouse.clone();
                    let ran = tokio::task::block_in_place(|| feeds::run_once(feed, &warehouse));
                    match ran {
                        Err(refusal) => {
                            // Loud and once. A feed that cannot run at all is a
                            // configuration problem, and repeating it every cadence buries
                            // everything else in the log. The registry keeps it after the
                            // line has scrolled away.
                            eprintln!("  feed `{name}` refused — {refusal}");
                            standing.halted(&name, &refusal, now_micros());
                        }
                        Ok(result) => {
                            standing.ran(
                                &name,
                                result.published,
                                result.quarantined,
                                result.already_read,
                                now_micros(),
                            );
                            if result.published > 0 || result.quarantined > 0 {
                                println!(
                                    "  feed `{name}`: {} published, {} quarantined, {} \
                                     source(s)",
                                    result.published, result.quarantined, result.sources
                                );
                            }
                            if let Some(reason) = result.stopped {
                                eprintln!(
                                    "  feed `{name}` STOPPED — {reason}. It will not run \
                                     again until somebody says `RESUME FEED {name}`"
                                );
                                standing.halted(&name, &reason.to_string(), now_micros());
                            }
                        }
                    }
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
    // Said out loud, both ways. A server that is in the clear and does not mention it is how
    // an operator comes to believe their passwords are encrypted, and the belief survives
    // until somebody captures a packet.
    println!(
        "  wire protocol {}",
        match server.transport_posture() {
            Posture::Clear => "unencrypted — passwords cross the network in plain text",
            Posture::Offered => "TLS offered; a client that does not ask is still served",
            Posture::Required => "TLS required",
            Posture::Mutual => "TLS required, and a client certificate with it",
        }
    );
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

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::transport_security;
    use std::collections::BTreeMap;

    /// A configuration built from arguments alone, which is the highest precedence there is.
    fn configured(pairs: &[(&str, &str)]) -> sankhya_config::Configuration {
        let arguments: BTreeMap<String, String> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        sankhya_config::Configuration::load_with(
            &[] as &[std::path::PathBuf],
            &BTreeMap::new(),
            &arguments,
        )
        .expect("a configuration from arguments")
    }

    /// Saying nothing about TLS is how every existing deployment is configured.
    #[test]
    fn a_configuration_that_says_nothing_about_tls_asks_for_none() {
        assert_eq!(transport_security(&configured(&[])), Ok(None));
    }

    /// A certificate with no key stops startup.
    ///
    /// The whole reason this function exists rather than two independent settings. Starting
    /// in the clear here would be the most expensive default available: everything works,
    /// nothing complains, and the operator believes their passwords are encrypted.
    #[test]
    fn a_certificate_without_its_key_refuses_to_start() {
        let refused = transport_security(&configured(&[("server.tls.certificate", "/x.crt")]))
            .expect_err("half a configuration is not a configuration");
        assert!(refused.contains("server.tls.private_key"), "{refused}");
        assert!(refused.contains("believes it is encrypted"), "{refused}");
    }

    /// And a key with no certificate, which is the same mistake made the other way round.
    #[test]
    fn a_key_without_its_certificate_refuses_to_start() {
        let refused = transport_security(&configured(&[("server.tls.private_key", "/x.key")]))
            .expect_err("half a configuration is not a configuration");
        assert!(refused.contains("server.tls.certificate"), "{refused}");
    }

    /// Both halves, and the default that has to be asked out of rather than into.
    #[test]
    fn a_complete_configuration_requires_tls_unless_told_otherwise() {
        let security = transport_security(&configured(&[
            ("server.tls.certificate", "/x.crt"),
            ("server.tls.private_key", "/x.key"),
        ]))
        .expect("a complete configuration")
        .expect("some security");
        assert!(security.require, "requiring is the default an operator gets by not deciding");
        assert_eq!(security.client_ca, None);

        let permissive = transport_security(&configured(&[
            ("server.tls.certificate", "/x.crt"),
            ("server.tls.private_key", "/x.key"),
            ("server.tls.require", "false"),
        ]))
        .expect("a complete configuration")
        .expect("some security");
        assert!(!permissive.require, "and it can be asked out of, by name");
    }

    /// A client bundle is what makes both doors mutual.
    #[test]
    fn a_client_bundle_is_carried_through() {
        let security = transport_security(&configured(&[
            ("server.tls.certificate", "/x.crt"),
            ("server.tls.private_key", "/x.key"),
            ("server.tls.client_ca", "/clients.pem"),
        ]))
        .expect("a complete configuration")
        .expect("some security");
        assert_eq!(security.client_ca, Some(std::path::PathBuf::from("/clients.pem")));
    }
}

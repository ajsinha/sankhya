//! PostgreSQL as a supervised child process.
//!
//! # What "embedded PostgreSQL" actually means here
//!
//! `REQUIREMENTS.md` DEC-02 settles a claim that would otherwise have produced a specification
//! nobody could implement: PostgreSQL is not linked into this binary and does not run inside
//! this process. It is a **child process whose entire lifecycle SANKHYA owns** --- `initdb`,
//! start, readiness, health, shutdown --- so that to an operator there is one process tree, one
//! artifact, one configuration file, no external installation and no DBA action.
//!
//! That is a real differentiator and a defensible claim. "Embedded" in the literal sense is
//! neither.
//!
//! # Managed mode is single-node, deliberately
//!
//! A supervised child belongs to one host. Multi-node deployments use **attached** mode against
//! an externally managed, highly-available cluster --- `REQUIREMENTS.md` records that as a
//! product boundary rather than a gap, and M8's leader election runs against *that*, not this.
//!
//! Building the supervisor first is still the right order: it is what makes a single-binary
//! evaluation possible, it is testable on this machine, and it needs no client library at all.
//!
//! # Why this needs no dependencies
//!
//! Everything here drives the vendored PostgreSQL **binaries**: `initdb` creates the cluster,
//! `pg_ctl` starts and stops it, `pg_isready` answers whether it is accepting connections.
//! Those are the same programs an operator would run, which means this supervisor cannot drift
//! from what a person doing it by hand would get.
//!
//! Pooling and migrations --- the other half of this crate's remit --- need a PostgreSQL client
//! library, and adding one is a dependency decision this workspace makes deliberately rather
//! than by reaching for it. It is recorded as an open decision rather than made here.
//!
//! # The socket, and why there is no port
//!
//! The postmaster is started with `listen_addresses = ''`, so it accepts connections only over
//! a Unix domain socket inside the data directory. Nothing on the network can reach it, which
//! is the property that lets a managed cluster hold the system of record without an operator
//! having to reason about firewalls.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Where the PostgreSQL programs live.
///
/// Located rather than assumed: managed mode extracts them into the data directory on first
/// boot, and a development tree has them under `.build/pg-install/bin`. A supervisor that
/// hardcoded one of those would work in exactly one of the two situations it exists for.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Binaries(PathBuf);

impl Binaries {
    /// The programs at this directory, if the ones this crate needs are all present.
    ///
    /// All four, checked together. A directory with `initdb` and no `pg_ctl` fails later, in
    /// the middle of starting a cluster that has already been created --- which is a worse
    /// place to discover it than here.
    #[must_use]
    pub fn at(directory: impl Into<PathBuf>) -> Option<Self> {
        let directory = directory.into();
        let complete = ["initdb", "pg_ctl", "pg_isready", "postgres"]
            .iter()
            .all(|program| directory.join(program).is_file());
        complete.then_some(Self(directory))
    }

    /// The path to one program.
    #[must_use]
    pub fn program(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

/// What went wrong, and what an operator should do about it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ClusterError {
    /// A program could not be run at all.
    Unrunnable {
        /// Which program.
        program: String,
        /// The operating system's reason.
        detail: String,
    },
    /// A program ran and failed.
    Refused {
        /// Which program.
        program: String,
        /// What it wrote, which is where PostgreSQL puts the reason.
        detail: String,
    },
    /// The cluster did not begin accepting connections in time.
    NotReady {
        /// How many probes were made.
        probes: u32,
    },
}

impl std::fmt::Display for ClusterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unrunnable { program, detail } => write!(
                f,
                "could not run `{program}`: {detail}. The vendored PostgreSQL programs are \
                 missing or not executable, which is a packaging fault rather than a \
                 configuration one"
            ),
            Self::Refused { program, detail } => {
                write!(f, "`{program}` failed: {detail}")
            }
            Self::NotReady { probes } => write!(
                f,
                "the cluster did not accept connections after {probes} probe(s). Its log is in \
                 the data directory and says why; starting it by hand with the same data \
                 directory reproduces it"
            ),
        }
    }
}

impl std::error::Error for ClusterError {}

/// A PostgreSQL cluster this process owns.
#[derive(Debug)]
pub struct Cluster {
    binaries: Binaries,
    data: PathBuf,
    running: bool,
}

impl Cluster {
    /// A cluster at `data`, run with these programs.
    ///
    /// Nothing happens until [`Cluster::start`]. Constructing this is not a side effect,
    /// because a constructor that starts a database is one nobody can call to ask a question.
    #[must_use]
    pub fn new(binaries: Binaries, data: impl Into<PathBuf>) -> Self {
        Self {
            binaries,
            data: data.into(),
            running: false,
        }
    }

    /// Where the cluster's files are.
    #[must_use]
    pub fn data_directory(&self) -> &Path {
        &self.data
    }

    /// The Unix socket directory clients connect through.
    ///
    /// Inside the data directory, so it inherits its permissions and disappears with it. A
    /// socket in `/tmp` outlives the cluster and is reachable by anybody on the host.
    #[must_use]
    pub fn socket_directory(&self) -> &Path {
        &self.data
    }

    /// Whether this data directory already holds a cluster.
    #[must_use]
    pub fn exists(&self) -> bool {
        self.data.join("PG_VERSION").is_file()
    }

    /// Create the cluster if it is not there, and start it.
    ///
    /// Idempotent in the way a supervisor needs: a data directory that already holds a cluster
    /// is started rather than re-created, because `initdb` over live data would destroy the
    /// system of record.
    ///
    /// # Errors
    ///
    /// [`ClusterError`] naming the program that failed and what it wrote.
    pub fn start(&mut self, ready_within: std::time::Duration) -> Result<(), ClusterError> {
        if !self.exists() {
            self.initialise()?;
        }
        self.launch()?;
        self.await_ready(ready_within)?;
        self.running = true;
        Ok(())
    }

    /// Stop the cluster, waiting for it to finish.
    ///
    /// `fast` rather than `smart` or `immediate`: `smart` waits for every client to disconnect,
    /// which turns a shutdown into a hang whenever one has not; `immediate` skips the
    /// checkpoint and forces recovery on the next start. `fast` disconnects clients and shuts
    /// down cleanly, which is what a supervised child should do when its parent is going away.
    ///
    /// # Errors
    ///
    /// [`ClusterError`] if `pg_ctl` could not be run or refused.
    pub fn stop(&mut self) -> Result<(), ClusterError> {
        if !self.running {
            return Ok(());
        }
        self.run(
            "pg_ctl",
            &[
                "-D",
                &self.data.to_string_lossy(),
                "-m",
                "fast",
                "-w",
                "stop",
            ],
        )?;
        self.running = false;
        Ok(())
    }

    /// Whether the cluster is accepting connections right now.
    ///
    /// Asked of the cluster rather than remembered, because a supervised child can die without
    /// telling its parent and a supervisor that trusts its own bookkeeping reports a database
    /// as healthy while it is gone.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        Command::new(self.binaries.program("pg_isready"))
            .args(["-h", &self.socket_directory().to_string_lossy()])
            .output()
            .is_ok_and(|out| out.status.success())
    }

    fn initialise(&self) -> Result<(), ClusterError> {
        std::fs::create_dir_all(&self.data).map_err(|error| ClusterError::Unrunnable {
            program: "initdb".to_string(),
            detail: error.to_string(),
        })?;
        // `--auth=trust` is safe *because* of `listen_addresses = ''`: the only way to reach
        // this cluster is a socket inside a directory the operator already owns. Trust over a
        // TCP listener would be an open database; trust over a private socket is the same
        // boundary the data directory already has.
        self.run(
            "initdb",
            &[
                "-D",
                &self.data.to_string_lossy(),
                "-U",
                "sankhya",
                "--auth=trust",
                "-E",
                "UTF8",
                "--no-sync",
            ],
        )
    }

    fn launch(&self) -> Result<(), ClusterError> {
        // Nothing on the network. The postmaster listens on a Unix socket in the data
        // directory and nowhere else, which is what makes a managed cluster something an
        // operator does not have to firewall.
        let options = format!(
            "-c listen_addresses='' -c unix_socket_directories='{}'",
            self.socket_directory().display()
        );
        self.run(
            "pg_ctl",
            &[
                "-D",
                &self.data.to_string_lossy(),
                "-l",
                &self.data.join("postgres.log").to_string_lossy(),
                "-o",
                &options,
                "-w",
                "start",
            ],
        )
    }

    fn await_ready(&self, within: std::time::Duration) -> Result<(), ClusterError> {
        let deadline = std::time::Instant::now() + within;
        let mut probes = 0_u32;
        loop {
            probes = probes.saturating_add(1);
            if self.is_ready() {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err(ClusterError::NotReady { probes });
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    fn run(&self, program: &str, args: &[&str]) -> Result<(), ClusterError> {
        let output = Command::new(self.binaries.program(program))
            .args(args)
            .output()
            .map_err(|error| ClusterError::Unrunnable {
                program: program.to_string(),
                detail: error.to_string(),
            })?;
        if output.status.success() {
            return Ok(());
        }
        // Both streams: `initdb` explains itself on stdout and `pg_ctl` on stderr, and a
        // supervisor that reported only one would swallow half the reasons it can fail for.
        let detail = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stderr).trim(),
            String::from_utf8_lossy(&output.stdout).trim()
        );
        Err(ClusterError::Refused {
            program: program.to_string(),
            detail,
        })
    }
}

impl Drop for Cluster {
    /// Stop the child when its supervisor goes away.
    ///
    /// Without this, a process that panics or returns early leaves a postmaster running against
    /// a data directory nothing owns any more --- and the next start finds the lock file held
    /// by a process that is not its child, which is a confusing way to learn about a bug
    /// somewhere else entirely.
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

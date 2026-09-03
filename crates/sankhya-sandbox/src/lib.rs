//! The operating-system boundary a user-supplied function runs behind.
//!
//! # What this crate is, in one sentence
//!
//! It starts a process that **cannot reach the network, cannot see the filesystem beyond a tree
//! it was given, cannot gain a privilege, cannot run for longer than it was allowed, and cannot
//! take more memory than it was allowed** --- and it enforces each of those with the kernel
//! rather than with the language the code inside is written in.
//!
//! # Why the kernel and not the interpreter
//!
//! [ADR-0023](../../../docs/adr/0023-the-sandbox-a-user-function-runs-in.md) Decision 1. The
//! cheap alternative is to restrict Python from inside Python --- strip `__builtins__`, install
//! an audit hook, run the source through a rewriter --- and it is not a security boundary.
//! CPython's own maintainers say so, and every published attempt has been escaped through some
//! path back to the interpreter's internals. Choosing it would not be choosing a weak boundary;
//! it would be choosing a **decoration** that makes a reviewer believe there is one.
//!
//! # Why this crate may write `unsafe` when nothing else may
//!
//! Every mechanism here is a syscall made between `fork` and `exec`. `ADR-0023` Decision 8
//! chose one named crate over the two alternatives: delegating to whatever sandbox binary
//! happens to be installed on the machine, which makes the security property somebody else's;
//! and dropping the feature. `cargo xtask check-lints` fails the build if a second crate opts
//! out of the workspace's `forbid(unsafe_code)`.
//!
//! The crate is deliberately small and knows nothing about warehouses, Arrow or queries. It
//! starts a process. Everything above it is ordinary safe Rust talking to that process.
//!
//! # Why the probe runs the mechanism instead of reading a flag
//!
//! Unprivileged user namespaces are a kernel feature that several distributions ship disabled
//! and several operators turn off deliberately. Reading `/proc/sys/...` to guess would be
//! reading one of the three places the answer might live. [`probe`] *enters* a namespace, once,
//! and reports what happened --- so a machine that cannot host the boundary says so at startup
//! rather than at the first `CREATE FUNCTION` in production.

use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant};

mod jail;
mod limits;

pub use limits::Bounds;

/// Why this machine cannot host the boundary.
///
/// Each variant names the mechanism, because "sandboxing is unavailable" is not something an
/// operator can act on and "this kernel does not allow unprivileged user namespaces" is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unavailable {
    /// The mechanisms are Linux's, and this is not Linux.
    NotLinux,
    /// `unshare` refused. The message is the system's own.
    Refused {
        /// Which namespace or limit could not be applied.
        mechanism: &'static str,
        /// What the system said.
        said: String,
    },
}

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotLinux => write!(
                f,
                "a user-defined function needs an operating-system boundary to run behind, and \
                 the mechanisms that provide one here are Linux's. Refused rather than run \
                 without one: unsandboxed, a user function is arbitrary code with this \
                 server's identity and a principal's rows in hand"
            ),
            Self::Refused { mechanism, said } => write!(
                f,
                "a user-defined function needs an operating-system boundary to run behind, and \
                 this machine would not provide one: {mechanism} ({said}). Several \
                 distributions ship unprivileged user namespaces disabled and several \
                 operators turn them off deliberately, so this is a decision somebody may have \
                 made on purpose. Refused rather than run without the boundary"
            ),
        }
    }
}

impl std::error::Error for Unavailable {}

/// Proof that the boundary works **on this machine**, obtained by entering it.
///
/// Held rather than re-derived: a value of this type is the only way to reach [`Ready::run`],
/// so there is no path that starts a worker without having first shown the boundary exists.
#[derive(Debug, Clone)]
pub struct Ready {
    _sealed: (),
}

/// How a run ended.
#[derive(Debug)]
pub enum Outcome {
    /// It finished within every bound. The bytes are what it wrote.
    Answered(Vec<u8>),
    /// It was still running at the wall-clock deadline and was killed.
    ///
    /// A separate variant from the others because it is the one an operator sees during an
    /// incident, and "the function `x` did not finish within 5s" is a different sentence from
    /// "the function `x` failed".
    OutOfTime {
        /// The deadline it passed.
        after: Duration,
    },
    /// It wrote back more than it was allowed to.
    ///
    /// Only ever the output bound. Memory is **not** reported here, and that is deliberate:
    /// running out of address space reaches the program as a failed allocation, and the program
    /// says what it was doing at the time. Folding that into one message would replace the
    /// sentence a reader can act on with one they cannot.
    OutOfRoom {
        /// Which bound, in the words the message uses.
        bound: &'static str,
    },
    /// It ended by itself, unsuccessfully. Whatever it wrote to standard error is here.
    Failed {
        /// The exit status, absent when a signal ended it.
        code: Option<i32>,
        /// The signal that ended it, when one did.
        signal: Option<i32>,
        /// What it said.
        said: String,
    },
}

/// Whether the boundary can be built here, established by building one.
///
/// # Errors
///
/// [`Unavailable`] naming the mechanism that would not apply.
pub fn probe() -> Result<Ready, Unavailable> {
    if !cfg!(target_os = "linux") {
        return Err(Unavailable::NotLinux);
    }
    jail::probe()?;
    Ok(Ready { _sealed: () })
}

impl Ready {
    /// Run a program behind the boundary, feed it `input`, and collect what it writes.
    ///
    /// `readable` is the **whole** of the filesystem the program will see. Anything not under
    /// one of those paths does not exist as far as it is concerned --- not "is denied", does
    /// not exist --- which is what makes reading the warehouse directly impossible rather than
    /// merely forbidden.
    ///
    /// # Errors
    ///
    /// The process could not be started at all. A process that started and then failed, ran
    /// too long, or asked for too much is an [`Outcome`], not an error: those are answers about
    /// the function, and the caller has to report them naming it.
    pub fn run(
        &self,
        program: &Path,
        arguments: &[&str],
        readable: &[&Path],
        input: &[u8],
        bounds: &Bounds,
    ) -> std::io::Result<Outcome> {
        // Which half failed, said in the error. The two are very different things to be told:
        // a plan that cannot be built is a path this server got wrong, and a spawn that fails
        // is the boundary refusing to apply --- and a bare `EPERM` distinguishes neither.
        let plan = jail::Plan::new(readable, bounds).map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("the sandbox could not be laid out: {error}"),
            )
        })?;
        let mut child = jail::spawn(program, arguments, &plan).map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("the sandbox could not be entered: {error}"),
            )
        })?;

        // Written before anything is read, and the pipe is closed after, because a worker that
        // waits for end-of-input and a parent that waits for output is a deadlock with no
        // symptom but a hung query.
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            let _ = stdin.write_all(input);
        }

        let started = Instant::now();
        let mut said = Vec::new();
        let mut answered = Vec::new();
        let outcome = loop {
            match child.try_wait()? {
                Some(status) => {
                    if let Some(mut out) = child.stdout.take() {
                        let _ = out.read_to_end(&mut answered);
                    }
                    if let Some(mut err) = child.stderr.take() {
                        let _ = err.read_to_end(&mut said);
                    }
                    if answered.len() > bounds.output {
                        break Outcome::OutOfRoom { bound: "the output it may write" };
                    }
                    break if status.success() {
                        Outcome::Answered(answered)
                    } else {
                        // A signal is reported as a signal. An earlier version mapped "no exit
                        // code" to the memory bound, which is a guess: `SIGSEGV` and `SIGXCPU`
                        // arrive the same way and mean entirely different things, and a caller
                        // told "out of memory" about a segmentation fault looks in the wrong
                        // place for as long as it takes them to stop believing the message.
                        use std::os::unix::process::ExitStatusExt;
                        Outcome::Failed {
                            code: status.code(),
                            signal: status.signal(),
                            said: String::from_utf8_lossy(&said).trim().to_owned(),
                        }
                    };
                }
                None => {
                    if started.elapsed() >= bounds.wall {
                        // Killed, not asked. A function that never returns is an outage rather
                        // than an error, and a polite request to stop is a request something
                        // in an infinite loop never reads.
                        let _ = child.kill();
                        let _ = child.wait();
                        break Outcome::OutOfTime { after: bounds.wall };
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
        };
        Ok(outcome)
    }
}

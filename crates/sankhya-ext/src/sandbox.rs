//! Stopping pack code that will not stop itself.
//!
//! [`crate::function::Invocation::check`] is how a well-behaved function cooperates: an
//! atomic load between units of work, and the function returns. That is cheap, exact, and
//! sufficient for every pack anyone writes on purpose.
//!
//! It is not sufficient for the one that loops forever, and the M4 exit criterion is
//! specifically about that one: *a pack function in a deliberate infinite loop is stopped,
//! and the query returns an error naming the pack rather than hanging.* A pack that never
//! checks cannot be stopped by asking it to.
//!
//! # What this actually does, stated plainly
//!
//! The call runs on a separate thread. The caller waits for it up to the deadline. If the
//! deadline passes, the caller **abandons** the thread and returns an error naming the pack.
//!
//! The query is unblocked and the engine stays responsive. The thread is not killed --- Rust
//! has no safe way to kill a thread, and there is no unsafe code in this repository --- so a
//! genuinely non-terminating function leaks one thread until the process ends.
//!
//! That is a real cost, and it is the right choice. The alternatives are worse: hanging the
//! query forever, or killing a thread mid-allocation and corrupting the allocator for
//! everything else. A leaked thread is bounded, observable and attributable to a named pack,
//! which makes it an operational problem with an obvious fix rather than an outage.
//!
//! [`Sandbox::abandoned`] counts them, so a pack that does this repeatedly is visible rather
//! than merely suspected.

use crate::error::PackError;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

/// Runs pack code under a wall-clock bound.
#[derive(Debug, Default)]
pub struct Sandbox {
    abandoned: AtomicUsize,
}

impl Sandbox {
    /// A new sandbox.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many calls have been abandoned because they would not stop.
    ///
    /// Each one is a leaked thread. A non-zero count is a defect in a pack, not in the
    /// engine, and this is what makes it attributable rather than merely suspected.
    #[must_use]
    pub fn abandoned(&self) -> usize {
        self.abandoned.load(Ordering::Relaxed)
    }

    /// Run `work` with at most `limit` of wall-clock time.
    ///
    /// The cancellation flag is raised before waiting, so a cooperating function stops
    /// promptly and cheaply. A function that ignores it is abandoned when the limit passes.
    pub fn run<T, F>(
        &self,
        pack: &str,
        function: &str,
        limit: Duration,
        cancelled: Arc<AtomicBool>,
        work: F,
    ) -> Result<T, SandboxError>
    where
        F: FnOnce() -> Result<T, PackError> + Send + 'static,
        T: Send + 'static,
    {
        let (sender, receiver) = mpsc::channel();
        let handle = std::thread::Builder::new()
            .name(format!("pack:{pack}:{function}"))
            .spawn(move || {
                // A closed channel means the caller gave up. Nothing to do about it here,
                // and nothing worth failing over.
                let _ = sender.send(work());
            });

        let Ok(handle) = handle else {
            return Err(SandboxError::CouldNotStart {
                pack: pack.to_string(),
                function: function.to_string(),
            });
        };

        match receiver.recv_timeout(limit) {
            Ok(Ok(value)) => {
                // Joining a thread that has already sent is immediate.
                drop(handle.join());
                Ok(value)
            }
            Ok(Err(error)) => {
                drop(handle.join());
                Err(SandboxError::Failed(error))
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Ask it to stop, in case it is merely slow rather than stuck. Then leave:
                // waiting again would reintroduce the hang this exists to prevent.
                cancelled.store(true, Ordering::Release);
                self.abandoned.fetch_add(1, Ordering::Relaxed);
                Err(SandboxError::WouldNotStop {
                    pack: pack.to_string(),
                    function: function.to_string(),
                    limit,
                })
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(SandboxError::Panicked {
                pack: pack.to_string(),
                function: function.to_string(),
            }),
        }
    }
}

/// Run one call under a bound, without keeping a sandbox around.
///
/// For callers that do not need the abandonment count.
pub fn run_bounded<T, F>(
    pack: &str,
    function: &str,
    limit: Duration,
    cancelled: Arc<AtomicBool>,
    work: F,
) -> Result<T, SandboxError>
where
    F: FnOnce() -> Result<T, PackError> + Send + 'static,
    T: Send + 'static,
{
    Sandbox::new().run(pack, function, limit, cancelled, work)
}

/// Why a sandboxed call did not produce a value.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SandboxError {
    /// The function returned an error of its own.
    Failed(PackError),
    /// The function did not return within its bound and was abandoned.
    WouldNotStop {
        /// Which pack.
        pack: String,
        /// Which function.
        function: String,
        /// How long it was given.
        limit: Duration,
    },
    /// The function panicked.
    ///
    /// A panic in pack code must not take the query down, let alone the process. It is
    /// caught by the thread boundary and reported as this pack's failure.
    Panicked {
        /// Which pack.
        pack: String,
        /// Which function.
        function: String,
    },
    /// A thread could not be started to run it.
    CouldNotStart {
        /// Which pack.
        pack: String,
        /// Which function.
        function: String,
    },
}

impl SandboxError {
    /// Which pack is responsible.
    ///
    /// Always answerable. A query that fails inside extension code and cannot say which
    /// extension is a support ticket nobody can act on.
    #[must_use]
    pub fn pack(&self) -> &str {
        match self {
            Self::Failed(error) => &error.pack,
            Self::WouldNotStop { pack, .. }
            | Self::Panicked { pack, .. }
            | Self::CouldNotStart { pack, .. } => pack,
        }
    }
}

impl std::fmt::Display for SandboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Failed(error) => write!(f, "{error}"),
            Self::WouldNotStop {
                pack,
                function,
                limit,
            } => write!(
                f,
                "the function {pack}.{function} did not return within {limit:?} and did not \
                 respond to cancellation; the query was failed rather than left to hang. \
                 The call was abandoned and its thread will not be reclaimed until the \
                 process ends, which is a defect in the pack"
            ),
            Self::Panicked { pack, function } => write!(
                f,
                "the function {pack}.{function} panicked; the query failed but the engine \
                 did not"
            ),
            Self::CouldNotStart { pack, function } => {
                write!(f, "could not start a thread to run {pack}.{function}")
            }
        }
    }
}

impl std::error::Error for SandboxError {}

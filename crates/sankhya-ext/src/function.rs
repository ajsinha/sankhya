//! What a pack contributes, and what it is given when called.

use crate::error::PackError;
use crate::value::{LogicalType, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// What a function accepts and returns.
///
/// Declared rather than inferred. A function that works out its own return type from its
/// arguments cannot be planned against, and the planner has to know the shape of a result
/// before it runs anything.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Signature {
    /// The type of each argument, in order.
    pub arguments: Vec<LogicalType>,
    /// What it returns.
    pub returns: LogicalType,
    /// Whether the same arguments always give the same answer.
    ///
    /// A deterministic function can be evaluated once for repeated arguments, hoisted out
    /// of a loop, and cached. Declaring a non-deterministic one as deterministic produces
    /// results that change depending on how the query was optimised, which is the kind of
    /// bug that never reproduces.
    pub deterministic: bool,
}

impl Signature {
    /// A deterministic signature.
    #[must_use]
    pub fn of(arguments: impl Into<Vec<LogicalType>>, returns: LogicalType) -> Self {
        Self {
            arguments: arguments.into(),
            returns,
            deterministic: true,
        }
    }

    /// The same signature, marked non-deterministic.
    #[must_use]
    pub fn non_deterministic(mut self) -> Self {
        self.deterministic = false;
        self
    }

    /// Whether these arguments may be passed to this signature.
    #[must_use]
    pub fn accepts(&self, arguments: &[Value]) -> bool {
        if arguments.len() != self.arguments.len() {
            return false;
        }
        arguments
            .iter()
            .zip(&self.arguments)
            .all(|(value, wanted)| {
                // Null satisfies every declared type: it is a valid value of all of them.
                value
                    .logical_type()
                    .is_none_or(|actual| actual.satisfies(wanted))
            })
    }
}

/// What a function is given besides its arguments.
///
/// Carries the tenant, so a function cannot accidentally operate outside the caller's
/// boundary, and the cancellation flag, so it can stop when the query does.
#[derive(Clone, Debug)]
pub struct Invocation {
    tenant: String,
    cancelled: Arc<AtomicBool>,
    pack: String,
    function: String,
}

impl Invocation {
    /// Build an invocation context. The engine does this, not a pack.
    #[must_use]
    pub fn new(
        tenant: impl Into<String>,
        pack: impl Into<String>,
        function: impl Into<String>,
        cancelled: Arc<AtomicBool>,
    ) -> Self {
        Self {
            tenant: tenant.into(),
            cancelled,
            pack: pack.into(),
            function: function.into(),
        }
    }

    /// Whose query this is.
    ///
    /// A pack function has no way to reach another tenant's data --- it is handed values,
    /// not a connection --- so this is for a pack that needs to key its own state, not a
    /// boundary a pack could cross.
    #[must_use]
    pub fn tenant(&self) -> &str {
        &self.tenant
    }

    /// Stop if the query has.
    ///
    /// One atomic load, so it can be called between rows without measurable cost. A
    /// function doing bounded work per call need never touch it; one that loops must.
    ///
    /// A function that never calls this is stopped anyway --- see
    /// [`crate::sandbox::run_bounded`] --- but stopping it that way costs a thread. Calling
    /// this is how a pack stays cheap to cancel.
    pub fn check(&self) -> Result<(), PackError> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(PackError::cancelled(&self.pack, &self.function));
        }
        Ok(())
    }

    /// Whether the query has been asked to stop.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// An invalid-argument error already naming this pack and function.
    #[must_use]
    pub fn invalid_argument(&self, detail: impl Into<String>) -> PackError {
        PackError::invalid_argument(&self.pack, &self.function, detail)
    }

    /// A general failure already naming this pack and function.
    #[must_use]
    pub fn failed(&self, detail: impl Into<String>) -> PackError {
        PackError::failed(&self.pack, &self.function, detail)
    }
}

/// A function returning one value per call.
pub trait ScalarFunction: Send + Sync + std::fmt::Debug {
    /// What it is called in SQL.
    fn name(&self) -> &str;

    /// What it accepts and returns.
    fn signature(&self) -> Signature;

    /// Compute one result.
    fn invoke(&self, arguments: &[Value], context: &Invocation) -> Result<Value, PackError>;

    /// A sentence for whoever reads the function list.
    fn description(&self) -> &str {
        ""
    }
}

/// A function returning rows.
pub trait TableFunction: Send + Sync + std::fmt::Debug {
    /// What it is called in SQL.
    fn name(&self) -> &str;

    /// What it accepts.
    fn signature(&self) -> Signature;

    /// The columns it returns, as `(name, type)`, fixed regardless of arguments.
    ///
    /// Fixed because the planner needs the shape before execution. A table function whose
    /// columns depend on its data cannot be joined against without running it first, which
    /// defeats the point of planning.
    fn columns(&self) -> Vec<(String, LogicalType)>;

    /// Produce the rows.
    fn invoke(
        &self,
        arguments: &[Value],
        context: &Invocation,
    ) -> Result<Vec<Vec<Value>>, PackError>;

    /// Roughly how many rows to expect.
    ///
    /// The planner orders the downstream join with this. A wrong estimate is better than
    /// none, and none is what `Option::None` means --- it is not a way of saying "few".
    fn estimated_rows(&self, _arguments: &[Value]) -> Option<usize> {
        None
    }

    /// A sentence for whoever reads the function list.
    fn description(&self) -> &str {
        ""
    }
}

/// What a pack is, and how it announces itself.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PackInfo {
    /// The pack's name. Prefixes every function it contributes.
    pub name: String,
    /// Its own version, independent of the engine's.
    pub version: String,
    /// The version of this API it was built against.
    ///
    /// Checked at load time. A pack built against an incompatible version is refused with
    /// a message naming both, rather than loaded and failing later inside a query where
    /// the cause is no longer visible.
    pub api_version: u32,
    /// A sentence about what it is for.
    pub description: String,
}

/// The trait a pack implements.
///
/// **Self-registration is the whole design.** A pack tells the registry what it has; no
/// core component names a pack, ever. `check-layers` enforces the other direction --- no
/// core crate may depend on a pack --- and the two together are what make the
/// zero-core-changes exit criterion mechanical rather than a matter of review.
pub trait Pack: Send + Sync + std::fmt::Debug {
    /// What this pack is.
    fn info(&self) -> PackInfo;

    /// Contribute everything this pack offers.
    fn register(&self, registry: &mut crate::registry::Registry);
}

/// The version of this API that packs compile against.
///
/// Bumped only for a breaking change. `cargo xtask` detects those mechanically by comparing
/// the public surface against the previous release, so the number cannot drift from reality
/// through someone forgetting to raise it.
pub const API_VERSION: u32 = 1;

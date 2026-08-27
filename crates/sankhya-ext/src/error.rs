//! What a pack function may fail with.
//!
//! Every variant names the pack, because a query that fails inside extension code and does
//! not say which extension is a support ticket nobody can act on. The engine cannot work it
//! out afterwards: by the time the error surfaces, the call stack is gone.

use std::fmt;

/// A failure inside pack code.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PackError {
    /// Which pack.
    pub pack: String,
    /// Which function within it.
    pub function: String,
    /// What went wrong.
    pub kind: PackErrorKind,
    /// A message for whoever reads the query result.
    pub detail: String,
}

impl PackError {
    /// A function was given something it cannot work with.
    #[must_use]
    pub fn invalid_argument(
        pack: impl Into<String>,
        function: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            pack: pack.into(),
            function: function.into(),
            kind: PackErrorKind::InvalidArgument,
            detail: detail.into(),
        }
    }

    /// The query was cancelled or ran out of time while inside this function.
    #[must_use]
    pub fn cancelled(pack: impl Into<String>, function: impl Into<String>) -> Self {
        Self {
            pack: pack.into(),
            function: function.into(),
            kind: PackErrorKind::Cancelled,
            detail: "the query was cancelled or exceeded its deadline".to_string(),
        }
    }

    /// The function did something it is not permitted to.
    #[must_use]
    pub fn forbidden(
        pack: impl Into<String>,
        function: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            pack: pack.into(),
            function: function.into(),
            kind: PackErrorKind::Forbidden,
            detail: detail.into(),
        }
    }

    /// Something else went wrong inside the function.
    #[must_use]
    pub fn failed(
        pack: impl Into<String>,
        function: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            pack: pack.into(),
            function: function.into(),
            kind: PackErrorKind::Failed,
            detail: detail.into(),
        }
    }

    /// Whether submitting the same query again could succeed unchanged.
    #[must_use]
    pub const fn retryable(&self) -> bool {
        matches!(self.kind, PackErrorKind::Cancelled)
    }
}

/// The category of a pack failure.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PackErrorKind {
    /// The arguments were wrong.
    InvalidArgument,
    /// The query stopped while inside the function.
    Cancelled,
    /// The function attempted something it may not do.
    Forbidden,
    /// Anything else.
    Failed,
}

impl fmt::Display for PackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self.kind {
            PackErrorKind::InvalidArgument => "invalid argument",
            PackErrorKind::Cancelled => "cancelled",
            PackErrorKind::Forbidden => "forbidden",
            PackErrorKind::Failed => "failed",
        };
        write!(
            f,
            "{kind} in {}.{}: {}",
            self.pack, self.function, self.detail
        )
    }
}

impl std::error::Error for PackError {}

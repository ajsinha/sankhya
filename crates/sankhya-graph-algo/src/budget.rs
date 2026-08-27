//! The bound every primitive runs under, and how a bounded result reports itself.
//!
//! An unbounded traversal on a real graph is not slow, it is *unbounded*: a six-hop
//! expansion through one high-degree vertex reaches most of the graph. So there is no
//! unbounded entry point in this crate. Every primitive takes a [`Budget`], and every
//! result says whether it hit one.
//!
//! The rule that matters is at the bottom of this file: **a truncated result must never be
//! mistakable for an absence of results**. "No paths found" and "gave up before finding
//! any" are different answers, and an investigation that confuses them reaches a
//! conclusion the data does not support.

/// What a primitive is allowed to spend.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Budget {
    /// How many results to return before stopping.
    pub max_results: usize,
    /// How many vertices may be visited.
    ///
    /// The bound that actually stops a runaway expansion. Result count does not: a
    /// traversal can visit millions of vertices and yield nothing.
    pub max_visits: usize,
    /// How far from the seeds to go.
    pub max_depth: u32,
    /// The largest degree a vertex may have before it is skipped rather than expanded.
    ///
    /// In a power-law network a few vertices connect to a large fraction of the graph.
    /// Expanding one is rarely informative and always expensive, and the skip is reported
    /// rather than silent --- a suppressed hub can be the whole answer.
    pub max_degree: usize,
}

impl Budget {
    /// A budget generous enough for a test, and still finite.
    ///
    /// Named for what it is. There is no `unlimited()`, because the entire point of this
    /// type is that the unbounded case does not exist.
    #[must_use]
    pub const fn generous() -> Self {
        Self {
            max_results: 100_000,
            max_visits: 10_000_000,
            max_depth: 64,
            max_degree: usize::MAX,
        }
    }

    /// A budget for an interactive query: shallow, narrow, and quick to refuse.
    #[must_use]
    pub const fn interactive() -> Self {
        Self {
            max_results: 1_000,
            max_visits: 100_000,
            max_depth: 6,
            max_degree: 10_000,
        }
    }

    /// The same budget, stopping after `n` results.
    #[must_use]
    pub const fn with_max_results(mut self, n: usize) -> Self {
        self.max_results = n;
        self
    }

    /// The same budget, going no deeper than `n`.
    #[must_use]
    pub const fn with_max_depth(mut self, n: u32) -> Self {
        self.max_depth = n;
        self
    }

    /// The same budget, skipping vertices of degree above `n`.
    #[must_use]
    pub const fn with_max_degree(mut self, n: usize) -> Self {
        self.max_degree = n;
        self
    }

    /// The same budget, visiting no more than `n` vertices.
    #[must_use]
    pub const fn with_max_visits(mut self, n: usize) -> Self {
        self.max_visits = n;
        self
    }
}

impl Default for Budget {
    fn default() -> Self {
        Self::interactive()
    }
}

/// Why a primitive stopped.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Truncation {
    /// It stopped because it had enough results, and more exist.
    pub by_results: bool,
    /// It stopped because it had visited enough vertices.
    pub by_visits: bool,
    /// It reached the depth limit with frontier left to expand.
    pub by_depth: bool,
    /// Vertices skipped for exceeding the degree cap.
    ///
    /// Listed rather than counted. Which hub was suppressed is frequently the finding,
    /// and a bare count cannot be followed up.
    pub suppressed: Vec<crate::ids::VertexId>,
}

impl Truncation {
    /// Whether the result is the whole answer.
    ///
    /// A suppressed vertex counts as truncation. The traversal did not look through it,
    /// so the result is a lower bound rather than an answer, and callers presenting it as
    /// complete would be overstating what was searched.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        !self.by_results && !self.by_visits && !self.by_depth && self.suppressed.is_empty()
    }

    /// A one-line account of why this is not the whole answer, or `None` if it is.
    ///
    /// Exists so callers have no excuse for rendering a truncated result as complete: the
    /// explanation is already written.
    #[must_use]
    pub fn explain(&self) -> Option<String> {
        if self.is_complete() {
            return None;
        }
        let mut reasons = Vec::new();
        if self.by_results {
            reasons.push("the result limit was reached and further results exist".to_string());
        }
        if self.by_visits {
            reasons.push("the visit budget was exhausted before the search finished".to_string());
        }
        if self.by_depth {
            reasons.push("the depth limit was reached with unexplored frontier".to_string());
        }
        if !self.suppressed.is_empty() {
            reasons.push(format!(
                "{} vertex/vertices exceeded the degree cap and were not expanded",
                self.suppressed.len()
            ));
        }
        Some(reasons.join("; "))
    }
}

/// A result together with why it might not be the whole one.
///
/// The two are inseparable by construction. Returning a bare `Vec` would let a caller drop
/// the truncation flag by accident, and the accident is invisible --- a short list looks
/// exactly like a short answer.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Bounded<T> {
    /// What was found.
    pub found: T,
    /// Why the search stopped, if it stopped early.
    pub truncation: Truncation,
}

impl<T> Bounded<T> {
    /// A complete result.
    pub const fn complete(found: T) -> Self {
        Self {
            found,
            truncation: Truncation {
                by_results: false,
                by_visits: false,
                by_depth: false,
                suppressed: Vec::new(),
            },
        }
    }

    /// A result that stopped early.
    pub const fn truncated(found: T, truncation: Truncation) -> Self {
        Self { found, truncation }
    }

    /// Whether this is the whole answer.
    pub fn is_complete(&self) -> bool {
        self.truncation.is_complete()
    }

    /// The result, only if it is the whole one.
    ///
    /// For callers whose conclusion would be wrong on a partial answer. Anything used as
    /// evidence should come through here rather than reading `found` directly.
    pub fn complete_only(&self) -> Option<&T> {
        self.truncation.is_complete().then_some(&self.found)
    }

    /// Apply a function to the result, keeping the truncation.
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Bounded<U> {
        Bounded {
            found: f(self.found),
            truncation: self.truncation,
        }
    }
}

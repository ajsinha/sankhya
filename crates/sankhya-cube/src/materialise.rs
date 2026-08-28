//! What is materialised, who decides, and where an answer comes from.
//!
//! # The key that cannot go stale
//!
//! A materialised cuboid is keyed by *(definition version, snapshot, cuboid)*. Per
//! `FR-QUERY-20` that is the whole invalidation story: files are immutable and every key
//! embeds the snapshot, so a new commit produces a **miss**, not a stale hit. There is no
//! invalidation protocol to get wrong, no time-to-live to tune, and no window in which a
//! stale answer is served.
//!
//! That changes what materialisation *is*. It is not a second copy of the truth --- it is a
//! cache, and being wrong about what to cache costs latency rather than correctness. Which
//! is exactly why it can be automatic.
//!
//! # Three levels of control, and the one that only goes one way
//!
//! | Level | Who sets it | What it controls |
//! |---|---|---|
//! | Definition | whoever models the cube | cuboids **pinned** --- always worth having |
//! | Configuration | the operator | the **budget** greedy selection spends |
//! | Session | the caller | whether *this* query uses materialisation at all |
//!
//! The session level is deliberately **one-directional**. A caller may ask for less ---
//! [`Session::Off`] to check a figure against the base data, [`Session::PinnedOnly`] to
//! avoid a cuboid selected from somebody else's query log --- and may not ask for more.
//!
//! A session that could raise the budget would be an unbounded storage grant to anybody who
//! can open one, which is a resource exhaustion with a polite interface. The asymmetry is
//! not a limitation of the implementation; it is the point of having an operator level at
//! all.
//!
//! # Materialisation must not change the answer
//!
//! `M7`'s exit criterion 3a: every query returns bit-identical results with materialisation
//! on and off. [`plan`] therefore only ever chooses *where* an answer is computed from, and
//! a cuboid is a candidate only when [`answerable_from`] permits every roll-up between it
//! and the query. A cache that changes results is not a cache.

use sankhya_cube_algo::ancestor::{answerable_from, rolled_away};
use sankhya_cube_algo::lattice::Cuboid;
use sankhya_cube_algo::measure::Measure;
use std::collections::BTreeSet;
use std::fmt;

/// Where a materialised cuboid lives.
///
/// Rendered into an ordinary published table name: the open-storage commitment gets no
/// exception for the fast path, so a materialised cuboid is readable by anything that can
/// read a table.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Key {
    /// The cube definition's fingerprint --- see [`crate::version`].
    pub definition: u64,
    /// The snapshot the cuboid was computed at.
    pub snapshot: u64,
    /// Which cuboid.
    pub cuboid: Cuboid,
}

impl Key {
    /// A key.
    #[must_use]
    pub const fn new(definition: u64, snapshot: u64, cuboid: Cuboid) -> Self {
        Self { definition, snapshot, cuboid }
    }

    /// The table this cuboid is published as.
    ///
    /// Each dimension is **length-prefixed**, not merely separated. A separator alone lets
    /// a cuboid over `a__b` and `c` render identically to one over `a` and `b__c`, and the
    /// two then share storage --- one cube's totals served for another's query.
    ///
    /// A count of dimensions does not fix it: both of those cuboids have two. The prefix
    /// does, and it is the same reasoning as the definition fingerprint's, arrived at the
    /// same way --- by a test that failed.
    #[must_use]
    pub fn table(&self, cube: &str) -> String {
        let mut out = format!(
            "__cube_{}_{cube}_{:016x}_{:016x}",
            cube.len(),
            self.definition,
            self.snapshot
        );
        for dimension in self.cuboid.dimensions() {
            out.push_str(&format!("_{}_{dimension}", dimension.len()));
        }
        out
    }
}

/// What a caller may ask of materialisation for one query.
///
/// One-directional by construction: every variant narrows. There is no `Session` value that
/// widens, so a caller cannot spend an operator's storage budget.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Session {
    /// Use whatever the definition and configuration provide.
    #[default]
    AsConfigured,
    /// Use only cuboids the definition pins, ignoring anything selected from a query log.
    PinnedOnly,
    /// Compute from the base data.
    ///
    /// The reproducibility check: a figure that differs between this and `AsConfigured` is
    /// a defect, not a tuning question.
    Off,
}

/// The three levels, resolved.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Policy {
    pinned: BTreeSet<Cuboid>,
    budget_rows: u64,
}

impl Policy {
    /// Cuboids the definition pins, and the rows configuration allows selection to spend.
    #[must_use]
    pub fn new(pinned: impl IntoIterator<Item = Cuboid>, budget_rows: u64) -> Self {
        Self {
            pinned: pinned.into_iter().collect(),
            budget_rows,
        }
    }

    /// Nothing pinned and nothing budgeted: every query computes from the base.
    #[must_use]
    pub fn none() -> Self {
        Self {
            pinned: BTreeSet::new(),
            budget_rows: 0,
        }
    }

    /// The pinned cuboids.
    #[must_use]
    pub fn pinned(&self) -> &BTreeSet<Cuboid> {
        &self.pinned
    }

    /// The rows selection may spend.
    ///
    /// Not reachable through [`Session`], because a caller raising it would be granting
    /// themselves the operator's storage.
    #[must_use]
    pub const fn budget_rows(&self) -> u64 {
        self.budget_rows
    }

    /// Which of `available` this query may draw on.
    ///
    /// `available` is what is materialised now --- pinned cuboids and whatever selection
    /// bought. A pinned cuboid that is not yet built is not usable, so pinning is a
    /// statement of intent rather than a promise about this instant.
    #[must_use]
    pub fn usable<'a>(&self, available: &'a [Cuboid], session: Session) -> Vec<&'a Cuboid> {
        match session {
            Session::Off => Vec::new(),
            Session::PinnedOnly => available
                .iter()
                .filter(|cuboid| self.pinned.contains(cuboid))
                .collect(),
            Session::AsConfigured => available.iter().collect(),
        }
    }
}

/// Where a query's answer will be computed from.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Plan {
    /// The cuboid to read.
    pub from: Cuboid,
    /// The dimensions rolled away between it and the query.
    ///
    /// Empty when the cuboid *is* the query. Reported because "why was this fast?" and "why
    /// was this slow?" are the same question asked twice, and an operator cannot answer
    /// either from a plan that only names a table.
    pub rolling_away: Vec<String>,
    /// Whether the answer comes from a materialised cuboid rather than the base.
    pub materialised: bool,
}

/// Choose where to answer `query` from.
///
/// Prefers the narrowest usable cuboid that may legally answer --- narrowest by dimension
/// count, which is the cheapest to scan, with ties broken by name so the plan is stable
/// across runs. Falls back to `base`, which answers everything.
///
/// A cuboid is a candidate only when the measure permits every roll-up between it and the
/// query. Skipping that test is how materialisation starts changing answers, and the change
/// is invisible: the number is real, it is just computed from partial aggregates that do not
/// compose.
#[must_use]
pub fn plan(
    query: &Cuboid,
    measure: &Measure,
    available: &[&Cuboid],
    base: &Cuboid,
) -> Plan {
    let mut best: Option<&Cuboid> = None;
    for candidate in available {
        if !permits(candidate, query, measure) {
            continue;
        }
        let better = best.is_none_or(|current| {
            (candidate.width(), candidate.dimensions())
                < (current.width(), current.dimensions())
        });
        if better {
            best = Some(candidate);
        }
    }

    match best {
        Some(cuboid) => Plan {
            rolling_away: away(cuboid, query),
            from: cuboid.clone(),
            materialised: true,
        },
        None => Plan {
            rolling_away: away(base, query),
            from: base.clone(),
            materialised: false,
        },
    }
}

/// Whether `candidate` may answer `query` for this measure.
fn permits(candidate: &Cuboid, query: &Cuboid, measure: &Measure) -> bool {
    let held = candidate.dimensions();
    let wanted = query.dimensions();
    rolled_away(&wanted, &held).is_some_and(|gone| answerable_from(measure, &gone).permitted())
}

/// The dimensions between a cuboid and a query, or none when it cannot answer it.
fn away(from: &Cuboid, query: &Cuboid) -> Vec<String> {
    let held = from.dimensions();
    let wanted = query.dimensions();
    rolled_away(&wanted, &held)
        .unwrap_or_default()
        .into_iter()
        .map(ToString::to_string)
        .collect()
}

impl fmt::Display for Plan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let source = if self.materialised { "materialised" } else { "base" };
        write!(f, "from the {source} cuboid ({})", self.from.dimensions().join(", "))?;
        if !self.rolling_away.is_empty() {
            write!(f, ", rolling away {}", self.rolling_away.join(", "))?;
        }
        Ok(())
    }
}

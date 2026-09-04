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
    /// **What the principal who caused it was permitted to see.**
    ///
    /// From `Guard::scope_digest`, and part of the key for the same reason it is part of the
    /// hydration cache's: an aggregate computed over the rows one principal may read is not
    /// an answer for another, so anything that stores an aggregate must key it by the scope
    /// it was computed under.
    ///
    /// Here the consequence is stronger than a cache miss. A materialised cuboid is a
    /// **published table**, so two scopes are two tables --- separate files, separate names,
    /// nothing shared. That is the strongest form the separation can take: a bug in the
    /// lookup logic cannot serve one scope's rows to another, because the rows are not in
    /// the file being read.
    pub scope: u64,
    /// **Which measure.**
    ///
    /// # Why a cuboid is not a shape alone
    ///
    /// A cube's measures are different questions over the same dimensions --- `amount` sums,
    /// `ratio` may be a mean, and one may have no aggregation rule at all. Without this the
    /// first measure to be materialised wrote each shape and every later one saw `exists()`
    /// and skipped; reads then built the same measure-free key and labelled whatever came back
    /// with the measure they had asked for.
    ///
    /// On the shipped fixture a maintained `sales` cube returned `amount`'s numbers under the
    /// name `ratio` --- and answered a `Rule::None` measure out of a stored aggregate, which is
    /// the one thing the ancestor-answerability machinery exists to prevent. Materialisation
    /// turned an honest refusal into a plausible number, which is worse than either.
    ///
    /// The in-memory catalog had this exact defect and was fixed by keying on
    /// `(cube, measure)`, with a comment explaining why. The on-disk key never got the same
    /// treatment: `COR-04`.
    pub measure: String,
    /// Which cuboid.
    pub cuboid: Cuboid,
}

impl Key {
    /// A key.
    #[must_use]
    pub fn new(
        definition: u64,
        snapshot: u64,
        scope: u64,
        measure: impl Into<String>,
        cuboid: Cuboid,
    ) -> Self {
        Self { definition, snapshot, scope, measure: measure.into(), cuboid }
    }

    /// The scope of a cuboid computed with nothing withheld.
    ///
    /// Zero, and a sentinel rather than a digest: `Guard::scope_digest` hashes the tenant and
    /// the table, so no real guard can produce this value. That is deliberate --- a reader is
    /// matched to this cuboid by asking whether their guard withholds anything
    /// (`Guard::withholds_nothing`), never by comparing digests, which could only ever miss.
    pub const UNRESTRICTED: u64 = 0;

    /// A key for a cuboid computed with nothing withheld.
    ///
    /// The unrestricted scope, named rather than written as a bare zero: a caller reaching
    /// for this is asserting that the cells behind it were computed over every row, and that
    /// assertion should be legible at the call site.
    #[must_use]
    pub fn unrestricted(
        definition: u64,
        snapshot: u64,
        measure: impl Into<String>,
        cuboid: Cuboid,
    ) -> Self {
        Self::new(definition, snapshot, Self::UNRESTRICTED, measure, cuboid)
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
            "__cube_{}_{cube}_{:016x}_{:016x}_{:016x}_{}_{}",
            cube.len(),
            self.definition,
            self.snapshot,
            self.scope,
            // Length-prefixed for the reason the cube's name is: a measure called `a__b` and
            // one called `a` on a cube called `_b` must not render to the same table.
            self.measure.len(),
            self.measure
        );
        for dimension in self.cuboid.dimensions() {
            out.push_str(&format!("_{}_{dimension}", dimension.len()));
        }
        out
    }
}

/// The cube and key a rendered table name refers to.
///
/// # Why a name can be read back at all
///
/// Because it was written to be. Each part is either fixed width — the definition, snapshot
/// and scope are sixteen hex digits each — or length-prefixed, which is why the cube's name
/// carries its own length. That prefix was put there so two different cuboids could not
/// render identically; it also makes the rendering reversible, and reversibility is what lets
/// a sweep decide whether a cuboid on disk is still worth keeping without a side table
/// recording what it already said.
///
/// Returns `None` for anything that is not one of ours, which is the important half: a
/// directory this cannot parse is a directory it must not delete.
#[must_use]
pub fn parse(table: &str) -> Option<(String, Key)> {
    let rest = table.strip_prefix("__cube_")?;
    let (length, rest) = rest.split_once('_')?;
    let length: usize = length.parse().ok()?;
    if rest.len() < length {
        return None;
    }
    let (cube, rest) = rest.split_at(length);
    let rest = rest.strip_prefix('_')?;

    let (definition, rest) = rest.split_at_checked(16)?;
    let rest = rest.strip_prefix('_')?;
    let (snapshot, rest) = rest.split_at_checked(16)?;
    let rest = rest.strip_prefix('_')?;
    let (scope, rest) = rest.split_at_checked(16)?;

    let rest = rest.strip_prefix('_')?;
    let (length, rest) = rest.split_once('_')?;
    let length: usize = length.parse().ok()?;
    if rest.len() < length {
        return None;
    }
    let (measure, mut rest) = rest.split_at(length);

    let mut dimensions: Vec<String> = Vec::new();
    while let Some(tail) = rest.strip_prefix('_') {
        let (length, tail) = tail.split_once('_')?;
        let length: usize = length.parse().ok()?;
        if tail.len() < length {
            return None;
        }
        let (dimension, tail) = tail.split_at(length);
        dimensions.push(dimension.to_string());
        rest = tail;
    }
    if !rest.is_empty() {
        return None;
    }

    Some((
        cube.to_string(),
        Key {
            definition: u64::from_str_radix(definition, 16).ok()?,
            snapshot: u64::from_str_radix(snapshot, 16).ok()?,
            scope: u64::from_str_radix(scope, 16).ok()?,
            measure: measure.to_string(),
            cuboid: Cuboid::of(&dimensions),
        },
    ))
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

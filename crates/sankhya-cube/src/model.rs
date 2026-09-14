//! What a cube is made of, and the one way to make one.

use crate::validate::{self, Rejection};
use crate::version::fingerprint;
use sankhya_cube_algo::hierarchy::Hierarchy;
use sankhya_cube_algo::measure::Measure;
use std::collections::BTreeMap;

/// One level of a dimension: a column of the dimension table, and the members it holds.
///
/// Levels are ordered coarse-to-fine within a dimension, which is the order a drill-down
/// walks. The order is the declaration order rather than something inferred from
/// cardinality --- inferring it means a month with more distinct values than its days (a
/// sparse fact table, early in a period) silently reverses the hierarchy.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Level {
    /// The level's name, as a query writes it.
    pub name: String,
    /// The column holding this level's member key.
    pub column: String,
}

impl Level {
    /// A level over a column.
    pub fn new(name: impl Into<String>, column: impl Into<String>) -> Self {
        Self { name: name.into(), column: column.into() }
    }
}

/// A dimension: a table, the column joining it to the fact table, and its levels.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Dimension {
    /// The dimension's name, as a query writes it.
    pub name: String,
    /// The published table holding its members.
    pub table: String,
    /// The fact-table column that joins to this dimension.
    pub joins_on: String,
    /// Levels, coarse to fine.
    pub levels: Vec<Level>,
    /// A hierarchy written into the definition rather than read from data.
    ///
    /// Alternate roll-ups and shared members are usually *declared* --- a handful of edges
    /// somebody maintains --- and being in the definition, they are checked before the cube
    /// exists. A hierarchy read from the dimension table instead is checked at hydration by
    /// the same [`Hierarchy::validate`], because a cycle discovered during a query is an
    /// unbounded traversal and a timeout that names nothing.
    pub rollups: Option<Hierarchy>,
    /// Parent-child edges, as *(child column, parent column)*, for a recursive hierarchy.
    ///
    /// Distinct from `levels`: a level hierarchy has a fixed depth known at definition
    /// time, and a parent-child one does not. A ragged organisation chart is the second
    /// kind, and flattening it into the first is what forces the padding that
    /// `FR-QUERY-11` forbids.
    pub parent_child: Option<(String, String)>,
}

impl Dimension {
    /// A dimension over a table.
    pub fn new(
        name: impl Into<String>,
        table: impl Into<String>,
        joins_on: impl Into<String>,
        levels: Vec<Level>,
    ) -> Self {
        Self {
            name: name.into(),
            table: table.into(),
            joins_on: joins_on.into(),
            levels,
            rollups: None,
            parent_child: None,
        }
    }

    /// The same dimension, with a parent-child hierarchy.
    #[must_use]
    pub fn recursive(mut self, child: impl Into<String>, parent: impl Into<String>) -> Self {
        self.parent_child = Some((child.into(), parent.into()));
        self
    }

    /// The same dimension, with a declared roll-up hierarchy.
    #[must_use]
    pub fn rolling_up(mut self, rollups: Hierarchy) -> Self {
        self.rollups = Some(rollups);
        self
    }

    /// The level of a given name, if it has one.
    #[must_use]
    pub fn level(&self, name: &str) -> Option<&Level> {
        self.levels.iter().find(|l| l.name == name)
    }
}

/// Whether a fact source is a declared query rather than a table name.
///
/// One definition, used by the model, by validation and by the reader, because three
/// spellings of "does this look like a query" is how two of them come to disagree about a
/// statement somebody actually typed.
#[must_use]
pub fn is_a_query(source: &str) -> bool {
    source.trim_start().starts_with('(')
}

/// A cube as somebody wrote it down --- not yet checked, and not yet usable.
///
/// Separate from [`Cube`] on purpose. A single type for both would mean every function
/// taking a cube has to decide whether it trusts what it was handed, and some of them would
/// decide wrongly.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Definition {
    /// The cube's name.
    pub name: String,
    /// Where the facts come from: a published table's name, or a **declared query**.
    ///
    /// A query is written parenthesised --- `FROM (SELECT …)` --- and is stored as it was
    /// written, because it is what the fingerprint hashes and what a reader has to be shown.
    ///
    /// [ADR-0012](../../../docs/adr/0012-open-capabilities.md): the generalisation is small
    /// under one rule --- an artefact that cannot say what it needs cannot be cached
    /// correctly, checked against policy, or bounded. So a query does not widen what a cube
    /// *is*; it widens what its fact source may be, and [`Definition::reads`] carries the
    /// declaration that keeps the rest working unchanged.
    pub fact_table: String,
    /// Every table this cube reads, which is what its authorization and its snapshot are
    /// resolved against.
    ///
    /// # Why a list rather than the one name
    ///
    /// Because a declared query may read several, and each of the three things the fact
    /// source is used for is a property of *all* of them:
    ///
    /// - **Authorization.** A principal must be allowed to read every table the query reads,
    ///   not the first one. A cube over a join is a cube over both sides.
    /// - **The snapshot.** The newest of its dependencies' snapshots, so the materialisation
    ///   key keeps working with no new invalidation protocol.
    /// - **The fingerprint.** A cube whose source query reads a different table is a
    ///   different cube, and nothing in the query *text* alone says which tables those are.
    ///
    /// A named fact table is the one-element case, which is why nothing above is a special
    /// path for it. Resolved by whoever creates the cube --- the query is planned under the
    /// caller's guard, and the tables it turns out to read are recorded here.
    pub reads: Vec<String>,
    /// Its dimensions.
    pub dimensions: Vec<Dimension>,
    /// Its measures, each declaring a rule per dimension.
    pub measures: Vec<Measure>,
    /// How stale this cube's materialised cells may be, in table versions.
    ///
    /// # Why versions and not a duration
    ///
    /// A materialised cuboid is keyed by the snapshot it was computed at, so its staleness is
    /// **exactly** the distance from the table's current version --- an integer, known without
    /// a clock. A duration would have to be estimated from commit rates, and an estimate is
    /// what makes an SLA a decoration.
    ///
    /// A staleness *target*, not a schedule. `Some(0)` means only a cuboid at the current
    /// version may be used; `Some(5)` tolerates five commits' drift; `None` is the
    /// [`Lifetime::Declared`] case --- nothing is materialised, so nothing can be stale.
    ///
    /// See [ADR-0009](../../../docs/adr/0009-the-cube-lifecycle.md).
    pub target_lag: Option<u64>,
    /// Cuboids this cube always wants materialised, whatever the query log says.
    ///
    /// The **definition** level of §11.6's three controls, and the one whoever models the
    /// cube owns. Selection spends an operator's budget on evidence; a pin is the statement
    /// that a shape is worth holding before any evidence exists --- the month-end roll-up
    /// nobody runs until the day it must be instant.
    ///
    /// Each entry is a list of dimension names. Stored that way rather than as a `Cuboid` so
    /// this type keeps no dependency it does not need and the persisted form stays obvious.
    pub pinned: Vec<Vec<String>>,
}

/// Which of the three lifetimes a cube has.
///
/// Derived from the definition rather than stored separately, so a cube cannot claim one
/// lifetime and behave as another. See
/// [ADR-0009](../../../docs/adr/0009-the-cube-lifecycle.md).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lifetime {
    /// Persisted, and nothing is pre-computed. Every query hydrates under its own scope.
    Declared,
    /// Persisted, materialised, and held to a stated lag.
    Maintained,
}

/// The fact source's tables, then every dimension table, each named once.
///
/// Order is stable --- the source's list as it was given, then the dimensions in declaration
/// order --- because this feeds a fingerprint, and a fingerprint that depends on iteration
/// order is one that changes when nothing did.
///
/// Deduplicated because a dimension over the fact table itself is an ordinary thing to
/// declare (`DIMENSION region FROM orders ON region`), and listing a table twice folds its
/// scope into the authorization digest twice --- which decides whether two principals share a
/// cache entry.
fn reads_of(source: &[String], dimensions: &[Dimension]) -> Vec<String> {
    let mut reads: Vec<String> = source.to_vec();
    for dimension in dimensions {
        let table = dimension.table.trim();
        if !table.is_empty() && !reads.iter().any(|held| held == table) {
            reads.push(table.to_string());
        }
    }
    reads
}

impl Definition {
    /// A definition. Nothing is checked here; call [`Definition::validate`].
    pub fn new(
        name: impl Into<String>,
        fact_table: impl Into<String>,
        dimensions: Vec<Dimension>,
        measures: Vec<Measure>,
    ) -> Self {
        let fact_table = fact_table.into();
        Self {
            // What the fact **source** reads. A name reads exactly itself; a declared query
            // reads whatever the caller resolved. The list is never empty, which is what lets
            // every reader iterate it rather than ask which kind of source this is.
            //
            // The **dimension tables are not here**, and that is deliberate rather than an
            // omission: they are folded in by [`Definition::validate`], where the dimensions
            // are final. Computing them at construction cached an answer that goes stale the
            // moment a caller edits `dimensions` afterwards --- which every fixture in this
            // repository does, and which is how the first version of this shipped a `reads`
            // still naming the table a dimension had been moved off.
            reads: vec![fact_table.clone()],
            name: name.into(),
            fact_table,
            dimensions,
            measures,
            // Declared, not maintained. Persisting a definition is cheap; materialising is
            // storage and work, and a cube should not acquire either by being written down.
            target_lag: None,
            pinned: Vec::new(),
        }
    }

    /// The same definition, over a **declared query** rather than a named table.
    ///
    /// `reads` is what the query was found to read, resolved by the caller. Given none, the
    /// definition is refused by [`validate`](crate::validate) rather than accepted with an
    /// empty dependency list --- a cube that cannot say what it reads cannot be authorized,
    /// keyed, or invalidated, and is the exact artefact `ADR-0012` refuses.
    ///
    /// The dimension tables are folded in by [`Definition::validate`], not here: the query is
    /// only half of what this cube opens, and a cube over a query joined to `sales.regions`
    /// reads `sales.regions` whether or not any `FROM` clause mentions it.
    #[must_use]
    pub fn over_query(mut self, query: impl Into<String>, reads: Vec<String>) -> Self {
        self.fact_table = query.into();
        self.reads = reads;
        self
    }

    /// Whether the fact source is a query rather than a table name.
    ///
    /// Asked of the text, because that is what was written down and stored: a declared query
    /// is parenthesised, and a table name cannot be.
    #[must_use]
    pub fn fact_is_a_query(&self) -> bool {
        is_a_query(&self.fact_table)
    }

    /// Whether this is a **derived result** rather than a cube.
    ///
    /// [ADR-0014](../../../docs/adr/0014-materialized-views-and-the-cube-lifetime.md) Option A,
    /// in one line: *a definition with no dimensions and no measures is simply a maintained
    /// query.* The shape is the discriminator rather than a flag beside it, because a flag can
    /// disagree with the shape and this cannot.
    ///
    /// # Why this is not a second kind of artefact
    ///
    /// It is the same `Definition`, so it gets the same declared query, the same dependency
    /// list, the same snapshot key, the same fingerprint and the same catalogue. That is the
    /// whole of the ADR's reasoning: every candidate design for a separate materialized-view
    /// crate reused all of those, and *"the failure mode is not that it will not work — it is
    /// that it will grow a second refresh loop, a second staleness rule and a second
    /// reclamation path, and the two will drift."*
    #[must_use]
    pub fn is_derived(&self) -> bool {
        self.dimensions.is_empty() && self.measures.is_empty()
    }

    /// The same definition, held to a staleness target.
    #[must_use]
    pub fn maintained_within(mut self, versions: u64) -> Self {
        self.target_lag = Some(versions);
        self
    }

    /// Always materialise this shape, whatever has been asked for.
    #[must_use]
    pub fn pinning(mut self, dimensions: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.pinned.push(dimensions.into_iter().map(Into::into).collect());
        self
    }

    /// Which lifetime this definition describes.
    #[must_use]
    pub const fn lifetime(&self) -> Lifetime {
        match self.target_lag {
            Some(_) => Lifetime::Maintained,
            None => Lifetime::Declared,
        }
    }

    /// A derived result: a declared query with no dimensions and no measures.
    #[must_use]
    pub fn derived(name: impl Into<String>, query: impl Into<String>, reads: Vec<String>) -> Self {
        let mut definition = Self::new(name, "", Vec::new(), Vec::new());
        definition.fact_table = query.into();
        definition.reads = reads;
        definition
    }

    /// Check it, and produce a cube.
    ///
    /// Returns **every** rejection, not the first. A definition with four undeclared
    /// measure rules should be fixable in one sitting; reporting them one per build is how
    /// a person stops reading the message and declares `Sum` for everything, which is the
    /// outcome this whole design exists to prevent.
    ///
    /// # Errors
    /// Returns the rejections. The list is non-empty and ordered for a stable diff.
    pub fn validate(mut self) -> Result<Cube, Vec<Rejection>> {
        let rejections = validate::inspect(&self);
        if !rejections.is_empty() {
            return Err(rejections);
        }
        // **Every dimension table, folded in here and nowhere earlier.**
        //
        // `reads` is what a principal must be allowed to read before this cube is registered
        // for them, what the snapshot is taken across, and part of the fingerprint that
        // decides whether a materialised cuboid still answers. A table missing from it is a
        // table outside all three --- and until `M24b` the dimension tables were missing,
        // while `GUIDE.md` said a cube whose *dimension tables* you cannot read is refused.
        //
        // Unreachable before `M23`, which is the honest part: until hydration opened them, a
        // cube genuinely did read only its facts.
        //
        // Here rather than in `new` because this is the last moment the definition changes.
        // Callers build a definition and then edit its dimensions --- a fixture moving a
        // dimension onto another table, `into_definition` restoring a stored `reads` over the
        // computed one --- and a list cached at construction is a list that goes quietly
        // stale. A `Cube` can be made no other way, so folding here makes it an invariant of
        // the type rather than a step somebody has to remember.
        self.reads = reads_of(&self.reads, &self.dimensions);
        let version = fingerprint(&self);
        Ok(Cube { definition: self, version })
    }
}

/// A validated cube.
///
/// The only way to hold one is [`Definition::validate`], so a function taking a `Cube` has
/// no reachable state in which its hierarchies contain a cycle or its measures are
/// undeclared. That is the point of the type existing separately.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Cube {
    definition: Definition,
    version: u64,
}

impl Cube {
    /// How stale its materialised cells may be, in table versions.
    ///
    /// `None` for a cube that materialises nothing, which is not the same as a target of
    /// zero: zero admits a cuboid at the current version, and `None` admits none at all.
    #[must_use]
    pub const fn target_lag(&self) -> Option<u64> {
        self.definition.target_lag
    }

    /// The cuboids this cube's definition pins.
    ///
    /// A pin is a statement of intent, not a promise about this instant: a pinned cuboid that
    /// has not been built yet is simply not there to use.
    #[must_use]
    pub fn pinned(&self) -> Vec<sankhya_cube_algo::lattice::Cuboid> {
        self.definition
            .pinned
            .iter()
            .map(|dimensions| {
                sankhya_cube_algo::lattice::Cuboid::of(
                    &dimensions.iter().map(String::as_str).collect::<Vec<&str>>(),
                )
            })
            .collect()
    }

    /// Which lifetime it has.
    #[must_use]
    pub const fn lifetime(&self) -> Lifetime {
        self.definition.lifetime()
    }

    /// The cube's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.definition.name
    }

    /// The published fact table, or the declared query, as it was written.
    #[must_use]
    pub fn fact_table(&self) -> &str {
        &self.definition.fact_table
    }

    /// Every table this cube reads.
    #[must_use]
    pub fn reads(&self) -> &[String] {
        &self.definition.reads
    }

    /// Whether the fact source is a query rather than a table name.
    #[must_use]
    pub fn fact_is_a_query(&self) -> bool {
        self.definition.fact_is_a_query()
    }

    /// Its dimensions.
    #[must_use]
    pub fn dimensions(&self) -> &[Dimension] {
        &self.definition.dimensions
    }

    /// Its measures.
    #[must_use]
    pub fn measures(&self) -> &[Measure] {
        &self.definition.measures
    }

    /// The dimension of a given name, if it has one.
    #[must_use]
    pub fn dimension(&self, name: &str) -> Option<&Dimension> {
        self.definition.dimensions.iter().find(|d| d.name == name)
    }

    /// The measure of a given name, if it has one.
    #[must_use]
    pub fn measure(&self, name: &str) -> Option<&Measure> {
        self.definition.measures.iter().find(|m| m.name == name)
    }

    /// The definition's fingerprint, which is its version.
    ///
    /// Derived from the content, so it changes when the definition does and cannot be
    /// forgotten. It is half of a materialisation key --- see [`crate::version`].
    #[must_use]
    pub fn version(&self) -> u64 {
        self.version
    }

    /// The definition it was validated from.
    #[must_use]
    pub fn definition(&self) -> &Definition {
        &self.definition
    }

    /// Every dimension name, in declaration order.
    #[must_use]
    pub fn dimension_names(&self) -> Vec<&str> {
        self.definition.dimensions.iter().map(|d| d.name.as_str()).collect()
    }

    /// Which fact-table column each dimension joins on, keyed by dimension name.
    #[must_use]
    pub fn joins(&self) -> BTreeMap<&str, &str> {
        self.definition
            .dimensions
            .iter()
            .map(|d| (d.name.as_str(), d.joins_on.as_str()))
            .collect()
    }
}

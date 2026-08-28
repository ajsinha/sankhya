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

/// A cube as somebody wrote it down --- not yet checked, and not yet usable.
///
/// Separate from [`Cube`] on purpose. A single type for both would mean every function
/// taking a cube has to decide whether it trusts what it was handed, and some of them would
/// decide wrongly.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Definition {
    /// The cube's name.
    pub name: String,
    /// The published fact table.
    pub fact_table: String,
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

impl Definition {
    /// A definition. Nothing is checked here; call [`Definition::validate`].
    pub fn new(
        name: impl Into<String>,
        fact_table: impl Into<String>,
        dimensions: Vec<Dimension>,
        measures: Vec<Measure>,
    ) -> Self {
        Self {
            name: name.into(),
            fact_table: fact_table.into(),
            dimensions,
            measures,
            // Declared, not maintained. Persisting a definition is cheap; materialising is
            // storage and work, and a cube should not acquire either by being written down.
            target_lag: None,
        }
    }

    /// The same definition, held to a staleness target.
    #[must_use]
    pub fn maintained_within(mut self, versions: u64) -> Self {
        self.target_lag = Some(versions);
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

    /// Check it, and produce a cube.
    ///
    /// Returns **every** rejection, not the first. A definition with four undeclared
    /// measure rules should be fixable in one sitting; reporting them one per build is how
    /// a person stops reading the message and declares `Sum` for everything, which is the
    /// outcome this whole design exists to prevent.
    ///
    /// # Errors
    /// Returns the rejections. The list is non-empty and ordered for a stable diff.
    pub fn validate(self) -> Result<Cube, Vec<Rejection>> {
        let rejections = validate::inspect(&self);
        if !rejections.is_empty() {
            return Err(rejections);
        }
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

    /// The published fact table.
    #[must_use]
    pub fn fact_table(&self) -> &str {
        &self.definition.fact_table
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

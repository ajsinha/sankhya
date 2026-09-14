//! Members read from the dimension table.
//!
//! # What was missing, and what it cost
//!
//! [`hydrate`](crate::hydrate) reads member keys from the **fact table's** `joins_on`
//! column. That is the whole of what the cube knew about its members, so
//! `DIMENSION region FROM sales.regions ON region (LEVEL area = area, LEVEL city = city)`
//! never opened `sales.regions`: `Dimension::table` and `Level::column` were parsed,
//! fingerprinted, and read by nothing.
//!
//! Two things follow from that, and both are the kind of wrong this system is arranged
//! against.
//!
//! **A hierarchy in the data did not exist.** A star schema puts the roll-up in the
//! dimension table --- one row per city, carrying its country and its area --- and a cube
//! could not use it. Only a hierarchy typed out in the `CREATE CUBE` worked, which is fine
//! for a dozen edges and absurd for a customer table.
//!
//! **There was no referential check.** A fact-table key absent from the dimension table
//! became a member anyway. It is a real member with real money against it, it appears in
//! results, and the moment anything consolidates it has no parent --- so it sits beside the
//! parents as a row nobody can explain, or, in a consumer that groups by level, silently
//! becomes an area of its own.
//!
//! # Keys are what the key column holds
//!
//! `keys` is the values of the dimension table's *key* column, and nothing else. A name that
//! appears only as somebody's parent is a member of the hierarchy but not a joinable key, and
//! conflating the two would make a fact pointing at an aggregate look like a legitimate join.
//!
//! For a recursive dimension this costs nothing --- the child column *is* the primary key, so
//! a root appears as its own row with a null parent.
//!
//! # A dimension row with no key is counted, not skipped
//!
//! The same rule [`hydrate`](crate::hydrate) applies to facts. A null key in the dimension
//! table means a row that declares no member; placing it under `""` invents one, and dropping
//! it silently means a fact that legitimately has no match is reported as an orphan of a
//! table that was itself short. So it is counted and reported as [`Members::unkeyed`].

use crate::hydrate::NotHydratable;
use crate::model::Dimension;
use arrow_array::{Array, RecordBatch};
use sankhya_cube_algo::hierarchy::{Cyclic, Hierarchy};
use std::collections::BTreeSet;

/// Where a dimension table keeps its member keys and its parent links.
///
/// Derived from the definition rather than guessed. A dimension declares one of the two
/// forms, and the reader has to know which before it can name a column.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Shape<'a> {
    /// `PARENT <child> TO <parent>` --- keys in `child`, each row's parent in `parent`.
    ///
    /// The ragged form. Depth is not known until the table is read, which is the whole
    /// reason it is spelled differently from levels.
    Recursive {
        /// The key column, which is also the child end of every edge.
        child: &'a str,
        /// The column naming this row's parent. Null means a root.
        parent: &'a str,
    },
    /// `LEVEL … = <column>`, **coarse to fine** --- keys in the finest, links up the list.
    ///
    /// The star-schema form: one row per leaf, carrying every ancestor as a column.
    Levels(Vec<&'a str>),
}

/// Which columns of the dimension table hold its members, or `None` if it declares neither.
///
/// `PARENT` wins when both are present. It is the more specific statement --- a definition
/// that says exactly which column is the parent has said something the level list only
/// implies --- and choosing the other way would mean a cube with both could not express a
/// ragged hierarchy at all.
#[must_use]
pub fn shape_of(dimension: &Dimension) -> Option<Shape<'_>> {
    if let Some((child, parent)) = &dimension.parent_child {
        return Some(Shape::Recursive { child, parent });
    }
    if dimension.levels.is_empty() {
        return None;
    }
    Some(Shape::Levels(
        dimension.levels.iter().map(|level| level.column.as_str()).collect(),
    ))
}

/// What one dimension table says about its members.
///
/// Built a batch at a time by [`absorb_members`], so the peak cost is a batch rather than a
/// table --- the same reason `publish_from_fact_table` streams.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Members {
    keys: BTreeSet<String>,
    rollups: Hierarchy,
    edges: u64,
    rows: u64,
    unkeyed: u64,
}

impl Members {
    /// Nothing read yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the dimension table holds this member key.
    ///
    /// Case-sensitive, because a member key is data rather than an identifier. `FR` and `fr`
    /// are two rows of a table somebody loaded, and deciding they are the same member is a
    /// judgement this layer has no standing to make.
    #[must_use]
    pub fn has(&self, key: &str) -> bool {
        self.keys.contains(key)
    }

    /// Every member key the table holds.
    #[must_use]
    pub const fn keys(&self) -> &BTreeSet<String> {
        &self.keys
    }

    /// The roll-up read from the table.
    #[must_use]
    pub const fn rollups(&self) -> &Hierarchy {
        &self.rollups
    }

    /// Whether the table gave any parent link at all.
    ///
    /// A single-level dimension, or a recursive one where every row is a root, gives keys and
    /// no edges. That is a legitimate table and an unusable hierarchy, and the difference has
    /// to be sayable so a consolidation can refuse for the right reason.
    #[must_use]
    pub const fn describes_a_hierarchy(&self) -> bool {
        self.edges > 0
    }

    /// How many parent links were read.
    #[must_use]
    pub const fn edges(&self) -> u64 {
        self.edges
    }

    /// Rows read from the dimension table.
    #[must_use]
    pub const fn rows(&self) -> u64 {
        self.rows
    }

    /// Rows whose key column was null, and which therefore declare no member.
    #[must_use]
    pub const fn unkeyed(&self) -> u64 {
        self.unkeyed
    }

    /// Which of these members the dimension table does not have.
    ///
    /// Deliberately one direction. A dimension-table member no fact mentions is **normal**
    /// --- a region that sold nothing this month --- and reporting it would train everybody
    /// to ignore the figure. A fact key no dimension row has is a broken join.
    ///
    /// Computed against the members actually in hand rather than stored, so the answer cannot
    /// disagree with the cells it is about. Cells read from a materialised cuboid hold the
    /// members that cuboid holds, and those are the ones a consolidation will move.
    #[must_use]
    pub fn orphans_among<'a>(
        &self,
        produced: impl IntoIterator<Item = &'a str>,
    ) -> BTreeSet<&'a str> {
        produced.into_iter().filter(|member| !self.keys.contains(*member)).collect()
    }

    /// The hierarchy this table describes, checked for cycles.
    ///
    /// # Errors
    /// [`Cyclic`] when a member reaches itself. Checked once here rather than at every
    /// traversal: a cycle found during a query is an unbounded walk and a timeout that names
    /// nothing, and a cycle found at hydration names the member.
    pub fn validate(&self) -> Result<(), Cyclic> {
        self.rollups.validate()
    }

}

/// Read one batch of a dimension table into `into`.
///
/// # Errors
/// [`NotHydratable`] when a column the definition names is absent from the dimension table,
/// or holds a type no member key can be read from. Refused rather than skipped, for the same
/// reason a missing fact column is: a dimension table read as empty makes every fact an
/// orphan, and a referential check that fails wholesale teaches people to turn it off.
pub fn absorb_members(
    dimension: &Dimension,
    batch: &RecordBatch,
    into: &mut Members,
) -> Result<(), NotHydratable> {
    let Some(shape) = shape_of(dimension) else {
        return Ok(());
    };
    into.rows = into.rows.saturating_add(batch.num_rows() as u64);
    match shape {
        Shape::Recursive { child, parent } => {
            let keys = column(dimension, batch, child)?;
            let parents = column(dimension, batch, parent)?;
            for row in 0..batch.num_rows() {
                let Some(key) = key_at(keys, row) else {
                    into.unkeyed = into.unkeyed.saturating_add(1);
                    continue;
                };
                into.keys.insert(key.clone());
                // A null parent is a root, not an error. A recursive hierarchy has to have
                // at least one, and the table says so by leaving the column empty.
                if let Some(above) = key_at(parents, row) {
                    if above != key {
                        into.rollups.rolls_up(key, above);
                        into.edges = into.edges.saturating_add(1);
                    }
                }
            }
        }
        Shape::Levels(columns) => {
            // Coarse to fine, as the definition lists them --- so the key is the last, and
            // every edge runs from a column to the one before it.
            let mut read = Vec::with_capacity(columns.len());
            for name in &columns {
                read.push(column(dimension, batch, name)?);
            }
            let Some(finest) = read.last().copied() else {
                return Ok(());
            };
            for row in 0..batch.num_rows() {
                let Some(key) = key_at(finest, row) else {
                    into.unkeyed = into.unkeyed.saturating_add(1);
                    continue;
                };
                into.keys.insert(key);
                // A null at some level is a **ragged** branch, not a broken row: the edge
                // to that level is simply absent, and the level below joins to whatever the
                // next non-null ancestor is. Padding it with a placeholder is the flattening
                // `FR-QUERY-11` forbids, and skipping the whole row would lose a real member.
                let mut below: Option<String> = None;
                for level in read.iter().rev() {
                    let Some(here) = key_at(*level, row) else {
                        continue;
                    };
                    if let Some(child) = below {
                        if child != here {
                            into.rollups.rolls_up(child, here.clone());
                            into.edges = into.edges.saturating_add(1);
                        }
                    }
                    below = Some(here);
                }
            }
        }
    }
    Ok(())
}

/// One member key, or `None` where the column is null.
///
/// The null check is **here** and not in [`member_at`](crate::hydrate::member_at), which
/// renders whatever the array holds --- for a null `Utf8` that is `""`.
///
/// Its fact-table caller tests nullness first, in `address_of`, so the omission was invisible
/// until a second caller existed. This one read a ragged dimension row and filed its gap under
/// the empty string: a member named `""` that is in no dimension table, appears in results and
/// in drill-downs, and has real money against it --- the exact invention the module header
/// says a null key must not become. Caught by the ragged test, which is the only fixture in
/// which a null reaches this at all.
fn key_at(column: &dyn Array, row: usize) -> Option<String> {
    if column.is_null(row) {
        return None;
    }
    crate::hydrate::member_at(column, row)
}

/// One column of the dimension table, named by the definition.
fn column<'a>(
    dimension: &Dimension,
    batch: &'a RecordBatch,
    name: &str,
) -> Result<&'a dyn Array, NotHydratable> {
    let found = batch.column_by_name(name).ok_or_else(|| NotHydratable::MissingMemberColumn {
        dimension: dimension.name.clone(),
        table: dimension.table.clone(),
        column: name.to_string(),
        found: batch.schema().fields().iter().map(|f| f.name().clone()).collect(),
    })?;
    if !crate::hydrate::readable_key(found.data_type()) {
        return Err(NotHydratable::UnreadableKey {
            column: name.to_string(),
            found: found.data_type().to_string(),
        });
    }
    Ok(found.as_ref())
}

//! The set of tables a reclamation decision has to consult instead of one.
//!
//! # What a sweeper of one table root actually needs
//!
//! A clone's inherited files stay where they are, under the **origin's** root. Files the clone
//! writes afterwards go under its own. So for a sweep of table `T`'s root, the tables that can
//! still name a file there are `T` itself and everything cloned from `T`, transitively.
//!
//! Ancestors are not in that set and it is worth saying why, because "the whole family" is the
//! intuitive answer and it is wider than necessary. If `T` is itself a clone of `O`, the files
//! `T` inherited are under `O`'s root and `O`'s sweeper is responsible for them; `O` cannot name
//! a file `T` wrote after the split, because `O` never heard of it. [`Lineages::readers_of`] is
//! therefore descendants and self, and nothing more.
//!
//! [`Lineages::family`] is the wider set, and exists for the questions that genuinely need it:
//! whether a table may be dropped, and what a backup of a clone would have to include.
//!
//! # A cycle cannot happen and is refused anyway
//!
//! A clone's origin exists before the clone, so lineage is a tree by construction. It is a tree
//! *in the log*, which is a file, which somebody can edit. A resolver that trusted the
//! construction would hang rather than fail, and a sweep that hangs stops every reclamation in
//! the warehouse until somebody notices --- so the walk carries a visited set and reports the
//! cycle instead of following it.

use crate::lineage::Lineage;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// Which tables were cloned from which.
///
/// Only clones appear. A table with no entry is an ordinary table, which is every table that
/// exists at the time of writing --- and the reason the whole mechanism costs nothing until
/// somebody clones something.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Lineages {
    by_table: BTreeMap<String, Lineage>,
}

impl Lineages {
    /// Nothing has been cloned.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that `table` is a clone.
    pub fn record(&mut self, table: impl Into<String>, lineage: Lineage) {
        self.by_table.insert(table.into(), lineage);
    }

    /// What `table` was cloned from, if it is a clone.
    #[must_use]
    pub fn of(&self, table: &str) -> Option<&Lineage> {
        self.by_table.get(table)
    }

    /// Whether anything has been cloned at all.
    ///
    /// The fast path every existing deployment takes: no clones, so no reclamation decision
    /// changes, so nothing pays for a feature nobody is using.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_table.is_empty()
    }

    /// Every table that could still name a file under `table`'s root: itself and its clones,
    /// transitively.
    ///
    /// This is the set a sweep consults. For a table nobody has cloned it is the table alone,
    /// and the sweep behaves exactly as it does today.
    ///
    /// # Errors
    ///
    /// [`Cycle`] when the lineage records form one, which they cannot by construction and can by
    /// editing. Refused rather than followed, because a sweep that hangs stops every reclamation
    /// in the warehouse until somebody notices.
    pub fn readers_of(&self, table: &str) -> Result<BTreeSet<String>, Cycle> {
        let mut reached = BTreeSet::from([table.to_string()]);
        let mut frontier = vec![table.to_string()];

        while let Some(current) = frontier.pop() {
            for (candidate, lineage) in &self.by_table {
                if lineage.origin != current || reached.contains(candidate) {
                    continue;
                }
                reached.insert(candidate.clone());
                frontier.push(candidate.clone());
            }
        }

        // The walk above cannot hang --- the visited set sees to that --- so it would happily
        // return a set assembled from records that contradict themselves. The upward check is
        // what refuses instead, because a table whose own ancestry loops has properties somebody
        // edited, and a reclamation decision taken from those is a decision taken on nonsense.
        self.ancestors(table)?;
        Ok(reached)
    }

    /// The chain from `table` up to the table it ultimately descends from, nearest first.
    ///
    /// # Errors
    ///
    /// [`Cycle`] when following origins revisits a table.
    pub fn ancestors(&self, table: &str) -> Result<Vec<String>, Cycle> {
        let mut chain = Vec::new();
        let mut seen = BTreeSet::from([table.to_string()]);
        let mut current = table.to_string();

        while let Some(lineage) = self.by_table.get(&current) {
            if !seen.insert(lineage.origin.clone()) {
                return Err(Cycle { at: lineage.origin.clone() });
            }
            chain.push(lineage.origin.clone());
            current = lineage.origin.clone();
        }
        Ok(chain)
    }

    /// Every table sharing an origin with `table`: the root it descends from, and everything
    /// descended from that root.
    ///
    /// Wider than [`Self::readers_of`], and used for the questions that need it --- whether a
    /// table may be dropped, and what a backup of a clone must include.
    ///
    /// # Errors
    ///
    /// [`Cycle`] as above.
    pub fn family(&self, table: &str) -> Result<BTreeSet<String>, Cycle> {
        let root = self
            .ancestors(table)?
            .last()
            .cloned()
            .unwrap_or_else(|| table.to_string());
        self.readers_of(&root)
    }

    /// The versions of `table` a clone still reads, and which its sweeper must therefore keep
    /// the live set of.
    ///
    /// # Why only direct clones, which is less than it looks like it should be
    ///
    /// `ADR-0016`'s Decision 1a means a clone's log names none of its origin's files: it records
    /// an origin and a version, and a read splices the origin's live set *at that version* with
    /// the clone's own log. So a clone of a clone pins a version of the **intermediate** table,
    /// not of the root --- the intermediate's files are protected by the intermediate's own
    /// sweeper, and the intermediate's dependence on the root is expressed by the intermediate's
    /// own pin.
    ///
    /// Transitivity therefore does not compound here, and the sweep does not need the
    /// [`Self::readers_of`] walk at all. That walk is for the questions that genuinely span the
    /// tree: whether a table may be dropped, and what a backup must include.
    ///
    /// Empty for a table nobody has cloned, which is every table that exists --- the sweep then
    /// does exactly what it does today.
    ///
    /// # The naming, which was wrong in every deployment
    ///
    /// A lineage records the **qualified** name --- `sales.orders` --- because that is what a
    /// person typed and what the catalogue resolves. The sweeper asks from the other end: it
    /// has a directory, and the only name a directory carries is its own. It asked for
    /// `orders`, `origin == table` was false, and the answer was *"no clone reads this"* ---
    /// not sometimes, but on every table in every warehouse, because `discover` only ever
    /// walks `<warehouse>/<schema>/<table>`. A clone's files then aged past the grace period
    /// and were deleted, and `SELECT` from the clone read short with no error.
    ///
    /// It survived because the one test that covered it built its table at the warehouse root
    /// and recorded a bare name --- the single shape in which the two forms cannot disagree.
    #[must_use]
    pub fn pinned_versions(&self, table: &str) -> BTreeSet<u64> {
        self.by_table
            .values()
            .filter(|lineage| same_table(&lineage.origin, table))
            .map(|lineage| lineage.version)
            .collect()
    }

    /// Which clones keep which of `table`'s versions alive, by version.
    ///
    /// [`Self::pinned_versions`] answers *"which versions must survive"*, which is all a sweep
    /// needs. This answers *"and what is holding each one"*, which is what a person needs:
    /// somebody looking at a pinned version is deciding what to drop to release it, and a
    /// number with no name attached tells them nothing they can act on.
    #[must_use]
    pub fn keepers_of(&self, table: &str) -> BTreeMap<u64, BTreeSet<String>> {
        let mut keepers: BTreeMap<u64, BTreeSet<String>> = BTreeMap::new();
        for (clone, lineage) in &self.by_table {
            if lineage.origin == table {
                keepers.entry(lineage.version).or_default().insert(clone.clone());
            }
        }
        keepers
    }

    /// The clones that would be broken by removing `table`.
    ///
    /// # Errors
    ///
    /// [`Cycle`] as above.
    pub fn dependents(&self, table: &str) -> Result<BTreeSet<String>, Cycle> {
        let mut readers = self.readers_of(table)?;
        readers.remove(table);
        Ok(readers)
    }
}

/// Whether two names name one table, when one of them may be unqualified.
///
/// # Why this is not `==`
///
/// The two ends of the reclamation question hold the name in different forms. A lineage is
/// written from a statement and is qualified; a sweeper reads a directory and has only the
/// leaf. Comparing them directly answers *no* for every clone that has ever been made, which
/// is `COR-01`.
///
/// Both qualified, or both bare, is an exact comparison and stays one --- `sales.orders` and
/// `hr.orders` are different tables and must not be conflated. Only the mixed case falls back
/// to the table half, and it falls back in the direction that **keeps files**: an unqualified
/// name carries no schema to disagree with, so treating it as a match can pin a file nothing
/// reads, and can never fail to pin one something does.
fn same_table(origin: &str, asked: &str) -> bool {
    if origin == asked {
        return true;
    }
    match (origin.rsplit_once('.'), asked.rsplit_once('.')) {
        // Both carry a schema, or neither does. `==` above was the whole answer.
        (Some(_), Some(_)) | (None, None) => false,
        (Some((_, leaf)), None) => leaf == asked,
        (None, Some((_, leaf))) => leaf == origin,
    }
}

/// The lineage records form a cycle.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Cycle {
    /// A table on the cycle.
    pub at: String,
}

impl fmt::Display for Cycle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the clone lineage forms a cycle at `{}`. A clone's origin exists before the clone, \
             so this cannot arise from cloning --- it means a table's properties were edited. \
             Refused rather than followed: a sweep that walked it would hang, and a sweep that \
             hangs stops every reclamation in the warehouse until somebody notices",
            self.at
        )
    }
}

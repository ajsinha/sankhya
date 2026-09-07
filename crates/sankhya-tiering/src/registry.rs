//! The archival registry: what was archived, where, from which snapshot, and until when.
//!
//! # What the registry is authority for
//!
//! `FR-TIER-16` divides the question in two. The source catalog is authority for the **hot
//! extent** --- what is still attached --- and the registry is authority for the **cold
//! extent**. Neither is authority for both, and the reason is that they are written by
//! different things at different times: a partition is detached by a purge and the catalog
//! notices, while the archive was written before the detach and nothing in the catalog ever
//! knew about it.
//!
//! Reading one and inferring the other is how a range comes to be served twice or not at all.
//!
//! # Why an entry cannot overlap another
//!
//! Two entries covering the same range of the same table are two claims about where that data
//! is. There is no rule for choosing between them that is not a guess, and the guess is made at
//! query time, when nobody is watching. So [`Registry::record`] refuses the overlap at the
//! moment it would be created, which is the moment somebody can still explain it.
//!
//! # Why a range is a pair of ordinals rather than two values
//!
//! Coverage is an ordering question --- does this range have a hole in it --- and the canonical
//! encoding [`crate::encode`] produces answers equality questions, not ordering ones. A
//! big-endian `i64` sorts wrongly across zero, and that is exactly the sort of defect that
//! shows up as a coverage gap nobody can reproduce.
//!
//! So a range is `[from, until)` over the tiering key's **ordinal**: days for a date,
//! microseconds for a timestamp, the value itself for an integer. A tiering key with no ordinal
//! is refused at policy creation ([`crate::policy::Ineligible::TieringKeyNotOrdinal`]), because
//! a range that cannot be ordered is a range that cannot be shown to be covered.

use crate::policy::Retention;
use crate::verify::{ColumnDigest, Hash};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// A half-open range `[from, until)` over the tiering key's ordinal.
///
/// Half-open because the alternative is an off-by-one nobody notices: with inclusive bounds,
/// two adjacent partitions either overlap on a day or leave a hole on one, and which of the two
/// happened depends on who wrote the second entry.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Range {
    /// Inclusive lower bound.
    pub from: i64,
    /// Exclusive upper bound.
    pub until: i64,
}

impl Range {
    /// A range.
    #[must_use]
    pub const fn new(from: i64, until: i64) -> Self {
        Self { from, until }
    }

    /// Whether it covers nothing.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.until <= self.from
    }

    /// Whether `at` falls inside it.
    #[must_use]
    pub const fn contains(&self, at: i64) -> bool {
        at >= self.from && at < self.until
    }

    /// Whether the two share any point.
    #[must_use]
    pub const fn intersects(&self, other: &Self) -> bool {
        !self.is_empty() && !other.is_empty() && self.from < other.until && other.from < self.until
    }
}

impl fmt::Display for Range {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}, {})", self.from, self.until)
    }
}

/// One archived range.
///
/// # Why the digests are here
///
/// `FR-TIER-12` requires the entry to be mirrored to write-once storage before the detach, and
/// an entry that records only a location is not worth mirroring: it proves nothing about what
/// is at that location. The digests are what an operator years later compares against, when the
/// source is gone and the only remaining question is whether the archive is what was purged.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Entry {
    /// The table the range was purged from.
    pub table: String,
    /// The range, over the tiering key's ordinal.
    pub range: Range,
    /// Where the archive is.
    pub archive: String,
    /// The source snapshot the copy was verified against.
    ///
    /// Pinned by [`Registry::pins`] until the retention basis lapses: without the snapshot
    /// there is nothing left to re-verify the archive against.
    pub snapshot: String,
    /// Rows archived.
    pub rows: u64,
    /// The primary-key Merkle root the verification agreed on.
    pub keys: Hash,
    /// The per-column digests the verification agreed on.
    pub columns: Vec<ColumnDigest>,
    /// When it was archived, in microseconds from the epoch.
    pub archived_at: i64,
    /// Why it must be kept, and for how long.
    pub retention: Retention,
    /// Whether a hold overrides the retention basis.
    pub legal_hold: bool,
    /// Who authorised the purge, carried from the journal.
    pub attribution: Vec<(String, String)>,
}

/// Microseconds in a day.
const DAY: i64 = 86_400 * 1_000_000;

impl Entry {
    /// When the retention basis lapses, in microseconds from the epoch.
    #[must_use]
    pub fn retained_until(&self) -> i64 {
        self.archived_at.saturating_add(i64::from(self.retention.days).saturating_mul(DAY))
    }

    /// Whether this entry still obliges anything to be kept at `now`.
    ///
    /// A legal hold outlives the retention basis by construction: a hold with an end date is a
    /// retention basis, and the ones that matter do not have one.
    #[must_use]
    pub fn binding_at(&self, now: i64) -> bool {
        self.legal_hold || now < self.retained_until()
    }

    /// What is written to write-once storage before the detach.
    ///
    /// `FR-TIER-12`. Deliberately `key=value`, one per line, rather than a serialisation format.
    /// The reader is a person with a copy of an object store and no build of this software, and
    /// a format that needs a parser is a format that needs a *version* of the parser --- which
    /// is the one thing that cannot be relied on years later.
    ///
    /// It is nonetheless machine-readable, because `FR-TIER-35` requires the evidence pack to be
    /// generatable from this alone. Lines rather than a single line so that a value containing
    /// spaces --- a retention basis is a sentence --- needs no escaping, and the first `=` on a
    /// line is the separator so a value containing one needs none either. An unknown key is
    /// ignored by [`crate::evidence::Pack::from_marker`], so adding a field later does not
    /// break a reader written today.
    #[must_use]
    pub fn marker(&self) -> String {
        let mut lines = vec![
            format!("table={}", self.table),
            format!("range={}", self.range),
            format!("archive={}", self.archive),
            format!("snapshot={}", self.snapshot),
            format!("rows={}", self.rows),
            format!("keys={}", self.keys),
            format!("archived_at={}", self.archived_at),
            format!("retention_days={}", self.retention.days),
            format!("retention_basis={}", self.retention.basis),
            format!("legal_hold={}", self.legal_hold),
        ];
        for (key, value) in &self.attribution {
            lines.push(format!("attribution.{key}={value}"));
        }
        lines.join("\n")
    }
}

/// Why an entry was not recorded.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Rejected {
    /// Another entry already covers part of this range.
    Overlaps {
        /// The range being recorded.
        offered: Range,
        /// The range already recorded.
        existing: Range,
    },
    /// The range covers nothing.
    EmptyRange {
        /// What was offered.
        offered: Range,
    },
}

impl fmt::Display for Rejected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Overlaps { offered, existing } => write!(
                f,
                "{offered} overlaps the archived range {existing}: two entries covering the \
                 same rows are two claims about where those rows are, and choosing between \
                 them at query time is a guess made where nobody is watching"
            ),
            Self::EmptyRange { offered } => {
                write!(f, "{offered} covers nothing, and an archive of nothing is not a record")
            }
        }
    }
}

/// Everything that has been archived.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Registry {
    entries: Vec<Entry>,
}

impl Registry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuild from entries, as a restore does.
    ///
    /// # Errors
    ///
    /// [`Rejected`] on the first entry that cannot be recorded, so a restore that would produce
    /// an ambiguous registry fails rather than serving from it.
    pub fn from_entries(entries: Vec<Entry>) -> Result<Self, Rejected> {
        let mut registry = Self::new();
        for entry in entries {
            registry.record(entry)?;
        }
        Ok(registry)
    }

    /// Record an archived range.
    ///
    /// # Errors
    ///
    /// [`Rejected::Overlaps`] if another entry for the same table already covers part of it,
    /// and [`Rejected::EmptyRange`] if it covers nothing.
    pub fn record(&mut self, entry: Entry) -> Result<(), Rejected> {
        if entry.range.is_empty() {
            return Err(Rejected::EmptyRange { offered: entry.range });
        }
        if let Some(existing) = self
            .entries
            .iter()
            .find(|other| other.table == entry.table && other.range.intersects(&entry.range))
        {
            return Err(Rejected::Overlaps { offered: entry.range, existing: existing.range });
        }
        self.entries.push(entry);
        self.entries.sort_by(|left, right| {
            (&left.table, left.range).cmp(&(&right.table, right.range))
        });
        Ok(())
    }

    /// Every entry, in table then range order.
    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Remove the entry covering exactly this range, returning it.
    ///
    /// # Why this is not public policy on its own
    ///
    /// Withdrawing an entry says *"this range is no longer archived"*, and the only occasion on
    /// which that is true is a re-attachment from quarantine --- which must withdraw it, because
    /// a range the registry still claims and the catalog has attached is the disagreement
    /// [`crate::unify`] has to flag. So this exists to be called by
    /// [`crate::quarantine::Quarantine::reattach`], which does both halves in one call and
    /// leaves no state in which one happened without the other.
    pub(crate) fn withdraw(&mut self, table: &str, range: Range) -> Option<Entry> {
        let at = self
            .entries
            .iter()
            .position(|entry| entry.table == table && entry.range == range)?;
        Some(self.entries.remove(at))
    }

    /// Which archives answer a range of a table, and what is not covered.
    #[must_use]
    pub fn coverage(&self, table: &str, wanted: Range) -> Coverage {
        let covering: Vec<&Entry> = self
            .entries
            .iter()
            .filter(|entry| entry.table == table && entry.range.intersects(&wanted))
            .collect();

        // Walk the wanted range left to right, closing over each entry in turn. Whatever the
        // walk steps over is a hole, and a hole is reported rather than assumed hot: a range
        // nobody claims is a range nobody can answer for.
        let mut gaps = Vec::new();
        let mut at = wanted.from;
        for entry in &covering {
            if entry.range.from > at {
                gaps.push(Range::new(at, entry.range.from.min(wanted.until)));
            }
            at = at.max(entry.range.until);
        }
        if at < wanted.until && !wanted.is_empty() {
            gaps.push(Range::new(at, wanted.until));
        }

        Coverage { archives: covering.into_iter().cloned().collect(), gaps }
    }

    /// The snapshots expiry may not remove.
    ///
    /// `FR-TIER-22`. A snapshot referenced by an entry whose retention has not lapsed is the
    /// only thing left that can re-verify the archive, so expiring it turns "the archive is
    /// provably the data" into "the archive is what we have".
    #[must_use]
    pub fn pins(&self, now: i64) -> Pins {
        Pins {
            snapshots: self
                .entries
                .iter()
                .filter(|entry| entry.binding_at(now))
                .map(|entry| entry.snapshot.clone())
                .collect(),
        }
    }

    /// Compare the registry against what the catalog says is attached.
    ///
    /// `FR-TIER-23`, run on startup and after any restore.
    #[must_use]
    pub fn reconcile(&self, attached: &[(String, Range)]) -> Reconciliation {
        let mut conflicts = Vec::new();
        for entry in &self.entries {
            for (table, hot) in attached {
                if *table == entry.table && hot.intersects(&entry.range) {
                    conflicts.push(Conflict::InBothTiers {
                        table: entry.table.clone(),
                        cold: entry.range,
                        hot: *hot,
                    });
                }
            }
        }
        // The union of what each authority named: the archived entries, and the hot ranges
        // the catalog supplied. A table in neither was not examined, and saying so is the
        // whole of the fix above.
        let mut examined: BTreeSet<String> = self.entries.iter().map(|e| e.table.clone()).collect();
        examined.extend(attached.iter().map(|(table, _)| table.clone()));
        Reconciliation { conflicts, examined }
    }

    /// What changed between two registries.
    ///
    /// `FR-TIER-24`: restore reports the archive delta **before serving traffic**, because a
    /// restore that silently loses an entry is a restore that silently un-archives a range ---
    /// and the first evidence would be a query answering from a source that no longer holds it.
    #[must_use]
    pub fn delta(&self, since: &Self) -> Delta {
        let key = |entry: &Entry| (entry.table.clone(), entry.range);
        let mine: BTreeMap<(String, Range), &Entry> =
            self.entries.iter().map(|entry| (key(entry), entry)).collect();
        let theirs: BTreeMap<(String, Range), &Entry> =
            since.entries.iter().map(|entry| (key(entry), entry)).collect();

        let mut delta = Delta::default();
        for (id, entry) in &mine {
            match theirs.get(id) {
                None => delta.added.push((*entry).clone()),
                Some(before) if *before != *entry => delta.changed.push((*entry).clone()),
                Some(_) => {}
            }
        }
        for (id, entry) in &theirs {
            if !mine.contains_key(id) {
                delta.removed.push((*entry).clone());
            }
        }
        delta
    }
}

/// What the registry can answer for a range, and what it cannot.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Coverage {
    /// The archives intersecting the wanted range.
    pub archives: Vec<Entry>,
    /// Sub-ranges no entry claims.
    ///
    /// `FR-TIER-17` makes an uncovered range intersecting the predicate a **coverage-gap
    /// error** rather than an empty result, because a query that quietly returns fewer rows
    /// than exist is the failure tiering is most able to cause and least able to detect.
    pub gaps: Vec<Range>,
}

impl Coverage {
    /// Whether every point of the wanted range is claimed by some archive.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.gaps.is_empty()
    }
}

/// Snapshots that may not be expired.
///
/// # Why this is a value rather than a check
///
/// `FR-TIER-22` says expiry must be **structurally incapable** of removing a referenced
/// snapshot. A function that consults the registry can be called with the consultation skipped;
/// a function that takes a `Pins` cannot be called without one, and the only way to obtain one
/// is [`Registry::pins`]. The registry is therefore not something expiry remembers to ask ---
/// it is something expiry cannot run without.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Pins {
    snapshots: BTreeSet<String>,
}

impl Pins {
    /// Whether a snapshot may be expired.
    ///
    /// # Errors
    ///
    /// [`Pinned`] naming the snapshot, so a sweep's report says what it kept and why.
    pub fn may_expire(&self, snapshot: &str) -> Result<(), Pinned> {
        if self.snapshots.contains(snapshot) {
            return Err(Pinned { snapshot: snapshot.to_string() });
        }
        Ok(())
    }

    /// How many snapshots are pinned.
    #[must_use]
    pub fn len(&self) -> usize {
        self.snapshots.len()
    }

    /// Whether nothing is pinned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.snapshots.is_empty()
    }
}

/// A snapshot an archival entry still depends on.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Pinned {
    /// Which snapshot.
    pub snapshot: String,
}

impl fmt::Display for Pinned {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the snapshot `{}` is referenced by an archival registry entry whose retention has \
             not lapsed; expiring it would leave the archive with nothing left to verify it \
             against",
            self.snapshot
        )
    }
}

/// A disagreement between the registry and the catalog.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Conflict {
    /// A range the registry believes cold and the catalog shows attached.
    InBothTiers {
        /// The table.
        table: String,
        /// What the registry claims.
        cold: Range,
        /// What the catalog shows.
        hot: Range,
    },
}

impl fmt::Display for Conflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InBothTiers { table, cold, hot } => write!(
                f,
                "`{table}`: the registry claims {cold} is archived and the catalog shows {hot} \
                 attached. Serving either one is a plausible wrong answer, and serving both is \
                 a duplicate"
            ),
        }
    }
}

/// The result of reconciling the registry against the catalog.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Reconciliation {
    /// Every disagreement found.
    pub conflicts: Vec<Conflict>,
    /// Every table this reconciliation actually looked at.
    ///
    /// # Why an absence of conflicts was not enough
    ///
    /// Without this, `servable` was `conflicts.iter().all(...)` --- and `all` over an empty
    /// vector is `true`. So a `Reconciliation` produced by a run that examined **nothing**
    /// handed out a witness for every table anybody asked about, which is precisely the
    /// forgetting the witness type exists to make impossible. A planner on a process that had
    /// never reconciled would get `Ok`, union both tiers, and serve the plausible wrong answer
    /// `FR-TIER-23` is written to prevent.
    ///
    /// Both docs said otherwise --- *"a table nobody reconciled is not servable either"* ---
    /// and the crate's own test helper proved it false in one line: `reconcile(&[])` followed
    /// by `.servable("entries").expect("nothing was found")`.
    pub examined: BTreeSet<String>,
}

impl Reconciliation {
    /// Whether a table may be served by a unified query.
    ///
    /// `FR-TIER-23` requires a conflict to make unified queries on the affected table fail with
    /// a typed error **rather than serving a plausible wrong answer**. So this is a witness
    /// rather than a boolean: a planner that wants to union the two tiers has to hold a
    /// [`Servable`], and the only source of one is a reconciliation that **examined** this
    /// table and had nothing to say about it. A table nobody reconciled is not servable
    /// either, which is the correct answer for a process that has not run the check yet ---
    /// and which this returned the opposite of until [`Reconciliation::examined`] existed.
    #[must_use]
    pub fn servable(&self, table: &str) -> Option<Servable> {
        // Examined **and** unconflicted. A check that has not run is not evidence that it
        // would pass, and `all` over an empty conflict list says nothing at all.
        let looked = self.examined.contains(table);
        let clean = self.conflicts.iter().all(|conflict| match conflict {
            Conflict::InBothTiers { table: affected, .. } => affected != table,
        });
        (looked && clean).then(|| Servable { table: table.to_string() })
    }

    /// The same witness, and the typed refusal when there is none.
    ///
    /// # Why this exists beside [`Self::servable`]
    ///
    /// `FR-TIER-23` requires a conflict to make unified queries on the affected table fail
    /// **with a typed error**. What it actually produced was `Option::None` --- and a `None`
    /// is not an error: it carries no code, no remediation and no name for what went wrong,
    /// so the caller has to invent all three or drop them. `SNK-S0002` was published as the
    /// code for exactly this and nothing could raise it.
    ///
    /// `Unservable::NotReconciled` was worse than absent: it was declared, and
    /// `unify::plan`'s `# Errors` section said it was returned "when the witness is for
    /// another table" --- which `plan` cannot detect, because it takes the table *from* the
    /// witness. A documented error path that the function could not take.
    ///
    /// # Errors
    ///
    /// [`Unservable::NotReconciled`] when the reconciliation found this table in both tiers,
    /// and when nothing reconciled it at all. Those are deliberately the same refusal: a
    /// check that has not run is not evidence that it would pass. Both arms are exercised ---
    /// the second one only became reachable when `servable` started requiring examination.
    pub fn servable_or_refuse(&self, table: &str) -> Result<Servable, crate::unify::Unservable> {
        self.servable(table)
            .ok_or_else(|| crate::unify::Unservable::NotReconciled {
                table: table.to_string(),
            })
    }

    /// The tables a unified query must refuse.
    #[must_use]
    pub fn refused(&self) -> BTreeSet<String> {
        self.conflicts
            .iter()
            .map(|conflict| match conflict {
                Conflict::InBothTiers { table, .. } => table.clone(),
            })
            .collect()
    }
}

/// Evidence that a table's tiers agree, and may therefore be unioned.
///
/// Not `Clone` or `Copy`: it is evidence about one reconciliation, and a copy kept across a
/// later restore would assert something nobody checked.
#[derive(Debug)]
pub struct Servable {
    table: String,
}

impl Servable {
    /// Which table.
    #[must_use]
    pub fn table(&self) -> &str {
        &self.table
    }
}

/// What changed between two registries.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Delta {
    /// Entries present now and not before.
    pub added: Vec<Entry>,
    /// Entries present before and not now.
    ///
    /// The dangerous half. An entry that vanished across a restore is a range the system now
    /// believes was never archived.
    pub removed: Vec<Entry>,
    /// Entries whose recorded facts differ.
    pub changed: Vec<Entry>,
}

impl Delta {
    /// Whether the two registries agree.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }
}

impl fmt::Display for Delta {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty() {
            return f.write_str("the archival registry is unchanged");
        }
        write!(
            f,
            "archival registry delta: {} added, {} removed, {} changed",
            self.added.len(),
            self.removed.len(),
            self.changed.len()
        )
    }
}

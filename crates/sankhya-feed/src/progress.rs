//! How far a feed has got, recorded where the rows are.
//!
//! # Committed with the rows, or not at all
//!
//! A feed that publishes rows and then records its position has two commits and a gap between
//! them. A crash in the gap leaves either rows nobody knows arrived --- which a restart
//! duplicates --- or a position ahead of the data, which a restart skips. Which one you get
//! depends on the order somebody chose, and both are silent.
//!
//! So the position is a **table property**, written in the same commit as the `add` actions
//! that publish the rows. Either both are visible or neither is, and a restart becomes a
//! question with an answer rather than a reconciliation exercise. It is the same mechanism
//! `M10` used to record a clone's lineage, for the same reason: a fact about a table belongs
//! in that table's log.
//!
//! # Why a high-water mark rather than a list of what is done
//!
//! *"Which sources have been ingested?"* answered as a set is a set that grows for as long as
//! the feed runs, held in a table property, rewritten on every commit. It is also the wrong
//! shape for the question a restart actually asks, which is *"what next?"*.
//!
//! A feed reads its directory in **name order** and records the last source it finished.
//! Everything sorting at or before that is done. This is bounded, it is one string, and it
//! makes the ordering explicit rather than incidental --- a feed whose files are
//! `2026-08-30.json`, `2026-08-31.json` is already in the order it wants, and one whose files
//! are not has a naming problem that this makes visible rather than hides.
//!
//! The cost is that a file arriving late --- sorting before the mark --- is **refused** rather
//! than ingested. That is the right answer to an ambiguous event: a source appearing behind
//! the mark means either a producer wrote it out of order or somebody replayed an old file,
//! and those want opposite responses. Refusing names it for whoever can tell.

use serde::{Deserialize, Serialize};

/// The property a feed records its position under.
///
/// Namespaced by feed, so two feeds landing in one table --- which is allowed, and is how a
/// table is fed from two directories --- do not overwrite each other's progress.
#[must_use]
pub fn key(feed: &str) -> String {
    format!("sankhya.feed.{feed}.position")
}

/// How far a feed has got.
#[derive(Clone, PartialEq, Eq, Debug, Default, Deserialize, Serialize)]
pub struct Position {
    /// The last source read to completion. Everything sorting at or before it is done.
    ///
    /// Empty before the first source finishes, which sorts before every real name.
    #[serde(default)]
    pub through: String,
    /// The source part-way through, and how many of its records have been published.
    ///
    /// `None` between sources. Present only when a run stopped --- or died --- with a source
    /// half read, which is exactly the case a restart has to get right.
    #[serde(default)]
    pub partial: Option<Partial>,
}

/// A source that is part-way read.
#[derive(Clone, PartialEq, Eq, Debug, Deserialize, Serialize)]
pub struct Partial {
    /// Which source.
    pub source: String,
    /// How many of its **lines** have been read. The next line to read is this one.
    ///
    /// # Lines read, not records published
    ///
    /// This was `records`, and it held the count of records *published* while the resume
    /// skipped by *line index*. Those are the same number only when every line so far fitted
    /// and there were no blanks — so a source of `[good, bad, good]` published two, recorded
    /// two, and on restart skipped lines 0 and 1 and resumed at line 2, **which it had already
    /// published**. One duplicate row per preceding quarantined or blank line, silently.
    ///
    /// `ADR-0018`'s amendment chose *never re-ingest* over *never duplicate*, on the grounds
    /// that duplication is silent and permanent. This produced exactly the outcome the ADR
    /// ruled out, and the test that existed asserted the conflation in its own name.
    ///
    /// The serialised key is unchanged, so a position written by an older build is still read
    /// — as a line count, which it under-states. A restart across that one upgrade therefore
    /// re-reads at most the refusals-and-blanks so far in the single source that was part-way
    /// at the time. That is bounded and one-off, where the defect it replaces was neither.
    #[serde(rename = "records")]
    pub read_through: u64,
}

/// What a source is, relative to a position.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Standing {
    /// Never seen. Read it from the beginning.
    Fresh,
    /// Part-read. Skip this many records and carry on.
    Resume(u64),
    /// At or below the mark: read already, as far as a mark can tell.
    ///
    /// Covers a source finished last week and a source that has only just appeared behind
    /// the mark, because those are the same answer to a structure that records where a feed
    /// got to rather than which files it read.
    Done,
}

impl Position {
    /// Read a position back from a table property.
    ///
    /// # Errors
    ///
    /// The parse error, when the property is present and unreadable. Distinguished from
    /// absent on purpose: *no position* means a feed that has not run, and *a position
    /// nobody can read* means one whose progress is unknown --- and starting from the
    /// beginning on the second is how a table acquires every row twice.
    pub fn from_property(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }

    /// The property's value.
    ///
    /// # Errors
    ///
    /// The serialization error, which cannot happen for these types and is returned rather
    /// than unwrapped because "cannot happen" is a claim.
    pub fn to_property(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// What this source is, relative to where the feed has got to.
    #[must_use]
    pub fn standing(&self, source: &str) -> Standing {
        if let Some(partial) = &self.partial {
            if partial.source == source {
                return Standing::Resume(partial.read_through);
            }
        }
        if self.through.is_empty() {
            // Nothing has ever finished. A partial source is handled above, so anything else
            // is new.
            return Standing::Fresh;
        }
        if source > self.through.as_str() {
            Standing::Fresh
        } else {
            Standing::Done
        }
    }

    /// Record that `source` has been read to completion.
    pub fn finished(&mut self, source: &str) {
        // Only ever forward. A source finishing out of order would otherwise move the mark
        // backwards and make everything between it and the old mark eligible again.
        if source > self.through.as_str() {
            self.through = source.to_owned();
        }
        self.partial = None;
    }

    /// Record that `read_through` lines of `source` have been read, with more to come.
    ///
    /// Lines rather than published records: see [`Partial::read_through`]. A caller passing a
    /// count of what it published is the defect this field's name exists to prevent.
    pub fn part_way(&mut self, source: &str, read_through: u64) {
        self.partial = Some(Partial { source: source.to_owned(), read_through });
    }
}

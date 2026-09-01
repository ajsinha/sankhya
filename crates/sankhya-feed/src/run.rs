//! Running a feed: sources in, rows published, refusals quarantined, position recorded.
//!
//! # This is a producer, not a storage path
//!
//! Every row and every quarantined record leaves through `sankhya-publish`. A feed that
//! reached storage another way would be the second writer `check-writers` refuses, and the
//! properties this records --- the position --- go in the same commit as the rows for the
//! reason [`crate::progress`] gives.
//!
//! # What one run does
//!
//! Reads the sources in a directory in name order, skipping what the position says is done
//! and refusing what arrived behind it. For each source: bind every record, publish the ones
//! that fit in microbatches bounded by size and by time, quarantine the ones that do not, and
//! stop if the rate says the source has stopped making sense.
//!
//! A run ends when the sources are exhausted or when the feed stops. **It does not loop.** A
//! caller that wants a feed running continuously calls this again, which keeps the decision
//! about how often to look at a directory outside a function that cannot see the deployment.

use crate::bind::{bind, Row};
use crate::declare::DateFrom;
use crate::progress::{key, Position, Standing};
use crate::quarantine::{self, Refused};
use crate::shape;
use crate::source::{records, sources, Unreadable};
use crate::stop::{Outcomes, Reason, Verdict};
use crate::validate::Feed;
use sankhya_publish::{Publication, PublishError};
use sankhya_types::Lsn;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

/// How many versions a publish will rebase over before giving up.
///
/// The same bound capture uses, for the same reason: maintenance commits to these tables
/// too, and a feed that failed the first time a compaction took its version would let
/// maintenance stop ingest --- which inverts the ordering rule.
const REBASE_ATTEMPTS: usize = 16;

/// What a run did.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Ran {
    /// Sources read to completion.
    pub sources: u64,
    /// Records published.
    pub published: u64,
    /// Records quarantined.
    pub quarantined: u64,
    /// Records skipped because the position said they were already published.
    pub resumed_past: u64,
    /// Sources skipped because they sort at or below the high-water mark.
    ///
    /// Ordinary rather than alarming: a spool directory keeps its files, so every source
    /// finished on a previous run is counted here on every run afterwards. It is reported
    /// because the *number* carries the information --- one that grows when it should be
    /// steady means sources are arriving behind the mark, which no mark can tell apart from
    /// files it read yesterday.
    pub already_read: u64,
    /// Why the feed stopped, if it did.
    pub stopped: Option<Reason>,
}

/// Why a run could not proceed.
///
/// Distinct from a record that does not fit, which is quarantined rather than raised. These
/// are conditions no record can be blamed for and no quarantine can absorb.
#[derive(Debug)]
pub enum RunError {
    /// The spool directory or a source could not be read.
    Source(Unreadable),
    /// A table could not be written.
    Publish(PublishError),
    /// A batch could not be assembled from bound rows.
    Unassembled(String),
    /// The recorded position is present and unreadable.
    ///
    /// Refused rather than treated as absent. *No position* means a feed that has not run;
    /// *a position nobody can read* means one whose progress is unknown, and starting from
    /// the beginning there gives the table every row a second time.
    UnreadablePosition {
        /// The feed.
        feed: String,
        /// What the parser said.
        detail: String,
    },
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Source(error) => write!(f, "{error}"),
            Self::Publish(error) => write!(f, "{error}"),
            Self::Unassembled(detail) => write!(f, "{detail}"),
            Self::UnreadablePosition { feed, detail } => write!(
                f,
                "the feed `{feed}` has a recorded position that cannot be read ({detail}). \
                 Refused rather than restarted from the beginning: that is how a table \
                 acquires every row a second time"
            ),
        }
    }
}

impl std::error::Error for RunError {}

/// Everything a run needs that is not the feed itself.
///
/// A struct rather than six arguments, and `now` is here because a run stamps every
/// quarantined record with when it arrived --- and a test that cannot say what time it is
/// cannot assert what was written.
pub struct Running<'a> {
    /// Where the target table lives.
    pub table: &'a Publication,
    /// Where refused records go.
    pub quarantine: &'a Publication,
    /// The version to attempt first for the target table.
    pub table_version: u64,
    /// The version to attempt first for the quarantine.
    pub quarantine_version: u64,
    /// Microseconds since the epoch.
    pub now: &'a dyn Fn() -> i64,
}

impl std::fmt::Debug for Running<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Running")
            .field("table_version", &self.table_version)
            .field("quarantine_version", &self.quarantine_version)
            .finish_non_exhaustive()
    }
}

/// Run a feed once over the sources in `directory`.
///
/// # Errors
///
/// [`RunError`] for a condition no record can be blamed for. A record that does not fit is
/// quarantined and counted, not raised.
pub fn run(feed: &Feed, directory: &Path, mut at: Running<'_>) -> Result<Ran, RunError> {
    let mut position = read_position(feed, at.table)?;
    let mut outcomes = Outcomes::under(feed.quarantine());
    let mut ran = Ran::default();

    for path in sources(directory).map_err(RunError::Source)? {
        let name = path
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
        let skip = match position.standing(&name) {
            Standing::Done => {
                ran.already_read = ran.already_read.saturating_add(1);
                continue;
            }
            Standing::Fresh => 0,
            Standing::Resume(records) => records,
        };
        ran.resumed_past += skip;

        let arrived = records(&path, skip).map_err(RunError::Source)?;
        let mut fitted: Vec<Row> = Vec::new();
        let mut refused: Vec<Refused> = Vec::new();
        let mut published_here = skip;
        let mut opened = Instant::now();
        let closes_after = Duration::from_secs(feed.microbatch().seconds);
        let mut stopped = None;

        for record in arrived {
            let outcome = match &record.document {
                Err(unfit) => Err(unfit.clone()),
                Ok(document) => bind(feed, document),
            };
            let fits = outcome.is_ok();
            match outcome {
                Ok(row) => fitted.push(row),
                Err(unfit) => refused.push(Refused {
                    feed: feed.name().to_owned(),
                    source: name.clone(),
                    position: i64::try_from(record.position).unwrap_or(i64::MAX),
                    arrived_at: (at.now)(),
                    reason_code: quarantine::code(&unfit),
                    reason: unfit.to_string(),
                    declaration: quarantine::fingerprint(feed.declaration()),
                    payload: record.text,
                }),
            }

            // Counted whether it fitted or not, and *before* the batch closes: the rate is
            // about records read, not about records that made it into a file.
            if let Verdict::Stop(reason) = outcomes.record(fits) {
                stopped = Some(reason);
            }

            let full = fitted.len() as u64 >= feed.microbatch().rows;
            let stale = opened.elapsed() >= closes_after;
            if stopped.is_some() || full || stale {
                published_here += fitted.len() as u64;
                commit_batch(
                    feed,
                    &mut at,
                    &mut position,
                    &name,
                    &fitted,
                    published_here,
                    false,
                )?;
                ran.published += fitted.len() as u64;
                fitted.clear();
                opened = Instant::now();
            }
            if stopped.is_some() {
                break;
            }
        }

        // Whatever is left, and the position moved to "this source is finished" — unless the
        // feed stopped part-way, in which case the partial position is what a restart needs.
        let finishing = stopped.is_none();
        published_here += fitted.len() as u64;
        commit_batch(feed, &mut at, &mut position, &name, &fitted, published_here, finishing)?;
        ran.published += fitted.len() as u64;

        ran.quarantined += refused.len() as u64;
        quarantine_all(&mut at, &refused)?;

        if let Some(reason) = stopped {
            ran.stopped = Some(reason);
            return Ok(ran);
        }
        ran.sources += 1;

        // A source that produced nothing usable is an outage rather than an incident, and
        // this is where that is decided — after the source, over the whole of it.
        if let Verdict::Stop(reason) = outcomes.finish_source() {
            ran.stopped = Some(reason);
            return Ok(ran);
        }
    }

    Ok(ran)
}

/// Publish a batch and the position it takes the feed to, in one commit.
fn commit_batch(
    feed: &Feed,
    at: &mut Running<'_>,
    position: &mut Position,
    source: &str,
    rows: &[Row],
    published_here: u64,
    finishing: bool,
) -> Result<(), RunError> {
    // The position moves whether or not there are rows: finishing an all-quarantined source
    // is still progress, and a restart that re-read it would quarantine every record twice.
    if finishing {
        position.finished(source);
    } else {
        position.part_way(source, published_here);
    }
    let recorded = position
        .to_property()
        .map_err(|error| RunError::Unassembled(error.to_string()))?;
    let mut properties = BTreeMap::new();
    properties.insert(key(feed.name()), recorded);

    if rows.is_empty() {
        // No rows, and still a position to record. Written as an empty batch rather than
        // skipped: the alternative is a feed that finishes a wholly-quarantined source and
        // records nothing, so a restart reads it again.
        let empty = shape::batch(feed, &[]).map_err(|error| RunError::Unassembled(error.to_string()))?;
        let rebased = at
            .table
            .append_recording(
                at.table_version,
                REBASE_ATTEMPTS,
                &format!("{}-{:06}.parquet", feed.name(), at.table_version),
                &empty,
                Lsn::new(at.table_version),
                &properties,
            )
            .map_err(RunError::Publish)?;
        at.table_version = rebased.version.saturating_add(1);
        return Ok(());
    }

    let batch = shape::batch(feed, rows)
        .map_err(|error| RunError::Unassembled(error.to_string()))?;
    let rebased = at
        .table
        .append_recording(
            at.table_version,
            REBASE_ATTEMPTS,
            &format!("{}-{:06}.parquet", feed.name(), at.table_version),
            &batch,
            Lsn::new(at.table_version),
            &properties,
        )
        .map_err(RunError::Publish)?;
    at.table_version = rebased.version.saturating_add(1);
    Ok(())
}

/// Write refused records to the quarantine.
fn quarantine_all(at: &mut Running<'_>, refused: &[Refused]) -> Result<(), RunError> {
    if refused.is_empty() {
        return Ok(());
    }
    let batch = quarantine::batch(refused)
        .map_err(|error| RunError::Unassembled(error.to_string()))?;
    let rebased = at
        .quarantine
        .append_rebasing(
            at.quarantine_version,
            REBASE_ATTEMPTS,
            &format!("quarantine-{:06}.parquet", at.quarantine_version),
            &batch,
            Lsn::new(at.quarantine_version),
        )
        .map_err(RunError::Publish)?;
    at.quarantine_version = rebased.version.saturating_add(1);
    Ok(())
}

/// The position recorded on the target table, or a fresh one.
fn read_position(feed: &Feed, table: &Publication) -> Result<Position, RunError> {
    match table.property(&key(feed.name())) {
        None => Ok(Position::default()),
        Some(text) => Position::from_property(&text).map_err(|error| {
            RunError::UnreadablePosition {
                feed: feed.name().to_owned(),
                detail: error.to_string(),
            }
        }),
    }
}

/// What the date axis means for a feed, for a caller wiring the publication.
///
/// Here rather than in the caller because the mapping is a fact about the declaration, and
/// two callers deriving it separately is how they come to disagree.
#[must_use]
pub fn dated_by(feed: &Feed) -> Option<&str> {
    match feed.date() {
        DateFrom::Ingest => None,
        DateFrom::Column { name } => Some(name.as_str()),
    }
}

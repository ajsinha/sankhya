//! What the diagnostic saw last time.
//!
//! # Why this file exists at all
//!
//! A rate needs two observations, and two observations need two runs. Without somewhere to
//! put the first one, *every* run is the first run: every projection is
//! [`Unknown::TooFewObservations`], and `FR-OPS-17` is satisfied on paper and never once in
//! practice. This is the half of the requirement that is easy to miss, because the
//! projection arithmetic looks like the hard part and is not.
//!
//! [`Unknown::TooFewObservations`]: crate::projection::Unknown::TooFewObservations
//!
//! # The shape, and what it is not
//!
//! An append-only text file, one observation per line, under the data directory. It is
//! deliberately **not** a table in the system it is diagnosing. A diagnostic that cannot run
//! when the database is unhealthy is a diagnostic that cannot run on the day it is needed,
//! and "record the observation" must not be able to fail for the same reason the thing being
//! observed is failing.
//!
//! Text, and not a binary format, so that `tail` answers "what did it see last night?"
//! without this crate. That is worth more than the bytes it costs.
//!
//! # Damage is expected, not exceptional
//!
//! The process can be killed mid-append, so the last line can be a partial one. A torn or
//! unparsable line is **skipped**, and skipping it is the whole recovery: an observation is a
//! sample of something that is still there to be sampled again, so losing one costs a little
//! confidence and nothing else. Refusing to start because a line is malformed would convert
//! a harmless truncation into an outage of the tool you reach for during an outage.

use crate::projection::{Observation, Trend};
use sankhya_version::{Compatibility, DIAGNOSTIC_HISTORY};
use std::collections::BTreeMap;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

/// How many observations of one measure are kept.
///
/// Enough to see a week of hourly samples. The file is rewritten when it grows past this,
/// because an operator diagnostic that grows without bound is itself a disk-space finding.
pub const OBSERVATIONS_KEPT: usize = 200;

/// The file name, under the data directory.
pub const HISTORY_FILE: &str = "diagnostic-history.tsv";

/// The header a history file opens with.
///
/// A comment line, so an operator running `head` on the file learns what it is, and so a
/// build reading a file from a newer release refuses **by name** rather than by discovering
/// that a column it expected is a different shape. A file with no header is format 1: the
/// header was added after the format existed, and its absence means the original.
pub const HISTORY_HEADER_PREFIX: &str = "# sankhya diagnostic history, format ";

/// What a measure is called, for storage: the check and what it was about.
///
/// A pair rather than a formatted string, so that two checks may use the same subject
/// without their observations mixing --- file count and byte count of the same table are
/// different series, and averaging them would be silent nonsense.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Measure {
    /// Which check.
    pub check: String,
    /// What it was about.
    pub subject: String,
}

impl Measure {
    /// Name a measure.
    pub fn new(check: impl Into<String>, subject: impl Into<String>) -> Self {
        Self {
            check: check.into(),
            subject: subject.into(),
        }
    }
}

/// The observations kept from previous runs.
#[derive(Clone, PartialEq, Debug)]
pub struct History {
    series: BTreeMap<Measure, Vec<Observation>>,
    /// Lines that could not be read. Reported, not raised.
    damaged_lines: usize,
    /// The format this file is in.
    format: u32,
    /// How many lines the file holds, including those since dropped by the bound.
    ///
    /// Tracked rather than inferred from the file size, which was the first attempt and was
    /// wrong: line lengths vary by an order of magnitude with the length of a table name, so
    /// a byte threshold either fires constantly on long names or never fires on short ones.
    lines_on_disk: usize,
}

impl Default for History {
    fn default() -> Self {
        Self {
            series: BTreeMap::new(),
            damaged_lines: 0,
            // A history nobody has read yet is in the format this build writes. Deriving
            // `Default` would have made it zero, which is not a format any file is in.
            format: DIAGNOSTIC_HISTORY.current,
            lines_on_disk: 0,
        }
    }
}

impl History {
    /// An empty history, for a first run or a test.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Read the history under a data directory.
    ///
    /// A missing file is an empty history and not an error: the first run of a new
    /// installation is the commonest case there is.
    ///
    /// # Errors
    ///
    /// Only when the file exists and cannot be read at all. Damage *within* the file is
    /// counted rather than raised --- see the module comment.
    pub fn read(data_dir: &Path) -> Result<Self, HistoryError> {
        let path = data_dir.join(HISTORY_FILE);
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Self::new()),
            Err(error) => {
                return Err(HistoryError::Unreadable {
                    path,
                    why: error.to_string(),
                })
            }
        };

        let mut history = Self::new();
        for line in BufReader::new(file).lines() {
            if let Ok(text) = &line {
                if let Some(version) = text.strip_prefix(HISTORY_HEADER_PREFIX) {
                    let found: u32 = version.trim().parse().unwrap_or(1);
                    history.format = found;
                    // Checked before a single observation is read. Otherwise a future format
                    // is discovered as a column that will not parse, counted as damage, and
                    // reported as a corrupt file — sending an operator to look for a bad
                    // disk when the answer is to upgrade.
                    if let Compatibility::Refused { why } = DIAGNOSTIC_HISTORY.admits(found) {
                        return Err(HistoryError::FromTheFuture {
                            path: path.clone(),
                            why,
                        });
                    }
                    continue;
                }
                if text.starts_with('#') {
                    continue;
                }
            }
            let Ok(line) = line else {
                // An I/O failure part-way through. Everything read so far is still valid
                // and still useful, which is the argument for keeping it.
                history.damaged_lines += 1;
                history.lines_on_disk += 1;
                break;
            };
            if line.trim().is_empty() {
                continue;
            }
            history.lines_on_disk += 1;
            match parse(&line) {
                Some((measure, observation)) => history.record(measure, observation),
                None => history.damaged_lines += 1,
            }
        }
        Ok(history)
    }

    /// Add an observation in memory.
    pub fn record(&mut self, measure: Measure, observation: Observation) {
        let series = self.series.entry(measure).or_default();
        series.push(observation);
        if series.len() > OBSERVATIONS_KEPT {
            let excess = series.len() - OBSERVATIONS_KEPT;
            series.drain(..excess);
        }
    }

    /// The series for one measure, ready to project from.
    #[must_use]
    pub fn trend(&self, measure: &Measure) -> Trend {
        self.series
            .get(measure)
            .map_or_else(Trend::default, |observations| {
                Trend::of(observations.iter().copied())
            })
    }

    /// Every measure held.
    pub fn measures(&self) -> impl Iterator<Item = &Measure> {
        self.series.keys()
    }

    /// Which format the file on disk is in.
    #[must_use]
    pub const fn format(&self) -> u32 {
        self.format
    }

    /// How many lines could not be read.
    ///
    /// Surfaced so a report can say so. A history quietly losing every second line still
    /// projects, and projects from half the samples it claims.
    #[must_use]
    pub const fn damaged_lines(&self) -> usize {
        self.damaged_lines
    }

    /// How many observations are held in total.
    #[must_use]
    pub fn len(&self) -> usize {
        self.series.values().map(Vec::len).sum()
    }

    /// Whether nothing has been observed yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.series.values().all(Vec::is_empty)
    }

    /// Append one observation to the file, and to this history.
    ///
    /// Appending rather than rewriting, so the cost does not grow with the file and so a
    /// crash mid-write can damage only the last line.
    ///
    /// # Errors
    ///
    /// When the file cannot be opened or written.
    pub fn append(
        &mut self,
        data_dir: &Path,
        measure: Measure,
        observation: Observation,
    ) -> Result<(), HistoryError> {
        let path = data_dir.join(HISTORY_FILE);
        let fresh = !path.exists();
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|error| HistoryError::Unwritable {
                path: path.clone(),
                why: error.to_string(),
            })?;
        if fresh {
            // Only on a new file. Appending a header to an existing one would put it in the
            // middle, where it is neither a header nor an observation.
            writeln!(file, "{HISTORY_HEADER_PREFIX}{}", DIAGNOSTIC_HISTORY.current).map_err(
                |error| HistoryError::Unwritable {
                    path: path.clone(),
                    why: error.to_string(),
                },
            )?;
        }
        // One write, so the line is as close to atomic as a filesystem will give without a
        // journal of our own. A short write still leaves a torn line, which is why the
        // reader tolerates one.
        let line = format(&measure, &observation);
        file.write_all(line.as_bytes())
            .and_then(|()| file.flush())
            .map_err(|error| HistoryError::Unwritable {
                path,
                why: error.to_string(),
            })?;
        self.record(measure, observation);
        self.lines_on_disk += 1;
        Ok(())
    }

    /// Rewrite the file with only the observations kept in memory.
    ///
    /// Called when the file has grown past what is worth keeping. Written to a temporary
    /// name and renamed, so an interrupted compaction leaves the old history rather than
    /// half of a new one.
    ///
    /// # Errors
    ///
    /// When the file cannot be written or renamed.
    pub fn compact(&self, data_dir: &Path) -> Result<(), HistoryError> {
        let path = data_dir.join(HISTORY_FILE);
        let temporary = data_dir.join(format!("{HISTORY_FILE}.new"));
        let mut buffer = format!("{HISTORY_HEADER_PREFIX}{}\n", DIAGNOSTIC_HISTORY.current);
        for (measure, observations) in &self.series {
            for observation in observations {
                buffer.push_str(&format(measure, observation));
            }
        }
        std::fs::write(&temporary, buffer).map_err(|error| HistoryError::Unwritable {
            path: temporary.clone(),
            why: error.to_string(),
        })?;
        std::fs::rename(&temporary, &path).map_err(|error| HistoryError::Unwritable {
            path,
            why: error.to_string(),
        })
    }

    /// How many lines the file holds, whether or not they are still kept in memory.
    #[must_use]
    pub const fn lines_on_disk(&self) -> usize {
        self.lines_on_disk
    }

    /// Whether the file holds enough dead weight to be worth rewriting.
    ///
    /// True once more than half of it is observations the bound has already dropped. A
    /// looser rule leaves the file growing forever; a tighter one rewrites on almost every
    /// run, which is a lot of I/O to save a few kilobytes and one more chance to be
    /// interrupted mid-rename.
    #[must_use]
    pub fn should_compact(&self) -> bool {
        self.lines_on_disk > 2 * self.len().max(1)
    }
}

/// One line: microseconds, check, subject, value, tab-separated.
///
/// The value is written with `{:?}` for length, not for accuracy. Both `{}` and `{:?}` read
/// back bit-identically for `f64` --- an earlier comment here claimed `{}` rounded, and it
/// does not --- but `{}` never uses an exponent, so `1e-300` becomes 302 characters of
/// zeroes. On a file appended to hourly forever, that is the difference worth having.
fn format(measure: &Measure, observation: &Observation) -> String {
    format!(
        "{}\t{}\t{}\t{:?}\n",
        observation.at,
        measure.check.replace('\t', " "),
        measure.subject.replace('\t', " "),
        observation.value
    )
}

/// The inverse, tolerating anything that is not exactly a line.
fn parse(line: &str) -> Option<(Measure, Observation)> {
    let mut fields = line.split('\t');
    let at: i64 = fields.next()?.parse().ok()?;
    let check = fields.next()?;
    let subject = fields.next()?;
    let value: f64 = fields.next()?.parse().ok()?;
    // A NaN would poison every projection it touched, silently: comparisons against a
    // threshold are all false, so it would read as "not there yet" forever.
    if !value.is_finite() {
        return None;
    }
    Some((Measure::new(check, subject), Observation::new(at, value)))
}

/// Why the history could not be used.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum HistoryError {
    /// Written by a newer release.
    ///
    /// Its own variant rather than one more `Unreadable`, because the action differs
    /// entirely: this one says upgrade the binary, and the others say look at the disk.
    FromTheFuture {
        /// Which file.
        path: PathBuf,
        /// What to do.
        why: String,
    },
    /// The file exists and cannot be read.
    Unreadable {
        /// Which file.
        path: PathBuf,
        /// What the filesystem said.
        why: String,
    },
    /// The file cannot be written.
    Unwritable {
        /// Which file.
        path: PathBuf,
        /// What the filesystem said.
        why: String,
    },
}

impl fmt::Display for HistoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FromTheFuture { path, why } => {
                write!(f, "{}: {why}", path.display())
            }
            Self::Unreadable { path, why } => write!(
                f,
                "the diagnostic history at {} could not be read ({why}); without it every \
                 run is a first run and no projection is possible",
                path.display()
            ),
            Self::Unwritable { path, why } => write!(
                f,
                "the diagnostic history at {} could not be written ({why}); this run's \
                 observations will be lost, so the next run will project from a gap",
                path.display()
            ),
        }
    }
}

impl std::error::Error for HistoryError {}

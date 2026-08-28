//! Reading what the process is actually using.
//!
//! `/proc` rather than a crate, and rather than a platform call. `forbid(unsafe_code)` rules
//! out the syscall directly, a dependency for two files would be a dependency to keep pinned
//! forever, and `/proc/self/status` has been stable for longer than most of the things that
//! would wrap it.
//!
//! Every reading is an `Option`. A soak that cannot read its own memory must report that it
//! could not, not zero --- zero is a perfectly steady measure, and a harness reporting a
//! steady zero passes every run while measuring nothing.

use crate::projection::Observation;

/// Resident memory, in bytes.
#[must_use]
pub fn resident_bytes() -> Option<f64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
    let kilobytes: f64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kilobytes * 1024.0)
}

/// How many file descriptors are open.
#[must_use]
pub fn open_files() -> Option<f64> {
    let entries = std::fs::read_dir("/proc/self/fd").ok()?;
    // The `read_dir` handle is itself one of them, and it is closed the moment this returns.
    // Subtracting it keeps a run's numbers comparable with a reading taken any other way.
    #[allow(clippy::cast_precision_loss)]
    Some((entries.count().saturating_sub(1)) as f64)
}

/// A file's size, for the artefacts a soak watches growing.
#[must_use]
pub fn file_bytes(path: &std::path::Path) -> Option<f64> {
    #[allow(clippy::cast_precision_loss)]
    std::fs::metadata(path).ok().map(|m| m.len() as f64)
}

/// Samples of every measure, keyed by name.
#[derive(Clone, Debug, Default)]
pub struct Samples {
    taken: std::collections::BTreeMap<String, Vec<Observation>>,
    /// Instants where a measure could not be read.
    ///
    /// Counted rather than skipped silently: a run that failed to sample half the time has
    /// half the evidence it appears to have, and the judgement should say so.
    missed: std::collections::BTreeMap<String, usize>,
}

impl Samples {
    /// An empty record.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a reading, or that one could not be taken.
    pub fn record(&mut self, measure: &str, at: i64, value: Option<f64>) {
        match value {
            Some(value) => self
                .taken
                .entry(measure.to_string())
                .or_default()
                .push(Observation::new(at, value)),
            None => *self.missed.entry(measure.to_string()).or_default() += 1,
        }
    }

    /// Everything recorded for a measure.
    #[must_use]
    pub fn of(&self, measure: &str) -> &[Observation] {
        self.taken.get(measure).map_or(&[], Vec::as_slice)
    }

    /// How many readings could not be taken.
    #[must_use]
    pub fn missed(&self, measure: &str) -> usize {
        self.missed.get(measure).copied().unwrap_or(0)
    }

    /// Every measure with at least one reading.
    pub fn measured(&self) -> impl Iterator<Item = &str> {
        self.taken.keys().map(String::as_str)
    }

    /// How long the run spans, in seconds.
    #[must_use]
    pub fn span_seconds(&self) -> i64 {
        let starts = self.taken.values().filter_map(|s| s.first().map(|o| o.at));
        let ends = self.taken.values().filter_map(|s| s.last().map(|o| o.at));
        match (starts.min(), ends.max()) {
            (Some(first), Some(last)) => (last - first) / 1_000_000,
            _ => 0,
        }
    }
}

/// How many bytes a directory tree occupies.
///
/// Walked rather than read from `statvfs`, because the question is what *this run* is
/// consuming, not what else is on the volume. A soak that reported free space would fail
/// when somebody else filled the disk and pass when it filled the disk itself.
#[must_use]
pub fn tree_bytes(path: &std::path::Path) -> Option<f64> {
    fn walk(dir: &std::path::Path, total: &mut u64) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.metadata() {
                Ok(metadata) if metadata.is_dir() => walk(&path, total),
                Ok(metadata) => *total = total.saturating_add(metadata.len()),
                Err(_) => {}
            }
        }
    }
    if !path.exists() {
        return None;
    }
    let mut total = 0u64;
    walk(path, &mut total);
    #[allow(clippy::cast_precision_loss)]
    Some(total as f64)
}

//! Writing a materialised cuboid, which is maintenance rather than querying.
//!
//! # Why this lives here and not in the query path
//!
//! A materialised cuboid is a published table, so writing one is a write to a warehouse — and
//! only two crates may do that. The single-writer rule caught this: making the server able to
//! write cuboids would have made it a third writer, and `check-writers` said so before any of
//! it shipped.
//!
//! The rule was right and the answer is not an exemption. This crate's licence to write is
//! *"rewrites already-published files rather than admitting new data — compaction and
//! retention, not ingestion"*, and a materialised cuboid is exactly that: derived from
//! published data, admitting nothing. [ADR-0009](../../../docs/adr/0009-the-cube-lifecycle.md)
//! had already put refresh in the maintenance tick for its own reasons; the writer rule
//! independently insisted on the same place.
//!
//! The query path **reads** cuboids, which is not a write and needs no licence.

use sankhya_cube::algo::Rule;
use sankhya_cube::cells::Cells;
use sankhya_cube::materialise::Key;
use sankhya_cube::store;
use sankhya_publish::{Publication, PublishError};
use sankhya_types::Lsn;
use std::path::{Path, PathBuf};

/// Where a warehouse keeps materialised cuboids.
///
/// Under `_cubes`: table discovery skips a schema beginning with `_`, so a cuboid stays
/// readable by anything that reads a table — the open-storage commitment gets no exception for
/// the fast path — while not appearing in a catalogue somebody browses, where its name is a
/// hash and it looks like a table they should query.
pub const CUBOIDS: &str = "_cubes";

/// Where one cuboid lives.
#[must_use]
pub fn root_of(warehouse: &Path, key: &Key, cube: &str) -> PathBuf {
    warehouse.join(CUBOIDS).join(key.table(cube))
}

/// Whether this cuboid has already been written.
///
/// The key embeds the definition version, the snapshot and the scope, so a cuboid that exists
/// is a cuboid that is still correct — there is no staleness to check and no invalidation
/// protocol to get wrong. That is the whole point of `FR-QUERY-20`'s key.
#[must_use]
pub fn exists(warehouse: &Path, key: &Key, cube: &str) -> bool {
    root_of(warehouse, key, cube).join("_delta_log").is_dir()
}

/// Write cells as a materialised cuboid.
///
/// Idempotent: a cuboid that already exists is left alone rather than rewritten, because the
/// key that named it also guarantees its contents.
///
/// # Errors
///
/// The publish error, if the table could not be created or written. A caller that treats this
/// as fatal has made a cache into a reason to refuse a correct answer; see
/// [`materialise_quietly`].
pub fn materialise(
    warehouse: &Path,
    key: &Key,
    cube: &str,
    cells: &Cells,
    rule: Rule,
) -> Result<bool, PublishError> {
    if exists(warehouse, key, cube) {
        return Ok(false);
    }
    let batch = store::to_batch(cells, rule).map_err(|error| PublishError::Write {
        file: key.table(cube),
        detail: error.to_string(),
    })?;
    // An empty cuboid is not written. A table of no rows is indistinguishable, on the way
    // back, from a cube that saw nothing --- and writing one would make the next run skip the
    // hydration that would have found the rows.
    if batch.num_rows() == 0 {
        return Ok(false);
    }

    let root = root_of(warehouse, key, cube);
    let publication = Publication::external(&root, cube);
    publication.create(&batch.schema())?;
    publication.append(1, "cuboid-000000.parquet", &batch, Lsn::new(key.snapshot))?;
    Ok(true)
}

/// Materialise, reporting failure rather than returning it.
///
/// For the tick: a cuboid that could not be written costs a rehydration next time, and
/// failing the maintenance pass over it would stop compaction for every table because a cache
/// could not be filled.
pub fn materialise_quietly(
    warehouse: &Path,
    key: &Key,
    cube: &str,
    cells: &Cells,
    rule: Rule,
) -> bool {
    match materialise(warehouse, key, cube, cells, rule) {
        Ok(written) => written,
        Err(error) => {
            eprintln!("  could not materialise cuboid for cube `{cube}`: {error}");
            false
        }
    }
}

// --- staleness, which is exact rather than estimated -------------------------

/// How far behind the table a materialised cuboid has fallen, in versions.
///
/// # Why this is an integer and not a duration
///
/// A cuboid is keyed by the snapshot it was computed at, so its staleness is **exactly** the
/// distance from the table's current version. Nothing is estimated and no clock is read.
///
/// A duration would have to be inferred from commit rates, and an inferred SLA is a
/// decoration: it is right when the system is behaving and wrong exactly when somebody needs
/// it — during a burst, which is when both the commit rate and the consequences change.
///
/// A cuboid *ahead* of the table is not negative and not an error. It means the table was
/// rewound, or the cuboid was written against a version that has since been rolled back, and
/// the honest answer is zero lag with the caller free to distrust it on other grounds.
#[must_use]
pub const fn lag(cuboid_snapshot: u64, table_version: u64) -> u64 {
    table_version.saturating_sub(cuboid_snapshot)
}

/// Whether a cuboid is still within its cube's stated target.
///
/// `None` — a cube nothing materialises — is never fresh, because there is nothing to be
/// fresh. Answering `true` would make a Declared cube look Maintained to every caller that
/// asks this question.
#[must_use]
pub const fn within_target(target_lag: Option<u64>, lag: u64) -> bool {
    match target_lag {
        Some(target) => lag <= target,
        None => false,
    }
}

/// Whether a refresh is due.
///
/// The complement of [`within_target`] for a maintained cube, and never for one that
/// materialises nothing — refreshing a cube with no target would build cuboids nobody
/// declared and charge an operator storage they did not ask for.
#[must_use]
pub const fn refresh_due(target_lag: Option<u64>, lag: u64) -> bool {
    match target_lag {
        Some(target) => lag > target,
        None => false,
    }
}

/// Whether a target is achievable given how long a refresh takes.
///
/// # Why an unmeetable target has to be said out loud
///
/// If a table advances by more versions during a refresh than the target allows, the cuboid
/// is stale the moment it is written and every query falls back to live aggregation. The
/// system still answers correctly — that is the point of the fallback — and it does so while
/// spending storage and maintenance time on a cache that can never be used.
///
/// Silence there turns an SLA into a decoration: the operator set a number, the system
/// accepted it, and nothing ever meets it. Reported in the same shape as the fan-out alarm —
/// state the measurement, state the target, name the thing to change.
#[must_use]
pub fn unmeetable(target_lag: Option<u64>, versions_during_refresh: u64) -> Option<String> {
    let target = target_lag?;
    if versions_during_refresh <= target {
        return None;
    }
    Some(format!(
        "the table advanced {versions_during_refresh} version(s) while the cuboid was being \
         built and the target lag is {target}, so it is stale before it is written and every \
         query will fall back to live aggregation. The cache costs storage and maintenance \
         time and can never be used: raise the target, or reduce what is materialised"
    ))
}

// --- retiring cuboids nothing can ask for ------------------------------------

/// What a sweep of the cuboid store decided.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Swept {
    /// Cuboid tables removed, by name.
    pub removed: Vec<String>,
    /// Bytes reclaimed.
    pub bytes_reclaimed: u64,
    /// Directories deliberately left, with the reason.
    ///
    /// Keeping one is never an error. It costs storage; removing one wrongly costs a query,
    /// or somebody else's data.
    pub retained: Vec<(String, String)>,
}

/// Remove cuboids no query can ask for.
///
/// # What makes one collectable
///
/// A cuboid is found by a key embedding the snapshot it was computed at, and a query asks at
/// the table's **current** version. So a cuboid at an older snapshot cannot be selected by
/// anything: its key can never match. It is garbage the moment the table advances.
///
/// Nothing collected it. The orphan sweep finds unreferenced files *within* a table, and a
/// superseded cuboid is a whole table that no log mentions --- so it fell between the two
/// mechanisms that exist. That is the same shape as the defect that filled a disk during the
/// soak: something producing garbage, and nothing reclaiming it.
///
/// # What protects one
///
/// `behind` is how many versions of drift to tolerate before removing. It is not the target
/// lag and should not be confused with it: `target_lag` decides what may be *served*, and
/// this decides what may be *deleted*, which must be strictly more generous. A query that
/// resolved a cuboid a moment ago is still reading it, and a file deleted from under a
/// running scan is an error naming a path the caller never mentioned.
///
/// A directory that cannot be parsed as a cuboid is **retained with a reason**, always. A
/// warehouse holds directories this code did not write, and a sweep that deletes what it does
/// not recognise is a sweep that eventually deletes something that mattered.
pub fn retire_superseded(
    warehouse: &Path,
    current: &std::collections::BTreeMap<String, u64>,
    behind: u64,
) -> Swept {
    let mut swept = Swept::default();
    let store = warehouse.join(CUBOIDS);
    let Ok(entries) = std::fs::read_dir(&store) else {
        // No cuboid store is not an error. A warehouse that has never materialised anything
        // is the ordinary case.
        return swept;
    };

    let mut names: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.metadata().is_ok_and(|meta| meta.is_dir()))
        .filter_map(|entry| entry.file_name().to_str().map(ToString::to_string))
        .collect();
    // Sorted, so two runs sweep in the same order and a log of what went is comparable.
    names.sort();

    for name in names {
        let Some((cube, key)) = sankhya_cube::materialise::parse(&name) else {
            swept.retained.push((
                name,
                "not a cuboid this version wrote; a sweep that deletes what it does not \
                 recognise eventually deletes something that mattered"
                    .to_string(),
            ));
            continue;
        };
        let Some(&version) = current.get(&cube) else {
            swept.retained.push((
                name,
                format!(
                    "no current version is known for cube `{cube}`, so how far behind this \
                     is cannot be decided --- and deleting on a guess is how a cache becomes \
                     a data loss"
                ),
            ));
            continue;
        };
        let drift = lag(key.snapshot, version);
        if drift <= behind {
            continue;
        }

        let path = store.join(&name);
        let bytes = tree_bytes(&path);
        match std::fs::remove_dir_all(&path) {
            Ok(()) => {
                swept.bytes_reclaimed = swept.bytes_reclaimed.saturating_add(bytes);
                swept.removed.push(name);
            }
            Err(error) => swept.retained.push((name, error.to_string())),
        }
    }
    swept
}

/// How many bytes a directory holds.
fn tree_bytes(at: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(at) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.metadata() {
            Ok(meta) if meta.is_dir() => tree_bytes(&entry.path()),
            Ok(meta) => meta.len(),
            Err(_) => 0,
        })
        .sum()
}

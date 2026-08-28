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

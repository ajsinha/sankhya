//! Finding the tables on disk and opening them for reading.
//!
//! # Why the schema comes from the log
//!
//! A server reads tables it did not write. On restart it has forgotten everything; in a
//! cluster another node wrote them. So the schema is read back out of each table's own log
//! rather than being remembered, configured, or inferred from a Parquet footer.
//!
//! Inferring from a footer would be the tempting shortcut and it is wrong in a specific
//! way: a table with no files yet has no footer to read, and one whose files were written
//! before a column was added would produce a schema missing it. The log is the only place
//! that knows what the table *is* rather than what happens to be in it.
//!
//! # The layout this walks
//!
//! `<warehouse>/<schema>/<table>/`, matching what the OLAP tier writes and what
//! `REQUIREMENTS` specifies, so a table's path says where it came from without a lookup.
//! A directory without a `_delta_log` is not a table and is skipped in silence --- an
//! object store holds all sorts of things and complaining about each would bury the ones
//! that matter.

use crate::execute::ServableTable;
use arrow_schema::Schema;
use sankhya_authz::policy::TableRef;
use sankhya_table_delta::LogCache;
use sankhya_table_delta::{read_actions, schema_from_string, Action};
use sankhya_types::Lsn;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A table found on disk, before it is opened.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FoundTable {
    /// Where it lives, as SQL names it.
    pub reference: TableRef,
    /// Where it lives, as the filesystem holds it.
    pub root: PathBuf,
    /// Its columns, read from its own log.
    pub schema: Arc<Schema>,
}

/// Resolve any table whose log has moved since its provider was built.
///
/// # Why a running server has to do this at all
///
/// It did not, and the reason it did not was sound: a server runs no ingest, so the warehouse
/// it serves does not move, and a provider resolved once at startup stays correct forever.
///
/// The server now runs **maintenance** in-process. Compaction replaces files and retirement
/// deletes the ones it replaced, so the warehouse moves whether or not anybody is writing to
/// it. A provider fixed at boot then names files that are gone, and the query fails with a
/// missing-file error naming a path nobody asked about.
///
/// Retirement's grace period is not the answer. It protects a reader that listed shortly
/// before a merge --- twenty-four ticks of it --- and cannot protect one that listed at
/// startup and has been serving from that listing since.
///
/// Returns how many were re-resolved, so the caller can say so rather than have it happen
/// invisibly.
pub fn refresh(tables: &mut [ServableTable], target: Lsn, cache: &LogCache) -> usize {
    let coverage = sankhya_types::LsnRange::new(sankhya_types::Lsn::new(0), target);
    let mut redone = 0;
    for table in tables {
        let now = sankhya_table_delta::live_files(&table.root)
            .ok()
            .and_then(|live| live.version)
            .unwrap_or(table.resolved_at);
        if now == table.resolved_at {
            continue;
        }
        // A table that will not resolve keeps the provider it has. The old one may fail on a
        // retired file, and the new one failed outright --- serving the stale reader is the
        // better of two bad answers, and the next attempt tries again.
        if let Ok(provider) = sankhya_readpath::resolve_cached(
            Arc::clone(&table.schema),
            &table.root,
            coverage,
            None,
            target,
            cache,
        ) {
            table.provider = Arc::new(provider);
            // Recorded so the next statement can tell this table has not moved. Forgetting
            // it costs a log read per query rather than a wrong answer, which is why no test
            // catches it and why there is no catalogue entry claiming one does.
            table.resolved_at = now;
            redone += 1;
        }
    }
    redone
}

/// Walk a warehouse and find every table in it.
///
/// Returns what it found and, separately, what it could not open and why. A table that
/// fails to open is **not** silently omitted: a server that starts with three of four
/// tables and says nothing has produced an outage that looks like a missing table to
/// whoever queries it.
#[must_use]
pub fn discover(warehouse: &Path) -> (Vec<FoundTable>, Vec<(PathBuf, String)>) {
    let mut found = Vec::new();
    let mut refused = Vec::new();

    let Ok(schemas) = std::fs::read_dir(warehouse) else {
        return (found, refused);
    };
    let mut schema_dirs: Vec<PathBuf> = schemas
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    // Sorted, so two nodes reading the same warehouse register tables in the same order and
    // a name collision is reported against the same table on both.
    schema_dirs.sort();

    for schema_dir in schema_dirs {
        let Some(schema_name) = schema_dir.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // A schema beginning with `_` holds the warehouse's own bookkeeping --- cube
        // definitions, materialised cuboids --- not user tables.
        //
        // Skipped from *discovery*, not hidden from storage. A materialised cuboid is a
        // published table on purpose, readable by anything that can read a table, because the
        // open-storage commitment gets no exception for the fast path. What it must not do is
        // appear in a catalogue somebody browses, where it looks like a table they should
        // query and its name is a hash.
        if schema_name.starts_with('_') {
            continue;
        }
        let Ok(tables) = std::fs::read_dir(&schema_dir) else {
            continue;
        };
        let mut table_dirs: Vec<PathBuf> = tables
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect();
        table_dirs.sort();

        for table_dir in table_dirs {
            // A directory without a log is not a table. Object stores hold all sorts of
            // things and complaining about each would bury the ones that matter.
            if !table_dir.join("_delta_log").is_dir() {
                continue;
            }
            let Some(table_name) = table_dir.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            match open(&table_dir) {
                Ok(schema) => found.push(FoundTable {
                    reference: TableRef::new(schema_name, table_name),
                    root: table_dir,
                    schema,
                }),
                Err(reason) => refused.push((table_dir, reason)),
            }
        }
    }
    (found, refused)
}

/// Read a table's schema out of its log.
fn open(table_root: &Path) -> Result<Arc<Schema>, String> {
    let actions = read_actions(table_root).map_err(|error| error.to_string())?;

    // The *last* metadata action, not the first. A schema evolution writes a new one, and
    // reading the first would serve the table's original shape forever — a column added
    // last year would be invisible, and nothing would say so.
    let latest = actions
        .iter()
        .rev()
        .find_map(|(_, action)| match action {
            Action::Metadata(metadata) => Some(metadata),
            _ => None,
        })
        .ok_or_else(|| "the log contains no metadata action".to_string())?;

    let schema = schema_from_string(&latest.schema_string).map_err(|error| error.to_string())?;
    Ok(Arc::new(schema))
}

/// Where a table's log lives, for a name a client used.
///
/// # Why this exists, and what was broken without it
///
/// Discovery reads `<schema>/<table>/`, and a session registers each table under its **bare**
/// name --- so a client says `orders` and means `sales/orders`. Cloning resolved the same name
/// as `warehouse/orders`, one level up, which is a directory that does not exist in any
/// deployment laid out the way discovery expects.
///
/// The consequence was that `CREATE TABLE ... CLONE` could only name a table the server does
/// not serve. It was tested against a warehouse whose tables sat at the root, so every test
/// passed, and the feature had never worked against a table anybody could query.
///
/// Two names resolve here. A qualified `sales.orders` names its schema; a bare `orders` is
/// searched for across schemas, which is what a client has after registration flattened them.
///
/// # Ambiguity is refused rather than ordered
///
/// Two schemas may hold a table of the same name. Picking one --- the first alphabetically,
/// say --- would clone the wrong table on a warehouse that grew a second `orders`, and it would
/// do it silently and correctly-looking. So [`Resolved::Ambiguous`] carries both and the caller
/// refuses with them named.
#[must_use]
pub fn resolve(warehouse: &Path, name: &str) -> Resolved {
    if let Some((schema, table)) = name.split_once('.') {
        let root = warehouse.join(schema).join(table);
        return if sankhya_publish::is_table(&root) {
            Resolved::One(root)
        } else {
            Resolved::Absent
        };
    }

    let mut found: Vec<PathBuf> = Vec::new();
    let Ok(entries) = std::fs::read_dir(warehouse) else {
        return Resolved::Absent;
    };
    let mut schemas: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    // Sorted, so an ambiguity is reported in the same order twice and a test can name it.
    schemas.sort();
    for schema in schemas {
        // `_`-prefixed schemas hold the warehouse's own bookkeeping and are not user tables,
        // exactly as `discover` treats them. A clone of a materialised cuboid is not a thing.
        if schema
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with('_'))
        {
            continue;
        }
        let root = schema.join(name);
        if sankhya_publish::is_table(&root) {
            found.push(root);
        }
    }

    match found.len() {
        0 => Resolved::Absent,
        1 => found.pop().map_or(Resolved::Absent, Resolved::One),
        _ => Resolved::Ambiguous(
            found
                .iter()
                .filter_map(|root| qualified_name(warehouse, root))
                .collect(),
        ),
    }
}

/// Where a **new** clone of this name goes, given where its origin lives.
///
/// # A clone stays in its origin's schema
///
/// Always, and a qualified name that says otherwise is refused rather than obeyed.
///
/// `ADR-0016` makes a clone a *reference* to its origin's files rather than a copy, and the
/// right to read it derives from the right to read what it references --- which is why
/// authorization resolves a clone through its root. Putting the clone under another schema
/// would put its **name** under one schema's policy while its **data** stays governed by
/// another's, and nobody could then say which rule applies to it.
///
/// It is also what anybody expects. The first thing done with a clone is to compare it with
/// what it came from, and a clone that landed somewhere its origin is not makes that a hunt.
///
/// A bare name therefore lands beside its origin, and a qualified one is accepted only when it
/// names the schema its origin is already in.
pub fn place_beside(warehouse: &Path, name: &str, origin_root: &Path) -> Result<PathBuf, Misplaced> {
    let origin_schema = origin_root.parent();
    match name.split_once('.') {
        None => origin_schema
            .map(|schema| schema.join(name))
            .ok_or(Misplaced::NoSchema),
        Some((schema, table)) => {
            let asked = warehouse.join(schema);
            if origin_schema.is_some_and(|origin| origin == asked) {
                Ok(asked.join(table))
            } else {
                Err(Misplaced::OtherSchema {
                    asked: schema.to_owned(),
                    origin: origin_schema
                        .and_then(|origin| origin.file_name())
                        .and_then(|name| name.to_str())
                        .unwrap_or_default()
                        .to_owned(),
                })
            }
        }
    }
}

/// Why a clone cannot go where the statement asked.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Misplaced {
    /// A schema was named, and it is not the origin's.
    OtherSchema {
        /// The schema the statement asked for.
        asked: String,
        /// The schema the origin is in, which is the only one available.
        origin: String,
    },
    /// The origin is not in a schema at all, so there is nowhere to put a clone beside it.
    NoSchema,
}

impl std::fmt::Display for Misplaced {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OtherSchema { asked, origin } => write!(
                f,
                "a clone stays in its origin's schema, and `{origin}` is not `{asked}`. A \
                 clone is a reference to its origin's files and is authorized through them, \
                 so one placed under another schema would have its name governed by one \
                 policy and its data by another"
            ),
            Self::NoSchema => write!(
                f,
                "the table to clone is not in a schema, so there is nowhere to put a clone \
                 beside it"
            ),
        }
    }
}

impl std::error::Error for Misplaced {}

/// A table root rendered as `schema.table`, for a message that has to be unambiguous.
#[must_use]
pub fn qualified_name(warehouse: &Path, root: &Path) -> Option<String> {
    let table = root.file_name()?.to_str()?;
    let schema = root.parent()?;
    if schema == warehouse {
        return Some(table.to_owned());
    }
    Some(format!("{}.{table}", schema.file_name()?.to_str()?))
}

/// What a name resolved to.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Resolved {
    /// Exactly one table.
    One(PathBuf),
    /// No table of that name.
    Absent,
    /// Several, named as `schema.table` so the caller can say which.
    Ambiguous(Vec<String>),
}

/// Open every discovered table for reading at `target`.
///
/// `target` is the position to read as of. Everything published up to it is visible and
/// nothing after it is, which is what makes two tables in one query agree with each other.
#[must_use]
pub fn servable(
    tables: &[FoundTable],
    target: Lsn,
    cache: &LogCache,
) -> (Vec<ServableTable>, Vec<(PathBuf, String)>) {
    let mut open = Vec::new();
    let mut refused = Vec::new();

    // What the published tier covers. The read path takes this as the caller's claim
    // rather than computing it, deliberately: the proof belongs to whoever decided which
    // position to read at.
    //
    // For **this** server the claim is true by construction. It runs no ingest, so there is
    // no arrival tier and nothing in the warehouse is unpublished — everything on disk is
    // published, and the published tier therefore covers the whole span being asked for.
    //
    // When ingest joins this process that stops being true, and the coverage must come from
    // the ingest position instead. Asserting it then would be exactly the defect the splice
    // exists to catch: a tier claiming coverage it does not hold, producing an answer that
    // is silently missing rows.
    let coverage = sankhya_types::LsnRange::new(sankhya_types::Lsn::new(0), target);

    for table in tables {
        match sankhya_readpath::resolve_cached(
            Arc::clone(&table.schema),
            &table.root,
            coverage,
            None,
            target,
            cache,
        ) {
            Ok(provider) => open.push(ServableTable {
                reference: table.reference.clone(),
                root: table.root.clone(),
                provider: Arc::new(provider),
                schema: Arc::clone(&table.schema),
                // What the log stood at when this file list was read. A provider whose table
                // has moved past it is stale --- and once the warehouse maintains itself,
                // stale means naming files retirement has deleted.
                resolved_at: sankhya_table_delta::live_files(&table.root)
                    .ok()
                    .and_then(|live| live.version)
                    .unwrap_or(0),
            }),
            Err(error) => refused.push((table.root.clone(), error.to_string())),
        }
    }
    (open, refused)
}

/// The catalogue description of a discovered table, for a schema browser.
#[must_use]
pub fn describe(tables: &[FoundTable]) -> Vec<sankhya_api_pg::catalog::CatalogTable> {
    use arrow_schema::DataType;
    use sankhya_api_pg::catalog::{CatalogColumn, CatalogTable};
    use sankhya_api_pg::message::oid;

    tables
        .iter()
        .map(|table| CatalogTable {
            schema: table.reference.schema.clone(),
            name: table.reference.table.clone(),
            columns: table
                .schema
                .fields()
                .iter()
                .map(|field| {
                    let (type_name, type_oid) = match field.data_type() {
                        DataType::Boolean => ("bool", oid::BOOL),
                        DataType::Int8 | DataType::Int16 => ("int2", oid::INT2),
                        DataType::Int32 => ("int4", oid::INT4),
                        DataType::Int64 | DataType::UInt64 => ("int8", oid::INT8),
                        DataType::Float32 => ("float4", oid::FLOAT4),
                        DataType::Float64 => ("float8", oid::FLOAT8),
                        DataType::Decimal128(_, _) => ("numeric", oid::NUMERIC),
                        DataType::Date32 => ("date", oid::DATE),
                        DataType::Timestamp(_, _) => ("timestamp", oid::TIMESTAMP),
                        DataType::Binary | DataType::LargeBinary => ("bytea", oid::BYTEA),
                        _ => ("text", oid::TEXT),
                    };
                    CatalogColumn {
                        name: field.name().clone(),
                        type_name: type_name.to_string(),
                        type_oid,
                        nullable: field.is_nullable(),
                    }
                })
                .collect(),
        })
        .collect()
}

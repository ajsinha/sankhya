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
                provider: Arc::new(provider),
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

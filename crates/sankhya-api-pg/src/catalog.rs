//! Enough `pg_catalog` for real tools to work.
//!
//! # Why this is more work than it looks
//!
//! `FR-API-03` is explicit: this is *the difference between "a command-line client
//! connects" and "a BI tool works"*. The gap is not small and it is not about SQL. A
//! reporting tool's first act after connecting is a burst of catalogue queries --- what
//! version is this, what schemas exist, what tables, what columns, what types, what keys ---
//! and it makes decisions from the answers before the user has typed anything.
//!
//! A tool that gets an error for one of those queries usually does not report it usefully.
//! It reports "could not connect", or shows an empty schema tree, or renders every column
//! as text. The user concludes the database is broken.
//!
//! # What is emulated, and what is deliberately not
//!
//! The queries below are the ones mainstream tooling actually issues. They are answered
//! from SANKHYA's own catalogue, translated into the shapes those tools expect.
//!
//! Nothing here pretends to be a complete `pg_catalog`. A tool asking for something not
//! covered gets a **named, specific error** rather than an empty result: an empty result is
//! indistinguishable from "you have no tables", and that sends someone looking in the wrong
//! place entirely.

use crate::message::{oid, FieldDescription};

/// A catalogue answer: the columns and the rows.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CatalogResult {
    /// The shape of the result.
    pub fields: Vec<FieldDescription>,
    /// The rows, already rendered as text.
    pub rows: Vec<Vec<Option<String>>>,
}

impl CatalogResult {
    /// How many rows.
    #[must_use]
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }
}

/// What a tool wants to know.
///
/// Recognised by shape rather than by exact text, because no two tools write the same query
/// and matching literally would work for one client and fail for the next.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CatalogQuery {
    /// `SELECT version()` — almost always the first thing a tool sends.
    Version,
    /// `SHOW <setting>` or a `current_setting` call.
    Setting {
        /// Which one.
        name: String,
    },
    /// `SELECT current_schema()` or `current_database()`.
    CurrentSchema,
    /// The list of schemas.
    Schemas,
    /// The tables, optionally within one schema.
    Tables {
        /// Which schema, if the query named one.
        schema: Option<String>,
    },
    /// The columns of a table.
    Columns {
        /// Which table, if the query named one.
        table: Option<String>,
    },
    /// The types a client should know about.
    Types,
    /// `SELECT 1`, which many pools use as a liveness check.
    Ping,
}

/// A table this server exposes, as the catalogue describes it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CatalogTable {
    /// Its schema.
    pub schema: String,
    /// Its name.
    pub name: String,
    /// Its columns, in order.
    pub columns: Vec<CatalogColumn>,
}

/// One column.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CatalogColumn {
    /// Its name.
    pub name: String,
    /// The PostgreSQL type name a client will recognise.
    pub type_name: String,
    /// The PostgreSQL type OID.
    pub type_oid: i32,
    /// Whether it may be null.
    pub nullable: bool,
}

/// Recognise a catalogue query by shape.
///
/// Deliberately lenient about whitespace, case and the exact projection: tools generate
/// these programmatically and no two produce identical text. Matching literally would work
/// for the client it was written against and fail for the next one, which is the failure
/// mode this whole module exists to avoid.
#[must_use]
pub fn recognise(sql: &str) -> Option<CatalogQuery> {
    let normalised = sql.trim().trim_end_matches(';').to_lowercase();
    let compact = normalised.split_whitespace().collect::<Vec<_>>().join(" ");

    if compact == "select 1" {
        return Some(CatalogQuery::Ping);
    }
    if compact.contains("version()") {
        return Some(CatalogQuery::Version);
    }
    if let Some(rest) = compact.strip_prefix("show ") {
        return Some(CatalogQuery::Setting {
            name: rest.trim().to_string(),
        });
    }
    if compact.contains("current_setting(") {
        let name = between(&compact, "current_setting(", ")")
            .unwrap_or_default()
            .trim_matches(|c| c == '\'' || c == '"')
            .to_string();
        return Some(CatalogQuery::Setting { name });
    }
    if compact.contains("current_schema") || compact.contains("current_database") {
        return Some(CatalogQuery::CurrentSchema);
    }
    if compact.contains("pg_namespace") || compact.contains("information_schema.schemata") {
        return Some(CatalogQuery::Schemas);
    }
    if compact.contains("pg_attribute") || compact.contains("information_schema.columns") {
        return Some(CatalogQuery::Columns {
            table: literal_after(&compact, "table_name"),
        });
    }
    if compact.contains("pg_class") || compact.contains("information_schema.tables") {
        return Some(CatalogQuery::Tables {
            schema: literal_after(&compact, "table_schema"),
        });
    }
    if compact.contains("pg_type") {
        return Some(CatalogQuery::Types);
    }
    None
}

/// The text between two markers.
fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = text.find(open)? + open.len();
    let rest = text.get(start..)?;
    let end = rest.find(close)?;
    rest.get(..end)
}

/// The quoted literal following `column =` in a `WHERE` clause.
fn literal_after(text: &str, column: &str) -> Option<String> {
    let at = text.find(column)? + column.len();
    let rest = text.get(at..)?.trim_start();
    let rest = rest.strip_prefix('=')?.trim_start();
    let rest = rest.strip_prefix('\'')?;
    let end = rest.find('\'')?;
    rest.get(..end).map(str::to_string)
}

/// Answer a catalogue query from this server's own tables.
#[must_use]
pub fn answer(
    query: &CatalogQuery,
    server_version: &str,
    current_schema: &str,
    tables: &[CatalogTable],
) -> CatalogResult {
    match query {
        CatalogQuery::Ping => CatalogResult {
            fields: vec![FieldDescription::text("?column?", oid::INT4, 4)],
            rows: vec![vec![Some("1".to_string())]],
        },
        CatalogQuery::Version => CatalogResult {
            fields: vec![FieldDescription::text("version", oid::TEXT, -1)],
            // The string begins with "PostgreSQL <n>.<n>" because every client parses the
            // major version out of it and refuses to proceed if it cannot. What follows
            // says truthfully what this actually is, so nobody is misled by the prefix.
            rows: vec![vec![Some(format!(
                "PostgreSQL 17.0 (SANKHYA {server_version}) on \
                 wire-protocol-compatible unified engine"
            ))]],
        },
        CatalogQuery::Setting { name } => CatalogResult {
            fields: vec![FieldDescription::text(name.clone(), oid::TEXT, -1)],
            rows: vec![vec![Some(setting_value(name, server_version))]],
        },
        CatalogQuery::CurrentSchema => CatalogResult {
            fields: vec![FieldDescription::text("current_schema", oid::TEXT, -1)],
            rows: vec![vec![Some(current_schema.to_string())]],
        },
        CatalogQuery::Schemas => {
            let mut names: Vec<String> = tables.iter().map(|t| t.schema.clone()).collect();
            names.sort();
            names.dedup();
            CatalogResult {
                fields: vec![FieldDescription::text("schema_name", oid::TEXT, -1)],
                rows: names.into_iter().map(|n| vec![Some(n)]).collect(),
            }
        }
        CatalogQuery::Tables { schema } => CatalogResult {
            fields: vec![
                FieldDescription::text("table_schema", oid::TEXT, -1),
                FieldDescription::text("table_name", oid::TEXT, -1),
                FieldDescription::text("table_type", oid::TEXT, -1),
            ],
            rows: tables
                .iter()
                .filter(|t| schema.as_ref().is_none_or(|s| &t.schema == s))
                .map(|t| {
                    vec![
                        Some(t.schema.clone()),
                        Some(t.name.clone()),
                        Some("BASE TABLE".to_string()),
                    ]
                })
                .collect(),
        },
        CatalogQuery::Columns { table } => CatalogResult {
            fields: vec![
                FieldDescription::text("table_schema", oid::TEXT, -1),
                FieldDescription::text("table_name", oid::TEXT, -1),
                FieldDescription::text("column_name", oid::TEXT, -1),
                FieldDescription::text("ordinal_position", oid::INT4, 4),
                FieldDescription::text("data_type", oid::TEXT, -1),
                FieldDescription::text("is_nullable", oid::TEXT, -1),
            ],
            rows: tables
                .iter()
                .filter(|t| table.as_ref().is_none_or(|name| &t.name == name))
                .flat_map(|t| {
                    t.columns.iter().enumerate().map(move |(index, column)| {
                        vec![
                            Some(t.schema.clone()),
                            Some(t.name.clone()),
                            Some(column.name.clone()),
                            Some((index + 1).to_string()),
                            Some(column.type_name.clone()),
                            // 'YES'/'NO' rather than true/false: this is what
                            // information_schema uses, and clients compare the string.
                            Some(if column.nullable { "YES" } else { "NO" }.to_string()),
                        ]
                    })
                })
                .collect(),
        },
        CatalogQuery::Types => CatalogResult {
            fields: vec![
                FieldDescription::text("oid", oid::OID, 4),
                FieldDescription::text("typname", oid::TEXT, -1),
            ],
            rows: known_types()
                .iter()
                .map(|(id, name)| vec![Some(id.to_string()), Some((*name).to_string())])
                .collect(),
        },
    }
}

/// The value of a runtime setting a client may ask about.
///
/// Every one of these is a value a client changes its behaviour on. `server_version_num` in
/// particular is compared numerically to decide which features to use, and a wrong answer
/// makes a tool attempt syntax this server does not have.
fn setting_value(name: &str, server_version: &str) -> String {
    match name {
        "server_version" => "17.0".to_string(),
        "server_version_num" => "170000".to_string(),
        // UTF8 always. Announcing anything else would make clients transcode, and every
        // string on this wire is already UTF-8.
        "client_encoding" | "server_encoding" => "UTF8".to_string(),
        "DateStyle" | "datestyle" => "ISO, MDY".to_string(),
        "TimeZone" | "timezone" => "UTC".to_string(),
        "integer_datetimes" => "on".to_string(),
        "standard_conforming_strings" => "on".to_string(),
        "application_name" => String::new(),
        "sankhya_version" => server_version.to_string(),
        _ => String::new(),
    }
}

/// The types a client is told about.
#[must_use]
pub fn known_types() -> Vec<(i32, &'static str)> {
    vec![
        (oid::BOOL, "bool"),
        (oid::BYTEA, "bytea"),
        (oid::INT8, "int8"),
        (oid::INT2, "int2"),
        (oid::INT4, "int4"),
        (oid::TEXT, "text"),
        (oid::FLOAT4, "float4"),
        (oid::FLOAT8, "float8"),
        (oid::VARCHAR, "varchar"),
        (oid::DATE, "date"),
        (oid::TIMESTAMP, "timestamp"),
        (oid::TIMESTAMPTZ, "timestamptz"),
        (oid::NUMERIC, "numeric"),
    ]
}

/// The parameters sent to a client immediately after authentication.
///
/// Not optional. A client that does not receive `client_encoding` or
/// `standard_conforming_strings` has to guess, and several guess wrong in ways that corrupt
/// string literals rather than failing.
#[must_use]
pub fn startup_parameters(server_version: &str) -> Vec<(String, String)> {
    [
        "server_version",
        "server_encoding",
        "client_encoding",
        "DateStyle",
        "TimeZone",
        "integer_datetimes",
        "standard_conforming_strings",
    ]
    .into_iter()
    .map(|name| (name.to_string(), setting_value(name, server_version)))
    .collect()
}

/// A catalogue query this server does not emulate.
///
/// Its own type so the failure is *named*. An empty result would be indistinguishable from
/// "you have no tables", and that sends someone looking in entirely the wrong place.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Unsupported {
    /// What was asked.
    pub sql: String,
}

impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "this server emulates part of pg_catalog and does not recognise this query: \
             {}. Returning an error rather than no rows, because an empty result is \
             indistinguishable from having no tables and sends the reader looking in the \
             wrong place",
            self.sql
        )
    }
}

impl std::error::Error for Unsupported {}

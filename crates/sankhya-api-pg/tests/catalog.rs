//! The catalogue queries real tools send before the user types anything.
//!
//! Each test names a client that actually issues the query in question. That matters
//! because the acceptance gate is a compatibility matrix, and a matrix is only meaningful
//! if the queries in it came from real clients rather than from imagination.
//!
//! The recurring theme: **a wrong answer here is worse than an error**. A tool that gets an
//! empty schema list shows an empty tree and the user concludes the database is empty. A
//! tool that gets the wrong `server_version_num` attempts syntax this server does not have.

// Tests may panic — that is how a test reports a failure. The workspace denies
// `unwrap`, `expect`, `panic` and indexing because a *server* must not do those things
// on data it did not choose; a test chooses all of its data, and an assertion that
// cannot fail loudly is worse than useless.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use sankhya_api_pg::catalog::{
    answer, known_types, recognise, startup_parameters, CatalogColumn, CatalogQuery, CatalogResult,
    CatalogTable,
};
use sankhya_api_pg::message::oid;

const VERSION: &str = "0.1.0";

fn tables() -> Vec<CatalogTable> {
    vec![
        CatalogTable {
            schema: "sales".to_string(),
            name: "orders".to_string(),
            columns: vec![
                CatalogColumn {
                    name: "id".to_string(),
                    type_name: "int8".to_string(),
                    type_oid: oid::INT8,
                    nullable: false,
                },
                CatalogColumn {
                    name: "region".to_string(),
                    type_name: "text".to_string(),
                    type_oid: oid::TEXT,
                    nullable: true,
                },
            ],
        },
        CatalogTable {
            schema: "hr".to_string(),
            name: "people".to_string(),
            columns: vec![CatalogColumn {
                name: "name".to_string(),
                type_name: "text".to_string(),
                type_oid: oid::TEXT,
                nullable: false,
            }],
        },
    ]
}

fn ask(sql: &str) -> CatalogResult {
    let query = recognise(sql).unwrap_or_else(|| panic!("not recognised: {sql}"));
    answer(&query, VERSION, "public", &tables())
}

fn cell(result: &CatalogResult, row: usize, column: usize) -> Option<String> {
    result.rows.get(row)?.get(column)?.clone()
}

// --- what tools send first ------------------------------------------------

#[test]
fn a_version_query_starts_with_the_string_every_client_parses() {
    // psql, JDBC, psycopg and every BI tool parse the major version out of this and refuse
    // to proceed if they cannot find it. What follows says truthfully what this is, so the
    // prefix does not mislead anyone reading it.
    let result = ask("SELECT version()");
    let version = cell(&result, 0, 0).expect("a version string");

    assert!(
        version.starts_with("PostgreSQL 17.0"),
        "clients parse the major version out of the prefix: {version}"
    );
    assert!(
        version.contains("SANKHYA"),
        "and it must say what this actually is: {version}"
    );
}

#[test]
fn a_connection_pool_liveness_check_is_answered() {
    // Almost every pool sends exactly this and closes the connection if it fails.
    let result = ask("SELECT 1");
    assert_eq!(cell(&result, 0, 0), Some("1".to_string()));
    assert_eq!(result.fields.first().map(|f| f.type_oid), Some(oid::INT4));
}

#[test]
fn the_settings_a_client_changes_its_behaviour_on_are_answered_correctly() {
    // server_version_num is compared numerically to decide which features to use. A wrong
    // answer makes a tool attempt syntax this server does not have.
    assert_eq!(
        cell(&ask("SHOW server_version_num"), 0, 0),
        Some("170000".to_string())
    );
    assert_eq!(
        cell(&ask("SHOW client_encoding"), 0, 0),
        Some("UTF8".to_string()),
        "announcing anything but UTF8 makes clients transcode text that is already UTF-8"
    );
    assert_eq!(
        cell(&ask("SHOW standard_conforming_strings"), 0, 0),
        Some("on".to_string()),
        "a client guessing wrong here corrupts string literals rather than failing"
    );
}

#[test]
fn a_current_setting_call_is_recognised_as_well_as_the_show_form() {
    // JDBC uses SHOW; several ORMs use current_setting(). Matching only one shape works for
    // the client it was written against and fails for the next.
    let via_function = ask("SELECT current_setting('server_version_num')");
    assert_eq!(cell(&via_function, 0, 0), Some("170000".to_string()));
}

#[test]
fn the_startup_parameters_a_client_needs_are_all_sent() {
    // Not optional. A client that does not receive client_encoding or
    // standard_conforming_strings has to guess, and several guess wrong in ways that
    // corrupt data rather than failing.
    let parameters = startup_parameters(VERSION);
    let names: Vec<&str> = parameters.iter().map(|(n, _)| n.as_str()).collect();

    for required in [
        "server_version",
        "client_encoding",
        "DateStyle",
        "integer_datetimes",
        "standard_conforming_strings",
    ] {
        assert!(
            names.contains(&required),
            "{required} is not sent at startup"
        );
    }
    assert!(parameters.iter().all(|(_, value)| !value.is_empty()));
}

// --- schema discovery -----------------------------------------------------

#[test]
fn a_schema_tree_query_returns_the_schemas_that_exist() {
    // A BI tool's first act after connecting. An empty result shows an empty tree and the
    // user concludes the database is empty.
    for sql in [
        "SELECT nspname FROM pg_namespace",
        "SELECT schema_name FROM information_schema.schemata",
    ] {
        let result = ask(sql);
        let schemas: Vec<Option<String>> = result
            .rows
            .iter()
            .filter_map(|r| r.first().cloned())
            .collect();
        assert_eq!(
            schemas,
            vec![Some("hr".to_string()), Some("sales".to_string())],
            "sorted and deduplicated, for {sql}"
        );
    }
}

#[test]
fn a_table_list_is_returned_in_the_shape_information_schema_uses() {
    let result = ask("SELECT * FROM information_schema.tables");
    assert_eq!(result.row_count(), 2);

    let names: Vec<&str> = result.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, vec!["table_schema", "table_name", "table_type"]);
    assert_eq!(
        cell(&result, 0, 2),
        Some("BASE TABLE".to_string()),
        "clients filter on this exact string"
    );
}

#[test]
fn a_table_list_can_be_narrowed_to_one_schema() {
    let result = ask("SELECT * FROM information_schema.tables WHERE table_schema = 'sales'");
    assert_eq!(result.row_count(), 1);
    assert_eq!(cell(&result, 0, 1), Some("orders".to_string()));
}

#[test]
fn a_column_list_reports_nullability_as_yes_or_no() {
    // information_schema uses the strings 'YES' and 'NO', and clients compare the string.
    // Returning true/false makes every column look non-nullable to a tool that compares.
    let result = ask("SELECT * FROM information_schema.columns WHERE table_name = 'orders'");
    assert_eq!(result.row_count(), 2);

    assert_eq!(cell(&result, 0, 2), Some("id".to_string()));
    assert_eq!(cell(&result, 0, 3), Some("1".to_string()), "one-based");
    assert_eq!(cell(&result, 0, 5), Some("NO".to_string()));
    assert_eq!(cell(&result, 1, 5), Some("YES".to_string()));
}

#[test]
fn the_pg_attribute_form_of_a_column_query_is_recognised_too() {
    // psql's \d uses pg_attribute; information_schema is what JDBC uses. Both have to work.
    let result = ask("SELECT attname FROM pg_attribute WHERE attrelid = 'orders'::regclass");
    assert!(result.row_count() >= 2);
}

#[test]
fn the_type_list_uses_real_oids() {
    // A client receiving an OID it does not recognise renders the value as an opaque
    // string, so an invented OID turns every integer column into text with no error.
    let result = ask("SELECT oid, typname FROM pg_type");
    assert_eq!(result.row_count(), known_types().len());
    assert!(result
        .rows
        .iter()
        .any(|r| r.first() == Some(&Some("20".to_string()))
            && r.get(1) == Some(&Some("int8".to_string()))));
}

#[test]
fn current_schema_is_answered() {
    assert_eq!(
        cell(&ask("SELECT current_schema()"), 0, 0),
        Some("public".to_string())
    );
    assert!(recognise("SELECT current_database()").is_some());
}

// --- what is deliberately not emulated ------------------------------------

#[test]
fn an_ordinary_query_is_not_mistaken_for_a_catalogue_one() {
    // The recogniser must not swallow real work. A SELECT against a user table that
    // happened to be answered from a fake catalogue would return the wrong rows silently.
    assert_eq!(recognise("SELECT id, region FROM orders"), None);
    assert_eq!(recognise("INSERT INTO orders VALUES (1)"), None);
    assert_eq!(recognise("SELECT count(*) FROM sales.orders"), None);
}

#[test]
fn an_unrecognised_catalogue_query_is_not_answered_with_nothing() {
    // An empty result is indistinguishable from "you have no tables", and that sends
    // someone looking in entirely the wrong place. The caller gets None and is expected to
    // raise a named error.
    assert_eq!(
        recognise("SELECT * FROM pg_stat_activity"),
        None,
        "a query about something not emulated must not silently produce an empty answer"
    );
}

#[test]
fn recognition_survives_the_formatting_tools_actually_produce() {
    // No two tools generate the same text. Matching literally would work for the client it
    // was written against and fail for the next, which is exactly the failure this module
    // exists to avoid.
    let variants = [
        "select VERSION()",
        "  SELECT version();  ",
        "SELECT\n  version()\n",
        "SELECT version() ;",
    ];
    for sql in variants {
        assert_eq!(
            recognise(sql),
            Some(CatalogQuery::Version),
            "not recognised: {sql:?}"
        );
    }
}

#[test]
fn a_setting_this_server_does_not_have_returns_empty_rather_than_failing() {
    // PostgreSQL itself errors on an unknown setting, but a tool probing for an optional
    // one treats an error as fatal and an empty value as absent. Absent is what it is.
    let result = ask("SHOW some_extension_setting");
    assert_eq!(cell(&result, 0, 0), Some(String::new()));
}

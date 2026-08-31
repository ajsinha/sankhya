//! `CREATE TABLE ... CLONE`, and the far more important question of what it declines to read.
//!
//! # The requirement that shapes this file
//!
//! The statement has to be recognised *before* the engine is asked, because the engine's parser
//! rejects it. That makes this a pre-filter on every statement the server receives, and a
//! pre-filter's dangerous failure is not rejecting a clone — it is **claiming something that was
//! never its business**. `CREATE TABLE orders (id BIGINT)` must reach the engine untouched, and
//! so must malformed SQL, whose error should come from the thing that owns the language.
//!
//! So most of these tests assert `None`.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_clone::ddl::{parse, Create, DdlError, Statement};

#[test]
fn a_clone_at_a_version_is_read() {
    assert_eq!(
        parse("CREATE TABLE staging CLONE entries AT VERSION 40"),
        Some(Ok(Statement::Create(Create {
            table: "staging".to_string(),
            origin: "entries".to_string(),
            version: Some(40)
        })))
    );
}

#[test]
fn a_clone_with_no_version_carries_none_rather_than_a_guess() {
    // "The newest version" is a question about a warehouse, and resolving it here would resolve
    // it at parse time — a different moment from the one the clone is made at.
    assert_eq!(
        parse("CREATE TABLE staging CLONE entries"),
        Some(Ok(Statement::Create(Create {
            table: "staging".to_string(),
            origin: "entries".to_string(),
            version: None
        })))
    );
}

#[test]
fn an_ordinary_create_table_is_not_ours() {
    // The failure that matters. A pre-filter that claimed this would break every table anybody
    // creates, and it would do it before the engine ever saw the statement.
    for sql in [
        "CREATE TABLE orders (id BIGINT, amount DECIMAL(38,9))",
        "CREATE TABLE orders AS SELECT * FROM entries",
        "CREATE TABLE IF NOT EXISTS orders (id BIGINT)",
        "CREATE EXTERNAL TABLE orders STORED AS PARQUET LOCATION '/data'",
    ] {
        assert_eq!(parse(sql), None, "claimed `{sql}`");
    }
}

#[test]
fn nothing_that_is_not_a_create_table_is_ours() {
    for sql in [
        "SELECT * FROM entries",
        "CREATE CUBE sales FROM entries",
        "CREATE VIEW v AS SELECT 1",
        "INSERT INTO entries VALUES (1)",
        "",
        "   ",
    ] {
        assert_eq!(parse(sql), None, "claimed `{sql}`");
    }
}

#[test]
fn a_query_that_merely_mentions_cloning_is_not_ours() {
    // The word appears in ordinary text and ordinary identifiers. Matching on it anywhere would
    // be a pre-filter that fails on somebody else's data.
    for sql in [
        "SELECT clone FROM entries",
        "SELECT * FROM clone_registry",
        "CREATE VIEW clone AS SELECT 1",
    ] {
        assert_eq!(parse(sql), None, "claimed `{sql}`");
    }
}

#[test]
fn malformed_sql_that_is_not_ours_still_reaches_the_engine() {
    // Its error must come from the thing that owns the language, not from a pre-filter that
    // happened to look first and had an opinion.
    for sql in ["CREATE TABLE", "CREATE TABLE (", "CREATE", "CRETAE TABLE x CLONE y"] {
        assert_eq!(parse(sql), None, "claimed `{sql}`");
    }
}

#[test]
fn the_keywords_are_case_insensitive() {
    for sql in [
        "create table staging clone entries at version 40",
        "CrEaTe TaBlE staging ClOnE entries At VeRsIoN 40",
    ] {
        let Statement::Create(parsed) = parse(sql).expect("ours").expect("read") else {
            panic!("a create")
        };
        assert_eq!(parsed.version, Some(40));
        assert_eq!(parsed.origin, "entries");
    }
}

#[test]
fn a_table_called_clone_is_a_table_somebody_may_already_have() {
    // Keywords are not reserved. Refusing this would be the parser deciding what names a
    // warehouse may use, which is not a decision it is entitled to make.
    let Statement::Create(parsed) = parse("CREATE TABLE clone CLONE entries")
        .expect("ours")
        .expect("read")
    else {
        panic!("a create")
    };
    assert_eq!(parsed.table, "clone");
    assert_eq!(parsed.origin, "entries");
}

#[test]
fn a_quoted_identifier_keeps_its_case_and_loses_its_quotes() {
    let Statement::Create(parsed) = parse(r#"CREATE TABLE "Staging" CLONE "Entries" AT VERSION 1"#)
        .expect("ours")
        .expect("read")
    else {
        panic!("a create")
    };
    assert_eq!(parsed.table, "Staging");
    assert_eq!(parsed.origin, "Entries");
}

#[test]
fn a_qualified_name_survives() {
    let Statement::Create(parsed) = parse("CREATE TABLE dev.staging CLONE prod.entries")
        .expect("ours")
        .expect("read")
    else {
        panic!("a create")
    };
    assert_eq!(parsed.table, "dev.staging");
    assert_eq!(parsed.origin, "prod.entries");
}

#[test]
fn a_trailing_semicolon_is_not_part_of_the_name() {
    let Statement::Create(parsed) = parse("CREATE TABLE staging CLONE entries;")
        .expect("ours")
        .expect("read")
    else {
        panic!("a create")
    };
    assert_eq!(parsed.origin, "entries");
}

#[test]
fn a_statement_that_began_as_ours_and_is_wrong_is_an_error_rather_than_a_pass() {
    // Once `CLONE` is there the statement is ours, and reporting `None` would hand the engine
    // something it will reject with a worse message than we can give.
    assert_eq!(
        parse("CREATE TABLE staging CLONE"),
        Some(Err(DdlError::Expected { wanted: "the table to clone", found: None }))
    );
    assert_eq!(
        parse("CREATE TABLE staging CLONE entries AT 40"),
        Some(Err(DdlError::Expected {
            wanted: "VERSION after AT",
            found: Some("40".to_string())
        }))
    );
    assert_eq!(
        parse("CREATE TABLE staging CLONE entries AT VERSION yesterday"),
        Some(Err(DdlError::UnreadableVersion { found: "yesterday".to_string() }))
    );
}

#[test]
fn a_clause_this_does_not_understand_is_refused_rather_than_ignored() {
    // Ignoring it would be answering a statement nobody wrote. The unread clause may be the one
    // that changes what was meant.
    let refusal = parse("CREATE TABLE staging CLONE entries AT VERSION 40 SHALLOW")
        .expect("ours")
        .expect_err("trailing");
    assert_eq!(refusal, DdlError::Trailing { found: "SHALLOW".to_string() });
    assert!(refusal.to_string().contains("changes what was meant"));
}

#[test]
fn an_error_names_a_position_rather_than_a_rule() {
    // The split with `may_clone`: a syntax error says what was written, a refusal says why it
    // may not happen. This module has never seen a warehouse and must not pretend to have.
    let said = DdlError::Expected { wanted: "a table name", found: Some("(".to_string()) }
        .to_string();
    assert!(said.contains("expected a table name"), "{said}");
    assert!(said.contains('('), "{said}");
}

#[test]
fn a_drop_table_is_read_but_not_decided() {
    // Read here, answered elsewhere. Whether `staging` is a clone is a question about a
    // warehouse this module has never seen, so the statement is handed back for somebody who
    // can answer it — and handed *back untouched* if the table turns out not to be a clone,
    // where the server's existing "this is a read path" refusal answers it in its own words.
    assert_eq!(
        parse("DROP TABLE staging"),
        Some(Ok(Statement::Drop { table: "staging".to_string(), if_exists: false }))
    );
    assert_eq!(
        parse("DROP TABLE IF EXISTS staging"),
        Some(Ok(Statement::Drop { table: "staging".to_string(), if_exists: true }))
    );
}

#[test]
fn a_dropped_name_keeps_its_quoting_and_qualification() {
    assert_eq!(
        parse(r#"DROP TABLE "Staging";"#),
        Some(Ok(Statement::Drop { table: "Staging".to_string(), if_exists: false }))
    );
    assert_eq!(
        parse("drop table dev.staging"),
        Some(Ok(Statement::Drop { table: "dev.staging".to_string(), if_exists: false }))
    );
}

#[test]
fn a_drop_of_something_that_is_not_a_table_is_not_ours() {
    for sql in ["DROP CUBE sales", "DROP VIEW v", "DROP SCHEMA dev", "DROP"] {
        assert_eq!(parse(sql), None, "claimed `{sql}`");
    }
}

#[test]
fn a_drop_with_a_clause_this_does_not_understand_is_refused() {
    assert_eq!(
        parse("DROP TABLE staging CASCADE"),
        Some(Err(DdlError::Trailing { found: "CASCADE".to_string() }))
    );
}

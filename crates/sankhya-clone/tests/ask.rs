//! Reading the two questions a client may ask about a clone.
//!
//! # What these are really guarding
//!
//! Over-claiming. This parser sits in front of every `SHOW` a client sends, and a
//! catalogue-browsing driver sends several on connection. A parser that answers `Some` for a
//! statement it does not understand breaks a client to answer a question it never asked ---
//! which is exactly how `SHOW FEEDS` came to be answered by the catalogue as an empty setting,
//! one layer up.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use sankhya_clone::ask::{parse, NotAQuestion, Question};

#[test]
fn lineage_and_dependents_are_read() {
    assert_eq!(
        parse("SHOW LINEAGE OF orders"),
        Some(Ok(Question::Lineage { table: "orders".to_owned() }))
    );
    assert_eq!(
        parse("SHOW DEPENDENTS OF orders"),
        Some(Ok(Question::Dependents { table: "orders".to_owned() }))
    );
}

#[test]
fn case_whitespace_and_a_trailing_semicolon_are_all_the_same_statement() {
    // Somebody typing this into a SQL prompt writes it however they write everything else.
    for sql in [
        "show lineage of orders",
        "  SHOW   LINEAGE   OF   orders  ",
        "SHOW LINEAGE OF orders;",
        "ShOw LiNeAgE oF orders",
    ] {
        assert_eq!(
            parse(sql),
            Some(Ok(Question::Lineage { table: "orders".to_owned() })),
            "{sql}"
        );
    }
}

#[test]
fn a_quoted_table_is_the_same_table() {
    for sql in [
        "SHOW LINEAGE OF 'orders'",
        "SHOW LINEAGE OF \"orders\"",
        "SHOW LINEAGE OF `orders`",
    ] {
        assert_eq!(
            parse(sql),
            Some(Ok(Question::Lineage { table: "orders".to_owned() })),
            "{sql}"
        );
    }
}

#[test]
fn every_other_statement_is_handed_back_untouched() {
    // The property that matters most. `None` means the caller passes it on; anything else
    // here would be this module answering a question somebody asked the engine.
    for sql in [
        "SELECT 1",
        "SHOW",
        "SHOW server_version_num",
        "SHOW FEEDS",
        "SHOW TIME ZONE",
        "SHOW ALL",
        "CREATE TABLE a CLONE b",
        "",
        "   ",
    ] {
        assert_eq!(parse(sql), None, "{sql} was claimed and is not ours");
    }
}

#[test]
fn a_question_with_no_table_is_an_error_rather_than_a_statement_for_the_engine() {
    // Once `SHOW LINEAGE` has been read, the statement cannot be anything else — so handing
    // it back would produce the engine's "syntax error near LINEAGE" for a statement this
    // server does implement, which sends somebody to fix a typo that is not there.
    let refused = parse("SHOW LINEAGE").expect("claimed").expect_err("no table");
    assert_eq!(refused, NotAQuestion::NoTableNamed { question: "LINEAGE" });
    assert!(refused.to_string().contains("needs the name of a table"));

    let refused = parse("SHOW DEPENDENTS").expect("claimed").expect_err("no table");
    assert_eq!(refused, NotAQuestion::NoTableNamed { question: "DEPENDENTS" });
}

#[test]
fn a_missing_of_says_what_the_statement_reads() {
    let refused = parse("SHOW LINEAGE orders").expect("claimed").expect_err("no OF");
    assert_eq!(
        refused,
        NotAQuestion::ExpectedOf { found: Some("orders".to_owned()) }
    );
    assert!(refused.to_string().contains("SHOW LINEAGE OF <table>"), "{refused}");

    // `OF` present and the table missing is the same mistake from the other side.
    let refused = parse("SHOW LINEAGE OF").expect("claimed").expect_err("no table");
    assert_eq!(refused, NotAQuestion::ExpectedOf { found: None });
}

#[test]
fn words_after_the_table_are_refused_rather_than_ignored() {
    // Ignoring them would make `SHOW LINEAGE OF orders AT VERSION 3` answer a question about
    // `orders` while looking like it answered one about a version.
    let refused = parse("SHOW LINEAGE OF orders AT VERSION 3")
        .expect("claimed")
        .expect_err("trailing");
    assert_eq!(refused, NotAQuestion::Trailing { after: "AT".to_owned() });
    assert!(refused.to_string().contains("one table and nothing else"), "{refused}");
}

#[test]
fn the_question_names_its_table() {
    assert_eq!(
        Question::Lineage { table: "orders".to_owned() }.table(),
        "orders"
    );
    assert_eq!(
        Question::Dependents { table: "orders".to_owned() }.table(),
        "orders"
    );
}

//! The statements a feed answers, and the far larger set it must leave alone.
//!
//! # The failure worth guarding
//!
//! A parser that claims too much is worse than one that claims too little: a statement this
//! took by mistake is one the engine never sees, and the user gets a refusal about feeds for a
//! query that had nothing to do with them.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_feed::command::{parse, Command, CommandError};

#[test]
fn showing_feeds_is_a_feed_command() {
    for sql in ["SHOW FEEDS", "show feeds", "  SHOW   feeds  ;", "Show Feeds;"] {
        assert_eq!(parse(sql), Some(Ok(Command::Show)), "{sql}");
    }
}

#[test]
fn resuming_names_a_feed_however_it_is_quoted() {
    // A user who has been typing SQL all day will quote it, and refusing that would be
    // pedantry with an error message attached.
    for sql in [
        "RESUME FEED orders",
        "resume feed 'orders'",
        "RESUME FEED \"orders\"",
        "RESUME FEED `orders`;",
    ] {
        assert_eq!(
            parse(sql),
            Some(Ok(Command::Resume { feed: "orders".to_owned() })),
            "{sql}"
        );
    }
}

#[test]
fn resuming_nothing_is_refused_and_says_where_the_names_are() {
    let refused = parse("RESUME FEED").expect("a feed command").expect_err("no name");

    assert_eq!(refused, CommandError::NoFeedNamed);
    assert!(refused.to_string().contains("SHOW FEEDS"), "{refused}");
}

#[test]
fn words_after_a_complete_statement_are_refused_rather_than_ignored() {
    // Ignoring them is how `RESUME FEED orders AND ALSO sessions` resumes one feed and reports
    // success, leaving somebody certain they resumed two.
    for sql in ["SHOW FEEDS NOW", "RESUME FEED orders please"] {
        match parse(sql).expect("a feed command").expect_err("trailing words") {
            CommandError::Trailing { after } => assert!(!after.is_empty(), "{sql}"),
            other => panic!("expected trailing words for `{sql}`, got {other:?}"),
        }
    }
}

#[test]
fn everything_else_is_the_engines() {
    // The important half. A statement this took by mistake is one the engine never sees, and
    // the user gets a refusal about feeds for a query that had nothing to do with them.
    for sql in [
        "SELECT * FROM orders",
        "SHOW server_version_num",
        "SHOW TABLES",
        "SELECT 'RESUME FEED orders'",
        "CREATE TABLE orders (id BIGINT)",
        "RESUMEFEED orders",
        "",
        "   ",
    ] {
        assert_eq!(parse(sql), None, "`{sql}` is not a feed command");
    }
}

//! Reading the statements a client uses to take, list and drop a snapshot.
//!
//! # What these are really guarding
//!
//! Two things. **Over-claiming**, because this parser sits in front of every `SHOW`, `CREATE`
//! and `DROP` a client sends, and one that answers `Some` for a statement it does not
//! understand breaks a client to answer a question it never asked. And **the mandatory
//! expiry**, because a parser that quietly supplied a default would undo `ADR-0019` Decision 3
//! without anybody editing the decision.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use sankhya_snapshot::expire::{Unaskable, LONGEST_DAYS};
use sankhya_snapshot::statement::{parse, NotAStatement, Statement};

#[test]
fn a_snapshot_is_taken_with_a_name_and_a_lifetime() {
    let taken = parse("CREATE SNAPSHOT eod EXPIRE AFTER 90 DAYS")
        .expect("claimed")
        .expect("a statement");
    match taken {
        Statement::Create { name, expiry } => {
            assert_eq!(name, "eod");
            assert_eq!(expiry.count(), 90);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn case_whitespace_and_a_trailing_semicolon_are_all_the_same_statement() {
    for sql in [
        "create snapshot eod expire after 90 days",
        "  CREATE   SNAPSHOT   eod   EXPIRE   AFTER   90   DAYS  ",
        "CREATE SNAPSHOT eod EXPIRE AFTER 90 DAYS;",
        "CrEaTe SnApShOt eod ExPiRe AfTeR 90 DaY",
    ] {
        assert!(
            matches!(
                parse(sql).expect("claimed").expect("a statement"),
                Statement::Create { .. }
            ),
            "{sql}"
        );
    }
}

#[test]
fn omitting_the_expiry_explains_the_decision_rather_than_naming_a_missing_word() {
    // Somebody who omitted it did not forget a keyword --- they expected a default, and there
    // is deliberately none. A message naming a missing token would send them looking for the
    // right syntax rather than telling them why there is no default.
    let refused = parse("CREATE SNAPSHOT eod").expect("claimed").expect_err("no expiry");
    assert_eq!(refused, NotAStatement::NoExpiry);

    let said = refused.to_string();
    assert!(said.contains("no default and no unbounded form"), "{said}");
    assert!(said.contains("pins files"), "{said}");
    assert!(said.contains("did not ask for it"), "{said}");
}

#[test]
fn a_lifetime_this_system_will_not_honour_is_refused_at_the_statement() {
    // Refused here rather than at the moment of writing the document, so the message arrives
    // while the person is still looking at what they typed.
    let refused = parse("CREATE SNAPSHOT eod EXPIRE AFTER 0 DAYS")
        .expect("claimed")
        .expect_err("zero");
    assert_eq!(refused, NotAStatement::Unaskable(Unaskable::Immediate));

    let too_long = format!("CREATE SNAPSHOT eod EXPIRE AFTER {} DAYS", LONGEST_DAYS + 1);
    let refused = parse(&too_long).expect("claimed").expect_err("too long");
    assert!(matches!(
        refused,
        NotAStatement::Unaskable(Unaskable::TooLong { .. })
    ));
}

#[test]
fn a_lifetime_that_is_not_a_number_names_what_was_wanted() {
    let refused = parse("CREATE SNAPSHOT eod EXPIRE AFTER soon DAYS")
        .expect("claimed")
        .expect_err("not a number");
    assert_eq!(
        refused,
        NotAStatement::Expected {
            wanted: "a number of days",
            found: Some("soon".to_owned()),
        }
    );
}

#[test]
fn snapshots_are_listed_and_dropped() {
    assert_eq!(parse("SHOW SNAPSHOTS").expect("claimed"), Ok(Statement::Show));
    assert_eq!(
        parse("DROP SNAPSHOT eod").expect("claimed"),
        Ok(Statement::Drop { name: "eod".to_owned(), if_exists: false })
    );
    assert_eq!(
        parse("DROP SNAPSHOT IF EXISTS eod").expect("claimed"),
        Ok(Statement::Drop { name: "eod".to_owned(), if_exists: true })
    );
}

#[test]
fn a_quoted_name_is_the_same_name() {
    for sql in [
        "DROP SNAPSHOT 'eod'",
        "DROP SNAPSHOT \"eod\"",
        "DROP SNAPSHOT `eod`",
    ] {
        assert_eq!(
            parse(sql).expect("claimed"),
            Ok(Statement::Drop { name: "eod".to_owned(), if_exists: false }),
            "{sql}"
        );
    }
}

#[test]
fn every_other_statement_is_handed_back_untouched() {
    // The property that matters most. This parser sits in front of every `SHOW`, `CREATE` and
    // `DROP` a client sends --- and a catalogue-browsing driver sends several on connection.
    // Anything but `None` here is this module answering a question somebody asked the engine.
    for sql in [
        "SELECT 1",
        "SHOW",
        "SHOW FEEDS",
        "SHOW LINEAGE OF orders",
        "SHOW server_version_num",
        "SHOW TRANSACTION ISOLATION LEVEL",
        "CREATE TABLE a CLONE b",
        "CREATE CUBE sales FROM orders",
        "DROP TABLE orders",
        "DROP CUBE sales",
        "",
        "   ",
    ] {
        assert_eq!(parse(sql), None, "`{sql}` was claimed and is not ours");
    }
}

#[test]
fn a_statement_that_began_as_ours_and_is_wrong_is_an_error_rather_than_a_pass() {
    // Once `CREATE SNAPSHOT` has been read, the statement cannot be anything else --- so
    // handing it back would produce the engine's "syntax error" for a statement this server
    // does implement, which sends somebody to fix a typo that is not there.
    for sql in [
        "CREATE SNAPSHOT",
        "CREATE SNAPSHOT eod EXPIRE 90 DAYS",
        "CREATE SNAPSHOT eod EXPIRE AFTER 90 WEEKS",
        "DROP SNAPSHOT",
    ] {
        assert!(
            parse(sql).expect("claimed").is_err(),
            "`{sql}` was handed back to the engine"
        );
    }
}

#[test]
fn words_after_a_complete_statement_are_refused_rather_than_ignored() {
    // Ignoring them would make `SHOW SNAPSHOTS OF sales` answer a question about the whole
    // warehouse while looking like it answered one about a schema.
    assert_eq!(
        parse("SHOW SNAPSHOTS OF sales").expect("claimed").expect_err("trailing"),
        NotAStatement::Trailing { after: "OF".to_owned() }
    );
    assert_eq!(
        parse("DROP SNAPSHOT eod CASCADE").expect("claimed").expect_err("trailing"),
        NotAStatement::Trailing { after: "CASCADE".to_owned() }
    );
    assert_eq!(
        parse("CREATE SNAPSHOT eod EXPIRE AFTER 90 DAYS NOW")
            .expect("claimed")
            .expect_err("trailing"),
        NotAStatement::Trailing { after: "NOW".to_owned() }
    );
}

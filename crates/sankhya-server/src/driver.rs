//! The statements a *driver* sends around a query.
//!
//! # Why these are answered rather than refused
//!
//! A driver's first act on a connection is a handful of statements it does not think of as
//! statements: a float-precision setting, an application name, a transaction it will commit
//! immediately, a `DISCARD ALL` when it returns the connection to a pool. Refusing them is
//! refusing the driver, which makes this door useless to every client it exists for.
//!
//! `SET` was refused as `XX000` --- a *fatal server configuration error* --- which makes a pool
//! discard the connection and try again, forever.
//!
//! # Its own module
//!
//! `wiring.rs` is the composition root and reaches the hard length limit every time a
//! capability lands. The right answer to that is not a larger limit: a file nobody can hold in
//! their head is where a statement comes to be intercepted twice, or not at all.

use sankhya_api_pg::session::{QueryFailure, QueryResult};

use crate::wiring::refusal;

/// Answer a statement a driver sends around a query, or `None` if it is not one.
///
/// # Why these are answered rather than refused
///
/// A driver's first act on a connection is a handful of statements it does not think of as
/// statements: a float-precision setting, an application name, a transaction it will
/// immediately commit. Refusing them is refusing the driver, which makes this door useless
/// to every client it was built for.
///
/// # Why `ROLLBACK` is not among them
///
/// Because it is the one whose meaning we would be faking. This server writes nothing, so
/// `BEGIN` and `COMMIT` are true statements about a transaction of one statement --- but a
/// client that asks to *undo* and is told it worked has been lied to about the only thing
/// it wanted. It is refused, and the refusal says why.
/// The row-locking clause a statement carries, if it carries one.
///
/// Text rather than plan, because the plan does not have it: `datafusion-sql` destructures
/// `locks` away during parsing --- three lines from where it returns `not_impl_err` for
/// `FETCH` --- so by the time a `LogicalPlan` exists the clause is gone without a word.
///
/// Single-quoted literals are removed before looking, so `WHERE note = 'for update'` selects a
/// row rather than refusing the statement that selects it. The scanner toggles on each quote,
/// so a doubled quote inside a literal toggles twice and leaves the state where it was.
fn names_a_lock(sql: &str) -> Option<&'static str> {
    let mut outside = String::with_capacity(sql.len());
    let mut in_literal = false;
    for c in sql.chars() {
        if c == '\'' {
            in_literal = !in_literal;
            outside.push(' ');
        } else if in_literal {
            outside.push(' ');
        } else {
            outside.push(c.to_ascii_uppercase());
        }
    }
    let words: Vec<&str> = outside.split_whitespace().collect();
    for (at, word) in words.iter().enumerate() {
        if *word != "FOR" {
            continue;
        }
        match words.get(at + 1..).unwrap_or(&[]) {
            ["NO", "KEY", "UPDATE", ..] => return Some("FOR NO KEY UPDATE"),
            ["KEY", "SHARE", ..] => return Some("FOR KEY SHARE"),
            ["UPDATE", ..] => return Some("FOR UPDATE"),
            ["SHARE", ..] => return Some("FOR SHARE"),
            _ => {}
        }
    }
    None
}


pub(crate) fn run_session_statement(
    sql: &str,
) -> Option<Result<QueryResult, QueryFailure>> {
    let compact = sql.trim().trim_end_matches(';').trim().to_uppercase();
    let first = compact.split_whitespace().next().unwrap_or_default();

    // A setting that would change an *answer* is refused, never accepted and ignored.
    //
    // The generic `SET` below is a no-op because nothing here reads a session setting, and that
    // is true --- today. `SET SNAPSHOT` is the first setting that would change what a statement
    // returns, and accepting it as a no-op would be the worst defect available: a caller who
    // asked to read one instant, served *now*, with no symptom at all.
    //
    // `ADR-0019` Decision 6 names this trap, and `DEC-47`'s rule already covers it: a statement
    // whose meaning is not implemented is refused, never confirmed and discarded.
    //
    // Listed by name rather than matched by a pattern. A pattern broad enough to be safe would
    // refuse the settings drivers need, and one narrow enough to be convenient would fail open
    // --- and failing open here is indistinguishable from working.
    // `SNAPSHOT` is no longer here: it is **honoured** now rather than refused, which is the
    // only other acceptable answer. `READ_AS_OF` stays, because nothing reads it.
    const CHANGES_AN_ANSWER: &[&str] = &["READ_AS_OF"];
    // Settings that make a promise this build cannot keep.
    //
    // These were accepted with the tag `SET` and no effect, and the comment beside
    // `CHANGES_AN_ANSWER` asserted there were none of them --- *"a setting that did change an
    // answer would have to be refused instead, and there is none"*. There are four.
    //
    // An isolation level is a claim about what concurrent statements may observe, and this
    // server cannot honour any of them: `BEGIN` is a no-op and every statement re-resolves its
    // tables, so two reads inside one transaction can see two versions. `SET ROLE` is worse
    // than a no-op --- an application that drops to a lesser role before running untrusted SQL
    // is told it succeeded and continues at full privilege.
    //
    // Matched on the statement rather than on its second word, because `SET SESSION` is also
    // the ordinary form for every harmless setting and refusing all of it would break the
    // connection handshake of every driver.
    let promises_more_than_it_can_keep = compact.contains("ISOLATION LEVEL")
        || compact.starts_with("SET ROLE")
        || compact.starts_with("SET LOCAL ROLE")
        || compact.starts_with("SET SESSION AUTHORIZATION");
    if promises_more_than_it_can_keep {
        return Some(Err(refusal(
            "0A000",
            "this server cannot honour that setting, and it is refused rather than accepted \
             quietly. An isolation level is a claim about what concurrent statements may \
             observe --- and here `BEGIN` is a no-op and every statement re-resolves its \
             tables, so two reads inside one transaction may see two versions. `SET ROLE` is \
             refused for the same reason in the other direction: an application that drops \
             privilege before running untrusted SQL must not be told that it worked",
        )));
    }
    if matches!(first, "SET" | "RESET") {
        let named = compact
            .split_whitespace()
            .nth(1)
            .unwrap_or_default()
            .trim_end_matches('=')
            .trim_matches('"');
        if CHANGES_AN_ANSWER.contains(&named) {
            return Some(Err(refusal(
                // `0A000`, feature_not_supported: a driver reads that as "never", which is
                // right until `M17` builds it.
                "0A000",
                &format!(
                    "`{named}` is not a setting this build honours, and it is refused rather \
                     than accepted quietly because it would change what a statement returns. \
                     A caller who asked to read one instant and was served the present would \
                     have no way to tell. Named snapshots are M17; see ADR-0019"
                ),
            )));
        }
    }

    // A lock clause is refused rather than dropped on the floor.
    //
    // `SELECT ... FOR UPDATE` reaches this server as an ordinary projection: DataFusion's
    // parser destructures `locks` away without a word (`FETCH`, three lines from it in the same
    // file, returns `not_impl_err`). So the one statement whose entire purpose is mutual
    // exclusion was answered with rows and no lock of any kind --- and an application ported
    // here on a pessimistic-locking design reads as working while having none.
    //
    // Detected in the text because the plan no longer carries it. Quoted literals are stripped
    // first, so a row containing the words does not refuse the statement selecting it.
    if let Some(clause) = names_a_lock(sql) {
        return Some(Err(refusal(
            "0A000",
            &format!(
                "`{clause}` is refused rather than ignored. This server is a read path over a \
                 published warehouse and has no lock manager, so the clause could only ever be \
                 discarded --- and a statement whose whole purpose is mutual exclusion, \
                 answered with rows and no lock, is indistinguishable from one that worked"
            ),
        )));
    }

    // A transaction of one statement, which is what a read path has.
    let tag = match first {
        "BEGIN" | "START" => "BEGIN",
        "COMMIT" | "END" => "COMMIT",
        "SET" | "RESET" => "SET",
        "DISCARD" => "DISCARD ALL",
        "ROLLBACK" | "ABORT" => {
            return Some(Err(refusal(
                // `25P01`, no_active_sql_transaction: there is nothing to undo, which is
                // the truthful answer rather than a cheerful one.
                "25P01",
                "there is nothing to roll back: this server is a read path over a \
                 published warehouse, so a statement is its own transaction and nothing \
                 it has done can be undone. Answering `ROLLBACK` with success would be \
                 the one lie that matters",
            )))
        }
        _ => return None,
    };
    // `SET` and `RESET` of a real setting are accepted and have no effect, which is true:
    // nothing here reads a session setting. A setting that *did* change an answer would
    // have to be refused instead, and there is none.
    Some(Ok(QueryResult { fields: Vec::new(), rows: Vec::new(), tag: tag.to_owned() }))
}

#[cfg(test)]
mod lock_clauses {
    #![allow(clippy::expect_used, clippy::panic)]
    use super::{names_a_lock, run_session_statement};

    #[test]
    fn every_spelling_of_a_row_lock_is_seen() {
        for (sql, expected) in [
            ("SELECT balance FROM accounts WHERE id = 42 FOR UPDATE", "FOR UPDATE"),
            ("select * from t for share", "FOR SHARE"),
            ("SELECT * FROM t FOR NO KEY UPDATE", "FOR NO KEY UPDATE"),
            ("SELECT * FROM t FOR KEY SHARE", "FOR KEY SHARE"),
            ("SELECT * FROM t FOR UPDATE OF t NOWAIT", "FOR UPDATE"),
        ] {
            assert_eq!(names_a_lock(sql), Some(expected), "{sql}");
        }
    }

    #[test]
    fn a_row_that_says_for_update_is_still_selectable() {
        // The clause is found in text because the plan no longer carries it, so the one way
        // this can go wrong is refusing a statement over data that happens to contain the
        // words. Literals are stripped before looking.
        assert_eq!(names_a_lock("SELECT * FROM notes WHERE body = 'for update'"), None);
        assert_eq!(names_a_lock("SELECT 'FOR SHARE' AS why FROM t"), None);
        // And an ordinary `FOR` that begins nothing.
        assert_eq!(names_a_lock("SELECT * FROM t WHERE reason = 4"), None);
    }

    #[test]
    fn a_lock_clause_is_refused_rather_than_answered_with_rows() {
        let refused = run_session_statement("SELECT * FROM t FOR UPDATE")
            .expect("a lock clause is a statement this layer answers")
            .expect_err("and the answer is a refusal");
        assert!(
            format!("{}", refused.message).contains("no lock manager"),
            "the refusal says why: {}",
            refused.message
        );
    }

    #[test]
    fn a_setting_this_build_cannot_honour_is_refused() {
        for sql in [
            "SET TRANSACTION ISOLATION LEVEL SERIALIZABLE",
            "SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL REPEATABLE READ",
            "SET ROLE analyst",
            "SET SESSION AUTHORIZATION analyst",
        ] {
            let answered = run_session_statement(sql).expect("this layer answers a SET");
            assert!(answered.is_err(), "accepted quietly: {sql}");
        }
    }

    #[test]
    fn an_ordinary_setting_is_still_a_no_op() {
        // Refusing `SET` broadly would break the handshake of every driver, which is a worse
        // defect than the one being fixed.
        for sql in [
            "SET application_name = 'psql'",
            "SET extra_float_digits = 3",
            "SET SESSION application_name = 'jdbc'",
            "SET client_encoding TO 'UTF8'",
        ] {
            let answered = run_session_statement(sql).expect("this layer answers a SET");
            assert!(answered.is_ok(), "refused a harmless setting: {sql}");
        }
    }
}

//! What a table is a clone of, and what still reads it — asked from a client.
//!
//! # Why this test goes over a socket
//!
//! `M10` built cloning and left both questions unaskable, and `M13`'s exit demonstration
//! showed what that class of gap costs: `SHOW FEEDS` was built, wired, unit-tested and
//! mutation-tested, and did not work over the wire, because the layer above the handler
//! answered it first. Every one of those tests called the handler directly.
//!
//! So these drive the real binary through the real protocol. A statement that a client cannot
//! send is not a feature, however well the function behind it is tested.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use common::{query_outcome, start, text_rows, write_warehouse};

/// A warehouse with the sample tables, and a running server.
fn running() -> (tempfile::TempDir, common::Running) {
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let server = start(&warehouse, &dir.path().join("data"));
    (dir, server)
}

#[test]
fn a_clone_says_what_it_is_a_clone_of() {
    let (_dir, server) = running();
    query_outcome(server.port, "CREATE TABLE q3_frozen CLONE orders").expect("cloning");

    let chain = text_rows(server.port, "SHOW LINEAGE OF q3_frozen");
    assert_eq!(chain.len(), 1, "one step: it is a clone of orders and orders is not a clone");
    assert_eq!(chain[0][0].as_deref(), Some("1"), "the first step");
    assert_eq!(
        chain[0][1].as_deref(),
        Some("sales.orders"),
        "qualified, because a lineage outlives the moment a bare name was unambiguous"
    );
    assert!(
        chain[0][2].as_ref().is_some_and(|version| version.parse::<u64>().is_ok()),
        "and the origin version it reads, which is what makes its numbers placeable: {:?}",
        chain[0][2]
    );
}

#[test]
fn a_clone_of_a_clone_names_the_whole_chain_nearest_first() {
    // The question a single step cannot answer. `q3_frozen` is a snapshot of `orders`, and
    // `q3_audit` is a snapshot of that snapshot — so "what is this ultimately a view of?" has
    // an answer only if the chain is walked.
    let (_dir, server) = running();
    query_outcome(server.port, "CREATE TABLE q3_frozen CLONE orders").expect("cloning");
    query_outcome(server.port, "CREATE TABLE q3_audit CLONE q3_frozen").expect("cloning again");

    let chain = text_rows(server.port, "SHOW LINEAGE OF q3_audit");
    let names: Vec<Option<String>> = chain.iter().map(|row| row[1].clone()).collect();
    assert_eq!(
        names,
        vec![Some("sales.q3_frozen".to_owned()), Some("sales.orders".to_owned())],
        "nearest first, so the first row is what it was cloned from and the last is the root"
    );
    assert_eq!(chain[0][0].as_deref(), Some("1"));
    assert_eq!(chain[1][0].as_deref(), Some("2"));
}

#[test]
fn a_table_that_is_not_a_clone_answers_with_no_rows_rather_than_an_error() {
    // "Nothing" is an answer, and it is different from "I could not tell you". A client that
    // got an error here could not distinguish an ordinary table from a broken one.
    let (_dir, server) = running();
    assert_eq!(
        query_outcome(server.port, "SHOW LINEAGE OF orders").expect("the statement runs"),
        0
    );
}

#[test]
fn a_table_says_what_still_reads_it_before_anybody_tries_to_drop_it() {
    // The whole point. `may_drop` names the clones that would break *after* the attempt, which
    // is no use to somebody who had no way to ask beforehand.
    let (_dir, server) = running();
    query_outcome(server.port, "CREATE TABLE q3_frozen CLONE orders").expect("cloning");
    query_outcome(server.port, "CREATE TABLE q4_frozen CLONE orders").expect("cloning again");

    let dependents = text_rows(server.port, "SHOW DEPENDENTS OF orders");
    let names: Vec<Option<String>> = dependents.iter().map(|row| row[0].clone()).collect();
    assert_eq!(
        names,
        vec![Some("sales.q3_frozen".to_owned()), Some("sales.q4_frozen".to_owned())]
    );
    assert!(dependents.iter().all(|row| row[1].as_deref() == Some("direct")));

}

#[test]
fn what_the_list_says_is_what_the_drop_refuses() {
    // The reason a client may ask at all: the list and the refusal must agree. A list that
    // disagreed would be worse than no list, because somebody would act on it.
    //
    // Dropped through a *clone*, because that is the only drop this server serves --- an
    // ordinary table is refused before cloning is consulted, since this is a read path over a
    // published warehouse.
    let (_dir, server) = running();
    query_outcome(server.port, "CREATE TABLE q3_frozen CLONE orders").expect("cloning");
    query_outcome(server.port, "CREATE TABLE q3_audit CLONE q3_frozen").expect("cloning again");

    let dependents = text_rows(server.port, "SHOW DEPENDENTS OF q3_frozen");
    let names: Vec<Option<String>> = dependents.iter().map(|row| row[0].clone()).collect();
    assert_eq!(names, vec![Some("sales.q3_audit".to_owned())]);

    let refused = query_outcome(server.port, "DROP TABLE q3_frozen").expect_err("refused");
    assert!(
        refused.contains("q3_audit"),
        "the refusal names what the list named: {refused}"
    );

    // And with the dependent gone, the same drop is permitted. The list is the reason, not a
    // permanent property of the table.
    query_outcome(server.port, "DROP TABLE q3_audit").expect("dropping the dependent");
    assert_eq!(
        query_outcome(server.port, "SHOW DEPENDENTS OF q3_frozen").expect("the statement runs"),
        0
    );
    query_outcome(server.port, "DROP TABLE q3_frozen").expect("now permitted");
}

#[test]
fn an_indirect_reader_is_listed_and_named_as_indirect() {
    // A clone of a clone still reads the root's files, so it belongs in the answer. But it is
    // not what a drop of the root breaks first, and collapsing the two would tell somebody the
    // wrong thing about which table to deal with.
    let (_dir, server) = running();
    query_outcome(server.port, "CREATE TABLE q3_frozen CLONE orders").expect("cloning");
    query_outcome(server.port, "CREATE TABLE q3_audit CLONE q3_frozen").expect("cloning again");

    let dependents = text_rows(server.port, "SHOW DEPENDENTS OF orders");
    let relation: Vec<(Option<String>, Option<String>)> = dependents
        .iter()
        .map(|row| (row[0].clone(), row[1].clone()))
        .collect();
    assert_eq!(
        relation,
        vec![
            (Some("sales.q3_audit".to_owned()), Some("indirect".to_owned())),
            (Some("sales.q3_frozen".to_owned()), Some("direct".to_owned())),
        ]
    );
}

#[test]
fn a_table_nobody_has_cloned_has_no_dependents_and_says_so_with_no_rows() {
    let (_dir, server) = running();
    assert_eq!(
        query_outcome(server.port, "SHOW DEPENDENTS OF orders").expect("the statement runs"),
        0
    );
}

#[test]
fn a_question_about_a_table_that_does_not_exist_is_refused_by_name() {
    let (_dir, server) = running();
    let refused = query_outcome(server.port, "SHOW LINEAGE OF nosuch").expect_err("refused");
    assert!(refused.contains("nosuch"), "{refused}");
    assert!(refused.contains("42P01"), "the SQLSTATE a driver branches on: {refused}");
}

#[test]
fn a_malformed_question_is_refused_here_rather_than_by_the_engine() {
    // It has to be refused by the thing that implements it. Handing `SHOW LINEAGE` to the
    // engine produces "syntax error near LINEAGE" for a statement this server does implement,
    // which sends somebody to fix a typo that is not there.
    let (_dir, server) = running();
    let refused = query_outcome(server.port, "SHOW LINEAGE").expect_err("refused");
    assert!(refused.contains("needs the name of a table"), "{refused}");
}

#[test]
fn another_show_still_reaches_the_layer_that_answers_it() {
    // The failure mode this whole surface risks: claiming statements that are not ours. A
    // catalogue-browsing client sends several `SHOW`s on connection, and one answered by the
    // clone handler would break the client to answer a question it never asked.
    let (_dir, server) = running();
    assert!(
        query_outcome(server.port, "SHOW server_version_num").is_ok(),
        "a settings query was claimed by the clone handler"
    );
}

// --- reading a clone, which is what makes it a table ------------------------

#[test]
fn a_clone_is_readable_without_restarting_the_server() {
    // `CREATE TABLE ... CLONE` succeeded and produced something unreadable. The clone was
    // committed, `SHOW LINEAGE` and `SHOW DEPENDENTS` saw it, re-issuing the create refused it
    // as already there --- and every `SELECT` answered "table not found", until a restart.
    //
    // The servable set was fixed at startup and never gained a table. Two reviewers found it
    // independently, which is what a statement that succeeds and does nothing looks like from
    // outside.
    let (_dir, server) = running();
    let before = query_outcome(server.port, "SELECT id FROM orders").expect("the origin reads");

    query_outcome(server.port, "CREATE TABLE q3_frozen CLONE sales.orders").expect("cloning");

    let after = query_outcome(server.port, "SELECT id FROM sales.q3_frozen")
        .expect("a clone that cannot be read is not a table");
    assert_eq!(after, before, "a clone reads exactly what its origin reads");
}

#[test]
fn a_clone_reads_its_origins_rows_rather_than_none() {
    // The second layer, and the worse one. `resolve_clone_cached` was called by nothing but
    // its own tests, so the server resolved a clone through the ordinary path, found a log
    // naming no files --- which is what a clone's log is --- and served it as **empty**.
    //
    // A whole table's worth of rows, silently absent, from a statement that reported success.
    let (_dir, server) = running();
    query_outcome(server.port, "CREATE TABLE q3_frozen CLONE sales.orders").expect("cloning");

    let origin = text_rows(server.port, "SELECT count(*), sum(amount) FROM sales.orders");
    let clone = text_rows(server.port, "SELECT count(*), sum(amount) FROM sales.q3_frozen");
    assert_eq!(clone, origin, "the clone answered differently from what it references");
    assert_ne!(clone[0][0].as_deref(), Some("0"), "the clone read as empty");
}

#[test]
fn a_clone_of_a_clone_reads_the_same_rows_as_the_root() {
    // The third layer. The splice carries one origin at one version, so a clone of a clone
    // spliced against a log that names no files and read as zero rows --- a chain of three
    // answering with the table, nothing, and nothing.
    //
    // It now walks to where the files are. The walk is bounded, because lineage records can be
    // made cyclic by editing a table's properties even though cloning cannot create one.
    let (_dir, server) = running();
    query_outcome(server.port, "CREATE TABLE q3_frozen CLONE sales.orders").expect("cloning");
    query_outcome(server.port, "CREATE TABLE q3_audit CLONE sales.q3_frozen").expect("again");
    query_outcome(server.port, "CREATE TABLE q3_deep CLONE sales.q3_audit").expect("and again");

    let root = text_rows(server.port, "SELECT count(*), sum(amount) FROM sales.orders");
    for table in ["sales.q3_frozen", "sales.q3_audit", "sales.q3_deep"] {
        let seen = text_rows(server.port, &format!("SELECT count(*), sum(amount) FROM {table}"));
        assert_eq!(seen, root, "{table} did not read what the family reads");
    }
}

#[test]
fn a_refusal_carries_the_names_it_cites_as_data() {
    // `ADR-0017` Decision 2, and the field it calls cheap now and expensive later. A refusal
    // here names things --- the clones that would break, the two tables a name could mean ---
    // and a client that wants to act on them must not have to parse the sentence.
    //
    // The moment it does, the sentence is an API: nobody may reword it, and every improvement
    // to the message breaks somebody. `Refused::StillRead` already carried the names as data
    // and this path flattened them into prose, which is Decision 2's own worked example
    // failing.
    let (_dir, server) = running();
    query_outcome(server.port, "CREATE TABLE q3_frozen CLONE sales.orders").expect("cloning");
    query_outcome(server.port, "CREATE TABLE q3_audit CLONE sales.q3_frozen").expect("again");

    // Read as **fields**, not as rendered bytes. A test that finds the name anywhere in the
    // buffer passes whether it arrived as data or only as prose, which is the whole
    // difference being guarded here.
    let fields = common::refusal_fields(server.port, "DROP TABLE sales.q3_frozen");
    assert_eq!(
        fields.get(&'H').map(String::as_str),
        Some("sales.q3_audit"),
        "the names did not travel as data: {fields:?}"
    );
    assert!(
        fields.get(&'D').is_some_and(|detail| detail.contains("SHOW DEPENDENTS OF")),
        "the refusal must say what to do about it: {fields:?}"
    );
}

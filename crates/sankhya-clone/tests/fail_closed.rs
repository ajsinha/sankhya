//! Every refused clone path, walked in one place.
//!
//! # Why an enumeration rather than the tests that already exist
//!
//! Each refusal has its own test, and passing all of them is a different claim from the one
//! `M10`'s exit criterion makes: *"every refused clone path shown to fail closed"*. Individual
//! tests prove each refusal refuses. Only a list proves there is no path somebody built and
//! forgot to refuse — and a list nobody can read end to end is a list nobody can check.
//!
//! `M9` makes the same argument for its nineteen purge refusals, in
//! `sankhya-tiering/tests/end_to_end.rs`. This is the same shape for cloning.
//!
//! # What "fail closed" means here
//!
//! Not "returns an error". **Returns an error and changes nothing.** A refusal that reported a
//! problem after removing a directory would be a refusal in name, so where an operation has an
//! effect the test asserts the effect did not happen.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_clone::ddl::parse;
use sankhya_clone::lineage::Lineage;
use sankhya_clone::refuse::{may_clone, may_drop, may_purge, may_read_as_of, Origin, Request};
use sankhya_clone::Lineages;

const T0: i64 = 1_700_000_000_000_000;

fn request() -> Request {
    Request {
        table: "staging".to_string(),
        tenant: "acme".to_string(),
        origin: "entries".to_string(),
        origin_tenant: "acme".to_string(),
        version: 40,
    }
}

fn healthy() -> Origin {
    Origin {
        latest_version: 52,
        earliest_retained_version: 12,
        purge_in_flight: false,
        schema_evolving: false,
    }
}

fn family() -> Lineages {
    let mut lineages = Lineages::new();
    lineages.record("staging", Lineage::new("entries", 40, T0));
    lineages.record("scratch", Lineage::new("staging", 3, T0));
    lineages
}

#[test]
fn every_refused_clone_path_fails_closed() {
    // One test that walks each way a clone operation can be refused and requires that it *is*
    // refused. Individually these are covered elsewhere; together they are the claim the exit
    // criterion makes — that there is no path where a refusal quietly becomes a permission.

    // --- creation ----------------------------------------------------------------------
    let creation: [(&str, Request, Origin); 5] = [
        (
            "the origin has a purge planned or running",
            request(),
            Origin { purge_in_flight: true, ..healthy() },
        ),
        (
            "the origin belongs to another tenant",
            Request { origin_tenant: "widgets".to_string(), ..request() },
            healthy(),
        ),
        (
            "the origin is mid-schema-evolution",
            request(),
            Origin { schema_evolving: true, ..healthy() },
        ),
        (
            "the origin never had that version",
            Request { version: 99, ..request() },
            healthy(),
        ),
        (
            "the origin can no longer reconstruct that version",
            Request { version: 3, ..request() },
            healthy(),
        ),
    ];
    for (why, asked, origin) in &creation {
        assert!(
            may_clone(asked, origin).is_err(),
            "a clone was permitted when {why}"
        );
    }

    // --- removal -----------------------------------------------------------------------
    assert!(
        may_drop("entries", &family()).is_err(),
        "an origin its clones still read was permitted to be dropped"
    );
    assert!(
        may_drop("staging", &family()).is_err(),
        "a clone that another clone reads was permitted to be dropped"
    );
    assert!(
        may_purge("entries", &family()).is_err(),
        "a table its clones still read was permitted to be purged"
    );

    // --- reading -----------------------------------------------------------------------
    assert!(
        may_read_as_of("staging", T0 - 1, &Lineage::new("entries", 40, T0)).is_err(),
        "a clone answered for a moment before it existed"
    );

    // --- a lineage that contradicts itself ---------------------------------------------
    let mut tangled = Lineages::new();
    tangled.record("a", Lineage::new("b", 1, T0));
    tangled.record("b", Lineage::new("a", 1, T0));
    assert!(
        may_drop("a", &tangled).is_err(),
        "a drop was decided from lineage records that disagree"
    );
    assert!(
        tangled.readers_of("a").is_err(),
        "a reclamation set was built from lineage records that disagree"
    );

    // --- the statement, which must refuse rather than guess -----------------------------
    for sql in [
        "CREATE TABLE staging CLONE",
        "CREATE TABLE staging CLONE entries AT 40",
        "CREATE TABLE staging CLONE entries AT VERSION yesterday",
        "CREATE TABLE staging CLONE entries AT VERSION 40 SHALLOW",
        "DROP TABLE staging CASCADE",
    ] {
        assert!(
            parse(sql).is_some_and(|read| read.is_err()),
            "`{sql}` was read as a valid statement"
        );
    }
}

#[test]
fn the_permitted_cases_are_permitted_which_is_what_makes_the_refusals_mean_something() {
    // The control. Every assertion above is satisfied by a function that refuses everything,
    // and a clone feature that refuses every clone passes the exit criterion while being
    // useless. So each refused shape has its permitted twin here.
    assert!(may_clone(&request(), &healthy()).is_ok(), "an ordinary clone");
    assert!(may_clone(&Request { version: 12, ..request() }, &healthy()).is_ok(), "the oldest");
    assert!(may_clone(&Request { version: 52, ..request() }, &healthy()).is_ok(), "the newest");

    assert!(may_drop("scratch", &family()).is_ok(), "a leaf clone");
    assert!(may_purge("untouched", &family()).is_ok(), "a table nobody cloned");
    assert!(
        may_read_as_of("staging", T0, &Lineage::new("entries", 40, T0)).is_ok(),
        "the instant it was made"
    );

    assert!(
        parse("CREATE TABLE staging CLONE entries AT VERSION 40")
            .is_some_and(|read| read.is_ok()),
        "a well-formed clone statement"
    );
    assert!(
        parse("DROP TABLE staging").is_some_and(|read| read.is_ok()),
        "a well-formed drop"
    );
    assert!(
        parse("CREATE TABLE orders (id BIGINT)").is_none(),
        "and an ordinary statement is left alone"
    );
}

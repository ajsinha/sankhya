//! Which tier a statement belongs to.
//!
//! # Why this is tested before the router exists
//!
//! Nothing routes to PostgreSQL yet. This is the half that will be **wrong**, and it is the
//! half that can be checked without a cluster: whether a statement names a built-in is a
//! property of its text.
//!
//! The failure it exists to prevent is not a crash. It is a statement calling `norm_cdf` sent
//! to a tier where `norm_cdf` does not exist --- so the user is refused for a function the
//! catalogue told them they had, and whether they are refused depends on a routing decision
//! they cannot see and did not make.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use sankhya_functions::catalogue::everything;
use sankhya_functions::{built_ins_in, tier_for, Tier};

fn tier(sql: &str) -> Tier {
    tier_for(sql, &everything())
}

#[test]
fn a_statement_calling_a_built_in_goes_analytical() {
    // The whole rule: these functions live in the analytical engine and nowhere else, so a
    // statement that calls one is an analytical statement whatever else it looks like.
    for sql in [
        "SELECT norm_cdf(1.0)",
        "SELECT * FROM t WHERE norm_cdf(x) > 0.9",
        "SELECT id FROM docs ORDER BY vec_cosine_similarity(embedding, $1) DESC LIMIT 10",
        "SELECT mat_cholesky(covariance) FROM risk",
        "select regress_slope(x, y) from readings",
        "SELECT NORM_INV(0.975)",
    ] {
        assert_eq!(tier(sql), Tier::Analytical, "`{sql}` was not routed analytically");
    }
}

#[test]
fn a_point_lookup_that_calls_nothing_may_go_either_way() {
    // The case the rule must not disturb. A key probe against the authoritative copy is
    // simultaneously fastest and freshest, and sending it analytically for no reason would
    // cost the b-tree probe on the statement that most wants it.
    for sql in [
        "SELECT * FROM orders WHERE id = 42",
        "SELECT name, amount FROM orders WHERE id IN (1, 2, 3)",
        "UPDATE orders SET amount = 5 WHERE id = 42",
        "SELECT count(*) FROM orders",
        "SELECT sum(amount) FROM orders GROUP BY region",
    ] {
        assert_eq!(tier(sql), Tier::Either, "`{sql}` was pushed analytical for nothing");
    }
}

#[test]
fn a_name_that_is_not_a_call_does_not_route_anything() {
    // A column called `erf`, a table called `functions`, a word inside an identifier. Routing
    // on any of those sends an ordinary lookup down the slow path for nothing.
    for sql in [
        "SELECT erf FROM measurements WHERE id = 1",
        "SELECT * FROM functions WHERE kind = 'x'",
        "SELECT my_erf(x) FROM t",
        "SELECT erf_total FROM t",
        "SELECT normalised FROM t WHERE id = 1",
    ] {
        assert_eq!(tier(sql), Tier::Either, "`{sql}` matched a name that is not a call");
    }
}

#[test]
fn one_name_being_a_prefix_of_another_does_not_confuse_it() {
    // The failure that would actually happen, because every family here has names that are
    // prefixes of each other: `erf` and `erfc`, `norm_cdf` and `norm_cdf`-like, `t_cdf` and
    // `t_cdf`. A match without a right-hand boundary reports `erfc(x)` as a call to `erf`.
    assert_eq!(built_ins_in("SELECT erfc(1.0)", &everything()), vec!["erfc"]);
    assert_eq!(built_ins_in("SELECT erf(1.0)", &everything()), vec!["erf"]);
    assert_eq!(
        built_ins_in("SELECT gamma_p(1.0, 2.0)", &everything()),
        vec!["gamma_p"]
    );
}

#[test]
fn whitespace_before_the_parenthesis_is_still_a_call() {
    // Legal SQL, and somebody writes it. A match that required the parenthesis to be adjacent
    // would route this to a tier that cannot answer it.
    assert_eq!(tier("SELECT norm_cdf  (1.0)"), Tier::Analytical);
    assert_eq!(tier("SELECT norm_cdf\n    (1.0)"), Tier::Analytical);
}

#[test]
fn every_function_in_the_catalogue_routes_itself() {
    // The property that keeps this honest as the catalogue grows: a function added and not
    // recognised here is a function that would be sent to a tier without it, and the refusal
    // would name a function the catalogue says exists.
    for entry in everything() {
        let sql = format!("SELECT {}(1.0)", entry.name);
        assert_eq!(
            tier(&sql),
            Tier::Analytical,
            "`{}` is in the catalogue and does not route itself",
            entry.name
        );
    }
}

#[test]
fn the_statement_says_which_built_ins_it_used() {
    // For a plan, and for an operator asking why a lookup went the slow way. Sorted and
    // deduplicated, so two readings of one statement agree.
    let found = built_ins_in(
        "SELECT norm_cdf(a), norm_cdf(b), vec_dot(u, v) FROM t",
        &everything(),
    );
    assert_eq!(found, vec!["norm_cdf", "vec_dot"]);
    assert!(built_ins_in("SELECT * FROM t WHERE id = 1", &everything()).is_empty());
}

#[test]
fn the_eager_direction_is_the_safe_one() {
    // A string literal that reads like a call routes analytically, and nothing about the
    // answer changes --- one query takes the slower path.
    //
    // The other direction is not survivable: missing a call and routing to a tier that cannot
    // answer is a refusal for a function the catalogue says exists. So the match is
    // deliberately eager, and this records that it is a decision rather than an oversight.
    assert_eq!(tier("SELECT 'norm_cdf(1)' AS a_string"), Tier::Analytical);
}

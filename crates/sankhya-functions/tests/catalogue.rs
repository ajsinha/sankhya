//! The catalogue, reached the way a statement reaches it.
//!
//! # Why these go through a session rather than calling the kernels
//!
//! The kernels are `sankhya-math`'s and are tested there. What is tested here is the half this
//! crate owns and nothing else covers: that a function has the **name** it claims, that it
//! accepts the argument types a person actually writes, and that a refusal says which argument
//! was wrong rather than surfacing a type error from three layers down.
//!
//! That distinction is the whole reason this crate exists. A kernel with correct arithmetic
//! and no name is work that was half done and looks finished --- which is exactly what
//! `check-kernels` was written to stop.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use datafusion::prelude::SessionContext;

fn session() -> SessionContext {
    let context = SessionContext::new();
    sankhya_functions::register(&context);
    // `vec_of`, so a test can write a matrix down. The catalogue under test does not depend on
    // this --- a session that registered only this crate answers every scalar function here.
    sankhya_olap::register_constructors(&context);
    context
}

async fn run(sql: &str) -> String {
    let batches = session()
        .sql(sql)
        .await
        .unwrap_or_else(|error| panic!("did not plan: {sql}\n{error}"))
        .collect()
        .await
        .unwrap_or_else(|error| panic!("did not run: {sql}\n{error}"));
    datafusion::arrow::util::pretty::pretty_format_batches(&batches)
        .map(|d| d.to_string())
        .unwrap_or_default()
}

async fn refused(sql: &str) -> String {
    match session().sql(sql).await {
        Err(error) => error.to_string(),
        Ok(frame) => match frame.collect().await {
            Err(error) => error.to_string(),
            Ok(_) => panic!("this was expected to be refused and was not: {sql}"),
        },
    }
}

#[tokio::test]
async fn every_function_has_a_distinct_name() {
    // Two functions of one name is a registration where the second silently replaces the
    // first, and the query that gets the wrong one still returns a number.
    let names = sankhya_functions::names();
    let mut unique = names.clone();
    unique.dedup();
    assert_eq!(names.len(), unique.len(), "a name is registered twice: {names:?}");
    assert!(names.len() >= 30, "the catalogue has shrunk: {}", names.len());
}

#[tokio::test]
async fn the_critical_values_a_person_would_look_up_come_back_right() {
    // Reached as a statement reaches them, so a function that computes correctly and is
    // registered under the wrong name fails here.
    let out = run(
        "SELECT round(norm_inv(0.975), 6) AS z, \
                round(t_inv(0.975, 10), 6) AS t, \
                round(chisq_inv(0.95, 1), 5) AS chi",
    )
    .await;
    assert!(out.contains("1.959964"), "{out}");
    assert!(out.contains("2.228139"), "{out}");
    assert!(out.contains("3.84146"), "{out}");
}

#[tokio::test]
async fn an_integer_argument_is_accepted_because_that_is_what_people_write() {
    // `norm_cdf(1)` rather than `norm_cdf(1.0)`. Declaring an exact `Float64` signature would
    // make the first a planning error, which is pedantry with a syntax error attached.
    let out = run("SELECT norm_cdf(1) AS a, t_cdf(2, 10) AS b, chisq_sf(3, 2) AS c").await;
    assert!(out.contains("0.84134"), "{out}");
    assert!(!out.contains("error"), "{out}");
}

#[tokio::test]
async fn a_null_argument_gives_a_null_answer_rather_than_a_number() {
    // Substituting zero would compute a real answer from a value nobody supplied --- and a
    // `norm_cdf` of a missing measurement is not 0.5.
    let out = run("SELECT norm_cdf(CAST(NULL AS DOUBLE)) AS n").await;
    assert!(out.contains(""), "{out}");
    assert!(!out.contains("0.5"), "a null became the midpoint: {out}");
}

#[tokio::test]
async fn a_fractional_count_is_refused_rather_than_rounded() {
    // `binom_pmf(2.7, 10, 0.5)` is not a question with an answer. Rounding to two answers a
    // question about a different number of events, silently.
    let said = refused("SELECT binom_pmf(2.7, 10, 0.5)").await;
    assert!(said.contains("whole number"), "{said}");
    assert!(said.contains("different number of events"), "{said}");

    // A negative count likewise, and the same message says why.
    assert!(refused("SELECT poisson_pmf(-1, 4.0)").await.contains("whole number"));

    // And the whole-numbered forms are accepted, so the check is not simply strict.
    let out = run("SELECT binom_pmf(5, 10, 0.5) AS p, poisson_pmf(2, 4.0) AS q").await;
    assert!(out.contains("0.246"), "{out}");
}

#[tokio::test]
async fn a_parameter_outside_its_domain_says_which_one_and_why() {
    // A distribution with a non-positive scale is not a narrow distribution --- it is not a
    // distribution, and answering would be inventing one.
    let said = refused("SELECT t_cdf(1.0, 0.0)").await;
    assert!(said.contains("freedom"), "the refusal names the parameter: {said}");
    assert!(said.contains("not a distribution"), "{said}");

    let said = refused("SELECT norm_inv(1.5)").await;
    assert!(said.contains("between zero and one"), "{said}");
    assert!(said.contains("clamp"), "it says why it does not clamp: {said}");
}

#[tokio::test]
async fn a_column_that_is_not_a_number_is_refused_rather_than_parsed() {
    // Reading a string as a number computes a real answer from text somebody never meant as
    // one, which is the wrong answer that looks most like a right one.
    let said = refused("SELECT norm_cdf('not a number')").await;
    assert!(said.contains("needs numbers"), "{said}");
    assert!(said.contains("nobody meant as a number"), "{said}");
}

#[tokio::test]
async fn the_wrong_number_of_arguments_names_both_counts() {
    let said = refused("SELECT norm_cdf(1.0, 2.0)").await;
    assert!(said.contains("takes 1 argument"), "{said}");
    assert!(said.contains("given 2"), "{said}");
}

#[tokio::test]
async fn a_matrix_that_is_not_square_is_refused_rather_than_read_as_the_square_it_could_be() {
    // Three values could be read as a 1x1 with two ignored. Refused instead, because a
    // decomposition of a matrix the caller did not supply is not an answer to their question.
    let said = refused("SELECT mat_eigenvalues(vec_of(1.0, 2.0, 3.0))").await;
    assert!(said.contains("square"), "{said}");
    assert!(said.contains("row-major"), "it says how a matrix is stored: {said}");
}

#[tokio::test]
async fn a_matrix_no_data_could_have_produced_answers_zero_rather_than_failing() {
    // `mat_is_positive_definite` is a *question*, so a matrix that is not gets `0` rather than
    // a refusal --- a caller asking whether something holds wants an answer either way. The
    // failing Cholesky underneath it is the refusal, and that one names the problem.
    let out = run(
        "SELECT mat_is_positive_definite(vec_of(1.0, -0.9, -0.9, -0.9, 1.0, -0.9, \
                -0.9, -0.9, 1.0)) AS impossible, \
                mat_is_positive_definite(vec_of(1.0, 0.5, 0.5, 1.0)) AS ordinary",
    )
    .await;
    assert!(out.contains('0'), "{out}");
    assert!(out.contains('1'), "{out}");

    // And the factorisation itself refuses, naming what went wrong.
    let said = refused(
        "SELECT mat_cholesky(vec_of(1.0, -0.9, -0.9, -0.9, 1.0, -0.9, -0.9, -0.9, 1.0))",
    )
    .await;
    assert!(said.contains("mat_cholesky"), "{said}");
}

#[test]
#[ignore = "a reporting tool, not an assertion: run to regenerate the catalogue document"]
fn dump() {
    for name in sankhya_functions::names() {
        println!("functions\t{name}");
    }
    println!("TOTAL\t{}", sankhya_functions::names().len());
}

// --- the catalogue against the registrations ------------------------------

#[tokio::test]
async fn every_registered_function_is_in_the_catalogue_and_the_reverse() {
    // Both directions, and the second is the dangerous one. A function registered and not
    // described is merely undiscoverable; an **entry describing a function nobody registered**
    // becomes a method in a generated binding, and a client calls it and gets a planning error
    // for a name the catalogue told them existed.
    use std::collections::BTreeSet;

    let registered: BTreeSet<String> = sankhya_functions::names().into_iter().collect();
    let described: BTreeSet<String> = sankhya_functions::catalogue::catalogue()
        .iter()
        .map(|entry| entry.name.to_owned())
        .collect();

    let undescribed: Vec<&String> = registered.difference(&described).collect();
    assert!(
        undescribed.is_empty(),
        "registered and not in the catalogue, so no binding can offer them: {undescribed:?}"
    );

    let unregistered: Vec<&String> = described.difference(&registered).collect();
    assert!(
        unregistered.is_empty(),
        "in the catalogue and not registered, so a binding would offer a name that does not \
         plan: {unregistered:?}"
    );
}

#[tokio::test]
async fn every_catalogue_entry_says_enough_to_offer_the_function() {
    // What a binding needs from an entry, and a client needs from a picker. An entry that
    // named a function and said nothing else would be a list a `SELECT` already gives.
    for entry in sankhya_functions::catalogue::catalogue() {
        assert!(!entry.category.is_empty(), "{} has no category", entry.name);
        assert!(
            entry.about.len() > 20,
            "{}'s description is too short to choose by: {:?}",
            entry.name,
            entry.about
        );
        assert!(entry.arity > 0, "{} takes no arguments", entry.name);
        assert!(
            !entry.about.ends_with('.'),
            "{}'s description ends with a full stop; the catalogue renders a list, not prose",
            entry.name
        );
    }
}

#[tokio::test]
async fn the_declared_arity_is_the_arity_the_function_enforces() {
    // The catalogue's arity is what a binding generates a signature from, so an entry that
    // disagreed with the function would produce a method whose every call is refused.
    //
    // Checked by calling each with **one argument too many** and requiring a refusal that says
    // so --- which also proves the arity check is reached before anything else.
    for entry in sankhya_functions::catalogue::catalogue() {
        if entry.takes != sankhya_functions::Takes::Numbers {
            // Only the numeric ones can be called with plain literals here; the rest need an
            // array, and building one per shape would test the fixture rather than the arity.
            continue;
        }
        let too_many: Vec<String> =
            (0..=entry.arity).map(|i| format!("{}.0", i + 1)).collect();
        let sql = format!("SELECT {}({})", entry.name, too_many.join(", "));
        let said = refused(&sql).await;
        assert!(
            said.contains(&format!("takes {} argument", entry.arity)),
            "{} declares arity {} and did not say so: {said}",
            entry.name,
            entry.arity
        );
    }
}

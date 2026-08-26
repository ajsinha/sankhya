//! What the analytical engine means by a query.
//!
//! # Why pin behaviour that is not in doubt
//!
//! Every case here is something the engine already does correctly. The corpus is not
//! looking for bugs in it — it is a record of what this system has been built against,
//! so that a version upgrade which changes one of these is a failing test rather than a
//! figure that quietly stops matching the transactional tier.
//!
//! The cases are chosen by consequence, not by coverage. Three-valued logic, grouping on
//! null, empty aggregates and join multiplicity are where a query that reads correctly
//! returns something other than what its author meant, and where the two tiers can
//! silently diverge.
//!
//! # Why the expected values are written out
//!
//! A corpus that computes its expectation the same way the engine does is a tautology.
//! Every value below is written by hand from the standard's rules, which is what makes a
//! disagreement mean something.

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

use datafusion::prelude::SessionContext;

/// One query and the single scalar it must produce.
///
/// Null is written as `NULL` rather than left blank, so "the engine returned nothing" and
/// "the engine returned null" are not the same string.
struct Case {
    name: &'static str,
    sql: &'static str,
    expect: &'static str,
}

async fn scalar(ctx: &SessionContext, sql: &str) -> String {
    let batches = match ctx.sql(sql).await {
        Ok(frame) => match frame.collect().await {
            Ok(b) => b,
            Err(e) => return format!("ERROR: {}", e.to_string().lines().next().unwrap_or("")),
        },
        Err(e) => return format!("ERROR: {}", e.to_string().lines().next().unwrap_or("")),
    };

    // "Returned nothing" and "returned null" are different answers, and the pretty
    // printer renders both as an empty cell. Conflating them would let a query that
    // produces no rows at all satisfy every case expecting a null -- a third of this
    // corpus.
    let rows: usize = batches.iter().map(arrow_array::RecordBatch::num_rows).sum();
    if rows == 0 {
        return "NO ROWS".to_string();
    }

    let formatted = arrow::util::pretty::pretty_format_batches(&batches)
        .expect("formatting")
        .to_string();
    let value = formatted
        .lines()
        .nth(3)
        .unwrap_or("")
        .trim_matches(|c| c == '|' || c == ' ')
        .to_string();
    if value.is_empty() {
        return "NULL".to_string();
    }
    value
}

/// Three-valued logic. The rule everyone knows and nobody applies consistently.
fn null_logic() -> Vec<Case> {
    vec![
        Case {
            name: "null is not equal to null",
            sql: "SELECT CAST(NULL AS INT) = CAST(NULL AS INT)",
            expect: "NULL",
        },
        Case {
            name: "null is not distinct from null",
            sql: "SELECT CAST(NULL AS INT) IS NOT DISTINCT FROM CAST(NULL AS INT)",
            expect: "true",
        },
        Case {
            name: "false AND null is false, not null",
            sql: "SELECT false AND CAST(NULL AS BOOLEAN)",
            expect: "false",
        },
        Case {
            name: "true OR null is true, not null",
            sql: "SELECT true OR CAST(NULL AS BOOLEAN)",
            expect: "true",
        },
        Case {
            name: "true AND null is null",
            sql: "SELECT true AND CAST(NULL AS BOOLEAN)",
            expect: "NULL",
        },
        Case {
            name: "NOT null is null",
            sql: "SELECT NOT CAST(NULL AS BOOLEAN)",
            expect: "NULL",
        },
        Case {
            name: "null is not in a list, and is not not-in it either",
            sql: "SELECT CAST(NULL AS INT) IN (1, 2)",
            expect: "NULL",
        },
        Case {
            name: "a null in the list makes a non-match null, not false",
            sql: "SELECT 3 IN (1, CAST(NULL AS INT))",
            expect: "NULL",
        },
        Case {
            name: "a match wins even with a null in the list",
            sql: "SELECT 1 IN (1, CAST(NULL AS INT))",
            expect: "true",
        },
        Case {
            name: "coalesce takes the first non-null",
            sql: "SELECT coalesce(CAST(NULL AS INT), CAST(NULL AS INT), 3)",
            expect: "3",
        },
    ]
}

/// Aggregates, where an empty input is the interesting case.
fn aggregates() -> Vec<Case> {
    vec![
        Case {
            name: "count of nothing is zero",
            sql: "SELECT count(*) FROM (VALUES (1)) t(x) WHERE false",
            expect: "0",
        },
        Case {
            name: "sum of nothing is null",
            sql: "SELECT sum(x) FROM (VALUES (1)) t(x) WHERE false",
            expect: "NULL",
        },
        Case {
            name: "a query returning no rows is not the same as one returning null",
            sql: "SELECT x FROM (VALUES (1)) t(x) WHERE false",
            expect: "NO ROWS",
        },
        Case {
            name: "min of nothing is null",
            sql: "SELECT min(x) FROM (VALUES (1)) t(x) WHERE false",
            expect: "NULL",
        },
        Case {
            name: "count of a column skips nulls",
            sql: "SELECT count(x) FROM (VALUES (1),(NULL),(3)) t(x)",
            expect: "2",
        },
        Case {
            name: "count star does not skip nulls",
            sql: "SELECT count(*) FROM (VALUES (1),(NULL),(3)) t(x)",
            expect: "3",
        },
        Case {
            name: "sum skips nulls rather than propagating them",
            sql: "SELECT sum(x) FROM (VALUES (1),(NULL),(3)) t(x)",
            expect: "4",
        },
        Case {
            name: "avg divides by the non-null count",
            sql: "SELECT avg(x) FROM (VALUES (1),(NULL),(3)) t(x)",
            expect: "2.0",
        },
        Case {
            name: "count distinct counts null once at most",
            sql: "SELECT count(DISTINCT x) FROM (VALUES (1),(NULL),(1),(NULL)) t(x)",
            expect: "1",
        },
    ]
}

/// Grouping, where null is a group and not an absence.
fn grouping() -> Vec<Case> {
    vec![
        Case {
            name: "null is one group, not several",
            sql: "SELECT count(*) FROM (SELECT x FROM (VALUES (NULL),(NULL),(1)) t(x) GROUP BY x) g",
            expect: "2",
        },
        Case {
            name: "grouping on null keeps its rows",
            sql: "SELECT count(*) FROM (VALUES (NULL),(NULL),(1)) t(x) GROUP BY x ORDER BY 1 DESC LIMIT 1",
            expect: "2",
        },
        Case {
            name: "distinct treats nulls as one value",
            sql: "SELECT count(*) FROM (SELECT DISTINCT x FROM (VALUES (NULL),(NULL),(2)) t(x)) g",
            expect: "2",
        },
        Case {
            name: "having filters groups, not rows",
            sql: "SELECT count(*) FROM (SELECT x FROM (VALUES (1),(1),(2)) t(x) GROUP BY x HAVING count(*) > 1) g",
            expect: "1",
        },
    ]
}

/// Joins, where multiplicity is what surprises people.
fn joins() -> Vec<Case> {
    vec![
        Case {
            name: "an inner join multiplies matching rows",
            sql: "SELECT count(*) FROM (VALUES (1),(1)) a(x) JOIN (VALUES (1),(1),(1)) b(y) ON a.x = b.y",
            expect: "6",
        },
        Case {
            name: "a left join keeps unmatched rows once",
            sql: "SELECT count(*) FROM (VALUES (1),(2)) a(x) LEFT JOIN (VALUES (1)) b(y) ON a.x = b.y",
            expect: "2",
        },
        Case {
            name: "a join never matches null to null",
            sql: "SELECT count(*) FROM (VALUES (NULL)) a(x) JOIN (VALUES (NULL)) b(y) ON a.x = b.y",
            expect: "0",
        },
        Case {
            name: "a left join fills unmatched columns with null",
            sql: "SELECT b.y FROM (VALUES (2)) a(x) LEFT JOIN (VALUES (1)) b(y) ON a.x = b.y",
            expect: "NULL",
        },
        Case {
            name: "an anti join keeps rows with no match",
            sql: "SELECT count(*) FROM (VALUES (1),(2)) a(x) WHERE x NOT IN (SELECT y FROM (VALUES (1)) b(y))",
            expect: "1",
        },
        Case {
            name: "a null on the inner side makes NOT IN return nothing",
            sql: "SELECT count(*) FROM (VALUES (1),(2)) a(x) WHERE x NOT IN (SELECT y FROM (VALUES (1),(NULL)) b(y))",
            expect: "0",
        },
    ]
}

/// Ordering, where nulls and limits interact.
fn ordering() -> Vec<Case> {
    vec![
        Case {
            name: "nulls sort last ascending",
            sql: "SELECT x FROM (VALUES (2),(NULL),(1)) t(x) ORDER BY x DESC LIMIT 1",
            expect: "NULL",
        },
        Case {
            name: "the smallest value comes first ascending",
            sql: "SELECT x FROM (VALUES (2),(NULL),(1)) t(x) ORDER BY x LIMIT 1",
            expect: "1",
        },
        Case {
            name: "nulls first can be asked for",
            sql: "SELECT x FROM (VALUES (2),(NULL),(1)) t(x) ORDER BY x NULLS FIRST LIMIT 1",
            expect: "NULL",
        },
        Case {
            name: "an offset skips before the limit applies",
            sql: "SELECT x FROM (VALUES (1),(2),(3)) t(x) ORDER BY x OFFSET 1 LIMIT 1",
            expect: "2",
        },
    ]
}

/// Types and coercion, where a comparison silently changes what is compared.
fn types() -> Vec<Case> {
    vec![
        Case {
            name: "an integer compares equal to the same decimal",
            sql: "SELECT 1 = 1.0",
            expect: "true",
        },
        Case {
            name: "casting a fraction to an integer truncates",
            sql: "SELECT CAST(1.9 AS INT)",
            expect: "1",
        },
        Case {
            name: "casting a negative fraction truncates toward zero",
            sql: "SELECT CAST(-1.9 AS INT)",
            expect: "-1",
        },
        Case {
            name: "a decimal keeps its scale through addition",
            sql: "SELECT CAST(1.05 AS DECIMAL(10,2)) + CAST(2.10 AS DECIMAL(10,2))",
            expect: "3.15",
        },
        Case {
            name: "string comparison is by bytes",
            sql: "SELECT 'A' < 'a'",
            expect: "true",
        },
        Case {
            name: "concatenation with null is null",
            sql: "SELECT 'a' || CAST(NULL AS VARCHAR)",
            expect: "NULL",
        },
    ]
}

fn corpus() -> Vec<(&'static str, Vec<Case>)> {
    vec![
        ("three-valued logic", null_logic()),
        ("aggregates", aggregates()),
        ("grouping", grouping()),
        ("joins", joins()),
        ("ordering", ordering()),
        ("types and coercion", types()),
    ]
}

#[tokio::test]
async fn the_engine_means_what_this_corpus_says() {
    let ctx = SessionContext::new();
    let mut wrong = Vec::new();
    let mut checked = 0;

    for (section, cases) in corpus() {
        for case in cases {
            checked += 1;
            let observed = scalar(&ctx, case.sql).await;
            if observed != case.expect {
                wrong.push(format!(
                    "{section} / {}: expected {:?}, got {observed:?}\n      {}",
                    case.name, case.expect, case.sql
                ));
            }
        }
    }

    assert!(
        wrong.is_empty(),
        "{} of {checked} cases no longer behave as recorded:\n  {}",
        wrong.len(),
        wrong.join("\n  ")
    );
    assert!(checked >= 35, "only {checked} cases; the corpus has shrunk");
}

#[test]
fn every_case_has_a_name_that_says_what_it_means() {
    // A corpus is read when something fails, and "case 27" tells the reader nothing.
    // The names are the documentation, so they have to be sentences.
    for (_, cases) in corpus() {
        for case in cases {
            assert!(
                case.name.len() > 15 && case.name.contains(' '),
                "{:?} is not a sentence",
                case.name
            );
        }
    }
}

#[test]
fn no_case_expects_an_error() {
    // This corpus records what the engine *means*, not what it refuses. A case expecting
    // an error would pass on any error, including one from a typo in the query -- which
    // is how a corpus quietly stops testing anything.
    for (_, cases) in corpus() {
        for case in cases {
            assert!(
                !case.expect.starts_with("ERROR"),
                "{} expects an error; refusals belong in a test that names the reason",
                case.name
            );
        }
    }
}

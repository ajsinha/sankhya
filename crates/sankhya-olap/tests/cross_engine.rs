//! Where the two engines disagree, enumerated and pinned.
//!
//! # Why this list has to exist
//!
//! SANKHYA presents one copy of the data through two engines. The same question asked of
//! the transactional tier and of the analytical tier is supposed to get the same answer,
//! and mostly does — which is precisely what makes the exceptions dangerous. Nobody
//! checks a figure that has agreed a thousand times.
//!
//! So every difference found is recorded here with its consequence, and the test asserts
//! the recorded behaviour of **both** engines. A difference that goes away, or a new one
//! that appears after an upgrade, fails this test rather than surfacing as a
//! reconciliation nobody can explain.
//!
//! # Why the agreements are pinned too
//!
//! A list of differences is only trustworthy if somebody checked the rest. Cases that
//! agree are asserted to agree, so an upgrade that introduces a *new* divergence is a
//! failure here rather than a discovery later.
//!
//! # Running it
//!
//! Needs the vendored PostgreSQL. Skipped unless `SANKHYA_PG_BIN` and
//! `SANKHYA_E2E_SOCKET` are set, so an ordinary `cargo test` does not require a database.

use datafusion::prelude::SessionContext;
use std::process::Command;

struct Pg {
    bin: String,
    socket: String,
}

impl Pg {
    fn from_env() -> Option<Self> {
        Some(Self {
            bin: std::env::var("SANKHYA_PG_BIN").ok()?,
            socket: std::env::var("SANKHYA_E2E_SOCKET").ok()?,
        })
    }

    /// The single scalar a statement produces, or the error it raised.
    ///
    /// Errors are returned rather than panicked on, because "this engine refuses" is one
    /// of the behaviours being compared.
    fn scalar(&self, sql: &str) -> String {
        let out = Command::new(format!("{}/psql", self.bin))
            .args([
                "-h",
                &self.socket,
                "-U",
                "sankhya",
                "-d",
                "postgres",
                "-tA",
                "-c",
                sql,
            ])
            .output()
            .expect("psql runs");

        if out.status.success() {
            let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if text.is_empty() {
                return "NULL".to_string();
            }
            return text;
        }
        format!(
            "ERROR: {}",
            String::from_utf8_lossy(&out.stderr)
                .lines()
                .next()
                .unwrap_or("")
                .trim_start_matches("ERROR:  ")
                .trim()
        )
    }
}

/// The single scalar a statement produces in the analytical engine, or its error.
async fn datafusion_scalar(ctx: &SessionContext, sql: &str) -> String {
    let frame = match ctx.sql(sql).await {
        Ok(f) => f,
        Err(e) => return format!("ERROR: {}", first_line(&e.to_string())),
    };
    let batches = match frame.collect().await {
        Ok(b) => b,
        Err(e) => return format!("ERROR: {}", first_line(&e.to_string())),
    };

    let formatted = arrow::util::pretty::pretty_format_batches(&batches)
        .expect("formatting")
        .to_string();
    // The pretty printer's third line is the first row; trimming the borders leaves the
    // value. Crude, and sufficient for single-column single-row results.
    let value = formatted
        .lines()
        .nth(3)
        .unwrap_or("")
        .trim_matches(|c| c == '|' || c == ' ')
        .to_string();

    // The pretty printer renders null as an empty cell, and `psql -tA` renders it as an
    // empty line. Both are normalised to the same word so a genuine difference is not
    // hidden behind two renderings of the same absence.
    if value.is_empty() {
        return "NULL".to_string();
    }
    value
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or("").trim().to_string()
}

/// What the two engines are expected to do.
enum Verdict {
    /// Both produce this.
    Agree(&'static str),
    /// They differ, and this is what each does and why it matters.
    Differ {
        postgres: &'static str,
        datafusion: &'static str,
        consequence: &'static str,
    },
}

struct Case {
    name: &'static str,
    postgres: &'static str,
    datafusion: &'static str,
    verdict: Verdict,
}

/// Every case checked, agreements included.
fn cases() -> Vec<Case> {
    use Verdict::{Agree, Differ};
    vec![
        // ---- Agreements. Pinned so a new divergence is a failure here. ----
        Case {
            name: "integer division truncates toward zero",
            postgres: "SELECT 7/2",
            datafusion: "SELECT 7/2",
            verdict: Agree("3"),
        },
        Case {
            name: "negative integer division truncates toward zero",
            postgres: "SELECT (-7)/2",
            datafusion: "SELECT (-7)/2",
            verdict: Agree("-3"),
        },
        Case {
            name: "modulo takes the sign of the dividend",
            postgres: "SELECT (-7) % 3",
            datafusion: "SELECT (-7) % 3",
            verdict: Agree("-1"),
        },
        Case {
            name: "division by zero is refused by both",
            postgres: "SELECT 1/0",
            datafusion: "SELECT 1/0",
            verdict: Differ {
                postgres: "ERROR: division by zero",
                datafusion: "ERROR: Arrow error: Divide by zero error",
                consequence: "Both refuse, which is what matters. Only the wording \
                              differs, so an application matching on the message text \
                              will match one and not the other.",
            },
        },
        Case {
            name: "sum over no rows is null, not zero",
            postgres: "SELECT sum(x) FROM (VALUES (1)) t(x) WHERE false",
            datafusion: "SELECT sum(x) FROM (VALUES (1)) t(x) WHERE false",
            verdict: Agree("NULL"),
        },
        Case {
            name: "count over no rows is zero, not null",
            postgres: "SELECT count(x) FROM (VALUES (1)) t(x) WHERE false",
            datafusion: "SELECT count(x) FROM (VALUES (1)) t(x) WHERE false",
            verdict: Agree("0"),
        },
        Case {
            name: "an empty string is not null",
            postgres: "SELECT ('' IS NULL)::text",
            datafusion: "SELECT '' IS NULL",
            verdict: Agree("false"),
        },
        Case {
            name: "float arithmetic is inexact in both",
            postgres: "SELECT 0.1::float8 + 0.2::float8",
            datafusion: "SELECT 0.1::DOUBLE + 0.2::DOUBLE",
            verdict: Agree("0.30000000000000004"),
        },
        Case {
            name: "fixed-point arithmetic is exact in both",
            postgres: "SELECT 0.1::numeric(10,2) + 0.2::numeric(10,2)",
            datafusion: "SELECT 0.1::DECIMAL(10,2) + 0.2::DECIMAL(10,2)",
            verdict: Agree("0.30"),
        },
        // ---- Differences. Each one is a way the two tiers can disagree. ----
        Case {
            name: "summing past the range of a 64-bit integer",
            postgres: "SELECT sum(x) FROM (VALUES (9223372036854775807::bigint),(1::bigint)) t(x)",
            datafusion: "SELECT sum(x) FROM (VALUES (9223372036854775807),(1)) t(x)",
            verdict: Differ {
                postgres: "9223372036854775808",
                datafusion: "-9223372036854775808",
                consequence: "The transactional tier widens the accumulator and gives the \
                              exact total. The analytical tier wraps, silently, and \
                              returns a large negative number where the answer is a large \
                              positive one. Nothing about the result says so.",
            },
        },
        Case {
            name: "multiplying past the range of a 64-bit integer",
            postgres: "SELECT 4611686018427387904::bigint * 4",
            datafusion: "SELECT 4611686018427387904 * 4",
            verdict: Differ {
                postgres: "ERROR: bigint out of range",
                datafusion: "0",
                consequence: "One refuses; the other returns zero. Zero is a plausible \
                              answer to an arithmetic question, which is what makes this \
                              worse than an error.",
            },
        },
        Case {
            name: "summing decimals past 38 digits",
            postgres: "SELECT sum(x) FROM (VALUES (99999999999999999999999999999999999999::numeric),(1::numeric)) t(x)",
            datafusion: "SELECT sum(x) FROM (VALUES (99999999999999999999999999999999999999::DECIMAL(38,0)),(1::DECIMAL(38,0))) t(x)",
            verdict: Differ {
                postgres: "100000000000000000000000000000000000000",
                datafusion: "99999999999999997748809823456034029569",
                consequence: "The analytical tier loses exactness on decimal overflow, \
                              returning a number close to the right one rather than \
                              refusing. For a type chosen because money must be exact, \
                              close is the wrong kind of wrong.",
            },
        },
        Case {
            name: "ordering text",
            // The first row in sort order, which is enough to show the orders differ and
            // avoids depending on an aggregate that is not in both engines.
            postgres: "SELECT x FROM (VALUES ('b'),('A'),('a')) t(x) ORDER BY x LIMIT 1",
            datafusion: "SELECT x FROM (VALUES ('b'),('A'),('a')) t(x) ORDER BY x LIMIT 1",
            verdict: Differ {
                postgres: "a",
                datafusion: "A",
                consequence: "The transactional tier orders by the database's collation; \
                              the analytical tier orders by bytes. Any paged or ranked \
                              result over text is in a different order in the two tiers, \
                              which surfaces as rows appearing on the wrong page rather \
                              than as an error.",
            },
        },
        Case {
            name: "averaging integers",
            postgres: "SELECT avg(x) FROM (VALUES (1),(2)) t(x)",
            datafusion: "SELECT avg(x) FROM (VALUES (1),(2)) t(x)",
            verdict: Differ {
                postgres: "1.5000000000000000",
                datafusion: "1.5",
                consequence: "Same value, different type: arbitrary-precision on one side \
                              and a 64-bit float on the other. The values diverge once an \
                              average has more significant digits than a float can hold.",
            },
        },
        Case {
            name: "dividing to a repeating fraction",
            postgres: "SELECT 1.0/3.0",
            datafusion: "SELECT 1.0/3.0",
            verdict: Differ {
                postgres: "0.33333333333333333333",
                datafusion: "0.3333333333333333",
                consequence: "Twenty significant digits against sixteen. A figure carried \
                              through several operations diverges further at each one.",
            },
        },
    ]
}

#[tokio::test]
async fn the_two_engines_differ_exactly_where_this_list_says() {
    let Some(pg) = Pg::from_env() else {
        eprintln!("skipping: set SANKHYA_PG_BIN and SANKHYA_E2E_SOCKET to run");
        return;
    };
    let ctx = SessionContext::new();

    let mut wrong = Vec::new();
    for case in cases() {
        let observed_pg = pg.scalar(case.postgres);
        let observed_df = datafusion_scalar(&ctx, case.datafusion).await;

        let (want_pg, want_df) = match case.verdict {
            Verdict::Agree(both) => (both, both),
            Verdict::Differ {
                postgres,
                datafusion,
                ..
            } => (postgres, datafusion),
        };

        if observed_pg != want_pg {
            wrong.push(format!(
                "{}: the transactional tier gave {observed_pg:?}, and this list says \
                 {want_pg:?}",
                case.name
            ));
        }
        if observed_df != want_df {
            wrong.push(format!(
                "{}: the analytical tier gave {observed_df:?}, and this list says \
                 {want_df:?}",
                case.name
            ));
        }
    }

    assert!(
        wrong.is_empty(),
        "the engines no longer behave as this list records, which means either a new \
         divergence exists or a recorded one has gone away:\n  {}",
        wrong.join("\n  ")
    );
}

#[test]
fn every_recorded_difference_says_what_it_costs() {
    // A difference with no consequence written down is a curiosity. The point of the
    // list is that somebody reading it can tell which entries would produce a wrong
    // number and which merely a differently-spelled one.
    for case in cases() {
        if let Verdict::Differ { consequence, .. } = case.verdict {
            assert!(
                consequence.len() > 40,
                "{} records a difference without saying what it costs",
                case.name
            );
        }
    }
}

#[test]
fn the_list_covers_agreements_as_well_as_differences() {
    // A list of differences is only trustworthy if somebody checked the rest. Without
    // agreements pinned, an upgrade that introduces a new divergence is a discovery
    // later rather than a failure here.
    let agreements = cases()
        .iter()
        .filter(|c| matches!(c.verdict, Verdict::Agree(_)))
        .count();
    assert!(
        agreements >= 6,
        "only {agreements} agreements are pinned; a differences-only list cannot detect \
         a new difference"
    );
}

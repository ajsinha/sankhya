//! The workload a kernel soak runs, and the answers it checks against.
//!
//! # Why a soak over the function catalogue at all
//!
//! The catalogue's unit tests ask whether each kernel computes the right number once, on data
//! the test chose. That is necessary and it is the easy half. What it cannot see:
//!
//! - **Drift.** The same statement, over the same rows, must return the *same bits* an hour
//!   later. A reduction whose result depends on how work was partitioned returns a different
//!   number when the machine is busier --- which is the failure `sankhya-math` is arranged
//!   against, and no single-shot test can observe it.
//! - **Shape.** A kernel is called on a literal in a unit test and on a **column** here, so a
//!   width read from the wrong stride, or a null handled differently in the array path than in
//!   the scalar one, shows up only under a real scan.
//! - **Accumulation.** Descriptors, memory and open files under sustained mixed load.
//! - **Interaction.** A similarity search that also rolls up a cube while a compaction runs.
//!
//! # Why every query carries its own expected answer
//!
//! A soak that only checks for the absence of an error measures whether the server stays up. A
//! wrong number is not an error --- it is the failure this whole system is arranged against ---
//! so each statement here is paired with what it must return, computed **independently** in
//! this file rather than by the kernel under test.
//!
//! Where an exact expectation is impractical, an **invariant** stands in: a correlation is
//! between minus one and one, a p-value is between zero and one, a covariance matrix factors.
//! An invariant is weaker than a value and far stronger than nothing.

use std::collections::BTreeMap;

/// One statement the soak runs, and what makes its answer acceptable.
#[derive(Debug)]
pub struct Probe {
    /// A short name, for the report.
    pub name: &'static str,
    /// The statement, ready to send.
    pub sql: String,
    /// What the first column must be.
    pub expect: Expectation,
}

/// What an answer has to satisfy.
#[derive(Debug)]
pub enum Expectation {
    /// Exactly this number, within a tolerance.
    Near {
        /// The value, computed independently of the kernel.
        value: f64,
        /// How far off is acceptable.
        tolerance: f64,
    },
    /// Anywhere in this closed interval.
    Within {
        /// The lower bound.
        low: f64,
        /// The upper bound.
        high: f64,
    },
    /// This many rows, whatever they hold.
    Rows(usize),
    /// A refusal, whose message contains this.
    Refused(&'static str),
    /// Any answer at all, but **the same one every time**.
    ///
    /// The drift check. A statement whose value this file cannot predict --- a decomposition
    /// of generated data, say --- is still required to be reproducible, and that is the
    /// property a soak is uniquely able to test.
    Stable,
}

/// A vector column's width, everywhere in the soak.
///
/// Small enough that a covariance matrix of it is a matrix a person can check, large enough
/// that a similarity search is not trivially cache-resident.
pub const WIDTH: usize = 8;

/// How many rows each generated table holds.
pub const ROWS: usize = 4_000;

/// A deterministic generator, so a failing soak can be reproduced from its seed.
///
/// Not a random number generator in the statistical sense and not used as one: every figure
/// this file predicts is computed from the same sequence, so "the data" and "the expected
/// answer" cannot drift apart.
#[derive(Debug)]
pub struct Seeded(u64);

impl Seeded {
    /// Start from a seed.
    #[must_use]
    pub fn from(seed: u64) -> Self {
        Self(seed | 1)
    }

    /// The next raw value.
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// A value in `[-1, 1)`.
    pub fn signed(&mut self) -> f64 {
        #[allow(clippy::cast_precision_loss)]
        {
            (self.next() >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
        }
    }
}

/// The embeddings the soak searches, and the query it searches with.
///
/// Returned rather than regenerated per use, so the expected answers below are computed from
/// exactly the rows that were written.
#[must_use]
pub fn embeddings(seed: u64) -> Vec<Vec<f64>> {
    let mut seeded = Seeded::from(seed);
    (0..ROWS)
        .map(|_| {
            let raw: Vec<f64> = (0..WIDTH).map(|_| seeded.signed()).collect();
            // Normalised, so a cosine similarity is a dot product and the expected answers
            // below can be written without a division that would have to agree bit for bit.
            let norm = raw.iter().map(|x| x * x).sum::<f64>().sqrt();
            if norm > 0.0 {
                raw.iter().map(|x| x / norm).collect()
            } else {
                raw
            }
        })
        .collect()
}

/// A series per row: a yield curve, a window of readings, a term structure.
#[must_use]
pub fn series(seed: u64) -> Vec<Vec<f64>> {
    let mut seeded = Seeded::from(seed);
    (0..ROWS)
        .map(|_| {
            // Monotone-ish with noise, which is what a curve looks like and what makes a
            // derivative and an integral meaningful rather than white.
            let base = seeded.signed().abs() * 3.0;
            (0..WIDTH)
                .map(|i| {
                    #[allow(clippy::cast_precision_loss)]
                    let t = i as f64;
                    base + t * 0.5 + seeded.signed() * 0.05
                })
                .collect()
        })
        .collect()
}

/// A covariance matrix per row, positive definite by construction.
///
/// Built as `LLᵀ + εI` from a random lower triangle, so every one factors --- which is what
/// makes a Cholesky failure in the soak a real finding rather than a property of the fixture.
#[must_use]
pub fn covariances(seed: u64, size: usize, count: usize) -> Vec<Vec<f64>> {
    let mut seeded = Seeded::from(seed);
    (0..count)
        .map(|_| {
            let mut lower = vec![0.0f64; size * size];
            for row in 0..size {
                for column in 0..=row {
                    if let Some(slot) = lower.get_mut(row * size + column) {
                        *slot = if row == column {
                            seeded.signed().abs() + 0.5
                        } else {
                            seeded.signed()
                        };
                    }
                }
            }
            let mut matrix = vec![0.0f64; size * size];
            let get = |at: usize| lower.get(at).copied().unwrap_or(0.0);
            for i in 0..size {
                for j in 0..size {
                    let mut sum = 0.0;
                    for k in 0..size {
                        sum += get(i * size + k) * get(j * size + k);
                    }
                    if let Some(slot) = matrix.get_mut(i * size + j) {
                        *slot = sum + if i == j { 1e-6 } else { 0.0 };
                    }
                }
            }
            matrix
        })
        .collect()
}

/// A vector written as a `vec_of(...)` call, for a statement that needs a literal.
#[must_use]
pub fn literal(values: &[f64]) -> String {
    let parts: Vec<String> = values.iter().map(|v| format!("{v:?}")).collect();
    format!("vec_of({})", parts.join(", "))
}

/// The probes, with their answers computed here rather than by the server.
///
/// `table` is the qualified name of the embeddings table; `curves` of the series table;
/// `matrices` of the covariance table.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn probes(
    table: &str,
    curves: &str,
    matrices: &str,
    data: &Embedded,
) -> Vec<Probe> {
    let query = &data.query;
    let mut probes = Vec::new();

    // --- similarity search, the shape a vector column exists for -------------
    //
    // The expected answer is the best cosine over the rows this file generated, computed here
    // by the same arithmetic a person would use --- and deliberately not by calling the
    // kernel, which would make the check a tautology.
    probes.push(Probe {
        name: "best cosine",
        sql: format!(
            "SELECT max(vec_cosine_similarity(embedding, {})) FROM {table}",
            literal(query)
        ),
        expect: Expectation::Near { value: data.best_cosine, tolerance: 1e-9 },
    });
    probes.push(Probe {
        name: "similarity ordering",
        sql: format!(
            "SELECT id FROM {table} ORDER BY vec_cosine_similarity(embedding, {}) DESC LIMIT 5",
            literal(query)
        ),
        expect: Expectation::Rows(5),
    });
    // Every row's embedding was normalised, so every norm is one. A width read from the wrong
    // stride gives a norm that is not, which is how a silently wrong scan announces itself.
    probes.push(Probe {
        name: "every embedding is a unit vector",
        sql: format!("SELECT max(abs(vec_norm_l2(embedding) - 1.0)) FROM {table}"),
        expect: Expectation::Near { value: 0.0, tolerance: 1e-12 },
    });

    // --- reductions over a column, where determinism is the property ---------
    probes.push(Probe {
        name: "total of every component",
        sql: format!("SELECT sum(vec_sum(embedding)) FROM {table}"),
        expect: Expectation::Near { value: data.total, tolerance: 1e-9 },
    });

    // --- statistics within a row --------------------------------------------
    probes.push(Probe {
        name: "correlation stays in its range",
        sql: format!(
            "SELECT min(vec_correlation(embedding, {})) FROM {table}",
            literal(query)
        ),
        expect: Expectation::Within { low: -1.000_001, high: 1.000_001 },
    });
    probes.push(Probe {
        name: "a p-value is a probability",
        sql: format!("SELECT max(ttest_1samp_p(curve, 0.0)) FROM {curves}"),
        expect: Expectation::Within { low: 0.0, high: 1.0 },
    });
    probes.push(Probe {
        name: "normality test over a column of series",
        sql: format!("SELECT min(jarque_bera_p(curve)) FROM {curves}"),
        expect: Expectation::Within { low: 0.0, high: 1.0 },
    });

    // --- calculus over a curve ----------------------------------------------
    //
    // Each curve rises by about half per step, so a first difference averages near that. The
    // bound is loose because the noise is real; what it catches is a derivative computed over
    // the wrong axis, which lands nowhere near.
    probes.push(Probe {
        name: "a curve's slope is where it was built",
        sql: format!("SELECT avg(vec_mean(vec_differences(curve))) FROM {curves}"),
        expect: Expectation::Within { low: 0.4, high: 0.6 },
    });
    probes.push(Probe {
        name: "a cumulative integral ends at the whole area",
        sql: format!(
            "SELECT max(abs(vec_max(vec_cumulative_integral(curve)) - vec_integral(curve))) \
             FROM {curves}"
        ),
        expect: Expectation::Near { value: 0.0, tolerance: 1e-9 },
    });

    // --- linear algebra over a matrix column ---------------------------------
    //
    // Every fixture matrix was built as `LLᵀ + εI`, so every one factors. A failure here is a
    // real finding rather than a property of the data.
    probes.push(Probe {
        name: "every covariance factors",
        sql: format!("SELECT min(mat_is_positive_definite(covariance)) FROM {matrices}"),
        expect: Expectation::Near { value: 1.0, tolerance: 0.0 },
    });
    probes.push(Probe {
        name: "every covariance is symmetric",
        sql: format!("SELECT min(mat_is_symmetric(covariance)) FROM {matrices}"),
        expect: Expectation::Near { value: 1.0, tolerance: 0.0 },
    });
    // A positive-definite matrix has strictly positive eigenvalues, which is the same fact the
    // factorisation reports and reached by an entirely different route --- so the two
    // disagreeing is a finding neither could produce alone.
    probes.push(Probe {
        name: "eigenvalues agree with the factorisation",
        sql: format!("SELECT min(vec_min(mat_eigenvalues(covariance))) FROM {matrices}"),
        expect: Expectation::Within { low: 0.0, high: f64::INFINITY },
    });
    probes.push(Probe {
        name: "the trace is the sum of the eigenvalues",
        sql: format!(
            "SELECT max(abs(vec_sum(mat_eigenvalues(covariance)) - mat_trace(covariance))) \
             FROM {matrices}"
        ),
        expect: Expectation::Near { value: 0.0, tolerance: 1e-6 },
    });
    probes.push(Probe {
        name: "a decomposition is reproducible",
        sql: format!("SELECT sum(vec_sum(mat_cholesky(covariance))) FROM {matrices}"),
        expect: Expectation::Stable,
    });

    // --- distributions, against values that can be looked up -----------------
    probes.push(Probe {
        name: "the normal critical value",
        sql: "SELECT norm_inv(0.975)".to_owned(),
        expect: Expectation::Near { value: 1.959_963_984_540_054, tolerance: 1e-9 },
    });
    probes.push(Probe {
        name: "a far tail keeps its digits",
        sql: "SELECT chisq_sf(200.0, 5.0)".to_owned(),
        expect: Expectation::Within { low: f64::MIN_POSITIVE, high: 1e-30 },
    });

    // --- the refusals, which must keep refusing under load -------------------
    //
    // A soak that only runs the happy path measures whether the happy path stays up. A guard
    // that stops guarding after an hour is the failure worth finding.
    probes.push(Probe {
        name: "a fractional count stays refused",
        sql: "SELECT binom_pmf(2.5, 10, 0.5)".to_owned(),
        expect: Expectation::Refused("whole number"),
    });
    probes.push(Probe {
        name: "a non-square matrix stays refused",
        sql: format!("SELECT mat_eigenvalues(vec_of(1.0, 2.0, 3.0)) FROM {matrices} LIMIT 1"),
        expect: Expectation::Refused("square"),
    });
    probes.push(Probe {
        name: "mismatched widths stay refused",
        sql: format!("SELECT vec_dot(embedding, vec_of(1.0, 2.0)) FROM {table} LIMIT 1"),
        expect: Expectation::Refused("vec_dot"),
    });

    probes
}

/// The generated data, and the answers computed from it.
#[derive(Debug)]
pub struct Embedded {
    /// The query vector a similarity search uses.
    pub query: Vec<f64>,
    /// The largest cosine between the query and any row.
    pub best_cosine: f64,
    /// The total of every component of every embedding.
    pub total: f64,
}

/// Compute the expected answers from the rows that will be written.
///
/// **Not by calling the kernels.** A soak whose expectations come from the code under test
/// checks that the code agrees with itself, which it always will.
#[must_use]
pub fn expectations(rows: &[Vec<f64>], seed: u64) -> Embedded {
    let mut seeded = Seeded::from(seed ^ 0xabcd);
    let raw: Vec<f64> = (0..WIDTH).map(|_| seeded.signed()).collect();
    let norm = raw.iter().map(|x| x * x).sum::<f64>().sqrt();
    let query: Vec<f64> = raw.iter().map(|x| x / norm).collect();

    let mut best = f64::NEG_INFINITY;
    for row in rows {
        // Both sides are unit vectors, so the cosine is the dot product and no division has
        // to agree bit for bit between this and the server.
        let dot: f64 = row.iter().zip(&query).map(|(a, b)| a * b).sum();
        if dot > best {
            best = dot;
        }
    }

    // Summed in the order the server will see them, which is the order they are written.
    let total: f64 = sankhya_math::deterministic_sum(
        &rows.iter().map(|row| sankhya_math::deterministic_sum(row)).collect::<Vec<_>>(),
    );

    Embedded { query, best_cosine: best, total }
}

/// What one pass of the probes found.
#[derive(Default, Debug)]
pub struct Findings {
    /// How many probes ran.
    pub ran: usize,
    /// Probe name to the reason it failed.
    pub wrong: BTreeMap<&'static str, String>,
    /// Probe name to the first answer seen, for the drift check.
    pub first: BTreeMap<&'static str, String>,
}

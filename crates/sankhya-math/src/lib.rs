//! Mathematics: reduction, vectors, matrices, statistics and calculus.
//!
//! # One property runs through all of it
//!
//! **Every reduction is order-independent.** Floating-point addition is not associative, so
//! a total whose order depends on how work was partitioned returns a different number when
//! the machine is busier, has more cores, or read its files in a different order. The
//! difference is small, and that is what makes it expensive: too small to notice and too
//! large to reconcile, so it surfaces as a figure that will not tie out and nobody can
//! explain.
//!
//! [`deterministic_sum`] is the foundation, and a dot product, a variance, a correlation
//! and a trapezoidal integral are all sums --- so all of them inherit it. That is also why
//! this crate does not delegate to a numeric library: reordering freely for speed is what a
//! good one does, and it is precisely what cannot be permitted here.
//!
//! # What is here
//!
//! | Module | Holds |
//! |---|---|
//! | [`reduce`] | Deterministic summation, and the merge of partial sums |
//! | [`vector`] | Elementwise operations, dot, norms, distances |
//! | [`matrix`] | Multiplication, transpose, trace, and LU with determinant, inverse, solve |
//! | [`stats`] | Mean, variance, covariance, correlation, skewness, kurtosis, least squares |
//! | [`quantile`] | Exact order statistics, with the conventions named rather than assumed |
//! | [`calculus`] | Numerical differentiation and integration over sampled data |
//!
//! # What this crate refuses to do
//!
//! Every function either produces an exact, reproducible answer or refuses. There is no
//! approximate path, no default convention, and no silent handling of a value that cannot
//! be ordered --- because each of those turns a question with no good answer into a
//! plausible number, and a plausible wrong number is the most expensive thing a numerical
//! library can return.
//!
//! It also declines whole disciplines rather than doing them badly. There is no QR, no SVD
//! and no eigendecomposition: those are where a specialist library genuinely earns its
//! dependency, and a subtly wrong SVD produces plausible singular values.
//!
//! # A limitation worth stating
//!
//! Exact order statistics here **buffer their input**. Selection is linear rather than
//! `n log n`, so it is faster than sorting, but every observation must be resident. The
//! requirements call for a bounded-memory algorithm over large inputs and this is not one;
//! it is exact and it does not scale past memory. That gap is recorded rather than hidden,
//! because "exact" and "bounded" are independent properties and a caller needs to know
//! which one it is getting.

#![doc(html_root_url = "https://docs.rs/sankhya-math")]

pub mod calculus;
pub mod decompose;
pub mod finance;
pub mod inference;
pub mod distribution;
pub mod special;
pub mod matrix;
mod quantile;
pub mod regression;
mod reduce;
pub mod stats;
pub mod timeseries;
pub mod vector;

pub use special::DomainError;
pub use matrix::MatrixError;
pub use quantile::{quantile, quantile_of_sum, Convention, QuantileError};
pub use reduce::{combine_partials, deterministic_sum, exact_sum, Exact};
pub use stats::{LinearFit, Population};
pub use vector::{
    add, cosine_distance, cosine_similarity, divide, dot, euclidean, matvec, mean, multiply,
    norm_l1, norm_l2, row_of, scale, subtract, sum, VectorError,
};

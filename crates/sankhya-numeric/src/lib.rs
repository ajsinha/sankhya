//! Exact order statistics, deterministic reduction, and the compositions that are wrong.
//!
//! # What this crate refuses to do
//!
//! Every function here either produces an exact, reproducible answer or refuses. There
//! is no approximate path, no default convention, and no silent handling of a value that
//! cannot be ordered — because each of those turns a question with no good answer into a
//! plausible number, and a plausible wrong number is the most expensive thing a
//! numerical library can return.
//!
//! # A limitation worth stating
//!
//! Exact order statistics here **buffer their input**. Selection is linear rather than
//! `n log n`, so it is faster than sorting, but every observation must be resident. The
//! requirements call for a bounded-memory algorithm over large inputs and this is not
//! one; it is exact and it does not scale past memory. That gap is recorded rather than
//! hidden, because "exact" and "bounded" are independent properties and a caller needs
//! to know which one it is getting.

#![doc(html_root_url = "https://docs.rs/sankhya-numeric")]

pub mod matrix;
mod quantile;
mod reduce;
pub mod vector;

pub use matrix::MatrixError;
pub use quantile::{quantile, quantile_of_sum, Convention, QuantileError};
pub use reduce::{combine_partials, deterministic_sum};
pub use vector::{
    add, cosine_distance, cosine_similarity, divide, dot, euclidean, matvec, mean, multiply,
    norm_l1, norm_l2, row_of, scale, subtract, sum, VectorError,
};

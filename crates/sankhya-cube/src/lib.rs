//! The declared cube definition.
//!
//! # What this crate is for
//!
//! [`sankhya_cube_algo`] answers questions about a cube in the abstract: whether a measure
//! composes along a dimension, which cuboid answers which query, what is worth
//! materialising. It knows nothing about tables, columns or versions, and it cannot be
//! wrong about them because it cannot see them.
//!
//! This crate is where the algebra meets a real warehouse. A [`Definition`] names published
//! tables and their columns; validating it produces a [`Cube`], and **a `Cube` cannot be
//! constructed any other way**. Everything downstream --- planning, consolidation,
//! materialisation --- takes a `Cube`, so no code path exists that operates on a definition
//! nobody checked.
//!
//! # The default that produces wrong numbers
//!
//! Every OLAP product this design was measured against defaults an undeclared measure to
//! summation. It is the convenient choice and it is the single most productive source of
//! wrong analytics numbers there is, because summing a balance across time, or a rate across
//! anything, produces a figure that is plausible, wrong, and indistinguishable from a
//! correct one.
//!
//! SANKHYA refuses. A measure with no declared rule for a dimension is a **definition
//! error**, reported before the cube exists, naming every dimension it failed to declare
//! --- not the first one, since fixing them one build at a time is how a person gives up
//! and declares `Sum` everywhere.
//!
//! # The version nobody has to remember to bump
//!
//! A materialised cuboid is keyed by *(definition version, snapshot, cuboid)*, so a query
//! against a changed definition must miss rather than hit stale data. That is only true if
//! the version actually changes when the definition does.
//!
//! So it is **derived, not declared**: [`Cube::version`] is a fingerprint of the validated
//! content. There is no field to forget to increment, and no review that has to catch it.
//! The same reasoning made `queryable_at` derived in the backup manifest.

#![doc(html_root_url = "https://docs.rs/sankhya-cube")]

pub mod consolidate;
pub mod model;
pub mod validate;
pub mod version;

pub use model::{Definition, Dimension, Level, Cube};
pub use consolidate::{consolidate, Consolidation, Incomplete};
pub use validate::Rejection;

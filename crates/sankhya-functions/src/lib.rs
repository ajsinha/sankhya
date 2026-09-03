//! The built-in function catalogue.
//!
//! # Why this crate exists rather than another module in `sankhya-olap`
//!
//! `ADR-0020` Decision 4. `sankhya-olap` holds the session and the exactness policy, and a
//! catalogue meant to grow tenfold cannot share a crate's length budget with unrelated
//! concerns. One crate, one registration point, and one place to look for the answer to
//! *"what can this server compute?"*.
//!
//! # What is in here, and what is not
//!
//! **Not the mathematics.** Every kernel lives in `sankhya-math`, which is layer one, has no
//! dependency on a query engine, and carries the reproducibility argument the whole system
//! rests on. This crate is the *naming*: it says which kernel is called what, checks the
//! arguments a statement supplied, and turns a refusal into one a person can act on.
//!
//! That split is load-bearing. A kernel with logic in its wrapper is a kernel whose behaviour
//! depends on how it was reached, and this system has two doors.

#![doc(html_root_url = "https://docs.rs/sankhya-functions")]

pub mod catalogue;
pub mod describe;
pub mod distributions;
pub mod entry;
pub mod inference;
pub mod routing;
pub mod linalg;
pub mod quant;
pub mod multi;
mod property;
pub mod rows;
pub mod seriesnum;
mod scalar;
mod series;

pub use entry::{Entry, Gives, Takes};
pub use routing::{built_ins_in, tier_for, Tier};
pub use scalar::{Numeric, NumericKernel};
pub use property::{Property, PropertyKernel};
pub use rows::Vectors;
pub use series::{ArrayKernel, Series};

use datafusion::logical_expr::ScalarUDF;
use datafusion::prelude::SessionContext;

/// Register every built-in against a session.
///
/// One call, so a session has the whole catalogue or none of it. A partially registered set
/// means a query works on one node and fails on another, and the difference is invisible until
/// somebody runs the same statement twice.
pub fn register(context: &SessionContext) {
    for function in functions() {
        context.register_udf(function);
    }
    // The catalogue over this crate's own entries. A server that also registers the vector,
    // matrix, cube and graph functions calls `describe::register` itself with the whole list,
    // which replaces this --- see its doc comment for why the entries are passed in.
    describe::register(context, catalogue::mine());
}

/// Every function this crate offers.
///
/// Assembled from the category modules rather than listed here, so adding a function is an
/// edit in one place.
#[must_use]
pub fn functions() -> Vec<ScalarUDF> {
    let mut all = distributions::functions();
    all.extend(linalg::functions());
    all.extend(linalg::property_functions());
    all.extend(inference::functions());
    all.extend(quant::functions());
    all
}

/// Every function's name, for a catalogue a client can enumerate.
///
/// `ADR-0020` Decision 4: a capability nobody can list is a reference manual nobody reads.
#[must_use]
pub fn names() -> Vec<String> {
    let mut names: Vec<String> = functions().iter().map(|f| f.name().to_owned()).collect();
    names.sort();
    names
}

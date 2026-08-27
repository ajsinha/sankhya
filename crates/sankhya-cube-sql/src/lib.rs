//! Cube navigation exposed as SQL table functions.
//!
//! A cube that cannot be joined against a table is a separate product with its own query
//! language, and the point of putting multidimensional analysis in the same engine is that
//! it is not one. `FR-CUBE-14` asks for slice, dice, roll-up, drill-down and pivot
//! **expressible from SQL with no separate build step**, so every operation here is a table
//! function with a fixed output schema, callable in a `FROM` clause:
//!
//! ```sql
//! SELECT r.region, r.amount, p.manager
//! FROM cube_rollup('figures', 'amount', 'by=region') AS r
//! JOIN people AS p ON p.region = r.region
//! WHERE r.completeness = 1.0 AND r.overlay IS NULL
//! ```
//!
//! # Everything that qualifies a number is a column
//!
//! This is the one design decision here, and it is inherited from
//! [`sankhya_graph_sql`](https://docs.rs/sankhya-graph-sql)'s truncation columns, which
//! were built as query metadata first and moved into the rows for a reason that applies with
//! more force to a cube:
//!
//! > A qualification that lives outside the rows is dropped by the first `SELECT` that does
//! > not mention it.
//!
//! For a graph that costs you a truncated result read as a complete one. For a cube the same
//! mistake produces a **partial total read as a total**, or a **what-if figure read as
//! fact** --- numbers that reconcile against nothing, in a report, with no way to tell from
//! the value what went wrong.
//!
//! So `completeness`, `withheld`, `overlay`, `materialised` and `from_cuboid` are columns on
//! every row. A query may project them away, but it has to do it on purpose, and the
//! statement then says so in its own text.
//!
//! # Why `materialised` is visible at all
//!
//! Not for correctness --- `M7`'s exit criterion 3a requires the answer to be identical
//! either way, and it is, down to the bit. It is there because "why was this fast?" and "why
//! was this slow?" are the same question, and an operator cannot answer either from a result
//! that only carries numbers.

#![doc(html_root_url = "https://docs.rs/sankhya-cube-sql")]

mod args;
pub mod catalog;
pub mod functions;
mod result;

pub use catalog::{CubeCatalog, Published, Unresolved};
pub use functions::register;

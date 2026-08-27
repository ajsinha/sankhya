//! Graph primitives exposed as SQL table functions.
//!
//! A traversal that cannot be joined against a table is a separate system with its own
//! query language, and the whole point of putting the graph in the same engine is that it
//! is not one. So every primitive here is a table function with a fixed output schema,
//! callable in a `FROM` clause and joinable like anything else:
//!
//! ```sql
//! SELECT p.name, r.depth
//! FROM graph_reachable('payments', 'acct-1', max_depth => 3) AS r
//! JOIN parties AS p ON p.key = r.vertex
//! WHERE NOT r.truncated
//! ```
//!
//! # Three things these functions do that a naive binding would not
//!
//! **They report statistics.** `FR-GRAPH-12` is blunt about why: without them the planner
//! orders the downstream join badly. A traversal returning forty rows joined against a
//! million-row table should drive the join from the traversal, and a planner told nothing
//! assumes otherwise. The traversal runs at planning time, so the row count reported is
//! exact rather than estimated.
//!
//! **They carry their truncation into the result.** Every row has a `truncated` column and
//! a `truncation_reason`. `FR-GRAPH-14` requires that a truncated result never be
//! mistakable for an absence of results, and a flag that lives outside the rows gets
//! dropped by the first projection that does not mention it.
//!
//! **They say which epoch answered.** Every row carries `epoch` and `snapshot`, so a graph
//! result can be reconciled with a relational one taken at a different moment. Without it
//! the two disagree in ways nobody can account for.
//!
//! # The cost of running at planning time
//!
//! `TableFunctionImpl::call_with_args` is invoked while the statement is being planned, so
//! the traversal happens there and the rows are materialised before execution begins. That
//! is what makes the statistics exact, and it is only defensible because every traversal is
//! hard-bounded --- the default budget stops at a thousand results. A primitive without a
//! bound could not be implemented this way, which is one more reason there is not one.

#![doc(html_root_url = "https://docs.rs/sankhya-graph-sql")]

pub mod args;
pub mod catalog;
pub mod functions;
pub mod result;

pub use args::Arguments;
pub use catalog::{GraphCatalog, Unresolved};
pub use functions::register;
pub use result::TraversalTable;

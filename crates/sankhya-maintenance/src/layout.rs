//! What a table's physical layout is declared to be.
//!
//! # Clustering, not partitioning
//!
//! `ARCHITECTURE` §9.8 states the bias plainly:
//!
//! > **partitioning is a physical commitment that is expensive or impossible to undo;
//! > clustering is cheap to change at the next compaction. When uncertain, prefer the
//! > reversible decision --- sort, do not partition.**
//!
//! So this declares a **sort order**, applied when a partition is compacted and settled. It
//! changes nothing about where files live, adds no directories, and a table that turns out
//! to be clustered on the wrong column is fixed by editing one line and waiting for the next
//! compaction. Adding a partition column instead multiplies the directory count by its
//! cardinality, is recorded permanently in the log, and is undone by rewriting the table.
//!
//! The gain is not small. Ordering a table by its date column took `TPC-H` Q6 --- which
//! selects one year in seven --- from 229 ms to 55 ms, because row-group bounds only prune
//! when the rows within a file are ordered by the thing being filtered on.
//!
//! # Why it is declared and not inferred
//!
//! §9.8 again: *"the engine cannot distinguish a meaningful query boundary from a merely
//! low-cardinality column"*. A column with four values looks identical to the engine whether
//! it is the axis every query filters on or an enum nobody has ever mentioned in a `WHERE`
//! clause. Guessing produces a table sorted for queries nobody runs, and the cost is paid at
//! every compaction for ever.

use sankhya_config::Configuration;

/// The configuration key a table's clustering is declared under.
///
/// `table.<schema>.<table>.clustering`, comma-separated, most significant column first ---
/// the same order a `SORT BY` would take, so somebody reading the setting and somebody
/// reading a query mean the same thing by it.
#[must_use]
pub fn clustering_key(schema: &str, table: &str) -> String {
    format!("table.{schema}.{table}.clustering")
}

/// The columns a table is declared to be clustered on.
///
/// Empty when nothing is declared, which is the honest default: an undeclared table is not
/// clustered, rather than clustered on whatever seemed reasonable.
#[must_use]
pub fn clustering(config: &Configuration, schema: &str, table: &str) -> Vec<String> {
    config
        .list(&clustering_key(schema, table))
        .unwrap_or_default()
}

/// Whether the declared clustering names only columns the table has.
///
/// # Errors
/// The names that are not columns. A clustering on a column that does not exist would make
/// every compaction fail, or --- worse, if it were skipped --- would leave a table that
/// reports itself clustered and is not, which nothing downstream can detect.
pub fn check(columns: &[String], declared: &[String]) -> Result<(), Vec<String>> {
    let missing: Vec<String> = declared
        .iter()
        .filter(|name| !columns.contains(name))
        .cloned()
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(missing)
    }
}

//! `functions()` --- the catalogue, as a table.
//!
//! # Why this exists
//!
//! For the reason `cubes()` exists: **a capability nobody can enumerate is a reference manual
//! nobody reads.** Before it, the only way to learn what this server could compute was to read
//! Rust, and two things followed from that. No binding could offer the functions, because a
//! binding cannot generate what it cannot list. And this document's own first draft had to be
//! produced by dumping a session from a test, which is not a thing a user can do.
//!
//! # What it deliberately does not do
//!
//! It does not evaluate anything. `functions()` is a description of the surface, so a client
//! can build a picker, a binding can generate its methods, and a person can find the name they
//! half-remember. Every one of those is a *reading* of the catalogue, and none of them should
//! have to run a kernel to get it.

use crate::entry::Entry;
use arrow_array::{ArrayRef, RecordBatch, StringArray, UInt32Array};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use datafusion::catalog::{MemTable, TableFunctionImpl, TableProvider};
use datafusion::common::Result;
use datafusion::logical_expr::Expr;
use datafusion::prelude::SessionContext;
use std::sync::Arc;

/// Register `functions()` against a session, over the entries given.
///
/// The entries are passed in rather than taken from this crate alone, because the catalogue a
/// **user** sees must include the vector, matrix, cube and graph functions too --- and those
/// are registered by other crates. A `functions()` that listed only this crate's would be a
/// catalogue that is wrong about the thing it exists to describe.
pub fn register(context: &SessionContext, entries: Vec<Entry>) {
    context.register_udtf("functions", Arc::new(Catalogue(Arc::new(entries))));
}

/// The table function.
#[derive(Debug)]
struct Catalogue(Arc<Vec<Entry>>);

impl TableFunctionImpl for Catalogue {
    fn call(&self, _arguments: &[Expr]) -> Result<Arc<dyn TableProvider>> {
        let schema: SchemaRef = Arc::new(Schema::new(vec![
            Field::new("function", DataType::Utf8, false),
            Field::new("category", DataType::Utf8, false),
            Field::new("arity", DataType::UInt32, false),
            Field::new("takes", DataType::Utf8, false),
            Field::new("gives", DataType::Utf8, false),
            Field::new("about", DataType::Utf8, false),
        ]));

        // Sorted by name, so two calls give the same order and a client diffing the catalogue
        // between versions sees only what changed.
        let mut entries: Vec<&Entry> = self.0.iter().collect();
        entries.sort_by_key(|entry| entry.name);

        let names: Vec<&str> = entries.iter().map(|e| e.name).collect();
        let categories: Vec<&str> = entries.iter().map(|e| e.category).collect();
        #[allow(clippy::cast_possible_truncation)]
        let arities: Vec<u32> = entries.iter().map(|e| e.arity as u32).collect();
        let takes: Vec<&str> = entries.iter().map(|e| e.takes.as_str()).collect();
        let gives: Vec<&str> = entries.iter().map(|e| e.gives.as_str()).collect();
        let about: Vec<&str> = entries.iter().map(|e| e.about).collect();

        let columns: Vec<ArrayRef> = vec![
            Arc::new(StringArray::from(names)),
            Arc::new(StringArray::from(categories)),
            Arc::new(UInt32Array::from(arities)),
            Arc::new(StringArray::from(takes)),
            Arc::new(StringArray::from(gives)),
            Arc::new(StringArray::from(about)),
        ];
        let batch = RecordBatch::try_new(Arc::clone(&schema), columns)?;
        Ok(Arc::new(MemTable::try_new(schema, vec![vec![batch]])?))
    }
}

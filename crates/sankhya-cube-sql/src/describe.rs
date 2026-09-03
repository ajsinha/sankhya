//! What a client needs to know before it can ask a cube anything.
//!
//! # Why this exists
//!
//! A cube surface that can only be *used* by somebody who already knows the cube's name, its
//! dimensions and its measures is a surface only its author can use. Every client that wants
//! to offer a picker — a UI, a notebook, an agent composing SQL — has to discover the model
//! first, and without this the only way is to hardcode it and drift.
//!
//! These are ordinary table functions returning ordinary rows, so a client discovers a cube
//! with the same `SELECT` it uses for everything else, and no second protocol exists to keep
//! in step with the first.
//!
//! # What is deliberately not here
//!
//! Cells. Describing a cube must not read its fact table: a picker that costs a hydration per
//! keystroke is a picker nobody leaves switched on. Everything below comes from the
//! definition, which is already in memory.

use crate::catalog::CubeCatalog;
use arrow_array::{ArrayRef, RecordBatch, StringArray, UInt32Array, UInt64Array};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use datafusion::catalog::TableProvider;
use datafusion::common::Result;
use datafusion::datasource::MemTable;
use datafusion::error::DataFusionError;
use datafusion::logical_expr::Expr;
use datafusion::prelude::SessionContext;
use sankhya_cube::model::Cube;
use std::sync::Arc;

/// Register the description functions against a session.
pub fn register(context: &SessionContext, cubes: Arc<Vec<Cube>>, catalog: Arc<CubeCatalog>) {
    context.register_udtf("cubes", Arc::new(Cubes(Arc::clone(&cubes), catalog)));
    context.register_udtf("cube_dimensions", Arc::new(Dimensions(Arc::clone(&cubes))));
    context.register_udtf("cube_measures", Arc::new(Measures(cubes)));
}

/// The cube a call names, or an error listing the ones that exist.
fn named<'a>(cubes: &'a [Cube], exprs: &[Expr]) -> Result<&'a Cube> {
    let Some(Expr::Literal(value, _)) = exprs.first() else {
        return Err(DataFusionError::Plan(
            "name the cube to describe, as cube_dimensions('sales')".to_string(),
        ));
    };
    let wanted = value.to_string().trim_matches('\'').to_string();
    cubes.iter().find(|cube| cube.name() == wanted).ok_or_else(|| {
        let known: Vec<&str> = cubes.iter().map(Cube::name).collect();
        DataFusionError::Plan(format!(
            "no cube named '{wanted}' — this server serves {known:?}"
        ))
    })
}

/// One batch as a table.
fn table(schema: SchemaRef, columns: Vec<ArrayRef>) -> Result<Arc<dyn TableProvider>> {
    let batch = RecordBatch::try_new(Arc::clone(&schema), columns)?;
    Ok(Arc::new(MemTable::try_new(schema, vec![vec![batch]])?))
}

/// `cubes()` — every cube this server serves.
#[derive(Debug)]
struct Cubes(Arc<Vec<Cube>>, Arc<CubeCatalog>);

impl datafusion::catalog::TableFunctionImpl for Cubes {
    fn call(&self, _exprs: &[Expr]) -> Result<Arc<dyn TableProvider>> {
        let schema = Arc::new(Schema::new(vec![
            Field::new("cube", DataType::Utf8, false),
            Field::new("fact_table", DataType::Utf8, false),
            // Every table this cube reads, comma-separated. For a cube over a named table it
            // is that name; for a cube over a declared query it is what the query was found
            // to read --- and that is what the cube is authorized against and keyed on, so a
            // reader who can see the query and not its dependencies has been shown the half
            // that does not decide anything.
            Field::new("reads", DataType::Utf8, false),
            // The fingerprint of the definition. A client caching anything about a cube keys
            // it on this, so an edited definition invalidates the client's copy the same way
            // it invalidates the server's cells.
            Field::new("definition_version", DataType::UInt64, false),
            Field::new("dimensions", DataType::UInt32, false),
            Field::new("measures", DataType::UInt32, false),
            // Which measures have cells in this session. A client can tell a cube it may ask
            // about from one that would answer "not published yet".
            Field::new("hydrated_measures", DataType::UInt32, false),
        ]));
        let names: Vec<&str> = self.0.iter().map(Cube::name).collect();
        let facts: Vec<&str> = self.0.iter().map(Cube::fact_table).collect();
        let reads: Vec<String> = self.0.iter().map(|c| c.reads().join(", ")).collect();
        let versions: Vec<u64> = self.0.iter().map(Cube::version).collect();
        #[allow(clippy::cast_possible_truncation)]
        let dimensions: Vec<u32> = self.0.iter().map(|c| c.dimensions().len() as u32).collect();
        #[allow(clippy::cast_possible_truncation)]
        let measures: Vec<u32> = self.0.iter().map(|c| c.measures().len() as u32).collect();
        #[allow(clippy::cast_possible_truncation)]
        let hydrated: Vec<u32> = self
            .0
            .iter()
            .map(|c| self.1.published_measures(c.name()).len() as u32)
            .collect();

        table(
            schema,
            vec![
                Arc::new(StringArray::from(names)),
                Arc::new(StringArray::from(facts)),
                Arc::new(StringArray::from(reads)),
                Arc::new(UInt64Array::from(versions)),
                Arc::new(UInt32Array::from(dimensions)),
                Arc::new(UInt32Array::from(measures)),
                Arc::new(UInt32Array::from(hydrated)),
            ],
        )
    }
}

/// `cube_dimensions(cube)` — its dimensions and their levels, coarse to fine.
#[derive(Debug)]
struct Dimensions(Arc<Vec<Cube>>);

impl datafusion::catalog::TableFunctionImpl for Dimensions {
    fn call(&self, exprs: &[Expr]) -> Result<Arc<dyn TableProvider>> {
        let cube = named(&self.0, exprs)?;
        let schema = Arc::new(Schema::new(vec![
            Field::new("dimension", DataType::Utf8, false),
            Field::new("level", DataType::Utf8, false),
            // Ordinal rather than an implied row order: levels are ordered coarse to fine and
            // that order is a *fact about the model*, not about how the rows arrived. A client
            // sorting the result must get the hierarchy back, not alphabetical order.
            Field::new("depth", DataType::UInt32, false),
            Field::new("column", DataType::Utf8, false),
            Field::new("joins_on", DataType::Utf8, false),
            Field::new("member_table", DataType::Utf8, false),
            // A parent-child dimension has no fixed depth, and a client that drew it as a
            // level hierarchy would pad it — inventing members that do not exist.
            Field::new("parent_child", DataType::Boolean, false),
        ]));

        let mut dimension = Vec::new();
        let mut level = Vec::new();
        let mut depth = Vec::new();
        let mut column = Vec::new();
        let mut joins_on = Vec::new();
        let mut member_table = Vec::new();
        let mut parent_child = Vec::new();
        for declared in cube.dimensions() {
            for (index, at) in declared.levels.iter().enumerate() {
                dimension.push(declared.name.clone());
                level.push(at.name.clone());
                depth.push(u32::try_from(index).unwrap_or(u32::MAX));
                column.push(at.column.clone());
                joins_on.push(declared.joins_on.clone());
                member_table.push(declared.table.clone());
                parent_child.push(declared.parent_child.is_some());
            }
        }

        table(
            schema,
            vec![
                Arc::new(StringArray::from(dimension)),
                Arc::new(StringArray::from(level)),
                Arc::new(UInt32Array::from(depth)),
                Arc::new(StringArray::from(column)),
                Arc::new(StringArray::from(joins_on)),
                Arc::new(StringArray::from(member_table)),
                Arc::new(arrow_array::BooleanArray::from(parent_child)),
            ],
        )
    }
}

/// `cube_measures(cube)` — its measures, and the rule along every dimension.
#[derive(Debug)]
struct Measures(Arc<Vec<Cube>>);

impl datafusion::catalog::TableFunctionImpl for Measures {
    fn call(&self, exprs: &[Expr]) -> Result<Arc<dyn TableProvider>> {
        let cube = named(&self.0, exprs)?;
        let schema = Arc::new(Schema::new(vec![
            Field::new("measure", DataType::Utf8, false),
            Field::new("dimension", DataType::Utf8, false),
            Field::new("rule", DataType::Utf8, false),
            // Whether this rule composes. A client offering "roll up by time" on a
            // non-composing measure is offering a button that cannot work, and finding out
            // at query time is worse than not offering it.
            Field::new("composes", DataType::Boolean, false),
        ]));

        let mut measure = Vec::new();
        let mut dimension = Vec::new();
        let mut rule = Vec::new();
        let mut composes = Vec::new();
        for declared in cube.measures() {
            for along in &declared.rules {
                measure.push(declared.name.clone());
                dimension.push(along.dimension.clone());
                rule.push(along.rule.as_str().to_string());
                composes.push(along.rule.composes());
            }
        }

        table(
            schema,
            vec![
                Arc::new(StringArray::from(measure)),
                Arc::new(StringArray::from(dimension)),
                Arc::new(StringArray::from(rule)),
                Arc::new(arrow_array::BooleanArray::from(composes)),
            ],
        )
    }
}

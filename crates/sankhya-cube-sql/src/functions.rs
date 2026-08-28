//! The table functions themselves.
//!
//! Each resolves a named cube, applies any overlay, narrows and rolls up as the options
//! ask, and returns the rows with everything that qualifies the numbers attached to every
//! one of them.
//!
//! The common columns are the point. `completeness` and `withheld` make a policy-filtered
//! total impossible to project away accidentally; `overlay` names the scenario a what-if
//! came from; `definition_version` and `snapshot` let a cube figure be reconciled with a
//! relational one taken at a different moment; `materialised` and `from_cuboid` answer "why
//! was this fast or slow?". Every one of them would be tidier as query metadata, and every
//! one would then be lost by the first `SELECT` that did not mention it.

use crate::args::Arguments;
use crate::catalog::{CubeCatalog, Published};
use crate::result::CubeTable;
use arrow_array::{
    ArrayRef, BooleanArray, Float64Array, RecordBatch, StringArray, UInt64Array,
};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use datafusion::catalog::{TableFunctionImpl, TableProvider};
use datafusion::common::{plan_datafusion_err, plan_err, Result};
use datafusion::execution::context::SessionContext;
use datafusion::logical_expr::Expr;
use sankhya_cube::cells::Cells;
use sankhya_cube::complete::{Assessed, Completeness, Threshold};
use sankhya_cube::navigate::{dice, roll_up, slice, Ordered};
use sankhya_cube::overlay::{Allocation, Applied};
use sankhya_cube_algo::measure::{Measure, Rule};
use std::sync::Arc;

/// Register every cube function against a session.
///
/// One call, so a session either has the whole surface or none of it. A partially
/// registered catalogue means a query works on one node and fails on another.
pub fn register(context: &SessionContext, catalog: Arc<CubeCatalog>) {
    context.register_udtf("cube_rollup", Arc::new(RollUp(Arc::clone(&catalog))));
    context.register_udtf("cube_slice", Arc::new(Slice(catalog)));
}

/// The columns every cube function carries, whatever else it returns.
fn provenance_fields() -> Vec<Field> {
    vec![
        Field::new("definition_version", DataType::UInt64, false),
        Field::new("snapshot", DataType::UInt64, false),
        Field::new("materialised", DataType::Boolean, false),
        Field::new("from_cuboid", DataType::Utf8, false),
        // Nullable: an aggregate over no rows has *no* completeness, which is not the same
        // as complete. See `sankhya_cube::complete`.
        Field::new("completeness", DataType::Float64, true),
        Field::new("withheld", DataType::UInt64, false),
        // Null means published data. A name means a what-if, and which one.
        Field::new("overlay", DataType::Utf8, true),
    ]
}

/// The provenance columns, filled for `rows` rows.
fn provenance_columns(
    published: &Published,
    completeness: &Completeness,
    overlay: Option<&str>,
    from: &str,
    materialised: bool,
    rows: usize,
) -> Vec<ArrayRef> {
    vec![
        Arc::new(UInt64Array::from(vec![published.cube.version(); rows])),
        Arc::new(UInt64Array::from(vec![published.snapshot; rows])),
        Arc::new(BooleanArray::from(vec![materialised; rows])),
        Arc::new(StringArray::from(vec![from; rows])),
        Arc::new(Float64Array::from(vec![completeness.fraction(); rows])),
        Arc::new(UInt64Array::from(vec![completeness.withheld(); rows])),
        Arc::new(StringArray::from(vec![overlay; rows])),
    ]
}

/// Resolve the cells for the cube named in argument one and the measure in argument two.
///
/// Both, together, because a set of cells holds one measure's values --- so the pair is the
/// identity of what a query is asking for, and resolving on the cube alone gives whichever
/// measure happened to be published last.
fn cube_of(catalog: &CubeCatalog, args: &Arguments) -> Result<Published> {
    let name = args.string_at(0, "cube name")?;
    let measure = args.string_at(1, "measure name")?;
    catalog
        .resolve(&name, &measure)
        .map_err(|e| plan_datafusion_err!("{e}"))
}

/// Resolve the measure named in argument two against the cube.
///
/// An unknown measure is refused rather than defaulted. There is no sensible default: the
/// measure decides which roll-ups are legal, so guessing one produces an answer computed
/// under rules nobody chose.
fn measure_of(published: &Published, args: &Arguments) -> Result<Measure> {
    let name = args.string_at(1, "measure name")?;
    // Belt and braces. `cube_of` resolved the cells *by* this measure, so a mismatch here
    // would mean the catalogue filed cells under a name that is not their own --- which
    // `CubeCatalog::publish` makes impossible by taking the name from the cells. Asserted
    // anyway, because the failure it guards is a plausible number rather than an error, and
    // that is worth one comparison.
    debug_assert_eq!(published.measure, name, "cells filed under another measure's name");
    published.cube.measure(&name).cloned().ok_or_else(|| {
        let known: Vec<&str> = published.cube.measures().iter().map(|m| m.name.as_str()).collect();
        plan_datafusion_err!(
            "cube '{}' has no measure named '{}' — it has {:?}",
            published.cube.name(),
            name,
            known
        )
    })
}

/// Apply the overlay the options name, if any.
fn overlaid(
    catalog: &CubeCatalog,
    published: &Published,
    args: &Arguments,
) -> Result<Applied<Cells>> {
    let Some(name) = args.string("overlay") else {
        return Ok(Applied::published((*published.cells).clone()));
    };
    let overlay = catalog
        .overlay(&name)
        .map_err(|e| plan_datafusion_err!("{e}"))?;
    let allocation = match args.string("allocate").as_deref() {
        None | Some("refuse") => Allocation::Refuse,
        Some("pro_rata") => Allocation::ProRata,
        Some(other) => {
            return plan_err!(
                "'{other}' is not an allocation. Write 'allocate=pro_rata' to spread an \
                 edited total across the cells beneath it, or leave it out to be refused a \
                 grain the overlay does not contain"
            )
        }
    };
    overlay
        .apply(&published.cells, published.cube.version(), allocation)
        .map_err(|e| plan_datafusion_err!("{e}"))
}

/// Narrow by the `where` option, written as `dimension:member|member`.
fn narrowed(cells: &Cells, args: &Arguments) -> Result<Cells> {
    let restrictions = args.list("where");
    if restrictions.is_empty() {
        return Ok(cells.clone());
    }
    let mut parsed: Vec<(String, Vec<String>)> = Vec::new();
    for restriction in &restrictions {
        let Some((dimension, members)) = restriction.split_once(':') else {
            return plan_err!(
                "'{restriction}' is not a restriction. Write them as \
                 'where=region:north/south|period:jan', dimension first"
            );
        };
        parsed.push((
            dimension.trim().to_string(),
            members.split('/').map(|m| m.trim().to_string()).collect(),
        ));
    }

    let borrowed: Vec<(&str, Vec<&str>)> = parsed
        .iter()
        .map(|(d, m)| (d.as_str(), m.iter().map(String::as_str).collect()))
        .collect();
    let pairs: Vec<(&str, &[&str])> = borrowed
        .iter()
        .map(|(d, m)| (*d, m.as_slice()))
        .collect();

    let diced = dice(cells, &pairs);
    if !diced.ignored.is_empty() {
        // A filter that silently did not apply is found in a reconciliation months later.
        return plan_err!(
            "cube '{}' has no dimension(s) {:?}, named in the 'where' option. Refused \
             rather than ignored: a restriction that did not apply returns more rows than \
             the query asked for, and nothing says so",
            "this cube",
            diced.ignored
        );
    }
    Ok(diced.cells)
}

/// Roll up to the dimensions the `by` option names, dropping the rest.
fn rolled(cells: &Cells, measure: &Measure, args: &Arguments) -> Result<Cells> {
    let keep = args.list("by");
    let order = args.list("order");
    let stated: Vec<&str> = order.iter().map(String::as_str).collect();

    let mut out = cells.clone();
    let dropping: Vec<String> = out
        .dimensions()
        .iter()
        .filter(|d| !keep.contains(d))
        .cloned()
        .collect();
    for dimension in dropping {
        let ordered = if stated.is_empty() {
            Ordered::Unstated
        } else {
            Ordered::By(&stated)
        };
        out = roll_up(&out, &dimension, measure, ordered)
            .map_err(|e| plan_datafusion_err!("{e}"))?;
    }
    Ok(out)
}

/// Build the output schema: one column per dimension, the measure, then provenance.
fn schema_for(dimensions: &[String], measure: &str) -> SchemaRef {
    let mut fields: Vec<Field> = dimensions
        .iter()
        .map(|d| Field::new(d, DataType::Utf8, false))
        .collect();
    // Nullable: an absent cell is absent, not zero. See `sankhya_cube::cells`.
    fields.push(Field::new(measure, DataType::Float64, true));
    fields.extend(provenance_fields());
    Arc::new(Schema::new(fields))
}

/// Turn a cube into rows.
fn batch(
    published: &Published,
    cells: &Cells,
    measure: &Measure,
    overlay: Option<&str>,
    completeness: &Completeness,
    materialised: bool,
) -> Result<Arc<dyn TableProvider>> {
    let dimensions: Vec<String> = cells.dimensions().to_vec();
    let schema = schema_for(&dimensions, &measure.name);

    let rows: Vec<(&Vec<String>, Option<f64>)> = cells
        .addresses()
        .map(|address| (address, cells.get(address, Rule::Sum)))
        .collect();

    let mut columns: Vec<ArrayRef> = Vec::with_capacity(schema.fields().len());
    for axis in 0..dimensions.len() {
        let members: Vec<&str> = rows
            .iter()
            .map(|(address, _)| address.get(axis).map_or("", String::as_str))
            .collect();
        columns.push(Arc::new(StringArray::from(members)));
    }
    let values: Vec<Option<f64>> = rows.iter().map(|(_, value)| *value).collect();
    columns.push(Arc::new(Float64Array::from(values)));

    let from = if dimensions.is_empty() {
        "()".to_string()
    } else {
        dimensions.join(", ")
    };
    columns.extend(provenance_columns(
        published,
        completeness,
        overlay,
        &from,
        materialised,
        rows.len(),
    ));

    let batch = RecordBatch::try_new(Arc::clone(&schema), columns)
        .map_err(|e| plan_datafusion_err!("building the cube result: {e}"))?;
    Ok(Arc::new(CubeTable::new(schema, batch)))
}

/// Refuse the whole result when it saw too little of its input.
///
/// `FR-QUERY-13`, at the surface a caller actually uses. The threshold is a per-query
/// option because how much filtering is acceptable is a property of the question, not of
/// the cube.
fn check_completeness(completeness: &Completeness, args: &Arguments) -> Result<()> {
    let Some(required) = args.number("min_completeness")? else {
        return Ok(());
    };
    let threshold = Threshold::at_least(required).map_err(|e| plan_datafusion_err!("{e}"))?;
    Assessed::new((), *completeness)
        .meeting(&threshold)
        .map_err(|e| plan_datafusion_err!("{e}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------

/// `cube_rollup(cube, measure, options)` --- a breakdown at the grain `by` names.
#[derive(Debug)]
struct RollUp(Arc<CubeCatalog>);

impl TableFunctionImpl for RollUp {
    fn call(&self, exprs: &[Expr]) -> Result<Arc<dyn TableProvider>> {
        let args = Arguments::parse(exprs, 2)?;
        let published = cube_of(&self.0, &args)?;
        let measure = measure_of(&published, &args)?;

        let applied = overlaid(&self.0, &published, &args)?;
        let overlay = applied.overlay().map(str::to_string);
        let narrowed = narrowed(applied.regardless(), &args)?;

        // From hydration, not from the rows that survived. Dicing is a query narrowing
        // rather than a loss, so it does not change what fraction of the fact table this
        // cube saw.
        let completeness = published.completeness;
        check_completeness(&completeness, &args)?;

        let rolled = rolled(&narrowed, &measure, &args)?;
        let materialised = args.boolean("materialise")?.unwrap_or(false);
        batch(
            &published,
            &rolled,
            &measure,
            overlay.as_deref(),
            &completeness,
            materialised,
        )
    }
}

/// `cube_slice(cube, measure, options)` --- one member fixed, that axis dropped.
#[derive(Debug)]
struct Slice(Arc<CubeCatalog>);

impl TableFunctionImpl for Slice {
    fn call(&self, exprs: &[Expr]) -> Result<Arc<dyn TableProvider>> {
        let args = Arguments::parse(exprs, 2)?;
        let published = cube_of(&self.0, &args)?;
        let measure = measure_of(&published, &args)?;

        let Some(restriction) = args.string("where") else {
            return plan_err!(
                "cube_slice needs the member to slice to, as 'where=region:north'"
            );
        };
        let Some((dimension, member)) = restriction.split_once(':') else {
            return plan_err!("'{restriction}' is not a slice. Write it as 'where=region:north'");
        };
        if published
            .cells
            .axis(dimension.trim())
            .is_none()
        {
            return plan_err!(
                "'{}' is not a dimension of this cube. Slicing on one it does not have \
                 would return every row unchanged, which reads as a filter that matched \
                 everything",
                dimension.trim()
            );
        }

        let applied = overlaid(&self.0, &published, &args)?;
        let overlay = applied.overlay().map(str::to_string);
        let sliced = slice(applied.regardless(), dimension.trim(), member.trim());

        let completeness = published.completeness;
        check_completeness(&completeness, &args)?;

        batch(
            &published,
            &sliced,
            &measure,
            overlay.as_deref(),
            &completeness,
            false,
        )
    }
}

//! Building a cube from a table the session can already read.
//!
//! # Closing the loop
//!
//! A [`Definition`](sankhya_cube::model::Definition) names its fact table. Until this
//! module existed nothing read it: cells were handed to the catalog by whoever registered
//! the cube, which in practice meant test fixtures. The cube was an algebra with a SQL
//! façade over data somebody else had already shaped.
//!
//! This reads the table the definition names, through the same session that will query the
//! cube, so the cube sees exactly what SQL sees --- the same providers, the same
//! projections, the same snapshot.
//!
//! # What it publishes alongside the cells
//!
//! The [`Completeness`] of the hydration, because that is the only place it can come from.
//! Rows that could not be placed --- a null key, a null measure --- leave no trace in the
//! cells, so a query counting what arrived would report every result complete for ever.
//! Publishing the figure with the data is what makes the `completeness` column on every
//! result row mean something.

use crate::catalog::{CubeCatalog, Published};
use datafusion::common::{plan_datafusion_err, Result};
use datafusion::execution::context::SessionContext;
use sankhya_cube::complete::Completeness;
use sankhya_cube::hydrate::{absorb, empty_for, Absorbed};
use sankhya_cube::model::Cube;
use sankhya_cube_algo::measure::Measure;
use std::sync::Arc;

/// Read a cube's fact table and publish it under `name`.
///
/// # Errors
/// The table cannot be read, or a batch cannot be absorbed --- a dimension's join column
/// absent, or a measure column that is not numeric. Both are refused rather than skipped:
/// a missing dimension column groups every row under one member.
pub async fn publish_from_fact_table(
    context: &SessionContext,
    catalog: &CubeCatalog,
    name: &str,
    cube: Arc<Cube>,
    measure: &Measure,
    snapshot: u64,
) -> Result<Absorbed> {
    let frame = context.table(cube.fact_table()).await?;
    let batches = frame.collect().await?;

    let mut cells = empty_for(&cube);
    let mut absorbed = Absorbed::default();
    for batch in &batches {
        let one = absorb(&cube, measure, batch, &mut cells)
            .map_err(|e| plan_datafusion_err!("hydrating cube '{name}': {e}"))?;
        absorbed = absorbed.and(one);
    }

    catalog.publish(
        name,
        Published {
            cube,
            cells: Arc::new(cells),
            snapshot,
            completeness: absorbed.completeness(),
        },
    );
    Ok(absorbed)
}

/// Publish cells somebody else built, stating how complete they are.
///
/// For a caller that has already shaped the data. The completeness is a parameter and has no
/// default, because [`Completeness::complete`] is a claim rather than an absence of one, and
/// a default would let every cube nobody thought about report itself whole.
pub fn publish_cells(
    catalog: &CubeCatalog,
    name: &str,
    cube: Arc<Cube>,
    cells: sankhya_cube::cells::Cells,
    snapshot: u64,
    completeness: Completeness,
) {
    catalog.publish(
        name,
        Published {
            cube,
            cells: Arc::new(cells),
            snapshot,
            completeness,
        },
    );
}

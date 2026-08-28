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
use futures::StreamExt;
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

    // Streamed, not collected.
    //
    // This read `frame.collect()`, which materialises **the whole fact table** as
    // `RecordBatch`es before absorbing any of it. Decompressed Arrow runs two to four times
    // the Parquet on disk, so a one-gigabyte table was two to four gigabytes resident --- and
    // the soak that first exercised this path showed resident memory at 5.6 GB against 779 MB
    // for the same scale without it.
    //
    // Nothing needed it. `absorb` takes one batch and folds it into `cells`, so the peak is a
    // batch rather than a table. Collecting was the convenient call, and convenience was the
    // whole cost.
    //
    // **No mutation entry claims this.** Streaming and collecting produce identical cells ---
    // the difference is memory, which no unit test measures --- so a catalogue entry would be
    // a claim nothing checks. The soak is what validates it, and the numbers it produced are
    // the reason this changed.
    let mut stream = frame.execute_stream().await?;

    let mut cells = empty_for(&cube);
    let mut absorbed = Absorbed::default();
    while let Some(batch) = stream.next().await {
        let batch = batch?;
        let one = absorb(&cube, measure, &batch, &mut cells)
            .map_err(|e| plan_datafusion_err!("hydrating cube '{name}': {e}"))?;
        absorbed = absorbed.and(one);
    }

    catalog.publish(
        name,
        Published {
            cube,
            cells: Arc::new(cells),
            measure: measure.name.clone(),
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
    measure: &str,
    snapshot: u64,
    completeness: Completeness,
) {
    catalog.publish(
        name,
        Published {
            cube,
            cells: Arc::new(cells),
            measure: measure.to_string(),
            snapshot,
            completeness,
        },
    );
}

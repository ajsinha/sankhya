//! Opening the table a dimension names.
//!
//! # Read through the session, like the facts
//!
//! [`publish_from_fact_table`](crate::publish::publish_from_fact_table) reads the fact table
//! through the session that will query the cube, so the cube sees what SQL sees --- the same
//! providers, the same snapshot, the same row-level policy. A dimension table read any other
//! way would break that in the direction that matters: a principal who cannot see a region's
//! rows would still learn its members, and a referential check against a table they can only
//! partly read would call their own facts orphans.
//!
//! So it is the same `SessionContext`, the same streaming, and the same refusal to collect a
//! whole table into memory before looking at any of it.
//!
//! # A dimension whose table is not there
//!
//! Reported, not ignored. `DIMENSION region FROM sales.regions` is a statement that
//! `sales.regions` exists, and a cube that quietly hydrates without it is a cube whose
//! referential check silently does nothing --- which is worse than no check, because the
//! result rows look the same either way.
//!
//! The one exception is a dimension that declares neither `LEVEL` nor `PARENT`. There is then
//! no column to read and nothing to check, and [`shape_of`] says so.

use datafusion::common::{plan_datafusion_err, Result};
use datafusion::execution::context::SessionContext;
use futures::StreamExt;
use sankhya_cube::members::{absorb_members, shape_of, Members};
use sankhya_cube::model::{Cube, Dimension};
use std::collections::BTreeMap;

/// Read every dimension table a cube names.
///
/// Dimensions that declare no member columns are absent from the result rather than present
/// and empty, so "nothing to read" stays distinguishable from "read, and it had no rows".
///
/// # Errors
/// The dimension table cannot be read, a column the definition names is not in it, or the
/// hierarchy it describes runs through a cycle. All three are refused: a cycle discovered
/// during a query is an unbounded traversal and a timeout that names nothing.
pub async fn read_members(
    context: &SessionContext,
    cube: &Cube,
) -> Result<BTreeMap<String, Members>> {
    let mut read = BTreeMap::new();
    for dimension in cube.dimensions() {
        if shape_of(dimension).is_none() {
            continue;
        }
        read.insert(dimension.name.clone(), one(context, dimension).await?);
    }
    Ok(read)
}

/// One dimension table, streamed.
async fn one(context: &SessionContext, dimension: &Dimension) -> Result<Members> {
    let frame = context.table(&dimension.table).await.map_err(|why| {
        plan_datafusion_err!(
            "dimension '{}' is declared `FROM {}`, which cannot be read: {why}. Refused \
             rather than hydrated without it: a cube that skips its dimension table has a \
             referential check that silently passes everything",
            dimension.name,
            dimension.table
        )
    })?;
    let mut stream = frame.execute_stream().await?;
    let mut members = Members::new();
    while let Some(batch) = stream.next().await {
        let batch = batch?;
        absorb_members(dimension, &batch, &mut members).map_err(|why| {
            plan_datafusion_err!(
                "reading dimension '{}' from '{}': {why}",
                dimension.name,
                dimension.table
            )
        })?;
    }
    members.validate().map_err(|why| {
        plan_datafusion_err!(
            "the hierarchy in '{}' is not one: {why}. Refused at hydration rather than at \
             query time, where a cycle is an unbounded walk and a timeout naming nothing",
            dimension.table
        )
    })?;
    Ok(members)
}

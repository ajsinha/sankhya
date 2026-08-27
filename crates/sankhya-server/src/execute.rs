//! Running a statement, from SQL text to wire rows.
//!
//! This is the piece that makes everything else reachable. Until it existed, the read path,
//! the policy component, the `Guard` and the enforcement above the scan were all built,
//! tested, and unreachable through the front door --- which meant the security properties
//! were provable in unit tests and unprovable end to end. That is a meaningful difference:
//! an unreachable enforcement point is one nobody has confirmed is actually on the path.
//!
//! # The order of operations, and why it is this order
//!
//! 1. **Authorise**, producing a [`Guard`] or nothing. No `Guard`, no table registered.
//! 2. **Wrap** each permitted table in a [`SecuredTable`], which conjoins the policy's row
//!    predicate where no provider can decline it.
//! 3. **Plan and execute** against a session holding only the wrapped tables.
//! 4. **Convert** Arrow batches to the text the wire wants.
//!
//! A table the caller may not read is never registered, so a query naming it fails to
//! resolve rather than being planned and then filtered. That is deliberate: a table that
//! plans and returns nothing is indistinguishable from an empty one, and the difference
//! matters to whoever is reading the result.

use arrow_array::{Array, RecordBatch};
use arrow_schema::{DataType, SchemaRef, TimeUnit};
use datafusion::catalog::TableProvider;
use datafusion::prelude::SessionContext;
use sankhya_api_pg::message::{oid, FieldDescription};
use sankhya_api_pg::session::{QueryFailure, QueryResult};
use sankhya_authz::policy::{Action, PolicySet, TableRef};
use sankhya_authz::principal::Principal;
use sankhya_catalog::guard::Guard;
use sankhya_catalog::secured::SecuredTable;
use sankhya_error::protocol::{statuses_for, sqlstate};
use std::sync::Arc;

/// A table this server can serve, and the provider behind it.
pub struct ServableTable {
    /// Where it lives.
    pub reference: TableRef,
    /// What answers a scan of it.
    pub provider: Arc<dyn TableProvider>,
}

impl std::fmt::Debug for ServableTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServableTable")
            .field("reference", &self.reference)
            .finish_non_exhaustive()
    }
}

/// Build a session containing exactly the tables this principal may read.
///
/// Returns the session and how many tables were registered. A caller that gets zero has a
/// principal who may read nothing, which is worth distinguishing from a query that found
/// no rows.
pub fn session_for(
    principal: &Principal,
    policy: &PolicySet,
    tables: &[ServableTable],
) -> Result<(SessionContext, usize), QueryFailure> {
    let context = SessionContext::new();
    let mut registered = 0usize;

    for table in tables {
        // No guard, no registration. A table the caller may not read is not present in the
        // session at all, so a query naming it fails to resolve rather than planning and
        // then returning nothing — which would be indistinguishable from an empty table.
        let Some(guard) = Guard::authorize(policy, principal, &table.reference, Action::Read)
        else {
            continue;
        };
        let secured = SecuredTable::new(Arc::clone(&table.provider), guard, &context.state())
            .map_err(|error| failure(sqlstate::INTERNAL_ERROR.as_str(), &error.to_string()))?;
        context
            .register_table(table.reference.table.as_str(), Arc::new(secured))
            .map_err(|error| failure(sqlstate::INTERNAL_ERROR.as_str(), &error.to_string()))?;
        registered = registered.saturating_add(1);
    }
    Ok((context, registered))
}

/// Run a statement and render its result for the wire.
///
/// `max_rows` bounds what is materialised. The wire protocol's simple-query flow has no way
/// to say "there are more", so the bound is a hard one and hitting it is reported as an
/// error rather than a truncated result presented as complete.
pub async fn run(
    context: &SessionContext,
    sql: &str,
    max_rows: usize,
) -> Result<QueryResult, QueryFailure> {
    let frame = context
        .sql(sql)
        .await
        .map_err(|error| plan_failure(&error))?;
    let schema = frame.schema().as_arrow().clone();
    let batches = frame
        .collect()
        .await
        .map_err(|error| plan_failure(&error))?;

    let total: usize = batches.iter().map(RecordBatch::num_rows).sum();
    if total > max_rows {
        return Err(failure(
            sqlstate::CONFIGURATION_LIMIT_EXCEEDED.as_str(),
            &format!(
                "this result has {total} rows and the limit is {max_rows}. Refusing rather \
                 than returning the first {max_rows}: a truncated result presented as a \
                 complete one is a wrong answer, and this protocol has no way to say there \
                 are more. Add a LIMIT, or narrow the query"
            ),
        ));
    }

    let fields = describe(&Arc::new(schema));
    let mut rows = Vec::with_capacity(total);
    for batch in &batches {
        for index in 0..batch.num_rows() {
            rows.push(render_row(batch, index));
        }
    }
    Ok(QueryResult {
        tag: format!("SELECT {}", rows.len()),
        fields,
        rows,
    })
}

/// The wire description of a result's columns.
fn describe(schema: &SchemaRef) -> Vec<FieldDescription> {
    schema
        .fields()
        .iter()
        .map(|field| {
            let (type_oid, size) = pg_type(field.data_type());
            FieldDescription::text(field.name(), type_oid, size)
        })
        .collect()
}

/// The PostgreSQL type a client should be told a column is.
///
/// Every OID here is a real one. A client receiving an OID it does not recognise renders
/// the value as an opaque string, so an invented OID turns a number into text with no error
/// raised anywhere --- and the user sees a column that will not sort or aggregate.
fn pg_type(arrow: &DataType) -> (i32, i16) {
    match arrow {
        DataType::Boolean => (oid::BOOL, 1),
        DataType::Int8 | DataType::Int16 | DataType::UInt8 => (oid::INT2, 2),
        DataType::Int32 | DataType::UInt16 => (oid::INT4, 4),
        DataType::Int64 | DataType::UInt32 | DataType::UInt64 => (oid::INT8, 8),
        DataType::Float32 => (oid::FLOAT4, 4),
        DataType::Float64 => (oid::FLOAT8, 8),
        DataType::Decimal128(_, _) | DataType::Decimal256(_, _) => (oid::NUMERIC, -1),
        DataType::Date32 | DataType::Date64 => (oid::DATE, 4),
        DataType::Timestamp(_, Some(_)) => (oid::TIMESTAMPTZ, 8),
        DataType::Timestamp(_, None) => (oid::TIMESTAMP, 8),
        DataType::Binary | DataType::LargeBinary => (oid::BYTEA, -1),
        // Everything else is rendered as text. Honest rather than clever: a client told a
        // value is text treats it as text, which is what it is going to receive.
        _ => (oid::TEXT, -1),
    }
}

/// Render one row as the text values the wire carries.
fn render_row(batch: &RecordBatch, row: usize) -> Vec<Option<String>> {
    (0..batch.num_columns())
        .map(|column| {
            let array = batch.column(column);
            // Null stays null all the way to the wire, where it becomes a length of -1.
            // Rendering it as an empty string here would be the same wrong answer one
            // layer earlier, and one nobody could see.
            if array.is_null(row) {
                return None;
            }
            Some(render_value(array.as_ref(), row))
        })
        .collect()
}

/// Render one value as PostgreSQL would print it.
fn render_value(array: &dyn Array, row: usize) -> String {
    use arrow_array::cast::AsArray;
    use arrow_array::types;

    match array.data_type() {
        DataType::Boolean => {
            // 't' and 'f', not 'true' and 'false'. This is what the protocol's text format
            // uses, and clients parse the single character.
            if array.as_boolean().value(row) {
                "t".to_string()
            } else {
                "f".to_string()
            }
        }
        DataType::Int64 => array.as_primitive::<types::Int64Type>().value(row).to_string(),
        DataType::Int32 => array.as_primitive::<types::Int32Type>().value(row).to_string(),
        DataType::UInt64 => array.as_primitive::<types::UInt64Type>().value(row).to_string(),
        DataType::Float64 => array.as_primitive::<types::Float64Type>().value(row).to_string(),
        DataType::Float32 => array.as_primitive::<types::Float32Type>().value(row).to_string(),
        DataType::Utf8 => array.as_string::<i32>().value(row).to_string(),
        DataType::LargeUtf8 => array.as_string::<i64>().value(row).to_string(),
        DataType::Timestamp(TimeUnit::Microsecond, _) => array
            .as_primitive::<types::TimestampMicrosecondType>()
            .value(row)
            .to_string(),
        // The generic path. Arrow's own display is used rather than a hand-written one,
        // because a hand-written renderer for every type is a long list of places to be
        // subtly wrong about a format nobody checks.
        _ => {
            use datafusion::arrow::util::display::{ArrayFormatter, FormatOptions};
            ArrayFormatter::try_new(array, &FormatOptions::default())
                .map(|formatter| formatter.value(row).to_string())
                .unwrap_or_default()
        }
    }
}

/// A failure in the shape the wire wants.
fn failure(sqlstate: &str, message: &str) -> QueryFailure {
    QueryFailure {
        sqlstate: sqlstate.to_string(),
        message: message.to_string(),
        detail: None,
    }
}

/// Translate a planning or execution error into something a client can act on.
///
/// The distinction that matters: a query naming a table the caller cannot read must not be
/// distinguishable from one naming a table that does not exist. The table was never
/// registered, so DataFusion says "no such table" either way --- and that is the right
/// answer rather than an accident, because saying "you may not read that" would confirm it
/// exists.
fn plan_failure(error: &datafusion::error::DataFusionError) -> QueryFailure {
    let message = error.to_string();
    let state = if message.contains("not found") || message.contains("No table") {
        sqlstate::SYNTAX_ERROR
    } else if message.contains("Schema error") || message.contains("SQL error") {
        sqlstate::SYNTAX_ERROR
    } else {
        statuses_for(sankhya_error::Class::Fatal).sqlstate
    };
    failure(state.as_str(), &message)
}

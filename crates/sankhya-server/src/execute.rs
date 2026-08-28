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
use sankhya_error::protocol::{sqlstate, statuses_for};
use sankhya_error::Classify;
use std::sync::Arc;

/// A table this server can serve, and the provider behind it.
#[derive(Clone)]
pub struct ServableTable {
    /// Where it lives.
    pub reference: TableRef,
    /// Where the filesystem holds it.
    ///
    /// Carried alongside the provider because a provider answers scans and deliberately
    /// says nothing about the shape of what it is scanning. The maintenance gauges need the
    /// file count, and asking the provider for it would be asking the read path to
    /// re-export the storage layout it exists to hide.
    pub root: std::path::PathBuf,
    /// What answers a scan of it.
    pub provider: Arc<dyn TableProvider>,
    /// Its columns, kept so the provider can be resolved again.
    pub schema: Arc<arrow_schema::Schema>,
    /// The log version this provider's file list was read at.
    ///
    /// # Why a provider has to know this
    ///
    /// `resolve` reads the log **once** and the provider holds the resulting file list for
    /// its lifetime. That was safe while a served warehouse did not move --- the server runs
    /// no ingest --- and stopped being safe the day the server began maintaining the
    /// warehouse in-process: compaction replaces files and retirement deletes what it
    /// replaced, so a list captured at boot eventually names files that are gone.
    ///
    /// Retirement's grace period protects a reader that listed *recently*. It cannot protect
    /// one that listed at startup and has been serving from it for hours. So the version is
    /// remembered, and a provider whose table has moved past it is resolved again.
    pub resolved_at: u64,
}

impl ServableTable {
    /// How many files this table currently consists of.
    ///
    /// # Errors
    ///
    /// When the log cannot be read or replayed.
    pub fn live_file_count(&self) -> Result<usize, String> {
        sankhya_table_delta::live_files(&self.root)
            .map(|live| live.files.len())
            .map_err(|error| error.to_string())
    }
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

    // The analytical functions the guide documents in its own sections.
    //
    // `sankhya-olap` was not a dependency of this crate at all, so every vector, matrix,
    // statistic and calculus function a reader was told to type answered `Invalid function`.
    // The guide's own test could not see it: it counted returned rows, and a refused
    // statement returns none --- so a broken example was indistinguishable from one that
    // legitimately matched nothing.
    //
    // Third time today that a whole SQL surface turned out to be unreachable from the thing
    // that serves SQL. The others were `sankhya-maintenance` and `sankhya-cube-sql`.
    sankhya_olap::register_constructors(&context);
    sankhya_olap::register_vector_functions(&context);
    sankhya_olap::register_matrix_functions(&context);

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
    // Planned and executed in two steps, deliberately. `SessionContext::sql` does both: it
    // runs data-definition statements *during planning* and hands back a frame over the
    // empty result, so a check against the returned plan happens after the table has already
    // been created. The refusal below only works from here.
    let plan = context
        .state()
        .create_logical_plan(sql)
        .await
        .map_err(|error| plan_failure(&error))?;
    refuse_if_not_a_read(&plan)?;

    let frame = context
        .execute_logical_plan(plan)
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

/// Refuse anything that is not a read.
///
/// # A success tag for work that did not happen
///
/// This server is a read path over a warehouse. Its session context is DataFusion's, and
/// DataFusion is perfectly willing to execute `CREATE TABLE` against its own in-memory
/// catalogue --- so the statement returned a success tag, the table existed for the rest of
/// that connection, and it was gone the moment the client reconnected. Nothing failed and
/// nothing was written. A user would reasonably conclude their table had been created.
///
/// That is the worst shape a defect can take here: not an error, not a wrong number, but a
/// confirmation of something that did not occur.
///
/// Checked against the **planned logical plan** rather than against the statement's first
/// word. Keyword-sniffing gets `WITH x AS (...) INSERT` wrong, gets comments and leading
/// whitespace wrong, and is a second parser maintained alongside the real one.
fn refuse_if_not_a_read(plan: &datafusion::logical_expr::LogicalPlan) -> Result<(), QueryFailure> {
    use datafusion::logical_expr::LogicalPlan as P;
    let what = match plan {
        P::Ddl(_) => "data definition",
        P::Dml(_) => "data modification",
        P::Copy(_) => "COPY",
        _ => return Ok(()),
    };
    let refusal = sankhya_error::Error::NotSupported(format!(
        "{what} is not served over this connection; this server is a read path over a \
         published warehouse"
    ));
    Err(QueryFailure {
        sqlstate: statuses_for(refusal.class()).sqlstate.as_str().to_string(),
        message: format!("[{}] {}", refusal.code(), refusal),
        // The remediation names the supported route rather than only saying no. A refusal
        // that does not say what to do instead sends somebody looking for a flag to turn it
        // on, and there is no flag: writes go to the transactional store and reach the
        // warehouse through capture, or through the publishing tool for an external table.
        detail: Some(
            "Write to the transactional store and let capture publish it, or publish an \
             external table with `sankhya-publish`. See GUIDE.md §3."
                .to_string(),
        ),
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
        DataType::Int64 => array
            .as_primitive::<types::Int64Type>()
            .value(row)
            .to_string(),
        DataType::Int32 => array
            .as_primitive::<types::Int32Type>()
            .value(row)
            .to_string(),
        DataType::UInt64 => array
            .as_primitive::<types::UInt64Type>()
            .value(row)
            .to_string(),
        DataType::Float64 => array
            .as_primitive::<types::Float64Type>()
            .value(row)
            .to_string(),
        DataType::Float32 => array
            .as_primitive::<types::Float32Type>()
            .value(row)
            .to_string(),
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
///
/// # Every failure that reaches a client carries a code and a remediation
///
/// `M6`'s sixth exit criterion asks that every user-reachable error have documented
/// remediation. An earlier version of this function returned the engine's own message with a
/// SQLSTATE guessed from substrings and nothing else --- so the errors a user actually meets,
/// which are almost all of them, were the ones with no code and nothing to do about them.
/// The catalogue existed and the path a person takes did not go through it.
fn plan_failure(error: &datafusion::error::DataFusionError) -> QueryFailure {
    let classified = classify(error);
    QueryFailure {
        sqlstate: statuses_for(classified.class()).sqlstate.as_str().to_string(),
        // The code first, because it is what a support conversation is conducted in and what
        // a runbook is indexed by.
        message: format!("[{}] {}", classified.code(), error),
        // PostgreSQL renders this as DETAIL, which every client shows.
        detail: Some(classified.remediation().to_string()),
    }
}

/// Which catalogue entry an engine failure is.
///
/// Matched on the error's variant rather than on its text wherever the variant carries the
/// distinction. Substring matching on a message is a mapping that changes silently when a
/// dependency reworks its wording, and the symptom is a client that stops retrying something
/// it should retry.
fn classify(error: &datafusion::error::DataFusionError) -> sankhya_error::Error {
    use datafusion::error::DataFusionError as E;
    let detail = error.to_string();
    match error {
        // Three wrappers, and unwrapping them is not optional. DataFusion 55 wraps a plan
        // error in `Diagnostic` to attach a source span, so matching on `Plan` alone never
        // fires --- "table not found", the single commonest error a user meets, fell through
        // to the catch-all and was reported as an execution failure. It looked plausible,
        // which is why it took running the classifier to notice.
        E::Diagnostic(_, inner) | E::Context(_, inner) => classify(inner),
        E::Shared(inner) => classify(inner),
        // A collection is reported by its first member: several errors with one code is a
        // choice, and the first is the one the others usually follow from.
        E::Collection(errors) => errors
            .first()
            .map_or_else(|| sankhya_error::Error::StatementFailed(detail), classify),
        // A statement that does not parse, does not resolve, or does not type-check. The
        // caller's problem, and the detail says what is wrong with it.
        E::SQL(..) | E::Plan(_) | E::SchemaError(..) => sankhya_error::Error::InvalidQuery(detail),
        E::NotImplemented(_) => sankhya_error::Error::NotSupported(detail),
        E::ResourcesExhausted(_) => sankhya_error::Error::AdmissionRejected(detail),
        // DataFusion's own `Internal` means *its* invariant did not hold, which is exactly
        // what this class is for: fail fast, and page.
        E::Internal(_) => sankhya_error::Error::InvariantViolated(detail),
        E::IoError(_) | E::ObjectStore(_) => sankhya_error::Error::StorageUnavailable(detail),
        E::Execution(_) | E::ArrowError(..) | E::ParquetError(..) => {
            sankhya_error::Error::StatementFailed(detail)
        }
        E::Configuration(_) => sankhya_error::Error::ConfigInvalid(detail),
        // Anything a future version of the engine adds. Reported as a statement failure
        // rather than as an invariant violation, because the alternative is paging somebody
        // for a new error variant.
        _ => sankhya_error::Error::StatementFailed(detail),
    }
}

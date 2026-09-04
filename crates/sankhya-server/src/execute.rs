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
use datafusion::catalog::{SchemaProvider, TableProvider};
use datafusion::catalog::memory::MemorySchemaProvider;
use datafusion::prelude::SessionContext;
use sankhya_api_pg::message::{oid, FieldDescription};
use sankhya_api_pg::session::{QueryFailure, QueryResult};
use sankhya_authz::policy::{Action, PolicySet, TableRef};
use sankhya_authz::principal::Principal;
use sankhya_catalog::guard::Guard;
use sankhya_catalog::secured::SecuredTable;
use sankhya_error::protocol::{sqlstate, statuses_for};
use sankhya_error::Classify;
use std::collections::BTreeMap;
use std::sync::Arc;

/// A table this server can serve, and the provider behind it.
#[derive(Clone)]
pub struct ServableTable {
    /// Where it lives.
    pub reference: TableRef,
    /// The table whose read right this one's derives from, when that is not itself.
    ///
    /// # Why a clone is authorized as something else
    ///
    /// `ADR-0016` makes a clone a **reference** to its origin's files rather than a copy, so
    /// the right to read it *is* the right to read what it references. A clone created a
    /// moment ago has no policy rule of its own, so authorizing it by its own name refuses it
    /// --- a table you can create and cannot read.
    ///
    /// That rule already existed in `Server::readable`, which resolves a clone through its
    /// root before asking the policy. Carrying it here puts the same rule in front of session
    /// registration, so a clone becomes queryable the moment it exists rather than at the next
    /// restart --- which is what it did, and what made cloning unusable.
    pub authorize_as: Option<TableRef>,
    /// What it inherits from the table it was cloned from, if it is a clone.
    ///
    /// Carried so that **re-resolving** a clone keeps the splice. Refreshing it through the
    /// ordinary path would resolve a log that names no files and quietly replace a working
    /// provider with an empty one --- a table that answered correctly until the first time its
    /// origin committed, and silently emptied afterwards.
    pub inherited: Option<sankhya_readpath::Inherited>,
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
    session_and_contested(principal, policy, tables).map(|built| (built.0, built.1))
}

/// [`session_for`], and the bare names it deliberately did not register.
///
/// # Why the caller is told
///
/// A contested bare name registers nowhere, so the planner answers *"table not found"* --- and
/// that is true and useless. A user cannot tell a typo from a name that needs qualifying, and
/// the second is fixed by typing four more characters while the first sends them looking for a
/// table that is right there.
///
/// So the names are returned, and a statement that fails to plan while mentioning one gets a
/// refusal that says which two tables it could have meant. The outcome does not change --- it
/// is still refused --- only whether the person can act on it.
///
/// # Errors
///
/// As [`session_for`].
pub fn session_and_contested(
    principal: &Principal,
    policy: &PolicySet,
    tables: &[ServableTable],
) -> Result<(SessionContext, usize, BTreeMap<String, Vec<String>>), QueryFailure> {
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
    // The wider catalogue: distributions, the special functions, and the decompositions.
    // One call, so a session has all of it or none --- a partially registered set means a
    // query works on one node and fails on another, and the difference is invisible until
    // somebody runs the same statement twice.
    sankhya_functions::register(&context);
    // The catalogue over **everything** this session has, not only the distributions: a
    // `functions()` that listed one crate's would be a catalogue that is wrong about the
    // thing it exists to describe.
    sankhya_functions::describe::register(&context, sankhya_functions::catalogue::everything());

    // The graph functions, against an empty catalogue.
    //
    // This process builds no graph epochs, so every call answers "no graph named that; this
    // session knows none". That is the **truthful** error and it points at the real gap ---
    // nothing hydrates a graph here --- whereas the previous answer, `Invalid function`,
    // pointed at a function the guide documents and implied it did not exist.
    //
    // Registering a surface whose catalogue is empty is not pretending. A cube does the same
    // thing: declared and unhydrated is a state worth being able to report, and collapsing it
    // into "no such name" sends somebody to fix a typo that is not there.
    sankhya_graph_sql::functions::register(
        &context,
        Arc::new(sankhya_graph_sql::catalog::GraphCatalog::new()),
    );

    let mut registered = 0usize;

    // Which bare names more than one table would claim.
    //
    // # Why this is counted before anything is registered
    //
    // Registration used to file every table under its **bare** name and nothing else. Two
    // tables of the same name in different schemas therefore registered twice under one key,
    // and the second silently replaced the first --- so one of them became unreachable, with
    // the catalogue still listing it and nothing said. A client cannot work around a table
    // that is present in `information_schema` and absent from the planner.
    //
    // It also meant `sales.orders` did not resolve at all, because the schema was discarded at
    // the moment of registration. `information_schema.tables` reported it correctly, so the
    // catalogue said the table existed and the planner said it did not.
    // Read rather than assumed to be `public`: it is a session setting, and a constant here
    // would be right until somebody changed it and then wrong in a way nothing would report.
    let default_schema = context
        .state()
        .config()
        .options()
        .catalog
        .default_schema
        .clone();
    let default_schema = default_schema.as_str();

    let mut claims: BTreeMap<&str, usize> = BTreeMap::new();
    for table in tables {
        *claims.entry(table.reference.table.as_str()).or_insert(0) += 1;
    }

    for table in tables {
        // No guard, no registration. A table the caller may not read is not present in the
        // session at all, so a query naming it fails to resolve rather than planning and
        // then returning nothing — which would be indistinguishable from an empty table.
        // Authorized as whatever this table's read right derives from --- itself for an
        // ordinary table, its root for a clone.
        let authority = table.authorize_as.as_ref().unwrap_or(&table.reference);
        let Some(guard) = Guard::authorize(policy, principal, authority, Action::Read) else {
            continue;
        };
        let secured: Arc<dyn TableProvider> = Arc::new(
            SecuredTable::new(Arc::clone(&table.provider), guard, &context.state())
                .map_err(|error| failure(sqlstate::INTERNAL_ERROR.as_str(), &error.to_string()))?,
        );

        // Under its own schema, so `sales.orders` means what it says.
        let schema = table.reference.schema.as_str();
        // A table whose schema *is* the session's default schema is already reachable by both
        // names after one registration, and registering it twice is an error rather than a
        // duplicate --- which is how this first went wrong, on a fixture whose tables live in
        // `public`.
        let in_the_default_schema = schema == default_schema;
        if !schema.is_empty() && !in_the_default_schema {
            schema_provider(&context, schema)?
                .register_table(table.reference.table.to_string(), Arc::clone(&secured))
                .map_err(|error| {
                    failure(sqlstate::INTERNAL_ERROR.as_str(), &error.to_string())
                })?;
        }

        // And under its bare name, when only one table claims it.
        //
        // Not for convenience. Every statement this server has ever answered used the bare
        // name --- it is what a session registered and what the guide's examples type --- and
        // dropping it would break every one of them to fix the qualified case.
        //
        // A **contested** bare name registers nowhere. Refusing to resolve it is the answer
        // that cannot be wrong: a client is told to qualify, rather than being given one of the
        // two tables on a rule nobody wrote down.
        if claims.get(table.reference.table.as_str()).copied().unwrap_or(0) == 1
            || schema.is_empty()
            || in_the_default_schema
        {
            context
                .register_table(table.reference.table.as_str(), Arc::clone(&secured))
                .map_err(|error| {
                    failure(sqlstate::INTERNAL_ERROR.as_str(), &error.to_string())
                })?;
        }
        registered = registered.saturating_add(1);
    }

    // The bare names two or more schemas claim, each with the qualified names it could mean.
    // Built from the same `claims` count the registration decision used, so the two cannot
    // disagree about which names are contested.
    let mut contested: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for table in tables {
        let bare = table.reference.table.as_str();
        if claims.get(bare).copied().unwrap_or(0) > 1 {
            contested
                .entry(bare.to_owned())
                .or_default()
                .push(format!("{}.{bare}", table.reference.schema));
        }
    }
    Ok((context, registered, contested))
}

/// The schema of this name in the session's catalogue, created if it is not there yet.
///
/// # Errors
///
/// [`QueryFailure`] when the session has no default catalogue to put a schema in, which would
/// mean the context was built differently from the one line above that builds it.
fn schema_provider(
    context: &SessionContext,
    name: &str,
) -> Result<Arc<dyn SchemaProvider>, QueryFailure> {
    let catalogue = context.catalog("datafusion").ok_or_else(|| {
        failure(
            sqlstate::INTERNAL_ERROR.as_str(),
            "the session has no default catalogue",
        )
    })?;
    if let Some(existing) = catalogue.schema(name) {
        return Ok(existing);
    }
    let created: Arc<dyn SchemaProvider> = Arc::new(MemorySchemaProvider::new());
    catalogue
        .register_schema(name, Arc::clone(&created))
        .map_err(|error| failure(sqlstate::INTERNAL_ERROR.as_str(), &error.to_string()))?;
    Ok(created)
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
    refuse_if_silently_ignored(sql)?;

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
    // Bounded, and the bound is not decoration.
    //
    // An adversarial review showed one client typing a cheap, arbitrarily expensive statement
    // --- a cross join over two `generate_series` --- and hanging up. The server went on
    // burning a whole core to completion, because nothing in the query path had a deadline:
    // `collect` awaits every batch, and `block_in_place` holds a runtime worker while it does.
    // Enough of those starve every other connection, and the client that started it is gone.
    //
    // `sankhya-governor` has `Budget`, `Deadline` and `Cancel` and **the query path used none
    // of them** --- they are a polling model, and nothing in this path polls. A deadline is
    // what the execution actually admits, so a deadline is what it gets.
    let batches = match tokio::time::timeout(statement_deadline(), frame.collect()).await {
        Ok(collected) => collected.map_err(|error| plan_failure(&error))?,
        Err(_) => {
            return Err(failure(
                // `57014`, query_canceled --- which the review found unreachable, because
                // nothing could cancel a query.
                "57014",
                &format!(
                    "this statement ran for longer than {} seconds and was stopped. It is \
                     stopped rather than left running because a statement nobody is waiting \
                     for still holds a worker, and enough of them starve every other \
                     connection",
                    statement_deadline().as_secs()
                ),
            ));
        }
    };

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
        // `0A000`, feature_not_supported. A driver reads that as "this server will never do
        // that" and stops asking; `42601` reads as "you typed it wrong" and it retries.
        sqlstate: "0A000".to_string(),
        // `Display` for a catalogue error already renders `[code] message`, so prefixing here
        // produced `[SNK-C0006] [SNK-C0006] ...` --- which reads like a bug in the thing
        // reporting the bug.
        message: refusal.to_string(),
        // The remediation names the supported route rather than only saying no. A refusal
        // that does not say what to do instead sends somebody looking for a flag to turn it
        // on, and there is no flag: writes go to the transactional store and reach the
        // warehouse through capture, or through the publishing tool for an external table.
        detail: Some(
            "Write to the transactional store and let capture publish it, or publish an \
             external table with `sankhya-publish`. See GUIDE.md §3."
                .to_string(),
        ),
        subjects: Vec::new(),
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
        // A vector is an array of doubles, and PostgreSQL has had a type for that for
        // decades. Sent as `text` --- which it was --- a client receives the eight characters
        // `[1.0, 2.0]` and has to parse them, and will get it wrong on a null element, on a
        // locale that renders a decimal comma, and on an empty array against a null one.
        //
        // The rendering in `render_row` changes with this and must stay with it: a client
        // told a value is `_float8` will decode PostgreSQL's array syntax, so announcing the
        // OID while sending Arrow's rendering would be worse than sending text.
        DataType::List(item) | DataType::LargeList(item) | DataType::FixedSizeList(item, _)
            if matches!(item.data_type(), DataType::Float64 | DataType::Float32) =>
        {
            (oid::FLOAT8_ARRAY, -1)
        }
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
        // Every timestamp unit, in **PostgreSQL's** text format rather than as a number or as
        // ISO-8601. `CLI-01`.
        //
        // Microseconds are this project's canonical unit and were rendered with
        // `.to_string()` on the raw `i64`, so `psql` printed `1756545242000000` and JDBC and
        // psycopg raised on it. The other three units fell through to Arrow's display, which
        // writes `T` between the date and the time and `Z` for the zone; PostgreSQL writes a
        // space and a numeric offset, and a driver parsing text is parsing *that* grammar.
        //
        // **Zero tests touched a timestamp.**
        DataType::Timestamp(unit, zone) => {
            let raw = match unit {
                TimeUnit::Second => array
                    .as_primitive::<types::TimestampSecondType>()
                    .value(row)
                    .checked_mul(1_000_000),
                TimeUnit::Millisecond => array
                    .as_primitive::<types::TimestampMillisecondType>()
                    .value(row)
                    .checked_mul(1_000),
                TimeUnit::Microsecond => Some(
                    array
                        .as_primitive::<types::TimestampMicrosecondType>()
                        .value(row),
                ),
                // Truncated toward negative infinity rather than toward zero, so a
                // pre-epoch instant does not round the wrong way across the boundary.
                TimeUnit::Nanosecond => Some(
                    array
                        .as_primitive::<types::TimestampNanosecondType>()
                        .value(row)
                        .div_euclid(1_000),
                ),
            };
            raw.map_or_else(String::new, |micros| timestamp_text(micros, zone.is_some()))
        }
        // `\x` first, which is what PostgreSQL's `bytea_output = hex` writes and what every
        // driver strips before decoding. Bare hex is decoded as the *characters*.
        DataType::Binary => format!("\\x{}", hex(array.as_binary::<i32>().value(row))),
        DataType::LargeBinary => format!("\\x{}", hex(array.as_binary::<i64>().value(row))),
        // An array of doubles, in PostgreSQL's own text syntax rather than Arrow's.
        //
        // `{1,2,3}`, not `[1.0, 2.0, 3.0]`. This travels with the `_float8` OID in `pg_type`
        // and neither is correct without the other: a client told a value is an array will
        // decode it as one, and Arrow's brackets are not that syntax.
        DataType::List(item) | DataType::LargeList(item) | DataType::FixedSizeList(item, _)
            if matches!(item.data_type(), DataType::Float64 | DataType::Float32) =>
        {
            render_double_array(array, row)
        }
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

/// Microseconds since the epoch, in PostgreSQL's text format.
///
/// `2026-08-30 09:14:02.000123`, and `+00` on the end when the column is zone-aware. That is
/// the grammar a driver parses: a space rather than a `T`, a numeric offset rather than `Z`,
/// and the fractional part omitted entirely when it is zero — which is what PostgreSQL does
/// and therefore what a round-trip test against it compares against.
///
/// Always UTC. This server has no session `TimeZone` to render in, and inventing one would
/// make the same instant print differently on two machines.
fn timestamp_text(micros: i64, zoned: bool) -> String {
    const DAY: i64 = 86_400 * 1_000_000;
    // Floor division, so an instant before 1970 borrows from the day rather than truncating
    // toward zero and landing a day late with a negative time of day.
    let days = micros.div_euclid(DAY);
    let within = micros.rem_euclid(DAY);
    let Ok(days) = i32::try_from(days) else {
        return String::new();
    };
    let (year, month, day) = sankhya_schema::civil_from_days(days);

    let seconds = within / 1_000_000;
    let fraction = within % 1_000_000;
    let (hour, minute, second) = (seconds / 3_600, (seconds / 60) % 60, seconds % 60);

    let mut out = format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}");
    if fraction != 0 {
        // Trailing zeroes trimmed, as PostgreSQL does: `.5` rather than `.500000`.
        let digits = format!("{fraction:06}");
        out.push('.');
        out.push_str(digits.trim_end_matches('0'));
    }
    if zoned {
        out.push_str("+00");
    }
    out
}

/// Bytes as lower-case hex, for `bytea`'s `\x` form.
fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// One row's array of doubles, in PostgreSQL's array syntax.
///
/// `{1,2,3}`, with a null element written as the bare word `NULL` --- which is how PostgreSQL
/// distinguishes it from the string `"NULL"`, and why an element is not quoted.
fn render_double_array(array: &dyn datafusion::arrow::array::Array, row: usize) -> String {
    use datafusion::arrow::array::AsArray;
    use arrow_array::types;

    let values: Option<datafusion::arrow::array::ArrayRef> = match array.data_type() {
        DataType::List(_) => Some(array.as_list::<i32>().value(row)),
        DataType::LargeList(_) => Some(array.as_list::<i64>().value(row)),
        DataType::FixedSizeList(_, _) => Some(array.as_fixed_size_list().value(row)),
        _ => None,
    };
    let Some(values) = values else {
        return String::new();
    };

    let mut rendered = String::from("{");
    for index in 0..values.len() {
        if index > 0 {
            rendered.push(',');
        }
        if values.is_null(index) {
            rendered.push_str("NULL");
        } else if let Some(doubles) = values.as_primitive_opt::<types::Float64Type>() {
            rendered.push_str(&doubles.value(index).to_string());
        } else if let Some(floats) = values.as_primitive_opt::<types::Float32Type>() {
            rendered.push_str(&floats.value(index).to_string());
        } else {
            rendered.push_str("NULL");
        }
    }
    rendered.push('}');
    rendered
}

/// A failure in the shape the wire wants.
fn failure(sqlstate: &str, message: &str) -> QueryFailure {
    QueryFailure {
        sqlstate: sqlstate.to_string(),
        message: message.to_string(),
        detail: None,
        subjects: Vec::new(),
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
        // The **specific** SQLSTATE where the failure has one, and the class's otherwise.
        //
        // The class alone is not enough, and the way it fails is expensive. Every user-class
        // failure answered `42601`, *syntax_error* --- so a migration tool asking for a table
        // that is not there yet was told its generated SQL was malformed, rather than
        // `42P01`, which is the code every one of them branches on to mean "create it".
        //
        // A driver's behaviour is driven by these five characters and not by the message. Get
        // them wrong and a well-written client does exactly the wrong thing.
        sqlstate: specific_sqlstate(error)
            .unwrap_or_else(|| statuses_for(classified.class()).sqlstate.as_str().to_string()),
        // The code first, because it is what a support conversation is conducted in and what
        // a runbook is indexed by --- but **once**. A refusal raised inside the engine already
        // carries its code in the text it was built with, and prefixing again produced
        // `[SNK-C0006] [SNK-C0006] ...`, which reads like a bug in the thing reporting the bug.
        message: {
            let said = error.to_string();
            let code = classified.code().as_str();
            if said.contains(&format!("[{code}]")) {
                said
            } else {
                format!("[{code}] {said}")
            }
        },
        // PostgreSQL renders this as DETAIL, which every client shows.
        detail: Some(classified.remediation().to_string()),
        // An engine failure cites no names of ours: whatever it names is inside its own
        // message, in its own words. Inventing a list by parsing that message would be the
        // very coupling `subjects` exists to remove.
        subjects: Vec::new(),
    }
}

/// How long one statement may run before it is stopped.
///
/// # Why there is a limit at all
///
/// A statement that outlives the client that asked for it is pure cost: nobody will read the
/// answer, and it holds a runtime worker until it finishes. One is a waste; enough of them are
/// a denial of service that any connected client can cause by typing a short query and hanging
/// up.
///
/// # Why it is generous
///
/// Because a legitimate analytical query over a large warehouse is genuinely slow, and a limit
/// that stops real work is a limit an operator raises to infinity. Half an hour is far beyond
/// any interactive statement and far below "forever".
///
/// `SANKHYA_STATEMENT_TIMEOUT_SECONDS` overrides it. Zero means no limit, which is a real
/// choice for a batch deployment with no untrusted clients --- said explicitly, rather than
/// arrived at by having no limit in the first place.
fn statement_deadline() -> std::time::Duration {
    /// What a deployment gets by not thinking about it.
    const DEFAULT_SECONDS: u64 = 1_800;

    static DEADLINE: std::sync::OnceLock<std::time::Duration> = std::sync::OnceLock::new();
    *DEADLINE.get_or_init(|| {
        let seconds = std::env::var("SANKHYA_STATEMENT_TIMEOUT_SECONDS")
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
            .unwrap_or(DEFAULT_SECONDS);
        if seconds == 0 {
            // Not "stop immediately". A deliberate opt-out, expressed as a deadline nothing
            // reaches, so the code below has one path rather than two.
            std::time::Duration::from_secs(u64::from(u32::MAX))
        } else {
            std::time::Duration::from_secs(seconds)
        }
    })
}

/// Refuse a construct the engine **parses and then ignores**.
///
/// # Why this is the worst failure available
///
/// `SELECT count(*) FROM orders TABLESAMPLE BERNOULLI (1)` asks for one per cent of a table
/// and was answered with all of it, reported as success. Nothing in the result says the
/// sampling did not happen. A caller doing statistical work on a sample gets the population,
/// with a confidence interval computed as though it had a sample --- and no symptom at all.
///
/// The catalogue's own remediation for `NotSupported` states the rule this violates: *"a
/// statement that silently means something slightly different from what it says is worse than
/// one that is rejected."*
///
/// # Why the check is textual
///
/// Because the plan does not carry it. The parser accepts the clause and drops it, so by the
/// time there is a `LogicalPlan` there is nothing left to notice. Matching the statement text
/// is crude, and it is the only place the information still exists.
///
/// Literals are removed first, so a row whose text happens to contain the word is not refused.
fn refuse_if_silently_ignored(sql: &str) -> Result<(), QueryFailure> {
    let structure = without_literals(&sql.to_uppercase());
    for ignored in ["TABLESAMPLE"] {
        if structure.split(|c: char| !c.is_ascii_alphanumeric() && c != '_').any(|word| word == ignored) {
            let refusal = sankhya_error::Error::NotSupported(format!(
                "{ignored} is parsed and then ignored by this engine, so the statement would \
                 be answered over the whole table while appearing to have sampled it. \
                 Refused rather than answered: a statement that silently means something \
                 different from what it says is worse than one that is rejected"
            ));
            return Err(QueryFailure {
                sqlstate: "0A000".to_string(),
                message: refusal.to_string(),
                detail: Some(
                    "Sample explicitly --- a `WHERE` on a hash of a key column gives a \
                     reproducible sample this engine really applies."
                        .to_string(),
                ),
                subjects: Vec::new(),
            });
        }
    }
    Ok(())
}

/// A statement with its single-quoted literals emptied.
///
/// So that a *value* containing a keyword cannot decide how the statement is treated. The same
/// rule the catalogue recogniser learned the hard way: structure and data are different things
/// and must not be matched as one string.
fn without_literals(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut characters = sql.chars().peekable();
    while let Some(character) = characters.next() {
        if character != '\'' {
            out.push(character);
            continue;
        }
        out.push_str("''");
        while let Some(inside) = characters.next() {
            if inside != '\'' {
                continue;
            }
            if characters.peek() == Some(&'\'') {
                characters.next();
                continue;
            }
            break;
        }
    }
    out
}

/// The SQLSTATE a failure has of its own, where its variant names one.
///
/// Matched on the **variant**, not on the message, wherever the variant carries the
/// distinction --- substring matching on a message is a mapping that changes silently when a
/// dependency rewords itself. Where the variant does not distinguish (`Plan` covers both a
/// missing table and a missing column), the message is consulted for the one token that does,
/// and the fallback is the class's own code rather than a guess.
fn specific_sqlstate(error: &datafusion::error::DataFusionError) -> Option<String> {
    use datafusion::error::DataFusionError as E;
    let code = match error {
        // The same three wrappers `classify` unwraps, for the same reason. DataFusion wraps a
        // plan error in `Diagnostic` to attach a source span, so matching on `Plan` alone
        // never fires --- and "table not found", the commonest error anybody meets, kept its
        // generic code while looking correct.
        E::Diagnostic(_, inner) | E::Context(_, inner) => return specific_sqlstate(inner),
        E::Shared(inner) => return specific_sqlstate(inner),
        E::Collection(errors) => return errors.first().and_then(specific_sqlstate),
        // `42703`, undefined_column. A `SchemaError` is about a field.
        E::SchemaError(..) => "42703",
        // `0A000`, feature_not_supported. A driver reads this as "this server will never do
        // that" and stops asking; `42601` reads as "you typed it wrong" and it retries.
        E::NotImplemented(_) => "0A000",
        E::ArrowError(inner, _) => {
            let said = inner.to_string();
            if said.contains("Divide by zero") {
                // `22012`, division_by_zero.
                "22012"
            } else if said.contains("Cast error") || said.contains("Parser error") {
                // `22P02`, invalid_text_representation.
                "22P02"
            } else {
                return None;
            }
        }
        E::Plan(said) | E::Execution(said) => {
            let said = said.to_lowercase();
            if said.contains("not found") && said.contains("table") {
                // `42P01`, undefined_table --- the one a migration tool branches on.
                "42P01"
            } else if said.contains("no field named") || said.contains("column") && said.contains("not found") {
                "42703"
            } else if said.contains("divide by zero") {
                "22012"
            } else if said.contains("cannot cast") || said.contains("cast error") {
                "22P02"
            } else {
                return None;
            }
        }
        _ => return None,
    };
    Some(code.to_string())
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

//! The composition root: where the pieces meet.
//!
//! Every other crate in this workspace is a component with no opinion about how it is
//! assembled. This is the one place that decides --- which policy set is in force, which
//! principal a connection becomes, what the audit chain records, what the quotas are.
//!
//! # Why the wiring is its own module and not the binary
//!
//! Because it needs testing. A binary is hard to test and easy to let drift; a struct with
//! a constructor and a handler implementation can be driven from a test that starts a
//! listener on an ephemeral port. The `main` function below it does nothing except read
//! configuration and call this.
//!
//! # What is deliberately not wired yet
//!
//! There is no query engine behind this. A statement that is not a catalogue query returns
//! a named error saying so, rather than an empty result or a plausible-looking zero.
//! Connecting the read path built in M3 is the next step, and until it happens this server
//! is honest about what it is: a front door that authenticates, enforces policy on what it
//! lists, audits what it did, and says clearly that the room behind it is empty.

use sankhya_api_pg::catalog::CatalogTable;
use sankhya_api_pg::session::{Handler, QueryFailure, QueryResult};
use sankhya_audit::chain::{Chain, Entry, RecordedDecision};
use sankhya_authz::policy::{Action, PolicySet, TableRef};
use sankhya_authz::principal::{Authentication, Principal, Role, TenantId};
use sankhya_error::protocol::{statuses_for_denied, statuses_for_unauthenticated};
use sankhya_governor::quota::{Quota, Quotas};
use sankhya_metrics::catalogue;
use sankhya_metrics::Registry;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use sankhya_catalog::guard::Guard;
use datafusion::prelude::SessionContext;
use crate::execute::{run, session_for, ServableTable};

/// How the server was configured.
#[derive(Clone, Debug)]
pub struct Settings {
    /// Where the wire-protocol front door listens.
    pub listen: String,
    /// The directory holding `<schema>/<table>/` for every table this server serves.
    pub warehouse: std::path::PathBuf,
    /// The published position to read as of.
    ///
    /// Everything up to it is visible and nothing after it is, which is what makes two
    /// tables in one query agree with each other. A running coordinator advances this;
    /// with no ingest in this process it is read once at startup.
    pub read_as_of: sankhya_types::Lsn,
    /// The tenant every connection belongs to, until federated identity is wired in.
    pub tenant: TenantId,
    /// How the warehouse maintains itself, or `None` to leave it alone.
    ///
    /// # Why this is a policy and not an interval
    ///
    /// A tick does two jobs on different cadences --- compaction, and collecting files
    /// nothing refers to --- and how often each runs is a property of the deployment, not a
    /// constant somebody guessed. Carrying the whole policy means adding a third cadence
    /// later is a field on the policy rather than another parallel scalar here, and the
    /// defaults live in one place next to the reasoning for them.
    ///
    /// `None` disables maintenance, which exists for the one honest case: another process is
    /// doing it. Two maintainers on one warehouse are two committers racing for the same
    /// version.
    pub maintenance: Option<sankhya_maintenance::MaintenancePolicy>,
    /// Whether a password is required.
    ///
    /// A setting rather than a constant because a development sandbox needs to run without
    /// one --- and because making it explicit means the log can say which it is, so nobody
    /// discovers by accident that their server is open.
    pub require_password: bool,
    /// Where the metrics endpoint listens, or `None` not to serve one.
    ///
    /// Its own address rather than a path on the wire-protocol port, so it can be bound to
    /// an interface clients cannot reach. Defaulting to loopback rather than to every
    /// interface, because the safe choice should be the one you get by not thinking.
    pub metrics_listen: Option<String>,
}

/// Everything the server owns.
#[derive(Debug)]
pub struct Server {
    settings: Settings,
    policy: PolicySet,
    quotas: Quotas,
    audit: parking_lot::Mutex<Chain>,
    tables: Vec<CatalogTable>,
    /// What actually answers a scan, alongside the catalogue's description of it.
    ///
    /// Separate from `tables` because the two answer different questions: one is what a
    /// schema browser is told, the other is what a query reads. Keeping them together would
    /// invite a table that is described but not readable, or readable but not described.
    servable: parking_lot::RwLock<Vec<ServableTable>>,
    /// The log cache the refresh above reads through.
    ///
    /// Shared with nothing else deliberately: it exists so that checking whether a table has
    /// moved costs a stat rather than a log replay, on a path that now runs per statement.
    log_cache: sankhya_table_delta::LogCache,
    /// The cubes this warehouse declares, validated at startup.
    ///
    /// Read once, here, rather than per query: a definition is a small JSON document, and
    /// re-reading it per statement would make a cube's cost depend on how often it is asked
    /// about. **Validated** here too, because a definition that cannot become a `Cube` is a
    /// deployment problem and belongs in the startup log beside the tables that would not
    /// open --- not in the first query that happens to name it, hours later, reported to
    /// whoever ran that query as though they had done something wrong.
    cubes: Vec<sankhya_cube::model::Cube>,
    /// Cells already hydrated, keyed by everything that makes them an answer.
    ///
    /// Shared across statements, which is the point: `session_for` builds a context per
    /// statement, so hydrating inside it would read the whole fact table on every query. The
    /// cache outlives the session; the *key* --- which includes the scope digest --- is what
    /// keeps that safe.
    hydrated: Arc<sankhya_cube_sql::hydrated::Hydrated>,
    clock: parking_lot::Mutex<i64>,
    /// How many connections are open, so the gauge can be set from either hook.
    ///
    /// A counter rather than reading the gauge back: two connections closing at once would
    /// both read the same value and both write one less than it.
    connections: AtomicUsize,
    /// Everything this process exports.
    ///
    /// Shared rather than owned, because the scrape endpoint reads it from another task.
    metrics: Arc<Registry>,
    /// Runs the async query path from the synchronous handler trait.
    ///
    /// The wire protocol handler is synchronous because the protocol is a conversation of
    /// small messages and making every method async would infect the whole state machine
    /// for one call. This is where the two worlds meet, and doing it in one place is what
    /// keeps the protocol code free of it.
    runtime: tokio::runtime::Handle,
}

/// Whether a statement asks for a cube at all.
///
/// Text, not a parse. The alternative is planning the statement twice --- once to discover
/// whether it mentions a cube function and once to run it --- and a false positive here costs
/// a cache lookup while a false negative costs a query that cannot resolve a cube it named.
fn mentions_a_cube_function(sql: &str) -> bool {
    sql.contains("cube_rollup") || sql.contains("cube_slice")
}

impl Server {
    /// Assemble a server.
    ///
    /// Must be called from inside a Tokio runtime: the synchronous protocol handler needs a
    /// handle to reach the asynchronous query path.
    #[must_use]
    pub fn new(settings: Settings, policy: PolicySet, tables: Vec<CatalogTable>) -> Self {
        Self::with_tables(settings, policy, tables, Vec::new())
    }

    /// Assemble a server that can actually answer queries.
    #[must_use]
    /// Load, validate and adopt the cubes a warehouse declares.
    ///
    /// # Why loudly, and why at startup
    ///
    /// A cube definition that will not parse, or that parses and does not describe a usable
    /// cube, is a deployment problem. Discovered at startup it is one line beside the tables
    /// that would not open, and whoever deployed it is still there. Discovered by the first
    /// query to name the cube, it is an error handed to a user who did nothing wrong, at
    /// whatever hour they happened to ask.
    ///
    /// A broken cube does **not** stop the server. The other cubes and every table are still
    /// servable, and refusing to start would turn one malformed JSON file into an outage.
    ///
    /// Returns a complaint per definition that could not be adopted, in the same shape as the
    /// table complaints the caller already prints.
    #[must_use]
    pub fn adopting_cubes(mut self, warehouse: &std::path::Path) -> (Self, Vec<String>) {
        let mut complaints = Vec::new();
        let definitions = match sankhya_cube::catalogue::load_all(warehouse) {
            Ok(definitions) => definitions,
            Err(error) => {
                complaints.push(error.to_string());
                Vec::new()
            }
        };
        for definition in definitions {
            let name = definition.name.clone();
            match definition.validate() {
                Ok(cube) => self.cubes.push(cube),
                Err(rejections) => {
                    let why: Vec<String> =
                        rejections.iter().map(ToString::to_string).collect();
                    complaints.push(format!("the cube `{name}`: {}", why.join("; ")));
                }
            }
        }
        (self, complaints)
    }

    /// The cubes this server adopted.
    #[must_use]
    pub fn cubes(&self) -> &[sankhya_cube::model::Cube] {
        &self.cubes
    }

    pub fn with_tables(
        settings: Settings,
        policy: PolicySet,
        tables: Vec<CatalogTable>,
        servable: Vec<ServableTable>,
    ) -> Self {
        let mut quotas = Quotas::new();
        quotas.set(settings.tenant, Quota::generous());
        Self {
            settings,
            policy,
            quotas,
            audit: parking_lot::Mutex::new(Chain::new()),
            tables,
            servable: parking_lot::RwLock::new(servable),
            log_cache: sankhya_table_delta::LogCache::new(),
            cubes: Vec::new(),
            hydrated: Arc::new(sankhya_cube_sql::hydrated::Hydrated::default()),
            clock: parking_lot::Mutex::new(0),
            connections: AtomicUsize::new(0),
            metrics: Arc::new(Registry::new()),
            runtime: tokio::runtime::Handle::current(),
        }
    }

    /// What this process exports, for the scrape endpoint.
    #[must_use]
    pub fn metrics(&self) -> Arc<Registry> {
        Arc::clone(&self.metrics)
    }

    /// Record the current live file count of every servable table.
    ///
    /// Called on scrape rather than on a timer, so the gauge is never stale --- a gauge
    /// refreshed on a schedule is wrong for as long as the schedule is slow, and a scrape
    /// arriving between refreshes reads a number from the previous era. The log cache makes
    /// this cheap: nothing has changed unless a commit landed.
    pub fn refresh_table_gauges(&self) {
        for table in self.servable.read().iter() {
            let Ok(files) = table.live_file_count() else {
                // A table that will not replay is the diagnostic's business, not the metrics
                // endpoint's. Recording a zero here would report an empty table.
                continue;
            };
            #[allow(clippy::cast_precision_loss)]
            self.metrics.set(
                &catalogue::TABLE_LIVE_FILES,
                &[("table", &table.reference.to_string())],
                files as f64,
            );
        }
        // The registry's own refusals, exported like everything else. A dashboard that
        // cannot see these cannot tell an incomplete metric from a quiet one.
        let rejections = self.metrics.rejections();
        for (reason, count) in [
            ("value_not_permitted", rejections.value_not_permitted),
            ("label_not_declared", rejections.label_not_declared),
            ("label_missing", rejections.label_missing),
            ("over_cap", rejections.over_cap),
        ] {
            #[allow(clippy::cast_precision_loss)]
            self.metrics.set(
                &catalogue::METRICS_REJECTED_TOTAL,
                &[("reason", reason)],
                count as f64,
            );
        }
    }

    /// How many rows one statement may return over this protocol.
    ///
    /// The simple-query flow has no way to say "there are more", so this is a hard bound
    /// and hitting it is an error rather than a truncated result presented as complete.
    const MAX_RESULT_ROWS: usize = 10_000;

    /// How the server is configured, for a startup log.
    ///
    /// Written so that an insecure configuration looks wrong in a log rather than being
    /// something an operator has to go and check.
    #[must_use]
    pub fn describe(&self) -> String {
        let auth = if self.settings.require_password {
            "password required"
        } else {
            "NO AUTHENTICATION — every connection is accepted"
        };
        // The bound address is printed separately by the caller, which is the only thing
        // that knows it. Repeating the *configured* one here printed ":0" beside the real
        // port, which is worse than saying nothing.
        format!(
            "tenant {}, {auth}, {} policy rule(s), {} table(s) known",
            self.settings.tenant,
            self.policy.len(),
            self.tables.len()
        )
    }

    /// How many tables this server serves.
    #[must_use]
    pub fn table_count(&self) -> usize {
        self.tables.len()
    }

    /// The audit chain's current head, for mirroring somewhere append-only.
    #[must_use]
    pub fn audit_head(&self) -> String {
        self.audit.lock().head().to_string()
    }

    /// How many things have been audited.
    #[must_use]
    pub fn audit_len(&self) -> usize {
        self.audit.lock().len()
    }

    /// Whether the audit chain still verifies.
    #[must_use]
    pub fn audit_intact(&self) -> bool {
        self.audit.lock().verify().is_ok()
    }

    /// The quota in force for the configured tenant.
    #[must_use]
    pub fn quota(&self) -> Option<Quota> {
        self.quotas.quota_for(&self.settings.tenant)
    }

    /// The principal a connection becomes.
    ///
    /// One principal per connection, established here and nowhere else. Every connection
    /// currently receives the same roles; federated identity replaces this function and
    /// nothing downstream changes, which is the point of having established the type first.
    fn principal(&self, user: &str) -> Option<Principal> {
        Principal::authenticated(
            user,
            self.settings.tenant,
            [Role::new("reader")],
            if self.settings.require_password {
                Authentication::Password
            } else {
                Authentication::Internal
            },
        )
        .ok()
    }

    /// Record something in the audit chain.
    ///
    /// The clock advances by one per record rather than being read from the system. A
    /// component that reads a clock cannot be replayed, and the audit is the one thing that
    /// must reproduce exactly. A real deployment supplies wall-clock time here.
    fn record(&self, principal: &Principal, table: TableRef, action: Action, allowed: bool) {
        let at = {
            let mut clock = self.clock.lock();
            *clock += 1;
            *clock
        };
        let decision = if allowed {
            RecordedDecision::allowed(None, &std::collections::BTreeMap::new())
        } else {
            RecordedDecision::denied()
        };
        self.audit
            .lock()
            .append(Entry::by(principal, table, action, decision, at));
        // Counted here rather than derived from the chain's length on scrape, so that the
        // number rises at the moment of the append. A gauge read from the chain would be
        // equally true and would not distinguish "the audit stopped recording" from "the
        // scrape stopped running", and only one of those is an emergency.
        self.metrics
            .increment(&catalogue::AUDIT_RECORDS_TOTAL, &[], 1.0);
    }
}

impl Handler for Server {
    fn requires_password(&self, _parameters: &[(String, String)]) -> bool {
        self.settings.require_password
    }

    fn authenticate(
        &self,
        parameters: &[(String, String)],
        password: Option<&[u8]>,
    ) -> Result<(), QueryFailure> {
        let user = parameters
            .iter()
            .find(|(key, _)| key == "user")
            .map(|(_, value)| value.as_str())
            .unwrap_or_default();

        // A connection with no user is refused rather than given a default. An
        // unattributable connection cannot be audited, and an audit that cannot name who
        // acted is not an audit.
        if user.is_empty() {
            return Err(refusal(
                statuses_for_unauthenticated().sqlstate.as_str(),
                "no user was supplied; an unattributable connection cannot be audited",
            ));
        }
        if self.settings.require_password && password.is_none_or(<[u8]>::is_empty) {
            return Err(refusal(
                statuses_for_unauthenticated().sqlstate.as_str(),
                "a password is required",
            ));
        }
        Ok(())
    }

    fn query(&self, sql: &str) -> Result<QueryResult, QueryFailure> {
        let started = std::time::Instant::now();
        let outcome = self.run_statement(sql);

        // Recorded on every path out, including the refusals above the query path. A
        // duration histogram that only sees successes describes a system that never fails,
        // and the tail an operator is looking for is made of failures.
        let label = outcome_label(&outcome);
        self.metrics
            .increment(&catalogue::QUERIES_TOTAL, &[("outcome", label)], 1.0);
        self.metrics.observe(
            &catalogue::QUERY_DURATION_SECONDS,
            &[("outcome", label)],
            started.elapsed().as_secs_f64(),
        );
        if let Ok(result) = &outcome {
            #[allow(clippy::cast_precision_loss)]
            self.metrics.increment(
                &catalogue::ROWS_RETURNED_TOTAL,
                &[],
                result.rows.len() as f64,
            );
        }
        outcome
    }

    fn connection_opened(&self) {
        let live = self.connections.fetch_add(1, Ordering::Relaxed) + 1;
        #[allow(clippy::cast_precision_loss)]
        self.metrics
            .set(&catalogue::CONNECTIONS_ACTIVE, &[], live as f64);
    }

    fn connection_closed(&self) {
        let live = self.connections.fetch_sub(1, Ordering::Relaxed).saturating_sub(1);
        #[allow(clippy::cast_precision_loss)]
        self.metrics
            .set(&catalogue::CONNECTIONS_ACTIVE, &[], live as f64);
    }

    fn visible_tables(&self) -> Vec<CatalogTable> {
        self.list_visible_tables()
    }

    fn server_version(&self) -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }
}

impl Server {
    /// Hydrate and register the cubes a statement asks about.
    ///
    /// # Why the statement is scanned for names
    ///
    /// A `SessionContext` is built per statement, so hydrating every cube for every query
    /// would read every fact table on every query --- the exact cost a cube exists to avoid.
    /// A table function cannot hydrate on demand either: `TableFunctionImpl::call` is
    /// synchronous and reading a table is not.
    ///
    /// So the statement's text is scanned, which is deliberately crude and deliberately
    /// conservative: a false positive costs one cache lookup, and a false negative is a query
    /// that fails to resolve a cube rather than one that answers wrongly. It is replaced by
    /// planning against a registered catalogue when the surface grows a resolver of its own.
    fn register_cubes(&self, context: &SessionContext, principal: &Principal, sql: &str) {
        if self.cubes.is_empty() {
            return;
        }
        // Describing a cube reads no data, so it is registered whatever the statement says.
        // Hydration is the expensive half and only that is gated on the statement naming a
        // navigation function --- a client listing cubes must not pay for reading one.
        let navigating = mentions_a_cube_function(sql);
        let catalog = Arc::new(sankhya_cube_sql::catalog::CubeCatalog::new());
        for cube in &self.cubes {
            catalog.declare(cube.name());
            if !navigating || !sql.contains(cube.name()) {
                continue;
            }
            // The scope this principal reads the fact table under.
            //
            // No guard means no access, and the cube is simply not registered --- the same
            // answer a table gets, for the same reason: saying "you may not read that"
            // confirms it exists.
            //
            // This is defence in depth rather than the load-bearing check, and saying so
            // matters. A principal who may not read the fact table has no `SecuredTable` for
            // it in this session either, so hydration would fail regardless; removing this
            // guard leaves the system correct and merely wasteful. A mutation test confirmed
            // exactly that by surviving its removal, which is why there is no catalogue entry
            // claiming otherwise.
            let Some(scope) = self.scope_for(principal, cube.fact_table()) else {
                continue;
            };
            // The table's *current* version, not the configured `read_as_of`.
            //
            // `read_as_of` defaults to `u64::MAX` --- "everything published" --- and is read
            // once at startup, so keying the cache on it makes the snapshot a constant for
            // the life of the process. The cache would then hold the first hydration forever
            // and serve it after every subsequent commit.
            //
            // `Hydrated` treats a new snapshot as a miss and has a test saying so. That
            // property is worth nothing if the caller passes something that never changes,
            // which is the shape of defect this warehouse keeps finding: a guard that is
            // correct and never reached.
            let snapshot = self.snapshot_of(cube.fact_table());
            for measure in cube.measures() {
                let key = sankhya_cube_sql::hydrated::Key {
                    cube: cube.name().to_string(),
                    measure: measure.name.clone(),
                    definition_version: cube.version(),
                    snapshot,
                    scope,
                };
                if let Some(held) = self.hydrated.get(&key) {
                    catalog.publish(cube.name(), held);
                    continue;
                }
                let hydrated = tokio::task::block_in_place(|| {
                    self.runtime.block_on(sankhya_cube_sql::publish::publish_from_fact_table(
                        context,
                        &catalog,
                        cube.name(),
                        Arc::new(cube.clone()),
                        measure,
                        snapshot,
                    ))
                });
                // A cube that will not hydrate is left unpublished rather than reported here.
                // The query naming it gets `MeasureNotPublished`, which says what happened
                // and what to do; failing the whole statement would take down a query that
                // also names three tables that are perfectly fine.
                if hydrated.is_ok() {
                    if let Ok(published) = catalog.resolve(cube.name(), &measure.name) {
                        self.hydrated.put(key, published);
                    }
                }
            }
        }
        sankhya_cube_sql::functions::register(context, Arc::clone(&catalog));
        // Description alongside navigation, always. A surface a client can use only by
        // already knowing the model is a surface only its author can use, and a picker that
        // hardcodes a cube's dimensions is a picker that drifts from the cube.
        sankhya_cube_sql::describe::register(context, Arc::new(self.cubes.clone()), catalog);
    }

    /// Where a materialised cuboid lives under this warehouse.
    ///
    /// Under `_cubes`, which discovery skips: a materialised cuboid is a published table on
    /// purpose --- readable by anything that reads a table, because the open-storage
    /// commitment gets no exception for the fast path --- and must still not appear in a
    /// catalogue somebody browses, where its name is a hash.
    fn cuboid_root(&self, key: &sankhya_cube::materialise::Key, cube: &str) -> std::path::PathBuf {
        self.settings
            .warehouse
            .join("_cubes")
            .join(key.table(cube))
    }

    /// Cells for this key, if a previous run left them on disk.
    ///
    /// The reason for materialising at all: the in-memory cache dies with the process, and a
    /// server that restarts should not make every dashboard pay for a fact-table read again.
    fn materialised(
        &self,
        key: &sankhya_cube::materialise::Key,
        cube: &sankhya_cube::model::Cube,
        measure: &sankhya_cube::algo::Measure,
    ) -> Option<sankhya_cube::cells::Cells> {
        let root = self.cuboid_root(key, cube.name());
        if !root.join("_delta_log").is_dir() {
            return None;
        }
        let dimensions: Vec<String> =
            cube.dimensions().iter().map(|d| d.name.clone()).collect();
        let schema = sankhya_cube::store::schema_for(&dimensions);
        let table = sankhya_readpath::resolve(
            schema,
            &root,
            sankhya_types::LsnRange::new(sankhya_types::Lsn::new(0), self.settings.read_as_of),
            None,
            self.settings.read_as_of,
        )
        .ok()?;

        let context = SessionContext::new();
        context.register_table("cuboid", Arc::new(table)).ok()?;
        let batches = tokio::task::block_in_place(|| {
            self.runtime.block_on(async {
                context.sql("SELECT * FROM cuboid").await.ok()?.collect().await.ok()
            })
        })?;

        // The rule along the *first* dimension. A cuboid stores one measure's cells and the
        // reduction that produced them, and every dimension of a stored cuboid shares the
        // grain --- so any declared rule reads it back identically. Named rather than
        // defaulted because a wrong rule here would round a stored expansion under the wrong
        // operation.
        let rule = cube
            .dimensions()
            .first()
            .and_then(|d| measure.rule(&d.name))?;
        let mut cells = sankhya_cube::cells::Cells::over(dimensions.clone());
        for batch in &batches {
            let read = sankhya_cube::store::from_batch(batch, &dimensions, rule).ok()?;
            for address in read.addresses() {
                let contributions = read.contributions(address)?;
                cells
                    .add_reduced(address.clone(), rule, contributions.exact_sum())
                    .ok()?;
            }
        }
        Some(cells)
    }

    /// The version a table's log currently stands at.
    ///
    /// Zero when the table cannot be found or its log cannot be read. Zero rather than
    /// `u64::MAX`: an unreadable log must not look like a snapshot that will never move, or
    /// a cube over it would be cached once and never refreshed. A wrong-but-low snapshot
    /// causes a rehydration; a wrong-but-constant one causes a stale answer.
    fn snapshot_of(&self, table: &str) -> u64 {
        self.servable
            .read()
            .iter()
            .find(|servable| servable.reference.table == table)
            .and_then(|servable| sankhya_table_delta::live_files(&servable.root).ok())
            .and_then(|live| live.version)
            .unwrap_or(0)
    }

    /// What this principal may see of a table, as a value.
    ///
    /// `None` when they may not read it at all.
    fn scope_for(&self, principal: &Principal, table: &str) -> Option<u64> {
        let reference = self
            .servable
            .read()
            .iter()
            .find(|servable| servable.reference.table == table)
            .map(|servable| servable.reference.clone())
            .unwrap_or_else(|| TableRef::new("", table));
        Guard::authorize(&self.policy, principal, &reference, Action::Read)
            .map(|guard| guard.scope_digest())
    }

    /// Everything `query` does, without the measuring.
    ///
    /// Split out so that the counter and the histogram are recorded on **every** path out of
    /// the statement, including the two refusals that never reach the query path. A
    /// duration histogram fed only by the successful path describes a system that never
    /// fails, and the tail an operator goes looking for is made of failures.
    fn run_statement(&self, sql: &str) -> Result<QueryResult, QueryFailure> {
        // Admission first. A statement refused for quota must not reach anything else, and
        // must be distinguishable from one refused for permission — the client's correct
        // response differs.
        if let Err(refused) = self.quotas.admit(
            &self.settings.tenant,
            &sankhya_governor::quota::Request::default(),
        ) {
            return Err(QueryFailure {
                sqlstate: "53400".to_string(),
                message: refused.to_string(),
                detail: None,
            });
        }

        let Some(principal) = self.principal("query") else {
            return Err(refusal(
                statuses_for_unauthenticated().sqlstate.as_str(),
                "no principal is established for this connection",
            ));
        };

        // Only the tables this principal may read are registered, so a query naming one
        // they may not fails to resolve — indistinguishable from naming one that does not
        // exist, which is the right answer rather than an accident. Saying "you may not
        // read that" would confirm it exists.
        // Any table whose log has moved is resolved again before it is registered.
        //
        // The server maintains the warehouse in-process now, so the warehouse moves whether
        // or not anybody writes to it: compaction replaces files and retirement deletes what
        // it replaced. A provider fixed at boot then names files that are gone, and the query
        // fails on a path nobody asked about.
        //
        // Checked per statement, through the log cache, so an unmoved table costs a stat.
        {
            let mut servable = self.servable.write();
            crate::warehouse::refresh(&mut servable, self.settings.read_as_of, &self.log_cache);
        }
        let servable = self.servable.read().clone();
        let (context, registered) = session_for(&principal, &self.policy, &servable)?;
        if registered == 0 && !servable.is_empty() {
            return Err(refusal(
                statuses_for_denied().sqlstate.as_str(),
                "this principal may not read any table",
            ));
        }

        // Cubes, if this statement asks for one.
        //
        // Hydration reads the fact table *through this session*, so the cells it builds are
        // filtered by the same `SecuredTable` that filters a plain SELECT. That is the whole
        // authorization story for cubes: there is no second implementation of the rule, and
        // therefore no second implementation to disagree with the first.
        self.register_cubes(&context, &principal, sql);

        // `block_in_place` rather than a bare `block_on`. This method is called from inside
        // a Tokio task — the connection's — and blocking that thread directly panics,
        // because the thread is driving the runtime. `block_in_place` hands the runtime's
        // work to another worker first, which is why the server needs a multi-threaded
        // runtime and would deadlock on a current-thread one.
        let outcome = tokio::task::block_in_place(|| {
            self.runtime
                .block_on(run(&context, sql, Self::MAX_RESULT_ROWS))
        });

        // Audited whichever way it went. A log that records only successes cannot show an
        // attempt to reach something forbidden, which is the pattern an investigation is
        // usually looking for.
        self.record(
            &principal,
            TableRef::new("", statement_shape(sql)),
            Action::Read,
            outcome.is_ok(),
        );
        outcome
    }

    /// Everything `visible_tables` does.
    fn list_visible_tables(&self) -> Vec<CatalogTable> {
        // Filtered by policy, because a catalogue that listed tables the caller cannot read
        // would disclose their existence — the leak the policy component refuses to permit
        // anywhere else, arriving through the back door of a schema browser.
        let Some(principal) = self.principal("catalogue") else {
            return Vec::new();
        };
        let visible = self.policy.visible_tables(&principal);
        self.record(
            &principal,
            TableRef::new("information_schema", "tables"),
            Action::Read,
            true,
        );
        self.tables
            .iter()
            .filter(|table| visible.contains(&TableRef::new(&table.schema, &table.name)))
            .cloned()
            .collect()
    }
}

/// Which outcome label a result carries.
///
/// The distinction between `refused` and `error` is the one worth keeping: a refusal is the
/// system working — a quota held, a permission enforced — and an error is not. Counting them
/// together makes a healthy system under load look like a broken one.
pub(crate) fn outcome_label(outcome: &Result<QueryResult, QueryFailure>) -> &'static str {
    match outcome {
        Ok(_) => "ok",
        Err(failure) => match failure.sqlstate.as_str() {
            // Class 53 is insufficient resources, 28 invalid authorization, 42501
            // insufficient privilege. All three are the system doing its job.
            state if state.starts_with("53") || state.starts_with("28") => "refused",
            "42501" => "refused",
            "57014" => "cancelled",
            _ => "error",
        },
    }
}

/// A short, non-identifying description of a statement, for the audit record.
///
/// The first word or two, not the statement itself. A statement can contain the values a
/// query was filtering on, and copying those verbatim into a durable log turns the audit
/// into a second place the data lives — one with different retention and different access
/// control from the table it came from.
fn statement_shape(sql: &str) -> String {
    sql.split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// A refusal in the shape the wire wants.
fn refusal(sqlstate: &str, message: &str) -> QueryFailure {
    QueryFailure {
        sqlstate: sqlstate.to_string(),
        message: message.to_string(),
        detail: None,
    }
}

/// A policy granting a reader access to everything in `tables`.
#[must_use]
pub fn permissive_policy(tenant: &TenantId, tables: &[CatalogTable]) -> PolicySet {
    tables.iter().fold(PolicySet::new(), |policy, table| {
        policy.with(sankhya_authz::policy::Rule::grant(
            *tenant,
            Role::new("reader"),
            TableRef::new(&table.schema, &table.name),
            Action::Read,
        ))
    })
}

/// Build a server and its listener, ready to serve.
///
/// Reads the warehouse once, at startup. Every table it finds is opened at the configured
/// position and described for the catalogue from its own log, so a schema browser and a
/// query see the same table.
pub async fn start(
    settings: Settings,
) -> std::io::Result<(
    Arc<Server>,
    sankhya_api_pg::listener::PgListener,
    Vec<String>,
)> {
    let (found, unopenable) = crate::warehouse::discover(&settings.warehouse);
    let cache = sankhya_table_delta::LogCache::new();
    let (servable, unreadable) = crate::warehouse::servable(&found, settings.read_as_of, &cache);

    // A table that could not be opened is reported rather than omitted. A server that
    // starts with three tables of four and says nothing has produced an outage that looks,
    // to whoever queries it, like a table that was never created.
    let complaints: Vec<String> = unopenable
        .into_iter()
        .chain(unreadable)
        .map(|(path, reason)| format!("{}: {reason}", path.display()))
        .collect();

    let tables = crate::warehouse::describe(&found);
    let policy = permissive_policy(&settings.tenant, &tables);
    let listener = sankhya_api_pg::listener::PgListener::bind(&settings.listen).await?;
    let warehouse = settings.warehouse.clone();
    let (server, cube_complaints) =
        Server::with_tables(settings, policy, tables, servable).adopting_cubes(&warehouse);
    // Cube complaints join the table ones rather than getting a channel of their own. They
    // are the same kind of news --- something in this warehouse could not be served --- and
    // an operator scanning startup output should not have to know there are two lists.
    let complaints: Vec<String> = complaints.into_iter().chain(cube_complaints).collect();
    Ok((Arc::new(server), listener, complaints))
}

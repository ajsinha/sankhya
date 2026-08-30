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
    /// Where Arrow Flight SQL listens, or `None` not to serve it.
    ///
    /// Its own port rather than a path on the wire-protocol one: Flight is a different
    /// protocol spoken by different clients, and an operator who wants one and not the other
    /// should not have to reason about which requests reach which handler.
    pub flight_listen: Option<String>,
    /// The rows greedy selection may spend on materialised cuboids, per cube.
    ///
    /// The **configuration** level of §11.6's three controls, and the operator's. It is an
    /// operator's storage being spent on their behalf by a selection reading somebody else's
    /// query log, so it is bounded by a number they set rather than by what the lattice
    /// happens to contain --- which is exponential in the dimension count and would be a
    /// budget in name only.
    ///
    /// Deliberately unreachable from a session. A caller who could raise it would be granting
    /// themselves storage, which is a resource exhaustion with a polite interface.
    pub cuboid_budget_rows: u64,
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
    /// The tables that can be served, as a whole list replaced at once.
    ///
    /// **An `Arc` the readers clone, not a `Vec` they borrow.** Every statement used to take
    /// this lock for *writing* and call `warehouse::refresh` inside it --- one filesystem probe
    /// per table --- so every statement serialized against every other, and a write lock
    /// excludes readers as well as writers.
    ///
    /// The probing now happens outside the lock and against a snapshot; the lock is taken only
    /// to publish a replacement list, and only when something actually moved. A reader holds it
    /// for the length of an `Arc::clone`.
    servable: parking_lot::RwLock<Arc<Vec<ServableTable>>>,
    /// Which readers are inside the warehouse right now.
    ///
    /// Shared with the maintenance thread, which is the entire point: a registry the sweeper
    /// cannot see protects nothing. A statement pins it for as long as it runs, so a file that
    /// stops being referenced while a query is in flight is not deleted until that query has
    /// finished --- rather than after a number of ticks chosen to be probably long enough.
    leases: Arc<sankhya_leases::Leases>,
    /// The log cache the refresh above reads through.
    ///
    /// Shared with nothing else deliberately: it exists so that checking whether a table has
    /// moved costs a stat rather than a log replay, on a path that now runs per statement.
    log_cache: sankhya_table_delta::LogCache,
    /// The cubes this warehouse declares, validated when they are adopted.
    ///
    /// Read at startup rather than per query: a definition is a small JSON document, and
    /// re-reading it per statement would make a cube's cost depend on how often it is asked
    /// about. **Validated** there too, because a definition that cannot become a `Cube` is a
    /// deployment problem and belongs in the startup log beside the tables that would not
    /// open --- not in the first query that happens to name it, hours later, reported to
    /// whoever ran that query as though they had done something wrong.
    ///
    /// # Why this is behind a lock, and why an `Arc` inside it
    ///
    /// `CREATE CUBE` and `DROP CUBE` change this set while the server is serving, so it can no
    /// longer be a plain field read by an `&self`. The lock is the smaller half of the choice.
    ///
    /// The `Arc` is the half that matters: a reader takes the lock, clones one pointer, and
    /// drops it. **Nothing is held across a hydration**, which is the whole discipline
    /// [ADR-0013](../../../docs/adr/0013-concurrency-and-data-safety.md) was written to
    /// establish --- hydrating a cube reads a fact table, and a lock spanning that would make
    /// every cube query wait behind every other one. Cloning the `Vec` instead would be
    /// correct and would copy every definition on every statement, which is a cost that grows
    /// with how many cubes a warehouse has rather than with what the statement asked for.
    ///
    /// Writers are DDL and therefore rare; readers are every statement. That asymmetry is why
    /// this is an `RwLock` and not a `Mutex`.
    cubes: std::sync::RwLock<Arc<Vec<sankhya_cube::model::Cube>>>,
    /// Cells already hydrated, keyed by everything that makes them an answer.
    ///
    /// Shared across statements, which is the point: `session_for` builds a context per
    /// statement, so hydrating inside it would read the whole fact table on every query. The
    /// cache outlives the session; the *key* --- which includes the scope digest --- is what
    /// keeps that safe.
    hydrated: Arc<sankhya_cube_sql::hydrated::Hydrated>,
    /// What people have asked each cube for.
    ///
    /// Selection has been implemented and untestable since M7 began, because nothing recorded
    /// the signal it reads. Bounded per cube, and it records a *shape* --- which dimensions
    /// were grouped by --- with nowhere to put a member or a principal.
    query_log: Arc<sankhya_cube::querylog::QueryLog>,
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

/// Roll base-grain cells to the grain a cuboid names.
///
/// `None` when a dimension cannot be rolled away --- the measure does not compose along it ---
/// which is a shape that must not be materialised rather than one to store approximately.
fn roll_to(
    cells: &sankhya_cube::cells::Cells,
    shape: &sankhya_cube::algo::Cuboid,
    measure: &sankhya_cube::algo::Measure,
) -> Option<sankhya_cube::cells::Cells> {
    let keep: Vec<&str> = shape.dimensions();
    let dropping: Vec<String> = cells
        .dimensions()
        .iter()
        .filter(|name| !keep.contains(&name.as_str()))
        .cloned()
        .collect();
    let mut out = cells.clone();
    for dimension in dropping {
        out = sankhya_cube::navigate::roll_up(
            &out,
            &dimension,
            measure,
            sankhya_cube::navigate::Ordered::Unstated,
        )
        .ok()?;
    }
    Some(out)
}

/// The default rows selection may spend per cube, when an operator states nothing.
pub const CUBOID_ROW_BUDGET: u64 = 10_000_000;

/// What a cuboid costs, when nothing better is known.
///
/// # Why this is not uniform, which was the first attempt
///
/// Counting every cuboid the same makes selection a **no-op**, and not obviously: a cuboid is
/// chosen for the rows it *saves*, and if every cuboid costs the same then answering from a
/// coarser one saves nothing, so nothing is ever worth holding. The first version of this
/// returned a constant and carried a comment claiming it "still selects usefully". It selects
/// nothing, and a test asking for one shape five times and finding it unmaterialised is what
/// said so.
///
/// So cost is monotone in width: a cuboid over fewer dimensions holds fewer distinct member
/// combinations. `ASSUMED_MEMBERS` per dimension is an estimate and is stated as one --- the
/// real figure is the distinct combinations actually present, which nothing here has measured.
/// What matters for selection is not the absolute number but that dropping a dimension makes a
/// cuboid cheaper, and that is true of the data whatever the constant is.
///
/// Replaced when cardinality is recorded rather than assumed; until then this is a shape that
/// ranks correctly rather than a number anybody should read.
struct EstimatedCost;

/// Distinct members assumed per dimension, for want of a measurement.
const ASSUMED_MEMBERS: u64 = 100;

impl sankhya_cube::algo::Cost for EstimatedCost {
    fn rows(&self, cuboid: &sankhya_cube::algo::Cuboid) -> u64 {
        ASSUMED_MEMBERS.saturating_pow(u32::try_from(cuboid.width()).unwrap_or(u32::MAX))
    }
}

/// How many versions of drift a superseded cuboid is allowed before it is removed.
///
/// **Not** a `target_lag`. That decides what may be *served* and is a per-cube setting; this
/// decides what may be *deleted* and must be strictly more generous, because a query that
/// resolved a cuboid a moment ago is still reading it and a file deleted from under a running
/// scan fails naming a path the caller never mentioned.
///
/// A hundred versions is far beyond any query's lifetime and still collects a cuboid within
/// minutes on a table under continuous ingest. The cost of it being too large is storage; the
/// cost of it being too small is a query that fails.
const CUBOID_DRIFT_TOLERATED: u64 = 100;

/// Whether a statement asks for a cube at all.
///
/// Text, not a parse. The alternative is planning the statement twice --- once to discover
/// whether it mentions a cube function and once to run it --- and a false positive here costs
/// a cache lookup while a false negative costs a query that cannot resolve a cube it named.
fn mentions_a_cube_function(sql: &str) -> bool {
    sql.contains("cube_rollup") || sql.contains("cube_slice")
}

/// The finest grain this statement needs from a cube.
///
/// # Why a coarser cuboid cannot just be handed over
///
/// Cells published to a session are the finest grain a query may reach: the SQL surface dices
/// and then rolls up **from them**. So publishing a cuboid coarser than the query needs would
/// answer a fine question from cells that cannot express it --- silently, because rolling up
/// something already rolled up produces a number rather than an error.
///
/// The requirement is therefore the union of two things:
///
/// - every dimension grouped by (`by=region|period`), because the result names them; and
/// - every dimension a dice restricts (`where=region:north`), because `narrowed` must find
///   that column to restrict it.
///
/// A union across *all* cube calls in the statement, which is the safe combination: a superset
/// of what each one needs is still enough for each one. Two roll-ups of the same cube at
/// different grains get cells fine enough for both.
///
/// Read from the statement text, like `mentions_a_cube_function`, and with the same caveat:
/// the decision is needed before the table function runs, so there is nothing better to read
/// yet. Erring wide is safe --- naming a dimension the query does not need only means a
/// coarser cuboid is passed over and a finer one used, which costs a scan and not an answer.
fn grain_needed(sql: &str) -> Vec<String> {
    let mut wanted: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for (option, take_dimension) in [("by=", false), ("where=", true)] {
        let mut rest = sql;
        while let Some(at) = rest.find(option) {
            let after = &rest[at + option.len()..];
            // An option ends at the next separator, or at the end of the literal.
            let end = after
                .find(|c| c == ',' || c == '\'' || c == '"' || c == ')')
                .unwrap_or(after.len());
            for part in after[..end].split('|') {
                let name = if take_dimension {
                    part.split(':').next().unwrap_or(part)
                } else {
                    part
                };
                let name = name.trim();
                if !name.is_empty() {
                    wanted.insert(name.to_string());
                }
            }
            rest = &after[end..];
        }
    }
    wanted.into_iter().collect()
}

/// What this statement asks of materialisation.
///
/// §11.6's **session** level, and the one that only goes one way. A caller may ask for less
/// --- `materialise=false` to check a figure against the base data, `materialise=pinned` to
/// avoid a cuboid selected from somebody else's query log --- and may not ask for more.
///
/// `materialise` is the option the cube functions already accept, rather than a second name
/// for the same idea. `args.rs` refuses an option it does not know, on the grounds that a
/// misspelling which quietly takes its default produces a result wrong in a way the query
/// text does not reveal --- and inventing `materialisation=` here would have been that
/// misspelling, shipped.
///
/// A session that could raise the budget would be an unbounded storage grant to anybody who
/// can open one, so there is deliberately no spelling of this that widens anything. Every
/// value narrows, which is a property of [`Session`](sankhya_cube::materialise::Session)
/// itself rather than of this parser: there is no variant to reach for.
///
/// Read from the statement text, like `mentions_a_cube_function` above and with the same
/// caveat: a `SessionContext` is built per statement and this decision is needed before the
/// table function runs, so there is nothing better to read yet. An unrecognised value is
/// **`AsConfigured`, never an error** --- refusing a whole statement over a hint about where
/// an answer is computed would turn a performance control into an outage.
fn asked_of_materialisation(sql: &str) -> sankhya_cube::materialise::Session {
    use sankhya_cube::materialise::Session;
    let lowered = sql.to_ascii_lowercase().replace(' ', "");
    if lowered.contains("materialise=false") {
        return Session::Off;
    }
    if lowered.contains("materialise=pinned") {
        return Session::PinnedOnly;
    }
    Session::AsConfigured
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
    pub fn adopting_cubes(self, warehouse: &std::path::Path) -> (Self, Vec<String>) {
        let mut complaints = Vec::new();
        let definitions = match sankhya_cube::catalogue::load_all(warehouse) {
            Ok(definitions) => definitions,
            Err(error) => {
                complaints.push(error.to_string());
                Vec::new()
            }
        };
        let mut adopted = Vec::new();
        for definition in definitions {
            let name = definition.name.clone();
            match definition.validate() {
                Ok(cube) => adopted.push(cube),
                Err(rejections) => {
                    let why: Vec<String> =
                        rejections.iter().map(ToString::to_string).collect();
                    complaints.push(format!("the cube `{name}`: {}", why.join("; ")));
                }
            }
        }
        // Constructing, so nothing else can hold the lock. Poisoning is recovered from
        // rather than propagated for the reason it is everywhere else here: a panic in a
        // statement that touched this list must not make every later cube query fail.
        if let Ok(mut cubes) = self.cubes.write() {
            *cubes = Arc::new(adopted);
        }
        (self, complaints)
    }

    /// The cubes this server currently serves.
    ///
    /// Returns a snapshot rather than a borrow, because `CREATE CUBE` and `DROP CUBE` can
    /// change the set between two statements. A caller holding this sees a consistent list
    /// for as long as it holds it, which is the right guarantee: a statement is planned
    /// against the cubes that existed when it started.
    #[must_use]
    pub fn cubes(&self) -> Arc<Vec<sankhya_cube::model::Cube>> {
        self.cubes
            .read()
            .map_or_else(|poisoned| Arc::clone(&poisoned.into_inner()), |cubes| Arc::clone(&cubes))
    }

    /// Assemble a server that can actually answer queries.
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
            servable: parking_lot::RwLock::new(Arc::new(servable)),
            leases: Arc::new(sankhya_leases::Leases::new()),
            log_cache: sankhya_table_delta::LogCache::new(),
            cubes: std::sync::RwLock::new(Arc::new(Vec::new())),
            hydrated: Arc::new(sankhya_cube_sql::hydrated::Hydrated::default()),
            query_log: Arc::new(sankhya_cube::querylog::QueryLog::new()),
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
        let cubes = self.cubes();
        if cubes.is_empty() {
            return;
        }
        // Describing a cube reads no data, so it is registered whatever the statement says.
        // Hydration is the expensive half and only that is gated on the statement naming a
        // navigation function --- a client listing cubes must not pay for reading one.
        let navigating = mentions_a_cube_function(sql);
        // What this caller will accept. `Session::Off` is the reproducibility check: a figure
        // that differs between it and `AsConfigured` is a defect rather than a tuning
        // question, and per exit criterion 3a the two must be bit-identical.
        let session = asked_of_materialisation(sql);
        // The finest grain any cube call in this statement needs. A cuboid coarser than this
        // cannot serve it, however cheap it would be to scan.
        let needed = sankhya_cube::algo::Cuboid::of(
            &grain_needed(sql).iter().map(String::as_str).collect::<Vec<&str>>(),
        );
        let catalog = Arc::new(sankhya_cube_sql::catalog::CubeCatalog::new());
        for cube in cubes.iter() {
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
                // The in-memory cache, unless this caller asked for the base data and what
                // is cached came from a cuboid.
                //
                // `materialise=false` promises the answer was computed from the base data,
                // and serving it a cached entry that was itself read from a cuboid breaks
                // that promise one level up --- the reproducibility check would then be
                // comparing a cuboid against itself and agreeing, which is the one way it can
                // fail to do its job. `Published::from_cuboid` is what makes this decidable
                // rather than guessable.
                let base_only = session == sankhya_cube::materialise::Session::Off;
                if let Some(held) = self.hydrated.get(&key) {
                    if !(base_only && held.from_cuboid) {
                        catalog.publish(cube.name(), held);
                        continue;
                    }
                }
                // A cuboid the maintenance tick already built, if there is one at this
                // scope and snapshot and it is inside its target lag.
                //
                // **This is what makes materialisation load-bearing rather than write-only.**
                // Until it existed the refresher built cuboids on a timer and nothing ever
                // read one: the storage was spent, the target lag was checked, and every
                // query still went to the fact table. The seventh instance of a capability
                // that is built, tested and unreachable, and the most expensive, because this
                // one was also writing files.
                //
                // The base cuboid only, for now. Cells published here are the finest grain a
                // query may dice to, and the SQL surface rolls up from them --- so publishing
                // a coarser cuboid would answer a fine query from cells that cannot express
                // it. Choosing a coarser one per query is what `materialise::plan` is for,
                // and it needs the query's shape at publish time. Recorded in `STATUS.md`
                // under "answering from a materialised ancestor" as the last piece of §11.6
                // that is designed and not reachable.
                // Which cuboid this caller may be served from.
                //
                // Their own scope first --- a cuboid built by an earlier query of theirs.
                // Then the unrestricted one, and **only if their guard withholds nothing**:
                // it holds an aggregate over every row, so serving it to somebody a policy
                // filters would be a disclosure through arithmetic, and an invisible one,
                // because the number is real and simply over rows they may not read.
                //
                // Comparing digests would not do. A digest hashes the tenant and the table
                // and so is never the zero sentinel `Key::unrestricted` uses, which is why
                // the background refresher's output could not serve anybody at all until this
                // existed --- it wrote scope 0 and every caller looked under a hash.
                let mut scopes = vec![scope];
                if self.withholds_nothing(principal, cube.fact_table()) {
                    scopes.push(sankhya_cube::materialise::Key::UNRESTRICTED);
                }
                if let Some(published) = scopes
                    .into_iter()
                    .find_map(|under| {
                        self.from_a_cuboid(cube, measure, under, snapshot, session, &needed)
                    })
                {
                    catalog.publish(cube.name(), published.clone());
                    self.hydrated.put(key, published);
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
        sankhya_cube_sql::functions::register(
            context,
            Arc::clone(&catalog),
            Arc::clone(&self.query_log),
        );
        // Description alongside navigation, always. A surface a client can use only by
        // already knowing the model is a surface only its author can use, and a picker that
        // hardcodes a cube's dimensions is a picker that drifts from the cube.
        sankhya_cube_sql::describe::register(context, self.cubes(), catalog);
    }

    /// Build the cuboids maintained cubes are missing, and report what was built.
    ///
    /// # Why this runs without a caller, and what that costs
    ///
    /// A cube marked maintained is maintained whether or not the person who declared it is
    /// logged in. That is the whole point of the lifetime --- a dashboard is fast at nine in
    /// the morning because something built its cells at four.
    ///
    /// But an aggregate is computed over the rows *some principal* may read, and a refresh
    /// running on a timer has no principal. So it builds the **unrestricted** cuboid, and per
    /// [ADR-0008](../../../docs/adr/0008-serving-cubes-under-policy.md) an unrestricted cuboid
    /// may serve only an unrestricted caller.
    ///
    /// The consequence is worth stating rather than discovering: **background refresh helps
    /// service accounts and dashboards, and does nothing for a restricted analyst**, whose
    /// cuboids can only be built by their own queries. Pre-building a restricted scope needs
    /// somebody to name the scopes, which is a decision nobody has made yet and not one to
    /// take by implication.
    ///
    /// Returns the cubes refreshed, so a caller can log it rather than have work happen
    /// invisibly.
    pub fn refresh_maintained_cubes(&self) -> Vec<String> {
        let mut refreshed = Vec::new();
        for cube in self.cubes().iter() {
            let Some(_) = cube.target_lag() else {
                // Declared, not maintained. Nothing to build, and building it anyway would
                // charge an operator storage they did not ask for.
                continue;
            };
            let snapshot = self.snapshot_of(cube.fact_table());
            let base = sankhya_cube::algo::Cuboid::of(
                &cube.dimensions().iter().map(|d| d.name.as_str()).collect::<Vec<&str>>(),
            );
            // What people have actually asked this cube for.
            //
            // §11.6's selection has existed and been tested since M7 began and could not run,
            // because nothing recorded the signal its own documentation says it needs:
            // selecting against the whole lattice "optimises for queries nobody runs, which
            // is the same mistake as a person guessing, made faster".
            //
            // A cube nobody has queried gets its base cuboid and nothing else. That is the
            // honest answer rather than a guess: there is no evidence about what would help,
            // and spending an operator's storage on a guess is worse than spending none.
            let asked = self.query_log.asked(cube.name());
            // §11.6's three levels, resolved. The definition pins what it always wants; the
            // operator's configuration bounds what selection may spend on evidence; the
            // session narrows what a given query will use, and is applied when serving rather
            // than when building --- a caller may ask for less, never for more, so nothing a
            // session says can change what gets written here.
            let policy = sankhya_cube::materialise::Policy::new(
                cube.pinned(),
                self.settings.cuboid_budget_rows,
            );
            // The base, plus everything pinned, plus whatever selection says is worth holding
            // given what has been asked. Selection spends the operator's row budget, so a
            // cube asked for one shape repeatedly gets that shape and a cube asked for twenty
            // gets whichever few fit.
            //
            // Pinned shapes are not put through selection. A pin is the statement that a
            // shape is worth holding *before* any evidence exists --- the month-end roll-up
            // nobody runs until the day it must be instant --- so making it compete against a
            // query log would be ignoring the one control the modeller has.
            let mut wanted = vec![base.clone()];
            for shape in policy.pinned() {
                if !wanted.contains(shape) {
                    wanted.push(shape.clone());
                }
            }
            if !asked.is_empty() {
                let lattice = sankhya_cube::algo::Lattice::over(asked.clone());
                for measure in cube.measures() {
                    for chosen in sankhya_cube::algo::select(
                        &lattice,
                        &asked,
                        measure,
                        &EstimatedCost,
                        policy.budget_rows(),
                        &base,
                    ) {
                        if !wanted.contains(&chosen.cuboid) {
                            wanted.push(chosen.cuboid);
                        }
                    }
                }
            }

            for measure in cube.measures() {
              for shape in &wanted {
                let key = sankhya_cube::materialise::Key::unrestricted(
                    cube.version(),
                    snapshot,
                    shape.clone(),
                );
                if sankhya_maintenance::cuboid::exists(
                    &self.settings.warehouse,
                    &key,
                    cube.name(),
                ) {
                    continue;
                }
                let Some((base_cells, completeness)) = self.hydrate_unrestricted(cube, measure)
                else {
                    continue;
                };
                // Rolled to the shape being stored.
                //
                // Hydration produces cells at the **base** grain, and storing those under a
                // coarser cuboid's key would file `[region, period]` cells as a `[region]`
                // cuboid --- a cache that lies about its own grain, which is worse than no
                // cache because a reader trusts the key.
                //
                // A dimension that will not roll away is a measure that does not compose
                // there, and the shape is skipped rather than stored wrong.
                let Some(cells) = roll_to(&base_cells, shape, measure) else {
                    continue;
                };
                let Some(rule) = cube
                    .dimensions()
                    .first()
                    .and_then(|d| measure.rule(&d.name))
                else {
                    continue;
                };
                // The completeness of the *hydration*, not of the roll-up. Rolling up moves
                // cells between addresses and withholds nothing, so what a coarser cuboid saw
                // is exactly what the base saw.
                if sankhya_maintenance::cuboid::materialise_quietly(
                    &self.settings.warehouse,
                    &key,
                    cube.name(),
                    &cells,
                    rule,
                    &completeness,
                ) {
                    refreshed.push(format!("{}.{}", cube.name(), measure.name));
                }
              }
            }
        }
        // Built, then swept. In that order: a cuboid written this pass is at the current
        // version and cannot be collected by the sweep below, and doing it the other way
        // round would leave the newest garbage until the next pass.
        self.retire_superseded_cuboids();
        refreshed
    }

    /// Remove cuboids no query can ask for.
    ///
    /// A cuboid is found by a key embedding its snapshot, and a query asks at the table's
    /// current version --- so a cuboid at an older snapshot is garbage the moment the table
    /// advances. Nothing collected it: the orphan sweep finds unreferenced files *within* a
    /// table, and a superseded cuboid is a whole table no log mentions, so it fell between
    /// the two mechanisms that exist.
    ///
    /// The tolerance is deliberately more generous than any `target_lag`. That decides what
    /// may be **served**; this decides what may be **deleted**, and a query that resolved a
    /// cuboid a moment ago is still reading it.
    fn retire_superseded_cuboids(&self) {
        let cubes = self.cubes();
        if cubes.is_empty() {
            return;
        }
        let current: std::collections::BTreeMap<String, u64> = cubes
            .iter()
            .map(|cube| {
                (
                    cube.name().to_string(),
                    self.snapshot_of(cube.fact_table()),
                )
            })
            .collect();
        let swept = sankhya_maintenance::cuboid::retire_superseded(
            &self.settings.warehouse,
            &current,
            CUBOID_DRIFT_TOLERATED,
        );
        if !swept.removed.is_empty() {
            println!(
                "  retired {} superseded cuboid(s), {:.1} MB",
                swept.removed.len(),
                swept.bytes_reclaimed as f64 / (1024.0 * 1024.0)
            );
        }
    }

    /// Hydrate a cube over every row, with no policy applied.
    ///
    /// The providers are registered **unsecured**, which is what makes this the unrestricted
    /// scope rather than one principal's. It is only ever used to build a cuboid keyed as
    /// unrestricted, and such a cuboid may only serve a caller who is themselves
    /// unrestricted --- so the widest cells never reach a narrower reader.
    fn hydrate_unrestricted(
        &self,
        cube: &sankhya_cube::model::Cube,
        measure: &sankhya_cube::algo::Measure,
    ) -> Option<(sankhya_cube::cells::Cells, sankhya_cube::complete::Completeness)> {
        let context = SessionContext::new();
        for table in self.servable.read().iter() {
            context
                .register_table(table.reference.table.as_str(), Arc::clone(&table.provider))
                .ok()?;
        }
        let catalog = Arc::new(sankhya_cube_sql::catalog::CubeCatalog::new());
        let hydrated = tokio::task::block_in_place(|| {
            self.runtime
                .block_on(sankhya_cube_sql::publish::publish_from_fact_table(
                    &context,
                    &catalog,
                    cube.name(),
                    Arc::new(cube.clone()),
                    measure,
                    self.snapshot_of(cube.fact_table()),
                ))
        });
        hydrated.ok()?;
        let published = catalog.resolve(cube.name(), &measure.name).ok()?;
        // The completeness travels with the cells from here to the stored cuboid. Dropping it
        // would leave the cuboid able to claim only that it was complete, which is the one
        // claim nothing may make on its own behalf.
        Some(((*published.cells).clone(), published.completeness))
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
    ) -> Option<(sankhya_cube::cells::Cells, sankhya_cube::complete::Completeness)> {
        // Within its stated lag, or not used at all.
        //
        // A cuboid past its target is not served as though it were fresh: the answer falls
        // back to live aggregation, which is slower and correct. Serving a stale figure
        // because it is quick is how a dashboard comes to disagree with the table it is drawn
        // from, with nobody able to say by how much.
        //
        // A cube with no target materialises nothing, so `within_target` is false for it and
        // this returns early --- which is the Declared lifetime behaving as declared rather
        // than as a special case.
        let behind = sankhya_maintenance::cuboid::lag(
            key.snapshot,
            self.snapshot_of(cube.fact_table()),
        );
        if !sankhya_maintenance::cuboid::within_target(cube.target_lag(), behind) {
            return None;
        }
        let root = self.cuboid_root(key, cube.name());
        if !root.join("_delta_log").is_dir() {
            return None;
        }
        // **The dimensions the key names, not the cube's.**
        //
        // A cuboid holds exactly the columns its key says it does --- that is what makes the
        // name reversible and what `roll_to` exists to keep true on the way in. Reading it
        // back at the cube's full grain worked for as long as the only cuboid ever read was
        // the base one, whose grain *is* the cube's, and broke the moment an ancestor was
        // chosen: the schema named a column the file does not have.
        let dimensions: Vec<String> = key
            .cuboid
            .dimensions()
            .iter()
            .map(ToString::to_string)
            .collect();
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
        // Every batch of a cuboid must agree on its completeness, for the same reason every
        // row must: the file records one hydration, and two answers to "how much did this
        // see" is not an answer. A cuboid with no batches has nothing to serve.
        let mut completeness: Option<sankhya_cube::complete::Completeness> = None;
        for batch in &batches {
            let (read, saw) = sankhya_cube::store::from_batch(batch, &dimensions, rule).ok()?;
            if *completeness.get_or_insert(saw) != saw {
                return None;
            }
            for address in read.addresses() {
                let contributions = read.contributions(address)?;
                cells
                    .add_reduced(address.clone(), rule, contributions.exact_sum())
                    .ok()?;
            }
        }
        Some((cells, completeness?))
    }

    /// Every cuboid materialised for this cube at this definition, snapshot and scope.
    ///
    /// Read from the cuboid store's directory names, which `materialise::parse` reverses. The
    /// name is the key, so this needs no index and cannot disagree with what is on disk ---
    /// an index would be a second record of the same fact, and the two would drift.
    ///
    /// A directory that does not parse, or parses to another cube, definition, snapshot or
    /// scope, is simply not a candidate. There is nothing to report: a warehouse holds
    /// cuboids for every cube it serves and most of them are somebody else's.
    fn materialised_shapes(
        &self,
        cube: &sankhya_cube::model::Cube,
        snapshot: u64,
        scope: u64,
    ) -> Vec<sankhya_cube::algo::Cuboid> {
        let store = self
            .settings
            .warehouse
            .join(sankhya_maintenance::cuboid::CUBOIDS);
        let Ok(entries) = std::fs::read_dir(&store) else {
            return Vec::new();
        };
        let mut shapes: Vec<sankhya_cube::algo::Cuboid> = entries
            .flatten()
            .filter_map(|entry| entry.file_name().to_str().map(ToString::to_string))
            .filter_map(|name| sankhya_cube::materialise::parse(&name))
            .filter(|(named, key)| {
                named == cube.name()
                    && key.definition == cube.version()
                    && key.snapshot == snapshot
                    && key.scope == scope
            })
            .map(|(_, key)| key.cuboid)
            .collect();
        // Sorted, so two runs plan identically and "why was this fast?" has one answer.
        shapes.sort();
        shapes
    }

    /// Whether this principal's guard on `table` removes nothing.
    ///
    /// `false` when there is no guard at all: no access is not unrestricted access.
    fn withholds_nothing(&self, principal: &Principal, table: &str) -> bool {
        let reference = self
            .servable
            .read()
            .iter()
            .find(|servable| servable.reference.table == table)
            .map(|servable| servable.reference.clone())
            .unwrap_or_else(|| TableRef::new("", table));
        Guard::authorize(&self.policy, principal, &reference, Action::Read)
            .is_some_and(|guard| guard.withholds_nothing())
    }

    /// A cube's cells from a materialised cuboid, if one can serve this caller.
    ///
    /// Returns `None` for every reason a cuboid might not apply --- none built, past its
    /// target lag, a different scope, a different snapshot --- and the caller then hydrates
    /// from the fact table. **A miss is slower and never wrong**, which is the property that
    /// lets materialisation be automatic: per M7's exit criterion 3a the answer must be
    /// bit-identical either way, so this may only ever change where a number is computed.
    ///
    /// The scope is part of the key rather than a check applied afterwards. An aggregate over
    /// the rows one principal may read is not an answer for another, and a lookup that cannot
    /// match is a stronger guarantee than a comparison somebody has to remember to write.
    fn from_a_cuboid(
        &self,
        cube: &sankhya_cube::model::Cube,
        measure: &sankhya_cube::algo::Measure,
        scope: u64,
        snapshot: u64,
        session: sankhya_cube::materialise::Session,
        needed: &sankhya_cube::algo::Cuboid,
    ) -> Option<sankhya_cube_sql::catalog::Published> {
        let base = sankhya_cube::algo::Cuboid::of(
            &cube.dimensions().iter().map(|d| d.name.as_str()).collect::<Vec<&str>>(),
        );
        // Everything materialised for this cube at this definition, snapshot and scope.
        let available = self.materialised_shapes(cube, snapshot, scope);
        // What this session will accept of it. `Off` accepts nothing and the answer is
        // computed from the base data, which is the check exit criterion 3a exists for.
        // `PinnedOnly` accepts a shape the definition names and not one selection bought from
        // somebody else's query log.
        let policy = sankhya_cube::materialise::Policy::new(
            cube.pinned(),
            self.settings.cuboid_budget_rows,
        );
        let usable: Vec<&sankhya_cube::algo::Cuboid> = policy.usable(&available, session);
        if usable.is_empty() {
            return None;
        }

        // **Answering from an ancestor**, which is §11.6's reason for the lattice.
        //
        // `plan` prefers the narrowest cuboid that may legally answer --- narrowest by
        // dimension count, which is the cheapest to scan --- and a cuboid is a candidate only
        // when the measure permits every roll-up between it and the query. Skipping that test
        // is how materialisation starts changing answers, and the change is invisible: the
        // number is real, it is just computed from partial aggregates that do not compose.
        //
        // That check is exit criterion 3b, and until this existed it was satisfied vacuously,
        // because nothing ever answered from an ancestor at all.
        let chosen = sankhya_cube::materialise::plan(needed, measure, &usable, &base);
        if !chosen.materialised {
            return None;
        }
        let key = sankhya_cube::materialise::Key {
            definition: cube.version(),
            snapshot,
            scope,
            cuboid: chosen.from,
        };
        let (cells, completeness) = self.materialised(&key, cube, measure)?;
        Some(sankhya_cube_sql::catalog::Published {
            cube: Arc::new(cube.clone()),
            cells: Arc::new(cells),
            measure: measure.name.clone(),
            snapshot,
            // Read from the cuboid, never assumed. A cuboid stores what its hydration saw
            // precisely so that serving it does not have to invent this.
            completeness,
            from_cuboid: true,
        })
    }

    /// The tenant this server serves.
    #[must_use]
    pub const fn tenant(&self) -> sankhya_authz::principal::TenantId {
        self.settings.tenant
    }

    /// The policy every session is built against.
    #[must_use]
    pub const fn policy_set(&self) -> &PolicySet {
        &self.policy
    }

    /// The tables that can be served right now, refreshed if their logs have moved.
    ///
    /// Refreshed here rather than read stale, for the reason the statement path refreshes:
    /// providers resolved once at boot name files that maintenance later retires, and a
    /// reader holding them fails on a path the caller never mentioned.
    #[must_use]
    pub fn servable_now(&self) -> Arc<Vec<ServableTable>> {
        self.refreshed_servable()
    }

    /// The servable tables, with any whose log has moved re-resolved.
    ///
    /// The filesystem work happens **outside** the lock, against a snapshot taken under it.
    /// Two statements refreshing at once may both do the probing and one of their lists wins;
    /// that costs a duplicated probe and never a wrong answer, because both are resolving the
    /// same logs at the same target. Serializing every statement to avoid it --- which is what
    /// holding the write lock across the probes did --- is the more expensive mistake.
    fn refreshed_servable(&self) -> Arc<Vec<ServableTable>> {
        let current = Arc::clone(&self.servable.read());
        let mut candidate = (*current).clone();
        let moved =
            crate::warehouse::refresh(&mut candidate, self.settings.read_as_of, &self.log_cache);
        if moved == 0 {
            // The ordinary case: nothing has committed since the last statement, so there is
            // nothing to publish and no reason to take the write lock at all.
            return current;
        }
        let replacement = Arc::new(candidate);
        *self.servable.write() = Arc::clone(&replacement);
        replacement
    }

    /// The principal a Flight request acts as.
    ///
    /// Tenant-scoped, because a ticket carries a tenant and not a subject. Every user of a
    /// tenant currently receives the same roles, so this is exactly the principal any of them
    /// would get --- and when that stops being true the subject has to travel in the ticket.
    #[must_use]
    pub fn flight_principal(&self) -> Option<Principal> {
        self.principal("flight")
    }

    /// The newest version any servable table stands at.
    ///
    /// What a Flight ticket records as the snapshot it was planned against. The newest across
    /// tables rather than one table's, because a statement may name several and the ticket has
    /// one field --- and taking the newest is the value that cannot be *older* than what the
    /// plan saw.
    #[must_use]
    pub fn newest_snapshot(&self) -> u64 {
        self.servable
            .read()
            .iter()
            .filter_map(|servable| sankhya_table_delta::live_files(&servable.root).ok())
            .filter_map(|live| live.version)
            .max()
            .unwrap_or(0)
    }

    /// The registry this server's statements pin, for the maintenance thread to consult.
    ///
    /// Handed out rather than rebuilt, because two registries would be worse than none: the
    /// sweeper would consult one that no reader ever announces into, conclude the warehouse is
    /// idle, and delete files under live queries --- while every test of either half passed.
    #[must_use]
    pub fn leases(&self) -> Arc<sankhya_leases::Leases> {
        Arc::clone(&self.leases)
    }

    /// The version a table's log stands at, for a test that must name the same one.
    #[must_use]
    pub fn snapshot_for_test(&self, table: &str) -> u64 {
        self.snapshot_of(table)
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

    /// Run a `CREATE CUBE` or `DROP CUBE`.
    ///
    /// # Why the same failure is reported for "no such table" and "you may not read it"
    ///
    /// Because they must be indistinguishable. The query path already refuses to confirm a
    /// table's existence to somebody who may not read it, and cube DDL naming a fact table
    /// would be a way to ask the same question through a different door: a `CREATE CUBE` that
    /// answered *"you may not read `payroll`"* has told you `payroll` exists.
    ///
    /// So the check is [`Self::scope_for`] --- the same authorization the query path uses,
    /// with no second implementation to disagree with it --- and both answers are the one
    /// sentence below.
    fn run_cube_ddl(
        &self,
        statement: Result<sankhya_cube_sql::Statement, sankhya_cube_sql::DdlError>,
        principal: &Principal,
    ) -> Result<QueryResult, QueryFailure> {
        use sankhya_error::protocol::sqlstate;

        let statement = statement.map_err(|error| {
            refusal(sqlstate::SYNTAX_ERROR.as_str(), &error.to_string())
        })?;

        match statement {
            sankhya_cube_sql::Statement::Create(definition) => {
                self.create_cube(*definition, principal)
            }
            sankhya_cube_sql::Statement::Drop { name, if_exists } => {
                self.drop_cube(&name, if_exists, principal)
            }
        }
    }

    /// Validate a definition, persist it, and start serving it.
    fn create_cube(
        &self,
        definition: sankhya_cube::model::Definition,
        principal: &Principal,
    ) -> Result<QueryResult, QueryFailure> {
        use sankhya_error::protocol::sqlstate;

        let name = definition.name.clone();

        // A name already taken is refused rather than replaced, and there is no
        // `OR REPLACE`. Replacing a cube orphans every cuboid it materialised, and the
        // reclamation of those is a real operation with a real cost --- see
        // `cuboid::retire_cube`. Hiding that inside a `CREATE` would make an expensive,
        // irreversible thing happen because somebody re-ran a script. `DROP` then `CREATE`
        // says it out loud.
        if self.cubes().iter().any(|cube| cube.name() == name) {
            return Err(refusal(
                sqlstate::DATA_EXCEPTION.as_str(),
                &format!(
                    "the cube `{name}` already exists. Drop it first: replacing a cube \
                     retires every cuboid it materialised, which is not something a \
                     re-run of a script should do silently"
                ),
            ));
        }

        // Every table the cube reads, checked against the same authorization the query path
        // uses. A cube whose fact table this principal cannot read would hydrate to nothing
        // anyway; refusing here means the refusal names the statement rather than arriving
        // later as an empty answer nobody can explain.
        let mut tables = vec![definition.fact_table.clone()];
        tables.extend(definition.dimensions.iter().map(|d| d.table.clone()));
        for table in tables {
            if self.scope_for(principal, &table).is_none() {
                return Err(refusal(
                    sqlstate::DATA_EXCEPTION.as_str(),
                    &format!("there is no table `{table}` to build a cube on"),
                ));
            }
        }

        // The one validator, reporting every rejection rather than the first. A definition
        // fixable in one sitting should be reported in one message.
        let cube = definition.validate().map_err(|rejections| {
            let why: Vec<String> = rejections.iter().map(ToString::to_string).collect();
            refusal(
                sqlstate::DATA_EXCEPTION.as_str(),
                &format!("the cube `{name}` was not created: {}", why.join("; ")),
            )
        })?;

        // Persisted before it is served, so a cube that answers a query is a cube that would
        // survive a restart. The other order produces a cube that works until it does not,
        // and the moment it stops is a restart nobody connects to the statement.
        sankhya_cube::catalogue::save(&self.settings.warehouse, cube.definition()).map_err(
            |error| refusal(sqlstate::IO_ERROR.as_str(), &error.to_string()),
        )?;

        if let Ok(mut cubes) = self.cubes.write() {
            let mut next: Vec<_> = cubes.iter().cloned().collect();
            next.push(cube);
            *cubes = Arc::new(next);
        }

        // Audited as an insert against the cube's own name. There is no `Action` for DDL
        // and inventing one would mean a second vocabulary for the audit reader to learn;
        // creating a cube adds something that was not there, which is what `Insert` says.
        self.record(principal, TableRef::new("", &name), Action::Insert, true);
        Ok(acknowledged("CREATE CUBE"))
    }

    /// Stop serving a cube, remove its definition, and reclaim what it materialised.
    fn drop_cube(
        &self,
        name: &str,
        if_exists: bool,
        principal: &Principal,
    ) -> Result<QueryResult, QueryFailure> {
        use sankhya_error::protocol::sqlstate;

        let Some(cube) = self.cubes().iter().find(|cube| cube.name() == name).cloned() else {
            if if_exists {
                return Ok(acknowledged("DROP CUBE"));
            }
            return Err(refusal(
                sqlstate::DATA_EXCEPTION.as_str(),
                &format!("there is no cube `{name}`"),
            ));
        };

        // Dropping a cube reads no table, so there is nothing to authorize against a fact
        // table --- but a principal who cannot read what the cube is built on has no business
        // removing it, and the check costs nothing. The refusal is the same sentence as
        // everywhere else, for the same reason.
        if self.scope_for(principal, cube.fact_table()).is_none() {
            return Err(refusal(
                sqlstate::DATA_EXCEPTION.as_str(),
                &format!("there is no cube `{name}`"),
            ));
        }

        // Out of the served set first, so no statement started after this point can resolve
        // the cube and reach files that are about to go. A statement already running holds
        // its files open and finishes against them.
        if let Ok(mut cubes) = self.cubes.write() {
            let next: Vec<_> =
                cubes.iter().filter(|held| held.name() != name).cloned().collect();
            *cubes = Arc::new(next);
        }

        let definition = sankhya_cube::catalogue::path_of(&self.settings.warehouse, name);
        if let Err(error) = std::fs::remove_file(&definition) {
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(refusal(sqlstate::IO_ERROR.as_str(), &error.to_string()));
            }
        }

        // And the cuboids, which nothing else will ever reclaim: `retire_superseded` keeps a
        // cuboid whose cube has no known current version, deliberately and with a reason, so
        // a dropped cube's materialised storage would otherwise be retained for good.
        let swept = sankhya_maintenance::cuboid::retire_cube(&self.settings.warehouse, name);
        if !swept.removed.is_empty() {
            tracing::info!(
                cube = name,
                cuboids = swept.removed.len(),
                bytes = swept.bytes_reclaimed,
                "retired the cuboids of a dropped cube"
            );
        }

        self.record(principal, TableRef::new("", name), Action::Delete, true);
        Ok(acknowledged("DROP CUBE"))
    }

    /// Everything `query` does, without the measuring.
    ///
    /// Split out so that the counter and the histogram are recorded on **every** path out of
    /// the statement, including the two refusals that never reach the query path. A
    /// duration histogram fed only by the successful path describes a system that never
    /// fails, and the tail an operator goes looking for is made of failures.
    fn run_statement(&self, sql: &str) -> Result<QueryResult, QueryFailure> {
        // Announced for as long as this statement runs.
        //
        // Taken here rather than around the scan, because the window that matters opens when
        // the statement resolves a table into a set of file paths and closes when the last of
        // them has been read. Pinning any later would leave the resolve unprotected, which is
        // exactly the gap: the paths are chosen from a log, and the files behind them can be
        // retired between the choosing and the opening.
        let _reading = self.leases.pin();

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

        // Cube DDL, before the engine is asked anything.
        //
        // `CREATE CUBE` is not SQL, so `sqlparser` rejects it before any DataFusion hook can
        // see it. It has to be recognised here or not at all. `parse_ddl` returns `None` for
        // everything that is not cube DDL, which is every other statement in the language.
        //
        // After admission and after the principal, because a cube is created *by* somebody
        // and against tables they must be allowed to read; before the session, because none
        // of what `session_for` builds is any use to a statement that reads no data.
        if let Some(statement) = sankhya_cube_sql::parse_ddl(sql) {
            return self.run_cube_ddl(statement, &principal);
        }

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
        let servable = self.refreshed_servable();
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
/// A statement that did something and returns no rows.
///
/// The tag is what a client prints and what a driver branches on, so it names the statement
/// rather than being an empty string that leaves `psql` silent about whether anything
/// happened.
fn acknowledged(tag: &str) -> QueryResult {
    QueryResult { fields: Vec::new(), rows: Vec::new(), tag: tag.to_string() }
}

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

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
use crate::execute::{run, session_and_contested, session_for, ServableTable};

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
    /// What each named user's password is checked against, from `server.credentials.<name>`.
    ///
    /// `SEC-01`: there was no credential store at all, and the check was that a password had
    /// been *presented*. Empty means no user has one written down, which is the old behaviour
    /// deliberately — an operator who has configured nothing has decided nothing. Non-empty
    /// means the map is the map, and a user absent from it is refused. §13.6a has the rest.
    pub credentials: std::collections::BTreeMap<String, sankhya_credential::Verifier>,
    /// The roles each named user holds, from `server.users.<name>`.
    ///
    /// # Why an empty map means *everybody is a reader*
    ///
    /// Because the presence of the map is the switch, and a separate flag is a flag somebody
    /// forgets. An operator who has written down no users has not decided anything about
    /// roles, and this build's behaviour --- one role for everybody --- is what they get.
    ///
    /// An operator who has written down **one** user has decided that the list is the list, so
    /// a user absent from it holds no role at all and every rule that grants by role passes
    /// them by. That is the direction to be wrong in: adding a user is a change somebody
    /// notices, and silently granting one is not.
    ///
    /// Until this existed, `Principal::authenticated` was handed a literal `reader` for every
    /// connection --- so the subject travelled inward correctly and **nothing downstream could
    /// tell two subjects apart**, which is a plumbing job finished and a feature that is not.
    pub roles: std::collections::BTreeMap<String, Vec<String>>,
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
    /// Whether this server will accept `CREATE AGGREGATION`, which runs supplied code.
    ///
    /// # Why this defaults to off
    ///
    /// `ADR-0023` Decision 4 says the capability "is not granted by default" and that "the
    /// grant is per principal, not per server". The per-principal half is not built. Until it
    /// is, a server-wide switch that starts closed is the honest version of that sentence ---
    /// the alternative, which shipped, was a statement that ran arbitrary Python for any
    /// caller including one holding no roles at all, while writing an audit entry saying the
    /// decision had been allowed.
    ///
    /// A switch rather than silence, so an operator who wants user functions turns them on and
    /// knows they did.
    pub user_functions: bool,
    /// Where the metrics endpoint listens, or `None` not to serve one.
    ///
    /// Its own address rather than a path on the wire-protocol port, so it can be bound to
    /// an interface clients cannot reach. Defaulting to loopback rather than to every
    /// interface, because the safe choice should be the one you get by not thinking.
    pub metrics_listen: Option<String>,
    /// The certificate both doors serve with, or `None` to serve in the clear.
    ///
    /// One certificate for both, because two would be two expiries, two renewals and two
    /// chances for the pair to disagree about who this server is. `sankhya-tls` loads it and
    /// each door names its own ALPN.
    pub transport_security: Option<TransportSecurity>,
}

/// What the name a client used turned out to name.
///
/// Its own type rather than `warehouse::Resolved` because the callers here want the *name*
/// --- it is what a lineage records and an authorization is checked against --- and carrying
/// the root beside it saves resolving the same name twice.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Qualified {
    /// One table: its `schema.table` name, and where its log lives.
    One(String, std::path::PathBuf),
    /// No table of that name.
    Absent,
    /// Several, named so the caller can say which.
    Ambiguous(Vec<String>),
}

/// What an operator configured about encryption.
///
/// # Why a half-configured door refuses to start
///
/// A certificate with no key, or a key with no certificate, is not a server that is *nearly*
/// encrypted --- it is a server with an operator who believes it is encrypted. Starting in
/// the clear at that point is the single most expensive default available, so the settings
/// are read as one unit and an incomplete one is an error rather than a fallback.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TransportSecurity {
    /// The certificate chain, PEM.
    pub certificate: std::path::PathBuf,
    /// Its private key, PEM.
    pub private_key: std::path::PathBuf,
    /// Anchors a client certificate must chain to, for mutual TLS.
    pub client_ca: Option<std::path::PathBuf>,
    /// Whether a wire-protocol client that never asks for TLS is refused.
    ///
    /// Only the wire protocol has this question: its clients negotiate, so a door can be
    /// encrypted and permissive at once. The columnar door has no such state --- a client
    /// either completes a handshake or gets nothing.
    pub require: bool,
}

/// The two doors' acceptors, from one certificate.
#[derive(Clone, Debug)]
pub struct Doors {
    /// The wire protocol's, advertising nothing.
    pub wire: sankhya_tls::Acceptor,
    /// The columnar door's, advertising HTTP/2.
    pub columnar: sankhya_tls::Acceptor,
}

/// Load one certificate and produce both doors' acceptors.
///
/// One load, because two would be two reads of a file that can change between them --- and a
/// server whose two doors present different certificates is one nobody can reason about.
///
/// # Errors
///
/// [`sankhya_tls::Refused`] naming the file, when the certificate, key or client bundle
/// cannot be used.
pub fn load_transport_security(
    security: &TransportSecurity,
) -> Result<Doors, sankhya_tls::Refused> {
    let material = sankhya_tls::Material::load(&security.certificate, &security.private_key)?;
    let material = match &security.client_ca {
        None => material,
        Some(bundle) => material.requiring_client_certificates(bundle)?,
    };
    Ok(Doors {
        wire: sankhya_tls::Acceptor::new(&material, sankhya_tls::Alpn::None)?,
        columnar: sankhya_tls::Acceptor::new(&material, sankhya_tls::Alpn::Http2)?,
    })
}

/// What this server's doors actually do, for saying out loud at startup.
///
/// Four states rather than a boolean, because *"encrypted"* covers three of them and an
/// operator reading a startup line needs to know which one they have. The dangerous one is
/// [`Posture::Offered`]: it looks like encryption in every log and serves a plain client
/// anyway.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Posture {
    /// No certificate. Everything crosses the network as it was typed.
    Clear,
    /// Encrypted for clients that ask; plain for the rest.
    Offered,
    /// Encrypted, and a client that does not ask is refused.
    Required,
    /// Encrypted, refused without asking, and a client certificate is checked too.
    Mutual,
}

/// Everything the server owns.
#[derive(Debug)]
pub struct Server {
    doors: Option<Doors>,
    /// What every declared feed is doing.
    ///
    /// On the server rather than in the task that runs feeds, because `ADR-0018` makes
    /// "halted" a state somebody has to act on --- and a state held in a task's local set is
    /// visible in the log line printed when it began and nowhere afterwards.
    feeds: Arc<sankhya_feed::state::Feeds>,
    /// Which table each declared feed writes into, by feed name.
    ///
    /// Held so that a feed command can be authorized against the same table a query would be.
    /// `RESUME FEED` restarts an ingest that halted because its source changed shape, and it
    /// took no principal at all --- so any caller could restart anybody's ingest. `SEC-04`.
    ///
    /// Written once at startup, where the declarations are read, and read by every feed
    /// command. A feed whose declaration did not load is absent here and its command is
    /// refused: not knowing what a feed writes into is not permission to start it.
    pub(crate) feed_targets: std::sync::RwLock<Arc<std::collections::BTreeMap<String, TableRef>>>,
    pub(crate) settings: Settings,
    pub(crate) policy: PolicySet,
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
    pub(crate) servable: parking_lot::RwLock<Arc<Vec<ServableTable>>>,
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
    pub(crate) cubes: std::sync::RwLock<Arc<Vec<sankhya_cube::model::Cube>>>,
    /// Aggregations somebody declared, and the worker that runs them.
    ///
    /// The worker is built **once** and only if this machine can host the boundary they must
    /// run behind. `None` is not a degraded mode: `CREATE AGGREGATION` then refuses, naming the
    /// mechanism, which is `ADR-0023` Decision 3 --- where the boundary cannot be built the
    /// feature is off rather than run without it.
    pub(crate) aggregations: std::sync::RwLock<Arc<Vec<sankhya_udf::Aggregation>>>,
    pub(crate) udf_worker: std::sync::OnceLock<Result<Arc<sankhya_udf::Worker>, String>>,
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
    pub(crate) runtime: tokio::runtime::Handle,
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
    /// Carry the loaded doors, so the columnar transport can be given its acceptor.
    ///
    /// The wire protocol's is applied to its listener at bind; the columnar one is started
    /// later by the binary, and this is how it reaches it without loading the certificate a
    /// second time.
    pub fn serving_encrypted(mut self, doors: Option<Doors>) -> Self {
        self.doors = doors;
        self
    }

    /// What this server's doors do.
    #[must_use]
    pub fn transport_posture(&self) -> Posture {
        match (&self.doors, &self.settings.transport_security) {
            (None, _) | (_, None) => Posture::Clear,
            (Some(doors), Some(security)) => {
                if doors.wire.is_mutual() {
                    Posture::Mutual
                } else if security.require {
                    Posture::Required
                } else {
                    Posture::Offered
                }
            }
        }
    }

    /// What every declared feed is doing, shared with whatever runs them.
    #[must_use]
    pub fn feeds(&self) -> Arc<sankhya_feed::state::Feeds> {
        Arc::clone(&self.feeds)
    }

    /// The tables this session sees when it has asked to read as of a named snapshot.
    ///
    /// # Errors
    ///
    /// As `snapshots::as_of`.
    fn as_of_snapshot(
        &self,
        caller: &sankhya_api_pg::session::Caller<'_>,
    ) -> Result<Option<Arc<Vec<ServableTable>>>, QueryFailure> {
        crate::snapshots::as_of(self, caller)
    }

    /// Where a table of this name lives, or `None` if it does not resolve to exactly one.
    #[must_use]
    pub(crate) fn root_of(&self, table: &str) -> Option<std::path::PathBuf> {
        match crate::warehouse::resolve(&self.settings.warehouse, table) {
            crate::warehouse::Resolved::One(root) => Some(root),
            crate::warehouse::Resolved::Absent | crate::warehouse::Resolved::Ambiguous(_) => None,
        }
    }

    /// Whether this principal may read this table, resolving a clone through its root.
    #[must_use]
    pub(crate) fn readable_by(&self, principal: &Principal, table: &str) -> bool {
        self.readable(principal, table, &self.lineages())
    }

    /// Today, as days from the epoch.
    pub(crate) fn today(&self) -> i32 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        i32::try_from(now / 86_400).unwrap_or(0)
    }

    /// Now, in microseconds from the epoch.
    pub(crate) fn now_micros(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| i64::try_from(since.as_micros()).unwrap_or(i64::MAX))
    }

    /// The columnar door's acceptor, if this server encrypts.
    #[must_use]
    pub fn columnar_acceptor(&self) -> Option<sankhya_tls::Acceptor> {
        self.doors.as_ref().map(|doors| doors.columnar.clone())
    }

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

        // And the aggregations, from the same directory tree and at the same moment. Adopted
        // here rather than lazily because a declared aggregation that only appears once
        // somebody calls it is one a restart silently drops --- and the symptom is a query
        // that worked yesterday failing to plan.
        //
        // **Not re-exercised.** `ADR-0010`'s determinism check runs at declaration; running it
        // again at every startup would fork a worker per aggregation before the server accepts
        // its first connection, and the code has not changed since it passed.
        let declared = crate::aggregations::stored(warehouse);
        if let Ok(mut aggregations) = self.aggregations.write() {
            *aggregations = Arc::new(declared);
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

    /// Every aggregation somebody has declared.
    pub fn aggregations(&self) -> Arc<Vec<sankhya_udf::Aggregation>> {
        self.aggregations.read().map_or_else(
            |poisoned| Arc::clone(&poisoned.into_inner()),
            |aggregations| Arc::clone(&aggregations),
        )
    }

    /// The worker user-supplied aggregations run behind, or why there is none.
    ///
    /// Built once, lazily, and the failure is **remembered**: probing the boundary forks, and a
    /// machine that cannot host it will not start being able to between two statements. Retried
    /// per statement it would be a fork per `CREATE AGGREGATION` on a machine where the answer
    /// is already known.
    pub(crate) fn worker(&self) -> Result<Arc<sankhya_udf::Worker>, sankhya_udf::Refused> {
        let outcome = self.udf_worker.get_or_init(|| {
            let python = std::path::Path::new("/usr/bin/python3");
            sankhya_udf::Worker::start(python)
                .map(Arc::new)
                .map_err(|refused| refused.to_string())
        });
        match outcome {
            Ok(worker) => Ok(Arc::clone(worker)),
            Err(said) => Err(sankhya_udf::Refused::NoBoundary(said.clone())),
        }
    }

    /// Serve one, having declared it.
    pub(crate) fn remember_aggregation(&self, aggregation: sankhya_udf::Aggregation) {
        if let Ok(mut held) = self.aggregations.write() {
            let mut next: Vec<_> = held.iter().cloned().collect();
            next.retain(|existing| existing.name != aggregation.name);
            next.push(aggregation);
            next.sort_by(|a, b| a.name.cmp(&b.name));
            *held = Arc::new(next);
        }
    }

    /// Stop serving one.
    pub(crate) fn forget_aggregation(&self, name: &str) {
        if let Ok(mut held) = self.aggregations.write() {
            let mut next: Vec<_> = held.iter().cloned().collect();
            next.retain(|existing| existing.name != name);
            *held = Arc::new(next);
        }
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
            doors: None,
            feeds: Arc::new(sankhya_feed::state::Feeds::new()),
            feed_targets: std::sync::RwLock::new(Arc::new(std::collections::BTreeMap::new())),
            settings,
            policy,
            quotas,
            audit: parking_lot::Mutex::new(Chain::new()),
            tables,
            servable: parking_lot::RwLock::new(Arc::new(servable)),
            aggregations: std::sync::RwLock::new(Arc::new(Vec::new())),
            udf_worker: std::sync::OnceLock::new(),
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
        // Three postures, and the line names which one is in force.
        //
        // It used to name two, and the one it called `PASSWORD UNVERIFIED` was the truth: the
        // check was that a password was *non-empty*, with nothing to compare it against
        // anywhere in the workspace. `SEC-01`. That line was written in Phase 0 precisely so
        // an operator reading "password required" beside the capitalised "NO AUTHENTICATION"
        // could not conclude the first one authenticated.
        //
        // It now can, when there are credentials to check against — and the middle case is
        // kept and still capitalised, because a server with `require_password` set and an
        // empty credential map is in exactly the old posture and must still look wrong in a
        // log rather than be something somebody has to go and check.
        let auth = match (self.settings.require_password, self.settings.credentials.len()) {
            (_, held) if held > 0 => format!("password verified for {held} user(s)"),
            (true, _) => concat!(
                "PASSWORD UNVERIFIED — no credentials are configured, so any non-empty ",
                "password is accepted from any user"
            )
            .to_owned(),
            (false, _) => "NO AUTHENTICATION — every connection is accepted".to_owned(),
        };
        // The bound address is printed separately by the caller, which is the only thing
        // that knows it. Repeating the *configured* one here printed ":0" beside the real
        // port, which is worse than saying nothing.
        //
        // One format string rather than one per posture: the copies drift, and the one that
        // drifts is the one nobody reads.
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
    /// The principal a named user acts as.
    ///
    /// Public because Flight needs it. It used to reach a wrapper that passed the literal
    /// `"flight"`, under a comment saying a ticket carries a tenant and not a subject and that
    /// every user of a tenant receives the same roles --- true when written, and untrue from
    /// the day roles became per-subject, with no code changing and no test failing. The subject
    /// now travels in the ticket, which is what that comment said would have to happen.
    /// `SEC-03`.
    pub fn principal(&self, user: &str) -> Option<Principal> {
        // The roles this user holds. See `Settings::roles` for why an empty map means one
        // thing and a populated one that does not name them means another.
        let held: Vec<Role> = if self.settings.roles.is_empty() {
            vec![Role::new("reader")]
        } else {
            self.settings
                .roles
                .get(user)
                .map(|names| names.iter().map(|name| Role::new(name.clone())).collect())
                .unwrap_or_default()
        };
        Principal::authenticated(
            user,
            self.settings.tenant,
            held,
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
    pub(crate) fn record(&self, principal: &Principal, table: TableRef, action: Action, allowed: bool) {
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

        // And **correctness**, which is `SEC-01`. Until this, the check above was the whole of
        // it: presence, from a self-asserted username, with no credential store to check
        // against anywhere in the workspace.
        //
        // One refusal for both "no such user" and "wrong password", deliberately. Telling them
        // apart turns the login into a directory of who exists here, which is the first thing
        // an attacker wants and the last thing this door should answer.
        if self.settings.credentials.is_empty() {
            // Nobody has a password written down, which is a decision an operator has not
            // taken rather than one they have taken loosely. See `Settings::credentials`.
            return Ok(());
        }
        let presented = password.unwrap_or_default();
        let verified = self
            .settings
            .credentials
            .get(user)
            .is_some_and(|verifier| verifier.verifies(presented));
        if !verified {
            return Err(refusal(
                statuses_for_unauthenticated().sqlstate.as_str(),
                "password authentication failed for this user",
            ));
        }
        Ok(())
    }

    /// The statements this server defines itself, which the catalogue must not answer for it.
    ///
    /// Only the feed commands today, and only because `SHOW FEEDS` collides with the
    /// catalogue's `SHOW <setting>` shortcut. Cube and clone DDL are listed too — not because
    /// anything shadows them now, but because "which statements are ours" is one question,
    /// and answering it in two places is how the next one comes to be shadowed silently.
    fn claims(&self, sql: &str) -> bool {
        // Read past any leading comment, exactly as the dispatch does. Asked of the raw text,
        // this said *"not mine"* for a commented `SHOW FEEDS` --- and the catalogue then
        // answered it as an unknown setting. Two places that must agree on what a statement
        // is, and they now agree by reading the same thing.
        let sql = sankhya_api_pg::catalog::without_leading_comments(sql);
        crate::driver::run_session_statement(sql).is_some()
            || sql.trim().to_uppercase().starts_with("SET SNAPSHOT")
            || sankhya_snapshot::parse(sql).is_some()
            || crate::aggregations::parse(sql).is_some()
            || sankhya_feed::parse_command(sql).is_some()
            || sankhya_clone::parse_question(sql).is_some()
            || sankhya_cube_sql::parse_ddl(sql).is_some()
            || sankhya_clone::parse_ddl(sql).is_some()
    }

    fn query(
        &self,
        sql: &str,
        caller: &sankhya_api_pg::session::Caller<'_>,
    ) -> Result<QueryResult, QueryFailure> {
        let started = std::time::Instant::now();
        let outcome = self.run_statement(sql, caller);

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

    fn visible_tables(&self, caller: &sankhya_api_pg::session::Caller<'_>) -> Vec<CatalogTable> {
        self.list_visible_tables(caller.user())
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
    fn register_cubes(
        &self,
        context: &SessionContext,
        principal: &Principal,
        sql: &str,
        pin: u64,
    ) {
        let cubes = self.cubes();
        // No early return on an empty set, and the omission was not free.
        //
        // This used to give up here, which skipped `describe::register` below --- so on a
        // warehouse with no cubes, `SELECT * FROM cubes()` answered *"table function 'cubes'
        // not found"*. A client could not tell **"no cubes yet"** from **"this server does not
        // do cubes"**, which are opposite facts with opposite responses, on the one warehouse
        // where the question is most likely to be asked: a new one.
        //
        // It is the same defect as a projected query over an empty table, one level up. A
        // surface that exists only once it has something to say is a surface nobody can build
        // a picker on. Registering a catalogue whose contents are empty is not pretending ---
        // the graph functions do exactly this, deliberately, and say so.
        //
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
            let Some(scope) = self.scope_across(principal, cube.reads()) else {
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
            let snapshot = self.snapshot_across(cube.reads());
            for measure in cube.measures() {
                let key = sankhya_cube_sql::hydrated::Key {
                    cube: cube.name().to_string(),
                    measure: measure.name.clone(),
                    definition_version: cube.version(),
                    snapshot,
                    scope,
                    // What position this session reads from. See `Key::pin`: a pinned read's
                    // cells were computed at a different position, and keying them under the
                    // present version served them to the next unpinned session.
                    pin,
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
                if cube
                    .reads()
                    .iter()
                    .all(|table| self.withholds_nothing(principal, table))
                {
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
            crate::aggregations::supplied_rules(self),
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
            let snapshot = self.snapshot_across(cube.reads());
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
                    // The measure is part of the key, and this loop is why. Without it the
                    // first measure wrote each shape and every later one found `exists()` true
                    // and skipped --- so a cube's second measure was served the first's
                    // numbers under its own name.
                    measure.name.clone(),
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
                    self.snapshot_across(cube.reads()),
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
                    self.snapshot_across(cube.reads()),
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
            self.snapshot_across(cube.reads()),
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
            measure: measure.name.clone(),
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
        let arrived = crate::adopt::new_tables(self, &mut candidate);
        if moved == 0 && arrived == 0 {
            // The ordinary case: nothing has committed since the last statement, so there is
            // nothing to publish and no reason to take the write lock at all.
            return current;
        }
        let replacement = Arc::new(candidate);
        *self.servable.write() = Arc::clone(&replacement);
        replacement
    }

    /// Where this server's warehouse is, for the modules that walk it.
    #[must_use]
    pub(crate) fn warehouse_path(&self) -> &std::path::Path {
        &self.settings.warehouse
    }

    /// The published position this server reads as of.
    #[must_use]
    pub(crate) fn read_as_of(&self) -> sankhya_types::Lsn {
        self.settings.read_as_of
    }

    /// The shared log cache, so a second reader does not re-read what the first just did.
    #[must_use]
    pub(crate) fn log_cache(&self) -> &sankhya_table_delta::LogCache {
        &self.log_cache
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

    /// Hits and misses on the hydration cache.
    ///
    /// Exposed so a test can prove it **reached** the cache rather than merely produced the
    /// right answer twice. A test of what a cached entry is an answer to is worth nothing if
    /// the second query missed, and a miss is invisible from the result.
    #[must_use]
    pub fn hydration_counts(&self) -> (u64, u64) {
        self.hydrated.counts()
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

    /// The newest version among every table a cube reads.
    ///
    /// The newest rather than the oldest, and rather than the first: a cube over a join is
    /// stale the moment **any** of its inputs moves, so keying on the newest is what makes a
    /// commit to either side a cache miss. Keying on one of them would serve an answer built
    /// from the other's previous version, and nothing about that answer would look wrong.
    fn snapshot_across(&self, tables: &[String]) -> u64 {
        tables.iter().map(|table| self.snapshot_of(table)).max().unwrap_or(0)
    }

    /// What this principal may see of every table a cube reads, as one value.
    ///
    /// `None` when there is one they may not read at all --- because a cube over a join is a
    /// cube over both sides, and a caller allowed to read one of them is not allowed to read
    /// what the join makes of the pair.
    ///
    /// The scopes are folded rather than added: this is a cache key, and two different sets
    /// of scopes must not collide into one. Addition collides on the first pair that sums the
    /// same way, and the collision serves one principal's rows to another.
    pub(crate) fn scope_across(&self, principal: &Principal, tables: &[String]) -> Option<u64> {
        let mut folded: u64 = 0xcbf2_9ce4_8422_2325;
        for table in tables {
            let scope = self.scope_for(principal, table)?;
            folded ^= scope;
            folded = folded.wrapping_mul(0x0000_0100_0000_01b3);
        }
        Some(folded)
    }

    /// What this principal may see of a table, as a value.
    ///
    /// `None` when they may not read it at all.
    /// The name a client used, as the qualified `schema.table` everything else records.
    ///
    /// # Why there is a single internal form
    ///
    /// A lineage, a dependency and an authorization all outlive the statement that created
    /// them, and a bare name is unambiguous only until a second schema grows a table of that
    /// name. Resolving once, at the edge, is what keeps ambiguity somewhere it can still be
    /// reported to a person who can qualify it.
    pub(crate) fn qualify(&self, name: &str) -> Qualified {
        match crate::warehouse::resolve(&self.settings.warehouse, name) {
            crate::warehouse::Resolved::One(root) => {
                crate::warehouse::qualified_name(&self.settings.warehouse, &root)
                    .map_or(Qualified::Absent, |qualified| Qualified::One(qualified, root))
            }
            crate::warehouse::Resolved::Absent => Qualified::Absent,
            crate::warehouse::Resolved::Ambiguous(candidates) => Qualified::Ambiguous(candidates),
        }
    }

    pub(crate) fn scope_for(&self, principal: &Principal, table: &str) -> Option<u64> {
        // Either name form, because both are names a client legitimately has: the catalogue
        // prints `sales.orders` and a session registers `orders`, and a statement may use
        // whichever it was given. Matching only the bare one meant a qualified name authorized
        // against `TableRef::new("", "sales.orders")` --- a table no policy has ever granted,
        // so every qualified statement was refused as though the table did not exist.
        let matching: Vec<TableRef> = {
            let servable = self.servable.read();
            match table.split_once('.') {
                Some((schema, name)) => servable
                    .iter()
                    .filter(|entry| {
                        entry.reference.schema == schema && entry.reference.table == name
                    })
                    .map(|entry| entry.reference.clone())
                    .collect(),
                None => servable
                    .iter()
                    .filter(|entry| entry.reference.table == table)
                    .map(|entry| entry.reference.clone())
                    .collect(),
            }
        };
        // A bare name two schemas claim authorizes nothing. The query path refuses to resolve
        // it for the same reason, and granting the first match here would decide on a rule
        // nobody wrote down --- on the authorization side, which is the worse place to guess.
        if matching.len() > 1 {
            return None;
        }
        // With no servable table of that name, the reference is built from the name itself.
        // A qualified name splits into the pair a policy rule is keyed by; a bare one has no
        // schema to offer and stays as it is, which is what every rule written before schemas
        // were resolvable expects.
        let reference = matching.into_iter().next().unwrap_or_else(|| {
            table.split_once('.').map_or_else(
                || TableRef::new("", table),
                |(schema, name)| TableRef::new(schema, name),
            )
        });
        Guard::authorize(&self.policy, principal, &reference, Action::Read)
            .map(|guard| guard.scope_digest())
    }

    /// Run a `CREATE TABLE ... CLONE`.
    ///
    /// # Why the origin is authorized as a read
    ///
    /// A clone *is* a read: `ADR-0016` makes it a reference to the origin's files rather than a
    /// copy of them, so somebody who may clone a table they cannot read has read it. The check
    /// is [`Self::scope_for`] --- the same authorization the query path uses --- and its failure
    /// is the same sentence the query path gives, because saying *"you may not read `payroll`"*
    /// would confirm that `payroll` exists.
    fn run_clone_ddl(
        &self,
        statement: Result<sankhya_clone::ddl::Statement, sankhya_clone::DdlError>,
        principal: &Principal,
    ) -> Option<Result<QueryResult, QueryFailure>> {
        use sankhya_error::protocol::sqlstate;

        let statement = match statement {
            Ok(statement) => statement,
            Err(error) => {
                return Some(Err(refusal(
                    sqlstate::SYNTAX_ERROR.as_str(),
                    &error.to_string(),
                )))
            }
        };

        match statement {
            sankhya_clone::Statement::Create(create) => {
                Some(self.create_clone_table(&create, principal))
            }
            sankhya_clone::Statement::Drop { table, if_exists } => {
                self.drop_clone(&table, if_exists, principal)
            }
        }
    }

    /// Whether this principal may read a table, resolving a clone to what it references.
    ///
    /// # Why a clone's authority comes from its root
    ///
    /// `ADR-0016` makes a clone a reference to its origin's files rather than a copy, so the
    /// right to read it *is* the right to read what it references. Checking the clone's own name
    /// does not work, and the way it fails is instructive: a clone created a moment ago has no
    /// policy rule of its own, so the principal who created it could neither read it, clone it,
    /// nor drop it --- a table you can make and cannot touch.
    ///
    /// Resolved to the **root** rather than one step, because a clone of a clone references the
    /// root's files just as surely. `ancestors` refuses a lineage cycle, and a table whose
    /// ancestry cannot be resolved is refused rather than granted.
    pub(crate) fn readable(
        &self,
        principal: &Principal,
        table: &str,
        lineages: &sankhya_clone::Lineages,
    ) -> bool {
        let Ok(chain) = lineages.ancestors(table) else {
            return false;
        };
        let root = chain.last().map_or(table, String::as_str);
        if root.is_empty() {
            return false;
        }
        self.scope_for(principal, root).is_some()
    }

    /// Drop a clone, or hand the statement back because the table is not one.
    ///
    /// # Why a clone must be droppable at all
    ///
    /// Because it is now creatable. A thing a statement can make and no statement can remove
    /// accumulates, and accumulation with nobody responsible is the shape `RSK-35` describes for
    /// rehydrated copies --- each one individually reasonable, and no day on which anybody could
    /// have decided otherwise.
    ///
    /// # Why only a clone
    ///
    /// This server refuses data definition wholesale, and that refusal is right: it is a read
    /// path over a published warehouse, and writes arrive through capture or the publishing
    /// tool. Cloning is the exception the milestone introduced, so dropping a clone is the
    /// exception it owes. Everything else is handed back untouched.
    fn drop_clone(
        &self,
        table: &str,
        if_exists: bool,
        principal: &Principal,
    ) -> Option<Result<QueryResult, QueryFailure>> {
        use sankhya_error::protocol::sqlstate;

        // Resolved to the qualified form everything else records, for the reason `qualify`
        // gives: the name a client has may be bare, and a record must not be.
        let (qualified, root) = match self.qualify(table) {
            Qualified::One(name, root) => (name, Some(root)),
            Qualified::Absent | Qualified::Ambiguous(_) => (table.to_owned(), None),
        };
        let table = qualified.as_str();
        let lineages = self.lineages();
        if lineages.of(table).is_none() {
            if if_exists && root.is_none() {
                // Nothing by that name and the statement said it might not be there. Answering
                // here rather than passing it on, because the engine would refuse a statement
                // that asked for nothing.
                return Some(Ok(acknowledged("DROP TABLE")));
            }
            // Not a clone. Not ours.
            return None;
        }

        // Authorized against the **origin**, not against the clone.
        //
        // A clone is a reference to its origin's files, so the right to act on it derives from
        // the right to read what it references. Checking the clone itself does not work and the
        // way it fails is instructive: a clone created a moment ago has no policy rule of its
        // own, so the principal who made it could not drop it --- a table you can create and
        // cannot remove, which is the accumulation this drop exists to prevent.
        //
        // A clone whose lineage cannot be read names no origin, so this refuses it. That is the
        // conservative answer and the right one: a clone nobody can resolve is not one anybody
        // should be removing on the strength of a guess.
        if !self.readable(principal, table, &lineages) {
            return Some(Err(refusal(
                sqlstate::DATA_EXCEPTION.as_str(),
                &format!("there is no table `{table}` to drop"),
            )));
        }

        // The refusal `ADR-0016` exists for, at the one door it can arrive through.
        if let Err(refused) = sankhya_clone::refuse::may_drop(table, &lineages) {
            // The clones it names, as a list. `Refused::StillRead` already carries them as
            // data and this used to flatten them into the sentence --- which is `ADR-0017`
            // Decision 2's own worked example, failing.
            let named = match &refused {
                sankhya_clone::Refused::StillRead { by, .. } => by.clone(),
                _ => Vec::new(),
            };
            return Some(Err(refusal_about(
                sqlstate::DATA_EXCEPTION.as_str(),
                &refused.to_string(),
                "Drop what still reads it first, or ask `SHOW DEPENDENTS OF` before dropping \
                 anything. Every name is in the `subjects` of this refusal.",
                named,
            )));
        }

        // A clone whose lineage is recorded but whose directory cannot be resolved is refused
        // rather than reported as dropped. There is nothing to remove and something is wrong,
        // and answering "done" would leave a lineage record pointing at nothing.
        let Some(root) = root else {
            return Some(Err(refusal(
                sqlstate::DATA_EXCEPTION.as_str(),
                &format!(
                    "`{table}` is recorded as a clone and its table could not be found in \
                     this warehouse"
                ),
            )));
        };
        if let Err(error) = std::fs::remove_dir_all(&root) {
            return Some(Err(refusal(
                sqlstate::DATA_EXCEPTION.as_str(),
                &format!("the clone could not be removed: {error}"),
            )));
        }
        self.record(principal, TableRef::new("", table), Action::Delete, true);
        Some(Ok(acknowledged("DROP TABLE")))
    }

    /// Which of the warehouse's tables are clones, read from their logs.
    ///
    /// Rebuilt per statement rather than cached. A clone created by another connection a moment
    /// ago must be visible to this one, and a cache that lagged would let a drop proceed against
    /// an origin whose newest clone it had not heard of --- which is the deletion this is all
    /// gated on, arriving through a stale read.
    pub(crate) fn lineages(&self) -> sankhya_clone::Lineages {
        let mut lineages = sankhya_clone::Lineages::new();
        // Keyed by the **qualified** name, `schema.table`.
        //
        // Not the bare one, and not the directory a level up --- which is what this used, so
        // the lineage of `sales/orders` was filed under `sales` and no question about `orders`
        // ever found it.
        //
        // Qualified rather than bare because a lineage is a *record*, and it outlives the
        // moment it was written. A bare `orders` is unambiguous until a second schema grows an
        // `orders`, and on that day every clone in the warehouse would silently point at
        // whichever one the walk found first. The name a client types is resolved to this form
        // at the edge, which is the one place ambiguity can still be reported to somebody who
        // can do something about it.
        let (found, _) = crate::warehouse::discover(&self.settings.warehouse);
        for table in found {
            let Some(name) = crate::warehouse::qualified_name(&self.settings.warehouse, &table.root)
            else {
                continue;
            };
            let name = name.as_str();
            let path = table.root.clone();
            let found = sankhya_table_delta::read_actions(&path).ok().and_then(|actions| {
                let actions: Vec<sankhya_table_delta::Action> =
                    actions.into_iter().map(|(_, action)| action).collect();
                sankhya_clone::lineage_of(&actions)
            });
            // A lineage that cannot be *read* is not a table that is not a clone. It is
            // reported by the crate as an error precisely so it cannot be mistaken for one, and
            // treating it as an ordinary table here would undo that --- so it is recorded as a
            // clone of nothing resolvable, which keeps every reclamation decision about it
            // conservative.
            match found {
                Some(Ok(lineage)) => lineages.record(name, lineage),
                Some(Err(_)) => {
                    lineages.record(name, sankhya_clone::Lineage::new(String::new(), 0, 0));
                }
                None => {}
            }
        }
        lineages
    }

    /// Create a clone.
    fn create_clone_table(
        &self,
        statement: &sankhya_clone::ddl::Create,
        principal: &Principal,
    ) -> Result<QueryResult, QueryFailure> {
        use sankhya_error::protocol::sqlstate;

        let lineages = self.lineages();
        // Resolved before it is authorized, so that a name two schemas claim gets the refusal
        // that tells somebody what to do about it rather than the one that says the table does
        // not exist. Both refuse; only one of them is any use.
        //
        // Resolved the way a *read* of the same name is. A client says `orders` and means the
        // table a session registered under that name; resolving it as a directory at the
        // warehouse root meant naming something no deployment has, so a clone could only ever
        // be made of a table this server does not serve.
        let (origin, origin_root) = match self.qualify(&statement.origin) {
            Qualified::One(name, root) => (name, root),
            Qualified::Absent => {
                return Err(refusal(
                    sqlstate::DATA_EXCEPTION.as_str(),
                    &format!("there is no table `{}` to clone", statement.origin),
                ))
            }
            Qualified::Ambiguous(candidates) => {
                let named = candidates.join(", ");
                return Err(refusal_about(
                    sqlstate::DATA_EXCEPTION.as_str(),
                    &format!("`{}` names more than one table: {named}", statement.origin),
                    "Qualify it with its schema --- choosing one here would clone the wrong \
                     table silently. Both candidates are in this refusal's `subjects`.",
                    candidates,
                ));
            }
        };

        if !self.readable(principal, &origin, &lineages) {
            return Err(refusal(
                sqlstate::DATA_EXCEPTION.as_str(),
                &format!("there is no table `{}` to clone", statement.origin),
            ));
        }

        // A name in use is asked of the **warehouse**, not of the policy. A clone has no policy
        // rule of its own, so asking the policy would report every clone as absent and let a
        // second one be created over it --- discovered only when the commit refused.
        let table_root = crate::warehouse::place_beside(
            &self.settings.warehouse,
            &statement.table,
            &origin_root,
        )
        .map_err(|misplaced| {
            refusal(sqlstate::DATA_EXCEPTION.as_str(), &misplaced.to_string())
        })?;
        if table_root.exists() {
            return Err(refusal(
                sqlstate::DATA_EXCEPTION.as_str(),
                &format!(
                    "the table `{}` already exists. A clone is created, never replaced: \
                     replacing one would drop a table somebody may be the only reader of",
                    statement.table
                ),
            ));
        }

        let facts = self.origin_facts(&origin_root, statement.version)?;
        let version = statement.version.unwrap_or(facts.latest_version);

        let request = sankhya_clone::refuse::Request {
            table: statement.table.clone(),
            // One tenant per warehouse today, so these agree by construction. Passed through
            // rather than skipped, so the refusal exists and is tested before the day they
            // stop agreeing --- which is when nobody will think to add it.
            tenant: self.settings.tenant.to_string(),
            origin: origin.clone(),
            origin_tenant: self.settings.tenant.to_string(),
            version,
        };
        sankhya_clone::refuse::may_clone(&request, &facts).map_err(|refused| {
            refusal(sqlstate::DATA_EXCEPTION.as_str(), &refused.to_string())
        })?;

        self.write_clone(&statement.table, &table_root, &origin, version, &origin_root)?;
        self.record(principal, TableRef::new("", &origin), Action::Read, true);
        Ok(acknowledged("CREATE TABLE"))
    }

    /// What the origin's log says, as `may_clone` needs it.
    ///
    /// # What `earliest_retained_version` means here, exactly
    ///
    /// **The oldest version the log still contains**, which is a bound rather than a promise:
    /// a version's commit can be present while the data files it names have been retired. So
    /// the requested version is *also* checked file by file, and the log bound is what the
    /// refusal quotes when that check fails.
    ///
    /// Computing the true oldest resolvable version would mean replaying to every version in
    /// turn, which is quadratic in the log. The bound is cheap, the file check is exact, and
    /// together they refuse correctly and explain approximately --- which is the right way
    /// round.
    fn origin_facts(
        &self,
        origin_root: &std::path::Path,
        wanted: Option<u64>,
    ) -> Result<sankhya_clone::refuse::Origin, QueryFailure> {
        use sankhya_error::protocol::sqlstate;

        let live = sankhya_table_delta::live_files(origin_root).map_err(|error| {
            refusal(
                sqlstate::DATA_EXCEPTION.as_str(),
                &format!("the table to clone could not be read: {error}"),
            )
        })?;
        let latest_version = live.version.unwrap_or_default();
        let oldest_in_log = sankhya_table_delta::commits(origin_root)
            .ok()
            .and_then(|commits| commits.first().map(|(version, _)| *version))
            .unwrap_or_default();

        // The exact half. A version whose commit survives and whose files do not is the case
        // the log bound cannot see, and it is the one that produces an empty table wearing the
        // name of a full one.
        let resolvable = match wanted {
            None => true,
            Some(version) if version > latest_version => true,
            Some(version) => sankhya_table_delta::live_files_at(origin_root, version)
                .map(|at| at.files.iter().all(|file| origin_root.join(&file.path).exists()))
                .unwrap_or(false),
        };

        Ok(sankhya_clone::refuse::Origin {
            latest_version,
            earliest_retained_version: if resolvable {
                oldest_in_log
            } else {
                wanted.unwrap_or_default().saturating_add(1).max(oldest_in_log)
            },
            // Nothing plans a purge yet --- `M9`'s destructive path stays disabled until
            // `M11` --- so this is false by construction rather than by omission. The refusal
            // is built and tested against the day it is not.
            purge_in_flight: false,
            // Schema evolution is not an operation this server has, so likewise.
            schema_evolving: false,
        })
    }

    /// Commit the clone's table: lineage properties, and no files.
    fn write_clone(
        &self,
        table: &str,
        table_root: &std::path::Path,
        origin: &str,
        version: u64,
        origin_root: &std::path::Path,
    ) -> Result<(), QueryFailure> {
        use sankhya_error::protocol::sqlstate;

        let schema = sankhya_table_delta::read_actions(origin_root)
            .ok()
            .and_then(|actions| {
                actions.into_iter().rev().find_map(|(_, action)| match action {
                    sankhya_table_delta::Action::Metadata(metadata) => {
                        Some(metadata.schema_string)
                    }
                    _ => None,
                })
            })
            .ok_or_else(|| {
                refusal(
                    sqlstate::DATA_EXCEPTION.as_str(),
                    "the table to clone declares no schema",
                )
            })?;

        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|since| i64::try_from(since.as_micros()).ok())
            .unwrap_or(0);
        let lineage = sankhya_clone::Lineage::new(origin, version, at);

        // Through the one official writer, not from here. `check-writers` caught the first
        // draft of this committing directly and it was right to: the point of that rule is
        // that table state has one write path, and a clone's creating commit is table state.
        // Widening the allowlist would have been the easy answer and the wrong one.
        sankhya_publish::Publication::external(table_root, table)
            .create_clone(&schema, &lineage.to_properties())
            .map_err(|error| {
                refusal(
                    sqlstate::DATA_EXCEPTION.as_str(),
                    &format!("the clone could not be committed: {error}"),
                )
            })
    }


    /// Everything `query` does, without the measuring.
    ///
    /// Split out so that the counter and the histogram are recorded on **every** path out of
    /// the statement, including the two refusals that never reach the query path. A
    /// duration histogram fed only by the successful path describes a system that never
    /// fails, and the tail an operator goes looking for is made of failures.
    fn run_statement(
        &self,
        sql: &str,
        caller: &sankhya_api_pg::session::Caller<'_>,
    ) -> Result<QueryResult, QueryFailure> {
        let user = caller.user();
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
                subjects: Vec::new(),
            });
        }

        // The user this connection authenticated as, not a constant.
        //
        // `principal("query")` was a literal, and so were `principal("catalogue")` and
        // `principal("flight")`. The authenticated user was read by `authenticate`, refused if
        // empty on the grounds that an unattributable connection cannot be audited --- and then
        // thrown away, because nothing downstream had a parameter to carry it in.
        //
        // `FR-SEC-02` asks that a principal be carried unchanged through planning, execution
        // and audit. Until now it was carried unchanged and it was the wrong one.
        let Some(principal) = self.principal(user) else {
            return Err(refusal(
                statuses_for_unauthenticated().sqlstate.as_str(),
                "no principal is established for this connection",
            ));
        };

        // Every statement this server implements itself is recognised by matching the start of
        // the text, because none of them is SQL. Matched against the raw text, a single
        // leading `--` comment made the server fail to recognise its own statement --- and
        // every script this repository ships as an example comments its statements. The
        // *engine* still receives `sql` unchanged; this is a recogniser's view, not a rewrite.
        let dispatch = sankhya_api_pg::catalog::without_leading_comments(sql);

        // Cube DDL, before the engine is asked anything.
        //
        // `CREATE CUBE` is not SQL, so `sqlparser` rejects it before any DataFusion hook can
        // see it. It has to be recognised here or not at all. `parse_ddl` returns `None` for
        // everything that is not cube DDL, which is every other statement in the language.
        //
        // After admission and after the principal, because a cube is created *by* somebody
        // and against tables they must be allowed to read; before the session, because none
        // of what `session_for` builds is any use to a statement that reads no data.
        if let Some(statement) = sankhya_cube_sql::parse_ddl(dispatch) {
            return crate::cubes::run_ddl(self, statement, &principal);
        }

        // Clone DDL, for the same reason and at the same point. `CREATE TABLE x CLONE y` is
        // not SQL either, and `parse_ddl` returns `None` for every statement that is not one
        // --- including every ordinary `CREATE TABLE`, which must reach the engine untouched.
        // Feed commands, before the engine sees them. `SHOW FEEDS` and `RESUME FEED x` are
        // not SQL, and `parse` returns `None` for everything that is not one --- including
        // `SHOW server_version_num`, which a catalogue-browsing client sends on connection.
        // The statements a *driver* sends around a query, answered rather than refused.
        //
        // Every connection pool and ORM opens with `SET extra_float_digits`, wraps work in
        // `BEGIN`/`COMMIT`, and returns a connection with `DISCARD ALL`. Refusing them made
        // this door unusable by the clients it exists for --- and `SET` was refused as
        // `XX000`, a *fatal server configuration error*, which makes a pool discard the
        // connection and try again forever.
        //
        // Answered honestly rather than pretended: this is a read path, so a transaction
        // spans one statement and `BEGIN`/`COMMIT` are already true. What must never happen is
        // silently accepting a statement whose meaning we do not implement --- so `ROLLBACK`
        // is refused, because a client that rolls back and is told it worked has been lied to
        // about the one thing it asked.
        // `SET SNAPSHOT` first, because it is the one setting whose *value* must be checked
        // --- and checked here rather than at the next statement, which is where somebody would
        // otherwise learn their snapshot does not exist.
        if let Some(answer) = crate::snapshots::check_setting(self, dispatch) {
            return answer;
        }

        // Snapshot and version statements **before** the generic session handler, because that
        // handler accepts any `SET` as a no-op --- and `SET VERSION OF <table> = <n>` is a
        // `SET`. Ordered the other way it was swallowed silently, which is the exact failure
        // `ADR-0019` Decision 6 names: a caller who asked to read a version, served the
        // present, with no symptom at all.
        // Aggregation DDL, before the engine and before the generic session handler. `CREATE
        // AGGREGATION` is not SQL, so `sqlparser` rejects it before any hook could see it.
        if let Some(statement) = crate::aggregations::parse(dispatch) {
            return crate::aggregations::run(self, statement, &principal);
        }

        if let Some(statement) = sankhya_snapshot::parse(dispatch) {
            return crate::snapshots::run_statement(self, statement, &principal);
        }

        if let Some(answer) = crate::driver::run_session_statement(dispatch) {
            return answer;
        }

        if let Some(command) = sankhya_feed::parse_command(dispatch) {
            return crate::feeds::run_command(self, &principal, command);
        }

        // The two questions about a clone, for the same reason and at the same point.
        // `parse_question` returns `None` for every other `SHOW`, including the several a
        // catalogue-browsing client sends on connection.
        if let Some(question) = sankhya_clone::parse_question(dispatch) {
            return crate::clones::answer(self, question, &principal);
        }

        if let Some(statement) = sankhya_clone::parse_ddl(dispatch) {
            // `None` means the statement turned out not to be this server's business after all
            // --- a `DROP TABLE` of something that is not a clone --- and it goes on to the
            // engine, whose "this is a read path" refusal answers it in its own words. A
            // pre-filter that answered it here would have replaced a good refusal with a
            // reimplementation of one.
            if let Some(answer) = self.run_clone_ddl(statement, &principal) {
                return answer;
            }
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
        // As of a named snapshot, when the session asked for one.
        //
        // Every table it names is resolved at the version it recorded, so four tables read at
        // four moments become four tables read at one. A table the snapshot does not name is
        // **left out of the session entirely**, so a statement naming it fails to resolve
        // rather than being answered from the present --- `ADR-0019` Decision 2, enforced by
        // absence rather than by a check somebody has to remember.
        let servable = match self.as_of_snapshot(caller)? {
            None => servable,
            Some(pinned) => pinned,
        };
        let (context, registered, contested) =
            session_and_contested(&principal, &self.policy, &servable)?;
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
        self.register_cubes(&context, &principal, sql, caller.position_digest());
        crate::aggregations::register(self, &context);
        crate::cubes::register_derived(self, &context, &principal);

        // `block_in_place` rather than a bare `block_on`. This method is called from inside
        // a Tokio task — the connection's — and blocking that thread directly panics,
        // because the thread is driving the runtime. `block_in_place` hands the runtime's
        // work to another worker first, which is why the server needs a multi-threaded
        // runtime and would deadlock on a current-thread one.
        let outcome = tokio::task::block_in_place(|| {
            self.runtime
                .block_on(run(&context, sql, Self::MAX_RESULT_ROWS))
        });

        // A statement that failed to plan while naming a contested table said something true
        // and useless: "table not found", about a table that is found twice. Saying which two
        // is the difference between a typo somebody hunts for and four characters they type.
        let outcome = outcome.map_err(|failure| explain_contested(failure, &contested));

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
    fn list_visible_tables(&self, user: &str) -> Vec<CatalogTable> {
        // Filtered by policy, because a catalogue that listed tables the caller cannot read
        // would disclose their existence — the leak the policy component refuses to permit
        // anywhere else, arriving through the back door of a schema browser.
        let Some(principal) = self.principal(user) else {
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
pub(crate) fn acknowledged(tag: &str) -> QueryResult {
    QueryResult { fields: Vec::new(), rows: Vec::new(), tag: tag.to_string() }
}

/// Turn *"table not found"* about a contested name into a refusal that names the candidates.
///
/// Only the **detail** changes. The statement is still refused and the SQLSTATE is untouched,
/// because a client dispatches on that and a message is not an API. What changes is whether the
/// person reading it can act: `orders` resolving to nothing when two schemas hold one is a
/// four-character fix that reads, without this, as a table that has gone missing.
fn explain_contested(
    failure: QueryFailure,
    contested: &std::collections::BTreeMap<String, Vec<String>>,
) -> QueryFailure {
    // Matched on the message rather than on the statement, because the planner knows which
    // name it could not resolve and a second parser here would sometimes disagree with it.
    let named = contested.iter().find(|(bare, _)| {
        failure
            .message
            .contains(&format!("'{}'", bare.as_str()))
            || failure.message.contains(&format!(".{}'", bare.as_str()))
    });
    let Some((bare, candidates)) = named else {
        return failure;
    };
    QueryFailure {
        detail: Some(format!(
            "`{bare}` names more than one table in this warehouse: {}. It is registered under \
             neither, because answering with one of them would hand back a table you had no \
             way to identify. Qualify it with its schema.",
            candidates.join(", ")
        )),
        // The candidates as data, so a client can offer them rather than parse them out of
        // the sentence above.
        subjects: candidates.clone(),
        ..failure
    }
}

pub(crate) fn refusal(sqlstate: &str, message: &str) -> QueryFailure {
    QueryFailure {
        sqlstate: sqlstate.to_string(),
        message: message.to_string(),
        detail: None,
        subjects: Vec::new(),
    }
}

/// A refusal that **names** what it is about, and says what to do.
///
/// # Why the names travel separately from the sentence
///
/// `ADR-0017` Decision 2. A refusal here names things --- the clones that would break, the
/// feed that is not declared, the two tables a name could mean --- and a client that wants to
/// act on them should not have to parse the sentence. The moment it does, the sentence is an
/// API: nobody may reword it, and every improvement to the message breaks somebody.
///
/// `subjects` is the field that is cheap now and expensive later, which is why it is filled in
/// at every site that has names rather than added when something asks.
pub(crate) fn refusal_about(
    sqlstate: &str,
    message: &str,
    remediation: &str,
    subjects: Vec<String>,
) -> QueryFailure {
    QueryFailure {
        sqlstate: sqlstate.to_string(),
        message: message.to_string(),
        detail: Some(remediation.to_string()),
        subjects,
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
    // Both doors' certificates, loaded once. A refusal here stops startup: an operator who
    // configured a certificate and gets a server listening in the clear has been told the
    // opposite of the truth by a process that exited zero.
    let encryption = match &settings.transport_security {
        None => None,
        Some(security) => Some(load_transport_security(security).map_err(|refused| {
            std::io::Error::other(format!("transport security could not be configured: {refused}"))
        })?),
    };
    let listener = sankhya_api_pg::listener::PgListener::bind(&settings.listen).await?;
    let listener = match &encryption {
        None => listener,
        Some(doors) => listener.encrypted(if settings.transport_security
            .as_ref()
            .is_some_and(|security| security.require)
        {
            sankhya_api_pg::Encryption::Required(doors.wire.clone())
        } else {
            sankhya_api_pg::Encryption::Offered(doors.wire.clone())
        }),
    };
    let warehouse = settings.warehouse.clone();
    let (server, cube_complaints) = Server::with_tables(settings, policy, tables, servable)
        .adopting_cubes(&warehouse);
    let server = server.serving_encrypted(encryption);
    // Cube complaints join the table ones rather than getting a channel of their own. They
    // are the same kind of news --- something in this warehouse could not be served --- and
    // an operator scanning startup output should not have to know there are two lists.
    let complaints: Vec<String> = complaints.into_iter().chain(cube_complaints).collect();
    Ok((Arc::new(server), listener, complaints))
}

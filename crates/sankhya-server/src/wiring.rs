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

use sankhya_api_pg::catalog::{CatalogColumn, CatalogTable};
use sankhya_api_pg::message::oid;
use sankhya_api_pg::session::{Handler, QueryFailure, QueryResult};
use sankhya_audit::chain::{Chain, Entry, RecordedDecision};
use sankhya_authz::policy::{Action, PolicySet, TableRef};
use sankhya_authz::principal::{Authentication, Principal, Role, TenantId};
use sankhya_error::protocol::statuses_for_unauthenticated;
use sankhya_governor::quota::{Quota, Quotas};
use std::sync::Arc;

/// How the server was configured.
#[derive(Clone, Debug)]
pub struct Settings {
    /// Where the wire-protocol front door listens.
    pub listen: String,
    /// The tenant every connection belongs to, until federated identity is wired in.
    pub tenant: TenantId,
    /// Whether a password is required.
    ///
    /// A setting rather than a constant because a development sandbox needs to run without
    /// one --- and because making it explicit means the log can say which it is, so nobody
    /// discovers by accident that their server is open.
    pub require_password: bool,
}

/// Everything the server owns.
#[derive(Debug)]
pub struct Server {
    settings: Settings,
    policy: PolicySet,
    quotas: Quotas,
    audit: parking_lot::Mutex<Chain>,
    tables: Vec<CatalogTable>,
    clock: parking_lot::Mutex<i64>,
}

impl Server {
    /// Assemble a server.
    #[must_use]
    pub fn new(settings: Settings, policy: PolicySet, tables: Vec<CatalogTable>) -> Self {
        let mut quotas = Quotas::new();
        quotas.set(settings.tenant, Quota::generous());
        Self {
            settings,
            policy,
            quotas,
            audit: parking_lot::Mutex::new(Chain::new()),
            tables,
            clock: parking_lot::Mutex::new(0),
        }
    }

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
        format!(
            "listening on {}, tenant {}, {auth}, {} policy rule(s), {} table(s) known",
            self.settings.listen,
            self.settings.tenant,
            self.policy.len(),
            self.tables.len()
        )
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

        // Audited before the outcome is known, and audited whatever the outcome is. A log
        // that records only what succeeded cannot show an attempt to reach something
        // forbidden, which is the pattern an investigation looks for.
        if let Some(principal) = self.principal("query") {
            self.record(
                &principal,
                TableRef::new("", statement_shape(sql)),
                Action::Read,
                false,
            );
        }

        // There is no engine behind this yet, and saying so is better than any of the
        // alternatives. An empty result would look like a table with no rows; a plausible
        // zero would look like an answer.
        Err(QueryFailure {
            sqlstate: "0A000".to_string(),
            message: format!(
                "this server answers catalogue queries and does not yet execute statements: \
                 {sql}"
            ),
            detail: Some(
                "The read path exists and is tested; it is not connected to this front door \
                 yet. Catalogue queries — version(), the schema and table lists, settings — \
                 are answered."
                    .to_string(),
            ),
        })
    }

    fn visible_tables(&self) -> Vec<CatalogTable> {
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

    fn server_version(&self) -> String {
        env!("CARGO_PKG_VERSION").to_string()
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

/// The tables a fresh server knows about.
///
/// A placeholder standing in for the catalogue the coordinator will own. It exists so the
/// front door has something true to say, and it is small enough that nobody will mistake it
/// for the real thing.
#[must_use]
pub fn example_tables() -> Vec<CatalogTable> {
    vec![CatalogTable {
        schema: "public".to_string(),
        name: "example".to_string(),
        columns: vec![
            CatalogColumn {
                name: "id".to_string(),
                type_name: "int8".to_string(),
                type_oid: oid::INT8,
                nullable: false,
            },
            CatalogColumn {
                name: "label".to_string(),
                type_name: "text".to_string(),
                type_oid: oid::TEXT,
                nullable: true,
            },
        ],
    }]
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
pub async fn start(
    settings: Settings,
) -> std::io::Result<(Arc<Server>, sankhya_api_pg::listener::PgListener)> {
    let tables = example_tables();
    let policy = permissive_policy(&settings.tenant, &tables);
    let listener = sankhya_api_pg::listener::PgListener::bind(&settings.listen).await?;
    let server = Arc::new(Server::new(settings, policy, tables));
    Ok((server, listener))
}

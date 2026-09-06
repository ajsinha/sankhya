//! Serving Arrow Flight SQL from this server.
//!
//! # Why this file is small and the protocol is elsewhere
//!
//! `sankhya-api-flight` already implements the protocol: ticket issue, tenant check on
//! redemption, expiry, and a `do_get` that streams batches as they arrive. It was complete and
//! tested and **nothing served it** --- `GUIDE.md` §7a documented a bulk plane a client had
//! nowhere to connect to.
//!
//! What was missing is the four questions the protocol asks of a server, and this answers them.
//!
//! # The same authorization, not a second one
//!
//! `plan` and `execute` build a session through `session_for`, exactly as the wire protocol's
//! statement path does. The policy is applied by the same `SecuredTable` wrappers, so a Flight
//! client sees precisely the rows a PostgreSQL client with the same identity would.
//!
//! That is deliberate and it is the reason this is thin. A second surface with its own
//! authorization is a second place the rule lives, and two implementations of one rule
//! eventually disagree --- the interesting question being which one is right, at a moment when
//! somebody is reading data they should not.
//!
//! # Identity travels as far as the ticket does, and no further
//!
//! `tenant_of` is handed the request metadata and refuses a request that does not name a user
//! --- an unattributable request cannot be audited, and an audit that cannot say who acted is
//! not one. But `plan` and `execute` receive a **tenant**, not a subject, because that is what
//! a ticket carries.
//!
//! Today that loses nothing: `Server::principal` gives every user of a tenant the same roles,
//! so a session built for the tenant is exactly the session any of its users would get. It is
//! recorded here because it stops being true the moment identity federates --- at which point
//! the subject has to travel in the ticket, and a reader of this file should meet that fact
//! before writing the code that assumes otherwise rather than after.
//!
//! # Why `execute` re-plans instead of holding a plan
//!
//! A ticket carries the statement and the snapshot, not a plan. Holding plans between
//! `GetFlightInfo` and `DoGet` would mean server-side state with a lifetime nobody bounded ---
//! a client that never redeems its ticket leaks one. Re-planning is cheap next to scanning, and
//! the snapshot in the ticket is what makes the second plan see the same data as the first.

use crate::execute::session_for;
use crate::wiring::Server;
use sankhya_api_pg::session::Handler;
use datafusion::physical_plan::SendableRecordBatchStream;
use sankhya_api_flight::{Caller, Queries, Ticket};
use std::sync::Arc;
use tonic::{Status, metadata::MetadataMap};

/// The metadata key naming who is asking.
///
/// Flight has no `user` parameter the way the wire protocol's startup message does, so the
/// identity comes from request metadata. One key, named here, rather than several accepted
/// spellings: a surface that takes `user` or `username` or `x-user` is one where a client can
/// be authenticated by accident.
pub const USER_KEY: &str = "sankhya-user";

/// The metadata key carrying the caller's password.
///
/// # Why this exists, and what it replaces
///
/// Nothing. The columnar door read [`USER_KEY`], checked it was non-empty, and served that
/// user's session --- **with no credential of any kind**. The wire door refuses a connection
/// when `server.require_password` is set and no password arrives, and then verifies what did
/// arrive against `server.credentials`. This door did neither, and it is enabled by default.
///
/// So a caller who could reach the port was served as any user named in `server.roles`,
/// without a password. Worse, [`Server::principal`] stamps the record with
/// `Authentication::Password` when passwords are required --- so the audit would have said
/// *authenticated by password* about a caller who presented none.
///
/// This is the "two doors, one rule" failure the wire door's own refusals were written
/// against, arriving on the door nobody re-read. The rule is not re-implemented here: the
/// credential is handed to the **same** `Handler::authenticate` the wire protocol calls, so
/// the two doors cannot drift.
pub const PASSWORD_KEY: &str = "sankhya-password";

/// This server, answering the questions Flight asks.
pub struct Flying {
    server: Arc<Server>,
}

impl Flying {
    /// Serve Flight from this server.
    #[must_use]
    pub const fn new(server: Arc<Server>) -> Self {
        Self { server }
    }

    /// The user named in the metadata, or a refusal.
    fn user_of(metadata: &MetadataMap) -> Result<String, Status> {
        let named = metadata
            .get(USER_KEY)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .trim()
            .to_string();
        if named.is_empty() {
            // Refused rather than defaulted, for the same reason the wire protocol refuses a
            // connection with no user: an unattributable request cannot be audited, and an
            // audit that cannot name who acted is not an audit.
            return Err(Status::unauthenticated(format!(
                "no `{USER_KEY}` was supplied; an unattributable request cannot be audited"
            )));
        }
        Ok(named)
    }

    /// A session carrying exactly what **this caller** may read.
    ///
    /// Built through `session_for`, the same function the wire protocol's statement path uses,
    /// so the policy is applied by the same `SecuredTable` wrappers. A Flight client sees
    /// precisely the rows a PostgreSQL client connected as the same user would --- because
    /// there is one implementation of the rule rather than two that eventually disagree.
    ///
    /// It used to take no user at all. The name was read from the metadata, checked non-empty,
    /// and dropped on the floor; every request then ran as the literal subject `"flight"`,
    /// whose roles came out of the same default branch as any unknown name's. A user an
    /// operator had deliberately left out of `server.roles` connected here and read. `SEC-03`.
    /// Verify the caller through the wire door's own credential check.
    ///
    /// Not a second implementation. `Handler::authenticate` is what the PostgreSQL door calls,
    /// and it holds the whole of the policy: the refusal when a password is required and
    /// absent, the verifier lookup, and the single refusal for both *no such user* and *wrong
    /// password* --- which is deliberate, because telling those apart turns a login into a
    /// directory of who exists here.
    ///
    /// The refusal is deliberately uniform for the same reason: a caller learns that they were
    /// not authenticated, and nothing about why.
    fn authenticated(&self, user: &str, metadata: &MetadataMap) -> Result<(), Status> {
        let password = metadata
            .get(PASSWORD_KEY)
            .and_then(|value| value.to_str().ok())
            .map(str::as_bytes);
        let parameters = [("user".to_string(), user.to_string())];
        Handler::authenticate(self.server.as_ref(), &parameters, password)
            .map_err(|_| Status::unauthenticated("authentication failed"))
    }

    fn session_for(&self, user: &str) -> Result<datafusion::prelude::SessionContext, Status> {
        let principal = self
            .server
            .principal(user)
            .ok_or_else(|| Status::unauthenticated("this user cannot be authenticated"))?;
        let servable = self.server.servable_now();
        let (context, registered) = session_for(&principal, self.server.policy_set(), &servable)
            .map_err(|failure| Status::permission_denied(failure.message))?;
        if registered == 0 && !servable.is_empty() {
            return Err(Status::permission_denied(
                "this principal may not read any table",
            ));
        }
        Ok(context)
    }
}

#[tonic::async_trait]
impl Queries for Flying {
    fn caller_of(&self, request_metadata: &MetadataMap) -> Result<Caller, Status> {
        // The tenant is still this server's, because a deployment serves one. The *subject* is
        // the caller's, which is the half that used to be read and thrown away.
        let user = Self::user_of(request_metadata)?;
        // Every request passes through here, which is why the check is here rather than in
        // `plan` and `execute` separately --- two call sites is how one of them comes to be
        // missed, which is the defect this whole change is about.
        self.authenticated(&user, request_metadata)?;
        Ok(Caller::new(self.server.tenant(), user))
    }

    async fn plan(&self, caller: &Caller, statement: &str) -> Result<u64, Status> {
        // Planned to prove it *can* be, and to refuse here rather than at redemption. A ticket
        // issued for a statement that cannot be planned is a promise the server will break, and
        // it breaks it in `do_get`, where a client has already committed to streaming.
        let context = self.session_for(&caller.subject)?;
        context
            .state()
            .create_logical_plan(statement)
            .await
            // Redacted here too, and this is the reason a shared function exists: the wire
            // protocol's translation was the only place that cut the planner's column list,
            // so the same statement leaked over Flight and not over PostgreSQL. `SEC-16`.
            .map_err(|error| {
                Status::invalid_argument(crate::execute::without_the_column_list(
                    &error.to_string(),
                ))
            })?;
        Ok(self.server.newest_snapshot())
    }

    async fn execute(&self, ticket: &Ticket) -> Result<SendableRecordBatchStream, Status> {
        // The session is rebuilt as the subject **named in the ticket**, not as whoever is
        // presenting it. `admit` has already refused a mismatch, so the two agree --- and
        // reading it from the ticket is what keeps them agreeing if `admit` is ever relaxed.
        let context = self.session_for(ticket.subject())?;
        let frame = context
            .sql(ticket.statement())
            .await
            // Redacted here too, and this is the reason a shared function exists: the wire
            // protocol's translation was the only place that cut the planner's column list,
            // so the same statement leaked over Flight and not over PostgreSQL. `SEC-16`.
            .map_err(|error| {
                Status::invalid_argument(crate::execute::without_the_column_list(
                    &error.to_string(),
                ))
            })?;
        // A stream, never a collection. `FR-API-07` forbids materialising a result
        // server-side, and this is the line where that is either honoured or quietly broken:
        // `frame.collect()` would compile, pass every test, and turn the bulk plane into the
        // thing it exists to replace.
        frame
            .execute_stream()
            .await
            .map_err(|error| {
                Status::internal(crate::execute::without_the_column_list(&error.to_string()))
            })
    }

    fn now(&self) -> i64 {
        i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|since| since.as_micros())
                .unwrap_or_default(),
        )
        .unwrap_or(i64::MAX)
    }
}

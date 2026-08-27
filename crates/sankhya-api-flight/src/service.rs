//! The Flight SQL service: plan a query, issue a ticket, stream the result.
//!
//! # What this exists to avoid
//!
//! The wire-protocol front door is a **row** protocol, so the last step of every query takes
//! columnar batches apart one value at a time so the receiver can put them back together.
//! For an interactive query that costs nothing worth measuring. For a bulk extract it is the
//! whole cost --- the data was columnar on disk, columnar in memory, columnar through every
//! operator, and is disassembled at the last possible moment.
//!
//! Here it is not. A batch is encoded in Arrow's IPC format and sent, and the client's
//! buffers are the same shape as the server's.
//!
//! # Nothing is materialised
//!
//! `FR-API-07` forbids a result set being built server-side. DataFusion produces a stream of
//! batches and Flight consumes a stream of batches, so they compose directly with no
//! intermediate buffer: a batch is encoded as it is produced and the memory for it is
//! released as soon as it is sent.
//!
//! The consequence has to be stated because it is a behaviour change: **an error can arrive
//! mid-stream.** A row protocol sends its error before the first row or not at all. This one
//! may have sent a gigabyte before a decode failure on the last file, and a client must
//! handle a stream that ends in an error rather than ending.
//!
//! Ending quietly would be worse than either: a truncated stream that closes cleanly is
//! indistinguishable from a complete one, which is the shape of wrong answer this whole
//! system is organised against.

use crate::ticket::{Refused, Ticket};
use arrow_flight::encode::FlightDataEncoderBuilder;
use arrow_flight::flight_service_server::FlightService;
use arrow_flight::{
    Action, ActionType, Criteria, Empty, FlightData, FlightDescriptor, FlightInfo,
    HandshakeRequest, HandshakeResponse, PollInfo, PutResult, SchemaResult, Ticket as FlightTicket,
};
use datafusion::execution::SendableRecordBatchStream;
use futures::{stream::BoxStream, StreamExt, TryStreamExt};
use sankhya_authz::principal::TenantId;
use std::pin::Pin;
use std::sync::Arc;
use tonic::{Request, Response, Status, Streaming};

/// How long a ticket stays redeemable, in microseconds.
///
/// A ticket names a snapshot and a snapshot's files are eventually retired, so an unbounded
/// ticket is a lease nobody granted. Five minutes is long enough for a client to redeem what
/// it just asked for and short enough that maintenance is not blocked by a forgotten one.
pub const TICKET_LIFETIME_MICROS: i64 = 5 * 60 * 1_000_000;

/// What the service needs from the rest of the system.
///
/// A trait so the Flight surface can be tested by driving it, with no engine behind it ---
/// which is what makes the tests in this crate possible without a warehouse.
#[tonic::async_trait]
pub trait Queries: Send + Sync + 'static {
    /// Which tenant a request belongs to.
    ///
    /// Established from the transport's credentials. Returning an error refuses the call,
    /// and there is deliberately no default tenant: an unattributable request cannot be
    /// audited, and a request nobody can attribute is one nobody can refuse either.
    fn tenant_of(
        &self,
        request_metadata: &tonic::metadata::MetadataMap,
    ) -> Result<TenantId, Status>;

    /// Authorize and plan a statement, returning the snapshot it was planned against.
    ///
    /// Called once, at `GetFlightInfo`. The result is what the ticket carries.
    async fn plan(&self, tenant: &TenantId, statement: &str) -> Result<u64, Status>;

    /// Execute a previously planned statement.
    ///
    /// Not re-authorized: the decision was made when the ticket was issued, and repeating it
    /// here would either be a second decision the caller did not ask for or --- if this path
    /// were laxer --- none at all.
    async fn execute(&self, ticket: &Ticket) -> Result<SendableRecordBatchStream, Status>;

    /// The current time, in microseconds from the epoch.
    ///
    /// Supplied rather than read, so ticket expiry can be tested without waiting and a
    /// decision can be replayed exactly.
    fn now(&self) -> i64;
}

/// The Flight service.
pub struct SankhyaFlight<Q: Queries> {
    queries: Arc<Q>,
}

impl<Q: Queries> SankhyaFlight<Q> {
    /// A service over these queries.
    pub fn new(queries: Arc<Q>) -> Self {
        Self { queries }
    }

    /// Turn a refusal into the status a client should act on.
    fn refusal(refused: Refused) -> Status {
        match refused {
            // Permission denied rather than not-found: the ticket is real and this caller
            // may not use it. Not-found would suggest retrying with a fresh one, which will
            // fail the same way.
            Refused::WrongTenant => Status::permission_denied(refused.to_string()),
            // Deadline-exceeded rather than invalid-argument: the ticket was valid and time
            // passed, so re-planning is the fix and a client that distinguishes the two will
            // do that automatically.
            Refused::Expired { .. } => Status::deadline_exceeded(refused.to_string()),
        }
    }
}

#[tonic::async_trait]
impl<Q: Queries> FlightService for SankhyaFlight<Q> {
    type HandshakeStream = BoxStream<'static, Result<HandshakeResponse, Status>>;
    type ListFlightsStream = BoxStream<'static, Result<FlightInfo, Status>>;
    type DoGetStream = BoxStream<'static, Result<FlightData, Status>>;
    type DoPutStream = BoxStream<'static, Result<PutResult, Status>>;
    type DoActionStream = BoxStream<'static, Result<arrow_flight::Result, Status>>;
    type ListActionsStream = BoxStream<'static, Result<ActionType, Status>>;
    type DoExchangeStream = BoxStream<'static, Result<FlightData, Status>>;

    async fn handshake(
        &self,
        _request: Request<Streaming<HandshakeRequest>>,
    ) -> Result<Response<Self::HandshakeStream>, Status> {
        // Authentication is the transport's business — mutual TLS, or a token in the
        // metadata. A handshake that established a second identity here would be a second
        // place the caller's identity is decided, and the two would drift.
        Ok(Response::new(futures::stream::empty().boxed()))
    }

    /// Plan a statement and return a ticket for its data.
    ///
    /// The authorization happens here and **only** here. The descriptor's command is the SQL.
    async fn get_flight_info(
        &self,
        request: Request<FlightDescriptor>,
    ) -> Result<Response<FlightInfo>, Status> {
        let tenant = self.queries.tenant_of(request.metadata())?;
        let descriptor = request.into_inner();

        let statement = std::str::from_utf8(&descriptor.cmd)
            .map_err(|_| Status::invalid_argument("the command is not valid UTF-8"))?
            .to_string();
        if statement.trim().is_empty() {
            return Err(Status::invalid_argument("no statement was supplied"));
        }

        let snapshot = self.queries.plan(&tenant, &statement).await?;
        let ticket = Ticket::issue(
            tenant,
            statement,
            snapshot,
            self.queries.now(),
            TICKET_LIFETIME_MICROS,
        );

        // No schema and no row count are advertised. Both would require running the query,
        // and `FR-API-07` forbids materialising a result server-side — so a figure here
        // would either be a guess presented as fact or the very buffering it prohibits.
        let info = FlightInfo::new()
            .with_descriptor(descriptor)
            .with_endpoint(
                arrow_flight::FlightEndpoint::new().with_ticket(FlightTicket {
                    ticket: ticket.encode().into(),
                }),
            )
            .with_ordered(false);
        Ok(Response::new(info))
    }

    /// Redeem a ticket and stream the result.
    async fn do_get(
        &self,
        request: Request<FlightTicket>,
    ) -> Result<Response<Self::DoGetStream>, Status> {
        let tenant = self.queries.tenant_of(request.metadata())?;
        let raw = request.into_inner();

        let ticket = Ticket::decode(&raw.ticket)
            .ok_or_else(|| Status::invalid_argument("this is not a ticket this server issued"))?;
        ticket
            .admit(&tenant, self.queries.now())
            .map_err(Self::refusal)?;

        let stream = self.queries.execute(&ticket).await?;
        let schema = stream.schema();

        // The batches are encoded as they arrive. Nothing accumulates: this is the whole
        // point of the surface, and collecting here would silently reintroduce exactly what
        // FR-API-07 forbids.
        let batches = stream.map_err(|error| {
            // An error mid-stream. Reported as a status on the stream rather than by
            // closing, because a truncated stream that ends cleanly is indistinguishable
            // from a complete one.
            arrow_flight::error::FlightError::ExternalError(Box::new(error))
        });

        let encoded = FlightDataEncoderBuilder::new()
            .with_schema(schema)
            .build(batches)
            .map_err(|error| Status::internal(error.to_string()));

        Ok(Response::new(encoded.boxed()))
    }

    async fn get_schema(
        &self,
        _request: Request<FlightDescriptor>,
    ) -> Result<Response<SchemaResult>, Status> {
        // Would require planning the query to know its schema, which is what
        // `get_flight_info` is for. Returning a guess would be worse than saying no.
        Err(Status::unimplemented(
            "a result's schema is known from the data itself; plan the query with \
             GetFlightInfo and read the schema from the stream",
        ))
    }

    async fn list_flights(
        &self,
        _request: Request<Criteria>,
    ) -> Result<Response<Self::ListFlightsStream>, Status> {
        // There is no persistent set of flights to list: every flight is one client's query.
        // An empty list would suggest there might be some later.
        Err(Status::unimplemented(
            "this server has no standing flights; each one is a query planned on request",
        ))
    }

    async fn poll_flight_info(
        &self,
        _request: Request<FlightDescriptor>,
    ) -> Result<Response<PollInfo>, Status> {
        Err(Status::unimplemented(
            "planning is synchronous here, so there is nothing to poll",
        ))
    }

    async fn do_put(
        &self,
        _request: Request<Streaming<FlightData>>,
    ) -> Result<Response<Self::DoPutStream>, Status> {
        // Deliberately absent. Writing goes through the transactional store for a managed
        // table and the publishing library for an external one; a third write path with its
        // own semantics would be a way for those two to disagree.
        Err(Status::unimplemented(
            "this surface is read-only. Write to a managed table through the transactional \
             store, or to an external one through the publishing library",
        ))
    }

    async fn do_exchange(
        &self,
        _request: Request<Streaming<FlightData>>,
    ) -> Result<Response<Self::DoExchangeStream>, Status> {
        Err(Status::unimplemented("DoExchange is not offered"))
    }

    async fn do_action(
        &self,
        _request: Request<Action>,
    ) -> Result<Response<Self::DoActionStream>, Status> {
        Err(Status::unimplemented(
            "no actions are offered: prepared statements and transactions are absent rather \
             than half-present, because a client that finds a method unimplemented is better \
             served than one that finds it works differently than it should",
        ))
    }

    async fn list_actions(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<Self::ListActionsStream>, Status> {
        // An empty list, truthfully: there are none, and this is the one place saying so is
        // the answer rather than a refusal.
        Ok(Response::new(futures::stream::empty().boxed()))
    }
}

/// Keep the pin import meaningful for the boxed stream types above.
type _Pinned = Pin<Box<dyn Send>>;

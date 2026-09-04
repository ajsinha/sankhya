//! Flight SQL, driven by a real client over a real socket.
//!
//! Two claims are under test. The data stays **columnar** end to end — the client's batches
//! are the server's batches, not rows reassembled. And the **ticket is a security boundary**:
//! the decision is made once, at planning, and redeeming does not re-authorize but does
//! check that the presenter is who the ticket was issued to.

// Tests may panic — that is how a test reports a failure. The workspace denies
// `unwrap`, `expect`, `panic` and indexing because a *server* must not do those things
// on data it did not choose; a test chooses all of its data, and an assertion that
// cannot fail loudly is worse than useless.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use arrow_array::{Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use datafusion::execution::SendableRecordBatchStream;
use datafusion::physical_plan::stream::RecordBatchStreamAdapter;
use sankhya_api_flight::service::{Queries, SankhyaFlight};
use sankhya_api_flight::ticket::{Refused, Ticket};
use sankhya_authz::principal::TenantId;
use std::sync::Arc;
use tonic::{Request, Status};

fn tenant(name: &str) -> TenantId {
    let mut bytes = [0u8; 16];
    for (slot, byte) in bytes.iter_mut().zip(name.bytes()) {
        *slot = byte;
    }
    TenantId::from_uuid(uuid::Uuid::from_bytes(bytes))
}

fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("label", DataType::Utf8, true),
    ]))
}

/// Three batches, so the stream is genuinely a stream.
fn batches() -> Vec<RecordBatch> {
    (0..3)
        .map(|chunk| {
            let ids: Vec<i64> = (chunk * 100..chunk * 100 + 100).collect();
            let labels: Vec<Option<String>> = ids
                .iter()
                .map(|i| (i % 7 != 0).then(|| format!("row-{i}")))
                .collect();
            RecordBatch::try_new(
                schema(),
                vec![
                    Arc::new(Int64Array::from(ids)),
                    Arc::new(StringArray::from(labels)),
                ],
            )
            .expect("a valid batch")
        })
        .collect()
}

/// A query source that plans, authorises and streams without an engine behind it.
struct Fixture {
    /// The statement this fixture refuses to plan, to exercise the authorization path.
    forbidden: &'static str,
    /// The clock, so expiry can be tested without waiting.
    now: std::sync::atomic::AtomicI64,
    /// Whether execution should fail partway through the stream.
    fail_midstream: bool,
}

impl Fixture {
    fn new() -> Self {
        Self {
            forbidden: "SELECT * FROM salaries",
            now: std::sync::atomic::AtomicI64::new(1_000),
            fail_midstream: false,
        }
    }
}

#[tonic::async_trait]
impl Queries for Fixture {
    fn tenant_of(&self, metadata: &tonic::metadata::MetadataMap) -> Result<TenantId, Status> {
        let name = metadata
            .get("sankhya-tenant")
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| Status::unauthenticated("no tenant was supplied"))?;
        Ok(tenant(name))
    }

    async fn plan(&self, _tenant: &TenantId, statement: &str) -> Result<u64, Status> {
        if statement == self.forbidden {
            // Not-found rather than permission-denied: naming the table in a refusal would
            // confirm it exists, and the difference is a working enumeration oracle.
            return Err(Status::not_found("no such table"));
        }
        Ok(41)
    }

    async fn execute(&self, _ticket: &Ticket) -> Result<SendableRecordBatchStream, Status> {
        let rows = batches();
        let stream: futures::stream::BoxStream<'static, datafusion::error::Result<RecordBatch>> =
            if self.fail_midstream {
                // One good batch, then a failure — the case a row protocol cannot produce.
                let first = rows.first().cloned().expect("a batch");
                Box::pin(futures::stream::iter(vec![
                    Ok(first),
                    Err(datafusion::error::DataFusionError::Execution(
                        "the last file could not be decoded".to_string(),
                    )),
                ]))
            } else {
                Box::pin(futures::stream::iter(rows.into_iter().map(Ok)))
            };
        Ok(Box::pin(RecordBatchStreamAdapter::new(schema(), stream)))
    }

    fn now(&self) -> i64 {
        self.now.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Start a Flight server on an ephemeral port and return a connected client.
async fn connect(
    fixture: Fixture,
) -> (
    arrow_flight::flight_service_client::FlightServiceClient<tonic::transport::Channel>,
    std::net::SocketAddr,
) {
    use arrow_flight::flight_service_server::FlightServiceServer;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("binding");
    let address = listener.local_addr().expect("an address");
    let service = SankhyaFlight::new(Arc::new(fixture));

    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(FlightServiceServer::new(service))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .ok();
    });

    // Retry briefly rather than sleeping a fixed time, so the test is neither flaky on a
    // loaded machine nor slow on an idle one.
    for _ in 0..50 {
        if let Ok(channel) = tonic::transport::Channel::from_shared(format!("http://{address}"))
            .expect("a valid endpoint")
            .connect()
            .await
        {
            return (
                arrow_flight::flight_service_client::FlightServiceClient::new(channel),
                address,
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("the Flight server did not start");
}

/// A request carrying a tenant.
fn as_tenant<T>(body: T, name: &str) -> Request<T> {
    let mut request = Request::new(body);
    request.metadata_mut().insert(
        "sankhya-tenant",
        name.parse().expect("a valid metadata value"),
    );
    request
}

// --- the columnar claim ---------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_result_arrives_as_arrow_batches_rather_than_rows() {
    // The whole reason this surface exists. The client's batches are the server's batches.
    let (mut client, _) = connect(Fixture::new()).await;

    let info = client
        .get_flight_info(as_tenant(
            arrow_flight::FlightDescriptor::new_cmd("SELECT id, label FROM t"),
            "acme",
        ))
        .await
        .expect("planning")
        .into_inner();

    let endpoint = info.endpoint.first().expect("one endpoint");
    let ticket = endpoint.ticket.clone().expect("a ticket");

    let stream = client
        .do_get(as_tenant(ticket, "acme"))
        .await
        .expect("redeeming")
        .into_inner();

    let decoded = arrow_flight::decode::FlightRecordBatchStream::new_from_flight_data(
        stream.map_err(|status| arrow_flight::error::FlightError::Tonic(Box::new(status))),
    );
    use futures::TryStreamExt;
    let received: Vec<RecordBatch> = decoded.try_collect().await.expect("decoding");

    let rows: usize = received.iter().map(RecordBatch::num_rows).sum();
    assert_eq!(rows, 300, "three batches of a hundred");
    assert_eq!(
        received.first().map(|b| b.schema()),
        Some(schema()),
        "the schema arrives with the data rather than being reconstructed"
    );

    // And a null is still a null, not an empty string.
    let first = received.first().expect("a batch");
    use arrow_array::Array;
    let labels = first
        .column(1)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("a string column");
    assert!(
        labels.is_null(0),
        "id 0 is divisible by seven, so its label is null — and a null must survive the \
         wire as a null rather than as an empty string"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn several_batches_arrive_as_several_batches() {
    // Not concatenated into one. A client streaming a large extract should see the batching
    // the server produced, or the "nothing is materialised" claim is untestable from outside.
    let (mut client, _) = connect(Fixture::new()).await;
    let info = client
        .get_flight_info(as_tenant(
            arrow_flight::FlightDescriptor::new_cmd("SELECT 1"),
            "acme",
        ))
        .await
        .expect("planning")
        .into_inner();
    let ticket = info
        .endpoint
        .first()
        .and_then(|e| e.ticket.clone())
        .expect("a ticket");

    let stream = client
        .do_get(as_tenant(ticket, "acme"))
        .await
        .expect("redeeming")
        .into_inner();
    use futures::TryStreamExt;
    let received: Vec<RecordBatch> =
        arrow_flight::decode::FlightRecordBatchStream::new_from_flight_data(
            stream.map_err(|status| arrow_flight::error::FlightError::Tonic(Box::new(status))),
        )
        .try_collect()
        .await
        .expect("decoding");

    assert_eq!(received.len(), 3, "the server's batching survives the wire");
}

// --- the ticket as a security boundary ------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_ticket_issued_to_one_tenant_is_refused_to_another() {
    // The reason a ticket carries its decision rather than its query: the principal
    // redeeming is not necessarily the one who requested.
    let (mut client, _) = connect(Fixture::new()).await;
    let info = client
        .get_flight_info(as_tenant(
            arrow_flight::FlightDescriptor::new_cmd("SELECT 1"),
            "acme",
        ))
        .await
        .expect("planning")
        .into_inner();
    let ticket = info
        .endpoint
        .first()
        .and_then(|e| e.ticket.clone())
        .expect("a ticket");

    let outcome = client.do_get(as_tenant(ticket, "rival")).await;
    let Err(status) = outcome else {
        panic!("a leaked ticket must not be redeemable by another tenant");
    };
    assert_eq!(status.code(), tonic::Code::PermissionDenied);
    assert!(
        !status.message().contains("acme"),
        "the refusal must not say whose ticket it is: {}",
        status.message()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_forged_ticket_is_refused() {
    let (mut client, _) = connect(Fixture::new()).await;
    let outcome = client
        .do_get(as_tenant(
            arrow_flight::Ticket {
                ticket: b"not-a-ticket".to_vec().into(),
            },
            "acme",
        ))
        .await;
    let Err(status) = outcome else {
        panic!("arbitrary bytes must not redeem");
    };
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_with_no_tenant_is_refused() {
    // An unattributable request cannot be audited, and one nobody can attribute is one
    // nobody can refuse either.
    let (mut client, _) = connect(Fixture::new()).await;
    let outcome = client
        .get_flight_info(Request::new(arrow_flight::FlightDescriptor::new_cmd(
            "SELECT 1",
        )))
        .await;
    assert_eq!(
        outcome.err().map(|status| status.code()),
        Some(tonic::Code::Unauthenticated)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn authorization_happens_at_planning_and_the_refusal_does_not_confirm_the_table() {
    let (mut client, _) = connect(Fixture::new()).await;
    let outcome = client
        .get_flight_info(as_tenant(
            arrow_flight::FlightDescriptor::new_cmd("SELECT * FROM salaries"),
            "acme",
        ))
        .await;
    let Err(status) = outcome else {
        panic!("a forbidden statement must not yield a ticket");
    };
    assert_eq!(status.code(), tonic::Code::NotFound);
}

// --- expiry ---------------------------------------------------------------

#[test]
fn an_expired_ticket_says_to_plan_again() {
    // A ticket names a snapshot and a snapshot's files are eventually retired, so an
    // unbounded ticket is a lease nobody granted.
    let ticket = Ticket::issue(tenant("acme"), "SELECT 1", 41, 0, 100);
    assert!(ticket.admit(&tenant("acme"), 99).is_ok());

    let Err(refused) = ticket.admit(&tenant("acme"), 100) else {
        panic!("expiry is exclusive");
    };
    assert!(matches!(refused, Refused::Expired { .. }));
    assert!(refused.to_string().contains("Plan the query again"));
}

/// A lifetime that is not a lifetime cannot produce a ticket that outlives its snapshot.
///
/// # Why there is no mutation for the clamp
///
/// One was written --- remove `.max(0)` --- and it **survived**, correctly. Clamped, a
/// negative lifetime gives `expires_at == now`; unclamped it gives something earlier. Both
/// are expired at every instant, so no test can tell them apart, and the mutation describes
/// no defect. It was removed rather than answered with a test contorted until it failed.
///
/// The property is still worth pinning. A ticket carries an authorization decision that is
/// deliberately never re-checked, so its expiry is the entire bound on how long it works, and
/// "a lifetime that is not a lifetime yields nothing usable" is the kind of thing that stops
/// being true when the arithmetic around it is rewritten.
#[test]
fn a_nonsensical_lifetime_cannot_outlive_the_moment_it_was_issued() {
    let issued_at = 1_000i64;
    let ticket = Ticket::issue(tenant("acme"), "SELECT 1", 41, issued_at, -5_000);

    assert!(
        ticket.admit(&tenant("acme"), issued_at).is_err(),
        "a ticket issued with a negative lifetime was admitted at the instant it was issued"
    );
    assert!(
        ticket.admit(&tenant("acme"), issued_at + 1).is_err(),
        "it must not become valid later either"
    );
}

#[test]
fn a_ticket_round_trips_and_a_tampered_one_does_not() {
    let ticket = Ticket::issue(tenant("acme"), "SELECT id FROM t", 41, 0, 1_000);
    let encoded = ticket.encode();
    assert_eq!(Ticket::decode(&encoded), Some(ticket));

    // Truncated, extended, and byte-flipped.
    assert_eq!(Ticket::decode(&encoded[..encoded.len() - 1]), None);
    let mut extended = encoded.clone();
    extended.push(0);
    assert_eq!(Ticket::decode(&extended), None);
    assert_eq!(Ticket::decode(b"skhyft1garbage"), None);
}

// --- what is deliberately absent ------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn writing_is_refused_with_a_reason_rather_than_silently_absent() {
    // A third write path with its own semantics would be a way for the other two to
    // disagree, so this says which of them to use.
    let (mut client, _) = connect(Fixture::new()).await;
    let outcome = client
        .do_put(as_tenant(
            futures::stream::empty::<arrow_flight::FlightData>(),
            "acme",
        ))
        .await;
    let Err(status) = outcome else {
        panic!("DoPut must be refused");
    };
    assert_eq!(status.code(), tonic::Code::Unimplemented);
    assert!(
        status.message().contains("publishing library"),
        "{}",
        status.message()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn unimplemented_methods_say_so_rather_than_returning_nothing() {
    // A client that finds a method unimplemented is better served than one that finds it
    // works differently than it should.
    let (mut client, _) = connect(Fixture::new()).await;

    assert_eq!(
        client
            .list_flights(as_tenant(arrow_flight::Criteria::default(), "acme"))
            .await
            .err()
            .map(|s| s.code()),
        Some(tonic::Code::Unimplemented)
    );
    assert_eq!(
        client
            .get_schema(as_tenant(
                arrow_flight::FlightDescriptor::new_cmd("SELECT 1"),
                "acme"
            ))
            .await
            .err()
            .map(|s| s.code()),
        Some(tonic::Code::Unimplemented)
    );
}

// --- the behaviour change worth documenting -------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn an_error_can_arrive_after_data_has_already_been_sent() {
    // A row protocol sends its error before the first row or not at all. This one may have
    // sent a gigabyte first — and it reports the failure rather than ending quietly, because
    // a truncated stream that closes cleanly is indistinguishable from a complete one.
    let (mut client, _) = connect(Fixture {
        fail_midstream: true,
        ..Fixture::new()
    })
    .await;

    let info = client
        .get_flight_info(as_tenant(
            arrow_flight::FlightDescriptor::new_cmd("SELECT 1"),
            "acme",
        ))
        .await
        .expect("planning")
        .into_inner();
    let ticket = info
        .endpoint
        .first()
        .and_then(|e| e.ticket.clone())
        .expect("a ticket");

    let stream = client
        .do_get(as_tenant(ticket, "acme"))
        .await
        .expect("the stream starts")
        .into_inner();

    use futures::TryStreamExt;
    let outcome: Result<Vec<RecordBatch>, _> =
        arrow_flight::decode::FlightRecordBatchStream::new_from_flight_data(
            stream.map_err(|status| arrow_flight::error::FlightError::Tonic(Box::new(status))),
        )
        .try_collect()
        .await;

    assert!(
        outcome.is_err(),
        "the stream must end in an error, not merely end"
    );
}

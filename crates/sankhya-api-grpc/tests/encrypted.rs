//! The columnar door, served over TLS, answering a real call.
//!
//! # Why this is a round trip rather than a handshake assertion
//!
//! A handshake that completes proves the certificate loaded. It does not prove that the
//! stream tonic was handed still speaks HTTP/2, that ALPN was negotiated, or that a service
//! reached through it behaves as it does on a plain socket. Those are the ways a transport
//! wrapped in TLS goes wrong, and none of them is visible from a handshake.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use arrow_flight::flight_service_client::FlightServiceClient;
use arrow_flight::FlightDescriptor;
use datafusion::execution::SendableRecordBatchStream;
use sankhya_api_flight::{Caller, Queries, SankhyaFlight, Ticket};
use sankhya_api_grpc::Transport;
use sankhya_types::TenantId;
use sankhya_testkit::certificates::self_signed;
use sankhya_tls::{Acceptor, Alpn, Material};
use std::sync::Arc;
use tonic::{Request, Status};

/// The smallest thing that can be asked a question.
///
/// Deliberately minimal: this crate's tests are about the transport, and a fixture that
/// planned and executed anything real would be testing `sankhya-api-flight` again through a
/// longer pipe.
#[derive(Debug)]
struct Reachable;

#[tonic::async_trait]
impl Queries for Reachable {
    fn caller_of(&self, metadata: &tonic::metadata::MetadataMap) -> Result<Caller, Status> {
        metadata
            .get("sankhya-tenant")
            .and_then(|value| value.to_str().ok())
            .map(|name| {
                let mut bytes = [0u8; 16];
                for (slot, byte) in bytes.iter_mut().zip(name.bytes()) {
                    *slot = byte;
                }
                Caller::new(TenantId::from_uuid(uuid::Uuid::from_bytes(bytes)), "ana")
            })
            .ok_or_else(|| Status::unauthenticated("no tenant was supplied"))
    }

    async fn plan(&self, _caller: &Caller, _statement: &str) -> Result<u64, Status> {
        Ok(1)
    }

    async fn execute(&self, _ticket: &Ticket) -> Result<SendableRecordBatchStream, Status> {
        Err(Status::unimplemented("this fixture only plans"))
    }

    fn now(&self) -> i64 {
        1_700_000_000_000_000
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_flight_call_completes_over_tls_and_not_without_it() {
    let directory = tempfile::tempdir().expect("temp");
    let pair = self_signed().expect("generate");
    let anchor = pair.certificate.clone();
    let (certificate, key) = pair.write(directory.path(), "server").expect("write");
    let material = Material::load(&certificate, &key).expect("load");
    // HTTP/2, stated. A gRPC client that negotiates nothing will not speak it, so this is
    // the one place ALPN is load-bearing rather than informational.
    let acceptor = Acceptor::new(&material, Alpn::Http2).expect("acceptor");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    drop(listener);

    tokio::spawn(async move {
        Transport::new(address)
            .encrypted(acceptor)
            .serve_until(
                SankhyaFlight::new(Arc::new(Reachable)),
                std::future::pending::<()>(),
            )
            .await
            .ok();
    });

    // A plain client, first. It reaches a listening port and gets nowhere, which is the
    // property that distinguishes an encrypted door from one that merely offers encryption.
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(address).await.is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    //
    // The assertion is on the *answer*, not on whether a socket opened. A TCP connection to
    // an encrypted door opens perfectly well — that is what makes this failure mode quiet —
    // and the door simply never becomes a session. What a plain client cannot do is get a
    // reply.
    let plain = tonic::transport::Channel::from_shared(format!("http://{address}"))
        .expect("endpoint")
        .connect_timeout(std::time::Duration::from_secs(2))
        .connect_lazy();
    let mut deaf = FlightServiceClient::new(plain);
    let mut attempt = Request::new(FlightDescriptor::new_cmd(b"SELECT 1".to_vec()));
    attempt
        .metadata_mut()
        .insert("sankhya-tenant", "acme".parse().expect("metadata"));
    let unanswered = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        deaf.get_flight_info(attempt),
    )
    .await;
    match unanswered {
        Ok(Ok(_)) => panic!("an unencrypted client got an answer from an encrypted door"),
        Ok(Err(_)) | Err(_) => {}
    }

    // And now over TLS, with the connector doing what a client library does: dial, wrap,
    // hand tonic a stream that is already a session.
    let mut store = rustls::RootCertStore::empty();
    for certificate in
        <rustls::pki_types::CertificateDer as rustls::pki_types::pem::PemObject>::pem_slice_iter(
            anchor.as_bytes(),
        )
    {
        store.add(certificate.expect("anchor")).expect("add");
    }
    let mut config = rustls::ClientConfig::builder_with_provider(sankhya_tls::provider())
        .with_safe_default_protocol_versions()
        .expect("versions")
        .with_root_certificates(store)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec()];
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));

    let channel = tonic::transport::Endpoint::from_shared(format!("https://{address}"))
        .expect("endpoint")
        .connect_with_connector(tower::service_fn(move |_: tonic::transport::Uri| {
            let connector = connector.clone();
            async move {
                let stream = tokio::net::TcpStream::connect(address).await?;
                let name = rustls::pki_types::ServerName::try_from("localhost")
                    .map_err(|error| std::io::Error::other(error.to_string()))?;
                let session = connector.connect(name, stream).await?;
                Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(session))
            }
        }))
        .await
        .expect("an encrypted channel");

    let mut client = FlightServiceClient::new(channel);
    let mut request = Request::new(FlightDescriptor::new_cmd(b"SELECT 1".to_vec()));
    request
        .metadata_mut()
        .insert("sankhya-tenant", "acme".parse().expect("metadata"));
    let information = client.get_flight_info(request).await.expect("a planned flight");

    assert!(
        !information.into_inner().endpoint.is_empty(),
        "the service answered through the tunnel"
    );
}

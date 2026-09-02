//! The negotiation in front of this door, and the connection it refuses.
//!
//! # Why this door cannot just be wrapped in TLS
//!
//! A PostgreSQL client does not open a TLS connection. It opens a plain socket, asks in
//! eight bytes whether encryption is available, and reads a single byte back --- `S` or `N`
//! --- before any handshake exists. Encryption here is part of the protocol rather than a
//! layer beneath it.
//!
//! The failure that matters is silent in the permissive direction. A server that answers `N`
//! when it meant to require TLS serves every connection perfectly well, in plain text, and
//! nothing a client prints says otherwise. So the tests below check the *answer*, not merely
//! that a connection succeeded.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_api_pg::listener::{Encryption, PgListener};
use sankhya_api_pg::message::{
    oid, FieldDescription, GSS_REQUEST_CODE, PROTOCOL_VERSION, SSL_REQUEST_CODE,
};
use sankhya_api_pg::catalog::CatalogTable;
use sankhya_api_pg::session::{Caller, Handler, QueryFailure, QueryResult};
use sankhya_testkit::certificates::{self_signed, Pair};
use sankhya_tls::{Acceptor, Alpn, Material};
use std::sync::Arc;
use rustls::pki_types::pem::PemObject;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// A handler that answers anything, because these tests are about the transport.
#[derive(Debug)]
struct Anything;

impl Handler for Anything {
    fn requires_password(&self, _parameters: &[(String, String)]) -> bool {
        false
    }

    fn authenticate(
        &self,
        _parameters: &[(String, String)],
        _password: Option<&[u8]>,
    ) -> Result<(), QueryFailure> {
        Ok(())
    }

    fn visible_tables(&self, _caller: &Caller<'_>) -> Vec<CatalogTable> {
        Vec::new()
    }

    fn query(&self, _sql: &str, _caller: &Caller<'_>) -> Result<QueryResult, QueryFailure> {
        Ok(QueryResult {
            fields: vec![FieldDescription::text("counted", oid::INT8, 8)],
            rows: vec![vec![Some("1".to_owned())]],
            tag: "SELECT 1".to_owned(),
        })
    }
}

/// A door, and the certificate a client should trust to reach it.
struct Door {
    address: std::net::SocketAddr,
    anchor: String,
    _directory: tempfile::TempDir,
}

/// Start a door serving exactly one connection.
async fn door(insisting: bool) -> Door {
    let directory = tempfile::tempdir().expect("temp");
    let pair: Pair = self_signed().expect("generate");
    let anchor = pair.certificate.clone();
    let (certificate, key) = pair.write(directory.path(), "server").expect("write");
    let material = Material::load(&certificate, &key).expect("load");
    let acceptor = Acceptor::new(&material, Alpn::None).expect("acceptor");
    let encryption = if insisting {
        Encryption::Required(acceptor)
    } else {
        Encryption::Offered(acceptor)
    };

    let listener = PgListener::bind("127.0.0.1:0")
        .await
        .expect("bind")
        .encrypted(encryption);
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move {
        let handler: Arc<dyn Handler> = Arc::new(Anything);
        listener.accept_one(handler).await.ok();
    });
    Door { address, anchor, _directory: directory }
}

/// The eight bytes that ask a question before the connection has a protocol.
fn request(code: i32) -> Vec<u8> {
    let mut packet = Vec::with_capacity(8);
    packet.extend_from_slice(&8i32.to_be_bytes());
    packet.extend_from_slice(&code.to_be_bytes());
    packet
}

/// A startup message for `user`.
fn startup(user: &str) -> Vec<u8> {
    let mut body = Vec::new();
    for (key, value) in [("user", user), ("database", "acme")] {
        body.extend_from_slice(key.as_bytes());
        body.push(0);
        body.extend_from_slice(value.as_bytes());
        body.push(0);
    }
    body.push(0);
    let mut packet = Vec::new();
    packet.extend_from_slice(&i32::try_from(body.len() + 8).unwrap_or(0).to_be_bytes());
    packet.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
    packet.extend_from_slice(&body);
    packet
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_door_that_can_encrypt_says_s_and_completes_a_handshake() {
    let door = door(false).await;
    let mut stream = TcpStream::connect(door.address).await.expect("connect");
    stream.write_all(&request(SSL_REQUEST_CODE)).await.expect("ask");

    let mut answer = [0u8; 1];
    stream.read_exact(&mut answer).await.expect("an answer");
    assert_eq!(answer[0], b'S', "the door offers encryption and says so");

    // And the byte is followed by a handshake rather than by protocol.
    let mut store = rustls::RootCertStore::empty();
    for certificate in rustls::pki_types::CertificateDer::pem_slice_iter(door.anchor.as_bytes()) {
        store.add(certificate.expect("anchor")).expect("add");
    }
    let config = rustls::ClientConfig::builder_with_provider(sankhya_tls::provider())
        .with_safe_default_protocol_versions()
        .expect("versions")
        .with_root_certificates(store)
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let name = rustls::pki_types::ServerName::try_from("localhost").expect("name");
    let mut tls = connector.connect(name, stream).await.expect("handshake");

    // Inside the tunnel the protocol starts over, which is what the specification says and
    // what a server that reused the outer connection's state would get wrong.
    tls.write_all(&startup("ana")).await.expect("startup");
    tls.flush().await.expect("flush");
    let mut seen = vec![0u8; 1024];
    let read = tls.read(&mut seen).await.expect("read");
    assert!(read > 0, "the encrypted session speaks the protocol");
    assert_eq!(seen[0], b'R', "authentication is the first thing inside the tunnel");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plain_connection_to_a_door_that_requires_tls_is_told_why() {
    let door = door(true).await;
    let mut stream = TcpStream::connect(door.address).await.expect("connect");

    // No question asked: straight to the startup message, which is what a client configured
    // with `sslmode=disable` does.
    stream.write_all(&startup("ana")).await.expect("startup");
    stream.flush().await.expect("flush");

    // Bounded, and the bound is part of the assertion. Reading to end-of-file to prove a
    // refusal works only when the refusal happens: a server that *serves* this connection
    // holds it open, and an unbounded read turns a clear failure into a test that hangs.
    // A mutation of the guard above did exactly that, and the audit stopped on a timeout
    // rather than reporting a survivor.
    let mut seen = Vec::new();
    let closed = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        stream.read_to_end(&mut seen),
    )
    .await;
    assert!(
        closed.is_ok(),
        "the connection was served rather than refused: it is still open"
    );

    assert_eq!(seen.first(), Some(&b'E'), "an error message, not a closed socket");
    let said = String::from_utf8_lossy(&seen);
    assert!(said.contains("28000"), "the SQLSTATE a driver branches on: {said}");
    assert!(said.contains("requires TLS"), "{said}");
    assert!(said.contains("sslmode=require"), "the remediation, not just the diagnosis");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_gssapi_request_is_declined_rather_than_ignored() {
    // `psql` with `gssencmode=prefer` — the default on several distributions — asks this
    // first. An unrecognised code would be a decode error, and the most ordinary client on
    // Linux would fail to connect for a reason that reads like corruption.
    let door = door(false).await;
    let mut stream = TcpStream::connect(door.address).await.expect("connect");
    stream.write_all(&request(GSS_REQUEST_CODE)).await.expect("ask");

    let mut answer = [0u8; 1];
    stream.read_exact(&mut answer).await.expect("an answer");
    assert_eq!(answer[0], b'N', "no GSSAPI here, said out loud");

    // And the connection carries on to ask the question it actually cares about.
    stream.write_all(&request(SSL_REQUEST_CODE)).await.expect("ask");
    stream.read_exact(&mut answer).await.expect("an answer");
    assert_eq!(answer[0], b'S');
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plain_door_still_declines_and_lets_the_client_carry_on() {
    // The behaviour every local test and tool in this project depends on, kept honest while
    // the encrypted path was added beside it.
    let listener = PgListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move {
        let handler: Arc<dyn Handler> = Arc::new(Anything);
        listener.accept_one(handler).await.ok();
    });

    let mut stream = TcpStream::connect(address).await.expect("connect");
    stream.write_all(&request(SSL_REQUEST_CODE)).await.expect("ask");
    let mut answer = [0u8; 1];
    stream.read_exact(&mut answer).await.expect("an answer");
    assert_eq!(answer[0], b'N');

    stream.write_all(&startup("ana")).await.expect("startup");
    let mut seen = vec![0u8; 512];
    let read = stream.read(&mut seen).await.expect("read");
    assert!(read > 0 && seen[0] == b'R', "the plain connection proceeds");
}

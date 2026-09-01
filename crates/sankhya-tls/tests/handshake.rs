//! Handshakes that complete, handshakes that are refused, and the one that never arrives.
//!
//! # Why a stalled handshake has its own test
//!
//! A connection that opens and then says nothing is free for the peer and not free for the
//! server: it holds a task and a buffer for as long as it is allowed to. Nothing in a
//! handshake requires a client to proceed, so the bound has to be the server's, and a bound
//! nobody exercises is a bound nobody knows is wired up.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_testkit::certificates::{self_signed, Authority, Pair};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::{ClientConfig, RootCertStore};
use sankhya_tls::acceptor::Failed;
use sankhya_tls::material::Material;
use sankhya_tls::{Acceptor, Alpn};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsConnector;

/// A client that trusts `anchors`, optionally presenting a certificate of its own.
fn client(anchors: &[&str], own: Option<&Pair>, alpn: &[&[u8]]) -> TlsConnector {
    let mut store = RootCertStore::empty();
    for anchor in anchors {
        for certificate in CertificateDer::pem_slice_iter(anchor.as_bytes()) {
            store.add(certificate.expect("anchor")).expect("add");
        }
    }
    let builder = ClientConfig::builder_with_provider(sankhya_tls::provider())
        .with_safe_default_protocol_versions()
        .expect("versions")
        .with_root_certificates(store);
    let mut config = match own {
        Some(pair) => {
            let chain = CertificateDer::pem_slice_iter(pair.certificate.as_bytes())
                .collect::<Result<Vec<_>, _>>()
                .expect("chain");
            let key = PrivateKeyDer::from_pem_slice(pair.key.as_bytes()).expect("key");
            builder.with_client_auth_cert(chain, key).expect("client cert")
        }
        None => builder.with_no_client_auth(),
    };
    config.alpn_protocols = alpn.iter().map(|protocol| protocol.to_vec()).collect();
    TlsConnector::from(Arc::new(config))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_that_trusts_the_certificate_completes_a_handshake() {
    let directory = tempfile::tempdir().expect("temp");
    let pair = self_signed().expect("generate");
    let anchor = pair.certificate.clone();
    let (certificate, key) = pair.write(directory.path(), "server").expect("write");
    let material = Material::load(&certificate, &key).expect("load");
    let acceptor = Acceptor::new(&material, Alpn::Http2).expect("acceptor");

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("addr");
    let served = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let mut tls = acceptor.accept(stream).await.expect("handshake");
        tls.write_all(b"counted").await.expect("write");
        tls.shutdown().await.expect("shutdown");
        // The negotiated protocol, read from the completed session rather than assumed.
        tls.get_ref().1.alpn_protocol().map(<[u8]>::to_vec)
    });

    let connector = client(&[&anchor], None, &[b"h2"]);
    let stream = TcpStream::connect(address).await.expect("connect");
    let name = ServerName::try_from("localhost").expect("name");
    let mut tls = connector.connect(name, stream).await.expect("client handshake");
    let mut said = String::new();
    tls.read_to_string(&mut said).await.expect("read");

    assert_eq!(said, "counted");
    assert_eq!(served.await.expect("join"), Some(b"h2".to_vec()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_peer_that_opens_a_connection_and_says_nothing_is_dropped() {
    let directory = tempfile::tempdir().expect("temp");
    let (certificate, key) = self_signed().expect("generate").write(directory.path(), "server").expect("write");
    let material = Material::load(&certificate, &key).expect("load");
    let acceptor = Acceptor::new(&material, Alpn::Http2)
        .expect("acceptor")
        .within(Duration::from_millis(100));

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("addr");
    let served = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        acceptor.accept(stream).await.err()
    });

    // Connect, and then be exactly as silent as a scanner is.
    let _quiet = TcpStream::connect(address).await.expect("connect");

    assert_eq!(served.await.expect("join"), Some(Failed::TimedOut));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_that_does_not_trust_the_certificate_is_rejected_by_both_sides() {
    let directory = tempfile::tempdir().expect("temp");
    let (certificate, key) = self_signed().expect("generate").write(directory.path(), "server").expect("write");
    let material = Material::load(&certificate, &key).expect("load");
    let acceptor = Acceptor::new(&material, Alpn::None).expect("acceptor");

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("addr");
    let served = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        acceptor.accept(stream).await.err()
    });

    // An empty trust store: the certificate is perfectly good and this client has no
    // reason to believe it.
    let connector = client(&[], None, &[]);
    let stream = TcpStream::connect(address).await.expect("connect");
    let name = ServerName::try_from("localhost").expect("name");
    let refused = connector.connect(name, stream).await;

    assert!(refused.is_err(), "an untrusted certificate is not accepted");
    match served.await.expect("join") {
        Some(Failed::Rejected(_)) => {}
        other => panic!("the server sees a rejected handshake, not {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mutual_door_refuses_a_client_with_no_certificate_of_its_own() {
    let directory = tempfile::tempdir().expect("temp");
    let authority = Authority::new().expect("authority");
    let bundle_pem = authority.bundle();
    let server = authority.sign("localhost").expect("sign");
    let anchor = bundle_pem.clone();
    let (certificate, key) = server.write(directory.path(), "server").expect("write");
    let bundle = directory.path().join("clients.pem");
    std::fs::write(&bundle, &bundle_pem).expect("write");
    let material = Material::load(&certificate, &key)
        .expect("load")
        .requiring_client_certificates(&bundle)
        .expect("bundle");
    let acceptor = Acceptor::new(&material, Alpn::None).expect("acceptor");

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("addr");
    let served = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        acceptor.accept(stream).await.err()
    });

    // The client trusts the server completely. It simply has nothing to show for itself,
    // which on a mutual door is the whole question.
    let connector = client(&[&anchor], None, &[]);
    let stream = TcpStream::connect(address).await.expect("connect");
    let name = ServerName::try_from("localhost").expect("name");
    let mut tls = connector.connect(name, stream).await.expect("client side proceeds");
    // Bounded: if the door admitted this client instead of refusing it, the connection
    // stays open and an unbounded read would hang rather than fail.
    let mut ignored = Vec::new();
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tls.read_to_end(&mut ignored),
    )
    .await;

    match served.await.expect("join") {
        Some(Failed::Rejected(_)) => {}
        other => panic!("a mutual door refuses an anonymous client, not {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mutual_door_admits_a_client_the_authority_signed_and_can_see_who_it_is() {
    let directory = tempfile::tempdir().expect("temp");
    let authority = Authority::new().expect("authority");
    let bundle_pem = authority.bundle();
    let anchor = bundle_pem.clone();
    let (certificate, key) = authority.sign("localhost").expect("sign").write(directory.path(), "server").expect("write");
    let bundle = directory.path().join("clients.pem");
    std::fs::write(&bundle, &bundle_pem).expect("write");
    let material = Material::load(&certificate, &key)
        .expect("load")
        .requiring_client_certificates(&bundle)
        .expect("bundle");
    let acceptor = Acceptor::new(&material, Alpn::None).expect("acceptor");

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("addr");
    let served = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let tls = acceptor.accept(stream).await.expect("handshake");
        // What the door knows about who connected. This is the hook identity will hang
        // from: a principal derived from a certificate, rather than from a password that
        // crossed the wire.
        tls.get_ref().1.peer_certificates().map(<[CertificateDer<'_>]>::len)
    });

    let connector = client(&[&anchor], Some(&authority.sign("analyst").expect("sign")), &[]);
    let stream = TcpStream::connect(address).await.expect("connect");
    let name = ServerName::try_from("localhost").expect("name");
    let _tls = connector.connect(name, stream).await.expect("client handshake");

    assert_eq!(served.await.expect("join"), Some(1), "the door holds the client's certificate");
}

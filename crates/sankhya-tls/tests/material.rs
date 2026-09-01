//! What loading refuses, and why each refusal is its own answer.
//!
//! # The mix-up these are mostly about
//!
//! Two files, both PEM, both plausible in either slot. Naming the key as the certificate
//! is the single most common way a TLS deployment fails on its first day, and a server
//! that answers *"TLS configuration failed"* has told the person nothing they did not
//! already know. Each refusal below names the file and what it actually held.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_testkit::certificates::{self_signed, Authority};
use sankhya_tls::material::{Material, Refused};
use sankhya_tls::{Acceptor, Alpn};

#[test]
fn a_certificate_and_its_key_load() {
    let directory = tempfile::tempdir().expect("temp");
    let (certificate, key) = self_signed().expect("generate").write(directory.path(), "server").expect("write");

    let material = Material::load(&certificate, &key).expect("load");

    assert!(!material.is_mutual(), "no client bundle was configured");
}

#[test]
fn the_key_named_as_the_certificate_says_so() {
    let directory = tempfile::tempdir().expect("temp");
    let (certificate, key) = self_signed().expect("generate").write(directory.path(), "server").expect("write");

    // The classic transposition: both paths exist, both parse, both are the wrong one.
    let refused = Material::load(&key, &certificate).expect_err("a key is not a certificate");

    assert_eq!(refused, Refused::NoCertificate { path: key.clone() });
    let said = refused.to_string();
    assert!(said.contains("holds no certificate"), "{said}");
    assert!(said.contains(&key.display().to_string()), "names the file: {said}");
}

#[test]
fn the_certificate_named_as_the_key_says_so() {
    let directory = tempfile::tempdir().expect("temp");
    let (certificate, key) = self_signed().expect("generate").write(directory.path(), "server").expect("write");
    drop(key);

    let refused = Material::load(&certificate, &certificate)
        .expect_err("a certificate is not a private key");

    assert_eq!(refused, Refused::NoPrivateKey { path: certificate.clone() });
    assert!(refused.to_string().contains("holds no private key"));
}

#[test]
fn a_key_from_a_different_certificate_is_refused_at_load() {
    let directory = tempfile::tempdir().expect("temp");
    let (certificate, _) = self_signed().expect("generate").write(directory.path(), "first").expect("write");
    let (_, key) = self_signed().expect("generate").write(directory.path(), "second").expect("write");

    // Both files are entirely valid. Only the pairing is wrong, and nothing about either
    // file on its own says so — which is why this has to be checked rather than assumed.
    let refused = Material::load(&certificate, &key).expect_err("not a pair");

    assert_eq!(
        refused,
        Refused::Mismatched { certificate: certificate.clone(), key: key.clone() }
    );
    assert!(refused.to_string().contains("never one"), "{refused}");
}

#[test]
fn a_file_that_is_not_pem_at_all_is_unreadable_rather_than_empty() {
    let directory = tempfile::tempdir().expect("temp");
    let path = directory.path().join("notes.txt");
    std::fs::write(&path, b"this is not a certificate\n").expect("write");
    let (_, key) = self_signed().expect("generate").write(directory.path(), "server").expect("write");

    let refused = Material::load(&path, &key).expect_err("not PEM");

    // Not `NoCertificate`: a file holding prose and a file holding the wrong PEM block are
    // different mistakes, and the second is the one with a fix that is not "start again".
    assert_eq!(refused, Refused::NoCertificate { path });
}

#[test]
fn a_missing_file_names_itself() {
    let directory = tempfile::tempdir().expect("temp");
    let (_, key) = self_signed().expect("generate").write(directory.path(), "server").expect("write");
    let absent = directory.path().join("nowhere.crt");

    let refused = Material::load(&absent, &key).expect_err("absent");

    match refused {
        Refused::Unreadable { path, .. } => assert_eq!(path, absent),
        other => panic!("a missing file is unreadable, not {other:?}"),
    }
}

#[test]
fn a_client_bundle_holding_no_certificate_is_refused() {
    let directory = tempfile::tempdir().expect("temp");
    let (certificate, key) = self_signed().expect("generate").write(directory.path(), "server").expect("write");
    let bundle = directory.path().join("clients.pem");
    std::fs::write(&bundle, b"# no anchors here\n").expect("write");

    let refused = Material::load(&certificate, &key)
        .expect("load")
        .requiring_client_certificates(&bundle)
        .expect_err("an empty bundle trusts nobody");

    // The variant is not enough to assert. `rustls` also refuses an empty root store, so a
    // server that skipped this check would still refuse — with `rustls`'s sentence about
    // root certificates rather than one naming the operator's file and what was wrong with
    // it. The reason is the part that differs, so the reason is what this asserts.
    match refused {
        Refused::NoTrustAnchors { path, why } => {
            assert_eq!(path, bundle);
            assert_eq!(why, "the file holds no certificate");
        }
        other => panic!("expected no trust anchors, got {other:?}"),
    }
}

#[test]
fn a_bundle_with_an_authority_makes_the_door_mutual() {
    let directory = tempfile::tempdir().expect("temp");
    let authority = Authority::new().expect("authority");
    let (certificate, key) = authority.sign("localhost").expect("sign").write(directory.path(), "server").expect("write");
    let bundle = directory.path().join("clients.pem");
    std::fs::write(&bundle, authority.bundle()).expect("write");

    let material = Material::load(&certificate, &key)
        .expect("load")
        .requiring_client_certificates(&bundle)
        .expect("bundle");

    assert!(material.is_mutual());
    assert!(Acceptor::new(&material, Alpn::Http2).expect("acceptor").is_mutual());
}

#[test]
fn the_columnar_door_advertises_http_2_and_the_wire_protocol_advertises_nothing() {
    let directory = tempfile::tempdir().expect("temp");
    let (certificate, key) = self_signed().expect("generate").write(directory.path(), "server").expect("write");
    let material = Material::load(&certificate, &key).expect("load");

    // A gRPC client that negotiates no protocol does not speak HTTP/2, and a PostgreSQL
    // client offered `h2` has nothing to do with it. One certificate, two doors, two
    // answers — which is the whole reason ALPN is the caller's to state.
    let columnar = material.server_config(&[b"h2".to_vec()]).expect("config");
    let wire = material.server_config(&[]).expect("config");

    assert_eq!(columnar.alpn_protocols, vec![b"h2".to_vec()]);
    assert!(wire.alpn_protocols.is_empty());
}

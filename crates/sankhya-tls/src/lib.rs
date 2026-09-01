//! Transport security: one certificate loader, both doors.
//!
//! # Why this is a crate and not two implementations
//!
//! SANKHYA serves two doors --- the columnar one over gRPC and the PostgreSQL wire
//! protocol --- and each transport ships its own way of being told about a certificate.
//! Taking both would mean two parsers, two sets of refusals, and two answers to *"is this
//! key the one for this certificate?"*, which is precisely the kind of divergence a person
//! discovers at three in the morning on the door they use less.
//!
//! So certificates are loaded **here, once**. Both doors are wrapped in an [`Acceptor`]
//! built from the same [`Material`], and a misconfiguration is refused identically whichever
//! door it was configured on.
//!
//! # The provider is named, never installed
//!
//! `rustls` lets a process install a global default cryptographic provider. This crate never
//! does, and never relies on one having been installed: every configuration is built with
//! [`provider`] passed explicitly.
//!
//! The reason is that a global default is **last-writer-wins across an entire dependency
//! graph**. A library that installs `aws-lc-rs` at startup would silently change what this
//! server negotiates, and nothing in a test or a log would say so. Naming the provider at
//! every construction costs one `Arc` clone and makes the question unanswerable in only one
//! way: by reading this file.
//!
//! # What is checked at load, and the one thing that is not
//!
//! Loading refuses an unreadable file, a file with no certificate in it, a file with no
//! private key in it, a key that does not match the certificate it was given with, and an
//! empty bundle of client trust anchors. Each is a distinct [`Refused`] naming the path,
//! because "TLS failed to start" is not a fault a person can act on.
//!
//! **Expiry is not checked.** Reading `notAfter` needs an X.509 parser this workspace does
//! not have and would carry for one field, and a certificate valid at startup expires while
//! the process runs anyway --- so a load-time check would be reassurance rather than a
//! control. An expired certificate is a failed handshake, which is visible where it happens.
//! Naming it here so that its absence is a decision rather than an oversight.

#![doc(html_root_url = "https://docs.rs/sankhya-tls")]

pub mod acceptor;
pub mod material;

pub use acceptor::{Acceptor, Alpn};
pub use material::{Material, Refused};

use std::sync::Arc;

/// The cryptographic provider every configuration in this process is built with.
///
/// `ring`, chosen once. See the module documentation for why this is a function rather
/// than a process-wide installation.
#[must_use]
pub fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

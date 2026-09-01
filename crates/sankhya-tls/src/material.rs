//! Certificates and keys, and the five ways loading them is refused.

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::{RootCertStore, ServerConfig};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Why transport security could not be configured.
///
/// Every variant names the path it read, because an operator fixing this is looking for a
/// file. A single "TLS configuration failed" would be true and useless.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Refused {
    /// A file could not be read, or held something that is not PEM.
    Unreadable {
        /// The file.
        path: PathBuf,
        /// What the reader said.
        why: String,
    },
    /// The file parsed and contains no certificate.
    ///
    /// Distinct from unreadable on purpose: a PEM file holding only a private key is a
    /// common mix-up, and it is a different sentence to a person than a corrupt file.
    NoCertificate {
        /// The file.
        path: PathBuf,
    },
    /// The file parsed and contains no private key.
    NoPrivateKey {
        /// The file.
        path: PathBuf,
    },
    /// The private key is not the key for the certificate it was given with.
    Mismatched {
        /// The certificate file.
        certificate: PathBuf,
        /// The key file.
        key: PathBuf,
    },
    /// A client trust bundle was configured and holds no usable anchor.
    ///
    /// Refused rather than treated as "trust nobody", which would start a server that
    /// rejects every client it was configured to accept.
    NoTrustAnchors {
        /// The bundle.
        path: PathBuf,
        /// What made it unusable, when the anchors parsed but could not be trusted.
        why: String,
    },
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable { path, why } => write!(
                f,
                "`{}` could not be read as PEM: {why}",
                path.display()
            ),
            Self::NoCertificate { path } => write!(
                f,
                "`{}` holds no certificate. A key file named as the certificate is the \
                 usual cause; the certificate is the file whose blocks say CERTIFICATE",
                path.display()
            ),
            Self::NoPrivateKey { path } => write!(
                f,
                "`{}` holds no private key. A certificate named as the key is the usual \
                 cause; the key is the file that never leaves the host",
                path.display()
            ),
            Self::Mismatched { certificate, key } => write!(
                f,
                "the key in `{}` is not the key for the certificate in `{}`. Both files \
                 loaded; they are simply a pair that was never one",
                key.display(),
                certificate.display()
            ),
            Self::NoTrustAnchors { path, why } => write!(
                f,
                "`{}` yields no usable client trust anchor ({why}). Refused rather than \
                 started: a server trusting nobody rejects every client it was configured \
                 to accept, and does it at connection time rather than here",
                path.display()
            ),
        }
    }
}

impl std::error::Error for Refused {}

/// A loaded certificate chain, its key, and optionally who may present a client
/// certificate.
#[derive(Debug)]
pub struct Material {
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
    certificate_path: PathBuf,
    key_path: PathBuf,
    clients: Option<Arc<dyn rustls::server::danger::ClientCertVerifier>>,
}

impl Material {
    /// Load a certificate chain and its private key.
    ///
    /// # Errors
    ///
    /// [`Refused`] when either file cannot be read, holds nothing of the kind it was named
    /// for, or the two are not a pair.
    pub fn load(certificate: &Path, key: &Path) -> Result<Self, Refused> {
        let chain = certificates(certificate)?;
        if chain.is_empty() {
            return Err(Refused::NoCertificate { path: certificate.to_path_buf() });
        }
        let key_der = PrivateKeyDer::from_pem_file(key).map_err(|error| match error {
            rustls::pki_types::pem::Error::NoItemsFound => {
                Refused::NoPrivateKey { path: key.to_path_buf() }
            }
            other => Refused::Unreadable {
                path: key.to_path_buf(),
                why: other.to_string(),
            },
        })?;

        let material = Self {
            chain,
            key: key_der,
            certificate_path: certificate.to_path_buf(),
            key_path: key.to_path_buf(),
            clients: None,
        };
        // Built once here so a mismatched pair is refused at load rather than at the first
        // connection — which is a different day, and usually somebody else's.
        material.server_config(&[])?;
        Ok(material)
    }

    /// Require a client certificate signed by an anchor in `bundle`.
    ///
    /// This is the mutual-TLS half of `FR-SEC-03`. Without it a client is authenticated by
    /// what it presents after the handshake; with it, the handshake itself is the first
    /// check.
    ///
    /// # Errors
    ///
    /// [`Refused`] when the bundle cannot be read or yields no usable anchor.
    pub fn requiring_client_certificates(mut self, bundle: &Path) -> Result<Self, Refused> {
        let anchors = certificates(bundle)?;
        if anchors.is_empty() {
            return Err(Refused::NoTrustAnchors {
                path: bundle.to_path_buf(),
                why: "the file holds no certificate".to_owned(),
            });
        }
        let mut store = RootCertStore::empty();
        for anchor in anchors {
            store.add(anchor).map_err(|error| Refused::NoTrustAnchors {
                path: bundle.to_path_buf(),
                why: error.to_string(),
            })?;
        }
        let verifier = WebPkiClientVerifier::builder_with_provider(
            Arc::new(store),
            crate::provider(),
        )
        .build()
        .map_err(|error| Refused::NoTrustAnchors {
            path: bundle.to_path_buf(),
            why: error.to_string(),
        })?;
        self.clients = Some(verifier);
        Ok(self)
    }

    /// Whether a client must present a certificate of its own.
    #[must_use]
    pub fn is_mutual(&self) -> bool {
        self.clients.is_some()
    }

    /// The server configuration, advertising `protocols` through ALPN.
    ///
    /// The protocol list is the caller's because the two doors differ: the columnar door
    /// speaks HTTP/2 and must say so, and the wire protocol negotiates nothing.
    ///
    /// # Errors
    ///
    /// [`Refused::Mismatched`] when the key is not the certificate's.
    pub fn server_config(&self, protocols: &[Vec<u8>]) -> Result<Arc<ServerConfig>, Refused> {
        let builder = ServerConfig::builder_with_provider(crate::provider())
            .with_safe_default_protocol_versions()
            .map_err(|error| Refused::Unreadable {
                path: self.certificate_path.clone(),
                why: error.to_string(),
            })?;
        let builder = match &self.clients {
            Some(verifier) => builder.with_client_cert_verifier(Arc::clone(verifier)),
            None => builder.with_no_client_auth(),
        };
        let mut config = builder
            .with_single_cert(self.chain.clone(), self.key.clone_key())
            .map_err(|_| Refused::Mismatched {
                certificate: self.certificate_path.clone(),
                key: self.key_path.clone(),
            })?;
        config.alpn_protocols = protocols.to_vec();
        Ok(Arc::new(config))
    }
}

/// Every certificate in a PEM file, in the order they appear.
fn certificates(path: &Path) -> Result<Vec<CertificateDer<'static>>, Refused> {
    match CertificateDer::pem_file_iter(path) {
        Ok(items) => items.collect::<Result<Vec<_>, _>>().map_err(|error| match error {
            rustls::pki_types::pem::Error::NoItemsFound => {
                Refused::NoCertificate { path: path.to_path_buf() }
            }
            other => Refused::Unreadable {
                path: path.to_path_buf(),
                why: other.to_string(),
            },
        }),
        Err(rustls::pki_types::pem::Error::NoItemsFound) => {
            Err(Refused::NoCertificate { path: path.to_path_buf() })
        }
        Err(other) => Err(Refused::Unreadable {
            path: path.to_path_buf(),
            why: other.to_string(),
        }),
    }
}

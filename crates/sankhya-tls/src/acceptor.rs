//! Wrapping a plain connection in TLS, and the timeout that keeps a stalled one cheap.

use crate::material::{Material, Refused};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_rustls::server::TlsStream;
use tokio_rustls::TlsAcceptor;

/// What a door advertises through ALPN.
///
/// Two doors, two answers, and the distinction is not cosmetic: a gRPC client that
/// negotiates no protocol will not speak HTTP/2, and a PostgreSQL client offered `h2` has
/// nothing to do with it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Alpn {
    /// HTTP/2, for the columnar door.
    Http2,
    /// Nothing. The wire protocol negotiates its own encryption and then speaks its own
    /// protocol; there is no application protocol to name.
    None,
}

impl Alpn {
    /// The protocol list, in the form `rustls` wants.
    fn protocols(self) -> Vec<Vec<u8>> {
        match self {
            Self::Http2 => vec![b"h2".to_vec()],
            Self::None => Vec::new(),
        }
    }
}

/// How long a connection may take to complete a handshake.
///
/// A TCP connection that opens and then says nothing costs a task and a buffer for as long
/// as it is allowed to. Without a bound, opening connections and never handshaking is a way
/// to exhaust a server with no traffic and no authentication — the same reasoning that
/// bounds the wire protocol's read buffer.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// A door's TLS acceptor.
#[derive(Clone)]
pub struct Acceptor {
    inner: TlsAcceptor,
    mutual: bool,
    handshake: Duration,
}

impl fmt::Debug for Acceptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Acceptor")
            .field("mutual", &self.mutual)
            .field("handshake", &self.handshake)
            .finish()
    }
}

impl Acceptor {
    /// Build an acceptor for one door.
    ///
    /// # Errors
    ///
    /// [`Refused`] when the material cannot produce a server configuration.
    pub fn new(material: &Material, alpn: Alpn) -> Result<Self, Refused> {
        let config = material.server_config(&alpn.protocols())?;
        Ok(Self {
            inner: TlsAcceptor::from(Arc::clone(&config)),
            mutual: material.is_mutual(),
            handshake: HANDSHAKE_TIMEOUT,
        })
    }

    /// Give this door a handshake deadline other than [`HANDSHAKE_TIMEOUT`].
    ///
    /// Present so the timeout is a value the tests can reach rather than ten seconds of
    /// waiting they would skip. A bound nobody exercises is a bound nobody knows is
    /// wired up — this crate would rather own that than assume it.
    #[must_use]
    pub const fn within(mut self, handshake: Duration) -> Self {
        self.handshake = handshake;
        self
    }

    /// Whether this door requires a client certificate.
    #[must_use]
    pub const fn is_mutual(&self) -> bool {
        self.mutual
    }

    /// Complete a handshake over an accepted connection.
    ///
    /// A handshake that does not finish within this door's deadline is abandoned. The
    /// error says which of the two happened, because a stalled client and a rejected one
    /// are different operational facts: the first is usually a scanner, the second is
    /// usually somebody's misconfigured certificate.
    ///
    /// # Errors
    ///
    /// [`Failed`] when the peer did not complete a handshake in time, or attempted one
    /// that was rejected.
    pub async fn accept<IO>(&self, stream: IO) -> Result<TlsStream<IO>, Failed>
    where
        IO: AsyncRead + AsyncWrite + Unpin,
    {
        match tokio::time::timeout(self.handshake, self.inner.accept(stream)).await {
            Ok(Ok(stream)) => Ok(stream),
            Ok(Err(error)) => Err(Failed::Rejected(error.to_string())),
            Err(_) => Err(Failed::TimedOut),
        }
    }
}

/// Why a connection did not become a TLS session.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Failed {
    /// The peer did not complete a handshake within the door's deadline.
    TimedOut,
    /// The handshake was attempted and rejected.
    Rejected(String),
}

impl fmt::Display for Failed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TimedOut => write!(
                f,
                "the peer opened a connection and did not complete a TLS handshake before \
                 the door's deadline"
            ),
            Self::Rejected(why) => write!(f, "the TLS handshake was rejected: {why}"),
        }
    }
}

impl std::error::Error for Failed {}

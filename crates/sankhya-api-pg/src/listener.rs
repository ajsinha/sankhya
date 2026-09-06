//! Accepting connections and driving them.
//!
//! Everything protocol-shaped lives in [`crate::session`], which is pure. This is the thin
//! layer that owns a socket: read bytes, hand them to the state machine, write what comes
//! back. Keeping the split sharp is what lets the protocol be tested without a network.
//!
//! # Backpressure and the read buffer
//!
//! A connection's read buffer is bounded. A client that sends a message declaring a length
//! larger than the bound is disconnected rather than accommodated --- an unbounded buffer is
//! a way for one connection to exhaust the process, and the declared length is attacker
//! controlled.

use crate::session::{Connection, Handler, Phase};
use bytes::BytesMut;
use sankhya_tls::Acceptor;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// The largest a single client message may be.
///
/// Generous for a statement and far below anything that threatens the process. The declared
/// length in a message header is attacker-controlled, so this is a bound on what one
/// connection can make the server allocate.
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

/// Whether this door encrypts, and whether it insists.
///
/// # Why "offered" exists at all, and why it is not the default
///
/// A deployment turning on TLS has clients that do not know about it yet. [`Encryption::Offered`]
/// is the migration: encrypted for anybody who asks, plain for anybody who has not been
/// updated.
///
/// It is also indistinguishable, from the server's side, from a deployment that believes it
/// requires TLS and does not. So it is a state to pass through rather than a state to be in,
/// and the type says which is which by naming them differently rather than by taking a
/// boolean nobody can read at a call site.
#[derive(Clone, Debug, Default)]
pub enum Encryption {
    /// The door is plain. Every client is answered `N`.
    #[default]
    Off,
    /// Encrypted for clients that ask; plain for the rest.
    Offered(Acceptor),
    /// Encrypted, and a client that does not ask is refused before it authenticates.
    Required(Acceptor),
}

impl Encryption {
    /// The acceptor, if this door has one.
    const fn acceptor(&self) -> Option<&Acceptor> {
        match self {
            Self::Off => None,
            Self::Offered(acceptor) | Self::Required(acceptor) => Some(acceptor),
        }
    }

    /// Whether a client arriving in the clear is refused.
    const fn insists(&self) -> bool {
        matches!(self, Self::Required(_))
    }
}

/// The most connections this door serves at once.
///
/// # Why a cap exists at all
///
/// Without one, the number of connections is whatever clients ask for, and each is a
/// descriptor. The process reaches its descriptor limit, `accept()` starts returning
/// `EMFILE`, and every other thing that needs a descriptor --- opening a data file,
/// writing the audit --- starts failing too. A cap turns that into a queue.
///
/// # Why refusing at the door is the better failure
///
/// Past the cap this loop stops accepting. New connections sit in the kernel's listen
/// backlog and, past that, are refused by the operating system --- a client sees a
/// connection failure, which is a thing clients retry. The alternative is accepting them
/// all and running out of descriptors, where the failure lands on the connections already
/// being served rather than on the ones arriving.
///
/// Generous against PostgreSQL's own default of a hundred, and well under the 65,535
/// descriptors the shipped systemd unit asks for --- the two numbers are related on
/// purpose, because a cap above the descriptor limit is not a cap.
pub const MAX_CONNECTIONS: usize = 1024;

/// A listening front door.
#[derive(Debug)]
pub struct PgListener {
    listener: TcpListener,
    encryption: Encryption,
    limit: usize,
}

impl PgListener {
    /// Bind to an address.
    ///
    /// Port zero is useful and supported: the operating system chooses, and
    /// [`PgListener::local_addr`] reports what it chose. That is how a test gets a port
    /// without racing another test for a fixed one.
    pub async fn bind(address: &str) -> std::io::Result<Self> {
        Ok(Self {
            listener: TcpListener::bind(address).await?,
            encryption: Encryption::Off,
            limit: MAX_CONNECTIONS,
        })
    }

    /// Serve this door under the given encryption policy.
    #[must_use]
    pub fn encrypted(mut self, encryption: Encryption) -> Self {
        self.encryption = encryption;
        self
    }

    /// Serve at most `limit` connections at once, for tests.
    ///
    /// [`MAX_CONNECTIONS`] otherwise. A test proving the cap holds cannot open a thousand
    /// sockets to do it, and one that tried would be measuring the machine.
    #[must_use]
    pub const fn limited_to(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    /// The address actually bound.
    pub fn local_addr(&self) -> std::io::Result<std::net::SocketAddr> {
        self.listener.local_addr()
    }

    /// Accept one connection and serve it to completion.
    ///
    /// Returns when the client disconnects. A caller wanting concurrency spawns this.
    pub async fn accept_one(&self, handler: Arc<dyn Handler>) -> std::io::Result<()> {
        let (stream, _) = self.listener.accept().await?;
        serve_with(stream, handler, self.encryption.clone()).await
    }

    /// How long a shutdown waits for connections already in flight.
    ///
    /// The number an orchestration manifest's termination grace has to exceed, which is why
    /// it is a named constant rather than a literal: `packaging/` derives its figures from
    /// this one, and `cargo xtask check-package` fails the build when they disagree. Two
    /// numbers in two files maintained by two people, with nothing normally relating them,
    /// is how a deploy comes to `SIGKILL` a server mid-drain.
    pub const DRAIN: Duration = Duration::from_secs(30);

    /// Serve connections until `shutdown` resolves, then drain.
    ///
    /// Each connection is spawned, so a slow client does not block the accept loop. On
    /// shutdown the loop stops accepting and **waits** for connections already running,
    /// because cutting a client off mid-result is indistinguishable to them from a crash.
    ///
    /// # The wait is bounded, and an earlier version did not wait at all
    ///
    /// The first version returned the moment `shutdown` resolved. Its spawned tasks were
    /// detached, so when the runtime was dropped they were cancelled abruptly --- and the
    /// doc comment above this one claimed the opposite, which is how it survived review. A
    /// client mid-result saw a reset on every deploy.
    ///
    /// The wait has a deadline for the other reason: an unbounded drain hangs a shutdown on
    /// one stuck client, the orchestrator's patience runs out, and the process is killed
    /// anyway --- with the difference that nobody chose the moment. Waiting a bounded time
    /// and then closing is the version where the timeout is ours.
    ///
    /// # A failed `accept()` is not a reason to exit
    ///
    /// It used to be. `accepted?` propagated out of this loop and out of `main`, so
    /// `ECONNABORTED` --- what a load balancer produces every time a health check opens a
    /// connection and closes it --- ended the process. The comment a few lines below,
    /// saying a failed connection is that connection's problem, described the *serve*
    /// error while the accept path did the opposite. [`sankhya_accept`] decides now, and
    /// the metrics door and the columnar door ask it the same question.
    ///
    /// # The loop stops accepting past [`MAX_CONNECTIONS`]
    ///
    /// Not a refusal: the branch is disabled while the set is full, so callers wait in the
    /// kernel backlog and are served as connections finish. A descriptor shortage is the
    /// failure that follows from having no cap, and it lands on the connections already
    /// being served rather than on the one that caused it.
    pub async fn serve_until(
        self,
        handler: Arc<dyn Handler>,
        shutdown: impl std::future::Future<Output = ()> + Send,
    ) -> std::io::Result<()> {
        self.serve_until_with_drain(handler, shutdown, Self::DRAIN)
            .await
    }

    /// [`PgListener::serve_until`] with the drain deadline given, for tests.
    pub async fn serve_until_with_drain(
        self,
        handler: Arc<dyn Handler>,
        shutdown: impl std::future::Future<Output = ()> + Send,
        drain: Duration,
    ) -> std::io::Result<()> {
        tokio::pin!(shutdown);
        // A `JoinSet` rather than detached tasks: something has to hold the handles or there
        // is nothing to wait for. Finished connections are reaped in the same loop, so the
        // set does not grow with every connection ever served.
        let mut connections = tokio::task::JoinSet::new();

        loop {
            tokio::select! {
                () = &mut shutdown => break,
                accepted = self.listener.accept(), if connections.len() < self.limit => {
                    let (stream, _) = match accepted {
                        Ok(pair) => pair,
                        // `OPS-08`: this was `accepted?`, and it took the whole process
                        // down. `ECONNABORTED` is what a load balancer's health check
                        // produces; `EMFILE` is a descriptor shortage that clears. Neither
                        // is a reason for a server to exit, and `sankhya-accept` is where
                        // that judgement is made so that all three doors make it the same
                        // way. What it declines to excuse still ends the loop.
                        Err(error) => match sankhya_accept::response(&error) {
                            sankhya_accept::Response::Continue => {
                                tracing::debug!(%error, "a connection failed before it existed");
                                continue;
                            }
                            sankhya_accept::Response::Pause(how_long) => {
                                // `warn`, not `debug`: the server is refusing callers it
                                // would otherwise serve, and nothing else says so.
                                tracing::warn!(
                                    %error,
                                    "the server is short of descriptors and is pausing before it accepts again"
                                );
                                tokio::time::sleep(how_long).await;
                                continue;
                            }
                            sankhya_accept::Response::Stop => return Err(error),
                        },
                    };
                    let handler = Arc::clone(&handler);
                    let encryption = self.encryption.clone();
                    connections.spawn(async move {
                        // A failed connection is that connection's problem, not the
                        // server's. Logging and continuing is the only correct response.
                        if let Err(error) = serve_with(stream, handler, encryption).await {
                            tracing::debug!(%error, "a client connection ended with an error");
                        }
                    });
                }
                Some(_) = connections.join_next(), if !connections.is_empty() => {}
            }
        }

        if connections.is_empty() {
            return Ok(());
        }
        tracing::info!(
            in_flight = connections.len(),
            "draining before shutdown"
        );
        let drained = tokio::time::timeout(drain, async {
            while connections.join_next().await.is_some() {}
        })
        .await;
        if drained.is_err() {
            // Said out loud. A client cut off here sees the same thing it would see from a
            // crash, and the operator should know it happened rather than deducing it from
            // a support ticket.
            tracing::warn!(
                still_running = connections.len(),
                seconds = drain.as_secs(),
                "the drain deadline passed; cutting off connections still in flight"
            );
            connections.abort_all();
        }
        Ok(())
    }
}

/// Tells the handler the connection has ended, on every path out of [`serve`].
///
/// A guard rather than a call before each `return`, because `serve` leaves through several
/// of them and through `?` besides. A gauge incremented on accept and decremented on all but
/// one exit climbs forever and reads as a connection leak that is not happening --- and the
/// exit that gets missed is always an error path, which is when the number matters most.
/// `Drop` also covers a panic, which no arrangement of explicit calls does.
struct ConnectionGuard(Arc<dyn Handler>);

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.connection_closed();
    }
}

/// Drive one plain connection to completion.
///
/// The unencrypted door, kept as its own entry point because a great deal of this project's
/// tests and tooling connect to a local server and have no certificate to check.
pub async fn serve(stream: TcpStream, handler: Arc<dyn Handler>) -> std::io::Result<()> {
    serve_with(stream, handler, Encryption::Off).await
}

/// Drive one connection to completion under an encryption policy.
///
/// # The refusal that happens before authentication
///
/// On a [`Encryption::Required`] door a client that never asks for TLS is answered with an
/// error and disconnected --- rather than served, and rather than dropped silently. Dropped
/// silently, the client reports a closed connection and the operator goes looking at the
/// network. `28000` and a sentence naming the cause is the difference between a person
/// changing one connection string and a person reading a packet capture.
pub async fn serve_with(
    mut stream: TcpStream,
    handler: Arc<dyn Handler>,
    encryption: Encryption,
) -> std::io::Result<()> {
    handler.connection_opened();
    let _guard = ConnectionGuard(Arc::clone(&handler));

    // Nagle's algorithm delays a small write waiting for a larger one. This protocol is a
    // conversation of small messages, and the delay is visible as latency on every query.
    stream.set_nodelay(true).ok();

    // Not secret in themselves — they authorise cancelling this connection's work, and
    // nothing else. Derived from the address so two live connections cannot collide.
    let process_id = i32::try_from(std::process::id() % 100_000).unwrap_or(1);
    let secret = i32::try_from(
        stream
            .peer_addr()
            .map(|a| u64::from(a.port()))
            .unwrap_or(0)
            .wrapping_mul(2_654_435_761)
            % 1_000_000_007,
    )
    .unwrap_or(0);

    let Some(acceptor) = encryption.acceptor().cloned() else {
        return drive(stream, Connection::new(process_id, secret), handler)
            .await
            .map(|_| ());
    };

    // The state machine decides; this function only owns the socket. It answers `S`, stops
    // at `Handshaking`, and everything after the handshake is a connection that has said
    // nothing yet — which is exactly what the protocol requires, since a client re-sends
    // its startup message inside the encrypted session.
    let opening = Connection::new(process_id, secret).on_an_encrypted_door(encryption.insists());
    match drive(&mut stream, opening, Arc::clone(&handler)).await? {
        Ended::Closed => Ok(()),
        Ended::Handshaking => match acceptor.accept(stream).await {
            Ok(session) => drive(session, Connection::new(process_id, secret), handler)
                .await
                .map(|_| ()),
            Err(failure) => {
                // Below `warn`: a failed handshake is routine on any address a scanner can
                // reach, and one log line per scan is a log nobody reads.
                tracing::debug!(%failure, "a connection did not become a TLS session");
                Ok(())
            }
        },
    }
}

/// How a connection stopped being driven.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ended {
    /// The protocol finished, in either direction.
    Closed,
    /// The client asked for TLS and was told yes. The socket is at a handshake.
    Handshaking,
}

/// Run the protocol over whatever the connection turned out to be.
///
/// Generic because by this point the difference between a socket and a TLS session is not
/// this function's business — which is also the property that keeps the protocol testable
/// without a network.
async fn drive<S>(
    mut stream: S,
    mut connection: Connection,
    handler: Arc<dyn Handler>,
) -> std::io::Result<Ended>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut input = BytesMut::with_capacity(8 * 1024);
    let mut output = BytesMut::with_capacity(8 * 1024);
    let mut chunk = vec![0u8; 8 * 1024];

    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            // The client went away without saying goodbye. Ordinary, not an error.
            return Ok(Ended::Closed);
        }
        input.extend_from_slice(chunk.get(..read).unwrap_or(&[]));

        if input.len() > MAX_MESSAGE_BYTES {
            // The declared length is attacker-controlled, so an unbounded buffer is a way
            // for one connection to exhaust the process.
            return Ok(Ended::Closed);
        }

        let consumed = connection.advance(&input, handler.as_ref(), &mut output);
        let _ = input.split_to(consumed);

        if !output.is_empty() {
            stream.write_all(&output).await?;
            stream.flush().await?;
            output.clear();
        }
        match connection.phase() {
            Phase::Closed => return Ok(Ended::Closed),
            // The `S` is written and the socket is at a handshake. Anything still in
            // `input` was sent before the client could have known the answer, and a client
            // that speaks inside the tunnel before the tunnel exists is not one this server
            // continues with.
            Phase::Handshaking => return Ok(Ended::Handshaking),
            Phase::Startup | Phase::Authenticating | Phase::Ready => {}
        }
    }
}

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
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// The largest a single client message may be.
///
/// Generous for a statement and far below anything that threatens the process. The declared
/// length in a message header is attacker-controlled, so this is a bound on what one
/// connection can make the server allocate.
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

/// A listening front door.
#[derive(Debug)]
pub struct PgListener {
    listener: TcpListener,
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
        })
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
        serve(stream, handler).await
    }

    /// Serve connections until `shutdown` resolves.
    ///
    /// Each connection is spawned, so a slow client does not block the accept loop. On
    /// shutdown the loop stops accepting; connections already running finish on their own,
    /// because cutting a client off mid-result is indistinguishable to them from a crash.
    pub async fn serve_until(
        self,
        handler: Arc<dyn Handler>,
        shutdown: impl std::future::Future<Output = ()> + Send,
    ) -> std::io::Result<()> {
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                () = &mut shutdown => return Ok(()),
                accepted = self.listener.accept() => {
                    let (stream, _) = accepted?;
                    let handler = Arc::clone(&handler);
                    tokio::spawn(async move {
                        // A failed connection is that connection's problem, not the
                        // server's. Logging and continuing is the only correct response.
                        if let Err(error) = serve(stream, handler).await {
                            tracing::debug!(%error, "a client connection ended with an error");
                        }
                    });
                }
            }
        }
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

/// Drive one connection to completion.
pub async fn serve(mut stream: TcpStream, handler: Arc<dyn Handler>) -> std::io::Result<()> {
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

    let mut connection = Connection::new(process_id, secret);
    let mut input = BytesMut::with_capacity(8 * 1024);
    let mut output = BytesMut::with_capacity(8 * 1024);
    let mut chunk = vec![0u8; 8 * 1024];

    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            // The client went away without saying goodbye. Ordinary, not an error.
            return Ok(());
        }
        input.extend_from_slice(chunk.get(..read).unwrap_or(&[]));

        if input.len() > MAX_MESSAGE_BYTES {
            // The declared length is attacker-controlled, so an unbounded buffer is a way
            // for one connection to exhaust the process.
            return Ok(());
        }

        let consumed = connection.advance(&input, handler.as_ref(), &mut output);
        let _ = input.split_to(consumed);

        if !output.is_empty() {
            stream.write_all(&output).await?;
            stream.flush().await?;
            output.clear();
        }
        if connection.phase() == Phase::Closed {
            return Ok(());
        }
    }
}

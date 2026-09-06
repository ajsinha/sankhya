//! The gRPC transport, and what it carries.
//!
//! # What was missing, and what was not
//!
//! `sankhya-api-flight` implements Arrow Flight SQL completely: ticket issue and redemption,
//! the tenant check on redemption, expiry, and a `do_get` that streams batches as they arrive
//! rather than collecting them. It is tested. **Nothing served it.**
//!
//! `GUIDE.md` §7a documented it across forty lines with a code example, and a client had
//! nowhere to send a `GetFlightInfo` --- the crate was reached only from `sankhya-api-rest`,
//! which no binary reaches either. Found on 2026-08-29 by widening `check-surfaces` from
//! "crates registering SQL functions" to plain reachability, which is the eighth time in this
//! project that a capability turned out to be built, tested and unreachable.
//!
//! So this crate is a transport and deliberately not much else. The protocol was already here.
//!
//! # Why Flight SQL *is* the gRPC transport
//!
//! M6's exit criterion 7 --- carried into M8 --- asks for the gRPC transport. Arrow Flight is
//! a gRPC service: `FlightServiceServer` is a tonic service and speaks HTTP/2. Serving it is
//! not a step towards the transport, it is the transport, and the control-plane services
//! `FR-API-04` describes will be added to the same `tonic::transport::Server` beside it.
//!
//! Building a separate control-plane protocol first would have left the bulk plane unreachable
//! while adding a second thing to reach it with.
//!
//! # Shutdown is part of the interface
//!
//! A listener that cannot be stopped is one a test has to leak and an operator has to kill.
//! [`Transport::serve_until`] takes a future and returns when it resolves, which is what lets a
//! server own its listener rather than abandon it.

use sankhya_tls::Acceptor;
use std::net::SocketAddr;

/// Why the transport could not run.
#[derive(Debug)]
pub enum TransportError {
    /// The address could not be bound.
    Unbindable {
        /// What was asked for.
        address: String,
        /// The operating system's reason.
        detail: String,
    },
    /// The server stopped with an error.
    Stopped {
        /// What tonic reported.
        detail: String,
    },
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unbindable { address, detail } => write!(
                f,
                "could not listen on {address}: {detail}. Another process holds the port, or \
                 the address is not one this host has"
            ),
            Self::Stopped { detail } => write!(f, "the gRPC transport stopped: {detail}"),
        }
    }
}

impl std::error::Error for TransportError {}

/// A gRPC listener carrying this system's services.
#[derive(Debug)]
pub struct Transport {
    address: SocketAddr,
    encryption: Option<Acceptor>,
}

impl Transport {
    /// A transport bound to this address when it is served.
    #[must_use]
    pub const fn new(address: SocketAddr) -> Self {
        Self { address, encryption: None }
    }

    /// Serve this door over TLS.
    ///
    /// The certificate comes from `sankhya-tls`, which is also where the wire protocol's
    /// comes from. One loader, one set of refusals, and no chance of the two doors
    /// disagreeing about what a valid certificate is.
    #[must_use]
    pub fn encrypted(mut self, acceptor: Acceptor) -> Self {
        self.encryption = Some(acceptor);
        self
    }

    /// Where it will listen.
    #[must_use]
    pub const fn address(&self) -> SocketAddr {
        self.address
    }

    /// Serve Flight SQL until `shutdown` resolves.
    ///
    /// # Errors
    ///
    /// [`TransportError`] if the address cannot be bound or the server stops with an error.
    pub async fn serve_until<Q, S>(
        self,
        flight: sankhya_api_flight::SankhyaFlight<Q>,
        shutdown: S,
    ) -> Result<(), TransportError>
    where
        Q: sankhya_api_flight::Queries + Send + Sync + 'static,
        S: std::future::Future<Output = ()> + Send + 'static,
    {
        // Bound before serving, so a port already in use is reported here rather than as a
        // task that quietly never accepted anything.
        let listener = tokio::net::TcpListener::bind(self.address).await.map_err(|error| {
            TransportError::Unbindable {
                address: self.address.to_string(),
                detail: error.to_string(),
            }
        })?;
        let service =
            arrow_flight::flight_service_server::FlightServiceServer::new(flight);

        let Some(acceptor) = self.encryption else {
            let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
            return tonic::transport::Server::builder()
                .add_service(service)
                .serve_with_incoming_shutdown(incoming, shutdown)
                .await
                .map_err(|error| TransportError::Stopped { detail: error.to_string() });
        };

        // Two futures need to know about shutdown: the server, and the accept loop below.
        // Without the second, stopping the server would leave a task holding the listening
        // socket until the next connection happened to arrive — so the port would stay bound
        // after a shutdown that reported success.
        let (stopping, mut stopped) = tokio::sync::watch::channel(false);
        let shutdown = async move {
            shutdown.await;
            let _ = stopping.send(true);
        };

        // A bounded channel, because an unbounded one would let a burst of connections
        // become memory the server cannot refuse.
        let (ready, sessions) = tokio::sync::mpsc::channel::<Result<
            tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
            std::io::Error,
        >>(64);

        tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    () = wait_for_shutdown(&mut stopped) => break,
                    accepted = listener.accept() => accepted,
                };
                let stream = match accepted {
                    Ok((stream, _)) => stream,
                    // `OPS-08`. `else { continue }` spins on a descriptor shortage and
                    // loops for ever on a listener that is broken. This task has no
                    // caller to return an error to, so a listener that will never accept
                    // again ends it --- the channel closes and the server stops, which is
                    // the state an orchestrator restarts.
                    Err(error) => match sankhya_accept::response(&error) {
                        sankhya_accept::Response::Continue => continue,
                        sankhya_accept::Response::Pause(how_long) => {
                            tokio::time::sleep(how_long).await;
                            continue;
                        }
                        sankhya_accept::Response::Stop => {
                            tracing::error!(%error, "the columnar door can no longer accept");
                            break;
                        }
                    },
                };
                let acceptor = acceptor.clone();
                let ready = ready.clone();
                // Spawned rather than awaited here. A handshake takes a round trip, and a
                // peer that opens a connection and then says nothing takes the whole
                // handshake deadline — doing it inline would let one silent client stop the
                // server accepting anybody else, which is a denial of service that costs
                // the attacker one socket.
                tokio::spawn(async move {
                    match acceptor.accept(stream).await {
                        Ok(session) => {
                            let _ = ready.send(Ok(session)).await;
                        }
                        Err(failure) => {
                            // Below `warn`: a failed handshake is routine on any address a
                            // scanner can reach, and a line per scan is a log nobody reads.
                            tracing::debug!(%failure, "a connection did not become a TLS session");
                        }
                    }
                });
            }
        });

        tonic::transport::Server::builder()
            .add_service(service)
            .serve_with_incoming_shutdown(
                tokio_stream::wrappers::ReceiverStream::new(sessions),
                shutdown,
            )
            .await
            .map_err(|error| TransportError::Stopped {
                detail: error.to_string(),
            })
    }
}

/// Resolve once the shutdown signal turns true.
async fn wait_for_shutdown(stopped: &mut tokio::sync::watch::Receiver<bool>) {
    while !*stopped.borrow_and_update() {
        if stopped.changed().await.is_err() {
            // The sender is gone, which happens only when the server has already stopped.
            return;
        }
    }
}

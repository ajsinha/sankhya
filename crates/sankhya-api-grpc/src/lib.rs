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
}

impl Transport {
    /// A transport bound to this address when it is served.
    #[must_use]
    pub const fn new(address: SocketAddr) -> Self {
        Self { address }
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
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);

        tonic::transport::Server::builder()
            .add_service(arrow_flight::flight_service_server::FlightServiceServer::new(
                flight,
            ))
            .serve_with_incoming_shutdown(incoming, shutdown)
            .await
            .map_err(|error| TransportError::Stopped {
                detail: error.to_string(),
            })
    }
}

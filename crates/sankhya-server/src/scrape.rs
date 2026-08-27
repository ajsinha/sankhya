//! The metrics endpoint.
//!
//! # Its own port, and only one route
//!
//! Separate from the wire protocol's listener, so that a collector scraping every fifteen
//! seconds never touches the port clients connect on, and so the endpoint can be bound to an
//! interface the clients cannot reach. A metrics endpoint reachable from wherever queries
//! come from is a small, permanent information disclosure: series names and label values
//! describe the deployment.
//!
//! One route, `GET /metrics`, and everything else is a 404. This is not an HTTP server and
//! must not grow into one --- the REST gateway is `M6` §10.8 and belongs there, where it can
//! have the authentication, the size caps and the Flight-ticket fallback that make it
//! defensible.
//!
//! # Deliberately unauthenticated, and deliberately narrow because of it
//!
//! Prometheus's convention is an unauthenticated endpoint on a private interface, and adding
//! authentication that the standard collector cannot use would produce a metrics endpoint
//! nobody scrapes. What makes that acceptable is that no label may carry tenant data --- the
//! metric catalogue enforces it structurally rather than by review --- so there is nothing
//! here to disclose beyond the shape of the deployment.

use crate::wiring::Server;
use sankhya_metrics::catalogue::ALL;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// The largest request line this endpoint will read.
///
/// A scrape request is a few dozen bytes. The bound is here because the alternative is an
/// unbounded read on an unauthenticated socket, which is a way for anyone who can reach the
/// port to exhaust the process.
const MAX_REQUEST_BYTES: usize = 8 * 1024;

/// Serve `/metrics` until `shutdown` resolves.
///
/// # Errors
///
/// When the listener cannot accept.
pub async fn serve_until(
    listener: TcpListener,
    server: Arc<Server>,
    shutdown: impl std::future::Future<Output = ()> + Send,
) -> std::io::Result<()> {
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            () = &mut shutdown => return Ok(()),
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                let server = Arc::clone(&server);
                // Spawned, so a collector that opens a connection and never sends anything
                // cannot stop the next scrape from being served.
                tokio::spawn(async move {
                    respond(stream, &server).await.ok();
                });
            }
        }
    }
}

/// Read one request and answer it.
async fn respond(mut stream: TcpStream, server: &Server) -> std::io::Result<()> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(());
        }
        buffer.extend_from_slice(chunk.get(..read).unwrap_or(&[]));
        if buffer.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if buffer.len() > MAX_REQUEST_BYTES {
            return stream
                .write_all(&response(413, "text/plain", "request too large"))
                .await;
        }
    }

    let request = String::from_utf8_lossy(&buffer);
    if !is_metrics_request(request.lines().next().unwrap_or_default()) {
        let body = response(404, "text/plain", "only GET /metrics is served here");
        return stream.write_all(&body).await;
    }

    // Refreshed at the moment of the scrape rather than on a timer, so a gauge is never
    // reporting a number from the previous era.
    server.refresh_table_gauges();
    let body = server.metrics().render(ALL);
    stream
        .write_all(&response(200, "text/plain; version=0.0.4", &body))
        .await
}

/// Whether a request line asks for exactly this endpoint's one route.
///
/// The target is compared whole rather than by prefix. `starts_with("GET /metrics")` was the
/// first version and it served `GET /metrics/../etc/passwd` --- harmless here, because this
/// endpoint reads no files, and exactly the shape that becomes a traversal the moment
/// somebody adds one. A route match that is right only because of what the handler happens
/// not to do is not a route match.
#[must_use]
pub fn is_metrics_request(line: &str) -> bool {
    let mut parts = line.split_whitespace();
    if parts.next() != Some("GET") {
        return false;
    }
    let Some(target) = parts.next() else {
        return false;
    };
    // A query string is permitted and ignored: some collectors append one, and refusing it
    // would fail a scrape for a reason nobody would guess from a 404.
    let path = target.split_once('?').map_or(target, |(path, _)| path);
    path == "/metrics"
}

/// A complete HTTP/1.1 response, with `Connection: close`.
///
/// Closing rather than keeping alive: a scrape is one request every several seconds, and a
/// keep-alive pool here would be state to manage for no benefit.
///
/// Returns owned bytes. An earlier version returned `&'static [u8]` by leaking the string,
/// which compiled and was a slow leak of one response per scrape --- a few hundred bytes
/// every fifteen seconds, forever, in the component whose entire job is to report that sort
/// of thing.
#[must_use]
pub fn response(status: u16, content_type: &str, body: &str) -> Vec<u8> {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        _ => "Payload Too Large",
    };
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: \
         {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

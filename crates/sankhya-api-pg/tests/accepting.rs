//! What the door does when it cannot accept, and when it will not.
//!
//! `OPS-08`. Two properties that look unrelated and are the same failure seen from both
//! ends: a server that exits because one `accept()` failed, and a server that runs out of
//! descriptors because nothing ever stopped it accepting.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_api_pg::catalog::CatalogTable;
use sankhya_api_pg::listener::{PgListener, MAX_CONNECTIONS};
use sankhya_api_pg::session::{Caller, Handler, QueryFailure, QueryResult};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A handler whose query blocks until the test lets it go.
struct Held {
    started: AtomicUsize,
    released: AtomicBool,
}

impl Held {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            started: AtomicUsize::new(0),
            released: AtomicBool::new(false),
        })
    }
}

impl Handler for Held {
    fn requires_password(&self, _parameters: &[(String, String)]) -> bool {
        false
    }

    fn authenticate(
        &self,
        _parameters: &[(String, String)],
        _password: Option<&[u8]>,
    ) -> Result<(), QueryFailure> {
        Ok(())
    }

    fn query(&self, _sql: &str, _caller: &Caller<'_>) -> Result<QueryResult, QueryFailure> {
        self.started.fetch_add(1, Ordering::SeqCst);
        // `block_in_place` for the reason `drain.rs` uses it: the protocol handler is
        // synchronous, so a query that waits blocks the worker thread it is on, and a bare
        // sleep here starves the runtime the observer needs.
        tokio::task::block_in_place(|| {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !self.released.load(Ordering::SeqCst) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        Ok(QueryResult {
            fields: Vec::new(),
            rows: Vec::new(),
            tag: "SELECT 0".to_string(),
        })
    }

    fn visible_tables(&self, _caller: &Caller<'_>) -> Vec<CatalogTable> {
        Vec::new()
    }
}

/// A startup packet, and the reply read to `ReadyForQuery`.
async fn shake_hands(stream: &mut tokio::net::TcpStream, within: Duration) -> bool {
    let mut body = 196_608i32.to_be_bytes().to_vec();
    body.extend_from_slice(b"user\0accepting\0\0");
    let mut startup = Vec::new();
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    startup.extend_from_slice(&((body.len() + 4) as i32).to_be_bytes());
    startup.extend_from_slice(&body);
    if stream.write_all(&startup).await.is_err() {
        return false;
    }

    let mut seen = Vec::new();
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        let mut scratch = [0u8; 1024];
        let left = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(left, stream.read(&mut scratch)).await {
            Ok(Ok(0)) | Ok(Err(_)) => return false,
            Ok(Ok(read)) => {
                seen.extend_from_slice(&scratch[..read]);
                if seen.contains(&b'Z') {
                    return true;
                }
            }
            Err(_) => return false,
        }
    }
    false
}

/// Send a query and do not wait for its reply.
async fn ask(stream: &mut tokio::net::TcpStream) {
    // Not `SELECT 1`: a driver's liveness ping is answered from the catalogue without ever
    // reaching the handler, so a test waiting for the handler would wait for ever.
    let payload = "SELECT id FROM sales.orders\0";
    let mut message = vec![b'Q'];
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    message.extend_from_slice(&((payload.len() + 4) as i32).to_be_bytes());
    message.extend_from_slice(payload.as_bytes());
    stream.write_all(&message).await.expect("query");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_caller_waits_rather_than_costing_a_descriptor() {
    // The cap. Before it there was none: the number of connections was whatever clients
    // asked for, each one a descriptor, and the process reached its limit --- at which
    // point every other thing needing a descriptor started failing too.
    //
    // A cap of one, because the property is "past the cap, the loop stops accepting", and
    // proving it with 1,024 sockets would be measuring the machine rather than the code.
    let handler = Held::new();
    let listener = PgListener::bind("127.0.0.1:0")
        .await
        .expect("a port")
        .limited_to(1);
    let address = listener.local_addr().expect("an address");

    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let serving = {
        let handler: Arc<dyn Handler> = Arc::clone(&handler) as Arc<_>;
        tokio::spawn(async move {
            listener
                .serve_until_with_drain(
                    handler,
                    async {
                        stopped.await.ok();
                    },
                    Duration::from_secs(5),
                )
                .await
        })
    };

    let mut first = tokio::net::TcpStream::connect(address).await.expect("connect");
    first.set_nodelay(true).ok();
    assert!(
        shake_hands(&mut first, Duration::from_secs(5)).await,
        "the first caller must be served"
    );
    ask(&mut first).await;
    let deadline = Instant::now() + Duration::from_secs(5);
    while handler.started.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(handler.started.load(Ordering::SeqCst), 1, "the first query never started");

    // The kernel completes this handshake from the listen backlog whether or not the server
    // has accepted it, so connecting proves nothing. Being *answered* does.
    let mut second = tokio::net::TcpStream::connect(address).await.expect("connect");
    second.set_nodelay(true).ok();
    assert!(
        !shake_hands(&mut second, Duration::from_millis(750)).await,
        "the second caller was served while the door was at its cap"
    );

    // And the wait is a queue, not a refusal: the slot frees and the caller is served.
    handler.released.store(true, Ordering::SeqCst);
    drop(first);
    assert!(
        shake_hands(&mut second, Duration::from_secs(10)).await,
        "the second caller was never served after the slot freed"
    );

    stop.send(()).ok();
    drop(second);
    let _ = tokio::time::timeout(Duration::from_secs(10), serving).await;
}

#[test]
fn the_cap_is_under_the_descriptor_limit_the_unit_asks_for() {
    // These two numbers are related on purpose and live in different files: a cap above the
    // process's descriptor limit is not a cap. `packaging/systemd/sankhya.service` asks for
    // 65,535, and a connection is not the only thing that needs one --- every open data
    // file does too, which is why this is far below rather than just below.
    assert!(MAX_CONNECTIONS <= 65_535 / 8, "the cap leaves no descriptors for data files");
    // Generous against PostgreSQL's own default of a hundred.
    assert!(MAX_CONNECTIONS >= 100);
}

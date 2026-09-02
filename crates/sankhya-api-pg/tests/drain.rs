//! What a shutdown does to a connection that is still talking.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_api_pg::catalog::CatalogTable;
use sankhya_api_pg::listener::PgListener;
use sankhya_api_pg::session::{Caller, Handler, QueryFailure, QueryResult};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A handler whose queries take as long as the test says.
struct Slow {
    query_takes: Duration,
    started: AtomicUsize,
    finished: AtomicUsize,
}

impl Slow {
    fn taking(query_takes: Duration) -> Arc<Self> {
        Arc::new(Self {
            query_takes,
            started: AtomicUsize::new(0),
            finished: AtomicUsize::new(0),
        })
    }
}

impl Handler for Slow {
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
        // `block_in_place` rather than a bare `thread::sleep`, because that is what the real
        // handler does and the difference is not cosmetic: the protocol handler is
        // synchronous, so a long query blocks the worker thread it is on. Without this the
        // runtime is starved — an earlier version of this test blocked its own polling loop
        // for the full ten seconds and then reported that the query had never started, when
        // it had started immediately and the observer could not run.
        tokio::task::block_in_place(|| std::thread::sleep(self.query_takes));
        self.finished.fetch_add(1, Ordering::SeqCst);
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

/// Wait until a query has reached the handler.
///
/// The condition is re-checked after the sleep and *before* the deadline is judged. The
/// first version asserted immediately after waking, so a loop that was descheduled past its
/// deadline reported "the query never started" about a query that had started at once — the
/// failure was in the observer, and it said so about the thing observed.
async fn await_started(handler: &Slow) {
    let waiting = Instant::now();
    loop {
        if handler.started.load(Ordering::SeqCst) > 0 {
            return;
        }
        assert!(
            waiting.elapsed() < Duration::from_secs(5),
            "the query never reached the handler"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Wait for the server task, with a bound.
///
/// Every wait in this file goes through here, and that is deliberate rather than tidy.
///
/// These tests hold their client sockets open, so a connection task does not end when its
/// query does — it goes back to reading and waits for a client that says nothing. The drain
/// deadline is the only thing that ends it. Awaiting the server task directly therefore turns
/// "somebody removed the deadline" into a test that **hangs**, which takes the whole build
/// with it and reports nothing.
///
/// That was not hypothetical: the mutation removing the deadline ran for over ten minutes
/// twice. Bounding one of the two tests was not enough, which is why this is a helper and not
/// a timeout written out at the call sites — the next test added here gets the bound by
/// default instead of by remembering.
async fn joined(
    serving: tokio::task::JoinHandle<std::io::Result<()>>,
    what: &str,
) {
    tokio::time::timeout(Duration::from_secs(10), serving)
        .await
        .unwrap_or_else(|_| panic!("{what}: the shutdown never returned"))
        .expect("the task joins")
        .expect("served");
}

/// A startup packet, then a query, without waiting for either reply.
async fn connect_and_query(address: std::net::SocketAddr) -> tokio::net::TcpStream {
    let mut stream = tokio::net::TcpStream::connect(address).await.expect("connect");
    stream.set_nodelay(true).ok();

    let mut body = 196_608i32.to_be_bytes().to_vec();
    body.extend_from_slice(b"user\0drain\0\0");
    let mut startup = Vec::new();
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    startup.extend_from_slice(&((body.len() + 4) as i32).to_be_bytes());
    startup.extend_from_slice(&body);
    stream.write_all(&startup).await.expect("startup");

    // Read until ReadyForQuery, rather than reading once. The startup reply is several
    // messages and they need not arrive in one segment; sending the query before the server
    // has finished the handshake leaves it sitting in a buffer the server is not yet reading
    // for, which is why the first version of this test saw the query never start.
    let mut seen = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let mut scratch = [0u8; 1024];
        let read = stream.read(&mut scratch).await.expect("a reply");
        assert!(read > 0, "the server closed during startup");
        seen.extend_from_slice(&scratch[..read]);
        if seen.contains(&b'Z') {
            break;
        }
        assert!(Instant::now() < deadline, "no ReadyForQuery arrived");
    }

    // Not `SELECT 1`: that is a driver's liveness ping and is answered from the catalogue
    // without ever reaching the handler, which is correct and made the first version of this
    // test wait forever for a query that was never going to arrive.
    let payload = "SELECT id FROM sales.orders\0";
    let mut message = vec![b'Q'];
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    message.extend_from_slice(&((payload.len() + 4) as i32).to_be_bytes());
    message.extend_from_slice(payload.as_bytes());
    stream.write_all(&message).await.expect("query");
    stream
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_shutdown_waits_for_a_query_already_running() {
    // The property the doc comment always claimed and the code did not have: the first
    // version returned the moment shutdown resolved, its spawned tasks were detached, and
    // dropping the runtime cancelled them. A client mid-result saw a reset on every deploy.
    let handler = Slow::taking(Duration::from_millis(600));
    let listener = PgListener::bind("127.0.0.1:0").await.expect("a port");
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

    let mut client = connect_and_query(address).await;
    // Wait until the query is genuinely in flight, rather than sleeping and hoping.
    await_started(&handler).await;

    stop.send(()).ok();
    joined(serving, "waiting for a query already running").await;

    assert_eq!(
        handler.finished.load(Ordering::SeqCst),
        1,
        "the shutdown waited for the query rather than abandoning it"
    );
    // And the client got its answer rather than a reset.
    let mut scratch = [0u8; 1024];
    let read = client.read(&mut scratch).await.expect("the reply arrives");
    assert!(read > 0, "the connection was cut off mid-result");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_drain_gives_up_rather_than_hanging_the_shutdown() {
    // An unbounded drain hangs on one stuck client until the orchestrator's patience runs
    // out and kills the process anyway — with the difference that nobody chose the moment.
    // Three seconds rather than thirty. `abort_all` cannot interrupt a blocking sleep, so
    // the runtime waits it out when it drops and the test costs that long on every build.
    // Three is fifteen times the drain deadline, which is all the assertion needs.
    let handler = Slow::taking(Duration::from_secs(3));
    let listener = PgListener::bind("127.0.0.1:0").await.expect("a port");
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
                    Duration::from_millis(200),
                )
                .await
        })
    };

    let _client = connect_and_query(address).await;
    await_started(&handler).await;

    let shutting_down = Instant::now();
    stop.send(()).ok();
    joined(serving, "giving up on a stuck query").await;
    let took = shutting_down.elapsed();

    assert!(
        took < Duration::from_secs(2),
        "the drain waited {took:?} on a three-second query; the deadline is 200ms"
    );
    assert_eq!(
        handler.finished.load(Ordering::SeqCst),
        0,
        "the query was still running when the deadline passed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_shutdown_with_nothing_in_flight_is_immediate() {
    // The common case, and it must not pay the drain deadline. A rolling deploy that waits
    // thirty seconds per idle instance is a rolling deploy nobody runs.
    let handler = Slow::taking(Duration::from_millis(1));
    let listener = PgListener::bind("127.0.0.1:0").await.expect("a port");

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
                    Duration::from_secs(30),
                )
                .await
        })
    };

    let shutting_down = Instant::now();
    stop.send(()).ok();
    joined(serving, "shutting down with nothing in flight").await;
    assert!(
        shutting_down.elapsed() < Duration::from_secs(2),
        "an idle server waited for a drain it did not need"
    );
}

#[test]
fn the_drain_default_is_a_named_constant_the_manifests_can_be_checked_against() {
    // Two numbers in two files maintained by two people, with nothing normally relating
    // them, is how a deploy comes to SIGKILL a server mid-drain.
    assert_eq!(PgListener::DRAIN, Duration::from_secs(30));
}

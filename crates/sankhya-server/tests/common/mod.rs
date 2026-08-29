//! The harness both server journey tests drive the real binary through.
//!
//! Shared rather than duplicated. Two copies of a wire-protocol client is two chances to be
//! subtly wrong about the protocol, and the second copy is the one nobody reviews.

#![allow(dead_code, clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use arrow_array::{Float64Array, Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use sankhya_publish::Publication;
use sankhya_types::Lsn;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

/// Write the sample warehouse a first-time user is told to generate.
pub(crate) fn write_warehouse(root: &std::path::Path) {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("region", DataType::Utf8, true),
        Field::new("period", DataType::Utf8, true),
        Field::new("amount", DataType::Float64, false),
        // A ratio, so the guide can show the refusal that matters: a measure with no way to
        // be derived from its parts must not be rolled up, and saying so is the whole reason
        // the additivity model exists.
        Field::new("margin_pct", DataType::Float64, false),
    ]));
    let table_root = root.join("sales").join("orders");
    // Through the product's own writer. This used to build the log by hand --- create the
    // metadata, write the parquet, assemble the add actions --- which meant the fixture
    // encoded the storage layout, and went on encoding the *old* layout after tables became
    // partitioned. A read test whose fixture cannot have the write path's bug is testing
    // less than it looks like it is.
    let publication = Publication::external(&table_root, "orders");
    publication.create(&schema).expect("creating");

    for file in 0..4u64 {
        let ids: Vec<i64> = (0..250)
            .map(|i| i64::try_from(file * 250 + i).unwrap_or(0))
            .collect();
        let regions: Vec<Option<&str>> = ids
            .iter()
            .map(|i| match i % 3 {
                0 => Some("north"),
                1 => Some("south"),
                _ => None,
            })
            .collect();
        let periods: Vec<Option<&str>> = ids
            .iter()
            .map(|i| if i % 2 == 0 { Some("q1") } else { Some("q2") })
            .collect();
        #[allow(clippy::cast_precision_loss)]
        let amounts: Vec<f64> = ids.iter().map(|i| *i as f64 * 1.5).collect();
        #[allow(clippy::cast_precision_loss)]
        let margins: Vec<f64> = ids.iter().map(|i| (*i % 40) as f64 / 100.0).collect();
        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![
                Arc::new(Int64Array::from(ids)),
                Arc::new(StringArray::from(regions)),
                Arc::new(StringArray::from(periods)),
                Arc::new(Float64Array::from(amounts)),
                Arc::new(Float64Array::from(margins)),
            ],
        )
        .expect("a valid batch");
        publication
            .append(
                file + 1,
                &format!("part-{file:04}.parquet"),
                &batch,
                Lsn::new(file + 1),
            )
            .expect("publishing");
    }

    declare_the_sample_cube(root);
}

/// The cube a first-time user is shown, declared into the warehouse's catalogue.
///
/// Two dimensions, because one cannot demonstrate a roll-up: rolling *up* means rolling a
/// dimension **away**, and with a single dimension every query is already the base. And two
/// measures of deliberately different kinds --- one that composes and one that cannot --- so
/// the guide can show both the answer and the refusal.
fn declare_the_sample_cube(root: &std::path::Path) {
    use sankhya_cube::algo::{Along, Measure, Rule};
    use sankhya_cube::model::{Definition, Dimension, Level};

    let definition = Definition::new(
        "sales",
        "orders",
        vec![
            Dimension {
                name: "region".to_string(),
                table: "orders".to_string(),
                joins_on: "region".to_string(),
                levels: vec![Level::new("area", "region")],
                rollups: None,
                parent_child: None,
            },
            Dimension {
                name: "period".to_string(),
                table: "orders".to_string(),
                joins_on: "period".to_string(),
                levels: vec![Level::new("quarter", "period")],
                rollups: None,
                parent_child: None,
            },
        ],
        vec![
            Measure::new(
                "amount",
                vec![
                    Along::new("region", Rule::Sum),
                    Along::new("period", Rule::Sum),
                ],
            ),
            // A ratio. There is no operation over the parts that yields the whole, so it is
            // declared as composing along nothing --- and a roll-up that would need it to is
            // refused while the query is planned rather than answered with a plausible number.
            Measure::new(
                "margin_pct",
                vec![
                    Along::new("region", Rule::None),
                    Along::new("period", Rule::None),
                ],
            ),
        ],
    );
    sankhya_cube::catalogue::save(root, &definition).expect("declaring the sample cube");
}

/// The server, and the port it actually bound.
pub(crate) struct Running {
    child: Child,
    pub(crate) port: u16,
}

impl Drop for Running {
    fn drop(&mut self) {
        // Killed rather than signalled: a test that leaves a server behind poisons every
        // later run on the same machine, and this one has already proven the shutdown path
        // elsewhere.
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

/// How long to wait for the server to announce its port before giving up.
///
/// Bounded, and the bound is not decoration. An earlier version read the banner on this
/// thread until the line appeared — so a server that started and printed *nothing* left the
/// test blocked forever, which took the whole build with it and produced no message. A test
/// that hangs is strictly worse than one that fails: a failure names what broke.
const BANNER_TIMEOUT: Duration = Duration::from_secs(30);

/// Start the binary and wait until it says which port it took.
///
/// Reading the banner rather than sleeping. A sleep long enough to be reliable dominates the
/// measurement; one short enough not to is a flaky test.
pub(crate) fn start(warehouse: &std::path::Path, data: &std::path::Path) -> Running {
    let mut child = Command::new(env!("CARGO_BIN_EXE_sankhya-server"))
        .env("SANKHYA_NO_PASSWORD", "1")
        .env("SANKHYA_LISTEN", "127.0.0.1:0")
        .env("SANKHYA_METRICS_LISTEN", "127.0.0.1:0")
        .env("SANKHYA_WAREHOUSE", warehouse)
        .env("SANKHYA_DATA_DIR", data)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the server binary starts");

    let stdout = child.stdout.take().expect("piped");
    // Read on another thread and wait with a deadline, because `read_line` has none. The
    // thread is left to finish on its own: it ends when the child does, and the child is
    // killed by `Running`'s `Drop` on every path out of this test.
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        let mut announced = false;
        // Keeps reading after the port is found, and that is not tidiness.
        //
        // An earlier version returned as soon as it had the port, which dropped the reader
        // and closed the pipe — so the server's *next* `println!` hit a broken pipe, the
        // process died, and the test's first read got `ConnectionReset`. It passed in
        // isolation because the whole banner usually landed in the pipe buffer before the
        // thread exited, and failed under a loaded `cargo test --workspace` because it
        // sometimes did not. A supervisor drains the pipe for the life of the child; so does
        // this.
        while reader.read_line(&mut line).unwrap_or(0) > 0 {
            if !announced {
                if let Some(address) = line.trim().strip_prefix("listening on ") {
                    let port = address
                        .rsplit_once(':')
                        .and_then(|(_, port)| port.parse::<u16>().ok());
                    sender.send(port).ok();
                    announced = true;
                }
            }
            line.clear();
        }
        if !announced {
            // Ended without announcing. Reported rather than left to time out, so the
            // failure says "it never said" instead of "something took too long".
            sender.send(None).ok();
        }
    });

    let announced = receiver.recv_timeout(BANNER_TIMEOUT);
    let port = match announced {
        Ok(Some(port)) => port,
        Ok(None) => {
            let mut running = Running { child, port: 0 };
            running.child.kill().ok();
            panic!("the server exited without announcing a port");
        }
        Err(_) => {
            let mut running = Running { child, port: 0 };
            running.child.kill().ok();
            panic!(
                "the server did not announce a port within {}s",
                BANNER_TIMEOUT.as_secs()
            );
        }
    };
    Running { child, port }
}

/// Run one simple query over the wire and return the rows it produced.
///
/// A hand-written client rather than `psql`, because `psql` needs the vendored PostgreSQL
/// build and this test must run on a machine that has not done it. It speaks the real
/// protocol, which is the half that can regress here.
pub(crate) fn query(port: u16, sql: &str) -> usize {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connecting");
    stream.set_nodelay(true).ok();

    let mut startup = Vec::new();
    let mut body = 196_608i32.to_be_bytes().to_vec();
    body.extend_from_slice(b"user\0quickstart\0\0");
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    startup.extend_from_slice(&((body.len() + 4) as i32).to_be_bytes());
    startup.extend_from_slice(&body);
    stream.write_all(&startup).expect("startup");

    let mut buffer = Vec::new();
    read_until_ready(&mut stream, &mut buffer);

    let mut message = vec![b'Q'];
    let payload = format!("{sql}\0");
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    message.extend_from_slice(&((payload.len() + 4) as i32).to_be_bytes());
    message.extend_from_slice(payload.as_bytes());
    stream.write_all(&message).expect("query");

    buffer.clear();
    read_until_ready(&mut stream, &mut buffer);
    // `D` is a DataRow. Counting tags rather than parsing the whole stream: the assertion is
    // that rows came back, and a parser here would be a second protocol implementation to
    // keep correct.
    count_tags(&buffer, b'D')
}

/// Run a statement and say whether the server refused it.
///
/// # Why counting rows was not enough
///
/// [`query`] returns a row count, and a **refused** statement returns no rows --- so a caller
/// checking only the count cannot tell a query that failed from one that legitimately matched
/// nothing. The guide test was doing exactly that: it asserted every example is *executed*,
/// which they were, and never that a non-error example *succeeded*. A broken example passed.
///
/// `E` is an ErrorResponse. Its presence is the difference, and it is one tag to look for.
pub(crate) fn query_outcome(port: u16, sql: &str) -> Result<usize, String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connecting");
    stream.set_nodelay(true).ok();

    let mut startup = Vec::new();
    let mut body = 196_608i32.to_be_bytes().to_vec();
    body.extend_from_slice(b"user\0quickstart\0\0");
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    startup.extend_from_slice(&((body.len() + 4) as i32).to_be_bytes());
    startup.extend_from_slice(&body);
    stream.write_all(&startup).expect("startup");

    let mut buffer = Vec::new();
    read_until_ready(&mut stream, &mut buffer);

    let mut message = vec![b'Q'];
    let payload = format!("{sql}\0");
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    message.extend_from_slice(&((payload.len() + 4) as i32).to_be_bytes());
    message.extend_from_slice(payload.as_bytes());
    stream.write_all(&message).expect("query");

    buffer.clear();
    read_until_ready(&mut stream, &mut buffer);

    if count_tags(&buffer, b'E') > 0 {
        // The message text, so a failing guide example says what was wrong rather than only
        // that something was.
        let text: String = buffer
            .iter()
            .map(|byte| if byte.is_ascii_graphic() || *byte == b' ' { *byte as char } else { ' ' })
            .collect();
        return Err(text.split_whitespace().collect::<Vec<&str>>().join(" "));
    }
    Ok(count_tags(&buffer, b'D'))
}

/// Read until the server says it is ready for the next statement.
pub(crate) fn read_until_ready(stream: &mut TcpStream, buffer: &mut Vec<u8>) {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("a read timeout");
    let mut chunk = [0u8; 4096];
    loop {
        let read = stream.read(&mut chunk).expect("the server answers");
        assert!(read > 0, "the server closed the connection");
        buffer.extend_from_slice(&chunk[..read]);
        if count_tags(buffer, b'Z') > 0 {
            return;
        }
    }
}

/// How many messages of this tag the buffer holds.
///
/// Walks the framing rather than searching for the byte, because a row's *data* can contain
/// any byte and a search would count values as messages.
pub(crate) fn count_tags(buffer: &[u8], tag: u8) -> usize {
    let mut at = 0usize;
    let mut found = 0usize;
    while at + 5 <= buffer.len() {
        let length = i32::from_be_bytes([
            buffer[at + 1],
            buffer[at + 2],
            buffer[at + 3],
            buffer[at + 4],
        ]);
        let Ok(length) = usize::try_from(length) else {
            return found;
        };
        if length < 4 || at + 1 + length > buffer.len() {
            return found;
        }
        if buffer[at] == tag {
            found += 1;
        }
        at += 1 + length;
    }
    found
}

/// Run a subcommand of the binary and return its exit status.
pub(crate) fn subcommand(what: &str, warehouse: &std::path::Path, data: &std::path::Path) -> i32 {
    Command::new(env!("CARGO_BIN_EXE_sankhya-server"))
        .arg(what)
        .env("SANKHYA_WAREHOUSE", warehouse)
        .env("SANKHYA_DATA_DIR", data)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("the subcommand runs")
        .code()
        .unwrap_or(-1)
}

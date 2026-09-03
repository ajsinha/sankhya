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
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Write the sample warehouse a first-time user is told to generate.
pub(crate) fn write_warehouse(root: &std::path::Path) {
    // The quarantine, empty. Created so the guide's example of reading it *runs* rather than
    // being excused: an example that does not run is documentation that lies, and this one
    // is read by somebody whose feed has just refused a record and who is not in a position
    // to tell a wrong query from a wrong system.
    let quarantine_root = root.join("sank").join(sankhya_feed::quarantine::TABLE);
    Publication::external(&quarantine_root, sankhya_feed::quarantine::TABLE)
        .create(&sankhya_feed::quarantine::schema())
        .expect("creating the quarantine");

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

    write_the_dimension_table(root);
    write_the_risk_table(root);
    declare_the_sample_cube(root);
}

/// A table of profit-and-loss vectors, one per position.
///
/// # Why the fixture has one at all
///
/// Because the function catalogue exists so that arithmetic happens **where the data is**, and
/// nothing demonstrated that. Every example called a function on a literal carried from the
/// client --- which is precisely the thing the catalogue is meant to make unnecessary, and a
/// fixture with no vector column is why nobody noticed.
///
/// A P&L vector per position is the shape a risk calculation actually has: five hundred
/// simulated outcomes for one instrument, and a value-at-risk is a quantile of them. Small
/// enough to check by hand, real enough to be the example.
///
/// # Why it also carries two plain numbers
///
/// A distribution reached with a literal is one value broadcast; the same distribution over a
/// column is a `Float64Array` read once per row. Different marshalling, and the second is what
/// a query does --- so a soak that called every scalar function on literals had never run the
/// path that matters for two thirds of the catalogue.
///
/// The two columns are named for their **domains**, not for a use: `confidence` lies strictly
/// inside `(0, 1)` and `exposure` is a positive real. A probability column and a scale column
/// are not interchangeable --- `norm_inv` of an exposure is refused, and rightly --- so a
/// fixture with one general-purpose number column would have produced a soak that compared
/// refusals and reported agreement.
///
/// # And why one of its columns is a matrix
///
/// `ADR-0021` Decision 2 says a matrix column is a `FixedSizeList` whose **field metadata**
/// carries the shape, because a run of sixteen values is a 4x4 or a 2x8 and nothing in the
/// values says which. Nothing stored one. So the functions that refuse an undeclared shape ---
/// transpose, multiply, applying a matrix to a vector --- were reachable only from a literal,
/// and the one thing they exist for, a matrix that is *stored*, had never been tried.
fn write_the_risk_table(root: &std::path::Path) {
    use arrow_array::builder::{FixedSizeListBuilder, Float64Builder};

    const OUTCOMES: i32 = 64;
    /// The order of the stored covariance matrix, whose column declares its shape.
    const ORDER: i32 = 4;

    let schema = Arc::new(Schema::new(vec![
        Field::new("position_id", DataType::Int64, false),
        Field::new("book", DataType::Utf8, false),
        Field::new("exposure", DataType::Float64, false),
        Field::new("confidence", DataType::Float64, false),
        Field::new(
            "covariance",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float64, true)),
                ORDER * ORDER,
            ),
            false,
        )
        .with_metadata(sankhya_olap::matrices::tensor_metadata(
            ORDER as usize,
            ORDER as usize,
        )),
        Field::new(
            "pnl",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float64, true)),
                OUTCOMES,
            ),
            false,
        ),
    ]));
    let table_root = root.join("risk").join("positions");
    let publication = Publication::external(&table_root, "positions");
    publication.create(&schema).expect("creating the risk table");

    let mut vectors = FixedSizeListBuilder::new(Float64Builder::new(), OUTCOMES);
    let mut ids: Vec<i64> = Vec::new();
    let mut books: Vec<&str> = Vec::new();
    let mut exposures: Vec<f64> = Vec::new();
    let mut confidences: Vec<f64> = Vec::new();
    let mut covariances = FixedSizeListBuilder::new(Float64Builder::new(), ORDER * ORDER);
    for position in 0..12i64 {
        ids.push(position + 1);
        #[allow(clippy::cast_precision_loss)]
        let step = position as f64;
        exposures.push(1.5 + step * 0.75);
        // Strictly inside the open interval at both ends: a confidence of exactly one is
        // refused by every inverse in the catalogue, and a fixture whose last row refused
        // would look like a defect in the function.
        confidences.push(0.90 + step * 0.004);

        // Symmetric and positive definite by construction, so a Cholesky over the column is a
        // real answer rather than a refusal the soak would count as agreement.
        for row in 0..ORDER {
            for column in 0..ORDER {
                let (row, column) = (f64::from(row), f64::from(column));
                let shared = (row + 1.0).min(column + 1.0);
                let diagonal = if (row - column).abs() < 0.5 { 1.0 + step * 0.1 } else { 0.0 };
                covariances.values().append_value(shared * 0.25 + diagonal);
            }
        }
        covariances.append(true);
        books.push(if position % 3 == 0 { "rates" } else if position % 3 == 1 { "credit" } else { "equity" });
        // A spread that differs per position, so a quantile across positions is not the same
        // number twelve times --- which is what a fixture of identical rows would produce, and
        // an example that showed one would demonstrate nothing.
        #[allow(clippy::cast_precision_loss)]
        let scale = 1.0 + position as f64 * 0.4;
        let outcomes: Vec<f64> = (0..OUTCOMES)
            .map(|outcome| {
                let t = f64::from(outcome) / f64::from(OUTCOMES) * std::f64::consts::TAU;
                // Deterministic, so the expected numbers in a test are the same every run.
                (t.sin() * 3.0 + (t * 2.7).cos() * 1.5) * scale
            })
            .collect();
        vectors.values().append_slice(&outcomes);
        vectors.append(true);
    }

    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(Int64Array::from(ids)),
            Arc::new(StringArray::from(books)),
            Arc::new(Float64Array::from(exposures)),
            Arc::new(Float64Array::from(confidences)),
            Arc::new(covariances.finish()),
            Arc::new(vectors.finish()),
        ],
    )
    .expect("a valid batch");
    publication
        .append(1, "part-0000.parquet", &batch, Lsn::new(12))
        .expect("publishing the risk table");
}

/// The member table a dimension hangs on.
///
/// # Why the fixture grew one
///
/// The shipped `CREATE CUBE` example joins a dimension to a `regions` table, because that is
/// what a cube over a real warehouse does --- the fact table holds a key and the member names,
/// levels and hierarchy live beside it. The fixture had only the fact table, so the example
/// could not run here, and *"the example needs a table this fixture does not have"* is the
/// same defect as a fixture whose shape is not the product's shape: it tests the fixture.
fn write_the_dimension_table(root: &std::path::Path) {
    let schema = Arc::new(Schema::new(vec![
        Field::new("region", DataType::Utf8, false),
        Field::new("area", DataType::Utf8, false),
    ]));
    let table_root = root.join("sales").join("regions");
    let publication = Publication::external(&table_root, "regions");
    publication.create(&schema).expect("creating the dimension table");

    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(StringArray::from(vec!["north", "south"])),
            Arc::new(StringArray::from(vec!["north", "south"])),
        ],
    )
    .expect("a valid batch");
    publication
        .append(1, "part-0000.parquet", &batch, Lsn::new(1))
        .expect("publishing the members");
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
    /// Every line the server printed, on either stream.
    ///
    /// Kept rather than discarded because some of what this system does is **say something**.
    /// A feed that stops is required by `ADR-0018` to stop loudly, and a test that only
    /// checks the feed stopped would pass against a server that stopped it in silence ---
    /// which is the failure an operator actually meets.
    said: Arc<Mutex<Vec<String>>>,
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
    start_with(warehouse, data, &[])
}

/// Start the binary with extra environment, and wait until it says which port it took.
///
/// The environment is a slice rather than a struct of known keys: what a test needs to
/// configure is a property of the test, and a struct here would grow a field per caller and
/// still be wrong for the next one.
///
/// **Both streams are captured.** `start` used to discard stderr, which is where every
/// refusal and every stopped feed is reported --- so the one channel a test would want to
/// assert on was the one thrown away.
pub(crate) fn start_with(
    warehouse: &std::path::Path,
    data: &std::path::Path,
    extra: &[(&str, &str)],
) -> Running {
    let mut command = Command::new(env!("CARGO_BIN_EXE_sankhya-server"));
    command
        .env("SANKHYA_NO_PASSWORD", "1")
        .env("SANKHYA_LISTEN", "127.0.0.1:0")
        .env("SANKHYA_METRICS_LISTEN", "127.0.0.1:0")
        .env("SANKHYA_WAREHOUSE", warehouse)
        .env("SANKHYA_DATA_DIR", data);
    for (name, value) in extra {
        command.env(name, value);
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the server binary starts");

    let said: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    // Stderr is drained on its own thread for the reason stdout is: a full pipe blocks the
    // writer, and the writer here is the process under test.
    if let Some(stderr) = child.stderr.take() {
        let said = Arc::clone(&said);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                if let Ok(mut lines) = said.lock() {
                    lines.push(line.trim_end().to_owned());
                }
                line.clear();
            }
        });
    }

    let stdout = child.stdout.take().expect("piped");
    // Read on another thread and wait with a deadline, because `read_line` has none. The
    // thread is left to finish on its own: it ends when the child does, and the child is
    // killed by `Running`'s `Drop` on every path out of this test.
    let (sender, receiver) = std::sync::mpsc::channel();
    let recording = Arc::clone(&said);
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
            if let Ok(mut lines) = recording.lock() {
                lines.push(line.trim_end().to_owned());
            }
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
            let mut running = Running { child, port: 0, said };
            running.child.kill().ok();
            panic!("the server exited without announcing a port");
        }
        Err(_) => {
            let mut running = Running { child, port: 0, said: Arc::clone(&said) };
            running.child.kill().ok();
            panic!(
                "the server did not announce a port within {}s",
                BANNER_TIMEOUT.as_secs()
            );
        }
    };
    Running { child, port, said }
}

impl Running {
    /// Wait until the server has printed a line containing `needle`, or give up.
    ///
    /// Bounded, and the bound is the assertion's other half: "it eventually says so" is not a
    /// property anybody can rely on, and a test that waits forever for a line that never
    /// comes takes the build with it rather than reporting anything.
    pub(crate) fn wait_until_said(&self, needle: &str, within: Duration) -> bool {
        let deadline = std::time::Instant::now() + within;
        loop {
            if self.said_matching(needle).next().is_some() {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Every line the server printed that contains `needle`.
    pub(crate) fn said_matching(&self, needle: &str) -> std::vec::IntoIter<String> {
        let needle = needle.to_owned();
        let lines = match self.said.lock() {
            Ok(lines) => lines.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        lines
            .into_iter()
            .filter(|line| line.contains(&needle))
            .collect::<Vec<_>>()
            .into_iter()
    }
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

/// Run a statement and return its rows as text.
///
/// # Why this exists beside [`query`] and [`query_outcome`]
///
/// Both of those answer *how many* rows came back, which is enough for a query whose subject
/// is the rows. It is not enough for a statement whose subject is a **status**: `SHOW FEEDS`
/// returns one row per feed whether the feed is running happily or stopped an hour ago, so a
/// count cannot tell those apart, and the whole point of the statement is that it can.
///
/// A `NULL` value is `None`; anything else is its text as the server sent it.
pub(crate) fn text_rows(port: u16, sql: &str) -> Vec<Vec<Option<String>>> {
    let buffer = exchange(port, sql);
    // A refused statement returns no rows, so a caller reading the rows alone cannot tell a
    // typo in a column name from a table that is legitimately empty. This panics with what
    // the server said instead, because every caller here wants the statement to succeed.
    assert!(
        count_tags(&buffer, b'E') == 0,
        "`{sql}` was refused: {}",
        buffer
            .iter()
            .map(|byte| if byte.is_ascii_graphic() || *byte == b' ' { *byte as char } else { ' ' })
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<&str>>()
            .join(" ")
    );
    data_rows(&buffer)
}

/// Connect, send one simple query, and return everything the server said.
fn exchange(port: u16, sql: &str) -> Vec<u8> {
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
    buffer
}

/// The type OID a statement's first column is described with.
///
/// # Why a test reads this at all
///
/// The OID is the contract a *driver* dispatches on, and it is invisible in the rendered
/// value: a column sent as `text` and a column sent as `float8[]` can carry identical bytes
/// and mean different things to the client. A test asserting only the rendering passes while
/// the type is wrong --- which is exactly what happened, and why this exists.
pub(crate) fn first_column_oid(port: u16, sql: &str) -> Option<i32> {
    let buffer = exchange(port, sql);
    let mut at = 0usize;
    while at + 5 <= buffer.len() {
        // Bounds-checked accessors rather than indexing: this walks a wire buffer, and a
        // malformed frame is a server defect worth seeing as a returned `None` rather than as
        // a panic inside a helper.
        let length = i32::from_be_bytes([
            *buffer.get(at + 1)?,
            *buffer.get(at + 2)?,
            *buffer.get(at + 3)?,
            *buffer.get(at + 4)?,
        ]);
        let length = usize::try_from(length).ok()?;
        if length < 4 || at + 1 + length > buffer.len() {
            return None;
        }
        // `T`, the row description: a field count, then per field a name, a table OID, a
        // column number, and the type OID.
        if buffer.get(at) == Some(&b'T') {
            let body = buffer.get(at + 5..at + 1 + length)?;
            let end = body.iter().skip(2).position(|byte| *byte == 0)? + 2;
            let type_at = end + 1 + 4 + 2;
            return Some(i32::from_be_bytes([
                *body.get(type_at)?,
                *body.get(type_at + 1)?,
                *body.get(type_at + 2)?,
                *body.get(type_at + 3)?,
            ]));
        }
        at += 1 + length;
    }
    None
}

/// The `DataRow` messages in a buffer, decoded to text.
///
/// Walks the framing exactly as [`count_tags`] does rather than searching for the tag byte,
/// because a value can contain any byte and a search would decode data as a message header.
fn data_rows(buffer: &[u8]) -> Vec<Vec<Option<String>>> {
    let mut rows = Vec::new();
    let mut at = 0usize;
    while at + 5 <= buffer.len() {
        let length = i32::from_be_bytes([
            buffer[at + 1],
            buffer[at + 2],
            buffer[at + 3],
            buffer[at + 4],
        ]);
        let Ok(length) = usize::try_from(length) else {
            return rows;
        };
        if length < 4 || at + 1 + length > buffer.len() {
            return rows;
        }
        if buffer[at] == b'D' {
            let body = &buffer[at + 5..at + 1 + length];
            if let Some(row) = decode_row(body) {
                rows.push(row);
            }
        }
        at += 1 + length;
    }
    rows
}

/// One `DataRow` body: a column count, then a length and that many bytes per column.
///
/// `None` for a body that does not decode, which the caller reports as a missing row rather
/// than as a panic --- a malformed row is a server defect worth seeing as a failed assertion
/// about the rows, not as a decoding stack trace.
fn decode_row(body: &[u8]) -> Option<Vec<Option<String>>> {
    let columns = u16::from_be_bytes([*body.first()?, *body.get(1)?]);
    let mut values = Vec::with_capacity(usize::from(columns));
    let mut at = 2usize;
    for _ in 0..columns {
        let length = i32::from_be_bytes([
            *body.get(at)?,
            *body.get(at + 1)?,
            *body.get(at + 2)?,
            *body.get(at + 3)?,
        ]);
        at += 4;
        if length < 0 {
            // The protocol's `NULL`, which is a length of -1 and no bytes.
            values.push(None);
            continue;
        }
        let length = usize::try_from(length).ok()?;
        let text = body.get(at..at + length)?;
        values.push(Some(String::from_utf8_lossy(text).into_owned()));
        at += length;
    }
    Some(values)
}

/// Run a statement expected to fail, and return the refusal's fields by their protocol tag.
///
/// `C` is the SQLSTATE, `M` the message, `D` the detail, `H` the hint --- which is where the
/// **names a refusal cites** travel, per `ADR-0017` Decision 2.
///
/// Its own helper because reading them out of the rendered buffer is exactly the mistake the
/// decision exists to prevent: a test that finds a name anywhere in the bytes passes whether
/// the name arrived as data or only as prose, which is the difference being guarded.
pub(crate) fn refusal_fields(port: u16, sql: &str) -> std::collections::BTreeMap<char, String> {
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

    let mut fields = std::collections::BTreeMap::new();
    let mut at = 0usize;
    while at + 5 <= buffer.len() {
        let length = i32::from_be_bytes([
            buffer[at + 1],
            buffer[at + 2],
            buffer[at + 3],
            buffer[at + 4],
        ]);
        let Ok(length) = usize::try_from(length) else {
            break;
        };
        if length < 4 || at + 1 + length > buffer.len() {
            break;
        }
        if buffer[at] == b'E' {
            // The body is a sequence of `tag, cstring`, ended by a zero tag.
            let mut inner = at + 5;
            while inner < at + 1 + length {
                let tag = buffer[inner];
                if tag == 0 {
                    break;
                }
                inner += 1;
                let start = inner;
                while inner < at + 1 + length && buffer[inner] != 0 {
                    inner += 1;
                }
                fields.insert(
                    tag as char,
                    String::from_utf8_lossy(&buffer[start..inner]).into_owned(),
                );
                inner += 1;
            }
        }
        at += 1 + length;
    }
    fields
}

/// One connection, held open across several statements.
///
/// # Why this exists
///
/// [`query`] and [`query_outcome`] open a connection, run one statement and hang up, which is
/// right for almost everything here and **wrong for anything about session state**. A `SET` on
/// a connection that is then closed has no observable effect, so a test written on those
/// helpers passes whether the setting is honoured or ignored --- which is the failure it was
/// meant to catch.
pub(crate) struct Session {
    stream: TcpStream,
}

impl Session {
    /// Open a connection and complete the handshake.
    pub(crate) fn open(port: u16) -> Self {
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
        Self { stream }
    }

    /// Run a statement on this connection, returning its rows or the refusal's text.
    pub(crate) fn run(&mut self, sql: &str) -> Result<usize, String> {
        let mut message = vec![b'Q'];
        let payload = format!("{sql}\0");
        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        message.extend_from_slice(&((payload.len() + 4) as i32).to_be_bytes());
        message.extend_from_slice(payload.as_bytes());
        self.stream.write_all(&message).expect("query");

        let mut buffer = Vec::new();
        read_until_ready(&mut self.stream, &mut buffer);
        if count_tags(&buffer, b'E') > 0 {
            let text: String = buffer
                .iter()
                .map(|byte| if byte.is_ascii_graphic() || *byte == b' ' { *byte as char } else { ' ' })
                .collect();
            return Err(text.split_whitespace().collect::<Vec<&str>>().join(" "));
        }
        Ok(count_tags(&buffer, b'D'))
    }
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

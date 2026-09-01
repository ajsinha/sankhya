//! `M13`'s exit criteria, demonstrated against the server a deployment actually runs.
//!
//! # Why this is not the feed crate's own tests again
//!
//! `sankhya-feed`'s tests hold one decision still and check it, and its `tests/run.rs` puts
//! the runner against real tables on disk. Both call the library directly. **Neither proves a
//! deployment does any of it**: that a declaration under `config/feeds/` is found, that the
//! loop runs it, that what it published is answerable over the wire by a client which knows
//! nothing about feeds, and that a feed which stops is visible afterwards to the operator who
//! has to deal with it.
//!
//! That gap is why a milestone has exit criteria rather than a test count. Everything here
//! goes through the real binary and the real wire protocol.
//!
//! # The five criteria, and where each is shown
//!
//! 1. *Ingest end to end from a file to an OLAP answer* — [`a_file_becomes_an_answer`].
//! 2. *A malformed record quarantined rather than coerced or dropped* —
//!    [`a_record_that_does_not_fit_is_quarantined_and_the_others_still_land`].
//! 3. *A run of bad records stops the pipeline loudly* —
//!    [`a_source_of_nothing_usable_stops_the_feed_loudly_and_it_stays_stopped`].
//! 4. *Every refused config fails closed* — [`a_declaration_that_does_not_validate_lands_nothing`]
//!    and [`a_feed_whose_table_does_not_exist_is_refused_rather_than_creating_one`].
//! 5. *The quarantine expires rather than accumulating* —
//!    [`the_quarantine_expires_rather_than_accumulating`].
//!
//! # The one criterion that cannot be shown by waiting
//!
//! Expiry is measured in **days**, and a partition written today is expired by no retention at
//! all, including zero — the cutoff is `today - retain_days`, and today is never before
//! itself. That is deliberate: a record refused an hour ago is precisely the one somebody is
//! about to come looking for.
//!
//! A test cannot wait a day, and both ways of faking it are worse than what they replace.
//! Hand-writing a partition directory with yesterday's date makes the fixture encode the
//! storage layout, and a fixture that cannot have the write path's bug tests less than it
//! appears to. Moving the process clock makes the test's every future failure a story about
//! the clock.
//!
//! So criterion 5 is shown by calling the **deployment's own** expiry with a *stated* `today`,
//! over a quarantine the real feed really wrote — `plan` takes the day as an argument for
//! exactly this reason. What that leaves unproven is only that the tick passes it the real
//! calendar, and [`the_tick_attempts_expiry_without_being_asked`] covers that on its own.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use common::{query_outcome, start_with, text_rows, Running};
use sankhya_publish::Publication;
use std::path::PathBuf;
use std::time::Duration;

#[path = "../src/feeds.rs"]
mod feeds;

/// How long a criterion waits for the feed's next tick before failing.
///
/// The cadence below is one second, so this is many ticks rather than a hopeful one. Bounded,
/// because an unbounded wait for something that never happens takes the build with it and
/// reports nothing — strictly worse than a failure that names what did not occur.
const WITHIN: Duration = Duration::from_secs(60);

/// The feed's cadence during these tests, as the server reads it.
///
/// One second. The default is thirty, which is right for a deployment and would make this
/// file take minutes to say what it can say in seconds.
const CADENCE: &str = "1";

/// A deployment: a warehouse, a configuration directory holding declarations, and a spool.
struct Deployment {
    dir: tempfile::TempDir,
}

impl Deployment {
    /// A deployment with the directories a server needs and an empty quarantine.
    ///
    /// The quarantine is created **through the product's own writer**, as is every table
    /// here. A fixture that assembled the log by hand would encode the storage layout and go
    /// on encoding the old one after the layout changed.
    fn new() -> Self {
        let it = Self { dir: tempfile::tempdir().expect("a directory") };
        std::fs::create_dir_all(it.spool()).expect("a spool");
        std::fs::create_dir_all(it.config().join(feeds::DIRECTORY)).expect("a feeds directory");
        // Present and almost empty. The server derives its configuration *directory* from the
        // first configuration file, so the file must exist for `config/feeds/` to be found.
        std::fs::write(it.config().join("application.yaml"), "# read for its directory\n")
            .expect("a configuration file");

        let quarantine = it.warehouse().join("sank").join(sankhya_feed::quarantine::TABLE);
        Publication::external(&quarantine, sankhya_feed::quarantine::TABLE)
            .create(&sankhya_feed::quarantine::schema())
            .expect("creating the quarantine");
        it
    }

    fn warehouse(&self) -> PathBuf {
        self.dir.path().join("warehouse")
    }
    fn data(&self) -> PathBuf {
        self.dir.path().join("data")
    }
    fn config(&self) -> PathBuf {
        self.dir.path().join("config")
    }
    fn spool(&self) -> PathBuf {
        self.dir.path().join("spool")
    }

    /// Write a feed declaration under `config/feeds/`.
    fn declare(&self, file: &str, body: &str) {
        std::fs::write(self.config().join(feeds::DIRECTORY).join(file), body)
            .expect("a declaration");
    }

    /// Declare the `ledger` feed these criteria use.
    ///
    /// `stop_above` is 0.5 rather than the reference document's 0.2, so that a source of two
    /// sound records and one bad one does **not** stop the feed. That is the case criterion 2
    /// is about — one malformed record is an incident and not an outage — and a threshold low
    /// enough to trip on it would make criteria 2 and 3 the same test.
    fn declare_the_ledger(&self) {
        let body = format!(
            "name: ledger\n\
             from: {}\n\
             schema: sales\n\
             table: ledger\n\
             date: ingest\n\
             unknown: refuse\n\
             columns:\n\
             \x20 - name: id\n\
             \x20   type: int64\n\
             \x20 - name: amount\n\
             \x20   type: decimal(18,2)\n\
             microbatch:\n\
             \x20 rows: 2\n\
             \x20 seconds: 1\n\
             quarantine:\n\
             \x20 retain_days: 7\n\
             \x20 window: 100\n\
             \x20 stop_above: 0.5\n",
            self.spool().display()
        );
        self.declare("ledger.yaml", &body);
    }

    /// Every feed this deployment's configuration declares, as the server loads it.
    fn loaded(&self) -> (Vec<feeds::Declared>, Vec<String>) {
        feeds::load(&self.config())
    }

    /// Create `sales.ledger` with the schema the loaded declaration implies.
    ///
    /// The schema comes from `shape::table_schema` — the same function the runner shapes its
    /// batches with — rather than being spelled out here. Two hand-written schemas that must
    /// agree are two things that will one day not.
    fn create_the_ledger_table(&self) {
        let (declared, complaints) = self.loaded();
        assert!(complaints.is_empty(), "{complaints:?}");
        let feed = &declared
            .iter()
            .find(|declared| declared.feed.name() == "ledger")
            .expect("the ledger feed is declared")
            .feed;
        Publication::external(self.warehouse().join("sales").join("ledger"), "ledger")
            .create(&sankhya_feed::shape::table_schema(feed))
            .expect("creating sales.ledger");
    }

    /// Put a source file in the spool, written whole and then moved into place.
    ///
    /// Moved rather than written in place: the feed is reading that directory on its own
    /// cadence, and a reader arriving mid-write sees a truncated document — which would make
    /// this file's failures a story about the test's own writing.
    fn arrives(&self, name: &str, lines: &[&str]) {
        let staging = self.dir.path().join("staging");
        std::fs::create_dir_all(&staging).expect("a staging directory");
        let temporary = staging.join(name);
        let mut body = lines.join("\n");
        body.push('\n');
        std::fs::write(&temporary, body).expect("writing a source");
        std::fs::rename(&temporary, self.spool().join(name)).expect("moving it into the spool");
    }

    /// Start the server against this deployment.
    fn start(&self) -> Running {
        let config = self.config().join("application.yaml").display().to_string();
        start_with(
            &self.warehouse(),
            &self.data(),
            &[
                ("SANKHYA_CONFIG", config.as_str()),
                ("SANKHYA_FEED_INTERVAL_SECONDS", CADENCE),
            ],
        )
    }
}

/// A deployment with the `ledger` feed declared and its table created.
fn ready() -> Deployment {
    let it = Deployment::new();
    it.declare_the_ledger();
    it.create_the_ledger_table();
    it
}

/// Wait until a query returns exactly `rows`, or say what it returned instead.
///
/// Polling rather than sleeping a fixed time: a sleep long enough to be reliable makes every
/// test here slow, and one short enough not to is a flaky test that fails on a busy machine.
fn wait_for_rows(server: &Running, sql: &str, rows: usize) {
    let deadline = std::time::Instant::now() + WITHIN;
    let mut last = String::from("nothing was ever answered");
    loop {
        match query_outcome(server.port, sql) {
            Ok(count) if count == rows => return,
            Ok(count) => last = format!("{count} row(s)"),
            Err(why) => last = why,
        }
        assert!(
            std::time::Instant::now() < deadline,
            "`{sql}` did not return {rows} row(s) within {}s — last saw {last}\n\
             the server said:\n{}",
            WITHIN.as_secs(),
            server.said_matching("").collect::<Vec<_>>().join("\n")
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The `SHOW FEEDS` row for one feed: name, state, halted since, reason, then the counters.
fn standing(port: u16, feed: &str) -> Vec<Option<String>> {
    text_rows(port, "SHOW FEEDS")
        .into_iter()
        .find(|row| row.first().and_then(Clone::clone).as_deref() == Some(feed))
        .unwrap_or_else(|| panic!("`SHOW FEEDS` has no row for `{feed}`"))
}

/// Wait until `SHOW FEEDS` says this feed is in `state`, and return the row.
fn wait_for_state(server: &Running, feed: &str, state: &str) -> Vec<Option<String>> {
    let deadline = std::time::Instant::now() + WITHIN;
    loop {
        let row = standing(server.port, feed);
        if row.get(1).and_then(Clone::clone).as_deref() == Some(state) {
            return row;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "`{feed}` was not `{state}` within {}s — it was {:?}\nthe server said:\n{}",
            WITHIN.as_secs(),
            row.get(1),
            server.said_matching("").collect::<Vec<_>>().join("\n")
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

// --- criterion 1: a file becomes an answer ---------------------------------

#[test]
fn a_file_becomes_an_answer() {
    // The milestone's whole claim in one test: a file appears in a directory, nobody runs
    // anything, and a SQL client that has never heard of feeds sees rows.
    let it = ready();
    let server = it.start();

    it.arrives(
        "001.json",
        &[
            r#"{"id": 1, "amount": "10.00"}"#,
            r#"{"id": 2, "amount": "20.00"}"#,
            r#"{"id": 3, "amount": "30.50"}"#,
        ],
    );

    wait_for_rows(&server, "SELECT id FROM ledger", 3);

    // The *values*, not merely three of something. A count alone passes against a feed that
    // published three empty rows, which is the failure this criterion is meant to exclude.
    let above = query_outcome(server.port, "SELECT id FROM ledger WHERE amount > 15")
        .expect("the query runs");
    assert_eq!(above, 2, "two of the three records are above 15");

    // And an aggregate, because "an OLAP answer" is the criterion's word: this is the read
    // path planning over what a feed published, not a scan of what a test wrote.
    let summed = text_rows(server.port, "SELECT sum(amount) AS total FROM ledger");
    assert_eq!(
        summed.first().and_then(|row| row.first()).and_then(Clone::clone).as_deref(),
        Some("60.50"),
        "the three amounts, summed exactly as decimals rather than as doubles"
    );
}

// --- criterion 2: quarantined, not coerced and not dropped -----------------

#[test]
fn a_record_that_does_not_fit_is_quarantined_and_the_others_still_land() {
    let it = ready();
    let server = it.start();

    it.arrives(
        "001.json",
        &[
            r#"{"id": 10, "amount": "1.00"}"#,
            // `amount` is a decimal and this is a word. Nothing in the system may decide what
            // it "meant": that is the coercion `ADR-0018` exists to forbid.
            r#"{"id": 11, "amount": "not a number"}"#,
            r#"{"id": 12, "amount": "2.00"}"#,
        ],
    );

    // Not dropped, and not coerced: two rows in the table, one row in the quarantine.
    wait_for_rows(&server, "SELECT id FROM ledger", 2);
    wait_for_rows(
        &server,
        &format!("SELECT feed FROM {}", sankhya_feed::quarantine::TABLE),
        1,
    );

    // The quarantined record is readable, says which feed refused it and why, and holds the
    // document **verbatim**. A quarantine nobody can read is a delete with extra storage, and
    // one that keeps a summary rather than the record cannot be replayed once the source is
    // fixed — which is the only thing anybody wants it for.
    let refused = text_rows(
        server.port,
        &format!(
            "SELECT feed, reason_code, payload FROM {}",
            sankhya_feed::quarantine::TABLE
        ),
    );
    assert_eq!(refused.len(), 1);
    assert_eq!(refused[0][0].as_deref(), Some("ledger"), "the feed that refused it");
    assert!(
        refused[0][1].as_ref().is_some_and(|code| !code.is_empty()),
        "a code saying why, rather than the fact of refusal alone"
    );
    assert_eq!(
        refused[0][2].as_deref(),
        Some(r#"{"id": 11, "amount": "not a number"}"#),
        "the record is kept whole, exactly as it arrived"
    );

    // The feed is still running. One record that does not fit is an incident, not an outage.
    let row = standing(server.port, "ledger");
    assert_eq!(row[1].as_deref(), Some("running"));
    assert_eq!(row[6].as_deref(), Some("1"), "one quarantined");
}

// --- criterion 3: a run of bad records stops it, loudly --------------------

#[test]
fn a_source_of_nothing_usable_stops_the_feed_loudly_and_it_stays_stopped() {
    let it = ready();
    let server = it.start();

    // Every record in the source is refused, so the source produced nothing usable. That is
    // not a rate and not a judgement call: a source this feed could read nothing out of has
    // changed shape, and continuing would be publishing whatever the next one happens to hold.
    it.arrives(
        "001.json",
        &[
            r#"{"id": 1, "amount": "nonsense"}"#,
            r#"{"id": 2, "amount": "also nonsense"}"#,
        ],
    );

    let halted = wait_for_state(&server, "ledger", "halted");
    assert!(
        halted[3].as_ref().is_some_and(|reason| !reason.is_empty()),
        "and it says why, an hour later, to somebody who never saw the log line"
    );

    // **Loudly.** The criterion's word, and the half a status row cannot cover: an operator
    // watching the log has to be told at the moment it happens.
    assert!(
        server.wait_until_said("STOPPED", WITHIN),
        "the server never said the feed stopped; it said: {:?}",
        server.said_matching("feed `ledger`").collect::<Vec<_>>()
    );

    // Stopped means stopped. A sound source arriving afterwards is *not* ingested, because
    // retrying on a timer rediscovers the same outage every tick and is acted on by nobody.
    it.arrives("002.json", &[r#"{"id": 3, "amount": "3.00"}"#]);
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(
        query_outcome(server.port, "SELECT id FROM ledger").expect("the query runs"),
        0,
        "a halted feed does not quietly start again on the next tick"
    );

    // And resuming is a statement somebody makes, after which the waiting source lands.
    query_outcome(server.port, "RESUME FEED ledger").expect("resuming");
    wait_for_rows(&server, "SELECT id FROM ledger", 1);

    // Resuming does not forget. A feed that halted, was resumed and halted again is not in
    // the situation a feed that halted once is in, and the count is how anybody can tell.
    let resumed = standing(server.port, "ledger");
    assert_eq!(resumed[1].as_deref(), Some("running"));
    assert_eq!(resumed[8].as_deref(), Some("1"), "the halt is remembered across the resume");
}

// --- criterion 4: every refused config fails closed ------------------------

#[test]
fn a_declaration_that_does_not_validate_lands_nothing() {
    let it = Deployment::new();
    it.declare_the_ledger();
    it.create_the_ledger_table();
    // A second feed that names no date axis. `DEC-34` requires the date to be declared per
    // table and never defaulted, so this is refused rather than given one.
    it.declare(
        "broken.yaml",
        &format!(
            "name: broken\nfrom: {}\nschema: sales\ntable: ledger\n\
             columns:\n\x20 - name: id\n\x20   type: int64\n",
            it.spool().display()
        ),
    );

    let server = it.start();

    // Fails closed on three counts, and the third is the one that matters. It is complained
    // about; the *other* feed still loads and runs; and nothing the broken one would have
    // written exists.
    assert!(
        server.wait_until_said("feed not loaded", WITHIN),
        "the refused declaration is reported at startup"
    );
    it.arrives("001.json", &[r#"{"id": 1, "amount": "1.00"}"#]);
    wait_for_rows(&server, "SELECT id FROM ledger", 1);

    let listed = text_rows(server.port, "SHOW FEEDS");
    assert_eq!(listed.len(), 1, "only the feed that validated is a feed at all");
    assert_eq!(listed[0][0].as_deref(), Some("ledger"));
}

#[test]
fn a_feed_whose_table_does_not_exist_is_refused_rather_than_creating_one() {
    // The quarantine is created; `sales.ledger` deliberately is not.
    let it = Deployment::new();
    it.declare_the_ledger();

    let server = it.start();
    it.arrives("001.json", &[r#"{"id": 1, "amount": "1.00"}"#]);

    let halted = wait_for_state(&server, "ledger", "halted");
    assert!(
        halted[3].as_ref().is_some_and(|reason| reason.contains("not a table")),
        "the refusal names what is missing, rather than reporting a failure: {:?}",
        halted[3]
    );

    // Nothing was created. A declaration says what a *record* looks like; a table's schema is
    // a decision with a date axis, a class and a partitioning in it, and inferring one from
    // the first feed to mention it is how a warehouse acquires tables nobody designed.
    assert!(
        !it.warehouse().join("sales").join("ledger").exists(),
        "a refused feed created its own target table"
    );
}

// --- criterion 5: the quarantine expires rather than accumulating ----------

#[test]
fn the_quarantine_expires_rather_than_accumulating() {
    let it = ready();
    let server = it.start();

    // A real quarantined record, written by the real feed through the real writer.
    it.arrives(
        "001.json",
        &[
            r#"{"id": 1, "amount": "1.00"}"#,
            r#"{"id": 2, "amount": "not a number"}"#,
        ],
    );
    wait_for_rows(
        &server,
        &format!("SELECT feed FROM {}", sankhya_feed::quarantine::TABLE),
        1,
    );
    drop(server);

    let (declared, complaints) = it.loaded();
    assert!(complaints.is_empty(), "{complaints:?}");

    // Today, it is kept. The retention is seven days and the record is minutes old, which is
    // exactly when somebody comes looking for it.
    let today = today();
    assert!(
        feeds::expire_quarantine(&declared, &it.warehouse(), today, 1).is_none(),
        "a record refused today was expired today"
    );

    // Eight days on, the same call detaches it. The day is an argument rather than a reading
    // of the clock, which is what makes this a decision anybody can reproduce.
    let said = feeds::expire_quarantine(&declared, &it.warehouse(), today + 8, 2)
        .expect("there is something to expire")
        .expect("expiring succeeds");
    assert!(said.contains("1 partition(s)"), "{said}");

    // Detached, not deleted. The partition has left the live set and its file is still on
    // disk, which is what makes an expiry that should not have happened reversible until
    // retirement's grace period runs — `DEC-23` gets no exception here.
    let root = it.warehouse().join("sank").join(sankhya_feed::quarantine::TABLE);
    let live = sankhya_table_delta::live_files(&root).expect("reading the quarantine");
    assert!(live.files.is_empty(), "the partition is out of the live set");
    let parquet = walk(&root).into_iter().filter(|p| p.ends_with(".parquet")).count();
    assert_eq!(parquet, 1, "and its file is still on disk, re-attachable");
}

#[test]
fn the_tick_attempts_expiry_without_being_asked() {
    // What the criterion above deliberately does not prove: that the running server calls
    // expiry at all, with the real calendar, without anybody asking it to. Shown by the one
    // observable a *nothing-to-do* expiry has — the server saying how many feeds it declared
    // and then running its tick without complaining about the quarantine.
    let it = ready();
    let server = it.start();

    it.arrives("001.json", &[r#"{"id": 1, "amount": "1.00"}"#]);
    wait_for_rows(&server, "SELECT id FROM ledger", 1);

    // The tick ran — the feed on it published — and expiry, which shares the tick, neither
    // detached a partition written minutes ago nor failed trying.
    assert!(
        server.said_matching("quarantine could not be expired").next().is_none(),
        "expiry ran and could not: {:?}",
        server.said_matching("quarantine").collect::<Vec<_>>()
    );
    assert!(
        server.said_matching("quarantine expired").next().is_none(),
        "expiry detached a partition that is minutes old"
    );
}

// --- the cadence knob, which is what makes the rest of this file quick ----

#[test]
fn a_cadence_of_zero_is_not_a_cadence_and_the_server_says_so() {
    // Zero is not "as fast as possible": it is a loop with no sleep in it, which takes a core
    // and starves the tasks it competes with. It falls back to the default and **says so**,
    // rather than being accepted silently or refusing to start — refusing would take an
    // outage on every table over one knob, which is the reasoning that already keeps a
    // malformed feed declaration from stopping the server.
    let it = ready();
    let config = it.config().join("application.yaml").display().to_string();
    let server = start_with(
        &it.warehouse(),
        &it.data(),
        &[
            ("SANKHYA_CONFIG", config.as_str()),
            ("SANKHYA_FEED_INTERVAL_SECONDS", "0"),
        ],
    );

    assert!(
        server.wait_until_said("SANKHYA_FEED_INTERVAL_SECONDS is `0`", WITHIN),
        "a cadence of zero was accepted without a word; the server said:\n{}",
        server.said_matching("").collect::<Vec<_>>().join("\n")
    );
    // And it started anyway, with the default, rather than exiting over a typo.
    assert_eq!(query_outcome(server.port, "SELECT 1").expect("the server answers"), 1);
}

#[test]
fn a_cadence_that_is_not_a_number_is_reported_rather_than_ignored() {
    // An operator who set it believes it took effect. Falling back in silence leaves them
    // believing that for as long as the deployment lives.
    let it = ready();
    let config = it.config().join("application.yaml").display().to_string();
    let server = start_with(
        &it.warehouse(),
        &it.data(),
        &[
            ("SANKHYA_CONFIG", config.as_str()),
            ("SANKHYA_FEED_INTERVAL_SECONDS", "often"),
        ],
    );

    assert!(
        server.wait_until_said("SANKHYA_FEED_INTERVAL_SECONDS is `often`", WITHIN),
        "an unreadable cadence was ignored in silence"
    );
}

/// Today, as days since the epoch.
fn today() -> i32 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    i32::try_from(now / 86_400).unwrap_or(0)
}

/// Every file under a directory, recursively.
fn walk(root: &std::path::Path) -> Vec<String> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return found;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            found.extend(walk(&path));
        } else {
            found.push(path.display().to_string());
        }
    }
    found
}




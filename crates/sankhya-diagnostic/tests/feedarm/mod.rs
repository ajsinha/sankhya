//! The soak's ingest arm: documents arriving, rows published, refusals quarantined.
//!
//! # What this adds that the rest of the soak cannot
//!
//! Everything else in the run publishes through a `Publication` the harness drives directly.
//! That exercises the write path and says nothing about the path a *deployment* uses, which
//! starts with a file somebody dropped in a directory.
//!
//! It is also the only arm whose correctness statement is exact rather than statistical.
//! Every document written here is known: how many were sound, how many were deliberately
//! malformed, and therefore exactly how many rows must be in the table and exactly how many
//! records must be in the quarantine. A discrepancy is not a threshold being missed --- it is
//! a row that arrived twice or did not arrive.
//!
//! # Why one in twenty is deliberately malformed
//!
//! Under the stop rate, so the feed runs for the whole soak rather than halting in the first
//! minute --- and above zero, so the quarantine path is exercised under load rather than only
//! in unit tests. A soak that only ever sends valid documents proves the happy path is
//! durable and nothing about the path that runs when a producer misbehaves.

#![allow(
    dead_code,
    unreachable_pub,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_feed::declare::{
    Column, DateFrom, Declaration, Microbatch, Missing, Quarantine, Unknown,
};
use sankhya_feed::run::{run, Running};
use sankhya_feed::validate::{validate, Feed};
use sankhya_feed::{quarantine, shape};
use sankhya_publish::Publication;
use std::path::{Path, PathBuf};

/// One document in twenty is malformed.
const BAD_IN: u64 = 20;

/// How many documents each round writes.
const PER_ROUND: u64 = 200;

/// The ingest arm's state for a run.
pub struct FeedArm {
    feed: Feed,
    spool: PathBuf,
    table: Publication,
    quarantine: Publication,
    /// Documents written, sound and malformed.
    pub written: u64,
    /// Documents deliberately malformed.
    pub malformed: u64,
    /// Rows the feed reported publishing.
    pub published: u64,
    /// Records the feed reported quarantining.
    pub quarantined: u64,
    /// Rounds in which the feed reported stopping.
    pub stopped: u64,
    /// The next source's ordinal, so names sort in the order they are written.
    round: u64,
}

impl FeedArm {
    /// Create the tables and the spool this arm needs, under the soak's warehouse.
    pub fn open(warehouse: &Path, schema: &str) -> Self {
        let feed = declaration(schema);
        let spool = warehouse.join("_spool");
        std::fs::create_dir_all(&spool).expect("a spool directory");

        let table = Publication::external(warehouse.join(schema).join("feed_orders"), "feed_orders");
        table.create(&shape::table_schema(&feed)).expect("the ingest table");
        let quarantined = Publication::external(
            warehouse.join(schema).join(quarantine::TABLE),
            quarantine::TABLE,
        );
        quarantined.create(&quarantine::schema()).expect("the quarantine");

        Self {
            feed,
            spool,
            table,
            quarantine: quarantined,
            written: 0,
            malformed: 0,
            published: 0,
            quarantined: 0,
            stopped: 0,
            round: 0,
        }
    }

    /// Write one source and run the feed over it.
    ///
    /// Returns the sentence to print when something happened worth saying, and `None` when the
    /// round was ordinary --- a line per round for forty-five minutes is a log nobody reads.
    pub fn step(&mut self, now: i64) -> Option<String> {
        let name = format!("{:08}.json", self.round);
        self.round = self.round.saturating_add(1);

        let mut lines = String::new();
        for index in 0..PER_ROUND {
            let id = self.written.saturating_add(index);
            if id % BAD_IN == BAD_IN - 1 {
                // A number where the declaration says a string. Well-formed JSON, refused by
                // the binder — which is the interesting case, not a corrupt line.
                lines.push_str(&format!("{{\"id\": {id}, \"amount\": {}}}\n", id % 97));
                self.malformed = self.malformed.saturating_add(1);
            } else {
                lines.push_str(&format!(
                    "{{\"id\": {id}, \"amount\": \"{}.{:02}\"}}\n",
                    id % 1_000,
                    id % 100
                ));
            }
        }
        std::fs::write(self.spool.join(&name), lines).expect("writing a source");
        self.written = self.written.saturating_add(PER_ROUND);

        let clock = move || now;
        let ran = run(
            &self.feed,
            &self.spool,
            Running {
                table: &self.table,
                quarantine: &self.quarantine,
                table_version: self.table.next_version(),
                quarantine_version: self.quarantine.next_version(),
                now: &clock,
            },
        );
        match ran {
            Ok(result) => {
                self.published = self.published.saturating_add(result.published);
                self.quarantined = self.quarantined.saturating_add(result.quarantined);
                if let Some(reason) = result.stopped {
                    self.stopped = self.stopped.saturating_add(1);
                    return Some(format!("the feed stopped: {reason}"));
                }
                None
            }
            Err(error) => Some(format!("the feed could not run: {error}")),
        }
    }

    /// What must hold at the end of the run, as a list of complaints.
    ///
    /// Returned rather than asserted, so the harness reports every arm's findings together
    /// instead of the first one panicking and hiding the rest.
    pub fn reconcile(&self) -> Vec<String> {
        let mut complaints = Vec::new();
        let sound = self.written.saturating_sub(self.malformed);

        if self.stopped > 0 {
            complaints.push(format!(
                "the feed stopped {} time(s); one document in {BAD_IN} is malformed and the \
                 stop rate is set above that, so it should have run throughout",
                self.stopped
            ));
        }
        if self.published != sound {
            complaints.push(format!(
                "{} sound document(s) were written and the feed published {} --- a row \
                 arrived twice or did not arrive",
                sound, self.published
            ));
        }
        if self.quarantined != self.malformed {
            complaints.push(format!(
                "{} malformed document(s) were written and {} were quarantined",
                self.malformed, self.quarantined
            ));
        }
        complaints
    }

    /// Where the ingested rows landed, for a caller that wants to read them back.
    pub fn table_root(&self) -> &Path {
        &self.table.root
    }
}

/// The feed this arm runs: two columns, ingest-dated, tolerant enough to survive the run.
fn declaration(schema: &str) -> Feed {
    validate(Declaration {
        name: "soak_orders".to_owned(),
        // Carried for the fingerprint; the arm passes the real directory to `run` because
        // the soak's warehouse is chosen at run time and a declaration is written before it.
        from: String::from("_spool"),
        schema: schema.to_owned(),
        table: "feed_orders".to_owned(),
        columns: vec![
            Column {
                name: "id".to_owned(),
                from: None,
                written_type: "int64".to_owned(),
                nullable: false,
                missing: Missing::Refuse,
            },
            Column {
                name: "amount".to_owned(),
                from: None,
                written_type: "decimal(18,2)".to_owned(),
                nullable: false,
                missing: Missing::Refuse,
            },
        ],
        date: Some(DateFrom::Ingest),
        unknown: Unknown::Refuse,
        // Small batches on purpose: more commits, more contention with the maintenance the
        // soak is also running, and more chances for the position and the rows to disagree.
        microbatch: Microbatch { rows: 64, seconds: 3_600 },
        // A tenth, comfortably above the one-in-twenty this arm writes and comfortably below
        // what a genuinely broken source would produce.
        quarantine: Quarantine { retain_days: 30, window: 200, stop_above: 0.10 },
    })
    .expect("the soak's feed declaration is sound")
}

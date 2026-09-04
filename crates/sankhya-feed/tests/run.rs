//! A feed, run against real tables on disk.
//!
//! # What only an end-to-end test can show
//!
//! Every other test in this crate holds one decision still and checks it. These check the
//! sequencing --- that the position moves with the rows and not beside them, that a restart
//! neither duplicates nor skips, that a wholly-quarantined source still counts as progress.
//! Those are the failures that survive a suite of correct units.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_feed::declare::{
    Column, DateFrom, Declaration, Microbatch, Missing, Quarantine, Unknown,
};
use sankhya_feed::progress::{key, Position};
use sankhya_feed::run::{run, Ran, RunError, Running};
use sankhya_feed::validate::{validate, Feed};
use sankhya_feed::{quarantine as quarantine_table, shape};
use sankhya_publish::Publication;
use std::path::Path;

/// A fixed moment, so a quarantined record's stamp is asserted rather than observed.
const NOW: i64 = 1_756_000_000_000_000;

/// A feed of two columns landing in `sales.orders`.
fn feed() -> Feed {
    validate(Declaration {
        name: "orders".to_owned(),
        from: "/spool".to_owned(),
        schema: "sales".to_owned(),
        table: "orders".to_owned(),
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
        microbatch: Microbatch { rows: 2, seconds: 3_600 },
        quarantine: Quarantine { retain_days: 30, window: 100, stop_above: 0.5 },
    })
    .expect("a sound feed")
}

/// A spool directory holding the given files, each a list of lines.
fn spool(at: &Path, files: &[(&str, &[&str])]) {
    std::fs::create_dir_all(at).expect("a spool");
    for (name, lines) in files {
        std::fs::write(at.join(name), format!("{}\n", lines.join("\n"))).expect("a source");
    }
}

/// The two tables a feed writes to, created.
fn tables(root: &Path, feed: &Feed) -> (Publication, Publication) {
    let table = Publication::external(root.join("orders"), "orders");
    table.create(&shape::table_schema(feed)).expect("the target table");
    let quarantine = Publication::external(root.join("quarantine"), quarantine_table::TABLE);
    quarantine.create(&quarantine_table::schema()).expect("the quarantine");
    (table, quarantine)
}

/// Run once, from the versions a fresh pair of tables is at.
fn once(feed: &Feed, spool_at: &Path, table: &Publication, quarantine: &Publication) -> Ran {
    let now = || NOW;
    run(
        feed,
        spool_at,
        Running {
            table,
            quarantine,
            table_version: table.next_version(),
            quarantine_version: quarantine.next_version(),
            now: &now,
        },
    )
    .expect("the run")
}

#[test]
fn records_that_fit_are_published_and_the_position_moves_with_them() {
    let dir = tempfile::tempdir().expect("a directory");
    let feed = feed();
    let (table, quarantine) = tables(dir.path(), &feed);
    spool(
        &dir.path().join("spool"),
        &[("2026-08-30.json", &[
            r#"{"id": 1, "amount": "10.00"}"#,
            r#"{"id": 2, "amount": "20.50"}"#,
            r#"{"id": 3, "amount": "0.01"}"#,
        ])],
    );

    let ran = once(&feed, &dir.path().join("spool"), &table, &quarantine);

    assert_eq!(ran.published, 3);
    assert_eq!(ran.quarantined, 0);
    assert_eq!(ran.sources, 1);
    assert_eq!(ran.stopped, None);

    // The position is on the table, in its own log, put there by the same commits that
    // published the rows.
    let recorded = table.property(&key("orders")).expect("a recorded position");
    let position = Position::from_property(&recorded).expect("a readable position");
    assert_eq!(position.through, "2026-08-30.json");
    assert_eq!(position.partial, None);
}

#[test]
fn a_second_run_over_the_same_directory_publishes_nothing() {
    // The property the whole position mechanism exists for. A feed that re-read a finished
    // source would give the table every row twice, and nothing about the result would say so.
    let dir = tempfile::tempdir().expect("a directory");
    let feed = feed();
    let (table, quarantine) = tables(dir.path(), &feed);
    let spool_at = dir.path().join("spool");
    spool(&spool_at, &[("2026-08-30.json", &[r#"{"id": 1, "amount": "1.00"}"#])]);

    let first = once(&feed, &spool_at, &table, &quarantine);
    assert_eq!(first.published, 1);

    let second = once(&feed, &spool_at, &table, &quarantine);
    assert_eq!(second.published, 0, "a finished source is done");
    assert_eq!(second.sources, 0);
}

#[test]
fn a_record_that_does_not_fit_is_quarantined_whole_and_the_rest_still_arrive() {
    let dir = tempfile::tempdir().expect("a directory");
    let feed = feed();
    let (table, quarantine) = tables(dir.path(), &feed);
    let spool_at = dir.path().join("spool");
    spool(
        &spool_at,
        &[("2026-08-30.json", &[
            r#"{"id": 1, "amount": "1.00"}"#,
            r#"{"id": "two", "amount": "2.00"}"#,
            r#"{"id": 3, "amount": "3.00"}"#,
        ])],
    );

    let ran = once(&feed, &spool_at, &table, &quarantine);

    // One bad record is an incident: it is set aside and the other two arrive.
    assert_eq!(ran.published, 2);
    assert_eq!(ran.quarantined, 1);
    assert_eq!(ran.stopped, None);
    assert!(quarantine.next_version() > 1, "the quarantine was written to");
}

#[test]
fn a_source_that_produced_nothing_usable_stops_the_feed_and_leaves_the_rest_unread() {
    let dir = tempfile::tempdir().expect("a directory");
    let feed = feed();
    let (table, quarantine) = tables(dir.path(), &feed);
    let spool_at = dir.path().join("spool");
    spool(
        &spool_at,
        &[
            ("2026-08-30.json", &[r#"{"id": "x", "amount": "1.00"}"#, r#"{"nope": 1}"#]),
            ("2026-08-31.json", &[r#"{"id": 9, "amount": "9.00"}"#]),
        ],
    );

    let ran = once(&feed, &spool_at, &table, &quarantine);

    assert_eq!(ran.quarantined, 2);
    assert_eq!(ran.published, 0);
    assert!(ran.stopped.is_some(), "a source with nothing usable in it is an outage");
    // And the next source is not read. A feed that carried on would leave every dashboard
    // green while the thing that changed shape went on producing nothing.
    assert_eq!(ran.sources, 1, "the second source was not started");
}

#[test]
fn a_wholly_quarantined_source_still_counts_as_progress() {
    // The subtle one. If the position only moved when rows were published, a source that
    // produced nothing usable would be re-read on every restart — and quarantined again,
    // every time, for ever.
    let dir = tempfile::tempdir().expect("a directory");
    let mut declaration = feed().declaration().clone();
    // A rate high enough that a wholly-bad short source does not stop the feed here: this
    // test is about the position, and the stop control has its own.
    declaration.quarantine.stop_above = 0.99;
    let forgiving = validate(declaration).expect("still sound");
    let (table, quarantine) = tables(dir.path(), &forgiving);
    let spool_at = dir.path().join("spool");
    spool(&spool_at, &[("a.json", &[r#"{"id": "x", "amount": "1.00"}"#])]);

    let ran = once(&forgiving, &spool_at, &table, &quarantine);
    assert_eq!(ran.quarantined, 1);
    assert_eq!(ran.published, 0);

    let recorded = table.property(&key("orders")).expect("a position");
    let position = Position::from_property(&recorded).expect("readable");
    assert_eq!(position.through, "a.json", "the source is finished, even having produced nothing");
}

#[test]
fn a_source_arriving_behind_the_mark_is_counted_and_not_ingested() {
    let dir = tempfile::tempdir().expect("a directory");
    let feed = feed();
    let (table, quarantine) = tables(dir.path(), &feed);
    let spool_at = dir.path().join("spool");
    spool(&spool_at, &[("2026-08-30.json", &[r#"{"id": 1, "amount": "1.00"}"#])]);
    once(&feed, &spool_at, &table, &quarantine);

    // A file appearing behind the mark is indistinguishable from one this feed finished
    // earlier, because a high-water mark records where it got to and not which files it
    // read. Never re-ingesting is the error this design prefers — duplication is silent and
    // permanent, where a skipped source is a file still sitting in the directory — and the
    // count is what makes an unexpected number visible.
    spool(&spool_at, &[("2026-08-29.json", &[r#"{"id": 99, "amount": "9.99"}"#])]);
    let ran = once(&feed, &spool_at, &table, &quarantine);

    assert_eq!(ran.published, 0, "nothing behind the mark is read again");
    assert_eq!(ran.already_read, 2, "both the finished source and the one behind it");
}

#[test]
fn a_line_that_is_not_json_is_quarantined_rather_than_ending_the_run() {
    // The ordinary state of a file somebody is still writing: everything up to the last
    // complete line is readable, and the truncated tail is a record like any other.
    let dir = tempfile::tempdir().expect("a directory");
    let feed = feed();
    let (table, quarantine) = tables(dir.path(), &feed);
    let spool_at = dir.path().join("spool");
    spool(
        &spool_at,
        &[("a.json", &[r#"{"id": 1, "amount": "1.00"}"#, r#"{"id": 2, "amo"#])],
    );

    let ran = once(&feed, &spool_at, &table, &quarantine);

    assert_eq!(ran.published, 1);
    assert_eq!(ran.quarantined, 1);
}

#[test]
fn a_position_nobody_can_read_stops_the_run_rather_than_starting_over() {
    // *No position* means a feed that has not run. *A position nobody can read* means one
    // whose progress is unknown, and treating the second as the first gives the table every
    // row a second time.
    let dir = tempfile::tempdir().expect("a directory");
    let feed = feed();
    let (table, quarantine) = tables(dir.path(), &feed);
    let spool_at = dir.path().join("spool");
    spool(&spool_at, &[("a.json", &[r#"{"id": 1, "amount": "1.00"}"#])]);
    once(&feed, &spool_at, &table, &quarantine);

    // Corrupt the recorded position through the writer that owns the table, which is the
    // only way anything in this system is allowed to change one.
    let mut properties = std::collections::BTreeMap::new();
    properties.insert(key("orders"), "{ not a position".to_owned());
    let empty = shape::batch(&feed, &[]).expect("an empty batch");
    table
        .append_recording(
            table.next_version(),
            4,
            "corrupting.parquet",
            &empty,
            sankhya_types::Lsn::new(99),
            &properties,
        )
        .expect("the corrupting commit");

    let now = || NOW;
    let refused = run(
        &feed,
        &spool_at,
        Running {
            table: &table,
            quarantine: &quarantine,
            table_version: table.next_version(),
            quarantine_version: quarantine.next_version(),
            now: &now,
        },
    )
    .expect_err("an unreadable position is not a fresh one");

    match refused {
        RunError::UnreadablePosition { feed, .. } => assert_eq!(feed, "orders"),
        other => panic!("expected an unreadable position, got {other}"),
    }
}

#[test]
fn a_feed_that_stops_part_way_through_a_source_leaves_a_position_to_resume_from() {
    // The record-level stop, as distinct from the source-level one: the window fills while
    // the source is still being read, so the feed stops with a file half-read. What must
    // survive is a position that says *how far*, or a restart re-reads what it published.
    let dir = tempfile::tempdir().expect("a directory");
    let mut declaration = feed().declaration().clone();
    declaration.quarantine.window = 4;
    declaration.quarantine.stop_above = 0.5;
    declaration.microbatch = Microbatch { rows: 1, seconds: 3_600 };
    let touchy = validate(declaration).expect("still sound");
    let (table, quarantine) = tables(dir.path(), &touchy);
    let spool_at = dir.path().join("spool");
    spool(
        &spool_at,
        &[
            ("a.json", &[
                r#"{"id": 1, "amount": "1.00"}"#,
                r#"{"id": "x", "amount": "1.00"}"#,
                r#"{"id": "x", "amount": "1.00"}"#,
                r#"{"id": "x", "amount": "1.00"}"#,
                r#"{"id": 5, "amount": "5.00"}"#,
                r#"{"id": 6, "amount": "6.00"}"#,
            ]),
            ("b.json", &[r#"{"id": 9, "amount": "9.00"}"#]),
        ],
    );

    let ran = once(&touchy, &spool_at, &table, &quarantine);

    assert!(ran.stopped.is_some(), "three of the last four did not fit");
    assert_eq!(ran.sources, 0, "the source it stopped in is not a finished source");
    // And the next source is untouched: a feed that stopped is stopped, not slowed.
    let recorded = table.property(&key("orders")).expect("a position");
    let position = Position::from_property(&recorded).expect("readable");
    assert_eq!(position.through, "", "no source has been finished");
    let partial = position.partial.expect("a partial position to resume from");
    assert_eq!(partial.source, "a.json");
    assert!(partial.read_through >= 1, "it read something before stopping");
}

#[test]
fn a_resume_after_a_refusal_does_not_publish_the_same_record_twice() {
    // `ING-01`. The position held the count of records **published** while the resume skipped
    // by **line index**, and those are the same number only when every line so far fitted.
    //
    // The conflation only bites where a source does **not** finish, because a finished source
    // is skipped whole. So this stops the feed part-way, which is the state a restart actually
    // finds: two good lines published at 0 and 2, refusals between and after them, and a
    // partial position that has to say *five lines read* rather than *two rows written*.
    //
    // Recorded as two, a restart skips two lines and re-reads line 2 — publishing it again.
    // One duplicate row per preceding refusal or blank, silently and permanently.
    // `ADR-0018`'s amendment chose *never re-ingest* over *never duplicate* for exactly this
    // reason: duplication is the one that cannot be found afterwards.
    let dir = tempfile::tempdir().expect("a directory");
    let mut declaration = feed().declaration().clone();
    declaration.quarantine.window = 4;
    declaration.quarantine.stop_above = 0.5;
    // One row per batch, so the position is written part-way rather than only at the end.
    declaration.microbatch = Microbatch { rows: 1, seconds: 3_600 };
    let stepwise = validate(declaration).expect("still sound");
    let (table, quarantine) = tables(dir.path(), &stepwise);
    let spool_at = dir.path().join("spool");
    spool(
        &spool_at,
        &[("a.json", &[
            r#"{"id": 1, "amount": "1.00"}"#,
            r#"{"id": "x", "amount": "2.00"}"#,
            r#"{"id": 3, "amount": "3.00"}"#,
            r#"{"id": "x", "amount": "4.00"}"#,
            r#"{"id": "x", "amount": "5.00"}"#,
            r#"{"id": "x", "amount": "6.00"}"#,
        ])],
    );

    let first = once(&stepwise, &spool_at, &table, &quarantine);
    assert!(first.stopped.is_some(), "the fixture must stop part-way, not finish");
    assert_eq!(first.published, 2, "lines 0 and 2 are the publishable ones");

    let recorded = table.property(&key("orders")).expect("a position");
    let position = Position::from_property(&recorded).expect("readable");
    assert_eq!(position.through, "", "the source did not finish");
    let partial = position.partial.clone().expect("a partial position to resume from");
    assert!(
        partial.read_through >= 3,
        "the position says {} lines read, and line 2 was published — so a restart re-reads it",
        partial.read_through
    );

    // The restart. Rows in the table is the assertion that matters: a duplicate is invisible
    // in every counter the run reports, because `published` counts what was sent.
    let _ = once(&stepwise, &spool_at, &table, &quarantine);
    let live = sankhya_table_delta::live_files(&dir.path().join("orders")).expect("a live set");
    let rows: u64 = live.files.iter().filter_map(sankhya_table_delta::AddFile::rows).sum();
    assert_eq!(
        rows, 2,
        "the table holds {rows} rows for two publishable records; a resume that skips by the \
         wrong unit republishes what it already wrote"
    );
}

#[test]
fn a_blank_line_is_not_a_record_and_is_not_a_refusal_either() {
    // Files written by hand have them, and so do files ending in a newline. Quarantining
    // them would fill the quarantine with nothing and, at four in a row, stop the feed for
    // a source that is perfectly fine.
    let dir = tempfile::tempdir().expect("a directory");
    let feed = feed();
    let (table, quarantine) = tables(dir.path(), &feed);
    let spool_at = dir.path().join("spool");
    spool(
        &spool_at,
        &[("a.json", &[
            r#"{"id": 1, "amount": "1.00"}"#,
            "",
            "   ",
            r#"{"id": 2, "amount": "2.00"}"#,
        ])],
    );

    let ran = once(&feed, &spool_at, &table, &quarantine);

    assert_eq!(ran.published, 2);
    assert_eq!(ran.quarantined, 0, "a blank line is not a record that does not fit");
    assert_eq!(ran.stopped, None);
}

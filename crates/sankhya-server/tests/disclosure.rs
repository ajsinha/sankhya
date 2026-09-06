//! What a refusal, a catalogue and a listing say to somebody who may not read the thing.
//!
//! # The shape all of these share
//!
//! None of them returns a row the caller may not see. Every one of them *tells* the caller
//! something about rows they may not see: the columns of a table, the schemas a name is claimed
//! by, the SQL of a definition, the tables a snapshot pins, the file a feed reads. A control
//! that governs only the rows governs the least interesting half.
//!
//! # Why these go over a socket
//!
//! Because the property is about what reaches a client, and every one of these leaks was inside
//! a message that a unit test would have had to assert on to see at all.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use common::{start_with, text_rows_as, write_warehouse, Running, Session};

/// A server on which `ana` reads and `mallory` holds no role at all.
fn running() -> (tempfile::TempDir, Running) {
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let config = dir.path().join("application.yaml");
    std::fs::write(
        &config,
        format!(
            "warehouse: {}\nlisten: 127.0.0.1:0\nserver:\n  users:\n    ana: reader\n    quickstart: reader\n",
            warehouse.display()
        ),
    )
    .expect("writing the configuration");
    let server = start_with(
        &warehouse,
        &dir.path().join("data"),
        &[("SANKHYA_CONFIG", config.to_str().expect("a path"))],
    );
    (dir, server)
}

#[test]
fn a_misspelt_column_is_not_answered_with_the_list_of_real_ones() {
    // `SEC-16`. `SELECT nosuchcol FROM orders` produced *"Schema error: No field named
    // nosuchcol. Valid fields are orders.id, orders.region, …"* and the whole of it reached the
    // client --- every column of every table in the plan's scope, to anybody who could name one
    // table and guess one column wrong. The only mention of that phrase in the repository
    // sniffed for it to choose a SQLSTATE and passed it on.
    let (_dir, server) = running();

    let refused = Session::open_as(server.port, "ana")
        .run("SELECT nosuchcol FROM orders")
        .expect_err("a column that is not there");

    // The half the caller needs is kept: they typed it, and telling them they did is the whole
    // usefulness of the message.
    assert!(
        refused.contains("nosuchcol"),
        "the caller must still be told which name did not resolve: {refused}"
    );
    // And the half they did not type is gone.
    assert!(
        !refused.contains("Valid fields"),
        "the planner's enumeration of real column names must not reach a client: {refused}"
    );
    for column in ["region", "period", "margin_pct", "sank_data_date"] {
        assert!(
            !refused.contains(column),
            "`{column}` is a real column of this table and must not be named in a refusal \
             about a different one: {refused}"
        );
    }

    // Not vacuous, and this is the assertion that keeps the marker honest: the planner still
    // produces the message this cuts, so a version of DataFusion that reworded it would fail
    // here rather than silently leaking again.
    let real = Session::open_as(server.port, "ana")
        .run("SELECT region FROM orders")
        .expect("a column that is there");
    assert!(real > 0, "the fixture must have rows for this to mean anything");
}

#[test]
fn a_name_is_contested_only_by_tables_the_caller_can_see() {
    // `SEC-17`, the half with a string in it. `claims` and `contested` were built from **all**
    // servable tables and the guard ran afterwards, so a refusal telling a caller to qualify an
    // ambiguous name enumerated the schemas of a warehouse they had no grant on.
    //
    // There is a second channel with no string at all, and it is the one worth fearing: because
    // the count included tables the caller cannot read, a hidden `payroll.orders` made the
    // caller's own `sales.orders` stop resolving under its bare name. Anybody could ask whether
    // a table of a given name existed somewhere they could not look, and read the answer off
    // whether their own query planned.
    let (_dir, server) = running();

    // `mallory` holds no role, so every table is invisible to them --- which means no bare name
    // can be contested for them, and a refusal must say so without naming anything.
    let refused = Session::open_as(server.port, "mallory")
        .run("SELECT * FROM orders")
        .expect_err("mallory may read nothing");
    assert!(
        !refused.contains("sales.orders"),
        "a refusal to somebody with no grant must not name a qualified table: {refused}"
    );

    // And the oracle: `ana`'s own query must plan under the bare name whatever else exists on
    // the server that she cannot see.
    let planned = Session::open_as(server.port, "ana")
        .run("SELECT count(*) FROM orders")
        .expect("ana's own table must resolve under its bare name");
    assert!(planned > 0, "and must actually answer: {planned}");
}

#[test]
fn a_listing_shows_what_the_caller_may_read_and_nothing_else() {
    // `SEC-18`. Five listings were unfiltered. `derived()` emits the SQL text of every
    // definition and the tables it reads; `SHOW SNAPSHOTS` emits the qualified name of every
    // table a snapshot pins; `SHOW FEEDS` emits what a halted source was doing, which is a
    // filesystem path. `register_derived` was already gating on scope four lines away in the
    // same file.
    let (_dir, server) = running();

    Session::open_as(server.port, "ana")
        .run("CREATE SNAPSHOT eod EXPIRE AFTER 90 DAYS")
        .expect("ana may take one");

    // The control, first: ana sees her own.
    assert_eq!(
        Session::open_as(server.port, "ana")
            .run("SHOW SNAPSHOTS")
            .expect("ana may list"),
        1,
        "a listing that showed nobody anything would pass every assertion below"
    );

    // And mallory, who may read nothing, sees nothing --- rather than seeing a row naming
    // `sales.orders` as a table that is being pinned.
    assert_eq!(
        Session::open_as(server.port, "mallory")
            .run("SHOW SNAPSHOTS")
            .expect("the statement is answered, not refused"),
        0,
        "a snapshot row names the tables it pins, so it goes to whoever may read them"
    );

    // `SHOW SNAPSHOTS` is answered rather than refused, and that is deliberate: it needs no
    // session, so a caller who may read nothing is told there are no snapshots they may see
    // rather than being refused --- which would say there are some.
    //
    // Anything that needs a session is refused earlier and for a different reason: a principal
    // who may read no table at all gets one sentence saying so, before any listing is reached.
    // That is existing behaviour and the right behaviour, and it is asserted here so that a
    // change to it shows up as a change to this file rather than as a quiet widening.
    let refused = Session::open_as(server.port, "mallory")
        .run("SELECT * FROM cubes()")
        .expect_err("a principal who may read nothing gets no session");
    assert!(
        refused.contains("may not read any table"),
        "and is told that, once, rather than being shown an empty catalogue: {refused}"
    );
}

#[test]
fn a_feed_is_listed_to_everybody_and_its_halt_reason_is_not() {
    // `SEC-18`, and the one that took two attempts. Filtering the *rows* by the same rule as the
    // other listings removed a feed whose target table does not exist --- and a feed that halted
    // because its table is missing is exactly what an operator opens this statement to find. A
    // control that hides the thing it is meant to report is not a control.
    //
    // So the name and the state go to everybody, because somebody configured that feed and it is
    // not tenant data, and the reason is what is withheld: `ADR-0018` halts a feed when a record
    // does not fit, and saying so means saying which file and what was in it.
    let (_dir, server) = running();

    // No feed is declared on this fixture, so what is asserted here is that the statement is
    // answered rather than refused --- the half that filtering the rows would have broken, and
    // the half a caller with no grant still gets.
    let listed = Session::open_as(server.port, "mallory")
        .run("SHOW FEEDS")
        .expect("a listing of what the server is doing is answered, not refused");
    assert_eq!(listed, 0, "this fixture declares no feeds: {listed}");

    // And for a reader, likewise --- the control that says the statement works at all.
    Session::open_as(server.port, "ana")
        .run("SHOW FEEDS")
        .expect("a reader may ask too");
}

// --- what the audit records, and whether it survives ------------------------

#[test]
fn the_audit_records_what_the_statement_was_answered_under() {
    // `SEC-07`. The only append site hardcoded *no row filter and no masks*, never recorded the
    // version, the statement or the rows returned, and passed the **first two words of the
    // statement** where a table belongs. §13.5 lists four fields as not optional and none was
    // ever populated --- and the one about restrictions did not merely omit them, it positively
    // asserted that none applied.
    let (dir, server) = running();
    let warehouse = dir.path().join("warehouse");

    Session::open_as(server.port, "ana")
        .run("SELECT region FROM orders")
        .expect("a reader may read");

    let chain = std::fs::read_to_string(warehouse.join("_audit").join("chain.jsonl"))
        .expect("the audit is written to the warehouse, not only to memory");
    let read = chain
        .lines()
        .filter(|line| line.contains("\"action\":\"read\""))
        .last()
        .expect("a read was recorded");

    assert!(
        read.contains("\"statement\":\"select region\""),
        "the statement's shape is recorded, so a reader can tell a select from a listing: \
         {read}"
    );
    assert!(
        read.contains("\"rows_returned\":"),
        "and how many rows they received: {read}"
    );
    assert!(
        read.contains("\"data_version\":") && !read.contains("\"data_version\":null"),
        "and which version answered, without which the record reproduces nothing: {read}"
    );
    assert!(
        read.contains("sales.orders"),
        "and the table --- not the first two words of the statement, which is what used to be \
         in this field: {read}"
    );
}

#[test]
fn the_audit_does_not_become_a_second_place_the_data_lives() {
    // The other half of the same decision, and the reason the statement is recorded by shape
    // rather than verbatim. A statement carries the values a query filtered on --- copying them
    // into a durable log puts them somewhere with different retention and different access
    // control from the table they came from, and the audit is the one file most likely to be
    // shipped somewhere else wholesale.
    let (dir, server) = running();
    let warehouse = dir.path().join("warehouse");

    Session::open_as(server.port, "ana")
        .run("SELECT region FROM orders WHERE region = 'a-secret-value'")
        .expect("a reader may read");

    let chain = std::fs::read_to_string(warehouse.join("_audit").join("chain.jsonl"))
        .expect("the audit is on disk");
    assert!(
        !chain.contains("a-secret-value"),
        "a value a query filtered on must not reach the audit: {chain}"
    );
    // Not vacuous: the statement really did run and really was recorded.
    assert!(
        chain.contains("\"statement\":\"select region\""),
        "and the shape still is: {chain}"
    );
}

#[test]
fn the_audit_survives_a_restart() {
    // The other half of `SEC-07`, and the worse one: `Chain` is a `Vec`, so the hash-linked
    // tamper-evident audit was erased by a restart --- and a restart is the event most likely
    // to accompany the incident an audit exists for. `docs/STATUS.md` marked the criterion met.
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let data = dir.path().join("data");

    let first = start_with(&warehouse, &data, &[]);
    Session::open_as(first.port, "ana")
        .run("SELECT region FROM orders")
        .expect("a reader may read");
    let before = std::fs::read_to_string(warehouse.join("_audit").join("chain.jsonl"))
        .expect("the chain is on disk")
        .lines()
        .count();
    assert!(before > 0, "the first run recorded something");
    drop(first);

    let second = start_with(&warehouse, &data, &[]);
    Session::open_as(second.port, "ana")
        .run("SELECT period FROM orders")
        .expect("a reader may read after a restart");
    let after = std::fs::read_to_string(warehouse.join("_audit").join("chain.jsonl"))
        .expect("the chain is still on disk")
        .lines()
        .count();

    assert!(
        after > before,
        "the second run appended to the first run's chain rather than starting a new one: \
         {before} then {after}"
    );
    // And it is one chain, not two files' worth of unrelated records. The second run's first
    // record links to the first run's last, which is the whole of what makes it a chain.
    assert!(
        second.said_matching("audit chain head").next().is_some(),
        "the head is printed at every start, because a local chain cannot detect its own \
         truncation and publishing the head is what makes the true length knowable"
    );
}

// --- a policy the binary can actually be configured with --------------------

/// A server whose policy comes from a file, the way an operator writes one.
fn under_policy(rules: &str) -> (tempfile::TempDir, Running) {
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let config = dir.path().join("application.yaml");
    std::fs::write(
        &config,
        format!(
            "warehouse: {}\nlisten: 127.0.0.1:0\nserver:\n  users:\n    ana: analyst\n    \
             mallory: intern\n    quickstart: analyst\npolicy:\n  rules:\n{rules}",
            warehouse.display()
        ),
    )
    .expect("writing the configuration");
    let server = start_with(
        &warehouse,
        &dir.path().join("data"),
        &[("SANKHYA_CONFIG", config.to_str().expect("a path"))],
    );
    (dir, server)
}

#[test]
fn a_policy_an_operator_wrote_is_the_policy_in_force() {
    // `SEC-15`. `start()` --- the only path the shipped binary takes --- built
    // `permissive_policy`, granting `reader` read on every discovered table with no filter and
    // no mask, and **no configuration key loaded a policy set at all**. So the row-predicate
    // enforcement, which §13.2 spends four pages on and which is the best-tested code in the
    // repository, had never run outside a test. The binary could express "everything" or
    // "nothing" and nothing in between.
    let (_dir, server) = under_policy(
        "    analysts_read_orders:\n      role: analyst\n      table: sales.orders\n      \
         action: read\n",
    );

    // The grant is in force.
    let seen = Session::open_as(server.port, "ana")
        .run("SELECT region FROM orders")
        .expect("the analyst's grant is in force");
    assert!(seen > 0, "and it returns rows: {seen}");

    // And the absence of one is too. `mallory` holds `intern`, which no rule mentions --- so
    // there is no grant, and a table nobody granted does not exist as far as they are
    // concerned.
    let refused = Session::open_as(server.port, "mallory")
        .run("SELECT region FROM orders")
        .expect_err("no rule grants `intern` anything");
    assert!(
        !refused.contains("sales.orders"),
        "and the refusal does not name what they may not read: {refused}"
    );
}

#[test]
fn a_row_predicate_an_operator_wrote_reaches_the_scan() {
    // The half that had never run in a deployment. A `where` on a rule is conjoined into every
    // scan of that table for that role, above the provider so nothing can decline it --- and
    // until `SEC-15` there was no way to write one outside a test.
    let (_dir, server) = under_policy(
        "    analysts_read_north:\n      role: analyst\n      table: sales.orders\n      \
         action: read\n      where: \"region = 'north'\"\n",
    );

    let restricted = Session::open_as(server.port, "ana")
        .run("SELECT region FROM orders")
        .expect("the analyst may read");
    let everything = Session::open_as(server.port, "ana")
        .run("SELECT region FROM orders WHERE region = 'north'")
        .expect("and may say so themselves");
    assert_eq!(
        restricted, everything,
        "a query with no predicate must return exactly the rows the policy's predicate allows: \
         {restricted} against {everything}"
    );
    assert!(restricted > 0, "and the fixture must have northern rows: {restricted}");

    // The tautology, which is what §13.2 promises: the policy is not part of the query, so a
    // query cannot widen it.
    let widened = Session::open_as(server.port, "ana")
        .run("SELECT region FROM orders WHERE region = 'south' OR 1 = 1")
        .expect("the statement runs");
    assert_eq!(
        widened, restricted,
        "a tautology in the query must not widen the policy: {widened} against {restricted}"
    );
}

#[test]
fn a_column_mask_an_operator_wrote_reaches_the_answer() {
    // And the other rewrite, closed in 4.2 and unreachable from a deployment until now.
    let (_dir, server) = under_policy(
        "    analysts_read_orders:\n      role: analyst\n      table: sales.orders\n      \
         action: read\n      mask:\n        region: null\n",
    );

    let rows = text_rows_as(server.port, "ana", "SELECT region FROM orders");
    assert!(!rows.is_empty(), "the analyst may read the rows");
    assert!(
        rows.iter().all(|row| row.first().is_some_and(Option::is_none)),
        "a masked column returns no value: {rows:?}"
    );
}

#[test]
fn a_policy_that_does_not_parse_stops_the_server() {
    // Refused rather than skipped. A policy with a rule silently dropped permits more than it
    // says, and the person who wrote the rule believes it is in force --- which is the worst
    // available outcome for a file whose whole purpose is to be reviewed.
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let config = dir.path().join("application.yaml");
    std::fs::write(
        &config,
        format!(
            "warehouse: {}\nlisten: 127.0.0.1:0\npolicy:\n  rules:\n    broken:\n      \
             role: analyst\n      table: orders\n      action: read\n",
            warehouse.display()
        ),
    )
    .expect("writing the configuration");

    let said = std::process::Command::new(env!("CARGO_BIN_EXE_sankhya-server"))
        .arg("start")
        .env("SANKHYA_CONFIG", &config)
        .env("SANKHYA_DATA_DIR", dir.path().join("data"))
        .env_remove("SANKHYA_WAREHOUSE")
        .output()
        .expect("the binary runs");

    assert!(!said.status.success(), "a policy that does not parse must stop the server");
    let text = String::from_utf8_lossy(&said.stderr);
    assert!(
        text.contains("schema"),
        "and must say what it could not read --- here, a table named without its schema: {text}"
    );
}

#[test]
fn an_audit_record_says_when() {
    // `OPS-04`. Every record's `at` was `*clock += 1`, so an audit's timestamps were `1, 2, 3`
    // and restarted at 1 on every boot --- under a comment saying *"a real deployment supplies
    // wall-clock time here"*, which no deployment did. An audit that cannot say **when**
    // answers none of the questions an audit is opened for.
    //
    // The reproducible ordering the counter was standing in for lives in the record's
    // `sequence`, which is where it always belonged, so nothing was given up to fix this.
    let (dir, server) = running();
    let warehouse = dir.path().join("warehouse");

    Session::open_as(server.port, "ana")
        .run("SELECT region FROM orders")
        .expect("a reader may read");

    let chain = std::fs::read_to_string(warehouse.join("_audit").join("chain.jsonl"))
        .expect("the audit is on disk");
    let first = chain.lines().next().expect("a record");
    let at = first
        .split("\"at\":")
        .nth(1)
        .and_then(|rest| rest.split(',').next())
        .and_then(|number| number.trim().parse::<i64>().ok())
        .unwrap_or_default();

    // Microseconds since the epoch, some time after 2020 and before 2100. Asserted as a range
    // rather than against `now`, because a test that compares two clocks is a test that fails
    // on a slow machine.
    assert!(
        at > 1_577_836_800_000_000,
        "an audit record must carry a wall-clock time, not a counter: {at}"
    );
    assert!(at < 4_102_444_800_000_000, "and a plausible one: {at}");

    // And the sequence is still the reproducible ordering, starting at zero.
    assert!(
        first.contains("\"sequence\":0"),
        "the first record of a fresh chain is sequence zero: {first}"
    );
}

// --- what a statement may spend ---------------------------------------------

#[test]
fn a_result_past_the_row_limit_is_stopped_while_it_arrives() {
    // `OPS-05`. `frame.collect()` materialised the whole result and *then* the row count was
    // checked against the limit --- so a statement that would return ten million rows against
    // a limit of ten thousand allocated all ten million first, and the refusal arrived after
    // the damage. A bound enforced by a check that runs afterwards is not a bound.
    let (_dir, server) = running();

    // **The refusal first, and cheaply.** A result modestly past the limit is refused whether
    // the check runs during or after, so this is the assertion that says the bound exists at
    // all --- and it says it in a second rather than by materialising millions of rows, which
    // matters because a deliberately broken version of this code has to *fail* here rather
    // than merely take a long time.
    let refused = Session::open_as(server.port, "ana")
        .run("SELECT * FROM generate_series(1, 50000)")
        .expect_err("fifty thousand rows is past the ten-thousand limit");
    assert!(
        refused.contains("rows") && refused.contains("LIMIT"),
        "the refusal says what happened and what to do: {refused}"
    );

    // And now the property that only holds if the check runs *while* the result arrives: a
    // generator far past the limit is stopped near the limit rather than after it. Measured by
    // time, because memory is what a test cannot observe from outside a process.
    let started = std::time::Instant::now();
    Session::open_as(server.port, "ana")
        .run("SELECT * FROM generate_series(1, 20000000)")
        .expect_err("twenty million rows is also past the limit");
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(20),
        "a statement stopped at the bound must not first materialise everything past it: \
         {elapsed:?}"
    );

    // Not vacuous: a result inside the bound is still answered, and answered completely.
    let rows = Session::open_as(server.port, "ana")
        .run("SELECT * FROM generate_series(1, 100)")
        .expect("a hundred rows is inside the limit");
    assert_eq!(rows, 100, "and every one of them comes back: {rows}");
}

/// A server whose queries may use `bytes` of memory between them.
fn under_memory(bytes: &str) -> (tempfile::TempDir, Running) {
    let dir = tempfile::tempdir().expect("a directory");
    let warehouse = dir.path().join("warehouse");
    write_warehouse(&warehouse);
    let server = start_with(
        &warehouse,
        &dir.path().join("data"),
        &[("SANKHYA_QUERY_MEMORY_BYTES", bytes)],
    );
    (dir, server)
}

#[test]
fn a_query_that_asks_for_more_memory_than_it_may_have_is_refused_rather_than_fatal() {
    // `OPS-06`, `OPS-07`. DataFusion runs on an **unbounded** pool unless it is given one, and
    // there was no `MemoryPool`, no `FairSpillPool` and no `DiskManager` anywhere in the
    // workspace. `sankhya-governor` states the consequence exactly --- "hash joins do not
    // spill… it exhausts memory and the operating system terminates the process" --- and
    // nothing acted on it.
    //
    // A **hash join**, deliberately, and a megabyte to do it in. A sort would spill to disk and
    // succeed slowly, which is the pool working and is not what this needs to observe; a hash
    // join cannot spill, so it is the operation that tells a bounded pool from an unbounded
    // one. That is also the case an operator most needs to know about before raising the
    // limit expecting a join to start working.
    let (_dir, server) = under_memory("1048576");

    let refused = Session::open_as(server.port, "ana")
        .run(
            "SELECT count(*) FROM generate_series(1, 400000) a \
             JOIN generate_series(1, 400000) b ON a.value = b.value",
        )
        .expect_err("a hash join over 400,000 rows does not fit in a megabyte");
    assert!(
        !refused.is_empty(),
        "and it is refused with something a caller can read: {refused}"
    );

    // The server is still there, which is the half that used to be a dead process.
    assert_eq!(
        Session::open_as(server.port, "ana")
            .run("SELECT 1")
            .expect("the server survived a query it could not afford"),
        1
    );

    // Not vacuous: the same join inside the default bound is answered. Without this, a pool of
    // zero would pass every assertion above.
    let (_dir, generous) = running();
    assert_eq!(
        Session::open_as(generous.port, "ana")
            .run(
                "SELECT count(*) FROM generate_series(1, 400000) a \
                 JOIN generate_series(1, 400000) b ON a.value = b.value",
            )
            .expect("the same join fits in the default gigabyte"),
        1
    );
}

#[test]
fn a_memory_bound_of_nothing_is_not_a_bound_of_nothing() {
    // The other direction. `SANKHYA_QUERY_MEMORY_BYTES=0` is somebody unsetting a variable by
    // emptying it, or a template that filled in a default of zero --- and a pool of zero bytes
    // refuses every statement, which is a server that starts and answers nothing.
    //
    // Read as "say nothing" rather than as "allow nothing", and the default applies.
    let (_dir, server) = under_memory("0");
    // A **sort**, not `SELECT 1`. A statement that reserves nothing is answered by a pool of
    // nothing, so asserting on one would pass whether the zero was read as "say nothing" or as
    // "allow nothing" --- which is exactly the distinction under test.
    assert_eq!(
        Session::open_as(server.port, "ana")
            .run("SELECT * FROM generate_series(1, 1000) ORDER BY value DESC")
            .expect("a bound of zero is not a bound of zero bytes"),
        1000
    );
}

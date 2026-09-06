//! What a read puts in the audit.
//!
//! # Why this is not in `wiring.rs`
//!
//! Because that file is at its line limit, and because this belongs beside the read path rather
//! than beside the composition root: what a statement was answered under is decided where the
//! session is built, and the audit is where that decision is written down.
//!
//! `wiring.rs` keeps [`Server::record`](crate::wiring::Server::record), which is for statements
//! about the warehouse's own bookkeeping documents --- a snapshot has no row filter over it, so
//! recording none there is an observation. Everything a *table* is read under goes through here.

use crate::wiring::{statement_shape, Server};
use sankhya_api_pg::session::QueryResult;
use sankhya_audit::{Entry, RecordedDecision};
use sankhya_authz::policy::{Action, TableRef};
use sankhya_authz::principal::Principal;
use sankhya_metrics::catalogue;

/// Record a statement, with what it was answered under.
///
/// # What this used to record
///
/// One entry, naming *the first two words of the statement* where a table belongs, asserting
/// **no row filter and no column masks**, and carrying no version, no statement text and no
/// row count. §13.5 lists four fields as not optional; none of them was ever populated, and
/// the one about restrictions was not merely empty --- it positively said none applied, on
/// statements where one did. `SEC-07`.
///
/// # One entry per table reached
///
/// Because a restriction is a property of a table and not of a statement: two tables in one
/// query can be filtered differently, and a single row cannot say so. It is also the shape
/// the question takes --- *"what did this principal see of `payroll`"* is answered by
/// entries about `payroll`, not by reading every statement anybody ran.
///
/// A statement that reached no table still writes one entry. A refusal is evidence, and a
/// statement whose planning failed is the shape an investigation looks for.
pub(crate) fn record_read(
server: &Server,
principal: &Principal,
    sql: &str,
    restrictions: &crate::execute::Restrictions,
    touched: &[TableRef],
    answer: Option<&QueryResult>,
) {
    let rows = answer.map(|result| result.rows.len() as u64);
    // The graph epoch is `None` and is not guessed. §13.5 lists it as not optional, and
    // this server's read path does not traverse a graph --- an epoch invented here would be
    // a field that is always present and never true, which is worse than one that is
    // absent and says so.
    let version = sankhya_audit::DataVersion {
        snapshot: server.newest_snapshot(),
        graph_epoch: None,
    };
    // The tables the **plan** scanned, not every table the session authorized. Recording
    // one entry per authorized table attributes a row count to tables nobody read, and a
    // wrong fact in an audit is read as a fact.
    if touched.is_empty() {
        // A statement that scanned nothing: a refusal, or a query over no table at all.
        // Still recorded --- a refusal is evidence, and an attempt to reach something
        // forbidden is the pattern an investigation looks for. Named by its shape, which is
        // all there is to name.
        append_read(server, principal, TableRef::new("", statement_shape(sql)), None, sql, rows, version);
        return;
    }
    for table in touched {
        // Matched on the bare name when the plan gives no schema, because a session
        // registers a table under **both** names and a statement may have used either. The
        // audit records the qualified one whichever was typed: an entry saying `orders` is
        // an entry somebody has to guess the schema of, and a bare name is exactly what is
        // ambiguous once a second schema grows a table of that name.
        //
        // A bare name that resolves to two tables cannot reach here: a contested bare name
        // registers nowhere, so a statement using one does not plan.
        let restriction = restrictions.iter().find(|(reference, _, _)| {
            reference.table == table.table
                && (table.schema.is_empty() || reference.schema == table.schema)
        });
        let (named, restriction) = match restriction {
            Some((reference, filter, masks)) => (
                reference.clone(),
                Some((filter.clone(), masks.clone())),
            ),
            // The plan named a table the session did not authorize, which cannot happen
            // through this path --- and is recorded as a refusal rather than as an
            // unrestricted grant if it ever does.
            None => (table.clone(), None),
        };
        append_read(server, principal, named, restriction, sql, rows, version.clone());
    }
}

/// One audit entry for a read, with everything known about it.
fn append_read(
server: &Server,
principal: &Principal,
    table: TableRef,
    restriction: Option<(Option<String>, std::collections::BTreeMap<String, String>)>,
    sql: &str,
    rows: Option<u64>,
    version: sankhya_audit::DataVersion,
) {
    // Wall clock, because an audit that cannot say *when* answers none of the questions an
    // audit is opened for. The reproducible ordering lives in the record's `sequence`, which
    // is where it always belonged.
    let at = server.now_micros();
    let decision = match restriction {
        // Nothing was authorized, so nothing was restricted --- and this is a refusal
        // rather than a grant, which is what makes the absence honest.
        None => RecordedDecision::denied(),
        Some((filter, masks)) => RecordedDecision {
            allowed: true,
            row_filter: filter,
            column_masks: masks,
        },
    };
    // The statement's **shape**, never its text.
    //
    // `select 1` and `select id`, not `SELECT id FROM example WHERE national_id =
    // '123-45-6789'`. A statement carries the values a query filtered on, and copying those
    // into a durable log makes the audit a second place the data lives --- with different
    // retention and different access control from the table it came from. There is a test
    // that says so, it predates this change, and it is right: §13.5 asks for the filter, the
    // masks, the version and the epoch, and none of those is the statement.
    //
    // What `SEC-07` complained about is that this shape was being recorded **as the table**.
    // It goes in the field for it, and the table goes in the field for the table.
    let entry = Entry::by(principal, table, Action::Read, decision, at)
        .running(statement_shape(sql))
        .from_version(version);
    let entry = match rows {
        Some(rows) => entry.returning(rows),
        None => entry,
    };
    append(server, entry);
}

/// Read the chain back from the warehouse and keep writing to it.
///
/// Called once, at startup, before anything is served. Returns what could not be read: a
/// line that is not a record is **reported**, because each record links to the one before
/// it, so a gap makes every record after it fail verification --- and somebody who is told
/// only "this chain does not verify" has a file to bisect.
///
/// A journal that cannot be opened leaves the chain in memory and says so. Refusing to
/// start would be defensible; it is not what this server promises today, and the posture is
/// printed at startup rather than assumed.
pub fn resume_audit(server: &Server) -> Vec<String> {
    // A **window**, not the whole file. A warehouse with a year of audit behind it would
    // otherwise load all of it into memory before answering anything --- which turns
    // `OPS-04`'s unbounded growth into an unbounded boot. `len` and `head` still describe the
    // whole chain; the records held are the recent ones somebody would actually look at.
    let (chain, mut complaints) = sankhya_audit::journal::read_windowed(
        &server.settings.warehouse,
        sankhya_audit::chain::WINDOW,
    );
    // Verified on the way in. A chain that does not verify is reported at startup rather
    // than at the moment somebody needs it, which is during an incident.
    if let Err(broken) = chain.verify() {
        complaints.push(format!("the stored audit chain does not verify: {broken}"));
    }
    let journal = match sankhya_audit::journal::Journal::open(&server.settings.warehouse) {
        Ok(journal) => Some(journal),
        Err(error) => {
            complaints.push(format!("the audit could not be written to disk: {error}"));
            None
        }
    };
    let mut held = server.audit.lock();
    *held = (chain, journal);
    complaints
}

/// Whether the audit is being written to disk.
///
/// Reported at startup, because "the audit is in memory only" is a posture an operator has
/// to know they have and cannot otherwise discover until a restart has already lost it.
#[must_use]
pub fn audit_is_durable(server: &Server) -> bool {
    server.audit.lock().1.is_some()
}

/// Put one entry in the chain, and on the disk.
pub(crate) fn append(server: &Server, entry: Entry) {
    {
        let mut held = server.audit.lock();
        let sequence = held.0.len();
        held.0.append(entry);
        // Written through to the file, if there is one. A chain that lives only in memory
        // is erased by a restart, and a restart is the event most likely to accompany the
        // incident the audit exists for. `SEC-07`.
        //
        // A failed write is reported and does not stop the statement. The alternative ---
        // refusing to answer when the audit cannot be written --- is defensible and is not
        // what this server promises today; what it must not do is fail silently, because an
        // audit that quietly stopped recording is worse than one that was never claimed.
        let (chain, journal) = &mut *held;
        let written = chain
            .records()
            .get(sequence)
            .cloned()
            .zip(journal.as_mut())
            .map(|(record, journal)| journal.append(&record));
        if let Some(Err(error)) = written {
            tracing::error!(%error, "the audit record could not be written to disk");
            server.metrics()
                .increment(&catalogue::AUDIT_UNWRITTEN_TOTAL, &[], 1.0);
        }
    }
    // Counted here rather than derived from the chain's length on scrape, so that the
    // number rises at the moment of the append. A gauge read from the chain would be
    // equally true and would not distinguish "the audit stopped recording" from "the
    // scrape stopped running", and only one of those is an emergency.
    server.metrics()
        .increment(&catalogue::AUDIT_RECORDS_TOTAL, &[], 1.0);
}

/// What the chain currently is: its head, its length, and whether it verifies.
///
/// The three together rather than three accessors, because they are three readings of one lock.
/// A caller that took them separately could report a head from one moment and a length from
/// another --- and those two are exactly the pair somebody compares against what was mirrored,
/// which is the only way a truncated chain is ever detected.
#[must_use]
pub(crate) fn standing(server: &Server) -> (String, usize, bool) {
    let held = server.audit.lock();
    (held.0.head().to_string(), held.0.len(), held.0.verify().is_ok())
}

/// The chain's current head, for mirroring somewhere append-only.
#[must_use]
pub(crate) fn head(server: &Server) -> String {
    standing(server).0
}

/// How many things have been audited.
#[must_use]
pub(crate) fn len(server: &Server) -> usize {
    standing(server).1
}

/// Whether the chain still verifies.
#[must_use]
pub(crate) fn intact(server: &Server) -> bool {
    standing(server).2
}

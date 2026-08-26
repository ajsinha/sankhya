//! A mutable table returns the current state, not its history.
//!
//! Before this existed, a row that had been updated came back twice — once as it was and
//! once as it is — and `SUM` added the old value to the new one. Every test that read
//! captured data used append-only fixtures, so the suite never asked a question about a
//! row that changed.

// Tests may panic — that is how a test reports a failure. The workspace denies
// `unwrap`, `expect`, `panic` and indexing because a *server* must not do those things
// on data it did not choose; a test chooses all of its data, and an assertion that
// cannot fail loudly is worse than useless.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use arrow_array::{Int64Array, RecordBatch, StringArray, UInt64Array};
use arrow_schema::{DataType, Field, Schema};
use datafusion::prelude::SessionContext;
use sankhya_plan::{plan_splice, TierRef};
use sankhya_readpath::{Capability, LoggedFile, ResolvedTable, SankhyaTable};
use sankhya_table::{write_parquet, WriterConfig};
use sankhya_types::{Lsn, LsnRange};
use std::sync::Arc;

fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("balance", DataType::Int64, false),
        Field::new("_sankhya_commit_lsn", DataType::UInt64, false),
        Field::new("_sankhya_op", DataType::Utf8, false),
    ]))
}

/// One captured change: an id, a balance, the position it happened at, and what happened.
struct Change(i64, i64, u64, &'static str);

/// Write the changes as one published file and return a raw provider over it.
fn table(dir: &std::path::Path, changes: &[Change]) -> Arc<SankhyaTable> {
    let batch = RecordBatch::try_new(
        schema(),
        vec![
            Arc::new(Int64Array::from(
                changes.iter().map(|c| c.0).collect::<Vec<_>>(),
            )),
            Arc::new(Int64Array::from(
                changes.iter().map(|c| c.1).collect::<Vec<_>>(),
            )),
            Arc::new(UInt64Array::from(
                changes.iter().map(|c| c.2).collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                changes.iter().map(|c| c.3).collect::<Vec<_>>(),
            )),
        ],
    )
    .expect("building");

    let target = changes.iter().map(|c| c.2).max().unwrap_or(1);
    let report = write_parquet(
        dir,
        "part-0000.parquet",
        &batch,
        Lsn::new(target),
        WriterConfig::default(),
    )
    .expect("writing");

    let file = LoggedFile::new(
        dir.join("part-0000.parquet")
            .to_str()
            .expect("a utf-8 path")
            .to_string(),
        report.bytes,
        changes.len() as u64,
    );
    let coverage = LsnRange::up_to(Lsn::new(target));
    let splice =
        plan_splice(&[TierRef::new("published", coverage)], Lsn::new(target)).expect("a tier");

    Arc::new(SankhyaTable::new(
        schema(),
        vec![file],
        Vec::new(),
        Lsn::new(target),
        splice,
        true,
    ))
}

async fn query(provider: Arc<dyn datafusion::catalog::TableProvider>, sql: &str) -> Vec<String> {
    let ctx = SessionContext::new();
    ctx.register_table("accounts", provider)
        .expect("registering");
    let batches = ctx
        .sql(sql)
        .await
        .expect("planning")
        .collect()
        .await
        .expect("executing");
    arrow::util::pretty::pretty_format_batches(&batches)
        .expect("formatting")
        .to_string()
        .lines()
        .skip(3)
        .filter(|l| l.starts_with('|'))
        .map(|l| {
            // The pretty printer pads columns to a common width. Comparing padded text
            // makes an assertion fail when a value's *neighbour* changes length, which
            // has nothing to do with what is being tested.
            l.trim_matches(|c| c == '|' || c == ' ')
                .split('|')
                .map(str::trim)
                .collect::<Vec<_>>()
                .join(" | ")
        })
        .collect()
}

fn resolved(raw: Arc<SankhyaTable>) -> Arc<ResolvedTable> {
    let capability = Capability::mutable(["id"]).expect("a key");
    Arc::new(ResolvedTable::new(raw, capability.key()).expect("resolving"))
}

#[tokio::test]
async fn an_updated_row_is_returned_once_with_its_new_value() {
    // The defect, directly. Unresolved this returns two rows and a sum of 350.
    let dir = tempfile::tempdir().expect("a temp dir");
    let raw = table(
        dir.path(),
        &[Change(1, 100, 10, "I"), Change(1, 250, 20, "U")],
    );

    let rows = query(
        resolved(Arc::clone(&raw)),
        "SELECT count(*), sum(balance) FROM accounts",
    )
    .await;
    assert_eq!(rows, vec!["1 | 250"]);

    // And the raw table still holds both, which is what makes time travel possible.
    let history = query(raw, "SELECT count(*) FROM accounts").await;
    assert_eq!(history, vec!["2"]);
}

#[tokio::test]
async fn a_deleted_row_is_absent_rather_than_stale() {
    // The ordering that matters: the deletion filter runs *after* the resolution. Run
    // before, it removes the tombstone and lets the previous version win -- so a deleted
    // row comes back holding the values it had before it was deleted, which looks like
    // data rather than like duplication.
    let dir = tempfile::tempdir().expect("a temp dir");
    let raw = table(
        dir.path(),
        &[
            Change(1, 100, 10, "I"),
            Change(2, 500, 11, "I"),
            Change(1, 250, 20, "U"),
            Change(1, 250, 30, "D"),
        ],
    );

    let rows = query(
        resolved(raw),
        "SELECT id, balance FROM accounts ORDER BY id",
    )
    .await;
    assert_eq!(rows, vec!["2 | 500"], "the deleted row came back");
}

#[tokio::test]
async fn a_row_deleted_and_reinserted_is_present_again() {
    // Reachable in any system where a key is reused. Treating a tombstone as permanent
    // would lose the new row entirely.
    let dir = tempfile::tempdir().expect("a temp dir");
    let raw = table(
        dir.path(),
        &[
            Change(1, 100, 10, "I"),
            Change(1, 100, 20, "D"),
            Change(1, 900, 30, "I"),
        ],
    );

    let rows = query(resolved(raw), "SELECT id, balance FROM accounts").await;
    assert_eq!(rows, vec!["1 | 900"]);
}

#[tokio::test]
async fn several_keys_resolve_independently() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let raw = table(
        dir.path(),
        &[
            Change(1, 10, 1, "I"),
            Change(2, 20, 2, "I"),
            Change(3, 30, 3, "I"),
            Change(2, 99, 4, "U"),
            Change(1, 11, 5, "U"),
            Change(3, 30, 6, "D"),
        ],
    );

    let rows = query(
        resolved(raw),
        "SELECT id, balance FROM accounts ORDER BY id",
    )
    .await;
    assert_eq!(rows, vec!["1 | 11", "2 | 99"]);
}

#[tokio::test]
async fn the_latest_version_wins_regardless_of_the_order_rows_were_written() {
    // Files are read in whatever order the scan produces, and a resolution that trusted
    // that order would return whichever version happened to be read last.
    let dir = tempfile::tempdir().expect("a temp dir");
    let raw = table(
        dir.path(),
        &[
            Change(1, 999, 30, "U"),
            Change(1, 100, 10, "I"),
            Change(1, 250, 20, "U"),
        ],
    );

    let rows = query(resolved(raw), "SELECT balance FROM accounts").await;
    assert_eq!(rows, vec!["999"]);
}

#[tokio::test]
async fn an_append_only_table_is_not_resolved_at_all() {
    // Most high-volume tables are append-only, so most queries take this path and it has
    // to cost nothing -- not a cheap check, nothing.
    let capability = Capability::AppendOnly;
    assert!(!capability.needs_resolution());
    assert!(capability.key().is_empty());

    // Two rows with the same identifier are two rows, because nothing declared them to be
    // versions of one another.
    let dir = tempfile::tempdir().expect("a temp dir");
    let raw = table(
        dir.path(),
        &[Change(1, 100, 10, "I"), Change(1, 250, 20, "I")],
    );
    let rows = query(raw, "SELECT count(*), sum(balance) FROM accounts").await;
    assert_eq!(rows, vec!["2 | 350"]);
}

#[tokio::test]
async fn a_mutable_table_with_no_key_is_refused() {
    // Defaulting to whole-row identity would turn every update into a new row, which is
    // the unresolved behaviour arrived at by a different route.
    let err = Capability::mutable(Vec::<String>::new()).expect_err("no key");
    assert!(format!("{err}").contains("every update into a new row"));
}

#[tokio::test]
async fn a_key_naming_a_column_that_does_not_exist_is_refused() {
    // Resolving on a column that is not there would silently keep every version of every
    // row -- the defect, reintroduced by the mechanism meant to fix it.
    let dir = tempfile::tempdir().expect("a temp dir");
    let raw = table(dir.path(), &[Change(1, 100, 10, "I")]);

    let err = ResolvedTable::new(raw, &["not_a_column".to_string()])
        .expect_err("an unknown key must be refused");
    assert!(format!("{err}").contains("keep every version of every row"));
}

#[tokio::test]
async fn a_table_that_did_not_come_through_capture_is_refused() {
    // Without a commit position there is no "latest", and without an operation column a
    // deletion is indistinguishable from an update.
    let dir = tempfile::tempdir().expect("a temp dir");
    let plain = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]));
    let batch = RecordBatch::try_new(
        Arc::clone(&plain),
        vec![Arc::new(Int64Array::from(vec![1i64]))],
    )
    .expect("building");
    let report = write_parquet(
        dir.path(),
        "p.parquet",
        &batch,
        Lsn::new(1),
        WriterConfig::default(),
    )
    .expect("writing");

    let coverage = LsnRange::up_to(Lsn::new(1));
    let splice = plan_splice(&[TierRef::new("published", coverage)], Lsn::new(1)).expect("a tier");
    let raw = Arc::new(SankhyaTable::new(
        plain,
        vec![LoggedFile::new(
            dir.path()
                .join("p.parquet")
                .to_str()
                .expect("utf-8")
                .to_string(),
            report.bytes,
            1,
        )],
        Vec::new(),
        Lsn::new(1),
        splice,
        true,
    ));

    let err = ResolvedTable::new(raw, &["id".to_string()])
        .expect_err("a table without capture columns must be refused");
    assert!(format!("{err}").contains("did not come through capture"));
}

#[tokio::test]
async fn scanning_a_resolved_table_directly_refuses_rather_than_serving_history() {
    // The planner inlines the resolution and never calls `scan`, so nothing above
    // exercises this path — which is exactly why it is worth a test. If a future planner
    // stopped inlining, a fallback that quietly scanned the raw table would serve every
    // version of every row again, and the only symptom would be the numbers being wrong.
    use datafusion::catalog::TableProvider;

    let dir = tempfile::tempdir().expect("a temp dir");
    let raw = table(
        dir.path(),
        &[Change(1, 100, 10, "I"), Change(1, 250, 20, "U")],
    );
    let resolved = resolved(raw);

    let ctx = SessionContext::new();
    let err = resolved
        .scan(&ctx.state(), None, &[], None)
        .await
        .expect_err("a resolved table must not serve a raw scan");

    assert!(
        format!("{err}").contains("every version of every row"),
        "the refusal must say what serving a raw scan would cost: {err}"
    );
}

#[test]
fn the_capability_comes_from_what_the_source_declared() {
    // The source states which columns identify a row -- its replica identity -- and that
    // statement arrives with the relation. Reading it is taking the declaration, not
    // inferring one, which is the distinction the architecture draws.
    let mutable = Capability::from_source(["id"]);
    assert!(mutable.needs_resolution());
    assert_eq!(mutable.key(), ["id".to_string()]);

    let composite = Capability::from_source(["tenant", "id"]);
    assert_eq!(composite.key(), ["tenant".to_string(), "id".to_string()]);
}

#[test]
fn a_relation_with_no_row_identity_is_append_only_for_reading() {
    // Not a guess that no updates will arrive. A statement that none could be applied:
    // with no key there is nothing to resolve versions against, so an update has no
    // meaning here even if the source sends one. The fix is the source's replica
    // identity, which onboarding already warns about.
    let capability = Capability::from_source(Vec::<String>::new());
    assert_eq!(capability, Capability::AppendOnly);
    assert!(!capability.needs_resolution());
}

#[tokio::test]
async fn a_captured_relation_resolves_on_the_key_the_source_gave() {
    // End to end from the relation description: the columns the source marked as
    // identifying are the ones the read path resolves on, with nothing in between
    // deciding.
    use sankhya_cdc_model::{ColumnDescriptor, RelationDescriptor, ReplicaIdentity};
    use sankhya_schema::onboard_relation;

    let relation = RelationDescriptor {
        relation_id: 1,
        namespace: "public".into(),
        name: "accounts".into(),
        replica_identity: ReplicaIdentity::Default,
        columns: vec![
            ColumnDescriptor {
                name: "id".into(),
                type_oid: 20,
                type_modifier: -1,
                is_key: true,
            },
            ColumnDescriptor {
                name: "balance".into(),
                type_oid: 20,
                type_modifier: -1,
                is_key: false,
            },
        ],
    };

    let onboarded = onboard_relation(&relation).expect("onboarding");
    let keys: Vec<&str> = onboarded
        .schema
        .key_fields()
        .iter()
        .map(|f| f.name.as_str())
        .collect();
    let capability = Capability::from_source(keys);

    assert!(capability.needs_resolution());
    assert_eq!(capability.key(), ["id".to_string()]);

    // And it actually resolves.
    let dir = tempfile::tempdir().expect("a temp dir");
    let raw = table(
        dir.path(),
        &[Change(1, 100, 10, "I"), Change(1, 250, 20, "U")],
    );
    let resolved = Arc::new(ResolvedTable::new(raw, capability.key()).expect("resolving"));
    assert_eq!(
        query(resolved, "SELECT count(*), sum(balance) FROM accounts").await,
        vec!["1 | 250"]
    );
}

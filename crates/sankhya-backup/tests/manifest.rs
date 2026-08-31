//! What a manifest refuses to be.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use sankhya_backup::manifest::{
    InconsistentBackup, KeyGeneration, Manifest, SourceBackup, TableSnapshot,
};
use sankhya_ingest::TableDigest;
use sankhya_types::Lsn;

fn digest(rows: u64, checksum: u128) -> TableDigest {
    TableDigest::from_parts(rows, checksum)
}

fn source(restores_to: u64) -> SourceBackup {
    SourceBackup {
        location: "s3://backups/2026-08-27".to_string(),
        restores_to: Lsn::new(restores_to),
        artefact_digest: "sha256:abc".to_string(),
    }
}

fn table(name: &str, version: u64, covers_to: u64) -> TableSnapshot {
    TableSnapshot::new(name, version, Lsn::new(covers_to), digest(100, 4_242))
}

fn clone_of(name: &str, origin: &str, origin_version: u64, covers_to: u64) -> TableSnapshot {
    TableSnapshot::cloned(
        name,
        1,
        Lsn::new(covers_to),
        digest(100, 4_242),
        origin,
        origin_version,
    )
}

fn keys() -> KeyGeneration {
    KeyGeneration {
        name: "warehouse".to_string(),
        version: 3,
    }
}

fn bind(tables: Vec<TableSnapshot>, restores_to: u64) -> Result<Manifest, InconsistentBackup> {
    Manifest::bind(1_000, source(restores_to), tables, keys(), 9_000)
}

// --- the rule the manifest exists to enforce ----------------------------

#[test]
fn a_table_ahead_of_the_source_is_refused_at_build_time() {
    // The failure: after restoring, the analytical tier holds rows the transactional store
    // no longer has. Capture resumes behind them and republishes that range at different
    // positions. Not detectable afterwards from either side alone — which is why this is
    // checked when the backup is *recorded*, not when it is needed.
    let refused = bind(
        vec![table("sales.orders", 7, 500), table("sales.items", 4, 900)],
        800,
    )
    .expect_err("a table covering past the source must be refused");

    let InconsistentBackup::TableAheadOfSource { tables } = &refused else {
        panic!("wrong refusal: {refused:?}");
    };
    assert_eq!(tables.len(), 1);
    assert_eq!(tables[0].0, "sales.items");
    assert!(refused.to_string().contains("republish"));
}

#[test]
fn every_table_ahead_is_named_not_just_the_first() {
    // An operator fixing them one at a time learns about the next only after another full
    // backup, which for a large warehouse is hours.
    let refused = bind(
        vec![
            table("a", 1, 900),
            table("b", 1, 100),
            table("c", 1, 950),
            table("d", 1, 1_000),
        ],
        800,
    )
    .expect_err("three tables are ahead");
    let InconsistentBackup::TableAheadOfSource { tables } = &refused else {
        panic!("wrong refusal");
    };
    assert_eq!(tables.len(), 3);
    let named: Vec<&str> = tables.iter().map(|(name, _, _)| name.as_str()).collect();
    assert_eq!(named, ["a", "c", "d"]);
}

#[test]
fn a_table_exactly_at_the_source_is_fine() {
    // The boundary. Equal is consistent: the table covers everything the source has.
    let manifest = bind(vec![table("a", 1, 800)], 800).expect("equal is consistent");
    assert_eq!(manifest.queryable_at, Lsn::new(800));
    assert_eq!(manifest.recapture_span(), 0);
}

#[test]
fn a_backup_of_no_tables_is_refused() {
    // It would restore a source with no analytical tier, and nothing would say so.
    assert_eq!(bind(Vec::new(), 800), Err(InconsistentBackup::NoTables));
}

// --- the two positions --------------------------------------------------

#[test]
fn the_queryable_position_is_the_slowest_table_not_the_source() {
    // The defect this file exists to prevent is recording one number and calling it "the
    // consistent point". A cross-table query can only be answered where *both* tables
    // reach, so the consistent point is the minimum — and it is derived rather than
    // supplied, because a caller who could set it could set it wrong.
    let manifest = bind(
        vec![
            table("fast", 9, 790),
            table("slow", 2, 300),
            table("middling", 5, 600),
        ],
        800,
    )
    .expect("consistent");

    assert_eq!(manifest.queryable_at, Lsn::new(300), "the slowest table decides");
    assert_eq!(manifest.source_restores_to, Lsn::new(800));
    assert_eq!(
        manifest.recapture_span(),
        500,
        "how much re-capture a restore implies before a cross-table query reaches the source"
    );
}

#[test]
fn tables_are_recorded_in_name_order_whatever_order_they_arrive_in() {
    // So two manifests of the same state serialise identically and a diff between them
    // shows what changed rather than what was iterated first.
    let one = bind(
        vec![table("c", 1, 10), table("a", 1, 10), table("b", 1, 10)],
        800,
    )
    .expect("consistent");
    let two = bind(
        vec![table("a", 1, 10), table("b", 1, 10), table("c", 1, 10)],
        800,
    )
    .expect("consistent");

    let names: Vec<&str> = one.tables.iter().map(|t| t.table.as_str()).collect();
    assert_eq!(names, ["a", "b", "c"]);
    assert_eq!(
        one.tables, two.tables,
        "the same state produces the same record"
    );
}

// --- what it records ----------------------------------------------------

#[test]
fn a_manifest_round_trips_through_json_without_losing_the_checksum() {
    // A u128 checksum through a JSON number becomes a double and quietly loses its low
    // bits, which is precisely the failure a digest exists to catch — so it is written as a
    // string. This test is the reason that decision is visible.
    let huge = u128::MAX - 12_345;
    let manifest = Manifest::bind(
        1_000,
        source(800),
        vec![TableSnapshot::new(
            "sales.orders",
            7,
            Lsn::new(700),
            digest(9_007_199_254_740_993, huge),
        )],
        keys(),
        9_000,
    )
    .expect("consistent");

    let back = Manifest::from_json(&manifest.to_json().expect("serialises")).expect("parses");
    assert_eq!(back, manifest);
    let read = back.tables[0].digest().expect("the digest survives");
    assert_eq!(read.checksum(), huge);
    assert_eq!(read.rows(), 9_007_199_254_740_993);
}

#[test]
fn a_checksum_that_will_not_parse_is_none_rather_than_zero() {
    // A digest that fails to parse and reads as zero compares unequal to everything, which
    // looks like corrupted *data* rather than a corrupted *manifest* — and sends an
    // operator to investigate the wrong artefact entirely.
    let mut snapshot = table("a", 1, 10);
    snapshot.checksum = "not a number".to_string();
    assert_eq!(snapshot.digest(), None);
}

#[test]
fn the_manifest_names_what_it_protects_by_version_not_by_file() {
    // A manifest that embeds a file list goes stale the moment compaction rewrites one, and
    // a table can hold thousands of files. The reachable set of a retained version is
    // something the log already answers.
    let manifest = bind(vec![table("a", 3, 10), table("b", 9, 20)], 800).expect("consistent");
    assert_eq!(manifest.protected_snapshots(), [("a", 3), ("b", 9)]);
}

#[test]
fn the_key_generation_is_recorded_because_data_without_its_key_is_noise() {
    let manifest = bind(vec![table("a", 1, 10)], 800).expect("consistent");
    assert_eq!(manifest.keys.name, "warehouse");
    assert_eq!(manifest.keys.version, 3);
}

#[test]
fn two_backups_taken_at_the_same_instant_have_different_identities() {
    // A name is something a person can reuse and a timestamp is something two operators can
    // share.
    let one = bind(vec![table("a", 1, 10)], 800).expect("consistent");
    let two = bind(vec![table("a", 1, 10)], 800).expect("consistent");
    assert_ne!(one.id, two.id);
    assert!(one.id.to_string().starts_with("backup:"));
}

// --- a clone in a backup, and what it needs beside it -------------------

#[test]
fn a_backup_holding_a_clone_and_its_origin_is_bound() {
    let manifest = bind(
        vec![table("entries", 52, 10), clone_of("staging", "entries", 40, 10)],
        20,
    )
    .expect("both are there");

    let staging = manifest
        .tables
        .iter()
        .find(|table| table.table == "staging")
        .expect("the clone is recorded");
    let cloned = staging.cloned_from.as_ref().expect("as a clone");
    assert_eq!(cloned.origin, "entries");
    assert_eq!(cloned.version, 40);
}

#[test]
fn a_backup_holding_a_clone_without_its_origin_is_refused_at_build_time() {
    // The failure: a clone's log names none of its origin's files — it reads the origin's live
    // set at a version and splices its own log over it — so restoring one without its origin
    // produces a table that is present, readable and **empty**. Nothing about it looks broken.
    //
    // Refused where the position check is refused, and for the same reason: a backup that
    // cannot be restored is worse than no backup, because it is counted as one.
    let refusal = bind(vec![clone_of("staging", "entries", 40, 10)], 20)
        .expect_err("its origin is not in the backup");

    assert_eq!(
        refusal,
        InconsistentBackup::CloneWithoutItsOrigin {
            clones: vec![("staging".to_string(), "entries".to_string())]
        }
    );
    let said = refusal.to_string();
    assert!(said.contains("present, readable and empty"), "{said}");
    assert!(said.contains("materialise"), "{said}");
}

#[test]
fn every_orphaned_clone_is_named_rather_than_the_first() {
    // An operator who takes another full backup to discover the second omission has been made
    // to pay twice for one mistake.
    let refusal = bind(
        vec![
            clone_of("staging", "entries", 40, 10),
            clone_of("scratch", "ledgerless", 3, 10),
            table("orders", 7, 10),
        ],
        20,
    )
    .expect_err("two clones are orphaned");

    let InconsistentBackup::CloneWithoutItsOrigin { clones } = refusal else {
        panic!("an orphaned clone")
    };
    assert_eq!(clones.len(), 2);
    assert!(clones.contains(&("staging".to_string(), "entries".to_string())));
    assert!(clones.contains(&("scratch".to_string(), "ledgerless".to_string())));
}

#[test]
fn a_chain_of_clones_is_bound_when_the_whole_chain_is_present() {
    // `scratch` reads `staging`, which reads `entries`. Each link is checked against the
    // backup's own contents, so the chain holds without the check knowing it is a chain.
    assert!(bind(
        vec![
            table("entries", 52, 10),
            clone_of("staging", "entries", 40, 10),
            clone_of("scratch", "staging", 3, 10),
        ],
        20,
    )
    .is_ok());
}

#[test]
fn a_broken_link_part_way_up_a_chain_is_refused() {
    // `scratch` reads `staging` and `staging` is absent. That the *root* is present does not
    // help: `scratch` splices `staging`'s live set, not `entries`'s.
    let refusal = bind(
        vec![table("entries", 52, 10), clone_of("scratch", "staging", 3, 10)],
        20,
    )
    .expect_err("the middle of the chain is missing");
    assert!(matches!(refusal, InconsistentBackup::CloneWithoutItsOrigin { .. }));
}

#[test]
fn an_ordinary_table_records_no_lineage() {
    let manifest = bind(vec![table("entries", 52, 10)], 20).expect("bound");
    assert_eq!(manifest.tables[0].cloned_from, None);
}

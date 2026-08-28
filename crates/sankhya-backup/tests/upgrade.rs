//! Reading what an earlier release wrote, and refusing what a later one will.
//!
//! # The corpus is the old binary
//!
//! Testing an upgrade properly means running release *n−1* against release *n*'s data, and
//! release *n* against release *n−1*'s. That needs two binaries, which a build does not have.
//!
//! It does not need two binaries. **It needs one binary's output.** A fixture written by an
//! earlier release, checked into the repository, is that release's behaviour preserved — and
//! unlike the binary it never stops building, never needs a toolchain that has been removed,
//! and is legible in a diff. Every future build reads it.
//!
//! The fixtures below are written by hand rather than generated, deliberately. A generated
//! fixture regenerates when the format changes, which makes it agree with the current code
//! by construction and proves nothing at all.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_backup::manifest::{Manifest, UnreadableManifest};

/// A manifest as the release that predates format stamping wrote it: no `format` field.
const FORMAT_ZERO_ERA: &str = r#"{
  "id": "01a04442-936a-73a1-bfd1-964c8cd66330",
  "taken_at": 1787851600000000,
  "source_restores_to": 200,
  "queryable_at": 100,
  "source": {
    "location": "file:///backups/pg",
    "restores_to": 200,
    "artefact_digest": "sha256:abc"
  },
  "tables": [
    {
      "table": "sales.orders",
      "version": 1,
      "covers_to": 100,
      "rows": 1000,
      "checksum": "123456789"
    }
  ],
  "keys": { "name": "warehouse", "version": 1 },
  "protect_until": 1787938000000000
}"#;

/// The same manifest, stamped format 1 as this build writes it.
const FORMAT_ONE: &str = r#"{
  "format": 1,
  "id": "01a04442-936a-73a1-bfd1-964c8cd66330",
  "taken_at": 1787851600000000,
  "source_restores_to": 200,
  "queryable_at": 100,
  "source": {
    "location": "file:///backups/pg",
    "restores_to": 200,
    "artefact_digest": "sha256:abc"
  },
  "tables": [
    {
      "table": "sales.orders",
      "version": 1,
      "covers_to": 100,
      "rows": 1000,
      "checksum": "123456789"
    }
  ],
  "keys": { "name": "warehouse", "version": 1 },
  "protect_until": 1787938000000000
}"#;

/// A manifest from a release that has not been written yet.
const FROM_THE_FUTURE: &str = r#"{
  "format": 99,
  "id": "01a04442-936a-73a1-bfd1-964c8cd66330",
  "taken_at": 1787851600000000,
  "consistency": { "restores_to": 200, "queryable_at": 100 },
  "tables": [],
  "keys": { "name": "warehouse", "version": 1, "wrapped_by": "kms://something" },
  "protect_until": 1787938000000000
}"#;

#[test]
fn a_manifest_written_before_formats_were_stamped_still_reads() {
    // The upgrade direction everybody tests, and it still has to be tested: an absent
    // `format` means the original, not zero and not a failure.
    let manifest = Manifest::from_json(FORMAT_ZERO_ERA).expect("an unstamped manifest reads");
    assert_eq!(manifest.format, 1);
    assert_eq!(manifest.tables.len(), 1);
    assert_eq!(manifest.tables[0].table, "sales.orders");
    assert_eq!(manifest.queryable_at.get(), 100);
    assert_eq!(
        manifest.tables[0].digest().expect("a digest").rows(),
        1000,
        "the data survived the version boundary, not just the parse"
    );
}

#[test]
fn the_current_format_reads_identically_to_the_unstamped_one() {
    // Stamping the format must not have changed what a manifest *means*. If these two
    // disagree, the version field was not the only thing that moved.
    let old = Manifest::from_json(FORMAT_ZERO_ERA).expect("reads");
    let new = Manifest::from_json(FORMAT_ONE).expect("reads");
    assert_eq!(old, new);
}

#[test]
fn a_manifest_from_a_newer_release_is_refused_and_says_to_upgrade() {
    // The direction that decides whether a rollback is survivable. Note the fixture also
    // restructures fields — `consistency` instead of two flat keys — which is what a real
    // format bump looks like and what would otherwise surface as a confusing parse error
    // about a missing field.
    let refused = Manifest::from_json(FROM_THE_FUTURE).expect_err("format 99 is refused");
    let UnreadableManifest::FromTheFuture(why) = &refused else {
        panic!("a newer format must not be reported as damage: {refused:?}");
    };
    assert!(why.contains("format 99"), "{why}");
    assert!(why.contains("Upgrade"), "{why}");
    assert!(
        !why.contains("missing field"),
        "the version is read before the rest, so a restructured file never reaches the \
         parser's complaint: {why}"
    );
}

#[test]
fn damage_is_reported_as_damage_and_not_as_a_version_problem() {
    // The other half of the distinction. These send an operator to different places, and
    // getting it backwards wastes the time that matters most.
    let refused = Manifest::from_json("{ this is not json").expect_err("not a manifest");
    assert!(
        matches!(refused, UnreadableManifest::Malformed(_)),
        "{refused:?}"
    );
    assert!(
        refused.to_string().contains("damage or the wrong file"),
        "{refused}"
    );
}

#[test]
fn a_manifest_this_build_writes_can_be_read_back_by_this_build() {
    // Trivial, and it is the thing that breaks when a field is added without a default.
    let manifest = Manifest::from_json(FORMAT_ONE).expect("reads");
    let round_tripped =
        Manifest::from_json(&manifest.to_json().expect("serialises")).expect("reads back");
    assert_eq!(round_tripped, manifest);
}

#[test]
fn the_version_is_the_first_thing_in_the_file() {
    // So `head -1` answers "what wrote this", and so the field is read before any other
    // field's shape matters. A version buried at the end of a large JSON object is a version
    // you only learn after successfully parsing everything you were trying to avoid parsing.
    let manifest = Manifest::from_json(FORMAT_ONE).expect("reads");
    let json = manifest.to_json().expect("serialises");
    let first = json
        .lines()
        .nth(1)
        .expect("a pretty-printed object has a first field");
    assert!(first.trim().starts_with("\"format\""), "{first}");
}

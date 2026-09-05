//! A cube that survives the process that declared it.
//!
//! `STATUS.md` recorded the gap plainly: *"Cube definitions are not persisted or loaded from
//! a catalogue; a cube is registered against a session by the embedding application."* So a
//! cube lasted exactly as long as a process, and a server could not serve one.
//!
//! The cause was one layer down and is worth keeping in view: [`Measure`] held `&'static
//! str`, which made a measure a compile-time construct. A definition could name only measures
//! a Rust source file had already spelled out, so no persistence code could have worked --- a
//! definition read from disk had nowhere to put its own measure's name.
//!
//! These tests are the ones whose absence let the gap stand. They save a definition, throw it
//! away, read it back, and require the result to be the same cube.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use sankhya_cube::catalogue::{self, Stored};
use sankhya_cube::model::{Definition, Dimension, Level};
use sankhya_cube_algo::hierarchy::Hierarchy;
use sankhya_cube_algo::measure::{Along, Measure, Rule};

/// A cube with a semi-additive measure, a declared roll-up and two dimensions.
///
/// Deliberately not the simplest cube. A round trip that only carries `Sum` proves nothing
/// about the rule that matters: `Last` along time is the one whose loss turns a closing
/// balance into a twelve-month total, and it looks entirely plausible.
fn sales() -> Definition {
    let mut rollups = Hierarchy::new();
    rollups.rolls_up("north", "uk");
    rollups.rolls_up("south", "uk");
    // A shared member: reachable by two paths, and it must survive as two edges.
    rollups.rolls_up("south", "coastal");

    Definition::new(
        "sales",
        "sales.orders",
        vec![
            Dimension {
                name: "region".to_string(),
                table: "sales.regions".to_string(),
                joins_on: "region".to_string(),
                levels: vec![Level::new("country", "country"), Level::new("area", "area")],
                rollups: Some(rollups),
                parent_child: None,
            },
            Dimension {
                name: "time".to_string(),
                table: "sales.calendar".to_string(),
                joins_on: "event_date".to_string(),
                levels: vec![Level::new("month", "month"), Level::new("day", "day")],
                rollups: None,
                parent_child: Some(("child".to_string(), "parent".to_string())),
            },
        ],
        vec![
            Measure::new(
                "amount",
                vec![
                    Along::new("region", Rule::Sum),
                    Along::new("time", Rule::Sum),
                ],
            ),
            Measure::new(
                "closing_balance",
                vec![
                    Along::new("region", Rule::Sum),
                    Along::new("time", Rule::Last),
                ],
            ),
        ],
    )
}

#[test]
fn a_definition_saved_is_the_definition_loaded() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let defined = sales();
    catalogue::save(dir.path(), &defined).expect("storing the cube");

    let loaded = catalogue::load(dir.path(), "sales").expect("loading the cube");
    assert_eq!(
        loaded, defined,
        "a cube read back must be the cube written, or the catalogue is a lossy copy of \
         something the server will then serve"
    );
}

#[test]
fn a_semi_additive_rule_survives_the_round_trip() {
    // The failure this guards is quiet. A `Last` that comes back as `Sum` turns a closing
    // balance into the sum of twelve month-end balances, which is a number of the right
    // magnitude, the right sign, and entirely wrong.
    let dir = tempfile::tempdir().expect("a temporary directory");
    catalogue::save(dir.path(), &sales()).expect("storing");
    let loaded = catalogue::load(dir.path(), "sales").expect("loading");

    let balance = loaded
        .measures
        .iter()
        .find(|measure| measure.name == "closing_balance")
        .expect("the measure survived");
    assert_eq!(balance.rule("time"), Some(Rule::Last));
    assert_eq!(balance.rule("region"), Some(Rule::Sum));
}

#[test]
fn a_shared_member_keeps_both_of_its_parents() {
    // `south` rolls up into both `uk` and `coastal`. Storing a hierarchy as edges and losing
    // one of them turns a shared member into an ordinary one, and the total it contributes
    // to silently changes.
    let dir = tempfile::tempdir().expect("a temporary directory");
    catalogue::save(dir.path(), &sales()).expect("storing");
    let loaded = catalogue::load(dir.path(), "sales").expect("loading");

    let region = loaded
        .dimensions
        .iter()
        .find(|dimension| dimension.name == "region")
        .expect("the dimension survived");
    let hierarchy = region.rollups.as_ref().expect("the roll-up survived");
    assert_eq!(
        hierarchy.children_of("uk").len(),
        2,
        "uk consolidates north and south"
    );
    assert_eq!(
        hierarchy.children_of("coastal").len(),
        1,
        "south is shared, and its second parent must survive too"
    );
}

#[test]
fn a_rule_this_version_does_not_know_is_refused_rather_than_defaulted() {
    // The whole additivity model exists to stop a measure acquiring an implicit `Sum`. A
    // loader that shrugged at an unknown rule name and defaulted would reintroduce exactly
    // that, one file at a time, in the place nobody looks.
    let dir = tempfile::tempdir().expect("a temporary directory");
    catalogue::save(dir.path(), &sales()).expect("storing");

    let at = catalogue::path_of(dir.path(), "sales").expect("an ordinary name");
    let text = std::fs::read_to_string(&at).expect("the stored file");
    std::fs::write(&at, text.replace("\"last\"", "\"median\"")).expect("rewriting");

    let error = catalogue::load(dir.path(), "sales").expect_err("an unknown rule is refused");
    let message = format!("{error}");
    assert!(
        message.contains("median"),
        "the refusal names the rule it did not understand: {message}"
    );
}

#[test]
fn a_file_that_is_not_a_definition_is_reported_rather_than_skipped() {
    // A catalogue that returns what it could parse and says nothing about the rest brings a
    // server up looking healthy with a cube missing, and the reason sits in a file nobody
    // thought to open.
    let dir = tempfile::tempdir().expect("a temporary directory");
    catalogue::save(dir.path(), &sales()).expect("storing");
    std::fs::write(
        catalogue::path_of(dir.path(), "broken").expect("an ordinary name"),
        "{ this is not a cube",
    )
    .expect("writing rubbish");

    let error = catalogue::load_all(dir.path()).expect_err("the bad file is reported");
    assert!(
        format!("{error}").contains("broken"),
        "the error names the file: {error}"
    );
}

#[test]
fn a_warehouse_with_no_cubes_is_not_an_error() {
    // The ordinary case. A server starting against a warehouse that has never had a cube
    // must come up, not fail to.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let found = catalogue::load_all(dir.path()).expect("an empty catalogue is fine");
    assert!(found.is_empty());
}

#[test]
fn every_cube_in_the_catalogue_is_loaded() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    catalogue::save(dir.path(), &sales()).expect("storing sales");
    let mut other = sales();
    other.name = "returns".to_string();
    catalogue::save(dir.path(), &other).expect("storing returns");

    let names: Vec<String> = catalogue::load_all(dir.path())
        .expect("loading")
        .into_iter()
        .map(|definition| definition.name)
        .collect();
    assert_eq!(
        names,
        vec!["returns".to_string(), "sales".to_string()],
        "sorted, so a server's cubes come up in the same order on every start"
    );
}

#[test]
fn the_stored_form_is_stable_across_saves() {
    // A definition that serialises differently each time produces a diff per write, and a
    // fingerprint nobody can compare between two deployments.
    let once = serde_json::to_string(&Stored::of(&sales())).expect("serialising");
    let twice = serde_json::to_string(&Stored::of(&sales())).expect("serialising");
    assert_eq!(once, twice);
}

#[test]
fn the_catalogue_is_not_mistaken_for_a_table() {
    // Under `_`, which table discovery and the orphan sweep already skip. A cube definition
    // sitting where a table is looked for is a table that fails to open, reported as a
    // warehouse problem.
    let dir = tempfile::tempdir().expect("a temporary directory");
    catalogue::save(dir.path(), &sales()).expect("storing");
    let at = catalogue::path_of(dir.path(), "sales").expect("an ordinary name");
    let relative = at.strip_prefix(dir.path()).expect("under the warehouse");
    assert!(
        relative
            .components()
            .next()
            .is_some_and(|first| first.as_os_str().to_string_lossy().starts_with('_')),
        "the catalogue lives under an underscore directory: {}",
        relative.display()
    );
}

// --- the three lifetimes, persisted ------------------------------------------

#[test]
fn a_definition_is_declared_unless_it_says_otherwise() {
    // Persisting a definition is cheap; materialising is storage and work. A cube must not
    // acquire either by being written down — see ADR-0009.
    let definition = sales();
    assert_eq!(definition.target_lag, None);
    assert_eq!(
        definition.lifetime(),
        sankhya_cube::model::Lifetime::Declared
    );
}

#[test]
fn a_staleness_target_survives_the_round_trip() {
    // The one field that distinguishes the two persisted lifetimes. Losing it demotes a
    // Maintained cube to Declared on the next restart — which is not an error anywhere, just
    // a dashboard that quietly stops being fast.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let maintained = sales().maintained_within(5);
    assert_eq!(
        maintained.lifetime(),
        sankhya_cube::model::Lifetime::Maintained
    );
    catalogue::save(dir.path(), &maintained).expect("storing");

    let loaded = catalogue::load(dir.path(), "sales").expect("loading");
    assert_eq!(loaded.target_lag, Some(5));
    assert_eq!(loaded, maintained, "and nothing else moved");
}

#[test]
fn a_target_of_zero_is_not_the_absence_of_one() {
    // Zero admits a cuboid at the current version. `None` admits none at all. Serialising
    // one as the other turns the strictest maintained cube into a cube that materialises
    // nothing, or the reverse.
    let dir = tempfile::tempdir().expect("a temporary directory");
    catalogue::save(dir.path(), &sales().maintained_within(0)).expect("storing");
    let loaded = catalogue::load(dir.path(), "sales").expect("loading");

    assert_eq!(loaded.target_lag, Some(0));
    assert_ne!(loaded.target_lag, None);
    assert_eq!(loaded.lifetime(), sankhya_cube::model::Lifetime::Maintained);
}

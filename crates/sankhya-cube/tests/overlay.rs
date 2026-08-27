//! What-if figures, and keeping them distinguishable from the truth.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use sankhya_cube::cells::Cells;
use sankhya_cube::overlay::{Adjustment, Allocation, Applied, NotApplicable, Overlay};
use sankhya_cube_algo::measure::Rule;

const DEFINITION: u64 = 0x0bad_1dea_0bad_1dea;

fn address(members: &[&str]) -> Vec<String> {
    members.iter().map(|m| (*m).to_string()).collect()
}

fn cube() -> Cells {
    let mut cells = Cells::over(vec!["region".to_string(), "branch".to_string()]);
    for (region, branch, value) in [
        ("north", "a", 30.0),
        ("north", "b", 70.0),
        ("south", "c", 50.0),
    ] {
        cells.add(address(&[region, branch]), value).expect("well-formed");
    }
    cells
}

/// The same facts rolled up to region — the grain a planner edits at.
fn by_region() -> Cells {
    let mut cells = Cells::over(vec!["region".to_string()]);
    cells.add(address(&["north"]), 100.0).expect("well-formed");
    cells.add(address(&["south"]), 50.0).expect("well-formed");
    cells
}

// --- a query states which overlay it used -------------------------------

#[test]
fn an_overlaid_figure_names_the_scenario_it_came_from() {
    // A flag does not answer the question a reader has when three scenarios are open.
    let mut overlay = Overlay::named("budget-2027", DEFINITION);
    overlay.record(vec!["region".to_string()], address(&["north"]), Adjustment::Set(120.0));

    let applied = overlay
        .apply(&by_region(), DEFINITION, Allocation::Refuse)
        .expect("same grain");

    assert!(applied.is_what_if());
    assert_eq!(applied.overlay(), Some("budget-2027"));
    assert_eq!(applied.published_only(), Err("budget-2027"));
    assert_eq!(applied.regardless().get(&address(&["north"]), Rule::Sum), Some(120.0));
}

#[test]
fn published_data_says_it_is_published() {
    let published = Applied::published(by_region());
    assert!(!published.is_what_if());
    assert_eq!(published.overlay(), None);
    assert!(published.published_only().is_ok());
}

#[test]
fn an_overlay_never_touches_the_cube_it_was_applied_to() {
    // FR-CUBE-19: the files underneath are the same files.
    let original = by_region();
    let mut overlay = Overlay::named("stress", DEFINITION);
    overlay.record(vec!["region".to_string()], address(&["north"]), Adjustment::Set(0.0));
    let _ = overlay.apply(&original, DEFINITION, Allocation::Refuse).expect("applies");

    assert_eq!(original.get(&address(&["north"]), Rule::Sum), Some(100.0));
}

// --- bound to the definition it was written against ---------------------

#[test]
fn an_overlay_refuses_a_definition_it_was_not_written_against() {
    // A figure written when `revenue` summed across time means something else once it is a
    // closing balance. The same reasoning as the backup manifest's bind.
    let mut overlay = Overlay::named("budget-2027", DEFINITION);
    overlay.record(vec!["region".to_string()], address(&["north"]), Adjustment::Set(120.0));

    let refused = overlay
        .apply(&by_region(), DEFINITION + 1, Allocation::Refuse)
        .expect_err("applied to a different model");
    assert!(matches!(refused, NotApplicable::DifferentDefinition { .. }));
    assert!(refused.to_string().contains("budget-2027"));
}

// --- the grain it was written at ----------------------------------------

#[test]
fn drilling_beneath_an_edited_total_is_refused_rather_than_contradicted() {
    // The part that gets built wrong. The overlay holds a figure for `north`; the cube below
    // holds `north/a` and `north/b`, which the overlay does not contain. Showing the
    // unadjusted branches beneath an adjusted region makes a drill-down contradict the row
    // above it, and nothing on the screen says so.
    let mut overlay = Overlay::named("budget-2027", DEFINITION);
    overlay.record(vec!["region".to_string()], address(&["north"]), Adjustment::Set(120.0));

    let refused = overlay
        .apply(&cube(), DEFINITION, Allocation::Refuse)
        .expect_err("served a grain the overlay does not contain");

    let NotApplicable::FinerThanWritten { written_at, asked_at, .. } = &refused else {
        panic!("wrong refusal: {refused:?}");
    };
    assert_eq!(written_at, &["region".to_string()]);
    assert_eq!(asked_at, &["region".to_string(), "branch".to_string()]);
    assert!(refused.to_string().contains("State an allocation"), "{refused}");
}

#[test]
fn an_allocation_spreads_the_edit_in_proportion_to_what_is_there() {
    // The other honest answer: say how to spread it, and the drill-down reconciles.
    let mut overlay = Overlay::named("budget-2027", DEFINITION);
    overlay.record(vec!["region".to_string()], address(&["north"]), Adjustment::Set(120.0));

    let applied = overlay
        .apply(&cube(), DEFINITION, Allocation::ProRata)
        .expect("allocation stated");
    let cells = applied.regardless();

    // 30:70 of 120.
    assert_eq!(cells.get(&address(&["north", "a"]), Rule::Sum), Some(36.0));
    assert_eq!(cells.get(&address(&["north", "b"]), Rule::Sum), Some(84.0));
    // And the region it did not touch is untouched.
    assert_eq!(cells.get(&address(&["south", "c"]), Rule::Sum), Some(50.0));
}

#[test]
fn an_allocation_with_nothing_to_be_proportional_to_is_refused() {
    // Dividing equally is a different assumption wearing this one's name, and it would not
    // be visible in the result.
    let mut empty = Cells::over(vec!["region".to_string(), "branch".to_string()]);
    empty.add(address(&["north", "a"]), 0.0).expect("well-formed");

    let mut overlay = Overlay::named("budget-2027", DEFINITION);
    overlay.record(vec!["region".to_string()], address(&["north"]), Adjustment::Set(120.0));

    let refused = overlay
        .apply(&empty, DEFINITION, Allocation::ProRata)
        .expect_err("divided by zero, or equally");
    assert!(matches!(refused, NotApplicable::NothingToAllocateAcross { .. }));
}

#[test]
fn an_entry_finer_than_the_cube_is_left_alone_rather_than_double_counted() {
    // A leaf figure added into a total it is not part of would double-count.
    let mut overlay = Overlay::named("branch-plan", DEFINITION);
    overlay.record(
        vec!["region".to_string(), "branch".to_string()],
        address(&["north", "a"]),
        Adjustment::Set(999.0),
    );

    let applied = overlay
        .apply(&by_region(), DEFINITION, Allocation::Refuse)
        .expect("coarser is not finer");
    assert_eq!(applied.regardless().get(&address(&["north"]), Rule::Sum), Some(100.0));
}

// --- set against delta --------------------------------------------------

#[test]
fn a_delta_against_an_absent_cell_is_still_the_delta() {
    // The scenario says "five more than whatever happens". Collapsing that to a `Set` loses
    // the half the planner meant, and treating absence as zero is the one place it reads
    // correctly — because the planner supplied the other half.
    let mut overlay = Overlay::named("growth", DEFINITION);
    overlay.record(vec!["region".to_string()], address(&["east"]), Adjustment::Delta(5.0));

    let applied = overlay
        .apply(&by_region(), DEFINITION, Allocation::Refuse)
        .expect("same grain");
    assert_eq!(applied.regardless().get(&address(&["east"]), Rule::Sum), Some(5.0));
}

#[test]
fn a_delta_against_a_present_cell_adds_to_it() {
    let mut overlay = Overlay::named("growth", DEFINITION);
    overlay.record(vec!["region".to_string()], address(&["north"]), Adjustment::Delta(5.0));

    let applied = overlay
        .apply(&by_region(), DEFINITION, Allocation::Refuse)
        .expect("same grain");
    assert_eq!(applied.regardless().get(&address(&["north"]), Rule::Sum), Some(105.0));
}

#[test]
fn an_empty_overlay_still_marks_its_result_as_a_what_if() {
    // The scenario was selected; that it happens to hold no entries yet does not make the
    // answer published data, and a reader comparing two runs needs to know which model
    // produced each.
    let overlay = Overlay::named("empty-scenario", DEFINITION);
    assert!(overlay.is_empty());
    let applied = overlay
        .apply(&by_region(), DEFINITION, Allocation::Refuse)
        .expect("nothing to apply");
    assert_eq!(applied.overlay(), Some("empty-scenario"));
}

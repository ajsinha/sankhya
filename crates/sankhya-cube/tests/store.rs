//! A materialised cuboid that gives back exactly what was put in it.
//!
//! Exit criterion 3a is the reason this is fussy: *every query returns bit-identical results
//! with materialisation on and off*, compared by bits and not within a tolerance. A cache
//! that changes the answer is not a cache, it is a second source of truth — and the change is
//! invisible, because a cube rolls up in stages, every stage rounds, and
//! `round(round(a+b) + round(c+d))` is not `round(a+b+c+d)`.
//!
//! Fixing the order of summation makes one reduction reproducible. It does nothing about
//! **associativity**, and a materialised cuboid is exactly a re-association of the same
//! addition. So the stored value is the unrounded Shewchuk expansion, and these tests exist
//! to prove the round trip preserves it — including on values chosen so that rounding early
//! gives a different answer.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::float_cmp)]

use sankhya_cube::cells::Cells;
use sankhya_cube::store::{self, NotCells};
use sankhya_cube_algo::measure::Rule;

fn over(dimensions: &[&str]) -> Cells {
    Cells::over(dimensions.iter().map(|d| (*d).to_string()).collect())
}

fn address(members: &[&str]) -> Vec<String> {
    members.iter().map(|m| (*m).to_string()).collect()
}

/// Values whose sum depends on when you round.
///
/// `1e16` swamps `1.0` in a double, so `(1e16 + 1.0) - 1e16` is zero while the exact sum is
/// one. Any storage that rounds before the final addition loses the one.
const CATASTROPHIC: [f64; 4] = [1e16, 1.0, -1e16, 1.0];

#[test]
fn a_cell_survives_the_round_trip_exactly() {
    let mut cells = over(&["region"]);
    for value in CATASTROPHIC {
        cells.add(address(&["north"]), value).expect("well-formed");
    }
    let expected = cells.get(&address(&["north"]), Rule::Sum).expect("a total");
    assert_eq!(expected, 2.0, "the exact sum is two, whatever the order");

    let batch = store::to_batch(&cells, Rule::Sum).expect("storable");
    let read = store::from_batch(&batch, cells.dimensions(), Rule::Sum).expect("readable");

    let got = read.get(&address(&["north"]), Rule::Sum).expect("a total");
    assert_eq!(
        got.to_bits(),
        expected.to_bits(),
        "bit-identical, not merely close: {got} against {expected}. A materialised answer \
         that differs in the last place is how two reports come to disagree by a penny with \
         nobody able to point at a defect"
    );
}

#[test]
fn a_stored_partial_can_be_rolled_up_further_without_drifting() {
    // The property the expansion actually buys, and the one a single-cell round trip does
    // **not** test: rounding once at the end is fine, so a cell read straight back is
    // identical either way. The loss appears when a *stored partial* is added to another
    // one — which is exactly what answering a query from a materialised ancestor does.
    //
    // Cell A holds 1e16 + 1, whose double is 1e16: the one is below the last place. Cell B
    // holds -1e16. Rolled up, the exact answer is 1 and the round-early answer is 0.
    let mut cells = over(&["region"]);
    for value in [1e16, 1.0] {
        cells.add(address(&["north"]), value).expect("well-formed");
    }
    cells.add(address(&["south"]), -1e16).expect("well-formed");

    let batch = store::to_batch(&cells, Rule::Sum).expect("storable");
    let read = store::from_batch(&batch, cells.dimensions(), Rule::Sum).expect("readable");

    // Roll the two stored cells up, the way an ancestor answers a coarser query.
    let mut rolled = sankhya_math::Exact::zero();
    for address in read.addresses() {
        let contributions = read.contributions(address).expect("a cell");
        for component in contributions.exact_sum().components() {
            rolled.add(*component);
        }
    }

    assert_eq!(
        rolled.to_f64().to_bits(),
        1.0_f64.to_bits(),
        "rolling two stored partials up gave {}, and the base answer is 1. A partial stored \
         rounded loses what is below its last place, and the loss only appears when \
         something adds to it --- which is what a materialised ancestor is for",
        rolled.to_f64()
    );
}

#[test]
fn storing_the_rounded_total_would_have_lost_it() {
    // The test that shows the previous one is not vacuous. If the stored form were the
    // rounded total of each partial group, this is the value it would give.
    let mut early = 0.0_f64;
    for value in CATASTROPHIC {
        early += value;
    }
    assert_ne!(
        early, 2.0,
        "naive summation of these values loses one, which is why the expansion is stored"
    );
}

#[test]
fn every_cell_and_every_member_comes_back() {
    let mut cells = over(&["region", "period"]);
    for (region, period, value) in [
        ("north", "jan", 10.0),
        ("north", "feb", 20.0),
        ("south", "jan", 30.0),
    ] {
        cells.add(address(&[region, period]), value).expect("well-formed");
    }

    let batch = store::to_batch(&cells, Rule::Sum).expect("storable");
    assert_eq!(batch.num_rows(), 3, "one row per cell");

    let read = store::from_batch(&batch, cells.dimensions(), Rule::Sum).expect("readable");
    assert_eq!(read.get(&address(&["north", "jan"]), Rule::Sum), Some(10.0));
    assert_eq!(read.get(&address(&["north", "feb"]), Rule::Sum), Some(20.0));
    assert_eq!(read.get(&address(&["south", "jan"]), Rule::Sum), Some(30.0));
    assert_eq!(
        read.get(&address(&["south", "feb"]), Rule::Sum),
        None,
        "a cell nobody wrote is absent, not zero — an absent cell and one that nets to zero \
         lead to opposite actions"
    );
}

#[test]
fn two_runs_produce_the_same_rows_in_the_same_order() {
    // Two servers materialising the same cuboid must produce comparable files. Order that
    // varies makes a digest over them meaningless, and a digest is how a restore drill knows
    // the copy is the original.
    let mut cells = over(&["region"]);
    for (region, value) in [("south", 2.0), ("north", 1.0), ("east", 3.0)] {
        cells.add(address(&[region]), value).expect("well-formed");
    }

    let once = store::to_batch(&cells, Rule::Sum).expect("storable");
    let twice = store::to_batch(&cells, Rule::Sum).expect("storable");
    assert_eq!(format!("{once:?}"), format!("{twice:?}"));

    let read = store::from_batch(&once, cells.dimensions(), Rule::Sum).expect("readable");
    let members: Vec<&str> = read
        .addresses()
        .filter_map(|address| address.first().map(String::as_str))
        .collect();
    assert_eq!(members, vec!["east", "north", "south"], "sorted, both times");
}

#[test]
fn an_empty_cuboid_stores_and_reads_as_empty() {
    let cells = over(&["region"]);
    let batch = store::to_batch(&cells, Rule::Sum).expect("storable");
    assert_eq!(batch.num_rows(), 0);

    let read = store::from_batch(&batch, cells.dimensions(), Rule::Sum).expect("readable");
    assert_eq!(read.addresses().count(), 0);
}

#[test]
fn a_batch_missing_a_dimension_is_refused_by_name() {
    // Reading it would group every row under one member and report a total that looks right,
    // which is the failure mode this whole crate is arranged against.
    let mut cells = over(&["region"]);
    cells.add(address(&["north"]), 1.0).expect("well-formed");
    let batch = store::to_batch(&cells, Rule::Sum).expect("storable");

    let wanted = vec!["region".to_string(), "period".to_string()];
    let error = store::from_batch(&batch, &wanted, Rule::Sum).expect_err("refused");
    assert!(
        matches!(&error, NotCells::MissingDimension { dimension, .. } if dimension == "period"),
        "the refusal names the dimension: {error}"
    );
}

#[test]
fn a_batch_without_the_expansion_column_is_refused() {
    use arrow_array::{RecordBatch, StringArray};
    use arrow_schema::{DataType, Field, Schema};
    use std::sync::Arc;

    let schema = Arc::new(Schema::new(vec![Field::new("region", DataType::Utf8, false)]));
    let batch = RecordBatch::try_new(schema, vec![Arc::new(StringArray::from(vec!["north"]))])
        .expect("a valid batch");

    let error = store::from_batch(&batch, &["region".to_string()], Rule::Sum)
        .expect_err("refused");
    assert!(
        matches!(error, NotCells::MissingExact { .. }),
        "without the unrounded expansion a materialised answer cannot be bit-identical, \
         which is the only reason to trust it: {error}"
    );
}

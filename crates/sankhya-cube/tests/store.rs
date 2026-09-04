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

/// What a fixture's cells saw: their own rows, with nothing withheld.
///
/// Stated rather than defaulted. `Completeness` has no `Default` so that a value nobody
/// thought about cannot report itself complete, and a test fixture is no exception.
const SAW_EVERYTHING: sankhya_cube::complete::Completeness =
    sankhya_cube::complete::Completeness::complete(2);

#[test]
fn a_cell_survives_the_round_trip_exactly() {
    let mut cells = over(&["region"]);
    for value in CATASTROPHIC {
        cells.add(address(&["north"]), value).expect("well-formed");
    }
    let expected = cells.get(&address(&["north"]), Rule::Sum).expect("a total");
    assert_eq!(expected, 2.0, "the exact sum is two, whatever the order");

    let batch = store::to_batch(&cells, Rule::Sum, &SAW_EVERYTHING).expect("storable");
    let (read, _) = store::from_batch(&batch, cells.dimensions(), Rule::Sum).expect("readable");

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

    let batch = store::to_batch(&cells, Rule::Sum, &SAW_EVERYTHING).expect("storable");
    let (read, _) = store::from_batch(&batch, cells.dimensions(), Rule::Sum).expect("readable");

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

    let batch = store::to_batch(&cells, Rule::Sum, &SAW_EVERYTHING).expect("storable");
    assert_eq!(batch.num_rows(), 3, "one row per cell");

    let (read, _) = store::from_batch(&batch, cells.dimensions(), Rule::Sum).expect("readable");
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

    let once = store::to_batch(&cells, Rule::Sum, &SAW_EVERYTHING).expect("storable");
    let twice = store::to_batch(&cells, Rule::Sum, &SAW_EVERYTHING).expect("storable");
    assert_eq!(format!("{once:?}"), format!("{twice:?}"));

    let (read, _) = store::from_batch(&once, cells.dimensions(), Rule::Sum).expect("readable");
    let members: Vec<&str> = read
        .addresses()
        .filter_map(|address| address.first().map(String::as_str))
        .collect();
    assert_eq!(members, vec!["east", "north", "south"], "sorted, both times");
}

#[test]
fn an_empty_cuboid_stores_and_reads_as_empty() {
    let cells = over(&["region"]);
    let batch = store::to_batch(&cells, Rule::Sum, &SAW_EVERYTHING).expect("storable");
    assert_eq!(batch.num_rows(), 0);

    let (read, _) = store::from_batch(&batch, cells.dimensions(), Rule::Sum).expect("readable");
    assert_eq!(read.addresses().count(), 0);
}

#[test]
fn a_batch_missing_a_dimension_is_refused_by_name() {
    // Reading it would group every row under one member and report a total that looks right,
    // which is the failure mode this whole crate is arranged against.
    let mut cells = over(&["region"]);
    cells.add(address(&["north"]), 1.0).expect("well-formed");
    let batch = store::to_batch(&cells, Rule::Sum, &SAW_EVERYTHING).expect("storable");

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

#[test]
fn completeness_survives_the_round_trip() {
    // The reason it is stored at all. A set of cells cannot say how much of the fact table
    // reached it --- a withheld or unplaceable row leaves no trace, so counting what arrived
    // and dividing by what arrived gives one, always. A cuboid that lost this on the way to
    // disk could only ever be served as complete, which is the one claim nothing may make on
    // its own behalf.
    let mut cells = Cells::over(vec!["region".to_string()]);
    cells.add(vec!["north".to_string()], 1.0).expect("well-formed");
    cells.add(vec!["south".to_string()], 2.0).expect("well-formed");

    let partial = sankhya_cube::complete::Completeness::of(80, 20);
    let batch = store::to_batch(&cells, Rule::Sum, &partial).expect("storable");
    let (_, read) = store::from_batch(&batch, cells.dimensions(), Rule::Sum).expect("readable");

    assert_eq!(read, partial);
    assert_eq!(read.withheld(), 20, "and the withheld count is the half nothing can recover");
    assert!(!read.is_complete(), "a policy-filtered cuboid does not read back as complete");
}

#[test]
fn a_cuboid_whose_rows_disagree_about_what_it_saw_is_refused() {
    // The value is constant within a cuboid by construction, so rows that disagree mean the
    // file was assembled by something that did not know that. Picking the first would be
    // choosing which of two claims to believe, and the claim decides whether a number gets
    // presented as the whole picture.
    let mut cells = Cells::over(vec!["region".to_string()]);
    cells.add(vec!["north".to_string()], 1.0).expect("well-formed");
    cells.add(vec!["south".to_string()], 2.0).expect("well-formed");
    let batch = store::to_batch(&cells, Rule::Sum, &SAW_EVERYTHING).expect("storable");

    // Rebuilt with a disagreeing `__sankhya_contributed` column.
    // The contributed column is second from the end; see `store::schema_for`.
    let mut columns: Vec<arrow_array::ArrayRef> = batch.columns().to_vec();
    let contributed = columns.len().saturating_sub(2);
    if let Some(slot) = columns.get_mut(contributed) {
        *slot = std::sync::Arc::new(arrow_array::UInt64Array::from(vec![2_u64, 99]));
    }
    let tampered =
        arrow_array::RecordBatch::try_new(batch.schema(), columns).expect("a valid batch");

    let error = store::from_batch(&tampered, cells.dimensions(), Rule::Sum)
        .expect_err("two answers to how much this saw is not an answer");
    assert!(
        matches!(error, store::NotCells::NoCompleteness { .. }),
        "{error:?}"
    );
}

#[test]
fn a_cuboid_stores_the_value_its_rule_produces_rather_than_the_sum() {
    // `COR-05`. `to_batch` computed `contributions.exact_sum()` and used `rule` only in a
    // fallback branch; `from_batch` read it back with `add_reduced`, and `reduce` early-returns
    // the stored value for **every** rule. So a measure declared `MAX ALONG region` and
    // maintained answered the sum.
    //
    // Every test in this file passed `Rule::Sum`, and every test in `cube_rules.rs` — the file
    // written on 2026-09-01 to pin exactly this — declared its cube without `MAINTAINED`. The
    // defect was resurrected one layer below where it was fixed.
    for (rule, expected, wrong) in [
        (Rule::Max, 40.0, 70.0),
        (Rule::Min, 30.0, 70.0),
        (Rule::Mean, 35.0, 70.0),
        (Rule::First, 30.0, 70.0),
        (Rule::Last, 40.0, 70.0),
    ] {
        let mut cells = over(&["region"]);
        cells.add(address(&["north"]), 30.0).expect("well-formed");
        cells.add(address(&["north"]), 40.0).expect("well-formed");

        let live = cells.get(&address(&["north"]), rule).expect("a live answer");
        assert_eq!(live, expected, "the fixture is wrong for {rule:?}");

        let batch = store::to_batch(&cells, rule, &SAW_EVERYTHING).expect("storable");
        let (read, _) = store::from_batch(&batch, cells.dimensions(), rule).expect("readable");
        let materialised = read.get(&address(&["north"]), rule).expect("a stored answer");

        assert_eq!(
            materialised.to_bits(),
            live.to_bits(),
            "{rule:?} materialised to {materialised} where the live path answers {live}; the \
             sum of these facts is {wrong}, which is what was stored"
        );
    }
}

#[test]
fn a_sum_is_still_stored_unrounded_so_a_roll_up_composes() {
    // The control on the fix. Only a sum composes without rounding, so only a sum keeps its
    // expansion — and it must keep it, or `a_cell_survives_the_round_trip_exactly` is the only
    // thing standing between a re-association and a penny.
    //
    // `CATASTROPHIC` is not the fixture for this: its exact total is `2.0`, which one double
    // holds, so a correct expansion has one component. The property needs a total that **no**
    // double can represent — `1e16 + 1.0` needs fifty-four significant bits — so that keeping
    // it exactly requires keeping two.
    let mut cells = over(&["region"]);
    for value in [1e16, 1.0] {
        cells.add(address(&["north"]), value).expect("well-formed");
    }
    let batch = store::to_batch(&cells, Rule::Sum, &SAW_EVERYTHING).expect("storable");
    let (read, _) = store::from_batch(&batch, cells.dimensions(), Rule::Sum).expect("readable");
    let stored = read
        .contributions(&address(&["north"]))
        .and_then(sankhya_cube::cells::Contributions::exact)
        .expect("a stored expansion");
    assert!(
        stored.components().len() > 1,
        "a sum no double can hold was flattened to one, so rolling it up rounds twice: {:?}",
        stored.components()
    );

    // And a reader that wants one number still gets the right one.
    assert_eq!(
        stored.to_f64(),
        1e16,
        "the exact total rounds to 1e16, and that is what a reader of one number gets"
    );
}

#[test]
fn a_measure_with_no_reduction_from_partials_is_not_materialised() {
    // `Rule::None` is the measure the ancestor-answerability machinery exists to refuse, and
    // `Rule::Supplied` is computed by a worker in another process. Neither has a value this
    // layer can compute from contributions, and writing a zero would be materialisation
    // turning a refusal into a number — which is the whole shape of `COR-04` and `COR-05`.
    let mut cells = over(&["region"]);
    cells.add(address(&["north"]), 30.0).expect("well-formed");

    let batch = store::to_batch(&cells, Rule::None, &SAW_EVERYTHING).expect("storable");
    assert_eq!(
        batch.num_rows(),
        0,
        "a measure that composes along nothing was materialised anyway"
    );
}

//! The sparse cube, and the difference between nothing and zero.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use proptest::prelude::*;
use sankhya_cube::cells::{Cells, Contributions};
use sankhya_cube_algo::measure::Rule;

fn address(members: &[&str]) -> Vec<String> {
    members.iter().map(|m| (*m).to_string()).collect()
}

fn cube() -> Cells {
    Cells::over(vec!["period".to_string(), "entity".to_string()])
}

// --- absent is not zero -------------------------------------------------

#[test]
fn an_absent_cell_is_absent_and_not_zero() {
    // "No transactions in this period" and "transactions that net to zero" are different
    // facts, and they lead to opposite actions: one is a reconciled position, the other is a
    // feed that did not arrive. Rendered identically as `0`, the incident becomes a clean
    // report and nobody investigates a clean report.
    let mut cells = cube();
    cells.add(address(&["jan", "a"]), 5.0).expect("well-formed");
    cells.add(address(&["jan", "b"]), 3.0).expect("well-formed");
    cells.add(address(&["jan", "b"]), -3.0).expect("well-formed");

    assert_eq!(cells.get(&address(&["jan", "b"]), Rule::Sum), Some(0.0), "netted to zero");
    assert_eq!(cells.get(&address(&["feb", "a"]), Rule::Sum), None, "no data at all");
}

#[test]
fn a_cell_with_no_contributions_reduces_to_nothing_under_every_rule() {
    let empty = Contributions::none();
    for rule in [Rule::Sum, Rule::First, Rule::Last, Rule::Max, Rule::Min, Rule::Mean] {
        assert_eq!(empty.reduce(rule), None, "{rule} invented a value from nothing");
    }
}

#[test]
fn a_measure_that_composes_along_nothing_has_no_value_from_contributions() {
    // A distinct count over partial aggregates is not a distinct count. It has a value over
    // the rows; this type does not hold the rows.
    let mut some = Contributions::none();
    some.push(1.0);
    some.push(2.0);
    assert_eq!(some.reduce(Rule::None), None);
}

// --- determinism --------------------------------------------------------

#[test]
fn a_sum_does_not_depend_on_the_order_facts_arrived() {
    // Floating point addition is not associative, and partitioned work finishing in a
    // different order is enough to change a total. FR-QUERY-10 asks for bit-identical.
    let values = [1e16, 1.0, -1e16, 1.0, 3.5e-8, 2.0];
    let mut forwards = Contributions::none();
    for v in values {
        forwards.push(v);
    }
    let mut backwards = Contributions::none();
    for v in values.iter().rev() {
        backwards.push(*v);
    }
    assert_eq!(
        forwards.reduce(Rule::Sum).unwrap().to_bits(),
        backwards.reduce(Rule::Sum).unwrap().to_bits(),
        "bit-identical, not merely close"
    );
}

#[test]
fn cells_are_reported_in_a_canonical_order() {
    // A result set whose row order varies is not identical however equal its contents are.
    let mut one = cube();
    let mut two = cube();
    for a in [["feb", "b"], ["jan", "a"], ["jan", "b"]] {
        one.add(address(&a), 1.0).expect("well-formed");
    }
    for a in [["jan", "b"], ["feb", "b"], ["jan", "a"]] {
        two.add(address(&a), 1.0).expect("well-formed");
    }
    let left: Vec<_> = one.addresses().collect();
    let right: Vec<_> = two.addresses().collect();
    assert_eq!(left, right);
}

// --- addressing ---------------------------------------------------------

#[test]
fn an_address_of_the_wrong_width_is_refused_not_padded() {
    // Padding or truncating files the fact in a cell nobody addressed, and it is then a
    // real number in a real total.
    let mut cells = cube();
    let short = cells.add(address(&["jan"]), 1.0).expect_err("accepted a short address");
    assert_eq!(short.expected, 2);
    assert_eq!(short.found, 1);
    assert!(cells.add(address(&["jan", "a", "x"]), 1.0).is_err());
    assert!(cells.is_empty(), "nothing was filed");
}

// --- reduction rules ----------------------------------------------------

#[test]
fn each_rule_reduces_what_it_says_it_does() {
    let mut c = Contributions::none();
    for v in [3.0, 1.0, 2.0] {
        c.push(v);
    }
    assert_eq!(c.reduce(Rule::Sum), Some(6.0));
    assert_eq!(c.reduce(Rule::First), Some(3.0), "arrival order, not sorted");
    assert_eq!(c.reduce(Rule::Last), Some(2.0));
    assert_eq!(c.reduce(Rule::Max), Some(3.0));
    assert_eq!(c.reduce(Rule::Min), Some(1.0));
    assert_eq!(c.reduce(Rule::Mean), Some(2.0));
}

#[test]
fn a_nan_does_not_swallow_a_maximum() {
    // `f64::max` propagates the other operand, but a fold starting from NaN, or comparing
    // with `<`, silently yields NaN and the cell reads as broken rather than as its value.
    let mut c = Contributions::none();
    c.push(1.0);
    c.push(f64::NAN);
    c.push(4.0);
    assert_eq!(c.reduce(Rule::Max), Some(4.0));
    assert_eq!(c.reduce(Rule::Min), Some(1.0));
}

#[test]
fn a_cell_of_nothing_but_nan_is_present_and_not_absent() {
    // There is data here; it is unusable. Returning `None` would file a data-quality
    // problem under the same answer as a feed that never arrived, which are the two facts
    // this module exists to keep apart.
    let mut c = Contributions::none();
    c.push(f64::NAN);
    c.push(f64::NAN);
    assert!(c.reduce(Rule::Max).is_some_and(f64::is_nan), "present, and not a number");
    assert!(c.reduce(Rule::Min).is_some_and(f64::is_nan));
    assert_eq!(Contributions::none().reduce(Rule::Max), None, "and absent is still absent");
}

proptest! {
    /// A sum is the same however the facts are ordered.
    #[test]
    fn summation_is_order_independent(
        values in prop::collection::vec(-1e12f64..1e12, 1..40),
        rotation in 0usize..40
    ) {
        let mut forwards = Contributions::none();
        for v in &values {
            forwards.push(*v);
        }
        let mut rotated = Contributions::none();
        let split = rotation % values.len();
        for v in values[split..].iter().chain(values[..split].iter()) {
            rotated.push(*v);
        }
        prop_assert_eq!(
            forwards.reduce(Rule::Sum).map(f64::to_bits),
            rotated.reduce(Rule::Sum).map(f64::to_bits)
        );
    }
}

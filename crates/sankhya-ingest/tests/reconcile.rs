//! Reconciliation tests.
//!
//! The most important test here is [`xor_would_hide_exact_duplication`], which
//! demonstrates the defect the combiner was chosen to avoid rather than merely
//! asserting the choice.

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

use proptest::prelude::*;
use sankhya_ingest::{Discrepancy, Reconciliation, RowDigest, TableDigest};

fn digest_of(rows: &[&[Option<&str>]]) -> TableDigest {
    let mut d = TableDigest::empty();
    for row in rows {
        d.add_values(row);
    }
    d
}

#[test]
fn identical_data_reconciles() {
    let rows: &[&[Option<&str>]] = &[&[Some("1"), Some("a")], &[Some("2"), Some("b")]];
    let mut r = Reconciliation::new();
    r.compare("t", digest_of(rows), digest_of(rows));
    assert!(r.is_clean());
    assert_eq!(r.rows_reconciled(), 2);
}

#[test]
fn row_order_does_not_matter() {
    // Each side computes its digest independently, in whatever order it reads. If
    // order mattered, every comparison would need a sort neither side can afford.
    let forward: &[&[Option<&str>]] = &[&[Some("1")], &[Some("2")], &[Some("3")]];
    let shuffled: &[&[Option<&str>]] = &[&[Some("3")], &[Some("1")], &[Some("2")]];
    let mut r = Reconciliation::new();
    r.compare("t", digest_of(forward), digest_of(shuffled));
    assert!(r.is_clean());
}

#[test]
fn a_missing_row_is_detected() {
    let expected: &[&[Option<&str>]] = &[&[Some("1")], &[Some("2")]];
    let observed: &[&[Option<&str>]] = &[&[Some("1")]];
    let mut r = Reconciliation::new();
    r.compare("t", digest_of(expected), digest_of(observed));
    assert!(!r.is_clean());
    assert!(matches!(
        r.discrepancies()[0].1,
        Discrepancy::MissingRows { .. }
    ));
}

#[test]
fn a_duplicated_row_is_detected() {
    // At-least-once delivery applied more than once. Precisely what this harness is
    // for, and precisely what an XOR-combined digest would conceal.
    let expected: &[&[Option<&str>]] = &[&[Some("1")], &[Some("2")]];
    let observed: &[&[Option<&str>]] = &[&[Some("1")], &[Some("2")], &[Some("2")]];
    let mut r = Reconciliation::new();
    r.compare("t", digest_of(expected), digest_of(observed));
    let (_, discrepancy) = r.discrepancies()[0];
    assert!(matches!(discrepancy, Discrepancy::ExtraRows { .. }));
    assert!(
        discrepancy.to_string().contains("replayed delivery"),
        "the message should name the likely cause: {discrepancy}"
    );
}

#[test]
fn xor_is_blind_to_pairs_of_identical_rows() {
    // Demonstrating precisely where XOR fails, rather than asserting it vaguely.
    //
    // XOR is self-cancelling: any value appearing an even number of times contributes
    // nothing. So two datasets that differ only in WHICH row is duplicated produce
    // identical XOR digests, even though their contents differ and their row counts
    // agree.
    //
    // This is realistic rather than contrived. SANKHYA explicitly supports append-only
    // tables with no row identity, where duplicate rows are legitimate and expected —
    // and those are exactly the tables where a replay defect would substitute one
    // duplicated value for another.
    let expected: &[&[Option<&str>]] = &[&[Some("a")], &[Some("b")], &[Some("c")], &[Some("c")]];
    let observed: &[&[Option<&str>]] = &[&[Some("a")], &[Some("b")], &[Some("d")], &[Some("d")]];

    let xor = |rows: &[&[Option<&str>]]| {
        rows.iter()
            .fold(0u128, |acc, r| acc ^ RowDigest::of(r).get())
    };
    assert_eq!(
        xor(expected),
        xor(observed),
        "XOR must be shown to be blind here, or this test proves nothing"
    );

    // The additive combiner sees it.
    let e = digest_of(expected);
    let o = digest_of(observed);
    assert_eq!(
        e.rows(),
        o.rows(),
        "the counts agree, so only the checksum can catch this"
    );
    assert_ne!(
        e.checksum(),
        o.checksum(),
        "the additive combiner must distinguish datasets XOR cannot"
    );

    let mut r = Reconciliation::new();
    r.compare("t", e, o);
    assert!(
        !r.is_clean(),
        "reconciliation must fail on data XOR would have passed"
    );
}

#[test]
fn duplication_moves_the_checksum() {
    // The simpler property underneath: adding the same row again always changes the
    // digest. Under XOR it would restore the original value.
    let rows: &[&[Option<&str>]] = &[&[Some("1")], &[Some("2")]];
    let once = digest_of(rows);
    let mut twice = once;
    for row in rows {
        twice.add_values(row);
    }
    assert_ne!(once.checksum(), twice.checksum());
    assert_eq!(twice.rows(), 4);
}

#[test]
fn equal_counts_with_different_contents_are_detected() {
    // The most alarming outcome, and the one a count-only check passes.
    let expected: &[&[Option<&str>]] = &[&[Some("1"), Some("a")]];
    let observed: &[&[Option<&str>]] = &[&[Some("1"), Some("CORRUPTED")]];
    let mut r = Reconciliation::new();
    r.compare("t", digest_of(expected), digest_of(observed));
    let (_, discrepancy) = r.discrepancies()[0];
    assert!(matches!(discrepancy, Discrepancy::ContentMismatch { .. }));
    assert!(discrepancy
        .to_string()
        .contains("count-only check would have passed"));
}

#[test]
fn a_null_is_distinguishable_from_an_empty_string() {
    // Conflating them would make a null-clobbering defect invisible - and writing a
    // null over real data is exactly the failure the withheld-value handling exists to
    // prevent, so the harness must be able to see it.
    let with_null = RowDigest::of(&[None]);
    let with_empty = RowDigest::of(&[Some("")]);
    assert_ne!(with_null, with_empty);
}

#[test]
fn field_boundaries_are_unambiguous() {
    // Without length-prefixing, ("ab","c") and ("a","bc") would hash identically and a
    // column-shift defect would reconcile cleanly.
    assert_ne!(
        RowDigest::of(&[Some("ab"), Some("c")]),
        RowDigest::of(&[Some("a"), Some("bc")])
    );
}

#[test]
fn partial_digests_combine_in_any_order() {
    // A digest may be computed in parallel over arbitrary partitions, which is what
    // makes it affordable at scale.
    let all: &[&[Option<&str>]] = &[&[Some("1")], &[Some("2")], &[Some("3")], &[Some("4")]];
    let whole = digest_of(all);

    let left = digest_of(&all[..2]);
    let right = digest_of(&all[2..]);
    assert_eq!(left.merge(right), whole);
    assert_eq!(
        right.merge(left),
        whole,
        "merging must be order-independent"
    );
}

#[test]
fn a_clean_reconciliation_reports_what_it_checked() {
    let rows: &[&[Option<&str>]] = &[&[Some("1")]];
    let mut r = Reconciliation::new();
    r.compare("alpha", digest_of(rows), digest_of(rows));
    r.compare("beta", digest_of(rows), digest_of(rows));
    let report = r.to_string();
    assert!(report.contains("2 tables"), "{report}");
    assert!(report.contains("2 rows"), "{report}");
}

proptest! {
    /// Identical inputs always reconcile, whatever they contain.
    #[test]
    fn identical_inputs_always_reconcile(
        rows in prop::collection::vec(prop::collection::vec(prop::option::of(".{0,16}"), 1..5), 0..24)
    ) {
        let borrowed: Vec<Vec<Option<&str>>> = rows
            .iter()
            .map(|r| r.iter().map(|v| v.as_deref()).collect())
            .collect();

        let mut a = TableDigest::empty();
        let mut b = TableDigest::empty();
        for row in &borrowed {
            a.add_values(row);
        }
        // The observed side reads in reverse, as an independent reader might.
        for row in borrowed.iter().rev() {
            b.add_values(row);
        }

        let mut r = Reconciliation::new();
        r.compare("t", a, b);
        prop_assert!(r.is_clean(), "identical data must reconcile: {}", r);
    }

    /// Adding any row to one side always breaks reconciliation.
    ///
    /// This is the guarantee stated as a property: there is no dataset for which an
    /// extra row goes unnoticed.
    #[test]
    fn an_extra_row_is_never_missed(
        rows in prop::collection::vec(".{0,12}", 1..16),
        extra in ".{0,12}",
    ) {
        let mut expected = TableDigest::empty();
        for row in &rows {
            expected.add_values(&[Some(row.as_str())]);
        }
        let mut observed = expected;
        observed.add_values(&[Some(extra.as_str())]);

        let mut r = Reconciliation::new();
        r.compare("t", expected, observed);
        prop_assert!(!r.is_clean(), "an extra row must always be detected");
    }

    /// Digesting never panics, whatever the values.
    #[test]
    fn digesting_never_panics(values in prop::collection::vec(prop::option::of(".{0,64}"), 0..12)) {
        let borrowed: Vec<Option<&str>> = values.iter().map(|v| v.as_deref()).collect();
        let _ = RowDigest::of(&borrowed);
    }
}

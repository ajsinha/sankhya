//! The arrival tier holds what is not yet durable, and nothing else governs eviction.
//!
//! The valuable assertions here are about what the tier *refuses* to do: release a
//! segment publication has not covered, and accept work it cannot hold. Both refusals
//! look like malfunctions from the outside and are the whole point.

use arrow_array::{Int64Array, RecordBatch, UInt64Array};
use arrow_schema::{DataType, Field, Schema};
use sankhya_plan::{plan_splice, TierRef};
use sankhya_table_memory::{Admission, ArrivalBuffer, MemoryBudget};
use sankhya_types::{Lsn, LsnRange};
use std::sync::Arc;

fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("_sankhya_commit_lsn", DataType::UInt64, false),
    ]))
}

/// A segment covering `(from, to]`, one row per position.
fn segment(from: u64, to: u64) -> (RecordBatch, LsnRange) {
    let lsns: Vec<u64> = (from + 1..=to).collect();
    let ids: Vec<i64> = lsns
        .iter()
        .map(|l| i64::try_from(*l).expect("small"))
        .collect();
    let batch = RecordBatch::try_new(
        schema(),
        vec![
            Arc::new(Int64Array::from(ids)),
            Arc::new(UInt64Array::from(lsns)),
        ],
    )
    .expect("building a batch");
    (
        batch,
        LsnRange::new(Lsn::new(from), Lsn::new(to)).expect("a non-empty range"),
    )
}

fn buffer() -> ArrivalBuffer {
    ArrivalBuffer::new("arrival", schema(), MemoryBudget::default())
}

fn rows_in(batches: &[RecordBatch]) -> Vec<u64> {
    let mut out = Vec::new();
    for b in batches {
        let lsns = b
            .column_by_name("_sankhya_commit_lsn")
            .expect("the column")
            .as_any()
            .downcast_ref::<UInt64Array>()
            .expect("the type");
        out.extend((0..lsns.len()).map(|i| lsns.value(i)));
    }
    out
}

#[test]
fn a_write_is_visible_before_it_is_durable() {
    // The reason this tier exists. Nothing has been published, and the query still
    // returns the row.
    let mut b = buffer();
    let (batch, coverage) = segment(0, 5);
    assert_eq!(b.append(batch, coverage), Admission::Accepted);

    let rows = rows_in(&b.scan(Lsn::new(5)).expect("scanning"));
    assert_eq!(rows, vec![1, 2, 3, 4, 5]);
}

#[test]
fn a_durable_segment_is_released_and_an_undurable_one_is_not() {
    let mut b = buffer();
    for (from, to) in [(0, 10), (10, 20), (20, 30)] {
        let (batch, coverage) = segment(from, to);
        b.append(batch, coverage);
    }
    assert_eq!(b.segments(), 3);

    let released = b.note_durable(Lsn::new(20));

    assert_eq!(released.segments, 2);
    assert_eq!(released.rows, 20);
    assert_eq!(b.segments(), 1);
    assert!(b.bytes() > 0);
}

#[test]
fn memory_pressure_does_not_release_anything() {
    // The rule that makes this tier correct rather than merely useful. The tier is well
    // past its soft limit and publication has not moved; nothing is released, because
    // releasing would open a gap and the read path would have to refuse the query.
    let budget = MemoryBudget {
        soft_limit: 1,
        hard_limit: usize::MAX,
    };
    let mut b = ArrivalBuffer::new("arrival", schema(), budget);

    let (batch, coverage) = segment(0, 100);
    let admission = b.append(batch, coverage);

    assert!(matches!(admission, Admission::AcceptedUnderPressure { .. }));
    assert_eq!(b.segments(), 1, "pressure alone released a segment");

    // A second append still succeeds, still releases nothing.
    let (batch, coverage) = segment(100, 200);
    b.append(batch, coverage);
    assert_eq!(b.segments(), 2);
    assert_eq!(b.rows(), 200);
}

#[test]
fn a_full_tier_pushes_back_rather_than_dropping() {
    // With nothing releasable and no headroom, the only correct answer is to refuse.
    // Dropping would silently lose changes; the read path cannot detect that, because
    // the missing rows were never anywhere else.
    let (probe, _) = segment(0, 10);
    let one_segment = probe.get_array_memory_size();

    let budget = MemoryBudget {
        soft_limit: one_segment,
        hard_limit: one_segment + one_segment / 2,
    };
    let mut b = ArrivalBuffer::new("arrival", schema(), budget);

    let (batch, coverage) = segment(0, 10);
    assert!(b.append(batch, coverage).accepted());

    let (batch, coverage) = segment(10, 20);
    let refused = b.append(batch, coverage);

    assert!(!refused.accepted());
    let Admission::Refused {
        durable_through,
        held_through,
        ..
    } = refused
    else {
        panic!("expected a refusal");
    };
    // The diagnosis points at publication, which is the actual problem.
    assert_eq!(durable_through, Lsn::new(0));
    assert_eq!(held_through, Lsn::new(10));
    assert_eq!(b.rows(), 10, "the refused batch was partly admitted");
}

#[test]
fn refusal_clears_once_publication_catches_up() {
    // The refusal is a backpressure signal, not a terminal state.
    let (probe, _) = segment(0, 10);
    let one_segment = probe.get_array_memory_size();
    let budget = MemoryBudget {
        soft_limit: one_segment,
        hard_limit: one_segment + one_segment / 2,
    };
    let mut b = ArrivalBuffer::new("arrival", schema(), budget);

    let (batch, coverage) = segment(0, 10);
    b.append(batch, coverage);
    let (batch, coverage) = segment(10, 20);
    assert!(!b.append(batch, coverage).accepted());

    b.note_durable(Lsn::new(10));

    let (batch, coverage) = segment(10, 20);
    assert!(b.append(batch, coverage).accepted());
}

#[test]
fn coverage_abuts_the_published_tier_exactly() {
    // The splice planner rejects overlapping tiers rather than guessing which to
    // believe, so the arrival tier must declare coverage starting at the durable
    // frontier even though it physically holds more.
    let mut b = buffer();
    let (batch, coverage) = segment(0, 100);
    b.append(batch, coverage);
    b.note_durable(Lsn::new(60));

    let arrival = b.coverage().expect("the tier holds undurable data");
    assert_eq!(arrival.start_exclusive(), Lsn::new(60));
    assert_eq!(arrival.end_inclusive(), Lsn::new(100));

    let published = TierRef::new("published", LsnRange::up_to(Lsn::new(60)));
    let splice = plan_splice(
        &[published, TierRef::new("arrival", arrival)],
        Lsn::new(100),
    )
    .expect("the two tiers must splice");

    assert_eq!(splice.tier_names(), vec!["published", "arrival"]);
}

#[test]
fn a_straddling_segment_is_filtered_per_row_not_per_segment() {
    // The defect this prevents: the published tier already holds 1..=60, so returning
    // the whole segment would double-count them. This is the same mistake as
    // suppressing duplicates per batch rather than per row, one layer up.
    let mut b = buffer();
    let (batch, coverage) = segment(0, 100);
    b.append(batch, coverage);
    b.note_durable(Lsn::new(60));

    assert_eq!(
        b.segments(),
        1,
        "a straddling segment must be retained whole"
    );

    let rows = rows_in(&b.scan(Lsn::new(100)).expect("scanning"));
    assert_eq!(rows.len(), 40);
    assert_eq!(rows.first(), Some(&61));
    assert_eq!(rows.last(), Some(&100));
}

#[test]
fn a_scan_stops_at_its_target() {
    // Time travel and read-your-own-writes both pin a position. Returning anything past
    // it would show the caller a future it did not ask for.
    let mut b = buffer();
    let (batch, coverage) = segment(0, 100);
    b.append(batch, coverage);

    let rows = rows_in(&b.scan(Lsn::new(30)).expect("scanning"));
    assert_eq!(rows.len(), 30);
    assert_eq!(rows.last(), Some(&30));
}

#[test]
fn a_fully_published_tier_offers_no_coverage() {
    // With nothing left to contribute, the tier must drop out of the splice entirely
    // rather than offer an empty interval the planner would have to special-case.
    let mut b = buffer();
    let (batch, coverage) = segment(0, 10);
    b.append(batch, coverage);
    b.note_durable(Lsn::new(10));

    assert_eq!(b.coverage(), None);
    assert_eq!(b.segments(), 0);
    assert_eq!(b.bytes(), 0);
}

#[test]
fn a_stale_publication_report_does_not_move_the_frontier_backwards() {
    // Publication reports can arrive out of order. Lowering the frontier would make
    // already-released data look uncovered, which is a gap the read path cannot fill.
    let mut b = buffer();
    let (batch, coverage) = segment(0, 50);
    b.append(batch, coverage);
    let (batch, coverage) = segment(50, 100);
    b.append(batch, coverage);

    b.note_durable(Lsn::new(50));
    assert_eq!(b.durable_through(), Lsn::new(50));

    let released = b.note_durable(Lsn::new(20));
    assert_eq!(b.durable_through(), Lsn::new(50));
    assert_eq!(released.segments, 0);
}

#[test]
fn scanning_an_empty_tier_is_not_an_error() {
    let b = buffer();
    assert!(b.scan(Lsn::new(100)).expect("scanning").is_empty());
    assert_eq!(b.coverage(), None);
    assert_eq!(b.held_through(), Lsn::new(0));
}

#[test]
fn a_tier_that_starts_mid_stream_does_not_claim_what_came_before() {
    // Regression. The tier held only (700, 1000] but declared (0, 1000], because
    // coverage was trimmed against the durable frontier alone and the frontier was
    // still at the origin. The splice then found an exact cover that did not exist:
    // positions 1..=700 would simply have been absent from the answer, with no error.
    //
    // This is the normal case, not an edge case -- a table onboarded from a running
    // stream starts mid-stream by construction.
    let mut b = buffer();
    let (batch, coverage) = segment(700, 1_000);
    b.append(batch, coverage);

    assert_eq!(b.durable_through(), Lsn::new(0));
    assert_eq!(b.held_from(), Lsn::new(700));

    let declared = b.coverage().expect("the tier holds data");
    assert_eq!(
        declared.start_exclusive(),
        Lsn::new(700),
        "the tier claimed positions it never held"
    );
    assert_eq!(declared.end_inclusive(), Lsn::new(1_000));
}

#[test]
fn a_mid_stream_tier_leaves_a_gap_the_planner_can_see() {
    // The consequence of the fix, stated as behaviour: with nothing covering the space
    // between, the query is refused rather than answered short.
    let mut b = buffer();
    let (batch, coverage) = segment(700, 1_000);
    b.append(batch, coverage);

    let published = TierRef::new("published", LsnRange::up_to(Lsn::new(300)));
    let arrival = TierRef::new("arrival", b.coverage().expect("coverage"));

    assert!(
        plan_splice(&[published, arrival], Lsn::new(1_000)).is_err(),
        "positions 301..=700 are held by nothing and the splice must refuse"
    );
}

#[test]
fn declared_coverage_matches_the_scan_for_a_mid_stream_tier() {
    let mut b = buffer();
    let (batch, coverage) = segment(700, 1_000);
    b.append(batch, coverage);

    let declared = b.coverage().expect("coverage");
    let rows = rows_in(&b.scan(Lsn::new(1_000)).expect("scanning"));

    assert_eq!(rows.len(), 300);
    assert_eq!(
        u64::try_from(rows.len()).expect("small"),
        declared.end_inclusive().get() - declared.start_exclusive().get(),
        "the tier declared a different span from the one it can produce"
    );
}

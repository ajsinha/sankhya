//! The invariant, over arbitrary interleavings of appends and publications.
//!
//! Individual cases can only cover interleavings someone thought of. The failure mode
//! that matters here — a position counted twice, or lost between the two tiers — arises
//! from the *relationship* between publication and retention, so it is worth generating
//! that relationship rather than choosing it.

use arrow_array::{Int64Array, RecordBatch, UInt64Array};
use arrow_schema::{DataType, Field, Schema};
use proptest::prelude::*;
use sankhya_table_memory::{ArrivalBuffer, MemoryBudget};
use sankhya_types::{Lsn, LsnRange};
use std::sync::Arc;

fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("_sankhya_commit_lsn", DataType::UInt64, false),
    ]))
}

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
    .expect("building");
    (
        batch,
        LsnRange::new(Lsn::new(from), Lsn::new(to)).expect("non-empty"),
    )
}

fn scanned_positions(b: &ArrivalBuffer, target: Lsn) -> Vec<u64> {
    let batches = b.scan(target).expect("scanning");
    let mut out = Vec::new();
    for batch in &batches {
        let lsns = batch
            .column_by_name("_sankhya_commit_lsn")
            .expect("the column")
            .as_any()
            .downcast_ref::<UInt64Array>()
            .expect("the type");
        out.extend((0..lsns.len()).map(|i| lsns.value(i)));
    }
    out
}

/// A sequence of segment widths, and the publication frontier after each.
fn plan() -> impl Strategy<Value = Vec<(u64, u64)>> {
    prop::collection::vec((1u64..12, 0u64..14), 1..12)
}

proptest! {
    /// Every position between the durable frontier and the target appears exactly once.
    ///
    /// This is the tier's half of INV-1. The published tier supplies everything at or
    /// below the frontier; the arrival tier must supply everything above it, up to the
    /// target, with no duplicate and no gap. Either failure produces a wrong answer
    /// that nothing downstream can detect.
    #[test]
    fn the_two_tiers_cover_every_position_exactly_once(steps in plan()) {
        let mut b = ArrivalBuffer::new("arrival", schema(), MemoryBudget {
            soft_limit: usize::MAX,
            hard_limit: usize::MAX,
        });

        let mut frontier = 0u64;
        let mut end = 0u64;

        for (width, advance) in steps {
            let (batch, coverage) = segment(end, end + width);
            prop_assert!(b.append(batch, coverage).accepted());
            end += width;

            // Publication may lag arbitrarily, and may not exceed what exists.
            frontier = (frontier + advance).min(end);
            b.note_durable(Lsn::new(frontier));

            // The published tier answers (0, frontier]; the arrival tier answers the
            // rest. Together they must be exactly (0, end].
            let from_arrival = scanned_positions(&b, Lsn::new(end));
            let mut all: Vec<u64> = (1..=frontier).collect();
            all.extend(from_arrival.iter().copied());
            all.sort_unstable();

            let expected: Vec<u64> = (1..=end).collect();
            prop_assert_eq!(&all, &expected);
        }
    }

    /// A scan never returns a position at or below the durable frontier.
    ///
    /// Anything it did return would be double-counted against the published tier.
    #[test]
    fn a_scan_never_returns_a_durable_position(steps in plan()) {
        let mut b = ArrivalBuffer::new("arrival", schema(), MemoryBudget {
            soft_limit: usize::MAX,
            hard_limit: usize::MAX,
        });

        let mut frontier = 0u64;
        let mut end = 0u64;

        for (width, advance) in steps {
            let (batch, coverage) = segment(end, end + width);
            b.append(batch, coverage);
            end += width;
            frontier = (frontier + advance).min(end);
            b.note_durable(Lsn::new(frontier));

            for position in scanned_positions(&b, Lsn::new(end)) {
                prop_assert!(
                    position > frontier,
                    "position {} is at or below the durable frontier {}",
                    position,
                    frontier
                );
            }
        }
    }

    /// The declared coverage is exactly what a scan can produce.
    ///
    /// A tier that declares more than it holds makes the splice a lie; one that declares
    /// less makes the query refuse for no reason.
    #[test]
    fn declared_coverage_matches_what_the_scan_returns(steps in plan()) {
        let mut b = ArrivalBuffer::new("arrival", schema(), MemoryBudget {
            soft_limit: usize::MAX,
            hard_limit: usize::MAX,
        });

        let mut frontier = 0u64;
        let mut end = 0u64;

        for (width, advance) in steps {
            let (batch, coverage) = segment(end, end + width);
            b.append(batch, coverage);
            end += width;
            frontier = (frontier + advance).min(end);
            b.note_durable(Lsn::new(frontier));

            let positions = scanned_positions(&b, Lsn::new(end));
            match b.coverage() {
                None => prop_assert!(positions.is_empty()),
                Some(range) => {
                    prop_assert_eq!(range.start_exclusive(), Lsn::new(frontier));
                    prop_assert_eq!(range.end_inclusive(), Lsn::new(end));
                    prop_assert_eq!(positions.len() as u64, end - frontier);
                }
            }
        }
    }

    /// Releasing is monotone: the frontier only rises, and retained rows only shrink.
    #[test]
    fn the_frontier_never_retreats(steps in plan()) {
        let mut b = ArrivalBuffer::new("arrival", schema(), MemoryBudget {
            soft_limit: usize::MAX,
            hard_limit: usize::MAX,
        });
        let (batch, coverage) = segment(0, 200);
        b.append(batch, coverage);

        let mut highest = 0u64;
        for (width, advance) in steps {
            // Deliberately unordered: reports arrive out of order in practice.
            let reported = (width * 7 + advance * 13) % 220;
            b.note_durable(Lsn::new(reported));
            highest = highest.max(reported);
            prop_assert_eq!(b.durable_through(), Lsn::new(highest));
        }
    }
}

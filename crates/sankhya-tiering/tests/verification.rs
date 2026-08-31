//! Exhaustive verification: what it catches, and the corruptions that a cheaper check misses.
//!
//! `FR-TIER-09` names three checks and one prohibition --- **count equality alone is not
//! evidence** --- so most of these tests are constructed so that a weaker verification would
//! pass. A partition copied with every value replaced by its default has the right row count. A
//! partition whose rows had two values exchanged has the right count, the right key set and the
//! right per-column multisets.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use arrow_array::{Int32Array, Int64Array, RecordBatch, StringArray, TimestampMicrosecondArray};
use arrow_schema::{DataType, Field, Schema, TimeUnit};
use sankhya_tiering::verify::{compare, Discrepancy, Fingerprint, Scan, ScanFailure, Side};
use std::sync::Arc;

/// A batch of `(id, name, amount)`, keyed on `id`.
fn batch(ids: &[i64], names: &[&str], amounts: &[i64]) -> RecordBatch {
    let schema = Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("name", DataType::Utf8, true),
        Field::new("amount", DataType::Int64, true),
    ]);
    RecordBatch::try_new(
        Arc::new(schema),
        vec![
            Arc::new(Int64Array::from(ids.to_vec())),
            Arc::new(StringArray::from(names.to_vec())),
            Arc::new(Int64Array::from(amounts.to_vec())),
        ],
    )
    .expect("the arrays are the same length")
}

fn fingerprint(batches: &[RecordBatch]) -> Fingerprint {
    let mut scan = Scan::new(
        vec!["id".to_string()],
        vec!["name".to_string(), "amount".to_string()],
    )
    .expect("one key column");
    for one in batches {
        scan.absorb(one).expect("every column encodes");
    }
    scan.finish()
}

#[test]
fn a_faithful_copy_verifies() {
    let source = fingerprint(&[batch(&[1, 2, 3], &["a", "b", "c"], &[10, 20, 30])]);
    let archive = fingerprint(&[batch(&[1, 2, 3], &["a", "b", "c"], &[10, 20, 30])]);

    let verification = compare(3, &source, &archive);
    assert!(verification.passed(), "{:?}", verification.discrepancies);
    assert!(verification.proof().is_some());
}

#[test]
fn two_rows_with_their_values_exchanged_are_caught() {
    // The corruption that independent per-column checksums cannot see. Every column's multiset
    // is unchanged, the row count is unchanged and the key set is unchanged, so a verification
    // that checksums each column over its own sorted values reports a faithful archive --- of
    // data in which two rows' amounts have been swapped.
    let source = fingerprint(&[batch(&[1, 2], &["a", "b"], &[10, 20])]);
    let archive = fingerprint(&[batch(&[1, 2], &["a", "b"], &[20, 10])]);

    let verification = compare(2, &source, &archive);
    assert!(!verification.passed());
    assert!(
        verification
            .discrepancies
            .iter()
            .any(|d| matches!(d, Discrepancy::Column { column, .. } if column == "amount")),
        "{:?}",
        verification.discrepancies
    );
}

#[test]
fn the_row_count_matching_is_not_enough() {
    // `FR-TIER-09` says it outright. Same count, same keys, every value replaced.
    let source = fingerprint(&[batch(&[1, 2, 3], &["a", "b", "c"], &[10, 20, 30])]);
    let archive = fingerprint(&[batch(&[1, 2, 3], &["", "", ""], &[0, 0, 0])]);

    let verification = compare(3, &source, &archive);
    assert!(!verification.passed());
    assert_eq!(
        verification.discrepancies.len(),
        2,
        "both value columns, not the first: {:?}",
        verification.discrepancies
    );
}

#[test]
fn the_order_rows_arrive_in_does_not_matter() {
    // An archive scan and a source scan have no reason to agree on read order, and a
    // verification that depended on it would fail on faithful copies.
    let source = fingerprint(&[batch(&[3, 1, 2], &["c", "a", "b"], &[30, 10, 20])]);
    let archive = fingerprint(&[batch(&[1, 2, 3], &["a", "b", "c"], &[10, 20, 30])]);

    assert!(compare(3, &source, &archive).passed());
}

#[test]
fn the_batch_boundaries_do_not_matter() {
    let source = fingerprint(&[batch(&[1, 2, 3, 4], &["a", "b", "c", "d"], &[1, 2, 3, 4])]);
    let archive = fingerprint(&[
        batch(&[1, 2], &["a", "b"], &[1, 2]),
        batch(&[3], &["c"], &[3]),
        batch(&[4], &["d"], &[4]),
    ]);

    assert!(compare(4, &source, &archive).passed());
}

#[test]
fn a_missing_row_is_a_count_and_a_key_set_failure() {
    let source = fingerprint(&[batch(&[1, 2, 3], &["a", "b", "c"], &[10, 20, 30])]);
    let archive = fingerprint(&[batch(&[1, 2], &["a", "b"], &[10, 20])]);

    let verification = compare(3, &source, &archive);
    assert!(verification
        .discrepancies
        .iter()
        .any(|d| matches!(d, Discrepancy::RowCount { .. })));
    assert!(verification
        .discrepancies
        .iter()
        .any(|d| matches!(d, Discrepancy::KeySet { .. })));
}

#[test]
fn both_sides_scanning_nothing_is_a_failure_rather_than_a_pass() {
    // The hole a source-against-archive comparison has on its own: two scans pointed at the
    // wrong place agree about everything, because there is nothing to disagree about. The plan
    // already knows how many rows the partition holds.
    let empty = fingerprint(&[batch(&[], &[], &[])]);
    let also_empty = fingerprint(&[batch(&[], &[], &[])]);

    let verification = compare(12_000, &empty, &also_empty);
    assert!(!verification.passed());
    assert_eq!(
        verification.discrepancies,
        vec![Discrepancy::PlannedRowCount { planned: 12_000, source: 0 }]
    );
}

#[test]
fn a_null_and_an_empty_string_are_different_values() {
    // A scheme that writes zero bytes for both cannot tell an archive that dropped a value from
    // one that preserved an empty one.
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("name", DataType::Utf8, true),
    ]));
    let with_null = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(Int64Array::from(vec![1_i64])),
            Arc::new(StringArray::from(vec![None::<&str>])),
        ],
    )
    .unwrap();
    let with_empty = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from(vec![1_i64])),
            Arc::new(StringArray::from(vec![Some("")])),
        ],
    )
    .unwrap();

    let mut one = Scan::new(vec!["id".to_string()], vec!["name".to_string()]).unwrap();
    one.absorb(&with_null).unwrap();
    let mut other = Scan::new(vec!["id".to_string()], vec!["name".to_string()]).unwrap();
    other.absorb(&with_empty).unwrap();

    assert!(!compare(1, &one.finish(), &other.finish()).passed());
}

#[test]
fn the_same_number_at_two_widths_is_two_values() {
    // An archive whose schema drifted from Int64 to Int32 holds the same integers in a
    // different type. Without a type tag the checksums would agree and the drift would be
    // archived as faithful.
    let wide = RecordBatch::try_new(
        Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("n", DataType::Int64, false),
        ])),
        vec![
            Arc::new(Int64Array::from(vec![1_i64])),
            Arc::new(Int64Array::from(vec![7_i64])),
        ],
    )
    .unwrap();
    let narrow = RecordBatch::try_new(
        Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("n", DataType::Int32, false),
        ])),
        vec![
            Arc::new(Int64Array::from(vec![1_i64])),
            Arc::new(Int32Array::from(vec![7_i32])),
        ],
    )
    .unwrap();

    let mut one = Scan::new(vec!["id".to_string()], vec!["n".to_string()]).unwrap();
    one.absorb(&wide).unwrap();
    let mut other = Scan::new(vec!["id".to_string()], vec!["n".to_string()]).unwrap();
    other.absorb(&narrow).unwrap();

    assert!(!compare(1, &one.finish(), &other.finish()).passed());
}

#[test]
fn a_composite_key_is_a_function_of_its_parts_not_their_concatenation() {
    // `["ab", "c"]` and `["a", "bc"]` are different keys. Unprefixed, they are the same bytes.
    let schema = Arc::new(Schema::new(vec![
        Field::new("left", DataType::Utf8, false),
        Field::new("right", DataType::Utf8, false),
    ]));
    let one = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(StringArray::from(vec!["ab"])),
            Arc::new(StringArray::from(vec!["c"])),
        ],
    )
    .unwrap();
    let other = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(vec!["a"])),
            Arc::new(StringArray::from(vec!["bc"])),
        ],
    )
    .unwrap();

    let keys = vec!["left".to_string(), "right".to_string()];
    let mut first = Scan::new(keys.clone(), Vec::new()).unwrap();
    first.absorb(&one).unwrap();
    let mut second = Scan::new(keys, Vec::new()).unwrap();
    second.absorb(&other).unwrap();

    let verification = compare(1, &first.finish(), &second.finish());
    assert!(verification
        .discrepancies
        .iter()
        .any(|d| matches!(d, Discrepancy::KeySet { .. })));
}

#[test]
fn a_duplicated_key_is_reported_against_the_side_that_has_it() {
    let source = fingerprint(&[batch(&[1, 2], &["a", "b"], &[10, 20])]);
    let archive = fingerprint(&[batch(&[1, 1], &["a", "a"], &[10, 10])]);

    let verification = compare(2, &source, &archive);
    assert!(verification.discrepancies.contains(&Discrepancy::DuplicateKeys {
        side: Side::Archive,
        count: 1
    }));
}

#[test]
fn a_column_on_one_side_only_is_reported_with_the_side_it_is_missing_from() {
    let source = fingerprint(&[batch(&[1], &["a"], &[10])]);

    let mut archive = Scan::new(vec!["id".to_string()], vec!["name".to_string()]).unwrap();
    archive.absorb(&batch(&[1], &["a"], &[10])).unwrap();

    let verification = compare(1, &source, &archive.finish());
    assert!(verification.discrepancies.contains(&Discrepancy::ColumnMissing {
        column: "amount".to_string(),
        side: Side::Archive
    }));
}

#[test]
fn a_change_beyond_the_first_block_is_still_caught() {
    // Everything under `BLOCK_ROWS` is one leaf and never exercises the tree. This crosses the
    // boundary, so the root is built from real internal nodes.
    let count = 5_000_i64;
    let ids: Vec<i64> = (0..count).collect();
    let names: Vec<String> = ids.iter().map(|id| format!("n{id}")).collect();
    let borrowed: Vec<&str> = names.iter().map(String::as_str).collect();
    let amounts: Vec<i64> = ids.clone();

    let source = fingerprint(&[batch(&ids, &borrowed, &amounts)]);

    let mut changed = amounts.clone();
    changed[4_999] = -1;
    let archive = fingerprint(&[batch(&ids, &borrowed, &changed)]);

    let verification = compare(count as u64, &source, &archive);
    assert!(verification
        .discrepancies
        .iter()
        .any(|d| matches!(d, Discrepancy::Column { column, .. } if column == "amount")));
    assert_eq!(source.keys.blocks, archive.keys.blocks, "the shapes agree");
    assert!(source.keys.blocks > 1, "the tree has internal nodes");
}

#[test]
fn a_scan_with_no_key_columns_is_refused() {
    assert_eq!(
        Scan::new(Vec::new(), vec!["a".to_string()]).err(),
        Some(ScanFailure::NoKeyColumns)
    );
}

#[test]
fn a_missing_key_column_is_fatal_rather_than_a_discrepancy() {
    // Without the key there is no set to compare, and the remaining two checks are exactly the
    // "count equality is not evidence" case.
    let mut scan = Scan::new(vec!["absent".to_string()], Vec::new()).unwrap();
    assert_eq!(
        scan.absorb(&batch(&[1], &["a"], &[10])),
        Err(ScanFailure::MissingKeyColumn { column: "absent".to_string() })
    );
}

#[test]
fn a_column_appearing_part_way_through_a_scan_is_refused() {
    let mut scan = Scan::new(vec!["id".to_string()], vec!["amount".to_string()]).unwrap();

    let without = RecordBatch::try_new(
        Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)])),
        vec![Arc::new(Int64Array::from(vec![1_i64]))],
    )
    .unwrap();
    scan.absorb(&without).unwrap();

    assert_eq!(
        scan.absorb(&batch(&[2], &["b"], &[20])),
        Err(ScanFailure::SchemaChanged { column: "amount".to_string() })
    );
}

#[test]
fn two_timestamps_that_differ_only_in_zone_are_different_values() {
    // `TimestampUtc` and `TimestampLocal` hold the same microsecond count and are not the same
    // value --- conflating them is how a column shifts by hours. Nothing in the number says
    // which one it is, so only the type tag distinguishes them, and an archive that lost the
    // zone would otherwise checksum identically to the source that had it.
    let at = 1_700_000_000_000_000_i64;
    let zoned = RecordBatch::try_new(
        Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int64, false),
            Field::new(
                "at",
                DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
                false,
            ),
        ])),
        vec![
            Arc::new(Int64Array::from(vec![1_i64])),
            Arc::new(TimestampMicrosecondArray::from(vec![at]).with_timezone("UTC")),
        ],
    )
    .unwrap();
    let naive = RecordBatch::try_new(
        Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("at", DataType::Timestamp(TimeUnit::Microsecond, None), false),
        ])),
        vec![
            Arc::new(Int64Array::from(vec![1_i64])),
            Arc::new(TimestampMicrosecondArray::from(vec![at])),
        ],
    )
    .unwrap();

    let mut one = Scan::new(vec!["id".to_string()], vec!["at".to_string()]).unwrap();
    one.absorb(&zoned).unwrap();
    let mut other = Scan::new(vec!["id".to_string()], vec!["at".to_string()]).unwrap();
    other.absorb(&naive).unwrap();

    let verification = compare(1, &one.finish(), &other.finish());
    assert!(
        !verification.passed(),
        "the same microseconds with and without a zone are not the same value"
    );
}

#[test]
fn a_value_containing_the_framing_bytes_cannot_imitate_a_column_boundary() {
    // The length prefix carries its weight only against values that contain the encoder's own
    // framing. Here the two composite keys are different --- `("a\x06\x01", "b")` and
    // `("a", "\x06\x01b")` --- and without a length prefix they concatenate to exactly the
    // same bytes, because the second key's payload reproduces the boundary the first key's
    // encoding writes. Two distinct rows would then share a primary key.
    let schema = Arc::new(Schema::new(vec![
        Field::new("left", DataType::Utf8, false),
        Field::new("right", DataType::Utf8, false),
    ]));
    let one = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(StringArray::from(vec!["a\u{6}\u{1}"])),
            Arc::new(StringArray::from(vec!["b"])),
        ],
    )
    .unwrap();
    let other = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(vec!["a"])),
            Arc::new(StringArray::from(vec!["\u{6}\u{1}b"])),
        ],
    )
    .unwrap();

    let keys = vec!["left".to_string(), "right".to_string()];
    let mut first = Scan::new(keys.clone(), Vec::new()).unwrap();
    first.absorb(&one).unwrap();
    let mut second = Scan::new(keys, Vec::new()).unwrap();
    second.absorb(&other).unwrap();

    let verification = compare(1, &first.finish(), &second.finish());
    assert!(
        verification
            .discrepancies
            .iter()
            .any(|d| matches!(d, Discrepancy::KeySet { .. })),
        "{:?}",
        verification.discrepancies
    );
}

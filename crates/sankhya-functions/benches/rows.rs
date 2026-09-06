//! What borrowing a row instead of copying it is actually worth.
//!
//! # Why this exists
//!
//! `PERF-01`, `PERF-02`. `ADR-0020` Decision 7 published a table --- 24.9× at width 8, 7.2× at
//! 64, 2.1× at 512 --- and `rows.rs` restated it, and `STATUS.md` restated it again. **No code
//! anywhere in the repository's history produced those numbers.** They existed only in prose,
//! and the audit that reconstructed them faithfully measured 2.2× where the table said 24.9×.
//!
//! The tell was internal. Against a fresh run the ADR's *copying* arm was 2.4× faster and its
//! *borrowing* arm 27× faster; a smaller dataset would have scaled both together. Only the fast
//! arm was anomalous, it was non-monotone in width, and 0.85 ms for a scalar reduction over that
//! data implies about 39 GB/s --- above this machine's memory bandwidth. The borrowing arm was
//! almost certainly deleted by the optimiser: its result was unused, so LLVM removed the loop,
//! while the copying arm survived because heap allocation has side effects.
//!
//! That is the classic benchmark failure, and the rule the same ADR states --- *"every claim
//! about speed carries its number"* --- is what made it survive: the number was there, so it
//! read as measured.
//!
//! # What this does about it
//!
//! Both arms consume their result through `black_box`, so neither can be deleted. Both allocate
//! their input once, outside the timed region. The arms differ in exactly one thing: whether a
//! row is a borrowed slice or a fresh `Vec`.

// A benchmark chooses all of its own data, exactly as a test does. The workspace denies
// `unwrap`, `expect`, `panic` and unchecked indexing because a *server* must not do those
// things to data it did not choose; a benchmark that cannot fail loudly on a fixture it built
// itself would report a number about the wrong thing.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use arrow_array::{ArrayRef, FixedSizeListArray, Float64Array};
use arrow_schema::{DataType, Field};
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use sankhya_functions::rows::Vectors;
use std::sync::Arc;

/// A million doubles, whatever the width --- so the arms compare the same amount of work.
const VALUES: usize = 1 << 20;

/// A column of `rows` vectors of `width` doubles.
fn column(width: usize) -> ArrayRef {
    let rows = VALUES / width;
    #[allow(clippy::cast_precision_loss)]
    let flat: Vec<f64> = (0..rows * width).map(|i| (i % 97) as f64).collect();
    let child = Arc::new(Float64Array::from(flat));
    let field = Arc::new(Field::new("item", DataType::Float64, true));
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    let list = FixedSizeListArray::new(field, width as i32, child, None);
    Arc::new(list)
}

/// Sum every row, borrowing each one.
fn borrowing(array: &ArrayRef, rows: usize) -> f64 {
    let mut reader = Vectors::read(array, "bench").expect("a column of doubles");
    let mut total = 0.0;
    for row in 0..rows {
        if let Some(values) = reader.row(row) {
            total += values.iter().sum::<f64>();
        }
    }
    total
}

/// Sum every row, copying each one first --- what every wrapper here used to do.
fn copying(array: &ArrayRef, rows: usize, width: usize) -> f64 {
    let list = array
        .as_any()
        .downcast_ref::<FixedSizeListArray>()
        .expect("a fixed-size list");
    let mut total = 0.0;
    for row in 0..rows {
        // `value(row)` builds an `Arc` and the values were then copied into a `Vec<f64>`,
        // per row per argument. Reproduced exactly, because a reconstruction that is kinder
        // than the original measures something nobody shipped.
        let one = list.value(row);
        let doubles = one
            .as_any()
            .downcast_ref::<Float64Array>()
            .expect("a child of doubles");
        let owned: Vec<f64> = doubles.values().iter().copied().take(width).collect();
        total += owned.iter().sum::<f64>();
    }
    total
}

fn a_row_borrowed_against_a_row_copied(c: &mut Criterion) {
    let mut group = c.benchmark_group("row-access");
    for width in [8usize, 64, 512] {
        let rows = VALUES / width;
        let array = column(width);
        // The same number of doubles at every width, so the ratio is about access and not
        // about how much data each arm touched.
        group.throughput(Throughput::Elements(VALUES as u64));

        group.bench_with_input(BenchmarkId::new("borrowing", width), &width, |b, _| {
            // `black_box` on the *result*, which is the half the published table lost: an
            // unused sum is a loop the optimiser is entitled to delete, and deleting it is
            // how a scalar reduction comes to imply 39 GB/s.
            b.iter(|| black_box(borrowing(black_box(&array), black_box(rows))));
        });
        group.bench_with_input(BenchmarkId::new("copying", width), &width, |b, _| {
            b.iter(|| black_box(copying(black_box(&array), black_box(rows), black_box(width))));
        });
    }
    group.finish();
}

criterion_group!(benches, a_row_borrowed_against_a_row_copied);
criterion_main!(benches);

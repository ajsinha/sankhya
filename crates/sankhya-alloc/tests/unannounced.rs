//! A program that installed no counting allocator reports nothing, rather than zero.
//!
//! Its own test binary, because announcement is once per process and cannot be undone: a
//! test asserting the un-announced state has to run somewhere nothing has announced. The
//! server's metrics endpoint depends on exactly this --- it is compiled into the binary and
//! into the integration tests that drive it, and only the binary installs an allocator, so a
//! `None` here is what keeps the two memory gauges from reporting a zero that reads like a
//! measurement.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

#[test]
fn nothing_is_reported_when_nothing_was_announced() {
    // A real allocation first, so the assertion cannot pass merely because the process has
    // been idle. Whatever this costs, an un-announced program still has no figure to give.
    let block = vec![0u8; 1024 * 1024];
    assert!(!block.is_empty());

    assert_eq!(
        sankhya_alloc::in_use(),
        None,
        "no program announced an allocator here"
    );
    assert_eq!(sankhya_alloc::peak(), None, "and so there is no peak either");
}

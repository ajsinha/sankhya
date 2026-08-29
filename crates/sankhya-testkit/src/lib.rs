//! Deterministic fakes and fault injectors for every seam.
//!
//! **Empty by intent, and dated.** No fault-injection infrastructure exists in this workspace
//! today, which is precisely why the concurrency defects found on 2026-08-28 went unseen by
//! seventeen hundred tests: **every test had a single writer**, and nothing could make two
//! arrive in the same microsecond on purpose.
//!
//! **Scheduled: M8 §12.1e.** A race that cannot be provoked deterministically is tested by
//! luck, and a test that passes by luck is indistinguishable from one that passes because the
//! code is right. This crate is what turns "we could not reproduce it" into a fixture: fail a
//! `link`, delay a `rename`, stall a reader between resolving a file and opening it.
//!
//! **What it must not become.** A place for helpers that build fixtures the product could build
//! itself. The golden rule stands --- no server functionality in test code --- and a testkit is
//! the most tempting place to break it, because breaking it there looks like sharing.

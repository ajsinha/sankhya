//! The maintenance command line.
//!
//! **Empty by intent, and dated.** The server maintains itself: compaction, retention, orphan
//! sweeping and cuboid refresh all run on the warehouse's own thread, and no operator action is
//! required for any of it. That was a deliberate decision --- *"the server maintains itself
//! automatically; tests are just tests"* --- and it is why this crate is not urgent.
//!
//! **Scheduled: M8.** What it is for is the work that is *not* automatic: inspecting what
//! maintenance decided and why, forcing a pass ahead of its schedule, and reporting what a
//! sweep would remove before it removes it.
//!
//! **What it must not become.** A second way to write to a warehouse. `sankhya-publish` and
//! `sankhya-maintenance` are the only writers, enforced by `cargo xtask check-writers`, and a
//! CLI that grew its own write path would be the third --- which is the defect that rule exists
//! to prevent, arriving through a door marked "operations".

fn main() {
    eprintln!(
        "sankhya-cli is not built yet: the server maintains itself, and this is scheduled for \
         M8. See docs/IMPLEMENTATION_PLAN.md §12."
    );
    std::process::exit(2);
}

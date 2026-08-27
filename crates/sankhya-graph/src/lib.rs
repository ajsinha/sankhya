//! Graph hydration, epochs, and the traversal service.
//!
//! The graph tier holds no durable state. Every epoch is **derived** --- built by scanning
//! published tables, bound to the snapshot it was built from, and thrown away on shutdown.
//! There is no graph write path, no graph transaction log and no way for the graph to
//! disagree with SQL, because it has no independent state to disagree from. An edge exists
//! because a row exists.
//!
//! That is a deliberate constraint rather than an unfinished feature. A second durable
//! store would need its own consistency story with the first, and the reconciliation
//! between them is exactly the problem this system exists to remove.
//!
//! # What an epoch guarantees
//!
//! - **Immutable.** Queries borrow the adjacency arrays without a lock, which is only sound
//!   because nothing can modify them. A rebuild produces a new epoch and swaps the pointer.
//! - **Identified.** Every epoch carries the snapshot it was built from and when, so a
//!   graph result can be reconciled with a relational one taken at a different moment.
//! - **Bounded.** A hydration that will not fit its declared budget fails while it is still
//!   a build job, with a message saying how large it was getting.

#![doc(html_root_url = "https://docs.rs/sankhya-graph")]

pub mod epoch;
pub mod hydrate;
pub mod overlay;
pub mod spec;

pub use epoch::{Epoch, EpochId, EpochRef, EpochSlot, Footprint, NotHydrated};
pub use hydrate::{Hydration, HydrationError, MemoryBudget};
pub use overlay::{Overlay, RebuildThreshold};
pub use spec::{EdgeSpec, GraphSpec};

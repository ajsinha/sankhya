//! Read modes, session tokens, and snapshot pinning with bounded leases.

#![doc(html_root_url = "https://docs.rs/sankhya-session")]

pub mod lease;
pub mod mode;

pub use lease::{Lease, LeaseError, Leases};
pub use mode::{ReadMode, TooStale};

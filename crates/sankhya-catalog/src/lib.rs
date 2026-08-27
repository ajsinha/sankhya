//! Table resolution and policy rewrite. The single security choke point.
//!
//! Cache keys live here rather than with the caches they index, because what makes a key
//! correct is a security property rather than a caching one: a key that omits the
//! entitlement set does not return a stale answer, it returns **someone else's**. See
//! [`key`].

#![doc(html_root_url = "https://docs.rs/sankhya-catalog")]

pub mod key;

pub use key::{Entitlements, PlanKey, ResultKey};

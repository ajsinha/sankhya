//! Table resolution and policy rewrite. The single security choke point.
//!
//! Cache keys live here rather than with the caches they index, because what makes a key
//! correct is a security property rather than a caching one: a key that omits the
//! entitlement set does not return a stale answer, it returns **someone else's**. See
//! [`key`].
//!
//! [`guard`] is what makes "choke point" a property of the type system rather than a
//! description of an intention. A [`Guard`] cannot be constructed except from an allowed
//! policy decision, so anything requiring one in its signature cannot be called without a
//! decision having been made. The usual failure is not a wrong policy but a code path that
//! never consulted one, and no amount of care in the policy component helps if it was never
//! called.

#![doc(html_root_url = "https://docs.rs/sankhya-catalog")]

pub mod guard;
pub mod key;
pub mod mask;
pub mod secured;

pub use guard::Guard;
pub use key::{Entitlements, PlanKey, ResultKey};
pub use mask::{Masked, Masking};
pub use secured::{assert_filter_present, SecuredTable};

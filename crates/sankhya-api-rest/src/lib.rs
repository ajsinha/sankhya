//! A REST gateway, and the thing it refuses to be.
//!
//! `FR-API-05` asks for *a thin REST gateway over the control plane for administration and
//! small ad-hoc queries*. `FR-API-06` says what it must not become:
//!
//! > Bulk data SHALL NOT be offered over JSON. Result size on the REST surface SHALL be
//! > hard-capped, returning a Flight ticket for anything larger.
//!
//! The second requirement is the interesting one, and its reason is a product reason rather
//! than an operational one — see [`size`]. The cap is not there to protect the server. It is
//! there so that the convenient surface does not become the measured one.

#![doc(html_root_url = "https://docs.rs/sankhya-api-rest")]

pub mod plane;
pub mod size;

pub use plane::{route, Route, Shape, ABSENT, ROUTES};
pub use size::{deliver, Budget, Delivery, Estimate, MAX_BYTES, MAX_ROWS};

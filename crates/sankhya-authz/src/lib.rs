//! Authentication and authorization.
//!
//! Two components with deliberately different characters.
//!
//! [`principal`] is about *identity*: one type, established once at the edge, carried
//! unchanged everywhere. There is no second construction path, so the notion of who is
//! asking cannot drift between layers.
//!
//! [`policy`] is about *decisions*, and it is **pure**. Every decision is a function of the
//! policy set and the principal --- no clock, no network, no database, no ambient state.
//! That is what makes it exhaustively testable, and this is the component that needs
//! exhaustive testing more than any other: a defect here is not a wrong answer, it is one
//! tenant reading another's data.
//!
//! # The two conservative rules
//!
//! Absence is denial, and denial wins. A principal with no rule granting access is refused,
//! so a policy set that fails to load grants nothing rather than everything. And an explicit
//! denial beats any number of grants, so an exclusion is expressible at all.
//!
//! # What is not a rule
//!
//! The tenant boundary. A rule is data, and data can be wrong or missing; the tenant check
//! runs before any rule is consulted and is not expressible as one. Everything else in this
//! system is configurable, and that deliberately is not.

#![doc(html_root_url = "https://docs.rs/sankhya-authz")]

pub mod policy;
pub mod principal;

pub use policy::{Action, Decision, DenialReason, Effect, Mask, PolicySet, Rule, TableRef};
pub use principal::{storage_prefix, Authentication, InvalidPrincipal, Principal, Role, TenantId};

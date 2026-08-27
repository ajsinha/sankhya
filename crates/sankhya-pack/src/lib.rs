//! The declarative pack tier.
//!
//! Most of a real domain pack is naming and composition, not novel computation: giving an
//! organisation's vocabulary to a threshold, a ratio, a comparison. Requiring a compiled
//! crate for that means a rebuild and a redeploy to change a number, which in practice
//! means the number does not get changed.
//!
//! A bundle is a TOML file. It declares functions as expressions and graph queries as
//! preset traversals, is validated at load, and can be replaced while the server runs.
//!
//! # Safe by construction, not by supervision
//!
//! The expression language has no loop, no recursion, no call and no I/O. Every expression
//! terminates in time proportional to its own text, which is fixed when the bundle loads.
//! A declarative function therefore cannot be the one that hangs a query --- the sandbox
//! exists for the compiled tier.
//!
//! That restriction is the point. An expression language with loops is a programming
//! language, and a programming language loaded from a configuration file is a remote code
//! execution feature with extra steps.

#![doc(html_root_url = "https://docs.rs/sankhya-pack")]

pub mod bundle;
pub mod expr;
pub mod loader;
pub mod parse;
pub mod verify;

pub use bundle::{Bundle, BundleError, DeclaredFunction, DeclaredGraphQuery, Validated};
pub use expr::{EvalError, Expr};
pub use loader::{Loaded, Loader, ReloadOutcome};
pub use parse::{parse, ParseError};
pub use verify::{Digest, PinnedDigests, Trust, TrustEverything, Verifier};

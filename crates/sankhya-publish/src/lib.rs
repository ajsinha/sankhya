//! The supported path for writing an external table, and for checking one written by
//! anything else.
//!
//! # Open to read, tooled to write
//!
//! The table format is open and documented, and external engines read it directly with no
//! process of this system involved. That is a constraint on *reading*, and the asymmetry
//! with writing is deliberate.
//!
//! A reader that misunderstands the format is wrong for itself, immediately and
//! recoverably. A writer that misunderstands it corrupts the table for everyone,
//! permanently, and usually undetectably --- because the writer's own reader shares the
//! misunderstanding and is perfectly happy.
//!
//! This system has direct evidence. Writing its own format with the specification open, it
//! omitted a non-nullable field from every `add` action. Its own reader accepted the result
//! without complaint, because a reader ignores a field it never writes. An independent
//! implementation rejected it on the first read.
//!
//! If that happens to a team writing the format on purpose, with tests around them, it will
//! happen to a team writing it under deadline as a means to an end.
//!
//! # What this offers instead of instructions
//!
//! [`publish`] makes the invariants unavoidable: there is no way to call it that produces
//! an invalid table, a file without statistics, or a schema that does not round-trip.
//!
//! [`verify`] does not assume it was used. Making a library the supported path is a
//! recommendation, and a recommendation is not an invariant --- so a table's log can be
//! checked, and the check reports *what* is wrong rather than *whether*, and distinguishes
//! a finding that makes queries slow from one that makes them wrong.

#![doc(html_root_url = "https://docs.rs/sankhya-publish")]

pub mod class;
pub mod publish;
pub mod verify;

pub use class::{configuration, key_columns, TableClass, CLASS_KEY, KEY_COLUMNS_KEY};
pub use publish::{is_table, publish_table, Publication, PublishError, Published};
pub use verify::{verify, Finding, Report};

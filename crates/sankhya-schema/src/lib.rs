//! The logical schema model and the type mapping.
//!
//! # The rule this crate exists to enforce
//!
//! **Lossless or rejected.** A source type is mapped only when the mapping round-trips
//! exactly. Anything else is refused at onboarding, loudly, with the column named.
//!
//! The alternative — mapping approximately and carrying on — is far worse than it
//! sounds. An unconstrained numeric silently truncated to a fixed precision, or a
//! timestamp quietly shifted by a timezone assumption, produces data that looks
//! entirely correct and reconciles against nothing. The defect is discovered years
//! later by someone comparing two reports, and by then the original is gone.
//!
//! So the supported set is explicitly enumerated, and it is deliberately smaller than
//! the set of types a source can express.

#![doc(html_root_url = "https://docs.rs/sankhya-schema")]

mod mapping;
mod naming;
mod onboard;
mod model;

pub use mapping::{MappingError, TypeMapping, map_source_type, numeric_modifier};
pub use model::{Field, LogicalSchema, LogicalType, Precision};
pub use naming::{
    CollisionCheck, NameClass, NamingError, PathSegment, RESERVED_SEGMENTS, TableLocation,
    segment_for,
};
pub use onboard::{
    Onboarded, OnboardingError, OnboardingWarning, WriteStrategy, inexact_columns,
    is_fully_exact, may_be_stored_out_of_line, onboard_relation,
};

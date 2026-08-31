//! What a clone commits when it is created.
//!
//! # A clone's log is its own writes and nothing else
//!
//! `ADR-0016`'s Decision 1a: the clone records an origin and a version, and a read splices the
//! origin's live set *at that version* with the clone's own log. So creating a clone commits a
//! table with lineage properties and **no `Add` actions at all** --- there is nothing to add,
//! because the rows it starts with are the origin's and stay where they are.
//!
//! That is what makes the clone constant-time and constant-space, and it is also what makes the
//! reclamation question answerable: the origin never has to be told which of its files somebody
//! else is naming, because nobody else names them.
//!
//! # Why the schema is copied rather than referenced
//!
//! A clone diverges. The first `ALTER` on either side would otherwise change both, which is the
//! opposite of what a clone is for --- a scratch copy exists precisely so that somebody can do
//! something to it that must not touch production. The schema string is taken from the origin at
//! the cloned version and written into the clone's own metadata, after which the two are
//! independent.

use crate::lineage::Lineage;
use sankhya_table_delta::{create, Action, Metadata};

/// The actions that create a clone.
///
/// `schema_string` is the origin's schema **at the cloned version**, passed in rather than read
/// here for the same reason [`Metadata::new`] takes one: the schema is owned by the type
/// mapping, and duplicating that translation is how the two drift apart.
#[must_use]
pub fn clone_table(
    id: impl Into<String>,
    schema_string: impl Into<String>,
    created_time: i64,
    lineage: &Lineage,
) -> Vec<Action> {
    let mut metadata = Metadata::new(id, schema_string, created_time);
    metadata.configuration.extend(lineage.to_properties());
    create(metadata)
}

/// Whether a set of actions creates a clone rather than an ordinary table.
///
/// Reads the lineage back out of the metadata, so a caller that has the actions and not the
/// table can tell. `None` for an ordinary table; the error case is a table claiming a lineage it
/// does not describe, which [`Lineage::from_properties`] refuses rather than ignoring.
#[must_use]
pub fn lineage_of(actions: &[Action]) -> Option<Result<Lineage, crate::lineage::Malformed>> {
    actions.iter().find_map(|action| match action {
        Action::Metadata(metadata) => Lineage::from_properties(&metadata.configuration),
        _ => None,
    })
}

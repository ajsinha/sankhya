//! The definition's version, which nobody has to remember to bump.
//!
//! # Why derived
//!
//! `FR-QUERY-20` requires a materialised cuboid to be keyed by *(definition version,
//! snapshot, cuboid)*, so that a query against changed inputs **misses** rather than
//! returning something stale. There is no invalidation protocol to get wrong, which is the
//! whole appeal --- but only if the version genuinely moves when the definition does.
//!
//! A declared version does not. It is a field somebody edits, a review that has to catch it
//! when they do not, and a silent wrong answer served from a cuboid built under the old
//! meaning of a measure. The failure is invisible: the number is a real number, computed
//! correctly, from a definition that no longer exists.
//!
//! So the version is a fingerprint of the validated content. Change a measure's rule and
//! the key changes; reformat the file and it does not.
//!
//! # What it is not
//!
//! **Not cryptographic.** This is `FNV-1a`, and it defends against accident, not against
//! somebody constructing a second definition with a colliding fingerprint. That attack buys
//! a stale read of a cube the attacker must already be able to redefine, which is a larger
//! problem than the one it causes. It is written down here because a hash in a cache key
//! invites the assumption, and the assumption is wrong.

use crate::model::Definition;

/// `FNV-1a`, 64-bit.
const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const PRIME: u64 = 0x0000_0100_0000_01b3;

/// A fingerprint of everything about a definition that changes what it means.
///
/// Order matters where it is meaningful --- levels are coarse-to-fine, and reordering them
/// reorders a drill-down --- so nothing is sorted before hashing. The declaration order of
/// dimensions and measures is included too: it is cheap, and a fingerprint that ignores a
/// change is worse than one that moves for a change nobody cares about.
#[must_use]
pub fn fingerprint(definition: &Definition) -> u64 {
    let mut h = OFFSET;
    feed(&mut h, definition.name.as_bytes());
    feed(&mut h, definition.fact_table.as_bytes());
    // What the facts are read from, not only how they were written. A query's text does not
    // say which tables it reads --- the same text resolves differently under a different
    // search path --- and a cube reading a different table is a different cube.
    for table in &definition.reads {
        feed(&mut h, table.as_bytes());
    }

    for dimension in &definition.dimensions {
        feed(&mut h, dimension.name.as_bytes());
        feed(&mut h, dimension.table.as_bytes());
        feed(&mut h, dimension.joins_on.as_bytes());
        for level in &dimension.levels {
            feed(&mut h, level.name.as_bytes());
            feed(&mut h, level.column.as_bytes());
        }
        if let Some((child, parent)) = &dimension.parent_child {
            feed(&mut h, child.as_bytes());
            feed(&mut h, parent.as_bytes());
        }
        if let Some(rollups) = &dimension.rollups {
            // Edges in a stable order: a hierarchy is a set of edges, and two definitions
            // declaring the same roll-ups in a different order mean the same thing.
            for member in rollups.members() {
                feed(&mut h, member.as_bytes());
                for child in rollups.children_of(member) {
                    feed(&mut h, child.as_bytes());
                }
            }
        }
    }

    for measure in &definition.measures {
        feed(&mut h, measure.name.as_bytes());
        for rule in &measure.rules {
            feed(&mut h, rule.dimension.as_bytes());
            feed(&mut h, rule.rule.as_str().as_bytes());
            // Which aggregation, not merely that there is one.
            //
            // `Rule::as_str` returns the constant `"an aggregation of your own"` for every
            // supplied rule, because the name lives beside the rule rather than inside it (see
            // `Along::supplied`). Without this, two cubes differing only in which declared
            // function a measure calls fingerprint identically --- and this fingerprint is the
            // invalidation key for both the cuboid store and the hydration cache, so one
            // cube's cells are served under the other's name.
            if let Some(supplied) = &rule.supplied {
                feed(&mut h, supplied.as_bytes());
            }
        }
    }
    h
}

/// One field, length-prefixed.
///
/// The length matters. Without it a dimension named `ab` with a column `c` fingerprints
/// identically to one named `a` with a column `bc`, and two different cubes share a
/// materialisation key.
fn feed(h: &mut u64, bytes: &[u8]) {
    for byte in (bytes.len() as u64).to_le_bytes() {
        *h = (*h ^ u64::from(byte)).wrapping_mul(PRIME);
    }
    for byte in bytes {
        *h = (*h ^ u64::from(*byte)).wrapping_mul(PRIME);
    }
}

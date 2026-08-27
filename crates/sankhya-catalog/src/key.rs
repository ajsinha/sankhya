//! Cache keys, and the two things that must never be left out of one.
//!
//! # Why a cache key is a security boundary
//!
//! A cache returns a previous answer when the key matches. So a key that omits something
//! the answer depended on does not produce a stale result — it produces **someone else's
//! result**, correctly and quickly.
//!
//! Two omissions have that character, and both are easy to make because neither appears
//! in the query text:
//!
//! - **The entitlement set.** Two users run the same SQL against the same data and are
//!   entitled to different rows. A result cached under the query alone serves the first
//!   user's rows to the second. This is not a leak through a bug; it is the cache
//!   working exactly as designed on a key that was wrong.
//! - **The policy version.** Row and column policies are rewritten into the plan. A plan
//!   cached before a policy tightened is still a valid plan — for the old policy — and
//!   reusing it silently un-applies the change for everyone whose plan was already
//!   cached.
//!
//! # Why they are constructor arguments rather than fields
//!
//! Both are required to build a key, so there is no partially-specified key to forget to
//! finish. A field can be left at its default; an argument has to be passed. The type
//! system is doing the remembering, because the failure mode is invisible and the review
//! that catches it has to notice an absence rather than a mistake.
//!
//! # Why the hash is defined here
//!
//! Keys must be identical across processes and machines, or a cache shared between nodes
//! silently misses everything and a cache *checked* across nodes disagrees about what it
//! holds. The default hasher is randomly seeded per process, deliberately, which makes
//! it exactly wrong for this.

use std::collections::BTreeSet;
use std::fmt;

/// A stable, process-independent hash.
///
/// FNV-1a with a finalising mix. Not cryptographic and not required to be: this
/// identifies a cache entry, it does not authenticate one. What it must be is the same
/// number everywhere, forever.
fn mix(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        h ^= u64::from(*byte);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let mut z = h.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Combine several components into one key.
///
/// Each component is length-prefixed, so `("ab", "c")` and `("a", "bc")` do not collide.
/// Without that, two different queries with the same concatenated bytes share a cache
/// entry — which is the same failure as omitting a component, arrived at differently.
fn combine(parts: &[&[u8]]) -> u64 {
    let mut buffer = Vec::new();
    for part in parts {
        buffer.extend_from_slice(&(part.len() as u64).to_be_bytes());
        buffer.extend_from_slice(part);
    }
    mix(&buffer)
}

/// Who is asking, expressed as what they may see.
///
/// A set rather than a user identity: two users with identical entitlements may share a
/// cached result, and caching per user would throw that away for nothing. It is the
/// entitlements that determine the answer, so it is the entitlements that belong in the
/// key.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Entitlements {
    /// Sorted and deduplicated, so two callers presenting the same grants in a different
    /// order produce the same key rather than missing the cache.
    grants: BTreeSet<String>,
}

impl Entitlements {
    #[must_use]
    pub fn new<I, S>(grants: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            grants: grants.into_iter().map(Into::into).collect(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.grants.is_empty()
    }

    fn fingerprint(&self) -> u64 {
        let parts: Vec<&[u8]> = self.grants.iter().map(|g| g.as_bytes()).collect();
        combine(&parts)
    }
}

/// Identifies a cached *plan*.
///
/// A plan is the result of compiling SQL against a schema under a policy. All three go
/// in, and the policy version is the one that is easy to forget because it is nowhere in
/// the query.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct PlanKey(u64);

impl PlanKey {
    /// Build a plan key.
    ///
    /// `policy_version` advances whenever any row or column policy changes. A plan
    /// cached under an old version is a plan for the old policy, and reusing it
    /// un-applies the change for everyone whose plan was already cached.
    #[must_use]
    pub fn new(sql: &str, schema_version: u64, policy_version: u64) -> Self {
        Self(combine(&[
            b"plan",
            sql.as_bytes(),
            &schema_version.to_be_bytes(),
            &policy_version.to_be_bytes(),
        ]))
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for PlanKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "plan:{:016x}", self.0)
    }
}

/// Identifies a cached *result*.
///
/// Everything in the plan key, plus the entitlements that decided which rows survived
/// and the data version the answer was computed over.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ResultKey(u64);

impl ResultKey {
    /// Build a result key.
    ///
    /// `snapshot_version` is what makes invalidation free: a new version is a new key,
    /// so nothing has to be found and evicted when the data changes.
    #[must_use]
    pub fn new(plan: PlanKey, entitlements: &Entitlements, snapshot_version: u64) -> Self {
        Self(combine(&[
            b"result",
            &plan.get().to_be_bytes(),
            &entitlements.fingerprint().to_be_bytes(),
            &snapshot_version.to_be_bytes(),
        ]))
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for ResultKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "result:{:016x}", self.0)
    }
}

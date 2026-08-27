//! Session tokens, and read-your-own-writes.
//!
//! # The requirement that saves the first demonstration
//!
//! `FR-API-13` is unusually direct about why this exists: *without this the first
//! demonstration anyone attempts shows their own write missing, and they will reasonably
//! conclude the system is broken.*
//!
//! The situation is structural rather than a defect. A write commits to the transactional
//! store; capture, apply and publication follow. An analytical query issued a moment later
//! reads a published state that does not include it yet. Everything worked correctly and
//! the user's row is not there.
//!
//! So a write returns a token carrying its commit position, and passing that token to a
//! later query makes the query **wait** for that position --- bounded by its own deadline
//! --- rather than answering without it.
//!
//! # Why the token is opaque
//!
//! `FR-API-16` requires it, and the reason is a version-skew one. A token exposing a
//! format-specific version identifier makes any later change to that format a breaking wire
//! change: clients would be parsing it, comparing it, and storing it. An opaque token can be
//! reissued in a new format whenever the server likes, because nothing outside the server
//! has ever looked inside one.
//!
//! It is opaque, not secret. Anyone holding it can read a position they already caused.

use std::fmt;

/// A position in the change stream.
///
/// The unit the applier advances through. A query told to wait for one waits until
/// publication has covered it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct CommitPosition(pub u64);

/// An opaque handle to a commit position, as handed to a client.
///
/// The inner representation is deliberately not public and deliberately not parseable. A
/// client that could read a version number out of this would start depending on it, and the
/// next format change would be a breaking one.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SessionToken(String);

impl SessionToken {
    /// Issue a token for a commit position.
    ///
    /// The encoding is intentionally uninteresting --- a prefix and a number. What matters
    /// is that it is *this crate's* business: nothing outside may construct one from parts
    /// or read the position back out of the text.
    #[must_use]
    pub fn issue(position: CommitPosition) -> Self {
        Self(format!("skhy1-{}", position.0))
    }

    /// The token as the opaque string a client holds.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Read a token back.
    ///
    /// Returns `None` for anything this server did not issue. Deliberately silent about
    /// *why*: a caller who could learn that the prefix was wrong, or that the number did
    /// not parse, would have learned the format.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let rest = text.strip_prefix("skhy1-")?;
        rest.parse::<u64>().ok()?;
        Some(Self(text.to_string()))
    }

    /// The position this token names.
    ///
    /// Crate-internal in spirit: it is public because the query path needs it, but nothing
    /// in a wire format should ever carry the result of this.
    #[must_use]
    pub fn position(&self) -> Option<CommitPosition> {
        self.0
            .strip_prefix("skhy1-")?
            .parse::<u64>()
            .ok()
            .map(CommitPosition)
    }
}

impl fmt::Display for SessionToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a request asks of the session.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SessionRequest {
    /// How current the answer must be.
    pub mode: crate::mode::ReadMode,
    /// A token whose write must be visible, if the caller supplied one.
    pub after: Option<SessionToken>,
}

/// A session, and what it will and will not agree to.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Session {
    pinned: Option<u64>,
}

impl Session {
    /// A session with nothing pinned.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Pin this session to a snapshot.
    #[must_use]
    pub const fn pinned_to(snapshot: u64) -> Self {
        Self {
            pinned: Some(snapshot),
        }
    }

    /// The snapshot this session is pinned to, if any.
    #[must_use]
    pub const fn pin(&self) -> Option<u64> {
        self.pinned
    }

    /// Resolve what a request actually means in this session.
    ///
    /// `FR-API-14`: requesting maximum freshness inside a pinned session is a
    /// **contradiction**, and is rejected rather than silently reconciled. Silently
    /// reconciling would mean picking one of the two, and whichever is picked, some caller
    /// gets the opposite of what they asked for without being told.
    pub fn resolve(&self, request: &SessionRequest) -> Result<Resolved, Contradiction> {
        let Some(pinned) = self.pinned else {
            return Ok(Resolved {
                mode: request.mode,
                wait_for: request.after.as_ref().and_then(SessionToken::position),
            });
        };

        match request.mode {
            crate::mode::ReadMode::Strong => Err(Contradiction::FreshnessInsidePin {
                pinned,
                asked_for: "strong consistency",
            }),
            crate::mode::ReadMode::BoundedFreshness { .. } => {
                Err(Contradiction::FreshnessInsidePin {
                    pinned,
                    asked_for: "bounded freshness",
                })
            }
            crate::mode::ReadMode::Pinned { snapshot } if snapshot != pinned => {
                Err(Contradiction::DifferentPin {
                    session_pin: pinned,
                    requested: snapshot,
                })
            }
            crate::mode::ReadMode::Pinned { .. } => Ok(Resolved {
                mode: crate::mode::ReadMode::Pinned { snapshot: pinned },
                // A pinned session reads committed data only, so there is nothing to wait
                // for: the pinned snapshot either includes the write or predates it, and
                // waiting cannot change which.
                wait_for: None,
            }),
        }
    }
}

/// What a request resolved to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Resolved {
    /// The mode to read in.
    pub mode: crate::mode::ReadMode,
    /// A position publication must have covered before answering.
    pub wait_for: Option<CommitPosition>,
}

/// A request that cannot mean what it says.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Contradiction {
    /// Freshness was requested inside a pinned session.
    FreshnessInsidePin {
        /// What the session is pinned to.
        pinned: u64,
        /// What was asked for.
        asked_for: &'static str,
    },
    /// A different snapshot was requested inside a pinned session.
    DifferentPin {
        /// What the session is pinned to.
        session_pin: u64,
        /// What the request named.
        requested: u64,
    },
}

impl fmt::Display for Contradiction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FreshnessInsidePin { pinned, asked_for } => write!(
                f,
                "this session is pinned to snapshot {pinned} and the request asks for \
                 {asked_for}. These cannot both be honoured, and reconciling them silently \
                 would give one of the two callers the opposite of what they asked for \
                 without telling them. Unpin the session or drop the freshness requirement"
            ),
            Self::DifferentPin {
                session_pin,
                requested,
            } => write!(
                f,
                "this session is pinned to snapshot {session_pin} and the request asks for \
                 {requested}. Re-pin the session rather than overriding it per request, or \
                 repeatable reads are not repeatable"
            ),
        }
    }
}

impl std::error::Error for Contradiction {}

/// Whether publication has reached the position a token names.
///
/// A pure decision, so the waiting itself belongs to the caller who owns the deadline.
#[must_use]
pub const fn is_visible(published_through: CommitPosition, wanted: CommitPosition) -> bool {
    published_through.0 >= wanted.0
}

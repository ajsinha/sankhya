//! Stopping before the operating system does.
//!
//! # Why the brake is set below the real limit
//!
//! An out-of-memory kill is not a degradation. The process dies, every other query dies
//! with it, and where the transactional store is supervised by the same process the
//! database dies too — an outage rather than a slow afternoon.
//!
//! So the brake fires while there is still headroom, and the headroom is the whole
//! design: by the time the operating system is involved there is no decision left to
//! make. **Shedding a query is a choice; being killed is not.**
//!
//! The number it reads comes from a counting allocator, because the engine's own pool
//! does not see every allocation. See the `sankhya-alloc` crate.

use std::fmt;

/// Where the brake fires.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BrakeLimits {
    /// Above this, work is shed.
    ///
    /// Set below the machine's real limit on purpose. By the time the operating system
    /// is involved there is no decision left to make.
    pub shed_bytes: usize,
    /// Above this, the system is close enough to warn about.
    pub warn_bytes: usize,
}

/// What the brake says.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pressure {
    /// Carry on.
    Clear,
    /// Close to the limit. Nothing is refused yet.
    Warning { in_use: usize, warn_at: usize },
    /// Over the limit. Shed work now.
    Shed { in_use: usize, shed_at: usize },
}

impl Pressure {
    #[must_use]
    pub const fn shedding(self) -> bool {
        matches!(self, Self::Shed { .. })
    }
}

impl fmt::Display for Pressure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Clear => f.write_str("clear"),
            Self::Warning { in_use, warn_at } => {
                write!(f, "{in_use} bytes in use, warning at {warn_at}")
            }
            Self::Shed { in_use, shed_at } => write!(
                f,
                "{in_use} bytes in use and the brake is at {shed_at}; shedding work now \
                 rather than waiting to be killed, because being killed takes every \
                 other query with it"
            ),
        }
    }
}

/// Read the current total against the limits.
#[must_use]
pub fn assess_memory(in_use: usize, limits: &BrakeLimits) -> Pressure {
    if in_use >= limits.shed_bytes {
        return Pressure::Shed {
            in_use,
            shed_at: limits.shed_bytes,
        };
    }
    if in_use >= limits.warn_bytes {
        return Pressure::Warning {
            in_use,
            warn_at: limits.warn_bytes,
        };
    }
    Pressure::Clear
}

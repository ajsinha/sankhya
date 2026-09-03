//! What a run may spend.

use std::time::Duration;

/// The bounds one run is held to.
///
/// # Why every one of these is mandatory
///
/// There is no `Bounds::unlimited`, and that is the point. `ADR-0010` states the requirement in
/// one line --- *an aggregation that never returns is an outage rather than an error* --- and a
/// default of "no limit" is how a field that exists comes to be unset everywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bounds {
    /// How long it may run in wall-clock time, enforced by the parent.
    ///
    /// Wall clock **and** CPU, because they catch different things: a function computing
    /// forever burns CPU, and a function sleeping forever does not.
    pub wall: Duration,
    /// How many seconds of CPU it may burn, enforced by `RLIMIT_CPU`.
    pub cpu: u64,
    /// How much address space it may map, enforced by `RLIMIT_AS`.
    pub memory: u64,
    /// How many bytes it may write back.
    ///
    /// A separate bound from memory, and needed separately: a function returning a gigabyte per
    /// batch is a denial of service with no loop in it and no allocation the limit above would
    /// catch, because the memory is the *parent's*.
    pub output: usize,
}

impl Bounds {
    /// The defaults a declaration inherits when it says nothing.
    ///
    /// Deliberately small. A function that needs more says so, and saying so is the moment
    /// somebody notices what it is going to cost.
    #[must_use]
    pub fn modest() -> Self {
        Self {
            wall: Duration::from_secs(5),
            cpu: 5,
            memory: 512 * 1024 * 1024,
            output: 64 * 1024 * 1024,
        }
    }
}

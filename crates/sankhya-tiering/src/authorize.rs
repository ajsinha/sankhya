//! The only two ways a purge can begin.
//!
//! # Why this type exists rather than a boolean
//!
//! `FR-TIER-03`: *"the purge state machine's entry point requires an authorization value whose
//! only constructors are the command path and the schedule evaluator. **Enumerating those
//! constructors SHALL constitute a complete audit of every way data can leave the system of
//! record.**"*
//!
//! That last sentence is the whole design. It is a claim about what somebody can learn by
//! reading, and it is only true if the type cannot be built any other way --- so `Origin` is
//! private, there is no `Default`, no `new`, and no way to construct one from parts. The two
//! functions below are the complete list, and `grep` finding them is the audit.
//!
//! `FR-TIER-02` is what it enforces: tiering happens **only** by explicit operator command or
//! by a named, enabled, change-controlled schedule, and *"no maintenance, retention,
//! compaction, vacuum or expiry job may originate a purge"*. A boolean parameter would let any
//! of those pass `true`, and the audit would then be a search of every call site rather than of
//! two constructors.
//!
//! This is the same shape as `Guard` in `sankhya-catalog`, which cannot be constructed except
//! from an allowed policy decision. The pattern is reused deliberately: it is the one mechanism
//! in this system that makes "was this checked?" a question the compiler answers.

use std::fmt;

/// Where a purge came from.
///
/// Deliberately private. A public enum would let anything construct the variant it wanted,
/// which is the audit property gone.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Origin {
    /// A person ran the command.
    Command {
        /// Who.
        principal: String,
        /// The change-management reference `FR-TIER-27` requires of a non-interactive run.
        change_reference: String,
    },
    /// A named schedule fired.
    Schedule {
        /// Which schedule.
        schedule: String,
        /// The service principal it runs as.
        service_principal: String,
        /// The person who defined this version of the schedule.
        ///
        /// `FR-TIER-34`: audit records name the service principal **and** the human definer
        /// and approver. *"The scheduler did it" is not an acceptable audit answer.*
        definer: String,
        /// The person who approved it.
        approver: String,
    },
}

/// Permission for one purge, and the record of where it came from.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Authorization {
    origin: Origin,
}

impl Authorization {
    /// **Constructor one of two:** an operator ran the command.
    ///
    /// `FR-TIER-27` requires a change-management reference for a non-interactive invocation,
    /// and it is taken here rather than checked later because a purge that reaches the machine
    /// without one has already been authorised by something.
    pub fn from_command(
        principal: impl Into<String>,
        change_reference: impl Into<String>,
    ) -> Self {
        Self {
            origin: Origin::Command {
                principal: principal.into(),
                change_reference: change_reference.into(),
            },
        }
    }

    /// **Constructor two of two:** a named, enabled schedule fired.
    ///
    /// Takes the definer and approver as well as the service principal, because `FR-TIER-34`
    /// requires the audit record to name the people. A schedule that cannot say who approved
    /// it cannot produce one of these.
    pub fn from_schedule(
        schedule: impl Into<String>,
        service_principal: impl Into<String>,
        definer: impl Into<String>,
        approver: impl Into<String>,
    ) -> Self {
        Self {
            origin: Origin::Schedule {
                schedule: schedule.into(),
                service_principal: service_principal.into(),
                definer: definer.into(),
                approver: approver.into(),
            },
        }
    }

    /// Whether a person triggered this directly.
    ///
    /// `FR-TIER-31` recommends scheduled runs stop before purge and leave the purge to a
    /// person, so the state machine has to be able to tell the two apart.
    #[must_use]
    pub const fn is_interactive(&self) -> bool {
        matches!(self.origin, Origin::Command { .. })
    }

    /// Every person and principal this purge is attributable to.
    ///
    /// Returned as pairs so an audit record can carry them all without this module knowing
    /// what an audit record looks like.
    #[must_use]
    pub fn attribution(&self) -> Vec<(&'static str, &str)> {
        match &self.origin {
            Origin::Command { principal, change_reference } => vec![
                ("principal", principal.as_str()),
                ("change_reference", change_reference.as_str()),
            ],
            Origin::Schedule { schedule, service_principal, definer, approver } => vec![
                ("schedule", schedule.as_str()),
                ("service_principal", service_principal.as_str()),
                ("definer", definer.as_str()),
                ("approver", approver.as_str()),
            ],
        }
    }
}

impl fmt::Display for Authorization {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.origin {
            Origin::Command { principal, change_reference } => {
                write!(f, "command by {principal} under {change_reference}")
            }
            Origin::Schedule { schedule, service_principal, definer, approver } => write!(
                f,
                "schedule `{schedule}` as {service_principal}, defined by {definer} and \
                 approved by {approver}"
            ),
        }
    }
}

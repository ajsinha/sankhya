//! Slot state as the source reports it.

use sankhya_types::Lsn;
use std::fmt;

/// The source's own view of how much log a slot is holding.
///
/// These are the database's terms, deliberately preserved rather than renamed, so an
/// operator can correlate what SANKHYA reports with what the database reports.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WalStatus {
    /// Within the configured retention limit.
    Reserved,
    /// Beyond the limit but the log is still present.
    Extended,
    /// Beyond the limit and at risk of removal at the next checkpoint.
    ///
    /// The last state from which recovery is still possible without a re-snapshot.
    Unreserved,
    /// The log has been removed. **The slot cannot be resumed.**
    Lost,
}

impl WalStatus {
    /// Parse the source's textual form. Unknown values are treated as the most
    /// alarming interpretation rather than ignored.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        match text.trim() {
            "reserved" => Self::Reserved,
            "extended" => Self::Extended,
            "unreserved" => Self::Unreserved,
            _ => Self::Lost,
        }
    }

    /// Whether the slot can still be used.
    #[must_use]
    pub const fn is_usable(self) -> bool {
        !matches!(self, Self::Lost)
    }

    /// Whether the source has already begun protecting itself.
    #[must_use]
    pub const fn is_past_limit(self) -> bool {
        matches!(self, Self::Extended | Self::Unreserved | Self::Lost)
    }
}

impl fmt::Display for WalStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Reserved => "reserved",
            Self::Extended => "extended",
            Self::Unreserved => "unreserved",
            Self::Lost => "lost",
        })
    }
}

/// A slot's observed state.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SlotState {
    pub name: String,
    pub active: bool,
    pub status: WalStatus,
    /// How much log the slot is holding back.
    pub retained_bytes: u64,
    /// Where the source is now.
    pub source_position: Lsn,
    /// What the consumer has confirmed durable.
    pub confirmed_position: Lsn,
}

impl SlotState {
    /// Positions between confirmed and current.
    #[must_use]
    pub fn behind_by(&self) -> u64 {
        self.source_position
            .get()
            .saturating_sub(self.confirmed_position.get())
    }

    /// Whether the consumer has caught up entirely.
    #[must_use]
    pub fn is_current(&self) -> bool {
        self.confirmed_position >= self.source_position
    }
}

/// A judgement about a slot, for reporting.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SlotHealth {
    pub state: SlotState,
    pub severity: crate::safety::Severity,
    pub message: String,
}

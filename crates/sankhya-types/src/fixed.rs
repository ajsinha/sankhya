//! Exact fixed-point arithmetic.
//!
//! # Why this exists in the core, and why currency does not
//!
//! Exact decimal arithmetic with explicit scale and checked overflow is domain-free
//! numeric hygiene. Inventory counts, billing quantities, sensor calibration, dosage
//! and monetary amounts all need it for the same reason: binary floating point cannot
//! represent decimal fractions exactly, so accumulating it produces results that do
//! not reconcile.
//!
//! Currency *tagging* — and the prohibition on adding two amounts denominated
//! differently — is domain semantics and belongs in a pack. The core provides the
//! arithmetic; a pack provides the meaning.
//!
//! # Why every operation is checked
//!
//! Silent wraparound in a quantity that reconciles against an external system is
//! among the worst defect classes available: it produces a plausible number. Every
//! operation here returns a `Result`. There is no unchecked variant, because the
//! moment one exists someone will reach for it.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Number of decimal places. Bounded because the underlying representation is 128-bit
/// and a scale beyond this leaves too few digits of magnitude to be useful.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Scale(u8);

impl Scale {
    /// The largest scale representable while retaining useful magnitude.
    pub const MAX: u8 = 28;

    /// Returns `None` above [`Scale::MAX`] rather than clamping: silently reducing a
    /// caller's requested precision is exactly the kind of quiet inaccuracy this
    /// module exists to prevent.
    #[must_use]
    pub const fn new(places: u8) -> Option<Self> {
        if places <= Self::MAX {
            Some(Self(places))
        } else {
            None
        }
    }

    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// What can go wrong in exact arithmetic. Each variant is actionable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FixedError {
    /// The result does not fit the 128-bit representation.
    Overflow,
    /// Operands carry different scales. Rescale explicitly and deliberately.
    ScaleMismatch { left: u8, right: u8 },
    /// Division by zero.
    DivideByZero,
    /// Rescaling would discard non-zero digits.
    PrecisionLoss { from: u8, to: u8 },
}

impl fmt::Display for FixedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Overflow => write!(f, "fixed-point overflow"),
            Self::ScaleMismatch { left, right } => {
                write!(f, "scale mismatch: {left} vs {right}; rescale explicitly")
            }
            Self::DivideByZero => write!(f, "divide by zero"),
            Self::PrecisionLoss { from, to } => {
                write!(f, "rescaling {from} -> {to} would discard non-zero digits")
            }
        }
    }
}

impl std::error::Error for FixedError {}

/// An exact decimal value: a 128-bit integer of minor units plus a scale.
///
/// Two values may be compared or combined only at the same scale. That restriction is
/// deliberate — implicit rescaling is where precision quietly disappears.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fixed {
    units: i128,
    scale: Scale,
}

impl Fixed {
    #[must_use]
    pub const fn from_units(units: i128, scale: Scale) -> Self {
        Self { units, scale }
    }

    #[must_use]
    pub const fn zero(scale: Scale) -> Self {
        Self { units: 0, scale }
    }

    #[must_use]
    pub const fn units(self) -> i128 {
        self.units
    }

    #[must_use]
    pub const fn scale(self) -> Scale {
        self.scale
    }

    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.units == 0
    }

    fn same_scale(self, other: Self) -> Result<(), FixedError> {
        if self.scale == other.scale {
            Ok(())
        } else {
            Err(FixedError::ScaleMismatch {
                left: self.scale.get(),
                right: other.scale.get(),
            })
        }
    }

    /// Checked addition at a common scale.
    pub fn add(self, other: Self) -> Result<Self, FixedError> {
        self.same_scale(other)?;
        self.units
            .checked_add(other.units)
            .map(|units| Self { units, scale: self.scale })
            .ok_or(FixedError::Overflow)
    }

    /// Checked subtraction at a common scale.
    pub fn sub(self, other: Self) -> Result<Self, FixedError> {
        self.same_scale(other)?;
        self.units
            .checked_sub(other.units)
            .map(|units| Self { units, scale: self.scale })
            .ok_or(FixedError::Overflow)
    }

    /// Multiply by a whole number, preserving scale exactly.
    pub fn mul_int(self, factor: i128) -> Result<Self, FixedError> {
        self.units
            .checked_mul(factor)
            .map(|units| Self { units, scale: self.scale })
            .ok_or(FixedError::Overflow)
    }

    /// Negation, checked because the 128-bit minimum has no positive counterpart.
    pub fn neg(self) -> Result<Self, FixedError> {
        self.units
            .checked_neg()
            .map(|units| Self { units, scale: self.scale })
            .ok_or(FixedError::Overflow)
    }

    /// Rescale, refusing to discard non-zero digits.
    ///
    /// Widening always succeeds if it fits. Narrowing succeeds only when the digits
    /// being dropped are all zero — a silent truncation here would be indistinguishable
    /// from a correct result and would not reconcile.
    pub fn rescale(self, to: Scale) -> Result<Self, FixedError> {
        let (from, to_places) = (self.scale.get(), to.get());
        if from == to_places {
            return Ok(self);
        }
        if to_places > from {
            let factor = pow10(to_places - from).ok_or(FixedError::Overflow)?;
            return self
                .units
                .checked_mul(factor)
                .map(|units| Self { units, scale: to })
                .ok_or(FixedError::Overflow);
        }
        let factor = pow10(from - to_places).ok_or(FixedError::Overflow)?;
        if self.units % factor != 0 {
            return Err(FixedError::PrecisionLoss { from, to: to_places });
        }
        Ok(Self { units: self.units / factor, scale: to })
    }

    /// Sum a sequence exactly.
    ///
    /// Deterministic by construction: integer addition is associative, so unlike
    /// floating point the result does not depend on partition or completion order.
    /// This is why reportable quantities use this type rather than a float — the same
    /// query must return the same answer on every run.
    pub fn sum(values: impl IntoIterator<Item = Self>, scale: Scale) -> Result<Self, FixedError> {
        let mut acc = Self::zero(scale);
        for v in values {
            acc = acc.add(v)?;
        }
        Ok(acc)
    }
}

impl PartialOrd for Fixed {
    /// Defined only at a common scale; comparing across scales is a programming error
    /// rather than a question with a sensible answer.
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        (self.scale == other.scale).then(|| self.units.cmp(&other.units))
    }
}

const fn pow10(exp: u8) -> Option<i128> {
    let mut acc: i128 = 1;
    let mut i = 0u8;
    while i < exp {
        match acc.checked_mul(10) {
            Some(v) => acc = v,
            None => return None,
        }
        i += 1;
    }
    Some(acc)
}

impl fmt::Display for Fixed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let places = usize::from(self.scale.get());
        if places == 0 {
            return write!(f, "{}", self.units);
        }
        let negative = self.units < 0;
        let magnitude = self.units.unsigned_abs();
        let Some(divisor) = pow10(self.scale.get()) else {
            return write!(f, "{}e-{}", self.units, places);
        };
        let divisor = divisor.unsigned_abs();
        let whole = magnitude / divisor;
        let frac = magnitude % divisor;
        if negative {
            f.write_str("-")?;
        }
        write!(f, "{whole}.{frac:0places$}")
    }
}

impl fmt::Debug for Fixed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fixed({self}, scale={})", self.scale.get())
    }
}

//! What-if figures, and keeping them distinguishable from the truth.
//!
//! # Never the published data
//!
//! Planning and what-if analysis need somewhere to put a number that is not a fact: a
//! budget, a proposed reorganisation, a stress scenario. `FR-CUBE-19` says that place is a
//! separately versioned **overlay**, and that it never modifies published data. An overlay
//! is applied when a query is answered and discarded afterwards; the files underneath are
//! the same files.
//!
//! # A query states whether one was applied
//!
//! An overlaid figure that reaches a report without saying so is the failure this whole
//! module exists to prevent, and it is a quiet one --- the number is well-formed, the query
//! succeeded, and the scenario it came from is not written anywhere on it.
//!
//! [`Applied`] carries the overlay's **name**, not a flag. "This is a what-if" is not
//! enough when three scenarios are open; the question a reader has is *which one*.
//!
//! # An entry written at a total does not have a value at a leaf
//!
//! This is the part that gets built wrong, and the wrong version is convenient.
//!
//! Somebody edits a total: "assume the northern region does 5 million next quarter". The
//! overlay now holds a figure at the region-and-quarter grain. Then somebody drills into it,
//! and the system has to produce numbers for the branches beneath --- which **the overlay
//! does not contain**.
//!
//! Two honest answers exist: allocate the edit down by a stated rule, or refuse to serve the
//! finer grain. What is not honest is inventing children that happen to sum to the edited
//! parent, or --- worse and more common --- showing the *unadjusted* children under an
//! adjusted parent, so a drill-down silently contradicts the row above it.
//!
//! So an entry records the grain it was written at, and [`Overlay::apply`] refuses a cube
//! finer than that grain unless an [`Allocation`] says how to spread it. The refusal names
//! the entry and both grains.
//!
//! # Bound to the definition it was written against
//!
//! An overlay written when `revenue` summed across time means something different once
//! `revenue` is a closing balance. It carries the definition fingerprint it was authored
//! against and refuses to apply to another --- the same reasoning as the backup manifest's
//! bind, and the same failure if omitted: a plausible number from a model nobody is using.

use crate::cells::{Address, Cells};
use sankhya_cube_algo::measure::Rule;
use sankhya_math::Exact;
use std::collections::BTreeMap;
use std::fmt;

/// What an overlay entry does to a cell.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Adjustment {
    /// Replace the cell's value.
    Set(f64),
    /// Add to it.
    ///
    /// Distinct from `Set` because a delta against an absent cell is still a delta: the
    /// scenario says "five million more than whatever happens", and collapsing that to a
    /// `Set` loses the part the planner meant.
    Delta(f64),
}

/// How an entry written at a coarse grain is spread to a finer one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Allocation {
    /// Refuse to serve a grain finer than the entry was written at.
    ///
    /// The default, and the honest one when nobody has said how to spread the figure.
    Refuse,
    /// Spread in proportion to the underlying values.
    ///
    /// Needs something to be proportional *to*: where the underlying cells are all absent
    /// or sum to zero there is no proportion, and that case is refused rather than divided
    /// equally, because equal division is a different assumption wearing this one's name.
    ProRata,
}

/// One what-if figure.
#[derive(Clone, PartialEq, Debug)]
pub struct Entry {
    /// The cell it applies to, at the grain it was written.
    pub address: Address,
    /// The dimensions that address names, in the cube's order.
    pub grain: Vec<String>,
    /// What it does.
    pub adjustment: Adjustment,
}

/// A separately versioned set of what-if figures.
#[derive(Clone, PartialEq, Debug)]
pub struct Overlay {
    name: String,
    definition: u64,
    entries: BTreeMap<Address, Entry>,
}

impl Overlay {
    /// An empty overlay, bound to the definition it is authored against.
    pub fn named(name: impl Into<String>, definition: u64) -> Self {
        Self {
            name: name.into(),
            definition,
            entries: BTreeMap::new(),
        }
    }

    /// Its name, which travels with every figure it touches.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The definition fingerprint it was authored against.
    #[must_use]
    pub const fn definition(&self) -> u64 {
        self.definition
    }

    /// Record a figure at a grain.
    pub fn record(&mut self, grain: Vec<String>, address: Address, adjustment: Adjustment) {
        self.entries.insert(
            address.clone(),
            Entry { address, grain, adjustment },
        );
    }

    /// The entries, in a canonical order.
    #[must_use]
    pub fn entries(&self) -> Vec<&Entry> {
        self.entries.values().collect()
    }

    /// Whether it holds nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Apply it to a cube.
    ///
    /// # Errors
    /// [`NotApplicable`] when the overlay was authored against a different definition, or
    /// when the cube is finer than an entry's grain and no allocation says how to spread it.
    pub fn apply(
        &self,
        cells: &Cells,
        definition: u64,
        allocation: Allocation,
    ) -> Result<Applied<Cells>, NotApplicable> {
        if definition != self.definition {
            return Err(NotApplicable::DifferentDefinition {
                overlay: self.name.clone(),
                authored_against: self.definition,
                applied_to: definition,
            });
        }

        let dimensions: Vec<String> = cells.dimensions().to_vec();
        let mut out = cells.clone();
        for entry in self.entries.values() {
            if entry.grain == dimensions {
                apply_here(&mut out, entry);
                continue;
            }
            // The cube names dimensions the entry does not: the entry was written at a
            // total and this cube is a drill-down through it.
            let finer: Vec<&String> = dimensions
                .iter()
                .filter(|d| !entry.grain.contains(d))
                .collect();
            if finer.is_empty() {
                // The cube is *coarser* than the entry. Adding a leaf figure into a total
                // it is not part of would double-count, so it is left alone.
                continue;
            }
            match allocation {
                Allocation::Refuse => {
                    return Err(NotApplicable::FinerThanWritten {
                        overlay: self.name.clone(),
                        written_at: entry.grain.clone(),
                        asked_at: dimensions.clone(),
                    })
                }
                Allocation::ProRata => allocate(&mut out, entry, &dimensions, &self.name)?,
            }
        }
        Ok(Applied {
            value: out,
            overlay: Some(self.name.clone()),
        })
    }
}

/// Set or add at one address in a cube of the entry's own grain.
fn apply_here(cells: &mut Cells, entry: &Entry) {
    let existing = cells.get(&entry.address, Rule::Sum);
    let value = match entry.adjustment {
        Adjustment::Set(to) => to,
        // A delta against an absent cell is the delta: the scenario says "this much more
        // than whatever happens", and treating absence as zero here is the one place it is
        // the right reading, because the planner supplied the other half.
        Adjustment::Delta(by) => existing.unwrap_or(0.0) + by,
    };
    let _ = cells.add_reduced(entry.address.clone(), Rule::Sum, Exact::of(&[value]));
}

/// Spread an entry across the cells beneath it, in proportion to what is there.
fn allocate(
    cells: &mut Cells,
    entry: &Entry,
    dimensions: &[String],
    overlay: &str,
) -> Result<(), NotApplicable> {
    // Which axes the entry pins, and to what.
    let pinned: Vec<(usize, &String)> = entry
        .grain
        .iter()
        .filter_map(|name| {
            let axis = dimensions.iter().position(|d| d == name)?;
            let member = entry.address.get(entry.grain.iter().position(|g| g == name)?)?;
            Some((axis, member))
        })
        .collect();

    let beneath: Vec<Address> = cells
        .addresses()
        .filter(|address| {
            pinned
                .iter()
                .all(|(axis, member)| address.get(*axis) == Some(*member))
        })
        .cloned()
        .collect();

    let mut total = 0.0;
    for address in &beneath {
        total += cells.get(address, Rule::Sum).unwrap_or(0.0);
    }
    if beneath.is_empty() || total == 0.0 {
        // Nothing to be proportional to. Dividing equally is a different assumption wearing
        // this one's name, and it would be invisible in the result.
        return Err(NotApplicable::NothingToAllocateAcross {
            overlay: overlay.to_string(),
            written_at: entry.grain.clone(),
        });
    }

    let target = match entry.adjustment {
        Adjustment::Set(to) => to,
        Adjustment::Delta(by) => total + by,
    };
    for address in beneath {
        let share = cells.get(&address, Rule::Sum).unwrap_or(0.0) / total;
        let _ = cells.add_reduced(address, Rule::Sum, Exact::of(&[target * share]));
    }
    Ok(())
}

/// Why an overlay was not applied.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum NotApplicable {
    /// It was authored against a different cube definition.
    DifferentDefinition {
        /// Which overlay.
        overlay: String,
        /// The definition fingerprint it was written against.
        authored_against: u64,
        /// The one it was offered.
        applied_to: u64,
    },
    /// The cube is finer than an entry was written at, and nothing said how to spread it.
    FinerThanWritten {
        /// Which overlay.
        overlay: String,
        /// The grain the entry was written at.
        written_at: Vec<String>,
        /// The grain asked for.
        asked_at: Vec<String>,
    },
    /// There was nothing beneath the entry to be proportional to.
    NothingToAllocateAcross {
        /// Which overlay.
        overlay: String,
        /// The grain the entry was written at.
        written_at: Vec<String>,
    },
}

impl fmt::Display for NotApplicable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DifferentDefinition { overlay, authored_against, applied_to } => write!(
                f,
                "overlay '{overlay}' was written against cube definition {authored_against:016x} \
                 and this query reads {applied_to:016x}; a measure that has changed how it \
                 aggregates makes the same figure mean something else"
            ),
            Self::FinerThanWritten { overlay, written_at, asked_at } => write!(
                f,
                "overlay '{}' holds a figure at ({}) and this query asks for ({}); the \
                 overlay does not contain those cells, and showing the unadjusted ones \
                 beneath an adjusted total would make a drill-down contradict the row above \
                 it. State an allocation, or ask at the grain it was written",
                overlay,
                written_at.join(", "),
                asked_at.join(", ")
            ),
            Self::NothingToAllocateAcross { overlay, written_at } => write!(
                f,
                "overlay '{}' holds a figure at ({}) and there is nothing beneath it to be \
                 proportional to; dividing equally is a different assumption, and it would \
                 not be visible in the result",
                overlay,
                written_at.join(", ")
            ),
        }
    }
}

impl std::error::Error for NotApplicable {}

/// A value, and which overlay --- if any --- produced it.
///
/// The name rather than a flag: "this is a what-if" does not answer the question a reader
/// has when three scenarios are open.
#[derive(Clone, PartialEq, Debug)]
pub struct Applied<T> {
    value: T,
    overlay: Option<String>,
}

impl<T> Applied<T> {
    /// A value no overlay touched.
    #[must_use]
    pub const fn published(value: T) -> Self {
        Self { value, overlay: None }
    }

    /// The overlay that produced it, or `None` for published data.
    #[must_use]
    pub fn overlay(&self) -> Option<&str> {
        self.overlay.as_deref()
    }

    /// Whether any overlay was applied.
    #[must_use]
    pub const fn is_what_if(&self) -> bool {
        self.overlay.is_some()
    }

    /// The value, only if no overlay was applied.
    ///
    /// # Errors
    /// The overlay's name. Anything reconciled against published figures should come through
    /// here, because a what-if reconciles to nothing.
    pub fn published_only(&self) -> Result<&T, &str> {
        match &self.overlay {
            None => Ok(&self.value),
            Some(name) => Err(name),
        }
    }

    /// The value, whichever it is.
    ///
    /// Named so that reading it is a decision, and so a reviewer can find every place that
    /// made it. A caller using this must carry [`Applied::overlay`] alongside.
    #[must_use]
    pub const fn regardless(&self) -> &T {
        &self.value
    }
}

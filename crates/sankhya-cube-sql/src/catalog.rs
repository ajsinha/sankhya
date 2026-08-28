//! The named cubes a query may navigate.
//!
//! A SQL statement names a cube the way it names a table --- by a string --- and something
//! has to turn that string into a validated cube and the cells currently published for it.
//!
//! Resolution distinguishes three states, because the caller's response differs for each:
//! the name is unknown (a query bug, fix the statement), the cube exists but nothing is
//! published yet (wait and retry), or here it is. Collapsing the first two into an empty
//! result makes a typo indistinguishable from a cube that genuinely has no data, and those
//! are answers somebody acts on differently.

use sankhya_cube::cells::Cells;
use sankhya_cube::complete::Completeness;
use sankhya_cube::model::Cube;
use sankhya_cube::overlay::Overlay;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

/// A cube and the cells published for it at a snapshot.
#[derive(Clone, Debug)]
pub struct Published {
    /// The validated definition.
    pub cube: Arc<Cube>,
    /// The cells, at the finest grain published.
    pub cells: Arc<Cells>,
    /// **Which measure these cells hold.**
    ///
    /// `Cells` is a map from address to contributions and carries no measure of its own, so
    /// a set of cells is the values of exactly one measure --- whichever one hydration was
    /// given. Nothing tied that to the measure a query later names.
    ///
    /// The consequence was a wrong number rather than an error: hydrate for `amount`, ask
    /// for `closing_balance`, and the rule resolved from the definition (`Last`) was applied
    /// to amount's values. Right shape, right magnitude, no complaint anywhere. It is
    /// unreachable only while nothing serves cubes, and becomes reachable the moment
    /// something does.
    ///
    /// Recorded here so the mismatch is refused by name. Serving several measures means a
    /// `Published` per *(cube, measure)*, which is the next change and not this one.
    pub measure: String,
    /// The snapshot they were read at.
    ///
    /// Half of the materialisation key, and the half a query result must carry so a cube
    /// figure can be reconciled with a relational one taken at a different moment.
    pub snapshot: u64,
    /// How much of the fact table reached these cells.
    ///
    /// Supplied by whatever hydrated them, and **not derivable from `cells`**. A row that
    /// hydration could not place, or that policy withheld, leaves no trace: counting what
    /// arrived and dividing by what arrived gives one, always. The first version of the
    /// query surface did exactly that and reported every result complete --- the trap
    /// `sankhya_cube::complete` documents, walked into one crate away from the warning.
    pub completeness: Completeness,
}

/// Every cube a session can navigate, and every overlay it may apply.
///
/// # Why the key is *(cube, measure)* and not the cube
///
/// [`Cells`] is a map from address to contributions and carries no measure of its own, so a
/// published set of cells is the values of exactly **one** measure --- whichever hydration was
/// given. Keyed by cube alone, publishing a second measure replaced the first, and a query
/// naming either got whatever was published last with its own rule applied to it.
///
/// That is a wrong number rather than an error: `Last` over `amount` has the shape and
/// magnitude of a closing balance. Keying by the pair means a cube can hold every measure it
/// declares at once, and a measure that was never hydrated is a missing entry rather than
/// somebody else's values.
#[derive(Debug, Default)]
pub struct CubeCatalog {
    cubes: parking_lot::RwLock<BTreeMap<(String, String), Published>>,
    /// Cubes known by name, whether or not anything is published for them.
    ///
    /// Kept separately so "declared but not loaded" stays distinguishable from "no such
    /// cube" --- one is a cube awaiting its first hydration, the other is a typo, and telling
    /// somebody the wrong one sends them to the wrong place.
    declared: parking_lot::RwLock<std::collections::BTreeSet<String>>,
    overlays: parking_lot::RwLock<BTreeMap<String, Arc<Overlay>>>,
}

impl CubeCatalog {
    /// An empty catalog.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare a cube by name with nothing published yet.
    ///
    /// Separate from [`CubeCatalog::publish`] so that "declared but not loaded" is a state
    /// the catalog can be in and report, rather than one that looks like a missing name.
    pub fn declare(&self, name: impl Into<String>) {
        self.declared.write().insert(name.into());
    }

    /// Publish one measure's cells for a cube.
    ///
    /// The measure comes from `published` rather than being a parameter, so the cells and the
    /// name they are filed under cannot disagree.
    pub fn publish(&self, name: impl Into<String>, published: Published) {
        let name = name.into();
        self.declared.write().insert(name.clone());
        let measure = published.measure.clone();
        self.cubes.write().insert((name, measure), published);
    }

    /// Register an overlay a query may name.
    pub fn register_overlay(&self, overlay: Arc<Overlay>) {
        self.overlays
            .write()
            .insert(overlay.name().to_string(), overlay);
    }

    /// What is published for a named cube.
    ///
    /// # Errors
    /// [`Unresolved`], distinguishing an unknown name from a cube awaiting its first load.
    pub fn resolve(&self, name: &str, measure: &str) -> Result<Published, Unresolved> {
        let cubes = self.cubes.read();
        if let Some(published) = cubes.get(&(name.to_string(), measure.to_string())) {
            return Ok(published.clone());
        }
        if !self.declared.read().contains(name) {
            return Err(Unresolved::NoSuchCube {
                name: name.to_string(),
                known: self.declared.read().iter().cloned().collect(),
            });
        }
        let published: Vec<String> = cubes
            .keys()
            .filter(|(cube, _)| cube == name)
            .map(|(_, measure)| measure.clone())
            .collect();
        // Nothing at all published is a *wait*: the cube is declared and its first hydration
        // has not happened. Some measures published but not this one is a different fact and
        // a different action --- one is retried, the other is corrected or loaded.
        if published.is_empty() {
            return Err(Unresolved::NothingPublished {
                name: name.to_string(),
            });
        }
        Err(Unresolved::MeasureNotPublished {
            name: name.to_string(),
            measure: measure.to_string(),
            published,
        })
    }

    /// An overlay by name.
    ///
    /// # Errors
    /// [`Unresolved::NoSuchOverlay`]. A named scenario that silently does not apply gives a
    /// published figure under a what-if's label, which is the one confusion the overlay
    /// machinery exists to prevent.
    pub fn overlay(&self, name: &str) -> Result<Arc<Overlay>, Unresolved> {
        let overlays = self.overlays.read();
        overlays.get(name).map(Arc::clone).ok_or_else(|| {
            Unresolved::NoSuchOverlay {
                name: name.to_string(),
                known: overlays.keys().cloned().collect(),
            }
        })
    }

    /// The cube names known, in order.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.declared.read().iter().cloned().collect()
    }

    /// Which measures of a cube are published.
    #[must_use]
    pub fn published_measures(&self, name: &str) -> Vec<String> {
        self.cubes
            .read()
            .keys()
            .filter(|(cube, _)| cube == name)
            .map(|(_, measure)| measure.clone())
            .collect()
    }
}

/// Why a name did not resolve.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Unresolved {
    /// No cube of that name is registered.
    NoSuchCube {
        /// The name asked for.
        name: String,
        /// The names that exist, so a typo is one glance away from being found.
        known: Vec<String>,
    },
    /// The cube exists, and nothing has been published for it yet.
    NothingPublished {
        /// The name.
        name: String,
    },
    /// The cube exists and *this measure* of it has not been hydrated.
    ///
    /// Distinct from a cube nobody has loaded, and distinct from a measure the definition
    /// does not declare. Collapsing the three would send somebody to fix a typo that is not
    /// there, or to wait for a load that is never coming.
    MeasureNotPublished {
        /// The cube.
        name: String,
        /// The measure asked for.
        measure: String,
        /// The measures of this cube that *are* published.
        published: Vec<String>,
    },
    /// No overlay of that name is registered.
    NoSuchOverlay {
        /// The name asked for.
        name: String,
        /// The overlays that exist.
        known: Vec<String>,
    },
}

impl fmt::Display for Unresolved {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSuchCube { name, known } => write!(
                f,
                "no cube named '{}' — this session knows {}. Refused rather than answered \
                 with no rows, because a typo and a cube with no data are not the same fact",
                name,
                if known.is_empty() {
                    "none".to_string()
                } else {
                    known.join(", ")
                }
            ),
            Self::MeasureNotPublished { name, measure, published } => write!(
                f,
                "cube '{}' has no published cells for measure '{}' — it has {}. Cells hold \
                 one measure's values, so answering from another measure's would apply this \
                 rule to the wrong column and give a number of about the right size",
                name,
                measure,
                if published.is_empty() {
                    "none yet".to_string()
                } else {
                    published.join(", ")
                }
            ),
            Self::NothingPublished { name } => write!(
                f,
                "cube '{name}' is declared but nothing has been published for it yet; this \
                 is a wait-and-retry, not a statement to correct"
            ),
            Self::NoSuchOverlay { name, known } => write!(
                f,
                "no overlay named '{}' — this session knows {}. A named scenario that \
                 silently did not apply would put a published figure under a what-if's label",
                name,
                if known.is_empty() {
                    "none".to_string()
                } else {
                    known.join(", ")
                }
            ),
        }
    }
}

impl std::error::Error for Unresolved {}

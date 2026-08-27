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
    /// The snapshot they were read at.
    ///
    /// Half of the materialisation key, and the half a query result must carry so a cube
    /// figure can be reconciled with a relational one taken at a different moment.
    pub snapshot: u64,
}

/// Every cube a session can navigate, by name, and every overlay it may apply.
#[derive(Debug, Default)]
pub struct CubeCatalog {
    cubes: parking_lot::RwLock<BTreeMap<String, Option<Published>>>,
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
        self.cubes.write().entry(name.into()).or_insert(None);
    }

    /// Publish cells for a cube.
    pub fn publish(&self, name: impl Into<String>, published: Published) {
        self.cubes.write().insert(name.into(), Some(published));
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
    pub fn resolve(&self, name: &str) -> Result<Published, Unresolved> {
        let cubes = self.cubes.read();
        match cubes.get(name) {
            None => Err(Unresolved::NoSuchCube {
                name: name.to_string(),
                known: cubes.keys().cloned().collect(),
            }),
            Some(None) => Err(Unresolved::NothingPublished {
                name: name.to_string(),
            }),
            Some(Some(published)) => Ok(published.clone()),
        }
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
        self.cubes.read().keys().cloned().collect()
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

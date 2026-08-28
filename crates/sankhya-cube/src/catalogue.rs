//! Cube definitions that outlive the process that declared them.
//!
//! # Why a cube could not be saved
//!
//! A cube was *"registered against a session by the embedding application"* --- which meant
//! it lived exactly as long as the process, and a server could not serve one. That was not an
//! omission in the storage layer; there was no storage layer to omit it from, because
//! [`Measure`] held `&'static str` and `&'static [Along]`. A measure was a **compile-time**
//! construct, so a definition could name only measures a Rust source file had already spelled
//! out, and no amount of persistence code could have loaded one from disk.
//!
//! Those are owned now. This module is the second half: a definition is written beside the
//! tables it describes, and read back.
//!
//! # Why the stored form is its own type
//!
//! [`Stored`] mirrors [`Definition`] rather than deriving `Serialize` on it. Two reasons, and
//! the second is the one that matters.
//!
//! [`Measure`] and [`Hierarchy`] live in `sankhya-cube-algo`, which has **zero dependencies**
//! --- that is what makes its property tests fast enough to exhaust rather than sample, and
//! `serde` would end it.
//!
//! And a stored definition is a **format**, not a struct. Anything that derives its
//! serialisation from an internal type has promised that the internal type will not be
//! refactored, which is a promise nobody remembers making until a rename silently orphans
//! every cube on disk. The conversion here is explicit and the field names are chosen, so a
//! refactor breaks a compile rather than a warehouse.
//!
//! # Where it goes, and why not a table
//!
//! `<warehouse>/_cubes/<name>.json`. Under `_`, which the orphan sweep and every table
//! discovery path already skip: a cube definition is not data and must never be mistaken for
//! a table. The obvious alternative --- a definition *is* a row in a system table, so the
//! store is the warehouse and the machinery exists --- is the better long-run answer and
//! needs the catalogue to exist first. This is deliberately the smaller thing.

use crate::model::{Definition, Dimension, Level};
use sankhya_cube_algo::hierarchy::Hierarchy;
use sankhya_cube_algo::measure::{Along, Measure, Rule};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The directory holding cube definitions under a warehouse.
const CUBES: &str = "_cubes";

/// Why a definition could not be stored or read back.
#[derive(Debug)]
pub enum CatalogueError {
    /// The definition could not be written, or the directory could not be made.
    Write { name: String, detail: String },
    /// A file exists and is not a definition this version understands.
    ///
    /// Never silently skipped. A cube that vanishes from a catalogue because its file did
    /// not parse is a cube whose queries start failing with "no such cube", and the reason
    /// is in a file nobody thought to open.
    Unreadable { path: PathBuf, detail: String },
    /// The stored definition parsed and does not describe a usable cube.
    Invalid { name: String, detail: String },
}

impl std::fmt::Display for CatalogueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Write { name, detail } => {
                write!(f, "the cube `{name}` could not be stored: {detail}")
            }
            Self::Unreadable { path, detail } => write!(
                f,
                "{} is in the cube catalogue and is not a definition ({detail}). It is \
                 reported rather than skipped: a cube that quietly disappears is a query \
                 that fails somewhere else",
                path.display()
            ),
            Self::Invalid { name, detail } => write!(
                f,
                "the stored cube `{name}` does not describe a usable cube: {detail}"
            ),
        }
    }
}

impl std::error::Error for CatalogueError {}

/// A cube definition, in the shape it takes on disk.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Stored {
    /// The format's version, so a future change can be detected rather than guessed at.
    pub format: u32,
    pub name: String,
    pub fact_table: String,
    pub dimensions: Vec<StoredDimension>,
    pub measures: Vec<StoredMeasure>,
    /// How stale materialised cells may be, in table versions.
    ///
    /// Absent for a cube nothing materialises, which is the default and the whole of the
    /// difference between the two persisted lifetimes.
    #[serde(default)]
    pub target_lag: Option<u64>,
}

/// A dimension, on disk.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct StoredDimension {
    pub name: String,
    pub table: String,
    pub joins_on: String,
    pub levels: Vec<StoredLevel>,
    /// Declared roll-up edges as *(child, parent)*, flattened for storage.
    #[serde(default)]
    pub rollups: Vec<(String, String)>,
    #[serde(default)]
    pub parent_child: Option<(String, String)>,
}

/// A level, on disk.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct StoredLevel {
    pub name: String,
    pub column: String,
}

/// A measure and its rule along every dimension, on disk.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct StoredMeasure {
    pub name: String,
    /// Rules as *(dimension, rule)*. Every dimension, with no default --- a missing entry is
    /// rejected on load exactly as it is at definition time, because an implicit `Sum` is the
    /// error this model exists to prevent.
    pub rules: Vec<(String, String)>,
}

/// The format this version writes.
const FORMAT: u32 = 1;

impl Stored {
    /// The stored shape of a definition.
    #[must_use]
    pub fn of(definition: &Definition) -> Self {
        Self {
            format: FORMAT,
            name: definition.name.clone(),
            fact_table: definition.fact_table.clone(),
            target_lag: definition.target_lag,
            dimensions: definition
                .dimensions
                .iter()
                .map(|dimension| StoredDimension {
                    name: dimension.name.clone(),
                    table: dimension.table.clone(),
                    joins_on: dimension.joins_on.clone(),
                    levels: dimension
                        .levels
                        .iter()
                        .map(|level| StoredLevel {
                            name: level.name.clone(),
                            column: level.column.clone(),
                        })
                        .collect(),
                    rollups: dimension
                        .rollups
                        .as_ref()
                        .map(edges_of)
                        .unwrap_or_default(),
                    parent_child: dimension.parent_child.clone(),
                })
                .collect(),
            measures: definition
                .measures
                .iter()
                .map(|measure| StoredMeasure {
                    name: measure.name.clone(),
                    rules: measure
                        .rules
                        .iter()
                        .map(|along| {
                            (along.dimension.clone(), along.rule.as_str().to_string())
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    /// The definition this describes.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogueError::Invalid`] for a rule name this version does not know. An
    /// unknown rule is refused rather than defaulted: a measure whose rule silently became
    /// `Sum` because a file said `Median` is a wrong number presented confidently, which is
    /// the one failure the additivity model exists to prevent.
    pub fn into_definition(self) -> Result<Definition, CatalogueError> {
        let mut dimensions = Vec::new();
        for stored in self.dimensions {
            let levels = stored
                .levels
                .into_iter()
                .map(|level| Level::new(level.name, level.column))
                .collect();
            let rollups = if stored.rollups.is_empty() {
                None
            } else {
                let mut hierarchy = Hierarchy::new();
                for (child, parent) in stored.rollups {
                    hierarchy.rolls_up(child, parent);
                }
                Some(hierarchy)
            };
            dimensions.push(Dimension {
                name: stored.name,
                table: stored.table,
                joins_on: stored.joins_on,
                levels,
                rollups,
                parent_child: stored.parent_child,
            });
        }

        let mut measures = Vec::new();
        for stored in self.measures {
            let mut rules = Vec::new();
            for (dimension, rule) in stored.rules {
                let Some(rule) = rule_named(&rule) else {
                    return Err(CatalogueError::Invalid {
                        name: self.name.clone(),
                        detail: format!(
                            "the measure `{}` declares `{rule}` along `{dimension}`, which is \
                             not a rule this version knows. Refused rather than defaulted: a \
                             rule that quietly becomes Sum is a wrong total nobody queries",
                            stored.name
                        ),
                    });
                };
                rules.push(Along::new(dimension, rule));
            }
            measures.push(Measure::new(stored.name, rules));
        }

        let mut definition = Definition::new(self.name, self.fact_table, dimensions, measures);
        definition.target_lag = self.target_lag;
        Ok(definition)
    }
}

/// The rule a stored name refers to, or `None` if this version does not know it.
fn rule_named(name: &str) -> Option<Rule> {
    [
        Rule::Sum,
        Rule::Last,
        Rule::First,
        Rule::Max,
        Rule::Min,
        Rule::Mean,
        Rule::None,
    ]
    .into_iter()
    .find(|rule| rule.as_str() == name)
}

/// A hierarchy's edges as *(child, parent)*.
fn edges_of(hierarchy: &Hierarchy) -> Vec<(String, String)> {
    // Walked downward, because that is the direction the structure exposes. Each parent
    // names its children, and an edge is one *(child, parent)* pair --- a shared member
    // appears once per parent, which is exactly the plural relationship worth preserving.
    let edges: Vec<(String, String)> = hierarchy
        .members()
        .into_iter()
        .flat_map(|parent| {
            hierarchy
                .children_of(parent)
                .into_iter()
                .map(move |child| (child.to_string(), parent.to_string()))
        })
        .collect();
    // Already ordered, and not sorted again here.
    //
    // `members` and `children_of` both return `BTreeSet`, so the walk above emits edges in a
    // deterministic order whatever order they were declared in --- which is the property that
    // matters: a definition serialising differently on each save produces a diff per write
    // and a fingerprint nobody can compare. A `sort()` here looked like it provided that and
    // provided nothing, which a mutation test showed by surviving its removal.
    edges
}

/// Where a cube's definition lives under a warehouse.
#[must_use]
pub fn path_of(warehouse: &Path, name: &str) -> PathBuf {
    warehouse.join(CUBES).join(format!("{name}.json"))
}

/// Write a definition into the warehouse's cube catalogue.
///
/// # Errors
///
/// Returns [`CatalogueError::Write`] if the directory or the file cannot be written.
pub fn save(warehouse: &Path, definition: &Definition) -> Result<(), CatalogueError> {
    let at = path_of(warehouse, &definition.name);
    if let Some(parent) = at.parent() {
        std::fs::create_dir_all(parent).map_err(|error| CatalogueError::Write {
            name: definition.name.clone(),
            detail: error.to_string(),
        })?;
    }
    let json = serde_json::to_string_pretty(&Stored::of(definition)).map_err(|error| {
        CatalogueError::Write {
            name: definition.name.clone(),
            detail: error.to_string(),
        }
    })?;
    std::fs::write(&at, json).map_err(|error| CatalogueError::Write {
        name: definition.name.clone(),
        detail: error.to_string(),
    })
}

/// Read one definition back.
///
/// # Errors
///
/// [`CatalogueError::Unreadable`] if the file is missing or not parseable,
/// [`CatalogueError::Invalid`] if it parses and does not describe a usable cube.
pub fn load(warehouse: &Path, name: &str) -> Result<Definition, CatalogueError> {
    let at = path_of(warehouse, name);
    let text = std::fs::read_to_string(&at).map_err(|error| CatalogueError::Unreadable {
        path: at.clone(),
        detail: error.to_string(),
    })?;
    let stored: Stored =
        serde_json::from_str(&text).map_err(|error| CatalogueError::Unreadable {
            path: at,
            detail: error.to_string(),
        })?;
    stored.into_definition()
}

/// Every definition in the warehouse's cube catalogue, by name.
///
/// # Errors
///
/// The first file that cannot be read, with its path. **Not** a partial list: a catalogue
/// that returns the cubes it could parse and says nothing about the one it could not is a
/// server that comes up looking healthy and is missing a cube.
pub fn load_all(warehouse: &Path) -> Result<Vec<Definition>, CatalogueError> {
    let directory = warehouse.join(CUBES);
    let Ok(entries) = std::fs::read_dir(&directory) else {
        // No catalogue is not an error. A warehouse with no cubes is the ordinary case.
        return Ok(Vec::new());
    };
    let mut found = Vec::new();
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "json"))
        .collect();
    // Sorted, so a server's cubes come up in the same order on every start and a log naming
    // them is comparable between restarts.
    paths.sort();
    for path in paths {
        let text = std::fs::read_to_string(&path).map_err(|error| CatalogueError::Unreadable {
            path: path.clone(),
            detail: error.to_string(),
        })?;
        let stored: Stored =
            serde_json::from_str(&text).map_err(|error| CatalogueError::Unreadable {
                path: path.clone(),
                detail: error.to_string(),
            })?;
        found.push(stored.into_definition()?);
    }
    Ok(found)
}

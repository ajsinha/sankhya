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
    /// The name may not become part of a path.
    ///
    /// Refused rather than sanitised. A name this rejects is a name somebody can retype; a
    /// name this quietly rewrote would be a cube stored under a name nobody asked for, which
    /// is the same disclosure one level down.
    Name { name: String, detail: String },
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
            Self::Name { name, detail } => {
                write!(f, "`{name}` is not a usable cube name: {detail}")
            }
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
    /// Every table the cube reads.
    ///
    /// Defaulted for a catalogue written before declared queries existed, where the fact
    /// source was always a name and read exactly itself --- see [`Stored::into_definition`],
    /// which fills it in rather than restoring a cube that says it reads nothing.
    #[serde(default)]
    pub reads: Vec<String>,
    pub dimensions: Vec<StoredDimension>,
    pub measures: Vec<StoredMeasure>,
    /// How stale materialised cells may be, in table versions.
    ///
    /// Absent for a cube nothing materialises, which is the default and the whole of the
    /// difference between the two persisted lifetimes.
    #[serde(default)]
    pub target_lag: Option<u64>,
    /// Cuboids the definition pins, as lists of dimension names.
    ///
    /// Defaulted when absent, so a cube written before pinning existed still reads --- an
    /// empty list is exactly "pins nothing", which is what those cubes meant.
    #[serde(default)]
    pub pinned: Vec<Vec<String>>,
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
            reads: definition.reads.clone(),
            target_lag: definition.target_lag,
            pinned: definition.pinned.clone(),
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
                            (along.dimension.clone(), rule_text(along))
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
                // A user's own aggregation carries its name in the same string, because the
                // stored form is a pair and widening it would make every catalogue written
                // before today unreadable by this version. `ADR-0016`'s rule for a format
                // change applies: a new field is a migration, and a new *value* of an existing
                // field is not.
                if let Some(supplied) = along_supplied(&dimension, &rule) {
                    rules.push(supplied);
                    continue;
                }
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
        // A catalogue written before declared queries existed has no `reads`, and the cube it
        // describes read exactly its fact table. Filled in rather than left empty, because an
        // empty dependency list is refused --- and refusing a cube that was valid when it was
        // written would be this change breaking somebody's warehouse on upgrade.
        if !self.reads.is_empty() {
            definition.reads = self.reads.clone();
        }
        definition.target_lag = self.target_lag;
        definition.pinned = self.pinned.clone();
        Ok(definition)
    }
}

/// The rule a stored name refers to, or `None` if this version does not know it.
/// The prefix a user-supplied rule is written under.
///
/// A prefix rather than a new field: the stored form is a pair of strings, and adding a third
/// would make every catalogue written before today unreadable by this version. A new *value* of
/// an existing field costs nothing to read.
const SUPPLIED: &str = "aggregation:";

/// How one rule is written down.
fn rule_text(along: &Along) -> String {
    match (along.rule, &along.supplied) {
        (Rule::Supplied { composes }, Some(name)) => {
            format!("{SUPPLIED}{name}:{}", if composes { "composes" } else { "base" })
        }
        _ => along.rule.as_str().to_string(),
    }
}

/// A user-supplied rule read back, or `None` if this is an ordinary one.
fn along_supplied(dimension: &str, text: &str) -> Option<Along> {
    let rest = text.strip_prefix(SUPPLIED)?;
    let (name, composes) = rest.rsplit_once(':')?;
    Some(Along::by_aggregation(dimension, name, composes == "composes"))
}

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
///
/// Fallible, and that is the point rather than an inconvenience. This used to be
/// `warehouse.join(CUBES).join(format!("{name}.json"))` over a name a statement supplied, and
/// `Path::join` replaces the whole path on an absolute component and honours `..` on a
/// relative one --- so a cube name was a way of writing and deleting files anywhere the
/// server's user could reach. `SEC-06`.
///
/// Returning a `Result` is what stops that coming back: there is no way to obtain the path
/// without having asked.
///
/// # Errors
///
/// [`CatalogueError::Name`] when the name may not become part of a path.
pub fn path_of(warehouse: &Path, name: &str) -> Result<PathBuf, CatalogueError> {
    let checked =
        sankhya_atomicfs::name::checked(name).map_err(|refused| CatalogueError::Name {
            name: name.to_owned(),
            detail: refused.to_string(),
        })?;
    Ok(warehouse.join(CUBES).join(format!("{checked}.json")))
}

/// Write a definition into the warehouse's cube catalogue.
///
/// # Errors
///
/// Returns [`CatalogueError::Write`] if the directory or the file cannot be written.
pub fn save(warehouse: &Path, definition: &Definition) -> Result<(), CatalogueError> {
    let at = path_of(warehouse, &definition.name)?;
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
    // Published, not written. A definition written straight onto its live path is truncated
    // and then rewritten, so a reader loading it mid-save gets a partial file --- and the
    // server adopts cubes at startup while maintenance reads them on every tick.
    sankhya_atomicfs::publish(&at, json.as_bytes()).map_err(|error| CatalogueError::Write {
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
    let at = path_of(warehouse, name)?;
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

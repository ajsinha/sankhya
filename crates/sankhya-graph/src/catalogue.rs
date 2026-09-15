//! Graphs somebody declared, on disk.
//!
//! # Why a graph has to be declared at all
//!
//! The traversal engine is complete and has been for months --- reachability, weighted and
//! *k*-shortest loopless paths, simple cycles, components, centrality, communities,
//! multiplicative influence, each bounded and each reporting its own truncation --- and every
//! `graph_*` call on every startable server answered *"no graph named '…'; known graphs are
//! []"*, under every configuration. `GraphCatalog` was constructed as a **temporary** inside
//! the session builder: the `Arc` was not bound, not stored, and the function runs per
//! session, so every session got a fresh empty map with no handle by which anything could
//! populate it. `register` and `publish` had zero call sites anywhere, tests included.
//!
//! What was missing was not an algorithm. It was a way to say *this table's rows are edges*.
//! That is what a declaration is, and this module is where one lives between restarts.
//!
//! # Why it is a file and not a table
//!
//! The same reason a cube's definition is. A declaration is read at startup, before any
//! session exists and before the query path is available to read a table with --- and a
//! catalogue that cannot be read without the thing it configures is a catalogue that cannot
//! recover a server. It sits beside `_cubes/` for the same reason and in the same shape.
//!
//! # What is deliberately not here
//!
//! No epoch, no adjacency, no vertex. The graph tier holds **no durable state**: an epoch is
//! built by scanning published tables, carries the snapshot it came from, and is dropped on
//! shutdown. Persisting one would be a second store to reconcile with the first, which is the
//! problem this system exists to remove. A declaration says how to rebuild; it never says what
//! was built.

use crate::spec::{EdgeSpec, GraphSpec};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Where declarations live, beside the cubes.
const GRAPHS: &str = "_graphs";

/// How large a hydration may get before it is refused, when the declaration does not say.
///
/// Refused while it is still a build job, with a message saying how large it was getting ---
/// which is the whole point of the budget. Two hundred and fifty-six megabytes holds a few
/// million edges, which is the scale at which somebody notices the choice and states one.
pub const DEFAULT_BUDGET_BYTES: usize = 256 * 1024 * 1024;

/// One edge kind, and the published table its rows come from.
///
/// The table is **here and not in `EdgeSpec`**, deliberately. A spec says which columns mean
/// what; it is matched against whatever batch arrives, and `Hydration::absorb` applies every
/// spec whose columns a batch happens to satisfy. That is the right shape for reading, and it
/// is no use at all for deciding what to scan or what a caller must be allowed to read.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DeclaredEdge {
    /// The published table to read, as a query would name it.
    pub table: String,
    /// Which of its columns supply which part of the edge.
    pub spec: EdgeSpec,
}

/// A graph, as somebody wrote it down.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Declaration {
    /// The name a query traverses it by.
    pub name: String,
    /// Every edge kind, in declaration order.
    pub edges: Vec<DeclaredEdge>,
    /// How large its hydration may get, in bytes.
    pub budget_bytes: usize,
}

impl Declaration {
    /// A declaration over one or more edge kinds.
    #[must_use]
    pub fn new(name: impl Into<String>, edges: Vec<DeclaredEdge>) -> Self {
        Self {
            name: name.into(),
            edges,
            budget_bytes: DEFAULT_BUDGET_BYTES,
        }
    }

    /// The same declaration, with a stated budget.
    #[must_use]
    pub const fn within(mut self, bytes: usize) -> Self {
        self.budget_bytes = bytes;
        self
    }

    /// Check it, and produce a graph.
    ///
    /// Returns **every** rejection rather than the first, for the reason the cube model gives:
    /// a declaration with four bad columns should be fixable in one sitting.
    ///
    /// # Errors
    /// Every way the declaration does not describe something hydratable.
    pub fn validate(self) -> Result<Graph, Vec<Rejection>> {
        let mut out = Vec::new();
        if self.name.trim().is_empty() {
            out.push(Rejection::Blank { what: "graph name", within: self.name.clone() });
        }
        if self.edges.is_empty() {
            out.push(Rejection::NoEdges { graph: self.name.clone() });
        }
        for edge in &self.edges {
            for (what, value) in [
                ("table", edge.table.as_str()),
                ("source column", edge.spec.source_column.as_str()),
                ("target column", edge.spec.target_column.as_str()),
                ("edge type", edge.spec.edge_type.as_str()),
            ] {
                if value.trim().is_empty() {
                    out.push(Rejection::Blank { what, within: self.name.clone() });
                }
            }
        }
        if self.budget_bytes == 0 {
            out.push(Rejection::NoBudget { graph: self.name.clone() });
        }
        if !out.is_empty() {
            return Err(out);
        }
        let spec = self
            .edges
            .iter()
            .fold(GraphSpec::new(), |spec, edge| spec.with(edge.spec.clone()));
        Ok(Graph { declaration: self, spec })
    }
}

/// A validated declaration.
///
/// The only way to hold one is [`Declaration::validate`], so anything taking a `Graph` has no
/// reachable state in which an edge names no table or no column.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Graph {
    declaration: Declaration,
    spec: GraphSpec,
}

impl Graph {
    /// The name a query traverses it by.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.declaration.name
    }

    /// What hydration needs: the edge kinds, without their tables.
    #[must_use]
    pub const fn spec(&self) -> &GraphSpec {
        &self.spec
    }

    /// How large its hydration may get.
    #[must_use]
    pub const fn budget_bytes(&self) -> usize {
        self.declaration.budget_bytes
    }

    /// The declaration it was built from.
    #[must_use]
    pub const fn declaration(&self) -> &Declaration {
        &self.declaration
    }

    /// Every published table this graph reads, each named once, in declaration order.
    ///
    /// What a scan opens and **what a caller must be allowed to read**. A graph is registered
    /// for a principal only where every one of these is readable by them, for the same reason
    /// a cube is: a traversal over edges somebody may not see is a disclosure through
    /// reachability, and an invisible one, because the vertices are real.
    #[must_use]
    pub fn reads(&self) -> Vec<String> {
        let mut seen = BTreeSet::new();
        self.declaration
            .edges
            .iter()
            .map(|edge| edge.table.clone())
            .filter(|table| seen.insert(table.clone()))
            .collect()
    }
}

/// Why a declaration does not describe something hydratable.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Rejection {
    /// A required name is empty.
    Blank {
        /// Which one.
        what: &'static str,
        /// The graph it is in.
        within: String,
    },
    /// The declaration names no edge kind, so it describes no graph.
    NoEdges {
        /// Which graph.
        graph: String,
    },
    /// A budget of zero admits nothing and would refuse its own first batch.
    NoBudget {
        /// Which graph.
        graph: String,
    },
}

impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Blank { what, within } => {
                write!(f, "the {what} is empty in the graph '{within}'")
            }
            Self::NoEdges { graph } => write!(
                f,
                "the graph '{graph}' declares no edges, so it describes no graph. A name with \
                 nothing under it would register, resolve, and answer every traversal with no \
                 rows --- which reads exactly like a traversal that found nothing"
            ),
            Self::NoBudget { graph } => write!(
                f,
                "the graph '{graph}' declares a budget of zero bytes, which refuses its own \
                 first batch. Leave it unstated for the default rather than setting it to none"
            ),
        }
    }
}

impl std::error::Error for Rejection {}

// --- the stored form -------------------------------------------------------------------

/// The format this version writes.
const FORMAT: u32 = 1;

/// A declaration, on disk.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Stored {
    pub format: u32,
    pub name: String,
    pub edges: Vec<StoredEdge>,
    #[serde(default)]
    pub budget_bytes: Option<usize>,
}

/// One edge kind, on disk.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct StoredEdge {
    pub table: String,
    pub source_column: String,
    pub target_column: String,
    pub edge_type: String,
    #[serde(default)]
    pub source_type: Option<String>,
    #[serde(default)]
    pub target_type: Option<String>,
    #[serde(default)]
    pub valid_from_column: Option<String>,
    #[serde(default)]
    pub valid_until_column: Option<String>,
    #[serde(default)]
    pub weight_column: Option<String>,
}

impl Stored {
    /// The stored shape of a declaration.
    #[must_use]
    pub fn of(declaration: &Declaration) -> Self {
        Self {
            format: FORMAT,
            name: declaration.name.clone(),
            budget_bytes: Some(declaration.budget_bytes),
            edges: declaration
                .edges
                .iter()
                .map(|edge| StoredEdge {
                    table: edge.table.clone(),
                    source_column: edge.spec.source_column.clone(),
                    target_column: edge.spec.target_column.clone(),
                    edge_type: edge.spec.edge_type.clone(),
                    source_type: Some(edge.spec.source_type.clone()),
                    target_type: Some(edge.spec.target_type.clone()),
                    valid_from_column: edge.spec.valid_from_column.clone(),
                    valid_until_column: edge.spec.valid_until_column.clone(),
                    weight_column: edge.spec.weight_column.clone(),
                })
                .collect(),
        }
    }

    /// The declaration it stands for.
    #[must_use]
    pub fn into_declaration(self) -> Declaration {
        let edges = self
            .edges
            .into_iter()
            .map(|stored| {
                let mut spec =
                    EdgeSpec::new(stored.source_column, stored.target_column, stored.edge_type);
                spec = spec.between(
                    stored.source_type.unwrap_or_else(|| "vertex".to_string()),
                    stored.target_type.unwrap_or_else(|| "vertex".to_string()),
                );
                spec.valid_from_column = stored.valid_from_column;
                spec.valid_until_column = stored.valid_until_column;
                spec.weight_column = stored.weight_column;
                DeclaredEdge { table: stored.table, spec }
            })
            .collect();
        Declaration {
            name: self.name,
            edges,
            budget_bytes: self.budget_bytes.unwrap_or(DEFAULT_BUDGET_BYTES),
        }
    }
}

/// Why a declaration could not be stored or read back.
#[derive(Debug)]
pub enum CatalogueError {
    /// It could not be written, or the directory could not be made.
    Write {
        /// Which graph.
        name: String,
        /// What went wrong.
        detail: String,
    },
    /// A file exists and is not a declaration this version understands.
    ///
    /// Never silently skipped, for the reason the cube catalogue gives: a graph that vanishes
    /// because its file did not parse is a graph whose queries start failing with "no such
    /// graph", and the reason is in a file nobody thought to open.
    Unreadable {
        /// The file.
        path: PathBuf,
        /// What went wrong.
        detail: String,
    },
    /// The name cannot be a file name.
    Name {
        /// Which graph.
        name: String,
        /// What went wrong.
        detail: String,
    },
}

impl std::fmt::Display for CatalogueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Write { name, detail } => {
                write!(f, "the graph '{name}' could not be written: {detail}")
            }
            Self::Unreadable { path, detail } => write!(
                f,
                "{} is in the graph catalogue and is not a declaration this version \
                 understands: {detail}. Refused rather than skipped --- a graph that vanishes \
                 from a catalogue answers every traversal with 'no such graph'",
                path.display()
            ),
            Self::Name { name, detail } => {
                write!(f, "'{name}' cannot name a graph: {detail}")
            }
        }
    }
}

impl std::error::Error for CatalogueError {}

/// Where a declaration of this name lives.
///
/// # Errors
/// [`CatalogueError::Name`] when the name cannot be a file name.
pub fn path_of(warehouse: &Path, name: &str) -> Result<PathBuf, CatalogueError> {
    let checked = sankhya_atomicfs::name::checked(name).map_err(|refused| CatalogueError::Name {
        name: name.to_owned(),
        detail: refused.to_string(),
    })?;
    Ok(warehouse.join(GRAPHS).join(format!("{checked}.json")))
}

/// Write a declaration into the warehouse's graph catalogue.
///
/// # Errors
/// [`CatalogueError::Write`] if the directory or the file cannot be written.
pub fn save(warehouse: &Path, declaration: &Declaration) -> Result<(), CatalogueError> {
    let at = path_of(warehouse, &declaration.name)?;
    if let Some(parent) = at.parent() {
        std::fs::create_dir_all(parent).map_err(|error| CatalogueError::Write {
            name: declaration.name.clone(),
            detail: error.to_string(),
        })?;
    }
    let json = serde_json::to_string_pretty(&Stored::of(declaration)).map_err(|error| {
        CatalogueError::Write {
            name: declaration.name.clone(),
            detail: error.to_string(),
        }
    })?;
    // Published, not written, for the reason the cube catalogue gives: a file written straight
    // onto its live path is truncated and then rewritten, and the server reads these at
    // startup while anything else may be saving one.
    sankhya_atomicfs::publish(&at, json.as_bytes()).map_err(|error| CatalogueError::Write {
        name: declaration.name.clone(),
        detail: error.to_string(),
    })
}

/// Every declaration in the warehouse, in a stable order.
///
/// # Errors
/// [`CatalogueError::Unreadable`] for a directory that exists and cannot be listed, or a file
/// that is not a declaration. A warehouse with **no** catalogue is not an error: that is the
/// ordinary case, and it is distinguishable from a catalogue that cannot be read.
pub fn load_all(warehouse: &Path) -> Result<Vec<Declaration>, CatalogueError> {
    let directory = warehouse.join(GRAPHS);
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(CatalogueError::Unreadable {
                path: directory,
                detail: error.to_string(),
            })
        }
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "json"))
        .collect();
    // Sorted, so a server's graphs come up in the same order on every start and a log naming
    // them is comparable between restarts.
    paths.sort();

    let mut found = Vec::new();
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
        if stored.format != FORMAT {
            return Err(CatalogueError::Unreadable {
                path,
                detail: format!(
                    "it declares format {} and this version writes {FORMAT}",
                    stored.format
                ),
            });
        }
        found.push(stored.into_declaration());
    }
    Ok(found)
}

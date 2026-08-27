//! The named graphs a query may traverse.
//!
//! A SQL statement names a graph the way it names a table --- by a string --- and something
//! has to turn that string into an epoch. That is all this is: a registry of named slots,
//! each holding whatever is currently published for that graph.
//!
//! Resolution is by name and never by position, and a name that is not registered is a
//! *typed* failure rather than an empty result. A traversal over a graph that does not
//! exist returning no rows is indistinguishable from a traversal that found nothing, and
//! the two mean opposite things.

use sankhya_graph::epoch::{EpochRef, EpochSlot};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Every graph a session can traverse, by name.
#[derive(Debug, Default)]
pub struct GraphCatalog {
    graphs: parking_lot::RwLock<BTreeMap<String, Arc<EpochSlot>>>,
}

impl GraphCatalog {
    /// An empty catalog.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a graph under a name, replacing any slot already there.
    pub fn register(&self, name: impl Into<String>, slot: Arc<EpochSlot>) {
        self.graphs.write().insert(name.into(), slot);
    }

    /// Publish an epoch under a name, creating the slot if needed.
    pub fn publish(&self, name: impl Into<String>, epoch: EpochRef) {
        let name = name.into();
        let mut graphs = self.graphs.write();
        let slot = graphs
            .entry(name)
            .or_insert_with(|| Arc::new(EpochSlot::empty()));
        slot.publish(epoch);
    }

    /// The currently published epoch for a named graph.
    ///
    /// Distinguishes three states deliberately, because the caller's response differs for
    /// each: the name is unknown (a query bug), the graph exists but nothing is hydrated
    /// (wait and retry), or here it is.
    pub fn resolve(&self, name: &str) -> Result<EpochRef, Unresolved> {
        let slot = {
            let graphs = self.graphs.read();
            graphs.get(name).map(Arc::clone)
        };
        let Some(slot) = slot else {
            return Err(Unresolved::NoSuchGraph {
                name: name.to_string(),
                known: self.names(),
            });
        };
        slot.current().ok_or_else(|| Unresolved::NotHydrated {
            name: name.to_string(),
        })
    }

    /// The registered names, sorted.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.graphs.read().keys().cloned().collect()
    }

    /// Whether anything is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.graphs.read().is_empty()
    }
}

/// Why a graph name did not produce an epoch.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Unresolved {
    /// No graph of that name is registered.
    NoSuchGraph {
        /// What was asked for.
        name: String,
        /// What is registered, so the message can suggest.
        known: Vec<String>,
    },
    /// The graph exists but has no epoch published yet.
    NotHydrated {
        /// Which graph.
        name: String,
    },
}

impl std::fmt::Display for Unresolved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSuchGraph { name, known } => write!(
                f,
                "no graph named '{name}' is registered; known graphs are {known:?}. \
                 Refusing rather than returning no rows: an empty traversal over a graph \
                 that does not exist reads exactly like one that found nothing"
            ),
            Self::NotHydrated { name } => write!(
                f,
                "the graph '{name}' is registered but has no epoch published yet: it is \
                 building or has not been hydrated. Wait and retry rather than treating \
                 this as a failed query"
            ),
        }
    }
}

impl std::error::Error for Unresolved {}

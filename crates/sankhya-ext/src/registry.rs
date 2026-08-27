//! What packs have contributed, and the rules about what they may contribute.
//!
//! Registration is where a pack's claims meet everyone else's. Two packs wanting the same
//! function name, or one wanting a name the engine already uses, are conflicts that must be
//! caught here --- at load, with both claimants named --- rather than at query time, where
//! the answer would depend on load order.

use crate::error::PackError;
use crate::function::{Pack, PackInfo, ScalarFunction, TableFunction, API_VERSION};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Names the engine reserves for itself.
///
/// A pack shadowing one of these would change the meaning of an existing query without
/// changing its text, which is the single most dangerous thing an extension mechanism can
/// permit. The adversarial pack attempts exactly this.
const RESERVED_PREFIXES: &[&str] = &["sankhya_", "graph_", "system_", "pg_"];

/// Everything packs have contributed.
#[derive(Debug, Default)]
pub struct Registry {
    packs: Vec<PackInfo>,
    scalars: BTreeMap<String, Registration<Arc<dyn ScalarFunction>>>,
    tables: BTreeMap<String, Registration<Arc<dyn TableFunction>>>,
    rejected: Vec<Rejection>,
    /// Which pack is currently registering, so a contribution can be attributed.
    current: Option<String>,
}

/// One contribution, and which pack made it.
#[derive(Clone, Debug)]
pub struct Registration<T> {
    /// The pack that contributed it.
    pub pack: String,
    /// The contribution.
    pub item: T,
}

/// A contribution that was refused, and why.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Rejection {
    /// Which pack offered it.
    pub pack: String,
    /// What it offered.
    pub name: String,
    /// Why it was refused.
    pub reason: String,
}

impl Registry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Load a pack, letting it register what it has.
    ///
    /// Refuses a pack built against an incompatible API version, naming both, rather than
    /// loading it and failing later inside a query where the cause is no longer visible.
    pub fn load(&mut self, pack: &dyn Pack) -> Result<(), PackError> {
        let info = pack.info();
        if info.api_version != API_VERSION {
            return Err(PackError::forbidden(
                &info.name,
                "load",
                format!(
                    "this pack was built against extension API version {} and this engine \
                     offers version {API_VERSION}. Refusing at load rather than failing \
                     inside a query later, where the cause would no longer be visible",
                    info.api_version
                ),
            ));
        }
        if self.packs.iter().any(|p| p.name == info.name) {
            return Err(PackError::forbidden(
                &info.name,
                "load",
                "a pack of this name is already loaded",
            ));
        }

        self.current = Some(info.name.clone());
        pack.register(self);
        self.current = None;
        self.packs.push(info);
        Ok(())
    }

    /// Offer a scalar function.
    ///
    /// Called by a pack from inside [`Pack::register`]. A refusal is recorded rather than
    /// returned, so one bad contribution does not abort a pack's other, valid ones --- and
    /// the refusals are readable afterwards through [`Registry::rejected`].
    pub fn add_scalar(&mut self, function: Arc<dyn ScalarFunction>) {
        let name = function.name().to_string();
        let pack = self
            .current
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        if let Some(reason) = self.why_not(&name) {
            self.rejected.push(Rejection { pack, name, reason });
            return;
        }
        self.scalars.insert(
            name,
            Registration {
                pack,
                item: function,
            },
        );
    }

    /// Offer a table function.
    pub fn add_table(&mut self, function: Arc<dyn TableFunction>) {
        let name = function.name().to_string();
        let pack = self
            .current
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        if let Some(reason) = self.why_not(&name) {
            self.rejected.push(Rejection { pack, name, reason });
            return;
        }
        self.tables.insert(
            name,
            Registration {
                pack,
                item: function,
            },
        );
    }

    /// Why this name may not be registered, or `None` if it may.
    fn why_not(&self, name: &str) -> Option<String> {
        if name.is_empty() {
            return Some("a function name may not be empty".to_string());
        }
        if let Some(prefix) = RESERVED_PREFIXES.iter().find(|p| name.starts_with(**p)) {
            return Some(format!(
                "'{prefix}' is reserved by the engine. A pack shadowing a core name would \
                 change what an existing query means without changing its text"
            ));
        }
        if let Some(existing) = self.scalars.get(name) {
            return Some(format!(
                "the pack '{}' already registered a scalar function of this name; \
                 resolving it by load order would make the answer depend on start-up",
                existing.pack
            ));
        }
        if let Some(existing) = self.tables.get(name) {
            return Some(format!(
                "the pack '{}' already registered a table function of this name",
                existing.pack
            ));
        }
        None
    }

    /// A scalar function by name.
    #[must_use]
    pub fn scalar(&self, name: &str) -> Option<&Registration<Arc<dyn ScalarFunction>>> {
        self.scalars.get(name)
    }

    /// A table function by name.
    #[must_use]
    pub fn table(&self, name: &str) -> Option<&Registration<Arc<dyn TableFunction>>> {
        self.tables.get(name)
    }

    /// Every loaded pack.
    #[must_use]
    pub fn packs(&self) -> &[PackInfo] {
        &self.packs
    }

    /// Every registered scalar name, sorted.
    #[must_use]
    pub fn scalar_names(&self) -> Vec<&str> {
        self.scalars.keys().map(String::as_str).collect()
    }

    /// Every registered table function name, sorted.
    #[must_use]
    pub fn table_names(&self) -> Vec<&str> {
        self.tables.keys().map(String::as_str).collect()
    }

    /// Everything that was refused, and why.
    ///
    /// Readable rather than logged: a pack whose functions were silently dropped looks
    /// exactly like a pack whose functions do not work, and the operator needs to tell
    /// those apart.
    #[must_use]
    pub fn rejected(&self) -> &[Rejection] {
        &self.rejected
    }
}

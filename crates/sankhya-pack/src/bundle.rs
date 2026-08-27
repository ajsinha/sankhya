//! The bundle format: what a declarative pack file says.

use crate::expr::{result_type, Expr};
use crate::parse::parse;
use sankhya_ext::function::{PackInfo, Signature, API_VERSION};
use sankhya_ext::value::LogicalType;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A declarative pack, as written in TOML.
#[derive(Clone, PartialEq, Debug, Deserialize, Serialize)]
pub struct Bundle {
    /// What this pack is.
    pub pack: Manifest,
    /// Scalar functions declared as expressions.
    #[serde(default)]
    pub function: Vec<DeclaredFunction>,
    /// Graph traversals declared with preset bounds.
    #[serde(default)]
    pub graph_query: Vec<DeclaredGraphQuery>,
}

/// The pack's identity.
#[derive(Clone, PartialEq, Debug, Deserialize, Serialize)]
pub struct Manifest {
    /// Its name. Every function it declares must begin with this and an underscore.
    pub name: String,
    /// Its own version, independent of the engine's.
    pub version: String,
    /// The extension API version it was written against.
    pub api_version: u32,
    /// A sentence about what it is for.
    #[serde(default)]
    pub description: String,
}

/// One scalar function, declared rather than compiled.
#[derive(Clone, PartialEq, Debug, Deserialize, Serialize)]
pub struct DeclaredFunction {
    /// What it is called in SQL.
    pub name: String,
    /// A sentence for whoever reads the function list.
    #[serde(default)]
    pub description: String,
    /// Its arguments, as `name = type` pairs.
    pub arguments: BTreeMap<String, String>,
    /// What it returns.
    pub returns: String,
    /// The expression computing it.
    pub expression: String,
}

/// One graph traversal, declared with its bounds fixed.
///
/// This is the shape most of a real pack takes: not a new algorithm, but an existing one
/// with this organisation's bounds and this organisation's name on it.
#[derive(Clone, PartialEq, Debug, Deserialize, Serialize)]
pub struct DeclaredGraphQuery {
    /// What it is called in SQL.
    pub name: String,
    /// A sentence for whoever reads the function list.
    #[serde(default)]
    pub description: String,
    /// Which registered graph it traverses.
    pub graph: String,
    /// Which primitive: `reachable`, `time_respecting`, `cycles`, `shortest_path`,
    /// `influence`.
    pub primitive: String,
    /// The options string handed to that primitive.
    #[serde(default)]
    pub options: String,
}

/// The primitives a declared graph query may name.
const PRIMITIVES: &[&str] = &[
    "reachable",
    "time_respecting",
    "cycles",
    "shortest_path",
    "influence",
];

impl Bundle {
    /// Read a bundle from TOML text.
    pub fn from_toml(text: &str) -> Result<Self, BundleError> {
        toml::from_str(text).map_err(|e| BundleError::Malformed {
            detail: e.to_string(),
        })
    }

    /// What this pack claims to be.
    #[must_use]
    pub fn info(&self) -> PackInfo {
        PackInfo {
            name: self.pack.name.clone(),
            version: self.pack.version.clone(),
            api_version: self.pack.api_version,
            description: self.pack.description.clone(),
        }
    }

    /// Check everything checkable before anything is registered.
    ///
    /// Validation is at load, deliberately and completely. A bundle whose expression fails
    /// to parse, or whose declared return type does not match what its expression actually
    /// produces, must be refused *now* --- not when a query happens to reach it, by which
    /// time it is in production and the person who wrote it has moved on.
    pub fn validate(&self) -> Result<Validated, BundleError> {
        if self.pack.api_version != API_VERSION {
            return Err(BundleError::WrongApiVersion {
                declared: self.pack.api_version,
                supported: API_VERSION,
            });
        }
        if self.pack.name.is_empty() {
            return Err(BundleError::Invalid {
                what: "the pack".to_string(),
                detail: "a pack must have a name".to_string(),
            });
        }

        let prefix = format!("{}_", self.pack.name);
        let mut functions = Vec::new();
        let mut seen: BTreeMap<String, ()> = BTreeMap::new();

        for declared in &self.function {
            // Every function is prefixed with its pack. Two packs cannot then collide by
            // accident, and a reader of a query can tell where a name came from.
            if !declared.name.starts_with(&prefix) {
                return Err(BundleError::Invalid {
                    what: declared.name.clone(),
                    detail: format!(
                        "every function in this pack must be named '{prefix}...', so a \
                         reader of a query can tell where the name came from and two packs \
                         cannot collide by accident"
                    ),
                });
            }
            if seen.insert(declared.name.clone(), ()).is_some() {
                return Err(BundleError::Invalid {
                    what: declared.name.clone(),
                    detail: "declared twice in the same bundle".to_string(),
                });
            }

            let mut argument_types = BTreeMap::new();
            let mut ordered = Vec::new();
            for (name, type_name) in &declared.arguments {
                let logical = logical_type(type_name).ok_or_else(|| BundleError::Invalid {
                    what: declared.name.clone(),
                    detail: format!("'{type_name}' is not a type this API offers"),
                })?;
                argument_types.insert(name.clone(), logical.clone());
                ordered.push((name.clone(), logical));
            }

            let expression = parse(&declared.expression).map_err(|e| BundleError::Invalid {
                what: declared.name.clone(),
                detail: format!("the expression does not parse: {e}"),
            })?;

            // Every name the expression uses must be an argument. A typo would otherwise
            // become a runtime error inside a query rather than a load failure.
            let mut used = Vec::new();
            collect_arguments(&expression, &mut used);
            for name in &used {
                if !argument_types.contains_key(name) {
                    return Err(BundleError::Invalid {
                        what: declared.name.clone(),
                        detail: format!(
                            "the expression uses '{name}', which is not one of its arguments \
                             ({:?})",
                            argument_types.keys().collect::<Vec<_>>()
                        ),
                    });
                }
            }

            let declared_return =
                logical_type(&declared.returns).ok_or_else(|| BundleError::Invalid {
                    what: declared.name.clone(),
                    detail: format!("'{}' is not a type this API offers", declared.returns),
                })?;

            // The declared return type must match what the expression produces. A
            // mismatch is caught here rather than surfacing as a wrong column type in a
            // result set, where it would be blamed on the query.
            if let Some(actual) = result_type(&expression, &argument_types) {
                if !actual.satisfies(&declared_return) {
                    return Err(BundleError::Invalid {
                        what: declared.name.clone(),
                        detail: format!(
                            "declares that it returns {declared_return} but its expression \
                             produces {actual}"
                        ),
                    });
                }
            }

            functions.push(ValidatedFunction {
                name: declared.name.clone(),
                description: declared.description.clone(),
                arguments: ordered,
                signature: Signature::of(
                    declared
                        .arguments
                        .values()
                        .filter_map(|t| logical_type(t))
                        .collect::<Vec<_>>(),
                    declared_return,
                ),
                expression,
            });
        }

        let mut queries = Vec::new();
        for declared in &self.graph_query {
            if !declared.name.starts_with(&prefix) {
                return Err(BundleError::Invalid {
                    what: declared.name.clone(),
                    detail: format!("every function in this pack must be named '{prefix}...'"),
                });
            }
            if seen.insert(declared.name.clone(), ()).is_some() {
                return Err(BundleError::Invalid {
                    what: declared.name.clone(),
                    detail: "declared twice in the same bundle".to_string(),
                });
            }
            if !PRIMITIVES.contains(&declared.primitive.as_str()) {
                return Err(BundleError::Invalid {
                    what: declared.name.clone(),
                    detail: format!(
                        "'{}' is not a graph primitive; the choices are {PRIMITIVES:?}",
                        declared.primitive
                    ),
                });
            }
            queries.push(declared.clone());
        }

        Ok(Validated {
            info: self.info(),
            functions,
            queries,
        })
    }
}

/// A bundle that has been checked.
///
/// Its own type, so that "validated" is something the type system carries rather than
/// something a caller has to remember to have done.
#[derive(Clone, Debug)]
pub struct Validated {
    /// What the pack is.
    pub info: PackInfo,
    /// Its scalar functions, parsed and type-checked.
    pub functions: Vec<ValidatedFunction>,
    /// Its graph queries.
    pub queries: Vec<DeclaredGraphQuery>,
}

/// One declared function, parsed and checked.
#[derive(Clone, Debug)]
pub struct ValidatedFunction {
    /// What it is called in SQL.
    pub name: String,
    /// A sentence about it.
    pub description: String,
    /// Its arguments in declaration order.
    pub arguments: Vec<(String, LogicalType)>,
    /// What it accepts and returns.
    pub signature: Signature,
    /// The expression computing it.
    pub expression: Expr,
}

/// Every argument name an expression mentions.
fn collect_arguments(expr: &Expr, into: &mut Vec<String>) {
    match expr {
        Expr::Argument(name) => into.push(name.clone()),
        Expr::Literal(_) => {}
        Expr::Unary { operand, .. } => collect_arguments(operand, into),
        Expr::Binary { left, right, .. } => {
            collect_arguments(left, into);
            collect_arguments(right, into);
        }
        Expr::IfElse {
            condition,
            then,
            otherwise,
        } => {
            collect_arguments(condition, into);
            collect_arguments(then, into);
            collect_arguments(otherwise, into);
        }
    }
}

/// The logical type a type name denotes.
fn logical_type(name: &str) -> Option<LogicalType> {
    Some(match name.to_lowercase().as_str() {
        "boolean" | "bool" => LogicalType::Boolean,
        "integer" | "int" => LogicalType::Integer,
        "real" | "float" | "double" => LogicalType::Real,
        "text" | "string" => LogicalType::Text,
        "bytes" | "binary" => LogicalType::Bytes,
        "instant" | "timestamp" => LogicalType::Instant,
        "any" => LogicalType::Any,
        _ => return None,
    })
}

/// Why a bundle could not be used.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum BundleError {
    /// The file is not valid TOML, or does not have the expected shape.
    Malformed {
        /// What the parser said.
        detail: String,
    },
    /// It was written against a different version of the extension API.
    WrongApiVersion {
        /// What it claims.
        declared: u32,
        /// What this engine offers.
        supported: u32,
    },
    /// Something in it does not hold together.
    Invalid {
        /// Which part.
        what: String,
        /// Why.
        detail: String,
    },
}

impl std::fmt::Display for BundleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed { detail } => write!(f, "the bundle could not be read: {detail}"),
            Self::WrongApiVersion {
                declared,
                supported,
            } => write!(
                f,
                "this bundle was written against extension API version {declared} and this \
                 engine offers version {supported}"
            ),
            Self::Invalid { what, detail } => write!(f, "'{what}' is not valid: {detail}"),
        }
    }
}

impl std::error::Error for BundleError {}

//! A pack that tries everything a pack must not be able to do.
//!
//! This is a **test fixture that ships**, and it is deliberately not confined to a test
//! directory. An extension mechanism's security properties are claims about what is
//! impossible, and a claim of impossibility is worth nothing until something has tried.
//!
//! Every function here is an attempt. Each one must fail, and each failure must **name this
//! pack** --- an engine that refuses an attack without saying who made it leaves an operator
//! with an alert and nothing to act on.
//!
//! # What is attempted, and what stops it
//!
//! | Attempt | What refuses it |
//! |---|---|
//! | Shadow a core function name | The registry's reserved-prefix rule, at load |
//! | Shadow another pack's name | The registry's conflict rule, at load |
//! | Claim an incompatible API version | The version check, at load |
//! | Loop forever | The sandbox's wall-clock bound, at call |
//! | Panic | The sandbox's thread boundary, at call |
//! | Read another tenant's data | No API for it exists --- a function is handed values |
//!
//! The last row is the one worth dwelling on. It is not defended at runtime because there
//! is nothing to defend: a pack function receives `&[Value]` and an [`Invocation`] carrying
//! a tenant *string*. It has no handle to a catalog, a connection, a file or an epoch. The
//! absence of the capability is the enforcement, which is stronger than a check, because a
//! check can be wrong.

#![doc(html_root_url = "https://docs.rs/pack-adversarial")]

use sankhya_ext::function::{Invocation, Pack, PackInfo, ScalarFunction, Signature, API_VERSION};
use sankhya_ext::registry::Registry;
use sankhya_ext::value::{LogicalType, Value};
use sankhya_ext::PackError;
use std::sync::Arc;

/// The hostile pack.
#[derive(Debug, Default)]
pub struct AdversarialPack;

impl Pack for AdversarialPack {
    fn info(&self) -> PackInfo {
        PackInfo {
            name: "adversarial".to_string(),
            version: "0.1.0".to_string(),
            api_version: API_VERSION,
            description: "Attempts every prohibited thing. Every attempt must be refused."
                .to_string(),
        }
    }

    fn register(&self, registry: &mut Registry) {
        // Each of these must be refused and recorded in `Registry::rejected`.
        registry.add_scalar(Arc::new(ShadowCore));
        registry.add_scalar(Arc::new(ShadowGraph));
        registry.add_scalar(Arc::new(Nameless));

        // These register successfully; their misbehaviour is at call time.
        registry.add_scalar(Arc::new(NeverReturns));
        registry.add_scalar(Arc::new(Panics));
        registry.add_scalar(Arc::new(ReachesForAnotherTenant));
    }
}

/// A pack claiming to be built against an API version this engine does not offer.
#[derive(Debug, Default)]
pub struct WrongApiVersionPack;

impl Pack for WrongApiVersionPack {
    fn info(&self) -> PackInfo {
        PackInfo {
            name: "adversarial-wrong-api".to_string(),
            version: "0.1.0".to_string(),
            api_version: API_VERSION + 99,
            description: "Claims a future API version.".to_string(),
        }
    }

    fn register(&self, registry: &mut Registry) {
        registry.add_scalar(Arc::new(NeverReturns));
    }
}

/// A macro's worth of boilerplate, written out because there are only six of these and a
/// macro would obscure what each one is attempting.
macro_rules! trivial_scalar {
    ($type:ident, $name:literal, $description:literal) => {
        #[derive(Debug)]
        struct $type;

        impl ScalarFunction for $type {
            fn name(&self) -> &str {
                $name
            }
            fn description(&self) -> &str {
                $description
            }
            fn signature(&self) -> Signature {
                Signature::of(vec![LogicalType::Any], LogicalType::Integer)
            }
            fn invoke(
                &self,
                arguments: &[Value],
                context: &Invocation,
            ) -> Result<Value, PackError> {
                let _ = arguments;
                let _ = context;
                Ok(Value::Integer(0))
            }
        }
    };
}

trivial_scalar!(
    ShadowCore,
    "sankhya_version",
    "Attempts to shadow a reserved engine name."
);
trivial_scalar!(
    ShadowGraph,
    "graph_reachable",
    "Attempts to shadow a core graph function, changing what existing queries mean."
);
trivial_scalar!(Nameless, "", "Registers under an empty name.");

/// A function that never returns.
///
/// The M4 exit criterion in one struct: the query must fail with an error naming this pack
/// rather than hanging. It does not call [`Invocation::check`], deliberately --- a
/// cooperating function is easy to stop and proves nothing.
#[derive(Debug)]
struct NeverReturns;

impl ScalarFunction for NeverReturns {
    fn name(&self) -> &str {
        "adversarial_never_returns"
    }

    fn description(&self) -> &str {
        "Loops forever without checking for cancellation."
    }

    fn signature(&self) -> Signature {
        Signature::of(vec![LogicalType::Integer], LogicalType::Integer)
    }

    fn invoke(&self, _arguments: &[Value], _context: &Invocation) -> Result<Value, PackError> {
        // Deliberately ignores the cancellation flag. `std::hint::black_box` keeps the
        // optimiser from deleting a loop it can prove has no effect --- without it, a
        // release build turns this into an immediate return and the test passes for the
        // wrong reason.
        let mut spin: u64 = 0;
        loop {
            spin = std::hint::black_box(spin.wrapping_add(1));
        }
    }
}

/// A function that panics.
///
/// A panic in pack code must fail the query and not the engine.
#[derive(Debug)]
struct Panics;

impl ScalarFunction for Panics {
    fn name(&self) -> &str {
        "adversarial_panics"
    }

    fn description(&self) -> &str {
        "Panics on every call."
    }

    fn signature(&self) -> Signature {
        Signature::of(vec![LogicalType::Integer], LogicalType::Integer)
    }

    #[allow(clippy::panic)]
    fn invoke(&self, _arguments: &[Value], _context: &Invocation) -> Result<Value, PackError> {
        panic!("this pack panics deliberately, to prove the engine survives it");
    }
}

/// A function that tries to read data belonging to someone else.
///
/// It cannot, and the interesting part is *why*: there is no API through which to try. It
/// is handed values and a tenant name, and holds no reference to a catalog, a connection, a
/// file or a graph epoch. So the best it can manage is to return the tenant string it was
/// given --- which is its own caller's --- and that is the point being demonstrated.
#[derive(Debug)]
struct ReachesForAnotherTenant;

impl ScalarFunction for ReachesForAnotherTenant {
    fn name(&self) -> &str {
        "adversarial_cross_tenant"
    }

    fn description(&self) -> &str {
        "Attempts to reach another tenant's data, and demonstrates that no API permits it."
    }

    fn signature(&self) -> Signature {
        Signature::of(vec![LogicalType::Text], LogicalType::Text)
    }

    fn invoke(&self, arguments: &[Value], context: &Invocation) -> Result<Value, PackError> {
        let wanted = arguments.first().and_then(Value::as_text).unwrap_or("");
        if wanted != context.tenant() {
            // There is no call it could make here. The refusal is written out so the
            // attempt is visible in a test, but nothing was prevented at runtime --- the
            // capability was never granted in the first place.
            return Err(PackError::forbidden(
                "adversarial",
                "adversarial_cross_tenant",
                format!(
                    "asked for tenant '{wanted}' while running for '{}'. No extension API \
                     exposes another tenant's data: a pack function receives values and a \
                     tenant name, and holds no catalog, connection or epoch. The absence of \
                     the capability is what enforces this, not a check",
                    context.tenant()
                ),
            ));
        }
        Ok(Value::Text(context.tenant().to_string()))
    }
}

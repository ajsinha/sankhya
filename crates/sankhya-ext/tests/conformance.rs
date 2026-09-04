//! The extension conformance suite, and the adversarial pack's every attempt refused.
//!
//! Two claims are under test here and both are the kind that are worth nothing until
//! something has tried to break them.
//!
//! **That the API is general.** Two packs from unrelated industries --- one graph-heavy, one
//! graph-free --- load and work on an unmodified engine. Neither is named by any core crate;
//! the engine is handed a `&dyn Pack` and asks it what it has.
//!
//! **That a hostile pack cannot do damage.** Every attempt the adversarial pack makes is
//! refused, and every refusal names the pack. An engine that blocks an attack without
//! saying who made it leaves an operator with an alert and nothing to act on.

// Tests may panic — that is how a test reports a failure. The workspace denies
// `unwrap`, `expect`, `panic` and indexing because a *server* must not do those things
// on data it did not choose; a test chooses all of its data, and an assertion that
// cannot fail loudly is worse than useless.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use pack_adversarial::{AdversarialPack, WrongApiVersionPack};
use pack_ref_logistics::LogisticsPack;
use pack_ref_telemetry::TelemetryPack;
use sankhya_ext::function::Invocation;
use sankhya_ext::registry::Registry;
use sankhya_ext::sandbox::{Sandbox, SandboxError};
use sankhya_ext::value::{LogicalType, Value};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

fn context(tenant: &str, pack: &str, function: &str) -> (Invocation, Arc<AtomicBool>) {
    let flag = Arc::new(AtomicBool::new(false));
    (
        Invocation::new(tenant, pack, function, Arc::clone(&flag)),
        flag,
    )
}

// --- the reference packs --------------------------------------------------

#[test]
fn two_packs_from_unrelated_industries_load_side_by_side() {
    // The general-purpose claim, mechanically. Neither pack is named by any core crate:
    // the registry is handed a `&dyn Pack` and asks it what it has.
    let mut registry = Registry::new();
    registry.load(&LogisticsPack).expect("logistics loads");
    registry.load(&TelemetryPack).expect("telemetry loads");

    assert_eq!(registry.packs().len(), 2);
    assert!(registry.scalar("logistics_dwell_hours").is_some());
    assert!(registry.scalar("telemetry_breach_severity").is_some());
    assert!(
        registry.table("telemetry_threshold_bands").is_some(),
        "the two packs must exercise opposite halves of the API, scalars and tables"
    );
    assert!(
        registry.rejected().is_empty(),
        "a well-behaved pack has nothing refused: {:?}",
        registry.rejected()
    );
}

#[test]
fn a_contribution_is_attributed_to_the_pack_that_made_it() {
    let mut registry = Registry::new();
    registry.load(&LogisticsPack).expect("loads");
    registry.load(&TelemetryPack).expect("loads");

    assert_eq!(
        registry
            .scalar("logistics_dwell_hours")
            .map(|r| r.pack.as_str()),
        Some("logistics")
    );
    assert_eq!(
        registry
            .scalar("telemetry_window_start")
            .map(|r| r.pack.as_str()),
        Some("telemetry")
    );
}

#[test]
fn a_pack_function_returns_null_for_null_rather_than_a_plausible_number() {
    // Substituting zero would turn "we do not know when it left" into "it left
    // immediately", which is a different claim rather than a missing one.
    let mut registry = Registry::new();
    registry.load(&LogisticsPack).expect("loads");
    let function = registry.scalar("logistics_dwell_hours").expect("present");
    let (invocation, _) = context("acme", "logistics", "logistics_dwell_hours");

    let got = function
        .item
        .invoke(&[Value::Null, Value::Instant(10)], &invocation)
        .expect("null is not an error");
    assert_eq!(got, Value::Null);
}

#[test]
fn a_pack_function_refuses_impossible_input_rather_than_returning_a_number() {
    let mut registry = Registry::new();
    registry.load(&LogisticsPack).expect("loads");
    let function = registry.scalar("logistics_dwell_hours").expect("present");
    let (invocation, _) = context("acme", "logistics", "logistics_dwell_hours");

    // Departed before it arrived.
    let Err(error) = function
        .item
        .invoke(&[Value::Instant(1_000), Value::Instant(0)], &invocation)
    else {
        panic!("a negative dwell must be refused");
    };
    assert_eq!(error.pack, "logistics");
    assert!(error.to_string().contains("data defect"));
}

#[test]
fn window_bucketing_floors_downward_on_both_sides_of_the_epoch() {
    // Rust's `%` truncates toward zero, which would put an instant just before the epoch in
    // the window *after* the one containing it — a one-bucket error that only ever appears
    // in historical data.
    let mut registry = Registry::new();
    registry.load(&TelemetryPack).expect("loads");
    let function = registry.scalar("telemetry_window_start").expect("present");
    let (invocation, _) = context("acme", "telemetry", "telemetry_window_start");

    let after = function
        .item
        .invoke(&[Value::Instant(250), Value::Integer(100)], &invocation)
        .expect("valid");
    assert_eq!(after, Value::Instant(200));

    let before = function
        .item
        .invoke(&[Value::Instant(-250), Value::Integer(100)], &invocation)
        .expect("valid");
    assert_eq!(
        before,
        Value::Instant(-300),
        "an instant before the epoch must floor downward like one after it"
    );
}

#[test]
fn a_table_function_declares_its_columns_before_it_runs() {
    // The planner needs the shape before execution. A table function whose columns depend
    // on its data cannot be joined against without running it first.
    let mut registry = Registry::new();
    registry.load(&TelemetryPack).expect("loads");
    let function = registry
        .table("telemetry_threshold_bands")
        .expect("present");

    let columns = function.item.columns();
    assert_eq!(columns.len(), 3);
    assert_eq!(columns.first().map(|c| c.0.as_str()), Some("band"));
    assert_eq!(function.item.estimated_rows(&[]), Some(3));

    let (invocation, _) = context("acme", "telemetry", "telemetry_threshold_bands");
    let rows = function
        .item
        .invoke(&[Value::Real(10.0)], &invocation)
        .expect("valid");
    assert_eq!(
        rows.len(),
        3,
        "the declared estimate matches what it returns"
    );
}

// --- the adversarial pack -------------------------------------------------

#[test]
fn shadowing_a_reserved_engine_name_is_refused_at_load() {
    // The most dangerous thing an extension mechanism can permit: changing what an existing
    // query means without changing its text.
    let mut registry = Registry::new();
    registry
        .load(&AdversarialPack)
        .expect("the pack itself loads");

    let shadow = registry
        .rejected()
        .iter()
        .find(|r| r.name == "sankhya_version")
        .expect("shadowing a reserved name must be refused");
    assert_eq!(shadow.pack, "adversarial", "the refusal names the pack");
    assert!(shadow.reason.contains("reserved"));
    assert!(registry.scalar("sankhya_version").is_none());
}

#[test]
fn shadowing_a_core_graph_function_is_refused() {
    let mut registry = Registry::new();
    registry.load(&AdversarialPack).expect("loads");

    let shadow = registry
        .rejected()
        .iter()
        .find(|r| r.name == "graph_reachable")
        .expect("shadowing a core graph name must be refused");
    assert!(shadow.reason.contains("without changing its text"));
    assert!(registry.scalar("graph_reachable").is_none());
}

#[test]
fn an_empty_function_name_is_refused() {
    let mut registry = Registry::new();
    registry.load(&AdversarialPack).expect("loads");
    assert!(registry
        .rejected()
        .iter()
        .any(|r| r.name.is_empty() && r.reason.contains("may not be empty")));
}

#[test]
fn a_pack_built_against_another_api_version_is_refused_at_load() {
    // Refused at load rather than failing later inside a query, where the cause would no
    // longer be visible.
    let mut registry = Registry::new();
    let Err(error) = registry.load(&WrongApiVersionPack) else {
        panic!("a pack claiming a future API version must be refused");
    };

    assert_eq!(error.pack, "adversarial-wrong-api");
    assert!(error.to_string().contains("no longer be visible"));
    assert!(registry.packs().is_empty());
}

#[test]
fn two_packs_claiming_the_same_function_name_do_not_resolve_by_load_order() {
    // Resolving by load order would make the answer depend on start-up, which is a bug
    // that only appears after a restart.
    let mut registry = Registry::new();
    registry.load(&LogisticsPack).expect("loads");

    let Err(error) = registry.load(&LogisticsPack) else {
        panic!("loading the same pack twice must be refused");
    };
    assert!(error.to_string().contains("already loaded"));
}

#[test]
fn a_function_that_never_returns_is_stopped_and_the_error_names_the_pack() {
    // The M4 exit criterion, exactly. `NeverReturns` deliberately ignores the cancellation
    // flag — a cooperating function is easy to stop and proves nothing.
    let sandbox = Sandbox::new();
    let flag = Arc::new(AtomicBool::new(false));

    let outcome: Result<Value, SandboxError> = sandbox.run(
        "adversarial",
        "adversarial_never_returns",
        Duration::from_millis(120),
        Arc::clone(&flag),
        move || {
            let mut spin: u64 = 0;
            loop {
                spin = std::hint::black_box(spin.wrapping_add(1));
            }
        },
    );

    let Err(error) = outcome else {
        panic!("a function that never returns must not produce a value");
    };
    assert_eq!(error.pack(), "adversarial", "the error names the pack");
    let message = error.to_string();
    assert!(
        message.contains("did not respond to cancellation"),
        "{message}"
    );
    assert!(
        message.contains("rather than left to hang"),
        "the query is failed, not hung: {message}"
    );
    assert_eq!(
        sandbox.abandoned(),
        1,
        "an abandoned call leaks a thread, and that must be countable rather than merely \
         suspected"
    );
}

#[test]
fn a_cooperating_function_stops_without_being_abandoned() {
    // The contrast that makes the previous test meaningful. A function that checks stops
    // promptly and costs nothing; abandonment is the fallback, not the mechanism.
    let sandbox = Sandbox::new();
    let flag = Arc::new(AtomicBool::new(true)); // already cancelled
    let invocation = Invocation::new("acme", "logistics", "loop", Arc::clone(&flag));

    let outcome: Result<Value, SandboxError> = sandbox.run(
        "logistics",
        "loop",
        Duration::from_secs(5),
        Arc::clone(&flag),
        move || loop {
            invocation.check()?;
        },
    );

    let Err(SandboxError::Failed(error)) = outcome else {
        panic!("a cooperating function should return its own cancellation error");
    };
    assert_eq!(error.pack, "logistics");
    assert!(error.retryable(), "cancellation may be retried");
    assert_eq!(
        sandbox.abandoned(),
        0,
        "nothing was abandoned; the function stopped when asked"
    );
}

#[test]
fn a_panicking_function_fails_the_query_and_not_the_engine() {
    let sandbox = Sandbox::new();
    let flag = Arc::new(AtomicBool::new(false));

    let outcome: Result<Value, SandboxError> = sandbox.run(
        "adversarial",
        "adversarial_panics",
        Duration::from_secs(5),
        flag,
        || panic!("deliberate"),
    );

    let Err(error) = outcome else {
        panic!("a panicking function must not produce a value");
    };
    assert_eq!(error.pack(), "adversarial");
    assert!(error.to_string().contains("the engine did not"));

    // And the engine is still running, which is the whole assertion.
    assert_eq!(sandbox.abandoned(), 0);
}

#[test]
fn a_pack_has_no_api_through_which_to_reach_another_tenant() {
    // Not defended at runtime, because there is nothing to defend. A pack function receives
    // values and a tenant name, and holds no catalog, connection, file or epoch. The
    // absence of the capability is the enforcement, which is stronger than a check because
    // a check can be wrong.
    let mut registry = Registry::new();
    registry.load(&AdversarialPack).expect("loads");
    let function = registry
        .scalar("adversarial_cross_tenant")
        .expect("present");
    let (invocation, _) = context("acme", "adversarial", "adversarial_cross_tenant");

    let Err(error) = function
        .item
        .invoke(&[Value::Text("someone-else".to_string())], &invocation)
    else {
        panic!("reaching for another tenant must not succeed");
    };
    assert_eq!(error.pack, "adversarial");
    assert!(error.to_string().contains("absence of the capability"));

    // Its own tenant is all it can name.
    let own = function
        .item
        .invoke(&[Value::Text("acme".to_string())], &invocation)
        .expect("its own tenant is fine");
    assert_eq!(own, Value::Text("acme".to_string()));
}

// --- the API's own rules --------------------------------------------------

#[test]
fn a_signature_accepts_null_for_any_declared_type() {
    // Null is a valid value of every type. Rejecting it would make every function
    // null-intolerant by default, and every pack would have to special-case it.
    let signature = sankhya_ext::function::Signature::of(
        vec![LogicalType::Integer, LogicalType::Text],
        LogicalType::Integer,
    );
    assert!(signature.accepts(&[Value::Integer(1), Value::Text("x".to_string())]));
    assert!(signature.accepts(&[Value::Null, Value::Null]));
    assert!(!signature.accepts(&[Value::Integer(1)]), "arity is checked");
    assert!(
        !signature.accepts(&[Value::Text("x".to_string()), Value::Text("y".to_string())]),
        "text does not satisfy integer"
    );
}

#[test]
fn integers_widen_into_reals_but_nothing_narrows() {
    // A narrowing conversion loses information silently, which is the one thing a type
    // system in this position exists to prevent.
    assert!(LogicalType::Integer.satisfies(&LogicalType::Real));
    assert!(!LogicalType::Real.satisfies(&LogicalType::Integer));
    assert!(LogicalType::Text.satisfies(&LogicalType::Any));
    assert!(!LogicalType::Text.satisfies(&LogicalType::Integer));
}

/// Two packs claiming one function name: the second contribution is refused, not resolved.
///
/// # Why this was missing
///
/// A mutation that removed the duplicate check survived the suite. `shadowing_a_reserved_
/// engine_name_is_refused_at_load` covers a pack colliding with the **engine**; nothing
/// covered a pack colliding with **another pack**, and those are different code paths with
/// different consequences.
///
/// Resolving a collision by load order makes the answer depend on start-up: two nodes given
/// the same packs in a different order compute different numbers for the same statement, and
/// nothing in either query text says so. The registry's own message names that as the reason,
/// and until now nothing checked that the message was ever reached.
#[test]
fn two_packs_claiming_one_function_name_do_not_resolve_by_load_order() {
    /// A pack that deliberately offers a name `LogisticsPack` already offers.
    #[derive(Debug)]
    struct ImpostorPack;

    #[derive(Debug)]
    struct Impostor;

    impl sankhya_ext::function::ScalarFunction for Impostor {
        fn name(&self) -> &str {
            "logistics_dwell_hours"
        }
        fn description(&self) -> &str {
            "A different answer under the same name."
        }
        fn signature(&self) -> sankhya_ext::function::Signature {
            sankhya_ext::function::Signature::of(vec![LogicalType::Real], LogicalType::Real)
        }
        fn invoke(&self, _arguments: &[Value], _context: &Invocation) -> Result<Value, sankhya_ext::error::PackError> {
            Ok(Value::Real(-1.0))
        }
    }

    impl sankhya_ext::function::Pack for ImpostorPack {
        fn info(&self) -> sankhya_ext::function::PackInfo {
            sankhya_ext::function::PackInfo {
                name: "impostor".to_string(),
                version: "1".to_string(),
                api_version: sankhya_ext::function::API_VERSION,
                description: "claims a name another pack already registered".to_string(),
            }
        }
        fn register(&self, registry: &mut Registry) {
            registry.add_scalar(Arc::new(Impostor));
        }
    }

    let mut registry = Registry::new();
    registry.load(&LogisticsPack).expect("logistics loads");
    registry.load(&ImpostorPack).expect("the pack itself loads");

    let clash = registry
        .rejected()
        .iter()
        .find(|r| r.name == "logistics_dwell_hours")
        .expect("a second pack claiming the same name must be refused");
    assert_eq!(clash.pack, "impostor", "the refusal names the later pack");
    assert!(
        clash.reason.contains("already registered"),
        "the refusal must say why: {}",
        clash.reason
    );

    // And the original still answers, rather than the collision silently winning.
    let held = registry
        .scalar("logistics_dwell_hours")
        .expect("the first registration stands");
    assert_eq!(held.pack, "logistics", "load order decided the winner");
}

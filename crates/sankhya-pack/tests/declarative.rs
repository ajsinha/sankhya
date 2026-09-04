//! A pack written as a file rather than as a crate.
//!
//! Two properties carry this tier. Everything checkable is checked **at load** --- a bundle
//! whose expression does not parse, or whose declared return type disagrees with what its
//! expression produces, is refused before anything is registered rather than when a query
//! happens to reach it. And a reload is **all or nothing**, because half a reload means
//! some queries see the new definitions and some the old, depending on timing.

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

use sankhya_ext::value::Value;
use sankhya_pack::bundle::Bundle;
use sankhya_pack::loader::Loader;
use sankhya_pack::parse::parse;
use sankhya_pack::verify::{Digest, PinnedDigests, TrustEverything, Verifier};
use std::collections::BTreeMap;

fn arguments(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect()
}

fn evaluate(text: &str, args: &[(&str, Value)]) -> Value {
    parse(text)
        .expect("the fixture expression parses")
        .evaluate(&arguments(args))
        .expect("the fixture expression evaluates")
}

const EXAMPLE: &str = r#"
[pack]
name = "batch"
version = "1.0.0"
api_version = 1
description = "Delay thresholds and stage naming."

[[function]]
name = "batch_is_late"
description = "Whether a run missed its promised time."
arguments = { actual = "instant", promised = "instant" }
returns = "boolean"
expression = "actual > promised"

[[function]]
name = "batch_delay_ratio"
description = "How far past the allowance a delay ran."
arguments = { delay = "real", allowance = "real" }
returns = "real"
expression = "delay / allowance"

[[graph_query]]
name = "batch_upstream_stages"
description = "Stages a run passed through, in order."
graph = "stages"
primitive = "time_respecting"
options = "max_depth=6"
"#;

// --- the expression language ---------------------------------------------

#[test]
fn arithmetic_follows_the_precedence_anyone_would_expect() {
    assert_eq!(evaluate("2 + 3 * 4", &[]), Value::Integer(14));
    assert_eq!(evaluate("(2 + 3) * 4", &[]), Value::Integer(20));
    assert_eq!(
        evaluate("10 - 2 - 3", &[]),
        Value::Integer(5),
        "left-associative"
    );
    assert_eq!(evaluate("100 / 10 / 2", &[]), Value::Integer(5));
}

#[test]
fn integers_stay_integers_and_mixed_arithmetic_widens() {
    assert_eq!(
        evaluate("7 / 2", &[]),
        Value::Integer(3),
        "integer division"
    );
    assert_eq!(evaluate("7.0 / 2", &[]), Value::Real(3.5));
}

#[test]
fn an_overflowing_sum_saturates_rather_than_wrapping() {
    // A wrapped total is a wrong number that looks ordinary. A saturated one is wrong in a
    // direction that is obvious.
    let got = evaluate("big + big", &[("big", Value::Integer(i64::MAX))]);
    assert_eq!(got, Value::Integer(i64::MAX));
}

#[test]
fn dividing_by_zero_is_an_error_rather_than_infinity() {
    // An infinite result sorts to the top of every ranked list and reads as an extreme
    // finding rather than a missing denominator.
    let outcome = parse("1.0 / z")
        .expect("parses")
        .evaluate(&arguments(&[("z", Value::Real(0.0))]));
    let Err(error) = outcome else {
        panic!("division by zero must not produce a value");
    };
    assert!(error.to_string().contains("division by zero"));
}

#[test]
fn null_propagates_through_arithmetic_and_comparison() {
    assert_eq!(evaluate("x + 1", &[("x", Value::Null)]), Value::Null);
    assert_eq!(evaluate("x > 1", &[("x", Value::Null)]), Value::Null);
}

#[test]
fn and_and_or_short_circuit_the_way_sql_does() {
    // `false and null` is false, because the answer is already known. Treating null as
    // false instead would make a missing value silently exclude rows.
    assert_eq!(
        evaluate("false and x", &[("x", Value::Null)]),
        Value::Boolean(false)
    );
    assert_eq!(
        evaluate("true or x", &[("x", Value::Null)]),
        Value::Boolean(true)
    );
    assert_eq!(
        evaluate("true and x", &[("x", Value::Null)]),
        Value::Null,
        "here the answer genuinely is not known"
    );
}

#[test]
fn short_circuiting_prevents_the_division_the_guard_was_written_to_prevent() {
    // The reason short-circuiting is not merely an optimisation.
    assert_eq!(
        evaluate("z <> 0 and 100 / z > 1", &[("z", Value::Integer(0))]),
        Value::Boolean(false)
    );
}

#[test]
fn a_null_condition_takes_neither_branch() {
    // Treating unknown as false would make "we do not know" mean "no".
    assert_eq!(
        evaluate("if c then 1 else 2", &[("c", Value::Null)]),
        Value::Null
    );
    assert_eq!(
        evaluate("if c then 1 else 2", &[("c", Value::Boolean(true))]),
        Value::Integer(1)
    );
}

#[test]
fn text_and_instants_compare_but_not_against_each_other() {
    assert_eq!(
        evaluate(
            "a < b",
            &[
                ("a", Value::Text("x".into())),
                ("b", Value::Text("y".into()))
            ]
        ),
        Value::Boolean(true)
    );
    let mixed = parse("a < b").expect("parses").evaluate(&arguments(&[
        ("a", Value::Text("x".into())),
        ("b", Value::Integer(1)),
    ]));
    assert!(
        mixed.is_err(),
        "text and integer are not the same kind of value"
    );
}

#[test]
fn an_expression_that_does_not_parse_says_where() {
    let Err(error) = parse("1 + ") else {
        panic!("an incomplete expression must not parse");
    };
    assert!(error.to_string().contains("at character"));
}

#[test]
fn the_language_has_no_loop_to_hang_a_query_with() {
    // Safe by construction rather than by supervision. There is no syntax here that can
    // repeat, so a declarative function cannot be the one the sandbox has to abandon.
    for attempt in ["while x do y", "loop 1", "f(x)", "x := 1"] {
        assert!(
            parse(attempt).is_err(),
            "'{attempt}' must not parse: an expression language with loops is a \
             programming language, and one loaded from a configuration file is remote code \
             execution with extra steps"
        );
    }
}

// --- bundles --------------------------------------------------------------

#[test]
fn a_well_formed_bundle_validates() {
    let bundle = Bundle::from_toml(EXAMPLE).expect("the fixture is valid TOML");
    let validated = bundle.validate().expect("and valid as a bundle");

    assert_eq!(validated.info.name, "batch");
    assert_eq!(validated.functions.len(), 2);
    assert_eq!(validated.queries.len(), 1);
    assert_eq!(
        validated.functions.first().map(|f| f.name.as_str()),
        Some("batch_is_late")
    );
}

#[test]
fn a_declared_function_computes_what_it_says() {
    let validated = Bundle::from_toml(EXAMPLE)
        .expect("valid TOML")
        .validate()
        .expect("valid bundle");
    let late = validated
        .functions
        .iter()
        .find(|f| f.name == "batch_is_late")
        .expect("declared");

    let yes = late
        .expression
        .evaluate(&arguments(&[
            ("actual", Value::Instant(200)),
            ("promised", Value::Instant(100)),
        ]))
        .expect("evaluates");
    assert_eq!(yes, Value::Boolean(true));
}

#[test]
fn a_return_type_that_disagrees_with_the_expression_is_refused_at_load() {
    // Caught here rather than surfacing as a wrong column type in a result set, where it
    // would be blamed on the query.
    let text = r#"
[pack]
name = "x"
version = "1"
api_version = 1
[[function]]
name = "x_wrong"
arguments = { a = "integer", b = "integer" }
returns = "text"
expression = "a + b"
"#;
    let Err(error) = Bundle::from_toml(text).expect("valid TOML").validate() else {
        panic!("a function returning an integer must not claim to return text");
    };
    assert!(error.to_string().contains("produces integer"), "{error}");
}

#[test]
fn an_expression_using_an_undeclared_name_is_refused_at_load() {
    // A typo would otherwise become a runtime error inside a query rather than a load
    // failure.
    let text = r#"
[pack]
name = "x"
version = "1"
api_version = 1
[[function]]
name = "x_typo"
arguments = { amount = "integer" }
returns = "integer"
expression = "amonut + 1"
"#;
    let Err(error) = Bundle::from_toml(text).expect("valid TOML").validate() else {
        panic!("a misspelled argument must be refused");
    };
    assert!(error.to_string().contains("amonut"), "{error}");
}

#[test]
fn a_function_not_prefixed_with_its_pack_is_refused() {
    let text = r#"
[pack]
name = "batch"
version = "1"
api_version = 1
[[function]]
name = "unprefixed"
arguments = { a = "integer" }
returns = "integer"
expression = "a"
"#;
    let Err(error) = Bundle::from_toml(text).expect("valid TOML").validate() else {
        panic!("an unprefixed name must be refused");
    };
    assert!(error.to_string().contains("batch_"));
}

#[test]
fn a_bundle_from_another_api_version_is_refused() {
    let text = r#"
[pack]
name = "x"
version = "1"
api_version = 99
"#;
    let Err(error) = Bundle::from_toml(text).expect("valid TOML").validate() else {
        panic!("a future API version must be refused");
    };
    assert!(error.to_string().contains("version 99"));
}

#[test]
fn an_unknown_graph_primitive_is_refused_and_the_message_lists_the_real_ones() {
    let text = r#"
[pack]
name = "x"
version = "1"
api_version = 1
[[graph_query]]
name = "x_q"
graph = "g"
primitive = "teleport"
"#;
    let Err(error) = Bundle::from_toml(text).expect("valid TOML").validate() else {
        panic!("an invented primitive must be refused");
    };
    assert!(error.to_string().contains("reachable"), "{error}");
}

// --- loading and verification ---------------------------------------------

fn write_bundle(dir: &std::path::Path, name: &str, text: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, text).expect("writing a fixture");
    path
}

#[test]
fn an_unpinned_bundle_is_refused_when_nothing_is_pinned() {
    // A trust policy that defaults to trusting is not a policy.
    let dir = tempfile::tempdir().expect("a temporary directory");
    write_bundle(dir.path(), "batch.toml", EXAMPLE);

    let mut loader = Loader::new(dir.path(), Box::new(PinnedDigests::default()));
    let outcome = loader.reload();

    assert!(!outcome.applied);
    assert_eq!(outcome.refused.len(), 1);
    let (_, reason) = outcome.refused.first().expect("one refusal");
    assert!(reason.contains("not a policy"), "{reason}");
}

#[test]
fn a_pinned_bundle_loads_and_an_edited_one_stops_loading() {
    // The control digest pinning actually provides: the file that loads is the file that
    // was reviewed, and any change to it fails closed.
    let dir = tempfile::tempdir().expect("a temporary directory");
    write_bundle(dir.path(), "batch.toml", EXAMPLE);

    let digest = Digest::of(EXAMPLE.as_bytes());
    let mut loader = Loader::new(dir.path(), Box::new(PinnedDigests::of([digest])));

    let first = loader.reload();
    assert!(first.applied, "{:?}", first.refused);
    assert_eq!(first.loaded.len(), 1);

    // Someone edits the file.
    let edited = EXAMPLE.replace("max_depth=6", "max_depth=60");
    write_bundle(dir.path(), "batch.toml", &edited);

    let second = loader.reload();
    assert!(!second.applied, "an edited bundle must not load");
    let (_, reason) = second.refused.first().expect("one refusal");
    assert!(reason.contains("changed since it was reviewed"), "{reason}");
    assert_eq!(
        second.loaded.len(),
        1,
        "and the previously loaded bundle stays in effect"
    );
}

#[test]
fn a_reload_is_all_or_nothing() {
    // Half a reload means some queries see new definitions and some old ones, depending on
    // timing, which is a class of bug nobody can reproduce.
    let dir = tempfile::tempdir().expect("a temporary directory");
    write_bundle(dir.path(), "a-batch.toml", EXAMPLE);

    let mut loader = Loader::new(dir.path(), Box::new(TrustEverything));
    assert!(loader.reload().applied);
    assert_eq!(loader.current().len(), 1);

    // Add a second, broken bundle.
    write_bundle(dir.path(), "b-broken.toml", "this is not toml [[[");
    let outcome = loader.reload();

    assert!(!outcome.applied);
    assert_eq!(outcome.refused.len(), 1);
    assert!(outcome.summary().contains("remain in effect"));
    assert_eq!(
        loader.current().len(),
        1,
        "the good bundle from before is still the one in effect, unchanged"
    );
}

#[test]
fn two_bundles_claiming_the_same_name_are_refused_as_a_set() {
    // Neither file can detect this on its own, and resolving it by load order would make
    // the answer depend on directory listing order.
    let dir = tempfile::tempdir().expect("a temporary directory");
    write_bundle(dir.path(), "one.toml", EXAMPLE);
    write_bundle(dir.path(), "two.toml", EXAMPLE);

    let mut loader = Loader::new(dir.path(), Box::new(TrustEverything));
    let outcome = loader.reload();

    assert!(!outcome.applied);
    let clash = outcome
        .refused
        .iter()
        .find(|(_, reason)| reason.contains("declared by both"))
        .expect("the clash is reported");
    assert!(clash.1.contains("directory listing order"));
}

#[test]
fn a_reload_that_changes_nothing_is_still_reported_honestly() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    write_bundle(dir.path(), "batch.toml", EXAMPLE);

    let mut loader = Loader::new(dir.path(), Box::new(TrustEverything));
    let first = loader.reload();
    let second = loader.reload();

    assert!(first.applied && second.applied);
    assert_eq!(first.loaded.len(), second.loaded.len());
    assert_eq!(
        first.loaded.first().map(|l| l.digest),
        second.loaded.first().map(|l| l.digest),
        "an unchanged file has an unchanged digest"
    );
}

#[test]
fn the_development_verifier_describes_itself_as_unsafe() {
    // An installation running this by accident has no control at all on its code-loading
    // path, so the description is written to look wrong in a production log.
    let description = TrustEverything.describe();
    assert!(description.contains("NO VERIFICATION"));
    assert!(description.contains("must not be used"));
}

#[test]
fn a_digest_round_trips_through_the_hexadecimal_operators_configure() {
    let digest = Digest::of(b"some bundle bytes");
    let text = digest.to_hex();
    assert_eq!(text.len(), 32);
    assert_eq!(Digest::from_hex(&text), Some(digest));
    assert_ne!(digest, Digest::of(b"some bundle byteS"));
}

/// The verdict that decides whether third-party bytes are executed.
///
/// # Why this was untested
///
/// A mutation inverting `Trust::is_allowed` --- so every refusal became permission to load ---
/// survived the whole suite. Nothing anywhere called it. The `Verifier` trait exists so that
/// a deployment can pin digests or check a signature, and the one line that reads its answer
/// had no test.
///
/// This is two lines of code and it is the gate on running somebody else's code in this
/// process. A refusal that is not read is not a refusal.
#[test]
fn only_an_allowed_verdict_permits_loading() {
    use sankhya_pack::verify::Trust;

    assert!(Trust::Allowed.is_allowed());
    assert!(
        !Trust::Refused {
            reason: "the digest is not on the allow-list".to_string(),
        }
        .is_allowed(),
        "a refusal read as permission loads code the deployment did not vouch for"
    );
}

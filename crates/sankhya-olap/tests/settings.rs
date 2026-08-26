//! Engine-settings tests.
//!
//! These guard against a failure with no symptom. A misconfigured engine returns
//! correct answers, raises no error, and simply runs an order of magnitude slower — so
//! nothing in normal operation would ever reveal it.

use datafusion::prelude::{SessionConfig, SessionContext};
use sankhya_olap::{apply_required_settings, session, verify_settings, REQUIRED};

#[test]
fn a_default_engine_is_not_configured_as_sankhya_requires() {
    // The premise of this whole module. If the defaults were already right, none of
    // this would be needed — and if they silently become right, this test tells us.
    let plain = SessionContext::new();
    let result = verify_settings(&plain);
    assert!(
        result.is_err(),
        "a stock engine unexpectedly satisfies SANKHYA's requirements; \
         if an upstream default has changed, the REQUIRED list should be revisited"
    );
}

#[test]
fn filter_pushdown_is_off_by_default() {
    // Named explicitly because it is the costly one: without it a selective query
    // decodes every payload column for every row rather than only the survivors.
    let plain = SessionContext::new();
    let err = verify_settings(&plain).expect_err("should not be configured");
    assert_eq!(err.key, "datafusion.execution.parquet.pushdown_filters");
    assert_eq!(err.actual, "false", "the default is expected to be false");
    assert!(
        err.to_string().contains("late materialization"),
        "the failure must say what is lost, not merely which key is wrong: {err}"
    );
}

#[test]
fn a_configured_session_passes_verification() {
    let ctx = session().expect("a configured session must verify");
    verify_settings(&ctx).expect("and must keep verifying");
}

#[test]
fn every_required_setting_actually_takes_effect() {
    // Applying a setting and having it take effect are different things: a key that is
    // silently ignored would leave the mechanism off while verification passed against
    // our own expectation rather than the engine's state.
    let ctx = SessionContext::new_with_config(apply_required_settings(SessionConfig::new()));
    let options = ctx.copied_config().options().clone();
    let entries = options.entries();

    for setting in REQUIRED {
        let entry = entries
            .iter()
            .find(|e| e.key == setting.key)
            .unwrap_or_else(|| panic!("{} is not a key the engine recognises", setting.key));
        assert_eq!(
            entry.value.as_deref(),
            Some(setting.value),
            "{} did not take effect",
            setting.key
        );
    }
}

#[test]
fn every_required_setting_explains_its_consequence() {
    // An operator reading a startup failure needs to know what breaks, not only which
    // key is wrong.
    for setting in REQUIRED {
        assert!(
            setting.because.len() > 40,
            "{} has no meaningful explanation",
            setting.key
        );
        assert!(
            setting.key.starts_with("datafusion."),
            "{} does not look like an engine setting",
            setting.key
        );
    }
}

#[test]
fn a_setting_that_is_already_correct_by_default_is_still_asserted() {
    // Bloom filters default to on. Asserting it anyway means a future default change
    // is caught at startup rather than absorbed silently.
    let bloom = REQUIRED
        .iter()
        .find(|s| s.key.ends_with("bloom_filter_on_read"))
        .expect("bloom filters should be asserted");
    assert_eq!(bloom.value, "true");
    assert!(
        bloom.because.contains("future default change"),
        "the reason should record why an already-correct default is still asserted"
    );

    // And confirm it really is the default today.
    let plain = SessionContext::new();
    let options = plain.copied_config().options().clone();
    let entry = options
        .entries()
        .into_iter()
        .find(|e| e.key == bloom.key)
        .expect("the key exists");
    assert_eq!(
        entry.value.as_deref(),
        Some("true"),
        "bloom filters are no longer on by default; the assertion has done its job"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_configured_session_can_actually_run_a_query() {
    // Verification must not be satisfiable by a session too broken to use.
    let ctx = session().expect("configured");
    let batches = ctx
        .sql("SELECT 1 AS one")
        .await
        .expect("plans")
        .collect()
        .await
        .expect("executes");
    assert_eq!(batches[0].num_rows(), 1);
}

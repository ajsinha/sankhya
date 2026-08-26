//! Required engine settings.

use datafusion::prelude::{SessionConfig, SessionContext};
use std::fmt;

/// A setting SANKHYA depends on, and why.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RequiredSetting {
    pub key: &'static str,
    pub value: &'static str,
    /// What is lost if it is wrong. Rendered into the failure message, so an operator
    /// sees the consequence rather than only the key.
    pub because: &'static str,
}

/// The settings SANKHYA requires, each with the reason it matters.
pub const REQUIRED: &[RequiredSetting] = &[
    RequiredSetting {
        key: "datafusion.execution.parquet.pushdown_filters",
        value: "true",
        because: "late materialization — evaluating predicates inside the Parquet \
                  decoder so payload columns are materialized only for surviving rows. \
                  Defaults to false. MEASURED NEUTRAL (1.02x) on a 5M-row, 523 MiB \
                  scan at 1-in-10,000 selectivity; see the pushdown_benefit test. It is \
                  required anyway because it is not harmful, it is expected to matter on \
                  wider payloads and higher selectivity, and a setting that is sometimes \
                  free and sometimes valuable is worth pinning either way",
    },
    RequiredSetting {
        key: "datafusion.execution.parquet.reorder_filters",
        value: "true",
        because: "evaluating cheap, selective predicates first. Without it filters run \
                  in written order, so an expensive one may be evaluated against rows a \
                  cheap one would have eliminated. Defaults to false",
    },
    RequiredSetting {
        key: "datafusion.execution.parquet.bloom_filter_on_read",
        value: "true",
        because: "skipping row groups that provably cannot contain a value. This one \
                  defaults to true, and is asserted so a future default change is \
                  caught rather than silently absorbed",
    },
];

/// Why the engine is not configured as SANKHYA requires.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SettingsError {
    pub key: &'static str,
    pub expected: &'static str,
    pub actual: String,
    pub because: &'static str,
}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "engine setting {} is {:?} but must be {:?}. This setting controls {}",
            self.key, self.actual, self.expected, self.because
        )
    }
}

impl std::error::Error for SettingsError {}

/// Apply every required setting to a configuration.
#[must_use]
pub fn apply_required_settings(mut config: SessionConfig) -> SessionConfig {
    for setting in REQUIRED {
        config = config.set_str(setting.key, setting.value);
    }
    config
}

/// Read the settings back and confirm each one took effect.
///
/// # Errors
///
/// Returns the first setting that is not as required, naming what is lost. Called at
/// startup rather than trusted, because a changed upstream default would otherwise
/// cost an order of magnitude with no symptom but slowness.
pub fn verify_settings(ctx: &SessionContext) -> Result<(), SettingsError> {
    let options = ctx.copied_config().options().clone();
    let entries = options.entries();

    for setting in REQUIRED {
        let actual = entries
            .iter()
            .find(|e| e.key == setting.key)
            .and_then(|e| e.value.clone())
            .unwrap_or_else(|| "unset".to_string());

        if actual != setting.value {
            return Err(SettingsError {
                key: setting.key,
                expected: setting.value,
                actual,
                because: setting.because,
            });
        }
    }
    Ok(())
}

/// Build a session configured as SANKHYA requires, and prove it.
///
/// # Errors
///
/// As [`verify_settings`]. A failure here is a startup failure: proceeding would mean
/// running at a fraction of the achievable speed with nothing to indicate why.
pub fn session() -> Result<SessionContext, SettingsError> {
    let ctx = SessionContext::new_with_config(apply_required_settings(SessionConfig::new()));
    verify_settings(&ctx)?;
    Ok(ctx)
}

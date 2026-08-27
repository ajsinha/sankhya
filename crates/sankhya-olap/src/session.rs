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

/// Filter pushdown, and why it is **not** in the list above.
///
/// # The reasoning it was added under
///
/// Late materialization — evaluating predicates inside the Parquet decoder so payload
/// columns are materialized only for surviving rows — is described everywhere, including
/// in this project's own architecture, as the single largest scan optimization available.
/// It defaults to off. Leaving a large win switched off with no symptom but slowness is
/// exactly the failure the assertion list exists to prevent, so it went in the list.
///
/// # What measurement found instead
///
/// It was measured neutral (1.02×) on a synthetic 5M-row scan, and pinned anyway on the
/// grounds that neutral is not harmful. Measured again on TPC-H at scale factor 1, it is
/// not neutral — it is a cost, at every selectivity tried:
///
/// | Rows surviving the filter | Off | On | |
/// |---|---|---|---|
/// | 1 in ~6,000,000 | 5.6 ms | 5.5 ms | 1.02× |
/// | 1 in ~1,500 | 4.5 ms | 4.8 ms | 0.94× |
/// | 1 in ~60 | 4.0 ms | 4.6 ms | 0.87× |
/// | 1 in ~7 | 116.6 ms | 162.0 ms | **0.72×** |
/// | all | 110.6 ms | 111.7 ms | 0.99× |
///
/// # Why it does not help, which matters more than that it does not
///
/// Late materialization saves the decode of payload columns for rows a predicate
/// eliminates. On this data those rows have already been eliminated — by row-group and
/// page statistics, before any decoding begins. The highly selective queries above
/// complete in four to six milliseconds because pruning left almost nothing to read, and
/// pushdown cannot save work that is not being done. What it adds is per-row bookkeeping
/// on the scan that remains.
///
/// **So the two mechanisms are not complementary here; the cheaper one has already won.**
/// That is a property of well-maintained statistics and sorted-enough data, which is what
/// the rest of this system exists to produce. It would look different on data with no
/// useful bounds, and that is where this setting should be reconsidered — per query,
/// from the statistics, rather than pinned on for everyone.
///
/// It is left at the engine's default rather than pinned off, because pinning a setting
/// off is still pinning it, and the evidence supports "not always" rather than "never".
pub const PUSHDOWN_FILTERS: &str = "datafusion.execution.parquet.pushdown_filters";

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

//! What a run concluded, in the shape somebody reads.

use crate::judge::{judge, Verdict};
use crate::measure::{Bound, Watched, WATCHED};
use crate::sample::Samples;
use std::fmt::Write as _;

/// Everything a run concluded.
#[derive(Clone, Debug)]
pub struct Report {
    /// Per measure, in declaration order.
    pub verdicts: Vec<(&'static Watched, Verdict)>,
    /// How long the run lasted, in seconds.
    pub span_seconds: i64,
    /// How far ahead a crossing still counts, in seconds.
    pub horizon_seconds: i64,
    /// How many opening samples were discarded from each measure.
    pub warm_up: usize,
}

/// The largest horizon a run may be asked about.
///
/// Derived from the span that is actually **judged** — the run minus its warm-up prefix —
/// rather than from the raw run. Every caller that computed this itself got it wrong by
/// exactly the warm-up on the first attempt, including both of the ones in this repository,
/// which is a good sign it does not belong at the call site.
///
/// A scheduled runner wants this too: it is the answer to "how long must this run to be able
/// to say anything about three weeks".
#[must_use]
pub fn supported_horizon(samples: &Samples) -> i64 {
    WATCHED
        .iter()
        .map(|measure| crate::judge::span_of(settled(samples.of(measure.name))))
        .max()
        .unwrap_or(0)
        .saturating_mul(crate::judge::EXTRAPOLATION_FACTOR)
}

/// The samples after the warm-up prefix.
///
/// A slice rather than a copy, and an empty slice when the run was shorter than the prefix —
/// which the judgement then reports as inconclusive rather than as steady.
#[must_use]
fn settled(samples: &[sankhya_diagnostic::projection::Observation]) -> &[sankhya_diagnostic::projection::Observation] {
    samples.get(WARM_UP_SAMPLES..).unwrap_or(&[])
}

/// How many opening samples are discarded before judging.
///
/// # Warm-up is not drift, and the difference is the whole measurement
///
/// A process allocates as it starts: session contexts, decoded batches, caches filling for
/// the first time. Every one of those is a one-off, and across the opening of a run they look
/// exactly like a linear climb — because over a short enough window, they are one.
///
/// The first run of this harness against the real server reported memory reaching its limit
/// in six minutes. It was not a leak. It was a third of a second of a process warming up,
/// extrapolated.
///
/// So a fixed, declared prefix is discarded, and the count is stated in the report. **Fixed
/// and declared** is what keeps this honest: discarding *until the series looks flat* would
/// hide every leak by construction, because a leak is precisely a series that does not go
/// flat. A run too short to spare the prefix is judged inconclusive, which is the correct
/// answer for a run too short to distinguish warm-up from drift.
pub const WARM_UP_SAMPLES: usize = 10;

impl Report {
    /// Judge a set of samples, discarding [`WARM_UP_SAMPLES`] from the opening of each.
    #[must_use]
    pub fn of(samples: &Samples, horizon_seconds: i64, now: i64) -> Self {
        let verdicts = WATCHED
            .iter()
            .map(|measure| {
                let reference = match measure.bound {
                    Bound::PerUnitOfWork { per, .. } => Some(samples.of(per)),
                    _ => None,
                };
                let verdict = judge(
                    measure,
                    settled(samples.of(measure.name)),
                    reference.map(settled),
                    horizon_seconds,
                    now,
                );
                (measure, verdict)
            })
            .collect();
        Self {
            verdicts,
            span_seconds: samples.span_seconds(),
            horizon_seconds,
            warm_up: WARM_UP_SAMPLES,
        }
    }

    /// Whether every measure was judged and every judgement was steady.
    ///
    /// Inconclusive counts as a failure. A soak whose sampling broke must not report the
    /// same green as one that ran properly, because the green is the thing everybody reads.
    #[must_use]
    pub fn passed(&self) -> bool {
        !self.verdicts.is_empty() && self.verdicts.iter().all(|(_, verdict)| verdict.passed())
    }

    /// The measures that did not pass.
    #[must_use]
    pub fn failures(&self) -> Vec<&(&'static Watched, Verdict)> {
        self.verdicts
            .iter()
            .filter(|(_, verdict)| !verdict.passed())
            .collect()
    }

    /// The report as a person reads it.
    #[must_use]
    pub fn describe(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "soak: {} over {}, judged against a {} horizon, first {} sample(s) discarded as \
             warm-up\n",
            if self.passed() { "PASS" } else { "FAIL" },
            sankhya_diagnostic::projection::human_duration(self.span_seconds),
            sankhya_diagnostic::projection::human_duration(self.horizon_seconds),
            self.warm_up,
        );
        for (measure, verdict) in &self.verdicts {
            let line = match verdict {
                Verdict::Steady => "steady".to_string(),
                Verdict::Growing { seconds, latest, means } => format!(
                    "GROWING — at {latest:.0} {} and reaching its limit in about {}.\n      {means}",
                    measure.unit,
                    sankhya_diagnostic::projection::human_duration(*seconds)
                ),
                Verdict::Breached { latest, means } => format!(
                    "BREACHED — {latest:.0} {} is already past the limit.\n      {means}",
                    measure.unit
                ),
                Verdict::Inconclusive { why } => format!("COULD NOT JUDGE — {why}"),
            };
            let _ = writeln!(out, "  {:<16} {line}", measure.name);
        }
        out
    }
}

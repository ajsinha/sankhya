//! Deciding whether a long run was clean.
//!
//! # "It did not crash" is not a result
//!
//! A soak that reports survival reports the one thing that was never in doubt. What it is
//! for is the class of failure invisible in any single sample and obvious across a week: a
//! measure that is fine now, fine in an hour, and crosses a line in three weeks.
//!
//! `M6`'s exit criterion is therefore not *clean*, which nobody can fail, but:
//!
//! > **A soak passes when no bounded measure has a projection that crosses its threshold
//! > within the observation horizon.**
//!
//! A measure trending upward with a crossing three weeks out is a **failure**. It is exactly
//! the failure that ships and gets diagnosed six months later by somebody else.
//!
//! # Inconclusive is a failure, not a pass
//!
//! Too few samples, or a shape no line describes, means the soak could not judge. That has
//! to be distinct from judging and finding nothing, and it has to fail --- otherwise a soak
//! whose sampling broke reports the same green as one that ran properly, and the green is
//! the thing everybody reads.
//!
//! It is the same distinction the diagnostic draws between a clean check and one that could
//! not run, arrived at from the other direction.

use crate::soak::measure::{Bound, Watched};
use crate::projection::{Observation, Trend};

/// How many windows a sawtooth is split into to find its peaks.
///
/// Twelve, so the peak series has enough points to trend while each window still spans
/// several cycles. Too few and one unlucky window decides the verdict; too many and a window
/// holds part of a cycle rather than a peak, which turns the peak series back into the raw
/// series it was extracted to avoid.
pub const PEAK_WINDOWS: usize = 12;

/// How much further ahead than the observed span a run may speak about.
///
/// # A run of half a second cannot say anything about three weeks
///
/// The first real run of this harness judged a 0.5-second sample window against a three-week
/// horizon — a factor of three and a half million — and reported memory reaching its limit
/// in nineteen minutes. Discarding more warm-up did not fix it and could not have: the whole
/// run was inside the ramp.
///
/// `sankhya-diagnostic` already carries this guard, as `EXTRAPOLATION_FACTOR`, and applying
/// it there and not here was the omission. A trend measured over an interval can speak about
/// a few multiples of that interval and no further. Past that the arithmetic still works and
/// stops being evidence.
///
/// The consequence is worth stating rather than hiding: **a short run cannot pass a long
/// horizon.** It reports that it was too short, which is true, and the answer is to run for
/// longer rather than to widen the limit.
pub const EXTRAPOLATION_FACTOR: i64 = 3;

/// How few peaks make a sawtooth judgement worthless.
///
/// Lower than [`FEWEST_SAMPLES`], and deliberately so. A peak is already the maximum of a
/// window holding many readings, so six of them carry the weight of the whole run — whereas
/// six *raw* samples carry six readings. Applying the raw minimum here was a conflict built
/// in by construction: the peak series can never be longer than [`PEAK_WINDOWS`], so a
/// threshold above it would report every sawtooth as unjudgeable forever.
pub const FEWEST_PEAKS: usize = 6;

/// What a soak concluded about one measure.
#[derive(Clone, PartialEq, Debug)]
pub enum Verdict {
    /// It is not going anywhere.
    Steady,
    /// It is heading for its limit, and this is when.
    Growing {
        /// How long until it crosses.
        seconds: i64,
        /// Where it was at the end of the run.
        latest: f64,
        /// What that means, from the declaration.
        means: &'static str,
    },
    /// It is already past its limit.
    Breached {
        /// Where it was at the end of the run.
        latest: f64,
        /// What that means.
        means: &'static str,
    },
    /// The run could not judge it.
    ///
    /// A failure. A soak whose sampling broke must not report the same green as one that
    /// ran properly.
    Inconclusive {
        /// Why not.
        why: String,
    },
}

impl Verdict {
    /// Whether this measure passed.
    #[must_use]
    pub const fn passed(&self) -> bool {
        matches!(self, Self::Steady)
    }
}

/// Judge one measure's samples against its declared bound.
///
/// `horizon` is how far ahead a crossing still counts as a failure, in seconds. It should be
/// far longer than the run: the whole point is to catch what would happen weeks after a run
/// that lasted hours.
#[must_use]
pub fn judge(
    measure: &Watched,
    samples: &[Observation],
    reference: Option<&[Observation]>,
    horizon: i64,
    now: i64,
) -> Verdict {
    match measure.bound {
        Bound::Steady { limit } => rising_to(measure, samples, limit, horizon, now),
        Bound::Sawtooth { limit } => {
            // The peaks, not the samples. A line through a sawtooth means nothing --- the
            // diagnostic refuses to draw one and is right to --- and the question here is
            // whether each cycle starts further behind than the last.
            let peaks = peaks_of(samples, PEAK_WINDOWS);
            // The span comes from the samples, not from the peaks. A peak sits *inside* its
            // window, so the peak series always spans less than the run it summarises — and
            // deriving the entitlement from it would shrink what the run may say about
            // itself by an amount that depends on where the peaks happened to fall.
            rising_to_at_least(measure, &peaks, limit, horizon, now, FEWEST_PEAKS, span_of(samples))
        }
        Bound::PerUnitOfWork { per, tolerance } => {
            let Some(reference) = reference else {
                return Verdict::Inconclusive {
                    why: format!(
                        "{} is judged per {per} and no samples of {per} were taken, so the \
                         ratio that matters could not be formed. The total alone can never \
                         show this: the total is supposed to rise",
                        measure.name
                    ),
                };
            };
            let Some(ratios) = ratio(samples, reference) else {
                return Verdict::Inconclusive {
                    why: format!(
                        "{} and {per} were not sampled at the same instants, so no ratio \
                         could be formed",
                        measure.name
                    ),
                };
            };
            // A ratio that drifts in *either* direction is the finding. Falling is worse:
            // rising means something is recorded twice, falling means something is not
            // recorded at all.
            let Some(first) = ratios.first().map(|o| o.value) else {
                return Verdict::Inconclusive {
                    why: format!("{} produced no ratio samples", measure.name),
                };
            };
            rising_to(measure, &ratios, first * (1.0 + tolerance), horizon, now)
        }
    }
}

/// How few samples make a judgement worthless.
///
/// A rate from two readings is a rate through two pieces of noise. Ten is not a statistical
/// threshold, it is the point below which a run has not observed enough to say anything, and
/// saying so is better than a confident slope through five points.
pub const FEWEST_SAMPLES: usize = 10;

/// The shared judgement: does this series reach `limit` within the horizon?
///
/// # This deliberately does not use `Projection`, and the reason is worth stating
///
/// [`Trend::time_until`] refuses to project through a series that does not fit a line, and it
/// is right to: a *diagnostic* giving a confident date from a sawtooth reports where in the
/// cycle the samples fell.
///
/// A soak asks a different question of the same numbers. "Is this drifting upward over
/// hours" is answered by the slope, and a healthy measure is **noisy and flat** --- which has
/// an r² near zero, because there is no trend to explain. Passing that through the linearity
/// gate reported the shape of every healthy measure as unjudgeable, so the baseline run
/// failed on memory, descriptors and the history file while nothing was wrong with any of
/// them.
///
/// It is the same trap as r² on a constant series, which had to be corrected in
/// `sankhya-math` earlier for the same reason: the statistic is undefined where there is
/// nothing to explain, and "undefined" is not "bad".
///
/// So the slope is used directly. Noise averages out across a run's worth of samples, a
/// non-positive slope is a pass, and a positive one is projected against the horizon --- which
/// is itself the guard against over-extrapolation, since a crossing beyond the horizon is not
/// something this run is claiming to know about.
fn rising_to(
    measure: &Watched,
    samples: &[Observation],
    limit: f64,
    horizon: i64,
    now: i64,
) -> Verdict {
    rising_to_at_least(
        measure,
        samples,
        limit,
        horizon,
        now,
        FEWEST_SAMPLES,
        span_of(samples),
    )
}

/// How long a series spans, in seconds.
#[must_use]
pub fn span_of(samples: &[Observation]) -> i64 {
    match (samples.first(), samples.last()) {
        (Some(first), Some(last)) => (last.at - first.at) / 1_000_000,
        _ => 0,
    }
}

/// [`rising_to`], with the minimum sample count given.
fn rising_to_at_least(
    measure: &Watched,
    samples: &[Observation],
    limit: f64,
    horizon: i64,
    _now: i64,
    fewest: usize,
    span: i64,
) -> Verdict {
    // What the run is entitled to speak about, before anything else is computed.
    let supported = span.saturating_mul(EXTRAPOLATION_FACTOR);
    if horizon > supported {
        return Verdict::Inconclusive {
            why: format!(
                "{} was observed for {}s and asked about {}s ahead — past about {}s the \
                 projection is arithmetic rather than evidence. Run for longer; do not widen \
                 the limit",
                measure.name, span, horizon, supported
            ),
        };
    }

    if samples.len() < fewest {
        return Verdict::Inconclusive {
            why: format!(
                "{} has {} sample(s) and {fewest} is the fewest worth judging — a rate \
                 from a handful of readings is a rate through a handful of noise",
                measure.name,
                samples.len()
            ),
        };
    }

    let trend = Trend::of(samples.iter().copied());
    let Some(latest) = trend.latest() else {
        return Verdict::Inconclusive {
            why: format!("{} was never sampled", measure.name),
        };
    };
    let Some(rate) = trend.rate_per_second() else {
        return Verdict::Inconclusive {
            why: format!(
                "{} produced no rate: every sample shares an instant",
                measure.name
            ),
        };
    };

    if latest.value >= limit {
        return Verdict::Breached {
            latest: latest.value,
            means: measure.means,
        };
    }
    if rate <= 0.0 {
        return Verdict::Steady;
    }

    let remaining = limit - latest.value;
    #[allow(clippy::cast_possible_truncation)]
    let seconds = (remaining / rate) as i64;
    if seconds <= horizon {
        return Verdict::Growing {
            seconds,
            latest: latest.value,
            means: measure.means,
        };
    }
    // Rising, and not reaching its limit inside the window this run can speak about. Saying
    // "steady" here is a claim about the horizon rather than about the measure, and the
    // horizon is stated in the report beside it.
    Verdict::Steady
}

/// The maximum in each of `windows` equal slices of the run.
///
/// Empty windows are skipped rather than reported as zero. A zero peak would drag the trend
/// downward and make a genuinely climbing sawtooth look steady, which is the one conclusion
/// this must never reach by accident.
#[must_use]
pub fn peaks_of(samples: &[Observation], windows: usize) -> Vec<Observation> {
    let (Some(first), Some(last)) = (samples.first(), samples.last()) else {
        return Vec::new();
    };
    let span = last.at.saturating_sub(first.at);
    if span <= 0 || windows == 0 {
        return Vec::new();
    }

    let mut peaks = Vec::with_capacity(windows);
    for window in 0..windows {
        #[allow(clippy::cast_possible_wrap)]
        let start = first.at + span * window as i64 / windows as i64;
        #[allow(clippy::cast_possible_wrap)]
        let end = first.at + span * (window as i64 + 1) / windows as i64;
        let inside: Vec<&Observation> = samples
            .iter()
            .filter(|sample| sample.at >= start && (sample.at < end || window + 1 == windows))
            .collect();
        let Some(peak) = inside
            .iter()
            .max_by(|a, b| a.value.partial_cmp(&b.value).unwrap_or(std::cmp::Ordering::Equal))
        else {
            continue;
        };
        peaks.push(Observation::new(peak.at, peak.value));
    }
    peaks
}

/// `samples` divided by `reference`, at the instants they share.
///
/// `None` when they share none. Interpolating between reference samples was the alternative
/// and it invents readings: the ratio is then partly a property of the interpolation, and a
/// drift in it cannot be told from a drift in the data.
#[must_use]
pub fn ratio(samples: &[Observation], reference: &[Observation]) -> Option<Vec<Observation>> {
    let mut out = Vec::new();
    for sample in samples {
        let Some(against) = reference.iter().find(|other| other.at == sample.at) else {
            continue;
        };
        if against.value == 0.0 {
            continue;
        }
        out.push(Observation::new(sample.at, sample.value / against.value));
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

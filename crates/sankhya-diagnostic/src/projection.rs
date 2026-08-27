//! Turning a measurement into a time.
//!
//! # The requirement, and the thing it does not say
//!
//! `FR-OPS-17` asks the diagnostic to report *when a problem becomes user-visible* rather
//! than its current value, and gives the example: **"compaction debt is 400 GB" is far less
//! actionable than "at the current write rate, query latency on this table will double in
//! about nine days"**.
//!
//! What it does not say, and what shapes everything here: **a time cannot be computed from a
//! single sample.** "400 GB" and "growing by 40 GB a day" are different kinds of fact, and
//! only the second yields a date. A diagnostic with one observation can report the value and
//! must not report a projection.
//!
//! That is uncomfortable, because the first time an operator runs a diagnostic there is
//! exactly one observation and the answer is "I cannot tell you yet". The alternative is
//! worse: a projection invented from one sample is a number with a date attached, and a date
//! is precisely what gets believed and acted on.
//!
//! So [`Projection::Unknown`] is a first-class outcome, it names what is missing, and no
//! check may skip it.

use sankhya_math::stats::{linear_fit, LinearFit};
use std::fmt;

/// One measurement of something, at a moment.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Observation {
    /// When, in microseconds from the epoch.
    pub at: i64,
    /// What was measured.
    pub value: f64,
}

impl Observation {
    /// A measurement.
    #[must_use]
    pub const fn new(at: i64, value: f64) -> Self {
        Self { at, value }
    }
}

/// The fewest observations from which a rate can be estimated at all.
///
/// Two give a slope. Two also give a slope through any pair of noise, so the projections
/// from exactly two are marked [`Confidence::Weak`] --- reported, because an operator with
/// two samples still wants the estimate, and marked, because acting on it as though it were
/// firm is how a maintenance window gets scheduled for the wrong week.
pub const MINIMUM_OBSERVATIONS: usize = 2;

/// Observations from which a rate can be estimated at all.
pub const FIRM_OBSERVATIONS: usize = 5;

/// How much further ahead than the observed span a projection may reach.
///
/// A trend measured over four days can say something about the next week or two. It says
/// nothing about six months from now, and a date six months out drawn from four days of
/// samples is a number with no evidence behind it and a calendar entry in front of it.
///
/// Three is a judgement, not a derivation. It is named here so that it can be argued with,
/// which is the point.
pub const EXTRAPOLATION_FACTOR: i64 = 3;

/// How much weight a projection deserves.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Confidence {
    /// Enough observations, and they lie close to a line.
    Firm,
    /// Enough to compute a slope, not enough to trust it.
    Weak,
}

/// What a series says about when a threshold will be crossed.
#[derive(Clone, PartialEq, Debug)]
pub enum Projection {
    /// It will cross, in this many seconds.
    Crossing {
        /// How long until it does.
        seconds: i64,
        /// How much weight the estimate deserves.
        confidence: Confidence,
        /// How well the observations fit a line, between zero and one.
        fit: f64,
    },
    /// It has already crossed.
    ///
    /// Distinct from crossing in zero seconds, because the response differs: one is a
    /// warning and the other is an incident.
    Already,
    /// It is moving away from the threshold, or not moving.
    Receding,
    /// It crosses on this trend, but too far out for these observations to support.
    ///
    /// Kept separate from a date because it *is* a different claim. Four days of samples
    /// projecting a crossing six months away is arithmetic, not evidence --- nothing in
    /// those four days says the rate survives the next six months, and the figure carries a
    /// precision the data cannot justify. An operator told "about 6 months" plans around it.
    Beyond {
        /// The furthest ahead these observations support, in seconds.
        horizon_seconds: i64,
    },
    /// Not enough is known to say.
    Unknown {
        /// What is missing.
        reason: Unknown,
    },
}

/// Why no time could be given.
///
/// Not `Eq`, because one variant carries a goodness-of-fit and comparing two floats for
/// equality is the thing this workspace's lints forbid everywhere else.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Unknown {
    /// Fewer than two observations, so there is no rate.
    ///
    /// The first run of a diagnostic is always this, and saying so is the honest answer.
    TooFewObservations {
        /// How many there were.
        have: usize,
    },
    /// The observations do not lie near a line, so a linear projection means nothing.
    ///
    /// A sawtooth --- debt accumulating and being compacted away --- fits a line badly by
    /// construction, and projecting through one produces a date derived from where in the
    /// cycle the samples happened to fall.
    NotLinear {
        /// How well it fitted, between zero and one.
        fit: f64,
    },
    /// Every observation was taken at the same instant.
    NoElapsedTime,
}

impl Projection {
    /// Whether this warrants telling somebody now.
    #[must_use]
    pub const fn is_actionable(&self) -> bool {
        matches!(self, Self::Already | Self::Crossing { .. })
    }

    /// A sentence an operator can act on.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Already => "this threshold has already been crossed".to_string(),
            Self::Receding => "it is moving away from the threshold".to_string(),
            Self::Beyond { horizon_seconds } => format!(
                "it is heading that way, but the crossing is further out than these observations support (they span too little to see past about {}); a date from here would be arithmetic rather than evidence",
                human_duration(*horizon_seconds)
            ),
            Self::Crossing {
                seconds,
                confidence,
                ..
            } => {
                let duration = human_duration(*seconds);
                match confidence {
                    Confidence::Firm => format!("at the current rate, about {duration}"),
                    // The caveat goes after the figure rather than inside it. Interrupting
                    // the sentence to hedge --- "very roughly, on only two observations, 1
                    // day" --- buries the number an operator is reading the line for.
                    Confidence::Weak => format!(
                        "at the current rate, roughly {duration} — but on only \
                         two observations, so treat it as a direction rather than a date"
                    ),
                }
            }
            Self::Unknown { reason } => match reason {
                Unknown::TooFewObservations { have } => format!(
                    "no projection is possible from {have} observation(s): a time needs a \
                     rate, and a rate needs at least {MINIMUM_OBSERVATIONS}"
                ),
                Unknown::NotLinear { fit } => format!(
                    "the measurements do not follow a line closely enough to project \
                     (fit {fit:.2}); a sawtooth fits a line badly by construction, and a \
                     date drawn through one says where in the cycle the samples fell"
                ),
                Unknown::NoElapsedTime => {
                    "every observation was taken at the same instant".to_string()
                }
            },
        }
    }
}

impl fmt::Display for Projection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.describe())
    }
}

/// A duration in the units a person thinks in.
///
/// Rounded deliberately coarsely. A projection accurate to the second implies a precision
/// the estimate does not have, and an operator reading "in 9 days" plans differently from
/// one reading "in 9 days, 4 hours and 12 minutes" --- who reasonably assumes somebody knows
/// that much.
#[must_use]
pub fn human_duration(seconds: i64) -> String {
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;

    // Singular where the count is one. "in 1 days" is the sort of thing that makes an
    // operator trust the rest of the line less, and they are right to.
    fn plural(count: i64, unit: &str) -> String {
        if count == 1 {
            format!("1 {unit}")
        } else {
            format!("{count} {unit}s")
        }
    }

    if seconds < MINUTE {
        "less than a minute".to_string()
    } else if seconds < HOUR {
        plural(seconds / MINUTE, "minute")
    } else if seconds < DAY {
        plural(seconds / HOUR, "hour")
    } else if seconds < 90 * DAY {
        plural(seconds / DAY, "day")
    } else {
        plural(seconds / (30 * DAY), "month")
    }
}

/// Which direction of travel is the problem.
///
/// This cannot be inferred from the slope, and inferring it was a real defect: a measure
/// *falling* while the threshold sits above it is receding, and a slope-based reading called
/// it already-crossed. Both directions occur --- compaction debt rising to a ceiling, free
/// space falling to zero --- and only the caller knows which one it is watching for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Concern {
    /// The measure growing up to the threshold is the problem.
    RisingTo,
    /// The measure shrinking down to the threshold is the problem.
    FallingTo,
}

/// A series of measurements of one thing.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Trend {
    observations: Vec<Observation>,
}

impl Trend {
    /// A trend from observations, in any order.
    #[must_use]
    pub fn of(observations: impl IntoIterator<Item = Observation>) -> Self {
        let mut observations: Vec<Observation> = observations.into_iter().collect();
        // Sorted by time, so a caller that appended out of order still gets a slope with
        // the sign the data has rather than the sign the insertion order gave it.
        observations.sort_by(|a, b| a.at.cmp(&b.at));
        Self { observations }
    }

    /// Add a measurement.
    pub fn observe(&mut self, observation: Observation) {
        self.observations.push(observation);
        self.observations.sort_by(|a, b| a.at.cmp(&b.at));
    }

    /// How many measurements there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.observations.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.observations.is_empty()
    }

    /// The most recent measurement.
    #[must_use]
    pub fn latest(&self) -> Option<Observation> {
        self.observations.last().copied()
    }

    /// The fitted line through the observations, if one can be fitted.
    fn line(&self) -> Option<LinearFit> {
        if self.observations.len() < MINIMUM_OBSERVATIONS {
            return None;
        }
        #[allow(clippy::cast_precision_loss)]
        let times: Vec<f64> = self.observations.iter().map(|o| o.at as f64).collect();
        let values: Vec<f64> = self.observations.iter().map(|o| o.value).collect();
        linear_fit(&times, &values).ok()
    }

    /// Every observation, oldest first.
    #[must_use]
    pub fn observations(&self) -> &[Observation] {
        &self.observations
    }

    /// How long the observations span, in seconds.
    ///
    /// Zero when there are fewer than two, or when they share an instant.
    #[must_use]
    pub fn observed_span_seconds(&self) -> i64 {
        match (self.observations.first(), self.observations.last()) {
            (Some(first), Some(last)) => (last.at - first.at) / 1_000_000,
            _ => 0,
        }
    }

    /// How fast the measure is changing, per second.
    ///
    /// `None` when there are too few observations or they all share an instant. Not zero:
    /// "not changing" and "cannot tell" are different facts and only one of them justifies
    /// doing nothing.
    #[must_use]
    pub fn rate_per_second(&self) -> Option<f64> {
        // The fit's slope is per microsecond, since that is the unit of `at`.
        self.line().map(|fit| fit.slope * 1_000_000.0)
    }

    /// When the measure will reach `threshold`.
    ///
    /// `now` is supplied rather than read from a clock, so a projection can be replayed
    /// exactly and tested without waiting.
    #[must_use]
    pub fn time_until(&self, threshold: f64, concern: Concern, now: i64) -> Projection {
        let Some(latest) = self.latest() else {
            return Projection::Unknown {
                reason: Unknown::TooFewObservations { have: 0 },
            };
        };

        // Which direction is trouble comes from the caller, not from the slope. Reading it
        // from the slope was a defect: a measure falling while the threshold sits above it
        // is receding, and the slope said it had already crossed.
        let already = match concern {
            Concern::RisingTo => latest.value >= threshold,
            Concern::FallingTo => latest.value <= threshold,
        };
        if already {
            return Projection::Already;
        }

        if self.observations.len() < MINIMUM_OBSERVATIONS {
            return Projection::Unknown {
                reason: Unknown::TooFewObservations {
                    have: self.observations.len(),
                },
            };
        }
        let Some(fit) = self.line() else {
            return Projection::Unknown {
                reason: Unknown::NoElapsedTime,
            };
        };
        // Asked before the direction, and the order is load-bearing. A sawtooth averages to
        // roughly no slope, so a direction test reached first calls it *receding* --- an
        // affirmative all-clear drawn from data that supports no conclusion at all.
        //
        // Below this the observations are not describing a line, and a date drawn through
        // them reports where in a cycle the samples fell rather than a trend. A sawtooth ---
        // debt accumulating and being compacted away --- is exactly that shape.
        const LINEAR_ENOUGH: f64 = 0.80;
        if fit.r_squared < LINEAR_ENOUGH && self.observations.len() > MINIMUM_OBSERVATIONS {
            return Projection::Unknown {
                reason: Unknown::NotLinear { fit: fit.r_squared },
            };
        }

        // Moving the wrong way for the concern, or not moving at all.
        let approaching = match concern {
            Concern::RisingTo => fit.slope > 0.0,
            Concern::FallingTo => fit.slope < 0.0,
        };
        if !approaching {
            return Projection::Receding;
        }

        // Solve the fitted line for the threshold, then subtract now.
        let crossing_at = (threshold - fit.intercept) / fit.slope;
        let micros = crossing_at - now as f64;
        if micros <= 0.0 {
            // The line says it should already have crossed and the latest value says
            // otherwise. Trusting the line over the measurement would report an incident
            // that is not happening.
            return Projection::Receding;
        }

        #[allow(clippy::cast_possible_truncation)]
        let seconds = (micros / 1_000_000.0) as i64;

        // Past this the linear model is arithmetic rather than evidence. See
        // `EXTRAPOLATION_FACTOR`.
        let horizon_seconds = self.observed_span_seconds() * EXTRAPOLATION_FACTOR;
        if seconds > horizon_seconds {
            return Projection::Beyond { horizon_seconds };
        }
        Projection::Crossing {
            seconds,
            confidence: if self.observations.len() >= FIRM_OBSERVATIONS {
                Confidence::Firm
            } else {
                Confidence::Weak
            },
            fit: fit.r_squared,
        }
    }
}

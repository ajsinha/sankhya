//! What a metric *is*, before anything records one.
//!
//! # The catalogue is the API, not documentation of it
//!
//! In most metrics libraries a metric is a string, and any string works. That is convenient
//! and it is how a dashboard comes to carry a series with no documented meaning, no unit, no
//! owner and no bound on its cardinality --- and then somebody builds an alert on it.
//!
//! Here a metric is a `&'static Metric`, and the only way to obtain a handle is to pass one.
//! There is no `counter("some_name")`. An undeclared metric is not refused at runtime; it is
//! **unrepresentable**, which is a different and much stronger property: it cannot be typed.
//!
//! # Labels are where the two dangerous things live
//!
//! `ARCHITECTURE` §17.1: *"No log line, trace attribute or metric label may contain tenant
//! data."* And separately, unbounded label cardinality is how a metrics system dies.
//!
//! Both are the same mistake seen from different sides --- putting a *value* where a
//! *dimension* belongs --- so both are handled in one place. A label declares what it may
//! hold:
//!
//! - [`Values::Closed`] names every permitted value. A value outside the set is refused, so
//!   such a label can never carry a row, a query, or a customer's name.
//! - [`Values::Identifier`] is for names that grow with the deployment --- tables, tenants
//!   --- and carries a **cap**. Past the cap, new series are refused and counted rather than
//!   created.
//!
//! There is deliberately no third variant. A label that varies per row or per query has no
//! way to be declared, which is the point.

/// What kind of measurement it is.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    /// Only ever increases. Reset to zero on restart, which is why alerts use rates.
    Counter,
    /// Goes up and down. The current value of something.
    Gauge,
    /// A distribution, summarised into cumulative buckets.
    Histogram {
        /// Upper bounds, ascending. `+Inf` is implicit and must not be listed.
        buckets: &'static [f64],
    },
}

impl Kind {
    /// The name Prometheus uses in a `# TYPE` line.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Counter => "counter",
            Self::Gauge => "gauge",
            Self::Histogram { .. } => "histogram",
        }
    }
}

/// What the number counts, so a dashboard need not guess.
///
/// A unit written into the catalogue rather than into the metric's name, because a name that
/// carries its unit (`..._bytes_total`) is right until the unit changes and then it is a lie
/// that no compiler can catch.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unit {
    /// A plain count of events or things.
    Count,
    /// Bytes.
    Bytes,
    /// Seconds. Never milliseconds --- a mixed-unit dashboard is arithmetic waiting to
    /// happen, and the base unit is the one convention agrees on.
    Seconds,
    /// A proportion between zero and one.
    Ratio,
}

impl Unit {
    /// How it is written in the catalogue and in the generated documentation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Count => "count",
            Self::Bytes => "bytes",
            Self::Seconds => "seconds",
            Self::Ratio => "ratio",
        }
    }
}

/// What a label is permitted to hold.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Values {
    /// Exactly these values, and nothing else.
    ///
    /// The enforcement that makes the tenant-data prohibition structural rather than a
    /// convention: a label whose permitted values are `ok` and `error` cannot be handed a
    /// customer's name, however the call site is written.
    Closed(&'static [&'static str]),
    /// A name that grows with the deployment --- a table, a tenant --- bounded by a cap.
    ///
    /// The cap is not a guess about how many there will be. It is the point past which the
    /// metric stops being worth its cost, and reaching it is itself information.
    Identifier {
        /// How many distinct values may exist before new ones are refused.
        cap: usize,
    },
}

/// One dimension of a metric.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Label {
    /// The label's name.
    pub name: &'static str,
    /// What it may hold.
    pub values: Values,
}

impl Label {
    /// A label restricted to a fixed set of values.
    #[must_use]
    pub const fn closed(name: &'static str, values: &'static [&'static str]) -> Self {
        Self {
            name,
            values: Values::Closed(values),
        }
    }

    /// A label holding a deployment-scoped name, bounded by a cap.
    #[must_use]
    pub const fn identifier(name: &'static str, cap: usize) -> Self {
        Self {
            name,
            values: Values::Identifier { cap },
        }
    }

    /// Whether this value is permitted.
    #[must_use]
    pub fn permits(&self, value: &str) -> bool {
        match self.values {
            Values::Closed(allowed) => allowed.contains(&value),
            // An identifier is bounded by count rather than by content, so the check that
            // matters happens in the registry, which is the only thing that knows how many
            // there already are.
            Values::Identifier { .. } => !value.is_empty(),
        }
    }
}

/// Which of the four groups a metric belongs to.
///
/// `ARCHITECTURE` §17.1 names them: *"query behaviour, resource pressure, pipeline health,
/// and maintenance debt"*. The grouping is in the catalogue rather than in a naming
/// convention, so that a metric cannot be in the wrong group merely by being named badly.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Group {
    /// What queries are doing.
    Query,
    /// What the process is running out of.
    Resource,
    /// Whether capture, apply and publication are keeping up.
    Pipeline,
    /// Work that has accumulated and has not been done.
    Maintenance,
}

impl Group {
    /// Its name in the generated catalogue.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Query => "query behaviour",
            Self::Resource => "resource pressure",
            Self::Pipeline => "pipeline health",
            Self::Maintenance => "maintenance debt",
        }
    }
}

/// Why a metric may wake somebody, and what they should do.
///
/// `ARCHITECTURE` §17.1 permits paging on a metric only where it *"precedes a user-visible
/// failure by a predictable interval"*. That interval is recorded here rather than left
/// implicit, because it is the entire justification for paging: an alert with no lead time
/// is an alert that fires at the same moment the user notices, which is a notification and
/// not an alert.
///
/// The runbook is required by construction --- the field is not an `Option`. `M6`'s fifth
/// exit criterion asks for a runbook for every alert that can page, and a type that permits
/// a paging metric without one makes that criterion something to be audited rather than
/// something that holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Alert {
    /// The runbook's file stem under `docs/runbooks/`.
    pub runbook: &'static str,
    /// What is about to break, in the words an operator woken at 03:00 needs.
    pub consequence: &'static str,
    /// How long there usually is between the alert firing and users noticing.
    pub lead_time: &'static str,
}

/// One declared metric.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Metric {
    /// The exported name.
    pub name: &'static str,
    /// Counter, gauge or histogram.
    pub kind: Kind,
    /// What the number counts.
    pub unit: Unit,
    /// Its dimensions.
    pub labels: &'static [Label],
    /// Which group it belongs to.
    pub group: Group,
    /// What it means, in a sentence.
    pub help: &'static str,
    /// Whether it may page, and if so, what to do.
    pub alert: Option<Alert>,
}

impl Metric {
    /// The label with this name, if it is declared.
    #[must_use]
    pub fn label(&self, name: &str) -> Option<&Label> {
        self.labels.iter().find(|label| label.name == name)
    }

    /// Whether reaching a threshold on this metric may wake somebody.
    #[must_use]
    pub const fn pages(&self) -> bool {
        self.alert.is_some()
    }
}

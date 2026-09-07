//! Where the numbers live.
//!
//! # Observability must not be able to break what it observes
//!
//! Every rejection here is counted and none of them fails a caller. A metric call that
//! returned an error would put a `?` on a line that exists only to record something, and the
//! first time that error was propagated instead of ignored, a mis-labelled counter would
//! take down a query. So the registry refuses quietly and **says how often it refused** ---
//! see [`Registry::rejections`], which is itself exported.
//!
//! Silence would be the other failure: a typo that produces a metric nobody notices is
//! missing. That case is handled a layer up rather than here, by making the metric name
//! unforgeable --- a caller passes the `&'static Metric` itself, so there is no string to
//! mistype.
//!
//! # Order is deterministic, on purpose
//!
//! A `BTreeMap` rather than a hash map, so a scrape renders identically for identical state.
//! A diff between two scrapes should show what changed, not where the hasher put things.

use crate::metric::{Kind, Label, Metric, Values};
use parking_lot::RwLock;
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// A series: one metric at one combination of label values.
type SeriesKey = (&'static str, Vec<(String, String)>);

/// What a series holds.
#[derive(Clone, Debug, Default)]
struct Series {
    /// For a counter or gauge.
    value: f64,
    /// For a histogram: how many observations fell at or below each declared bound.
    buckets: Vec<u64>,
    /// For a histogram: the total of the observed values, and how many there were.
    sum: f64,
    count: u64,
}

/// Why a recording was refused.
///
/// Each is a distinct mistake with a distinct fix, so they are counted separately. A single
/// "rejected" counter would tell an operator that something is wrong and nothing about what.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Rejections {
    /// A value outside a closed label's permitted set.
    ///
    /// The tenant-data prohibition doing its job, most likely: somebody put a value where a
    /// dimension belongs.
    pub value_not_permitted: u64,
    /// A label the metric does not declare.
    pub label_not_declared: u64,
    /// A declared label that was not supplied.
    ///
    /// Refused rather than defaulted to an empty string, because a series silently missing a
    /// dimension aggregates with series that have it and quietly doubles them.
    pub label_missing: u64,
    /// A new series that would have taken an identifier label past its cap.
    pub over_cap: u64,
}

impl Rejections {
    /// Whether anything at all was refused.
    #[must_use]
    pub const fn any(&self) -> bool {
        self.value_not_permitted > 0
            || self.label_not_declared > 0
            || self.label_missing > 0
            || self.over_cap > 0
    }

    /// How many recordings were refused in total.
    #[must_use]
    pub const fn total(&self) -> u64 {
        self.value_not_permitted + self.label_not_declared + self.label_missing + self.over_cap
    }
}

/// Everything recorded so far.
#[derive(Debug)]
pub struct Registry {
    inner: RwLock<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    series: BTreeMap<SeriesKey, Series>,
    /// Distinct values seen for each identifier label, so the cap can be enforced.
    ///
    /// Keyed by metric and label rather than by label alone: two metrics labelled by table
    /// have independent budgets, because one of them being expensive is not a reason to stop
    /// recording the other.
    seen: BTreeMap<(&'static str, &'static str), BTreeMap<String, ()>>,
    rejections: Rejections,
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

impl Registry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(Inner::default()),
        }
    }

    /// Add to a counter.
    ///
    /// Nothing happens if the labels are wrong, beyond a rejection being counted.
    pub fn increment(&self, metric: &'static Metric, labels: &[(&str, &str)], by: f64) {
        self.record(metric, labels, |series| series.value += by);
    }

    /// Set a gauge.
    pub fn set(&self, metric: &'static Metric, labels: &[(&str, &str)], value: f64) {
        self.record(metric, labels, |series| series.value = value);
    }

    /// Observe a value into a histogram.
    pub fn observe(&self, metric: &'static Metric, labels: &[(&str, &str)], value: f64) {
        let Kind::Histogram { buckets } = metric.kind else {
            // Not a rejection worth counting: it is a type error the catalogue already
            // records, and it cannot arise from data. Doing nothing is the least harmful
            // response available without a panic.
            return;
        };
        self.record(metric, labels, |series| {
            if series.buckets.len() != buckets.len() {
                series.buckets = vec![0; buckets.len()];
            }
            for (index, bound) in buckets.iter().enumerate() {
                if value <= *bound {
                    if let Some(slot) = series.buckets.get_mut(index) {
                        *slot += 1;
                    }
                }
            }
            series.sum += value;
            series.count += 1;
        });
    }

    /// The one path that validates, then mutates.
    fn record(
        &self,
        metric: &'static Metric,
        labels: &[(&str, &str)],
        mutate: impl FnOnce(&mut Series),
    ) {
        let mut inner = self.inner.write();

        // Every supplied label is declared, and holds a permitted value.
        for (name, value) in labels {
            let Some(declared) = metric.label(name) else {
                inner.rejections.label_not_declared += 1;
                return;
            };
            if !declared.permits(value) {
                inner.rejections.value_not_permitted += 1;
                return;
            }
        }
        // ...and every declared label is supplied. Checked in both directions, because a
        // missing dimension does not produce a missing series --- it produces a series that
        // aggregates with the ones that have it.
        if labels.len() != metric.labels.len() {
            inner.rejections.label_missing += 1;
            return;
        }

        // Identifier labels are bounded by count. The budget is charged only when a value is
        // new, so a hot table does not consume it repeatedly.
        for declared in metric.labels {
            let Values::Identifier { cap } = declared.values else {
                continue;
            };
            let Some((_, value)) = labels.iter().find(|(name, _)| *name == declared.name) else {
                continue;
            };
            if !Self::admit_identifier(&mut inner, metric, declared, value, cap) {
                return;
            }
        }

        let key: SeriesKey = (
            metric.name,
            // Sorted, so that the same labels supplied in a different order are the same
            // series rather than two of them.
            {
                let mut pairs: Vec<(String, String)> = labels
                    .iter()
                    .map(|(n, v)| ((*n).to_string(), (*v).to_string()))
                    .collect();
                pairs.sort();
                pairs
            },
        );
        mutate(inner.series.entry(key).or_default());
    }

    /// Whether a new identifier value fits within the metric's budget.
    fn admit_identifier(
        inner: &mut Inner,
        metric: &'static Metric,
        declared: &Label,
        value: &str,
        cap: usize,
    ) -> bool {
        let seen = inner
            .seen
            .entry((metric.name, declared.name))
            .or_default();
        if seen.contains_key(value) {
            return true;
        }
        if seen.len() >= cap {
            // The metric goes incomplete rather than unbounded, and the incompleteness is
            // visible. Growing without limit would take the process down; dropping without
            // saying so would make a dashboard quietly wrong, which is worse than a gap
            // because a gap is noticed.
            inner.rejections.over_cap += 1;
            return false;
        }
        seen.insert(value.to_string(), ());
        true
    }

    /// How many recordings were refused, and for which reason.
    #[must_use]
    pub fn rejections(&self) -> Rejections {
        self.inner.read().rejections
    }

    /// How many distinct series exist for a metric.
    #[must_use]
    pub fn series_count(&self, metric: &'static Metric) -> usize {
        self.inner
            .read()
            .series
            .keys()
            .filter(|(name, _)| *name == metric.name)
            .count()
    }

    /// The current value of one series, for tests and for the diagnostic.
    #[must_use]
    pub fn value(&self, metric: &'static Metric, labels: &[(&str, &str)]) -> Option<f64> {
        let mut pairs: Vec<(String, String)> = labels
            .iter()
            .map(|(n, v)| ((*n).to_string(), (*v).to_string()))
            .collect();
        pairs.sort();
        self.inner
            .read()
            .series
            .get(&(metric.name, pairs))
            .map(|series| series.value)
    }

    /// How many observations a histogram series holds.
    #[must_use]
    pub fn observation_count(&self, metric: &'static Metric, labels: &[(&str, &str)]) -> u64 {
        let mut pairs: Vec<(String, String)> = labels
            .iter()
            .map(|(n, v)| ((*n).to_string(), (*v).to_string()))
            .collect();
        pairs.sort();
        self.inner
            .read()
            .series
            .get(&(metric.name, pairs))
            .map_or(0, |series| series.count)
    }

    /// Render every series in the Prometheus text exposition format.
    ///
    /// `catalogue` is passed rather than held, so that a registry can render a subset and so
    /// that the help and type lines come from the declaration rather than from whatever
    /// happened to be recorded.
    #[must_use]
    pub fn render(&self, catalogue: &[&'static Metric]) -> String {
        let inner = self.inner.read();
        let mut out = String::new();
        for metric in catalogue {
            let series: Vec<_> = inner
                .series
                .iter()
                .filter(|((name, _), _)| *name == metric.name)
                .collect();
            let _ = writeln!(out, "# HELP {} {}", metric.name, metric.help);
            let _ = writeln!(out, "# TYPE {} {}", metric.name, metric.kind.as_str());

            // `# HELP` and `# TYPE` are not a sample, and Prometheus stores nothing for a
            // metric that has never been sampled --- so a declaration alone leaves *healthy*,
            // *never started* and *the collector is broken* byte-identical to anything
            // scraping. On a freshly started server, seven of the declared metrics were in
            // that state, including `sankhya_audit_unwritten_total`, the only page whose lead
            // time is *none*.
            //
            // So every combination a metric **can** be emitted at is emitted, at zero, unless
            // something has been recorded at it. Not "unless something has been recorded at
            // the metric": the first version branched on the whole metric being empty, and
            // that is a different rule with a much shorter life. One successful query records
            // `outcome="ok"`, the metric stops being empty, and `error`, `refused` and
            // `cancelled` **vanish from the scrape** --- so the panel a dashboard draws on the
            // error rate went blank on the first success and stayed blank until the first
            // failure, which is precisely when it is being read.
            //
            // A metric with a bounded label gets no zeros: its values are discovered from the
            // deployment, and inventing a table name is worse than saying nothing.
            let recorded: std::collections::BTreeSet<&Vec<(String, String)>> =
                series.iter().map(|((_, labels), _)| labels).collect();
            for labels in zero_combinations(metric) {
                if !recorded.contains(&labels) {
                    render_series(&mut out, metric, &labels, &Series::default());
                }
            }
            for ((_, labels), value) in series {
                render_series(&mut out, metric, labels, value);
            }
        }
        out
    }
}

/// Every label combination a metric can be emitted at before anything has happened.
///
/// One empty combination for a metric with no labels, the full cross-product for one whose
/// labels are all closed, and **none** for one with a bounded label --- whose values are
/// discovered from the deployment, so a zero series would be a name this server invented.
fn zero_combinations(metric: &Metric) -> Vec<Vec<(String, String)>> {
    let mut combinations: Vec<Vec<(String, String)>> = vec![Vec::new()];
    for label in metric.labels {
        let Values::Closed(values) = label.values else {
            return Vec::new();
        };
        combinations = combinations
            .into_iter()
            .flat_map(|so_far| {
                values.iter().map(move |value| {
                    let mut next = so_far.clone();
                    next.push((label.name.to_string(), (*value).to_string()));
                    next
                })
            })
            .collect();
    }
    // Sorted by name, because `record` sorts and the two sets are compared for equality: a
    // metric with two closed labels would otherwise emit its zero in declaration order and its
    // recorded value in sorted order, and every zero would survive alongside the real series it
    // was supposed to be replaced by. No metric declares two closed labels today, which is
    // exactly why this would have been found late.
    for combination in &mut combinations {
        combination.sort_by(|(left, _), (right, _)| left.cmp(right));
    }
    combinations
}

/// One series, in whichever shape its kind requires.
fn render_series(out: &mut String, metric: &Metric, labels: &[(String, String)], series: &Series) {
    let rendered = render_labels(labels, None);
    match metric.kind {
        Kind::Counter | Kind::Gauge => {
            let _ = writeln!(out, "{}{rendered} {}", metric.name, series.value);
        }
        Kind::Histogram { buckets } => {
            for (index, bound) in buckets.iter().enumerate() {
                let count = series.buckets.get(index).copied().unwrap_or(0);
                let with_le = render_labels(labels, Some(&bound.to_string()));
                let _ = writeln!(out, "{}_bucket{with_le} {count}", metric.name);
            }
            // `+Inf` is required and is always the total count. Omitting it makes the
            // histogram unusable rather than merely incomplete: quantile estimation needs
            // to know how many observations exceeded every declared bound.
            let with_inf = render_labels(labels, Some("+Inf"));
            let _ = writeln!(out, "{}_bucket{with_inf} {}", metric.name, series.count);
            let _ = writeln!(out, "{}_sum{rendered} {}", metric.name, series.sum);
            let _ = writeln!(out, "{}_count{rendered} {}", metric.name, series.count);
        }
    }
}

/// `{a="1",b="2"}`, or the empty string when there are none.
fn render_labels(labels: &[(String, String)], le: Option<&str>) -> String {
    if labels.is_empty() && le.is_none() {
        return String::new();
    }
    let mut parts: Vec<String> = labels
        .iter()
        .map(|(name, value)| format!("{name}=\"{}\"", escape(value)))
        .collect();
    if let Some(bound) = le {
        parts.push(format!("le=\"{bound}\""));
    }
    format!("{{{}}}", parts.join(","))
}

/// The exposition format's three escapes.
///
/// An identifier label carries a table name, and a table name is not guaranteed to be free
/// of a quote or a backslash. An unescaped one produces a scrape the collector rejects
/// wholesale --- so one awkward table name would take away every metric, not just its own.
fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

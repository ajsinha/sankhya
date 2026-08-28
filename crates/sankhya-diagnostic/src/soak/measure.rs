//! What is watched, and what "bounded" means for each thing.
//!
//! # Not everything that grows is a leak
//!
//! The naive soak watches every number and complains when one goes up. Half of them are
//! supposed to: queries served, audit records, bytes written. Complaining about those trains
//! everybody to ignore the report, which is the failure mode of every monitoring system that
//! has ever been switched off.
//!
//! So a measure declares **what kind of bounded it is**, and there are three kinds --- not
//! one, and the difference between them is where the interesting failures live.

/// How a measure is supposed to behave over a long run.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Bound {
    /// It must not trend upward at all.
    ///
    /// Resident memory, open file descriptors, distinct metric series. Anything here with a
    /// slope is a leak, and the only question is how long until it matters.
    Steady {
        /// The value at which it stops being a curiosity.
        limit: f64,
    },
    /// It grows with work done, and the **ratio** is what must be steady.
    ///
    /// Audit records are supposed to grow --- one per query. What must not grow is *records
    /// per query*: if that trends up, the audit is recording something twice, and the total
    /// alone can never show it because the total is supposed to rise.
    PerUnitOfWork {
        /// The measure this is counted against.
        per: &'static str,
        /// How far the ratio may drift before it means something.
        tolerance: f64,
    },
    /// It climbs and is reclaimed, repeatedly.
    ///
    /// Live file count is the example: writes add files, compaction removes them. The series
    /// is a sawtooth, a line through it means nothing --- the diagnostic refuses to project
    /// through one, correctly --- and the question a soak actually asks is different:
    /// **are the peaks getting higher?**
    ///
    /// A sawtooth whose troughs return to the same floor is a system keeping up. One whose
    /// peaks climb is a system falling behind, and it looks identical at any single moment.
    Sawtooth {
        /// The peak at which falling behind becomes visible to a user.
        limit: f64,
    },
}

/// Something watched across a long run.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Watched {
    /// What it is called.
    pub name: &'static str,
    /// What it counts.
    pub unit: &'static str,
    /// How it is supposed to behave.
    pub bound: Bound,
    /// What a breach means, in the words somebody reading the report needs.
    ///
    /// Not "memory grew". A soak report naming a measure and a slope is a puzzle; one saying
    /// what the slope implies is a finding.
    pub means: &'static str,
}

/// Everything a soak watches.
pub static WATCHED: &[Watched] = &[
    Watched {
        name: "resident_bytes",
        unit: "bytes",
        bound: Bound::Steady {
            limit: 8.0 * 1024.0 * 1024.0 * 1024.0,
        },
        means: "memory is being retained across requests. Under a long run this ends as the \
                process being killed by the kernel, at a moment nobody chose.",
    },
    Watched {
        name: "open_files",
        unit: "count",
        bound: Bound::Steady { limit: 1024.0 },
        means: "descriptors are not being returned. It ends as a refusal to accept \
                connections or to open a Parquet file, reported as an I/O error that looks \
                like a storage fault.",
    },
    Watched {
        name: "metric_series",
        unit: "count",
        bound: Bound::Steady { limit: 10_000.0 },
        means: "a label is taking values nobody bounded. The cap refuses new series and \
                counts the refusals, so the metric goes incomplete rather than unbounded --- \
                but an incomplete metric is a dashboard that is quietly wrong.",
    },
    Watched {
        name: "history_bytes",
        unit: "bytes",
        bound: Bound::Steady {
            limit: 64.0 * 1024.0 * 1024.0,
        },
        means: "the diagnostic's own history is not being compacted. The tool that reports \
                disk problems is causing one.",
    },
    Watched {
        name: "audit_records",
        unit: "count",
        bound: Bound::PerUnitOfWork {
            per: "queries",
            tolerance: 0.05,
        },
        means: "the audit is recording a different number of entries per query than it was. \
                Rising means something is recorded twice; falling means something is not \
                being recorded at all, and that is the worse direction.",
    },
    Watched {
        name: "warehouse_bytes",
        unit: "bytes",
        // A **budget**, not the size of the disk. A soak is entitled to a stated amount of
        // space and no more; a run that would need the whole volume has stopped being a
        // measurement of the system and become a measurement of the machine.
        //
        // Thirty-two gigabytes against a ten-gigabyte target leaves room for compaction
        // churn and the retention grace period. A projection crossing it inside the horizon
        // is the finding, reported *before* the space is gone rather than discovered after.
        bound: Bound::Steady {
            limit: 32.0 * 1024.0 * 1024.0 * 1024.0,
        },
        means: "the warehouse is consuming space faster than reclamation returns it. A run \
                that exhausts its disk stops being a measurement and becomes an incident, \
                and it takes its own evidence with it: the last one died writing its own \
                log, and its report is zero bytes.",
    },
    Watched {
        name: "live_files",
        unit: "count",
        bound: Bound::Sawtooth { limit: 1_000.0 },
        means: "compaction is not keeping up with the write rate. The troughs are where it \
                gets back to; if the peaks climb, each cycle starts further behind than the \
                last and query latency follows.",
    },
];

/// The declaration for a measure, if it is watched.
#[must_use]
pub fn watched(name: &str) -> Option<&'static Watched> {
    WATCHED.iter().find(|measure| measure.name == name)
}

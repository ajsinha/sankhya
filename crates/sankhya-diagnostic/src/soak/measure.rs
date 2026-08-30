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

/// The scale a run declares, and the single place everything derived from it is computed.
///
/// # Why this is here and not in the harness
///
/// The scale was doubled on 2026-08-29 by owner decision --- `SANKHYA_SOAK_GB` from ten to
/// twenty --- and the change landed in exactly one place: the harness's default. Three
/// thresholds had been sized against the old figure and stayed there, two of them in this
/// file. The next run at the new scale aborted inside ten minutes against a budget written
/// for half the data, and the report called it a reclamation failure, which it was not.
///
/// So the target lives here, the harness reads it from here, and the limits that follow from
/// it are arithmetic rather than restatement. A scale change now moves everything it should
/// move, which is the property that was missing rather than the numbers that were wrong.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Scale {
    /// Total data generated across all tables.
    pub gb: f64,
    /// How many tables it is spread across.
    pub tables: usize,
}

impl Scale {
    /// What this run declared, from the environment, with the defaults the harness documents.
    #[must_use]
    pub fn declared() -> Self {
        Self {
            gb: std::env::var("SANKHYA_SOAK_GB")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(Self::DEFAULT_GB),
            tables: std::env::var("SANKHYA_SOAK_TABLES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(Self::DEFAULT_TABLES),
        }
    }

    /// Twenty gigabytes, doubled from ten on 2026-08-29.
    ///
    /// The figure means something different than it did when ten was chosen: the soak did not
    /// read its data then, so it was a number about how much got written. Now that a run reads
    /// billions of rows the dataset is a working set, and the property that matters is that it
    /// does not fit in page cache.
    pub const DEFAULT_GB: f64 = 20.0;

    /// Ten tables.
    pub const DEFAULT_TABLES: usize = 10;

    /// Gigabytes in one table.
    #[must_use]
    pub fn per_table_gb(self) -> f64 {
        if self.tables == 0 {
            return self.gb;
        }
        #[allow(clippy::cast_precision_loss)]
        let tables = self.tables as f64;
        self.gb / tables
    }
}

/// Everything a soak watches, at the scale this run declared.
///
/// A `LazyLock` rather than a `static` slice, because two of the limits are arithmetic on the
/// declared scale and a `const` cannot read the environment. Evaluated once, on first use,
/// after the environment is set and before any measurement is judged against it.
pub static WATCHED: std::sync::LazyLock<Vec<Watched>> =
    std::sync::LazyLock::new(|| watched_at(Scale::declared()));

/// Everything a soak watches, at a given scale.
#[must_use]
pub fn watched_at(scale: Scale) -> Vec<Watched> {
    vec![
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
            // Three point two times the target, which is where the original thirty-two
            // gigabytes sat against a ten-gigabyte one. The multiple is what was reasoned
            // about --- room for compaction churn and the retention grace period on top of the
            // data itself --- and the absolute figure was only ever that multiple evaluated
            // once. Writing it as arithmetic is what keeps it true when the target moves.
            //
            // The transient is the part that needs the room. A settled run holds well under
            // the target: the ten-gigabyte run of 2026-08-28 sat steady at nine. What needs
            // headroom is the opening, before compaction has cycled and the grace period has
            // let go of anything, and that is proportional to the data.
            //
            // A projection crossing it inside the horizon is the finding, reported *before*
            // the space is gone rather than discovered after.
            bound: Bound::Steady {
                limit: 3.2 * scale.gb * 1024.0 * 1024.0 * 1024.0,
            },
            means: "the warehouse is consuming space faster than reclamation returns it. A run \
                    that exhausts its disk stops being a measurement and becomes an incident, \
                    and it takes its own evidence with it: the last one died writing its own \
                    log, and its report is zero bytes.",
        },
        Watched {
            name: "live_files",
            unit: "count",
            // A thousand files per gigabyte in the largest table, which is where the flat
            // thousand sat when a run put one gigabyte in each of ten tables. Measured against
            // it: the worst table in a two-gigabyte-per-table run held 1,080 live files, so
            // the same margin survives the scale change and a flat thousand would not have.
            //
            // Per table rather than per warehouse. A sum hides one table falling behind inside
            // nine tables' noise, and one table falling behind is the failure this watches for.
            bound: Bound::Sawtooth {
                limit: 1_000.0 * scale.per_table_gb(),
            },
            means: "compaction is not keeping up with the write rate. The troughs are where it \
                    gets back to; if the peaks climb, each cycle starts further behind than the \
                    last and query latency follows.",
            },
    ]
}

/// The declaration for a measure, if it is watched.
#[must_use]
pub fn watched(name: &str) -> Option<&'static Watched> {
    WATCHED.iter().find(|measure| measure.name == name)
}

//! When a run of records that do not fit means something different from one of them.
//!
//! # Why a rate, and why over a window
//!
//! A total accumulates over the life of a feed. It trips eventually for reasons that are
//! historical --- a bad afternoon last March --- and by then the number says nothing about
//! what is happening now. A rate over recent records says exactly that.
//!
//! # The two moments it is measured, and why one is not enough
//!
//! **While reading**, once enough records have been seen to have a rate at all. Waiting for a
//! full window is deliberate: the alternative makes the first record of a feed decide its
//! fate, and a feed that stops because record one was malformed is the opposite of *"one bad
//! record is an incident"*.
//!
//! **At the end of each source, when nothing in it fitted at all.** A file that produced not
//! one usable record is not a rate --- it needs no threshold and admits no argument --- and
//! without this check a feed whose window is a hundred would swallow a ninety-nine-record file
//! whole, move on to the next one, and leave every dashboard green while nothing arrived.
//!
//! Deliberately *not* a threshold over a source. A file of two records with one bad in it
//! would trip any fraction worth setting, and stopping a feed for that is the same mistake as
//! stopping it for its first malformed document. Anything between "one bad record" and "this
//! source is not usable" is a rate, and the window is what measures rates --- it spans
//! sources, so a run of half-bad files accumulates into it and stops the feed there.

use crate::declare::Quarantine;
use std::collections::VecDeque;

/// Whether the feed carries on.
#[derive(Clone, PartialEq, Debug)]
pub enum Verdict {
    /// Carry on.
    Continue,
    /// Stop, and wait for a person.
    ///
    /// Not a retry, not a back-off. A source whose shape has changed produces all-bad records
    /// for as long as it is running, and a feed that retries on a timer rediscovers the same
    /// outage every few minutes and is acted on by nobody.
    Stop(Reason),
}

/// Why a feed stopped.
#[derive(Clone, PartialEq, Debug)]
pub struct Reason {
    /// How many of the records considered did not fit.
    pub quarantined: u32,
    /// How many were considered.
    pub considered: u32,
    /// The threshold that was crossed.
    pub threshold: f64,
    /// Whether this was the trailing window or a whole source.
    pub over: Span,
}

/// What the rate was measured over.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Span {
    /// The most recent records, however many sources they came from.
    Window,
    /// One source, whole.
    Source,
}

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tail = "Stopped rather than continued: a source whose shape has changed \
                    quarantines everything, and a feed that keeps going leaves every dashboard \
                    green while nothing arrives";
        match self.over {
            Span::Window => write!(
                f,
                "{} of the last {} records did not fit, which is above the {:.0}% this feed \
                 stops at. {tail}",
                self.quarantined,
                self.considered,
                self.threshold * 100.0
            ),
            Span::Source => write!(
                f,
                "not one of this source's {} records fitted. {tail}",
                self.considered
            ),
        }
    }
}

/// The running count of what fitted and what did not.
#[derive(Clone, Debug)]
pub struct Outcomes {
    window: u32,
    threshold: f64,
    recent: VecDeque<bool>,
    unfit_in_window: u32,
    source_read: u32,
    source_unfit: u32,
    read: u64,
    unfit: u64,
}

impl Outcomes {
    /// Start counting, under a feed's quarantine policy.
    #[must_use]
    pub fn under(quarantine: Quarantine) -> Self {
        Self {
            window: quarantine.window,
            threshold: quarantine.stop_above,
            recent: VecDeque::with_capacity(quarantine.window as usize),
            unfit_in_window: 0,
            source_read: 0,
            source_unfit: 0,
            read: 0,
            unfit: 0,
        }
    }

    /// Record one record, and say whether the feed carries on.
    pub fn record(&mut self, fitted: bool) -> Verdict {
        self.read += 1;
        self.source_read += 1;
        if !fitted {
            self.unfit += 1;
            self.source_unfit += 1;
        }

        self.recent.push_back(fitted);
        if !fitted {
            self.unfit_in_window += 1;
        }
        while self.recent.len() > self.window as usize {
            if self.recent.pop_front() == Some(false) {
                self.unfit_in_window = self.unfit_in_window.saturating_sub(1);
            }
        }

        // Only once the window is full. Before that there is no rate --- there is one record,
        // or four, and a fraction of four is a number that means nothing about a feed.
        if self.recent.len() < self.window as usize {
            return Verdict::Continue;
        }
        self.verdict(self.unfit_in_window, self.window, Span::Window)
    }

    /// Finish a source, and say whether the feed carries on to the next one.
    ///
    /// Stops only when the source produced **nothing** that fitted. See the module
    /// documentation for why this is not a threshold: everything between one bad record and
    /// a wholly unusable source is a rate, and the window measures rates.
    ///
    /// Resets the per-source count either way --- a feed that continues has forgiven this
    /// source, and one that stops is not going to ask again.
    pub fn finish_source(&mut self) -> Verdict {
        let (read, unfit) = (self.source_read, self.source_unfit);
        self.source_read = 0;
        self.source_unfit = 0;
        if read > 0 && unfit == read {
            return Verdict::Stop(Reason {
                quarantined: unfit,
                considered: read,
                threshold: self.threshold,
                over: Span::Source,
            });
        }
        Verdict::Continue
    }

    /// How many records have been read, and how many did not fit.
    #[must_use]
    pub const fn totals(&self) -> (u64, u64) {
        (self.read, self.unfit)
    }

    /// The rate, against the threshold.
    fn verdict(&self, unfit: u32, considered: u32, over: Span) -> Verdict {
        if considered == 0 {
            return Verdict::Continue;
        }
        let rate = f64::from(unfit) / f64::from(considered);
        if rate > self.threshold {
            Verdict::Stop(Reason {
                quarantined: unfit,
                considered,
                threshold: self.threshold,
                over,
            })
        } else {
            Verdict::Continue
        }
    }
}

//! Proving the harness notices.
//!
//! A soak that would have been green anyway is an untested backup by another name: green
//! because nothing was capable of turning it red. So every shape of failure the harness
//! claims to detect is injected here and required to fail the run.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp,
    clippy::cast_precision_loss
)]

use sankhya_diagnostic::soak::judge::{peaks_of, ratio, Verdict};
use sankhya_diagnostic::soak::measure::{watched, Bound};
use sankhya_diagnostic::soak::sample::Samples;
use sankhya_diagnostic::soak::Report;

const MINUTE: i64 = 60 * 1_000_000;

/// How far ahead a run of `minutes` is entitled to speak.
///
/// Derived, not fixed. A three-week constant was the first version and every one of these
/// fixtures was then refused — correctly: four hours of samples asking about three weeks is
/// an extrapolation by a factor of a hundred and twenty-six, and the harness now says so.
///
/// The consequence for these tests is the honest one: **a four-hour run cannot establish
/// that a five-day leak exists.** So the injected leaks are steep enough to cross inside what
/// the run supports. That tests the harness's detection, which is the claim, rather than its
/// willingness to extrapolate, which is the thing it refuses to do.
fn horizon(minutes: i64) -> i64 {
    // From the span that is actually *judged*, which is the run minus the warm-up prefix.
    // Using the full run overshot by exactly the warm-up and every fixture came back
    // inconclusive — the harness refusing an extrapolation the test had asked for by
    // accident, which is the harness working.
    let judged_minutes =
        minutes - i64::try_from(sankhya_diagnostic::soak::report::WARM_UP_SAMPLES).unwrap_or(0) - 1;
    judged_minutes * 60 * sankhya_diagnostic::soak::judge::EXTRAPOLATION_FACTOR
}

/// Samples of one measure, one a minute, from a function of the minute number.
fn series(samples: &mut Samples, measure: &str, minutes: i64, value: impl Fn(i64) -> f64) {
    for minute in 0..minutes {
        samples.record(measure, minute * MINUTE, Some(value(minute)));
    }
}

/// A run where nothing moves.
fn steady_run(minutes: i64) -> Samples {
    let mut samples = Samples::new();
    series(&mut samples, "resident_bytes", minutes, |m| {
        // Real memory is noisy. A harness that only passes a perfectly flat line fails every
        // real run, gets its threshold raised, and then passes everything.
        400.0 * 1024.0 * 1024.0 + ((m % 7) as f64) * 1024.0 * 1024.0
    });
    series(&mut samples, "open_files", minutes, |m| {
        40.0 + f64::from(i32::try_from(m % 5).unwrap_or(0))
    });
    series(&mut samples, "metric_series", minutes, |_| 120.0);
    series(&mut samples, "history_bytes", minutes, |m| {
        // Grows, and is compacted back. A sawtooth in a Steady measure would be a finding;
        // this one stays under a ceiling.
        20_000.0 + ((m % 30) as f64) * 100.0
    });
    series(&mut samples, "queries", minutes, |m| (m as f64) * 500.0);
    series(&mut samples, "audit_records", minutes, |m| (m as f64) * 500.0);
    series(&mut samples, "warehouse_bytes", minutes, |m| {
        // Grows as data arrives and is reclaimed by retention, holding well under the stated
        // budget. A warehouse that only ever grows is the finding this measure exists for —
        // the run before it was added exhausted its disk and died writing its own log.
        8.0 * 1024.0 * 1024.0 * 1024.0 + ((m % 20) as f64) * 64.0 * 1024.0 * 1024.0
    });
    series(&mut samples, "live_files", minutes, |m| {
        // Writes add, compaction reclaims, and the peaks stay level.
        60.0 + ((m % 20) as f64) * 8.0
    });
    samples
}

fn judged(samples: &Samples, minutes: i64) -> Report {
    Report::of(samples, horizon(minutes), (minutes - 1) * MINUTE)
}

// --- the baseline the injections are measured against --------------------

#[test]
fn a_run_where_nothing_grows_passes() {
    // If this ever fails, every test below proves nothing: they would all be failing for the
    // reason this one does rather than for the leak they inject.
    let minutes = 240;
    let report = judged(&steady_run(minutes), minutes);
    assert!(
        report.passed(),
        "the steady baseline must pass, and did not:\n{}",
        report.describe()
    );
}

// --- one injection per shape of failure ---------------------------------

#[test]
fn a_memory_leak_is_caught_with_a_date() {
    // Four hours of run, a megabyte a minute. Nothing is visibly wrong at any moment — the
    // process is at 640 MB and its limit is 8 GB — and it reaches the limit in five days.
    let minutes = 240;
    let mut samples = steady_run(minutes);
    let mut leaking = Samples::new();
    for measure in samples.measured().map(str::to_string).collect::<Vec<_>>() {
        if measure != "resident_bytes" {
            for observation in samples.of(&measure) {
                leaking.record(&measure, observation.at, Some(observation.value));
            }
        }
    }
    // Twenty megabytes a minute, so the crossing lands inside what a four-hour run
    // supports. A slower leak is just as real and needs a longer run to establish — which is
    // the whole reason the scheduled soak runs for days.
    series(&mut leaking, "resident_bytes", minutes, |m| {
        400.0 * 1024.0 * 1024.0 + (m as f64) * 20.0 * 1024.0 * 1024.0
    });

    let report = judged(&leaking, minutes);
    assert!(!report.passed(), "a megabyte a minute is a leak");
    let failures = report.failures();
    assert_eq!(failures.len(), 1, "only the injected measure fails");
    let (measure, verdict) = failures[0];
    assert_eq!(measure.name, "resident_bytes");
    let Verdict::Growing { seconds, means, .. } = verdict else {
        panic!("a steady climb has a date: {verdict:?}");
    };
    assert!(*seconds > 0 && *seconds < horizon(minutes));
    assert!(
        means.contains("killed by the kernel"),
        "the report says what the slope implies, not just that there is one: {means}"
    );
    let _ = samples;
}

#[test]
fn descriptors_that_are_not_returned_are_caught() {
    let minutes = 240;
    let mut samples = Samples::new();
    // Four descriptors a minute: 1000 by the end of the run, against a limit of 1024.
    series(&mut samples, "open_files", minutes, |m| 40.0 + (m as f64) * 4.0);
    let report = judged(&samples, minutes);
    let (measure, verdict) = report
        .failures()
        .into_iter()
        .find(|(measure, _)| measure.name == "open_files")
        .expect("a descriptor leak fails");
    assert_eq!(measure.name, "open_files");
    assert!(matches!(verdict, Verdict::Growing { .. }), "{verdict:?}");
}

#[test]
fn a_sawtooth_whose_peaks_climb_is_caught_and_a_level_one_is_not() {
    // The distinction a point-in-time diagnostic cannot draw. Both series oscillate; both
    // look identical at any single moment. One is a system keeping up and one is a system
    // falling behind, and the difference is only in the peaks.
    let minutes = 480;

    let mut level = Samples::new();
    series(&mut level, "live_files", minutes, |m| {
        60.0 + ((m % 40) as f64) * 10.0
    });
    let level_report = judged(&level, minutes);
    let level_verdict = &level_report
        .verdicts
        .iter()
        .find(|(measure, _)| measure.name == "live_files")
        .expect("judged")
        .1;
    assert_eq!(
        level_verdict,
        &Verdict::Steady,
        "a sawtooth returning to the same floor is a system keeping up"
    );

    let mut climbing = Samples::new();
    series(&mut climbing, "live_files", minutes, |m| {
        // Same oscillation, and each cycle starts higher than the last — steeply enough to
        // reach the thousand-file limit inside what a run of this length supports.
        60.0 + ((m % 40) as f64) * 10.0 + (m as f64) * 1.5
    });
    let climbing_report = judged(&climbing, minutes);
    let climbing_verdict = &climbing_report
        .verdicts
        .iter()
        .find(|(measure, _)| measure.name == "live_files")
        .expect("judged")
        .1;
    assert!(
        !climbing_verdict.passed(),
        "climbing peaks mean each cycle starts further behind: {climbing_verdict:?}"
    );
}

#[test]
fn an_audit_recording_twice_is_caught_although_its_total_is_supposed_to_rise() {
    // The failure a total can never show. Audit records are meant to grow; what must not
    // grow is records *per query*. Watching the total alone passes this every time.
    let minutes = 240;
    let mut samples = Samples::new();
    series(&mut samples, "queries", minutes, |m| (m as f64) * 500.0);
    series(&mut samples, "audit_records", minutes, |m| {
        // Drifting from one per query to two, slowly.
        (m as f64) * 500.0 * (1.0 + (m as f64) / 480.0)
    });

    let report = judged(&samples, minutes);
    let (_, verdict) = report
        .failures()
        .into_iter()
        .find(|(measure, _)| measure.name == "audit_records")
        .expect("a drifting ratio fails");
    assert!(!verdict.passed(), "{verdict:?}");

    // And the total on its own looks perfectly healthy.
    let total = samples.of("audit_records").last().expect("samples").value;
    assert!(total > 0.0, "the total rose, as it is supposed to");
}

// --- inconclusive is a failure ------------------------------------------

#[test]
fn a_run_that_sampled_nothing_does_not_pass() {
    // The most important negative result here. A harness that reports the same green whether
    // it measured everything or nothing is worse than no harness, because the green is what
    // gets read.
    let report = Report::of(&Samples::new(), 3_600, 0);
    assert!(!report.passed());
    assert!(
        report
            .failures()
            .iter()
            .all(|(_, verdict)| matches!(verdict, Verdict::Inconclusive { .. })),
        "unmeasured is inconclusive, not steady"
    );
    assert!(report.describe().contains("COULD NOT JUDGE"));
}

#[test]
fn a_measure_that_could_not_be_read_is_counted_rather_than_recorded_as_zero() {
    // Zero is a perfectly steady measure. A harness that records a failed reading as zero
    // passes every run while measuring nothing.
    let mut samples = Samples::new();
    for minute in 0..10 {
        samples.record("resident_bytes", minute * MINUTE, None);
    }
    assert_eq!(samples.of("resident_bytes").len(), 0);
    assert_eq!(samples.missed("resident_bytes"), 10);
}

#[test]
fn too_few_samples_is_inconclusive_rather_than_steady() {
    let mut samples = Samples::new();
    samples.record("resident_bytes", 0, Some(400.0));
    let report = Report::of(&samples, 3_600, 0);
    let (_, verdict) = report
        .verdicts
        .iter()
        .find(|(measure, _)| measure.name == "resident_bytes")
        .expect("judged");
    assert!(matches!(verdict, Verdict::Inconclusive { .. }), "{verdict:?}");
}

// --- the pieces ---------------------------------------------------------

#[test]
fn peaks_are_the_maximum_of_each_window() {
    let samples: Vec<_> = (0..40)
        .map(|m| {
            sankhya_diagnostic::projection::Observation::new(
                m * MINUTE,
                if m % 10 == 5 { 100.0 } else { 10.0 },
            )
        })
        .collect();
    let peaks = peaks_of(&samples, 4);
    assert_eq!(peaks.len(), 4);
    assert!(peaks.iter().all(|peak| peak.value == 100.0), "{peaks:?}");
}

#[test]
fn an_empty_window_is_skipped_rather_than_counted_as_zero() {
    // A zero peak drags the trend downward and makes a genuinely climbing sawtooth look
    // steady, which is the one conclusion this must never reach by accident.
    let samples = vec![
        sankhya_diagnostic::projection::Observation::new(0, 10.0),
        sankhya_diagnostic::projection::Observation::new(100 * MINUTE, 20.0),
    ];
    let peaks = peaks_of(&samples, 10);
    assert!(peaks.iter().all(|peak| peak.value > 0.0), "{peaks:?}");
    assert!(peaks.len() < 10, "the empty windows contributed nothing");
}

#[test]
fn a_ratio_is_formed_only_at_instants_both_measures_share() {
    // Interpolating between reference samples invents readings, and the ratio then becomes
    // partly a property of the interpolation — so a drift in it cannot be told from a drift
    // in the data.
    let numerator = vec![
        sankhya_diagnostic::projection::Observation::new(0, 10.0),
        sankhya_diagnostic::projection::Observation::new(MINUTE, 20.0),
    ];
    let denominator = vec![sankhya_diagnostic::projection::Observation::new(0, 5.0)];
    let ratios = ratio(&numerator, &denominator).expect("one instant is shared");
    assert_eq!(ratios.len(), 1);
    assert_eq!(ratios[0].value, 2.0);

    assert_eq!(ratio(&numerator, &[]), None, "no shared instants, no ratio");
}

#[test]
fn every_watched_measure_says_what_a_breach_means() {
    // A soak report naming a measure and a slope is a puzzle. One saying what the slope
    // implies is a finding.
    for measure in sankhya_diagnostic::soak::WATCHED.iter() {
        assert!(
            measure.means.len() > 60,
            "{} does not say what a breach means",
            measure.name
        );
        assert!(watched(measure.name).is_some());
    }
}

// --- the refusals, each of which survived until it had a test ------------

#[test]
fn a_short_run_refuses_a_long_horizon_rather_than_extrapolating_into_it() {
    // The guard added after the first real run: samples spanning seconds, judged against
    // three weeks, reported a memory leak that was a process warming up. The arithmetic was
    // sound over a factor of three and a half million and was not evidence of anything.
    let mut samples = Samples::new();
    for round in 0..40_i64 {
        samples.record("resident_bytes", round * 100_000, Some(400.0 + round as f64));
    }

    let three_weeks = 21 * 24 * 3_600;
    let report = Report::of(&samples, three_weeks, 40 * 100_000);
    let (_, verdict) = report
        .verdicts
        .iter()
        .find(|(measure, _)| measure.name == "resident_bytes")
        .expect("judged");
    let Verdict::Inconclusive { why } = verdict else {
        panic!("four seconds cannot speak about three weeks: {verdict:?}");
    };
    assert!(why.contains("arithmetic rather than evidence"), "{why}");
    assert!(
        why.contains("Run for longer; do not widen the limit"),
        "widening the limit is the tempting wrong answer, so the message says so: {why}"
    );
    assert!(!report.passed(), "and it is a failure, not a pass");
}

#[test]
fn a_long_run_with_too_few_samples_is_still_inconclusive() {
    // The sample-count guard, tested where the *span* guard cannot mask it. The obvious
    // fixture — one sample — has a span of zero, so it is refused for being too short before
    // the count is ever consulted, and the count guard survived every mutation because of it.
    //
    // So: a generous span, sampled far too sparsely to say anything.
    let mut samples = Samples::new();
    for i in 0..(sankhya_diagnostic::soak::report::WARM_UP_SAMPLES as i64 + 4) {
        samples.record("resident_bytes", i * 20 * MINUTE, Some(400.0 + i as f64));
    }

    // A horizon the settled span *can* support, so the span guard does not fire first and
    // mask the thing being tested. Four samples twenty minutes apart span an hour, which
    // entitles the run to three — and it is still four samples.
    let report = Report::of(&samples, 3 * 3_600, 400 * MINUTE);
    let (_, verdict) = report
        .verdicts
        .iter()
        .find(|(measure, _)| measure.name == "resident_bytes")
        .expect("judged");
    let Verdict::Inconclusive { why } = verdict else {
        panic!("four settled samples is not a judgement: {verdict:?}");
    };
    assert!(
        why.contains("fewest worth judging"),
        "a rate from a handful of readings is a rate through a handful of noise: {why}"
    );
}

#[test]
fn the_warm_up_prefix_is_actually_discarded() {
    // A run that allocates hard while starting and is flat afterwards. Judged whole, the
    // opening ramp is a slope and the run fails; judged from the settled portion it is
    // steady, which is what it is.
    //
    // The exclusion is a fixed, declared count on purpose. Discarding *until the series looks
    // flat* would hide every leak by construction, because a leak is precisely a series that
    // does not go flat.
    let warm_up = sankhya_diagnostic::soak::report::WARM_UP_SAMPLES as i64;
    let mut samples = Samples::new();
    for i in 0..120_i64 {
        let value = if i < warm_up {
            // The ramp: a hundred megabytes a sample while caches fill.
            100.0 * 1024.0 * 1024.0 * (i as f64 + 1.0)
        } else {
            // Settled, and noisy the way a real measure is.
            1024.0 * 1024.0 * 1024.0 + ((i % 5) as f64) * 1024.0 * 1024.0
        };
        samples.record("resident_bytes", i * MINUTE, Some(value));
    }

    let report = Report::of(&samples, horizon(120), 119 * MINUTE);
    let (_, verdict) = report
        .verdicts
        .iter()
        .find(|(measure, _)| measure.name == "resident_bytes")
        .expect("judged");
    assert_eq!(
        verdict,
        &Verdict::Steady,
        "the opening ramp is warm-up, not drift, and judging it whole says otherwise"
    );
}

#[test]
fn a_sawtooth_is_judged_from_its_peaks_even_when_it_ends_in_a_trough() {
    // Why peaks rather than raw samples, in the only case that distinguishes them.
    //
    // The distance left to the limit is measured from the *latest* reading. A climbing
    // sawtooth that happens to end at a trough looks to have far more headroom than it has —
    // so the raw series projects a crossing well past the horizon, which reads as steady,
    // while the peaks (which end at a peak by construction) reach it comfortably inside.
    //
    // The first version of this test failed both ways and proved nothing: the amplitude has
    // to be large relative to the remaining headroom, or a trough and a peak give the same
    // answer.
    //
    // So the fixture is stated as fractions of the declared limit rather than as figures that
    // happen to sit under it. The limit is arithmetic on the run's scale --- it was a flat
    // thousand until the scale doubled and stranded it --- and a fixture carrying its own copy
    // would pass at one scale and prove nothing at another. This one asks what the limit is.
    let limit = match watched("live_files").map(|m| m.bound) {
        Some(Bound::Sawtooth { limit }) => limit,
        other => panic!("live_files is declared as a sawtooth with a limit, not {other:?}"),
    };
    let minutes = 481_i64;
    // Half the limit of swing per cycle, on a floor of three tenths of it, climbing a fifth of
    // a percent of the limit each minute: the peaks reach the limit inside the horizon and the
    // troughs do not come close, which is the only shape that distinguishes the two readings.
    let amplitude = limit * 0.5;
    let per_minute = limit * 0.0002;
    let floor = limit * 0.3;
    let mut samples = Samples::new();
    for m in 0..minutes {
        let cycle = m % 40;
        // Ramps from the floor to the floor plus the amplitude across each cycle, so cycle 0
        // is a trough and cycle 39 a peak.
        let value = floor + (m as f64) * per_minute + (cycle as f64 / 39.0) * amplitude;
        samples.record("live_files", m * MINUTE, Some(value));
    }
    assert_eq!(
        (minutes - 1) % 40,
        0,
        "the fixture must end on a trough for this test to mean anything"
    );

    let report = judged(&samples, minutes);
    let (_, verdict) = report
        .verdicts
        .iter()
        .find(|(measure, _)| measure.name == "live_files")
        .expect("judged");
    let Verdict::Growing { seconds, .. } = verdict else {
        panic!(
            "the peaks are approaching the limit; ending on a trough does not change that: \
             {verdict:?}"
        );
    };
    // And the distinguishing fact: from the trough the same series would look far further
    // from its limit than it is.
    let trough = samples
        .of("live_files")
        .last()
        .expect("samples")
        .value;
    #[allow(clippy::cast_possible_truncation)]
    let from_the_trough = ((limit - trough) / per_minute * 60.0) as i64;
    assert!(
        from_the_trough > horizon(minutes),
        "the fixture does not distinguish: from the trough it is {from_the_trough}s away and \
         the horizon is {}s",
        horizon(minutes)
    );
    assert!(
        *seconds < horizon(minutes),
        "and from the peaks it is inside the horizon"
    );
}

#[test]
fn the_baseline_supplies_every_watched_measure() {
    // Or the baseline is not a healthy *run*, it is a healthy subset — and every injection
    // test below inherits the gap. Adding `warehouse_bytes` to the watched set broke two
    // tests precisely this way: they failed because a measure had no samples, not because
    // of anything they injected.
    //
    // Asserted against the declaration rather than a list here, so the next measure someone
    // adds fails this test rather than the ones that matter.
    let samples = steady_run(30);
    let measured: std::collections::BTreeSet<&str> = samples.measured().collect();
    let mut missing = Vec::new();
    for declared in sankhya_diagnostic::soak::measure::WATCHED.iter() {
        if !measured.contains(declared.name) {
            missing.push(declared.name);
        }
    }
    assert!(
        missing.is_empty(),
        "the baseline does not supply {missing:?}, so every test that uses it is judging a \
         run with a measure that was never taken"
    );
}

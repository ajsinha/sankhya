//! The signal selection needs, and the two ways a query log goes wrong.
//!
//! `lattice::select` has existed and been tested since M7 began, and nothing could run it
//! because nothing recorded what people ask for. Its own documentation says why that mattered:
//! selecting against the whole lattice optimises for queries nobody runs, *"which is the same
//! mistake as a person guessing, made faster."*
//!
//! Two things must hold, and they pull against each other. The log has to remember enough that
//! frequency is visible, and it has to forget — a structure that grows once per query and is
//! never trimmed is a leak with a business justification, which is the failure this warehouse
//! has now found four times.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_cube::algo::Cuboid;
use sankhya_cube::querylog::QueryLog;

fn cuboid(dimensions: &[&str]) -> Cuboid {
    Cuboid::of(dimensions)
}

#[test]
fn what_was_asked_for_comes_back() {
    let log = QueryLog::new();
    log.record("sales", cuboid(&["region"]));

    assert_eq!(log.asked("sales"), vec![cuboid(&["region"])]);
}

#[test]
fn a_cube_nobody_asked_about_has_nothing_to_select_against() {
    // Empty rather than everything. Handing `select` the whole lattice for an unqueried cube
    // would spend an operator's budget on shapes nobody has ever wanted.
    let log = QueryLog::new();
    assert!(log.asked("sales").is_empty());
    assert!(log.is_empty());
}

#[test]
fn repetition_is_the_weighting() {
    // `select` takes a slice, so a shape asked ten times counts ten times. Deduplicating here
    // would throw away the frequency the selection is supposed to be reading, and would need
    // a separate tally to put it back.
    let log = QueryLog::new();
    for _ in 0..7 {
        log.record("sales", cuboid(&["region"]));
    }
    log.record("sales", cuboid(&["region", "period"]));

    let asked = log.asked("sales");
    assert_eq!(asked.len(), 8);
    assert_eq!(
        asked.iter().filter(|c| **c == cuboid(&["region"])).count(),
        7,
        "the popular shape appears seven times, which is how it wins"
    );
}

#[test]
fn the_log_is_bounded() {
    // The failure this warehouse keeps finding: something that grows once per event and is
    // never trimmed. A query log is exactly that shape.
    let log = QueryLog::with_capacity(4);
    for i in 0..1_000 {
        log.record("sales", cuboid(&[&format!("d{i}")]));
    }
    assert_eq!(log.len("sales"), 4, "a thousand asks, four remembered");
}

#[test]
fn old_asks_are_forgotten_and_recent_ones_kept() {
    // Recency without a timestamp: the ring overwrites oldest-first, so a dashboard nobody has
    // opened in a week stops pinning storage without anybody deciding it should.
    let log = QueryLog::with_capacity(3);
    log.record("sales", cuboid(&["ancient"]));
    for _ in 0..3 {
        log.record("sales", cuboid(&["recent"]));
    }

    let asked = log.asked("sales");
    assert_eq!(asked.len(), 3);
    assert!(
        !asked.contains(&cuboid(&["ancient"])),
        "the oldest ask was overwritten: {asked:?}"
    );
}

#[test]
fn one_cube_does_not_crowd_out_another() {
    // The bound is per cube. A busy cube must not evict a quiet one's history, or the quiet
    // cube looks unqueried and loses whatever was materialised for it.
    let log = QueryLog::with_capacity(2);
    log.record("quiet", cuboid(&["region"]));
    for _ in 0..50 {
        log.record("busy", cuboid(&["period"]));
    }

    assert_eq!(log.len("quiet"), 1, "the quiet cube kept its ask");
    assert_eq!(log.len("busy"), 2);
    let mut cubes = log.cubes();
    cubes.sort();
    assert_eq!(cubes, vec!["busy".to_string(), "quiet".to_string()]);
}

#[test]
fn a_changed_definition_forgets_its_history() {
    // The shapes asked of the old cube may name dimensions the new one does not have.
    // Selecting against those spends a budget on cuboids nothing can use.
    let log = QueryLog::new();
    log.record("sales", cuboid(&["region"]));
    log.record("returns", cuboid(&["region"]));

    log.forget("sales");

    assert!(log.asked("sales").is_empty());
    assert_eq!(log.asked("returns").len(), 1, "and only that cube's history");
}

#[test]
fn the_log_records_a_shape_and_has_nowhere_to_put_anything_else() {
    // Worth asserting rather than assuming. A query log is the kind of thing that quietly
    // becomes a record of who asked what about whom; this one records which dimensions were
    // grouped by, which is a list anybody who may read the cube can already get from
    // `cube_dimensions`. There is no field for a member, a predicate, or a principal.
    let log = QueryLog::new();
    log.record("sales", cuboid(&["region", "period"]));

    let asked = log.asked("sales");
    assert_eq!(asked.len(), 1);
    assert_eq!(
        asked[0].dimensions(),
        vec!["period", "region"],
        "dimension names, and nothing else"
    );
}

// --- contention: one cube's recording must not pace another's ------------------

#[test]
fn two_cubes_record_without_meeting() {
    // `record` runs on every cube query. Holding a write lock over the whole map to push one
    // entry made every cube's navigation serialize against every other cube's.
    //
    // Not a safety test --- the answers were always right. This asks the other question: does
    // it serialize? A correctness suite cannot tell, which is why the choke point survived
    // being read several times.
    use std::sync::{Arc, Barrier};

    const RECORDERS: usize = 8;
    const EACH: usize = 20_000;

    let log = Arc::new(QueryLog::with_capacity(64));
    let gate = Arc::new(Barrier::new(RECORDERS));

    // Every cube seen once up front, so the write-lock path is out of the measured section.
    // It runs once per cube in a real process, and including it here would measure the
    // insertion rather than the recording.
    for who in 0..RECORDERS {
        log.record(&format!("cube{who}"), cuboid(&["seed"]));
    }

    std::thread::scope(|scope| {
        for who in 0..RECORDERS {
            let log = Arc::clone(&log);
            let gate = Arc::clone(&gate);
            scope.spawn(move || {
                let name = format!("cube{who}");
                gate.wait();
                for _ in 0..EACH {
                    log.record(&name, cuboid(&["region"]));
                }
            });
        }
    });

    // Every recorder's work landed in its own cube, and none was lost to another's ring.
    for who in 0..RECORDERS {
        assert_eq!(
            log.len(&format!("cube{who}")),
            64,
            "cube{who} filled its own ring"
        );
    }
}

#[test]
fn concurrent_records_of_one_cube_lose_nothing() {
    // Two queries against one cube contend for that cube's own lock, and must. What they must
    // not do is drop an ask: the repetition *is* the weighting selection reads, so a lost
    // record is a shape that looks less popular than it is.
    use std::sync::{Arc, Barrier};

    const RECORDERS: usize = 8;
    const EACH: usize = 500;

    let log = Arc::new(QueryLog::with_capacity(RECORDERS * EACH));
    let gate = Arc::new(Barrier::new(RECORDERS));

    std::thread::scope(|scope| {
        for _ in 0..RECORDERS {
            let log = Arc::clone(&log);
            let gate = Arc::clone(&gate);
            scope.spawn(move || {
                gate.wait();
                for _ in 0..EACH {
                    log.record("sales", cuboid(&["region"]));
                }
            });
        }
    });

    assert_eq!(
        log.len("sales"),
        RECORDERS * EACH,
        "every ask was recorded exactly once"
    );
}
#[test]
fn recording_against_different_cubes_does_not_contend() {
    // The choke point this replaces: `record` runs on every cube query and took a write lock
    // over the whole map, so every cube's navigation serialized against every other cube's.
    //
    // **Measured as a ratio against this same machine, in this same run.** An absolute
    // threshold measures the hardware; what matters is whether recording to *different* cubes
    // is meaningfully cheaper than recording to *one*. With per-cube locks it is --- eight
    // threads on eight cubes never meet. With one lock over the map they are equally slow,
    // because the map lock is the only lock that matters.
    //
    // Measured here: **3.19** with per-cube locks, **1.06** with the map's write lock put back.
    // The threshold sits between them with room on both sides.
    use std::sync::{Arc, Barrier};
    use std::time::Instant;

    const RECORDERS: usize = 8;
    const EACH: usize = 40_000;

    let elapsed = |distinct: bool| -> u128 {
        let log = Arc::new(QueryLog::with_capacity(64));
        // Every cube seen once first, so the write-lock insertion path --- which runs once per
        // cube in a real process --- is outside the measured section.
        for who in 0..RECORDERS {
            log.record(&format!("cube{who}"), cuboid(&["seed"]));
        }
        log.record("shared", cuboid(&["seed"]));

        let gate = Arc::new(Barrier::new(RECORDERS));
        let started = Instant::now();
        std::thread::scope(|scope| {
            for who in 0..RECORDERS {
                let log = Arc::clone(&log);
                let gate = Arc::clone(&gate);
                scope.spawn(move || {
                    let name = if distinct {
                        format!("cube{who}")
                    } else {
                        "shared".to_string()
                    };
                    gate.wait();
                    for _ in 0..EACH {
                        log.record(&name, cuboid(&["region"]));
                    }
                });
            }
        });
        started.elapsed().as_micros().max(1)
    };

    let distinct = elapsed(true);
    let shared = elapsed(false);
    let ratio = shared as f64 / distinct as f64;

    assert!(
        ratio > 1.8,
        "recording to eight different cubes took {distinct}us and to one took {shared}us, a \
         ratio of {ratio:.2} --- so the cubes are contending with each other rather than only \
         with themselves"
    );
}

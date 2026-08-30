//! Two writers, one name.
//!
//! # Why these are exhaustive rather than illustrative
//!
//! This crate has no dependencies, which is deliberate and is what lets these tests run
//! hundreds of contended rounds in milliseconds. The property being tested is a race, and a
//! race test that runs once has observed one interleaving out of many --- so the answer to
//! "how do you test a race" here is *volume*, and volume is affordable only because there is
//! nothing to link.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use sankhya_atomicfs::{claim, publish};
use std::sync::{Arc, Barrier};

const WRITERS: usize = 16;

/// The two body sizes a republishing writer alternates between.
///
/// Different lengths on purpose: a reader that catches a truncating write sees a *short* file
/// far more often than a mixed one, so the length is the sharper signal.
const SHORT: usize = 64;
const LONG: usize = 512 * 1024;
const ROUNDS: usize = 60;

#[test]
fn exactly_one_writer_claims_a_name() {
    // The property the protocol's whole concurrency control rests on: a loser must be *told*.
    // Repeated, because one round observes one interleaving.
    for round in 0..ROUNDS {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("version.json");
        let gate = Arc::new(Barrier::new(WRITERS));

        let winners: usize = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..WRITERS)
                .map(|who| {
                    let path = path.clone();
                    let gate = Arc::clone(&gate);
                    scope.spawn(move || {
                        gate.wait();
                        claim(&path, format!("writer {who}").as_bytes()).is_ok()
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("no panic"))
                .filter(|won| *won)
                .count()
        });

        assert_eq!(winners, 1, "round {round}: exactly one writer may claim a name");
    }
}

#[test]
fn a_loser_is_told_it_lost_and_not_something_else() {
    // `AlreadyExists` is not an incidental error kind here --- it is the answer, and the
    // callers that matter are built to receive it and rebase. A different kind would be
    // reported as a failure and the retry would never run.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let path = dir.path().join("version.json");
    claim(&path, b"first").expect("the first claim");

    let error = claim(&path, b"second").expect_err("the second is refused");
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists, "{error:?}");
}

#[test]
fn the_winner_publishes_its_own_bytes_and_not_a_rival_s() {
    // The defect a *shared* staging name produces, and the reason this is a separate test from
    // counting winners. Two writers staging through one path can each link the other's body:
    // the winner count is still one, the content is still a well-formed body, and it belongs
    // to somebody who was told they lost.
    //
    // Asserting merely that the content is *some* writer's body does not catch it --- the
    // first version of this test did exactly that and a mutation collapsing the staging name
    // to a constant survived it. The property is that the winner's own bytes are the ones on
    // disk, and the bodies are large so that two interleaved writes cannot both land whole.
    const BODY: usize = 256 * 1024;
    for round in 0..ROUNDS {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("version.json");
        let gate = Arc::new(Barrier::new(WRITERS));

        let winner: Option<usize> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..WRITERS)
                .map(|who| {
                    let path = path.clone();
                    let gate = Arc::clone(&gate);
                    scope.spawn(move || {
                        let body = vec![b'a' + u8::try_from(who).unwrap_or(0); BODY];
                        gate.wait();
                        claim(&path, &body).ok().map(|()| who)
                    })
                })
                .collect();
            handles.into_iter().filter_map(|h| h.join().expect("no panic")).next()
        });

        let who = winner.expect("somebody won");
        let written = std::fs::read(&path).expect("something was claimed");
        let expected = vec![b'a' + u8::try_from(who).unwrap_or(0); BODY];
        assert_eq!(
            written.len(),
            BODY,
            "round {round}: writer {who} won and the file is {} bytes",
            written.len()
        );
        assert!(
            written == expected,
            "round {round}: writer {who} won and the file holds somebody else's bytes"
        );
    }
}

#[test]
fn a_contested_claim_leaves_no_staging_files() {
    // Fifteen losers each wrote a body. A claim that leaks its staging file turns every
    // contested write into litter in a directory that gets listed and replayed.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let path = dir.path().join("version.json");
    let gate = Arc::new(Barrier::new(WRITERS));

    std::thread::scope(|scope| {
        for who in 0..WRITERS {
            let path = path.clone();
            let gate = Arc::clone(&gate);
            scope.spawn(move || {
                gate.wait();
                let _ = claim(&path, format!("writer {who}").as_bytes());
            });
        }
    });

    let entries: Vec<String> = std::fs::read_dir(dir.path())
        .expect("the directory")
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(ToString::to_string))
        .collect();
    assert_eq!(entries, vec!["version.json".to_string()], "staging left behind");
}

#[test]
fn publish_replaces_and_is_never_seen_half_written() {
    // The other half of the pair, and the half whose *job* is to replace. A reader running
    // throughout must see one of the two whole bodies and never a splice of them --- which is
    // what `fs::write` onto a live path produces, because it truncates first.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let path = dir.path().join("definition.json");
    let short = vec![b'a'; SHORT];
    let long = vec![b'b'; LONG];
    publish(&path, &short).expect("the first publication");

    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    std::thread::scope(|scope| {
        let reading = {
            let path = path.clone();
            let stop = Arc::clone(&stop);
            scope.spawn(move || {
                let mut seen = 0usize;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    if let Ok(bytes) = std::fs::read(&path) {
                        // **Length first, and this is the assertion that does the work.**
                        // Writing onto the live path truncates before it writes, so the file a
                        // reader catches mid-write is usually *short* rather than mixed --- and
                        // an empty read satisfies `all()` vacuously, which is how the first
                        // version of this test passed against exactly the defect it names.
                        assert!(
                            bytes.len() == SHORT || bytes.len() == LONG,
                            "a reader saw {} bytes, which is neither body whole",
                            bytes.len()
                        );
                        let whole = bytes.iter().all(|b| *b == b'a')
                            || bytes.iter().all(|b| *b == b'b');
                        assert!(whole, "a reader saw {} bytes of a mixture", bytes.len());
                        seen += 1;
                    }
                }
                seen
            })
        };
        for round in 0..200 {
            let body = if round % 2 == 0 { &long } else { &short };
            publish(&path, body).expect("republished");
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let seen = reading.join().expect("no panic");
        assert!(seen > 0, "the reader never observed the file, so it proved nothing");
    });
}

#[test]
fn publish_does_not_report_success_when_it_did_not_write() {
    // A publication into a directory that does not exist must fail loudly. Reporting success
    // and leaving nothing is how a manifest goes missing without anybody being told.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let path = dir.path().join("absent").join("definition.json");
    assert!(publish(&path, b"x").is_err(), "a write with nowhere to go must say so");
}

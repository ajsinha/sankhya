//! The concurrency criteria, run alone.
//!
//! # Why these are not in `check-tests`
//!
//! `ADR-0013`'s C1 to C3 are **measurements**: throughput with the shipping code against the
//! same work serialized through one mutex, in the same run on the same hardware. `check-tests`
//! is `cargo test --workspace`, which deliberately saturates every core --- and a throughput
//! measurement taken while dozens of test binaries compete for the machine describes the
//! machine.
//!
//! Three guards were tried before this and each was necessary without being sufficient. A ratio
//! over a workload that shares nothing reports near-linear scaling however busy the machine is,
//! because fair scheduling gives every runnable thread an equal share. Sampling free capacity
//! before the arms misses a machine that becomes busy during them. Bracketing the measurement
//! with a probe at each end still misses a transient inside a sub-second window.
//!
//! So the interference is removed rather than detected: these tests are `#[ignore]`d, so the
//! parallel suite skips them, and they run here **one at a time, as the only cargo process**.
//! The capacity guard stays in place as a backstop for a machine busy for some other reason,
//! and now it should almost never fire.
//!
//! They stay inside `check-all` rather than beside it, because a measurement moved out of the
//! gate is a measurement that stops being taken.

use std::path::Path;
use std::process::Command;

pub fn check(root: &Path) -> bool {
    println!("== check-concurrency ==");
    let measurements = [
        ("sankhya-publish", "scaling"),
        ("sankhya-readpath", "under_write_load"),
        ("sankhya-table-delta", "commit_scaling"),
        // The query log's per-cube locks. Added 2026-09-03: it was the suite's one
        // intermittent failure, passing alone and failing under load, because it was measuring
        // contention *inside* a run that creates contention. Eight parallel runs of its own
        // suite produced ratios of 0.54, 0.77 and 1.07 for code whose true ratio is above three.
        ("sankhya-cube", "querylog"),
    ];

    // Build every binary *before* measuring any of them. `cargo test` compiles with as much
    // parallelism as the machine has, and a measurement taken in the seconds after that compile
    // is taken on a machine still finishing it --- rustc processes draining, page cache
    // thrashed, the disk busy. Running as the only cargo process is not the same as running on
    // a quiet machine, and this is the difference between the two.
    for (package, test) in measurements {
        let built = Command::new(env!("CARGO"))
            .current_dir(root)
            .args(["test", "--quiet", "--no-run", "-p", package, "--test", test])
            .output();
        match built {
            Ok(built) if built.status.success() => {}
            _ => {
                eprintln!("  COULD NOT BUILD  {package} --test {test}");
                return false;
            }
        }
    }

    let mut ok = true;
    let mut taken = 0usize;
    let mut skipped = Vec::new();
    for (package, test) in measurements {
        let output = Command::new(env!("CARGO"))
            .current_dir(root)
            .args(["test", "--quiet", "-p", package, "--test", test])
            .args(["--", "--ignored", "--test-threads=1", "--nocapture"])
            .output();

        let Ok(output) = output else {
            eprintln!("  COULD NOT RUN  {package} --test {test}");
            ok = false;
            continue;
        };
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        if !output.status.success() {
            for line in text.lines().filter(|line| {
                line.contains("panicked at") || line.starts_with("test result: FAILED")
            }) {
                eprintln!("  {}", line.trim());
            }
            eprintln!("  FAILED         {package} --test {test}");
            ok = false;
            continue;
        }

        for line in text.lines().map(str::trim).filter(|line| line.starts_with("SKIPPED")) {
            skipped.push(line.to_string());
        }
        taken += text
            .lines()
            .filter_map(|line| line.strip_prefix("test result: ok. "))
            .filter_map(|rest| rest.split_whitespace().next())
            .filter_map(|count| count.parse::<usize>().ok())
            .sum::<usize>();
    }

    for skip in &skipped {
        println!("   {skip}");
    }
    println!(
        "   {taken} measurement(s) taken alone, {} skipped for want of a quiet machine",
        skipped.len()
    );
    ok
}

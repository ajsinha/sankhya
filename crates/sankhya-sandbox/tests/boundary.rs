//! What the boundary actually stops, proved by trying it.
//!
//! # Why every one of these runs a real program
//!
//! Because a sandbox is the one thing that cannot be tested by asserting that a function was
//! called. `ADR-0023` Decision 1 refuses in-interpreter sandboxing on the grounds that it is a
//! *decoration* — something that makes a reviewer believe there is a boundary. A test that
//! checked the flags this crate passes would be the same decoration one level up: it would pass
//! for a mechanism that does not work on this kernel, which is exactly the case the feature has
//! to detect.
//!
//! So each test starts a process, asks it to do the forbidden thing, and reads what happened.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use sankhya_sandbox::{probe, Bounds, Outcome};
use std::path::Path;
use std::time::Duration;

/// The tree a shell needs to exist at all. Everything else is absent by construction.
fn a_shell_needs() -> Vec<&'static Path> {
    ["/bin", "/usr/bin", "/lib", "/lib64", "/usr/lib"]
        .into_iter()
        .map(Path::new)
        .filter(|path| path.exists())
        .collect()
}

/// `ENETUNREACH`, written out rather than imported: this crate's tests do not depend on `libc`,
/// and the number is fixed by Linux's ABI.
fn libc_enetunreach() -> i32 {
    101
}

fn said(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Answered(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        other => format!("{other:?}"),
    }
}

/// Run `sh -c <script>` behind the boundary, with the given extra readable paths.
fn shell(script: &str, extra: &[&Path], bounds: &Bounds) -> Outcome {
    let ready = probe().expect("this machine can host the boundary");
    let mut readable = a_shell_needs();
    readable.extend_from_slice(extra);
    ready
        .run(Path::new("/bin/sh"), &["-c", script], &readable, b"", bounds)
        .expect("the process starts")
}

#[test]
fn the_machine_can_host_the_boundary_or_says_which_mechanism_it_lacks() {
    // Not an assertion that it works. On a kernel with unprivileged user namespaces disabled
    // the honest answer is a refusal naming the mechanism, and that is a pass for this test ---
    // what would be a failure is a probe that returned success on a machine where the boundary
    // does not exist, because every other test here would then be testing nothing.
    match probe() {
        Ok(_) => {}
        Err(unavailable) => {
            let message = unavailable.to_string();
            assert!(
                message.contains("namespace") || message.contains("Linux"),
                "a refusal must name the mechanism an operator would have to change: {message}"
            );
        }
    }
}

#[test]
fn it_can_read_what_it_was_given() {
    // The control. Without this the four refusals below would all pass for a boundary that
    // simply stopped everything, including the work.
    let dir = tempfile::tempdir().expect("a directory");
    std::fs::write(dir.path().join("given"), b"forty-two").expect("a file to read");

    let outcome = shell(
        &format!("cat {}/given", dir.path().display()),
        &[dir.path()],
        &Bounds::modest(),
    );
    assert!(
        said(&outcome).contains("forty-two"),
        "a program behind the boundary must be able to read the tree it was given: {}",
        said(&outcome)
    );
}

#[test]
fn what_it_was_not_given_does_not_exist() {
    // Not "is denied" --- does not exist. That distinction is the whole reason the mechanism is
    // a mount namespace rather than a permission check: a denial tells you the file is there.
    let dir = tempfile::tempdir().expect("a directory");
    std::fs::write(dir.path().join("secret"), b"the warehouse").expect("a file to hide");

    // Deliberately *not* passed as readable.
    let outcome = shell(
        &format!("cat {}/secret 2>&1 || echo ABSENT", dir.path().display()),
        &[],
        &Bounds::modest(),
    );
    let text = said(&outcome);
    assert!(
        !text.contains("the warehouse"),
        "a file outside the given tree must be unreachable: {text}"
    );
    assert!(
        text.contains("ABSENT"),
        "and unreachable because it is not there: {text}"
    );
}

#[test]
fn it_cannot_reach_the_network() {
    // The prohibition that matters most, and for a reason worth restating: the function is
    // handed rows a policy already filtered *for a principal*, so a socket turns "may read"
    // into "may publish".
    //
    // Asked of Python because a shell cannot open a socket portably, and skipped rather than
    // faked where Python is absent --- a test that silently tests nothing is the failure this
    // file exists to prevent.
    let python = Path::new("/usr/bin/python3");
    if !python.exists() {
        println!("SKIPPED: no /usr/bin/python3, so the socket attempt could not be made");
        return;
    }
    let ready = probe().expect("this machine can host the boundary");
    let readable = a_shell_needs();
    let outcome = ready
        .run(
            python,
            &[
                "-c",
                "import socket,sys\n\
                 s=socket.socket()\n\
                 s.settimeout(2)\n\
                 try:\n    s.connect(('1.1.1.1',80)); print('REACHED')\n\
                 except OSError as e:\n    print('UNREACHABLE', e.errno)",
            ],
            &readable,
            b"",
            &Bounds::modest(),
        )
        .expect("the process starts");
    let text = said(&outcome);
    assert!(
        !text.contains("REACHED"),
        "a function behind the boundary must not reach the network: {text}"
    );
    // `ENETUNREACH`, specifically. A test that accepted any failure would pass on a build
    // machine with no internet connection --- for the wrong reason, and silently, and it would
    // go on passing after somebody removed the network namespace. There is no route because
    // there is no network to have a route on, and that is a different errno from a refused
    // connection or a timeout.
    assert!(
        text.contains(&format!("UNREACHABLE {}", libc_enetunreach())),
        "and must fail because this namespace has no route, not merely fail: {text}"
    );
}

#[test]
fn it_cannot_write_anything() {
    // Two mechanisms have to fail for this to succeed --- the read-only bind and a file-size
    // limit of zero --- and that is deliberate. `ADR-0022` Decision 3: a function that could
    // write would be a second writer, and every guarantee resting on one authoritative writer
    // would become conditional on what somebody's Python did.
    let dir = tempfile::tempdir().expect("a directory");
    let outcome = shell(
        &format!("echo written > {}/new 2>&1 || echo REFUSED", dir.path().display()),
        &[dir.path()],
        &Bounds::modest(),
    );
    assert!(
        said(&outcome).contains("REFUSED"),
        "writing must be refused: {}",
        said(&outcome)
    );
    assert!(
        !dir.path().join("new").exists(),
        "and nothing must appear on the host side of the bind"
    );
}

#[test]
fn a_function_that_never_returns_is_killed() {
    // An aggregation that never returns is an outage rather than an error. Killed rather than
    // asked to stop: a polite request is a request something in an infinite loop never reads.
    let bounds = Bounds { wall: Duration::from_millis(400), ..Bounds::modest() };
    let started = std::time::Instant::now();
    let outcome = shell("while true; do :; done", &[], &bounds);
    let elapsed = started.elapsed();

    assert!(
        matches!(outcome, Outcome::OutOfTime { .. }),
        "a loop must end at the deadline: {}",
        said(&outcome)
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "and end *at* it, rather than whenever something else noticed: {elapsed:?}"
    );
}

#[test]
fn the_limits_are_the_ones_that_were_asked_for() {
    // Asked of the process itself. Every other test here proves a *consequence* of a limit;
    // this one proves the limit arrived, which is what distinguishes a bound that works from a
    // bound that happens to agree with the machine's default.
    let bounds = Bounds {
        wall: Duration::from_secs(5),
        cpu: 3,
        memory: 200 * 1024 * 1024,
        output: 4096,
    };
    let outcome = shell("ulimit -t; ulimit -f; ulimit -v", &[], &bounds);
    let text = said(&outcome);
    let lines: Vec<&str> = text.split_whitespace().collect();
    assert_eq!(lines.first().copied(), Some("3"), "the CPU bound: {text}");
    assert_eq!(lines.get(1).copied(), Some("0"), "no file of any size: {text}");
    assert_eq!(
        lines.get(2).copied(),
        Some("204800"),
        "the address space, in kilobytes: {text}"
    );
}

#[test]
fn more_output_than_it_was_allowed_is_refused_rather_than_truncated() {
    // Truncating would be the accommodating choice and it is the wrong one: a truncated Arrow
    // batch is not a smaller answer, it is a corrupt one, and the caller cannot tell.
    let bounds = Bounds { output: 64, ..Bounds::modest() };
    let outcome = shell("i=0; while [ $i -lt 200 ]; do echo hello; i=$((i+1)); done", &[], &bounds);
    assert!(
        matches!(outcome, Outcome::OutOfRoom { .. }),
        "output past the cap must be refused: {}",
        said(&outcome)
    );
}

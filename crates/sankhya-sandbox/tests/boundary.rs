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

// --- what the audit found the boundary was not doing -----------------------

/// Run Python behind the boundary with the shell's tree readable, or say why it was skipped.
///
/// A skip prints, and prints why. A test that quietly tests nothing is the failure this file
/// exists to prevent, and a sandbox test that quietly tests nothing is the worst instance of it.
fn python(script: &str, bounds: &Bounds) -> Option<Outcome> {
    let interpreter = Path::new("/usr/bin/python3");
    if !interpreter.exists() {
        println!("SKIPPED: no /usr/bin/python3");
        return None;
    }
    let ready = probe().expect("this machine can host the boundary");
    Some(
        ready
            .run(interpreter, &["-c", script], &a_shell_needs(), b"", bounds)
            .expect("the process starts"),
    )
}

#[test]
fn a_function_cannot_see_the_process_that_started_it() {
    // `SEC-10`. `unshare(CLONE_NEWPID)` places the caller's *children* in the new namespace and
    // leaves the caller behind --- and the caller was the process that then `exec`ed into the
    // worker. So the worker was in the host PID namespace; and because its credentials map to
    // the server's own user, `os.kill(os.getppid(), 9)` succeeded. A user function could stop
    // the server.
    //
    // Asserted as "it is PID 1 and has no parent" rather than by actually killing something,
    // because a test that proves it by killing the test runner has no way to report the result.
    let Some(outcome) = python("import os;print('PID', os.getpid(), 'PPID', os.getppid())", &Bounds::modest()) else {
        return;
    };
    let text = said(&outcome);
    assert!(
        text.contains("PID 1 "),
        "the worker must be the first process in a namespace of its own: {text}"
    );
    assert!(
        text.contains("PPID 0"),
        "and must have no parent it can name, because its parent is outside that namespace: \
         {text}"
    );
}

#[test]
fn a_function_is_not_root_inside_its_own_namespace() {
    // `SEC-09`, the half of it that is deliverable. The map read `0 <server uid> 1`, so the
    // worker was `root` in its namespace with a full capability set --- and `execve` of a
    // non-setuid file only drops capabilities when the effective user id is not zero, so the
    // interpreter started holding every one of them.
    //
    // Outside the namespace it is still the server's user, and no unprivileged mechanism
    // changes that. What this asserts is the part that is not a documentation fix.
    let Some(outcome) = python("import os;print('UID', os.getuid(), 'GID', os.getgid())", &Bounds::modest()) else {
        return;
    };
    let text = said(&outcome);
    assert!(
        !text.contains("UID 0 "),
        "the worker must not be root inside its namespace: {text}"
    );
    assert!(
        !text.contains("GID 0"),
        "nor in its group: {text}"
    );
}

#[test]
fn a_forked_grandchild_does_not_hold_the_answer_hostage() {
    // `SEC-12`. The parent read the child's output only once `try_wait` said it was gone, and
    // a pipe's write end is held by every process that inherited it. A grandchild that outlived
    // the child meant `read_to_end` never saw end-of-file --- with the deadline loop already
    // exited, so nothing was left to kill anything. This runs inside a DataFusion accumulator
    // on a Tokio worker thread, and a handful of such queries stop the server.
    //
    // The function here answers and then leaves a child sleeping on the same pipe.
    let started = std::time::Instant::now();
    let Some(outcome) = python(
        "import os,sys,time\n\
         sys.stdout.write('ANSWERED')\n\
         sys.stdout.flush()\n\
         if os.fork() == 0:\n    time.sleep(30)\n    os._exit(0)\n\
         os._exit(0)",
        &Bounds { wall: std::time::Duration::from_secs(3), ..Bounds::modest() },
    ) else {
        return;
    };
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(10),
        "a grandchild holding the pipe must not hold the caller: {elapsed:?}"
    );
    assert!(
        said(&outcome).contains("ANSWERED"),
        "and what the function did write must still come back: {}",
        said(&outcome)
    );
}

#[test]
fn the_output_bound_fires_at_the_size_it_is_set_to() {
    // `SEC-13`. Output was read only after the child exited, so a child writing more than the
    // pipe holds --- about 64 KiB --- blocked on the write and was killed at the deadline, and
    // reported as having run out of *time*. With the shipped cap at 64 MiB the `OutOfRoom` arm
    // was unreachable, and the test that "proved" the cap used a cap of 64 bytes.
    //
    // # Why the writer is slow on purpose
    //
    // Because the two halves of the bound have to be told apart. Counting the bytes *while the
    // run is going* is what stops a runaway function; checking the total after it ends is what
    // catches one that finished between two polls. A fast writer would be caught by either, so
    // it proves neither.
    //
    // This one writes 64 KiB every 10ms and would go on for ten seconds. Against a 1 MiB bound
    // the counting stops it after about a fifth of a second; without that counting it runs into
    // the two-second deadline and comes back as `OutOfTime`, which is the wrong sentence about
    // the wrong problem.
    let bounds = Bounds {
        output: 1024 * 1024,
        wall: Duration::from_secs(2),
        ..Bounds::modest()
    };
    let started = std::time::Instant::now();
    let Some(outcome) = python(
        "import sys,time\n\
         block = 'x' * 65536\n\
         for _ in range(1000):\n    sys.stdout.write(block)\n    sys.stdout.flush()\n    \
         time.sleep(0.01)",
        &bounds,
    ) else {
        return;
    };
    assert!(
        matches!(outcome, Outcome::OutOfRoom { .. }),
        "a function writing past its output bound must be reported as having passed that \
         bound, not as having been killed at a deadline it never reached: {}",
        said(&outcome)
    );
    assert!(
        started.elapsed() < Duration::from_millis(1_800),
        "and must be stopped when it passes the bound rather than allowed to write until the \
         deadline: {:?}",
        started.elapsed()
    );
}

#[test]
fn a_probe_that_passed_means_a_run_can_start() {
    // `SEC-14`. `ADR-0023` Decision 3: *"The startup probe runs the mechanism, once, against a
    // trivial worker --- it does not read a capability flag and hope."* It forked, called
    // `unshare`, and stopped. A machine where `unshare` succeeds and `pivot_root` fails ---
    // which is any machine whose `/` cannot be made private, or whose temporary directory is on
    // a filesystem that cannot host a mount --- passed the probe and failed at the first
    // `CREATE AGGREGATION` in production. That is the outcome the decision exists to prevent.
    //
    // The property is a conditional, and it is stated as one: a probe that returned success is
    // a promise that a run can be started, so this asserts exactly that promise and nothing
    // about whether this machine can host the boundary at all.
    let Ok(ready) = probe() else {
        println!("SKIPPED: this machine cannot host the boundary, which is what the probe said");
        return;
    };
    let dir = tempfile::tempdir().expect("a directory");
    let outcome = ready.run(
        Path::new("/bin/sh"),
        &["-c", "exit 0"],
        &a_shell_needs(),
        b"",
        &Bounds::modest(),
    );
    assert!(
        outcome.is_ok(),
        "the probe said the boundary can be built here, so building one must not fail: {:?}",
        outcome.err()
    );
    // And the mounts really were applied, rather than the spawn merely succeeding: a run whose
    // `pivot_root` was skipped would still start, and would see the whole machine.
    let outcome = ready
        .run(
            Path::new("/bin/sh"),
            &["-c", &format!("test -e {} && echo LEAKED || echo SEALED", dir.path().display())],
            &a_shell_needs(),
            b"",
            &Bounds::modest(),
        )
        .expect("the process starts");
    assert!(
        said(&outcome).contains("SEALED"),
        "a probe that passed must mean the boundary is applied, not merely that a process \
         started: {}",
        said(&outcome)
    );
}

#[test]
fn a_worker_killed_at_the_deadline_does_not_outlive_the_kill() {
    // The worker is PID 1 of a namespace of its own, so nothing reaps it and its parent's death
    // is not its own --- and the deadline upstream kills the *intermediate*, which is the only
    // process the caller has a handle on. Without `PR_SET_PDEATHSIG` the function goes on
    // running after the query that started it has been told it timed out.
    //
    // Observed through the pipe rather than through the process table, because a worker in its
    // own PID namespace has no identity this side can name. A worker that is still alive still
    // holds the write end of standard output, so collecting the answer waits for it; a worker
    // that died with the kill closed it, and collecting returns at once. The gap between those
    // two is the whole of the property.
    let bounds = Bounds { wall: Duration::from_millis(300), ..Bounds::modest() };
    let started = std::time::Instant::now();
    let outcome = shell("while true; do :; done", &[], &bounds);
    let elapsed = started.elapsed();

    assert!(
        matches!(outcome, Outcome::OutOfTime { .. }),
        "the control: it must have been killed at the deadline: {}",
        said(&outcome)
    );
    assert!(
        elapsed < Duration::from_millis(1_500),
        "and nothing must still be holding its output open afterwards --- a run that takes \
         seconds past a 300ms deadline is one whose worker survived the kill: {elapsed:?}"
    );
}

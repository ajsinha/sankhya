//! What an accept loop does when `accept()` fails.
//!
//! # Why this is a crate and not three `match` arms
//!
//! SANKHYA runs three accept loops --- the PostgreSQL wire protocol, the columnar door over
//! gRPC, and the metrics endpoint --- and each of them had a different answer. One propagated
//! the error and brought the process down. Two discarded it and looped immediately. All three
//! were wrong, in two different directions, and the audit found the disagreement by reading
//! them next to each other, which is not how anybody reads code that lives in three crates.
//!
//! So the answer is decided **here, once**, and each door asks.
//!
//! # The two wrong answers
//!
//! **Propagating kills the server.** `ECONNABORTED` is what a load balancer's health check
//! produces when it opens a connection and closes it before the handshake; it is routine, it
//! is not the server's fault, and it took the whole process down with it. `EMFILE` --- the
//! process is at its descriptor limit --- did the same, which turns a transient shortage into
//! an outage.
//!
//! **Discarding spins.** A descriptor limit does not clear because the loop asked again
//! immediately: the retry is instant, it fails instantly, and the loop burns a core doing it
//! while every other task on the runtime --- including the ones holding the descriptors that
//! would have released --- competes for the CPU that would let them finish. The recovery
//! needs the loop to *stop asking* for a moment. Fifty milliseconds is enough to matter and
//! short enough that nobody waits for it.
//!
//! And a listener that is genuinely broken --- a descriptor that is not a socket any more ---
//! fails identically on every call for ever. Looping on that is a process that is up,
//! answering nothing, and reporting nothing, which is worse than exiting: an orchestrator
//! restarts a process that dies, and stares at one that lives.

#![doc(html_root_url = "https://docs.rs/sankhya-accept")]

use std::time::Duration;

/// How long to stop accepting after a resource shortage.
///
/// Short enough that a descriptor freed a moment later is used a moment later; long enough
/// that the loop is not the reason the descriptor never frees.
pub const PAUSE: Duration = Duration::from_millis(50);

/// What an accept loop should do about a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Response {
    /// Accept the next caller now. The connection failed before it existed, and the listener
    /// is fine.
    Continue,
    /// Wait, then accept again. The process is short of something that retrying cannot
    /// conjure, and asking again immediately is what stops it being returned.
    Pause(Duration),
    /// Stop. Every future accept on this listener fails the same way.
    Stop,
}

/// Out of file descriptors for this process.
const EMFILE: i32 = 24;
/// Out of file descriptors for the machine.
const ENFILE: i32 = 23;
/// Out of buffer space.
const ENOBUFS: i32 = 105;
/// Out of memory.
const ENOMEM: i32 = 12;

/// Decide what to do about a failed `accept()`.
///
/// # The default is [`Response::Stop`], deliberately
///
/// An unrecognised error is one nobody has thought about, and the two safe-looking answers
/// are not equally safe. Continuing turns it into a silent hot loop that an operator
/// diagnoses from a CPU graph; stopping turns it into a crash with the error in it, which is
/// the one that gets fixed. New arms belong here, named, when a real deployment produces one.
#[must_use]
pub fn response(error: &std::io::Error) -> Response {
    use std::io::ErrorKind;
    match error.kind() {
        // The peer went away between the SYN and the accept. Routine behind any load
        // balancer, and the connection it refers to no longer exists.
        ErrorKind::ConnectionAborted | ErrorKind::ConnectionReset => Response::Continue,
        // A signal arrived mid-call. Nothing happened.
        ErrorKind::Interrupted => Response::Continue,
        // A non-blocking listener with nothing pending. The runtime will wake the loop again.
        ErrorKind::WouldBlock => Response::Continue,
        // A packet filter refused this peer. Theirs, not ours.
        ErrorKind::PermissionDenied => Response::Continue,
        _ => match error.raw_os_error() {
            Some(EMFILE | ENFILE | ENOBUFS | ENOMEM) => Response::Pause(PAUSE),
            _ => Response::Stop,
        },
    }
}

#[cfg(test)]
mod deciding {
    use super::{response, Response, EMFILE, ENFILE, ENOBUFS, ENOMEM, PAUSE};
    use std::io::{Error, ErrorKind};

    #[test]
    fn an_aborted_connection_is_that_connections_problem() {
        let error = Error::from(ErrorKind::ConnectionAborted);
        assert_eq!(response(&error), Response::Continue);
    }

    #[test]
    fn a_reset_is_too() {
        assert_eq!(
            response(&Error::from(ErrorKind::ConnectionReset)),
            Response::Continue
        );
    }

    #[test]
    fn a_signal_interrupted_nothing() {
        assert_eq!(
            response(&Error::from(ErrorKind::Interrupted)),
            Response::Continue
        );
    }

    #[test]
    fn a_filtered_peer_is_not_our_failure() {
        assert_eq!(
            response(&Error::from(ErrorKind::PermissionDenied)),
            Response::Continue
        );
    }

    #[test]
    fn running_out_of_descriptors_pauses_rather_than_spinning() {
        // The failure this crate exists for. Continuing here is a hot loop that competes
        // with the tasks holding the descriptors it is waiting for.
        for code in [EMFILE, ENFILE, ENOBUFS, ENOMEM] {
            assert_eq!(
                response(&Error::from_raw_os_error(code)),
                Response::Pause(PAUSE),
                "os error {code} should pause"
            );
        }
    }

    #[test]
    fn a_shortage_never_stops_the_server() {
        // Stated separately from the arm above because it is the property `OPS-08` is
        // about: a transient shortage used to end the process.
        assert_ne!(response(&Error::from_raw_os_error(EMFILE)), Response::Stop);
    }

    #[test]
    fn a_broken_listener_stops() {
        // EBADF: the descriptor is not a socket. Every call from here fails identically,
        // and a process that loops on it is up and answering nothing.
        assert_eq!(response(&Error::from_raw_os_error(9)), Response::Stop);
    }

    #[test]
    fn an_unrecognised_error_stops_rather_than_looping() {
        assert_eq!(response(&Error::from(ErrorKind::Other)), Response::Stop);
    }

    #[test]
    fn the_pause_is_short_enough_that_nobody_waits_for_it() {
        assert!(PAUSE.as_millis() <= 100);
        assert!(PAUSE.as_millis() >= 1);
    }
}

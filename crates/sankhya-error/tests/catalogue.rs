//! The catalogue must stay classifiable, unique and documented.
//!
//! Without these tests a newly added variant silently inherits whatever the fallback
//! happens to be — which is how a fatal condition ends up being retried forever.

// Tests may panic — that is how a test reports a failure. The workspace denies
// `unwrap`, `expect`, `panic` and indexing because a *server* must not do those things
// on data it did not choose; a test chooses all of its data, and an assertion that
// cannot fail loudly is worse than useless.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use sankhya_error::{Class, Classify, Error};

#[test]
fn every_variant_is_classifiable_and_documented() {
    for e in Error::all() {
        let code = e.code().as_str();
        assert!(
            code.starts_with("SNK-") && code.len() == 9,
            "{code} does not match the SNK-Xnnnn form"
        );
        assert!(
            !e.remediation().is_empty(),
            "{code} has no remediation; every user-reachable error must say what to do"
        );
        // Exercising class() on every variant is the point: a non-exhaustive match
        // would fail to compile, and a fallback arm would be caught in review.
        let _ = e.class();
    }
}

#[test]
fn codes_are_unique() {
    let mut codes: Vec<&str> = Error::all().iter().map(|e| e.code().as_str()).collect();
    codes.sort_unstable();
    let before = codes.len();
    codes.dedup();
    assert_eq!(before, codes.len(), "duplicate error code in the catalogue");
}

#[test]
fn code_prefix_matches_class() {
    for e in Error::all() {
        let code = e.code().as_str();
        let expected = match e.class() {
            Class::User => 'C',
            Class::Resource => 'R',
            Class::Conflict => 'F',
            Class::Retryable { .. } => 'T',
            Class::Cancelled => 'X',
            Class::Fatal => 'S',
        };
        let actual = code.chars().nth(4).unwrap_or('?');
        assert_eq!(
            actual,
            expected,
            "{code} prefix disagrees with its class {:?}",
            e.class()
        );
    }
}

#[test]
fn conflict_is_not_blindly_retryable() {
    // A conflict needs a new plan against the new state. Retrying the same plan
    // loses the same race again, and calling it retryable would invite exactly that.
    let conflict = Error::CommitConflict { detail: None };
    assert!(!conflict.class().is_retryable());
    assert_eq!(conflict.class(), Class::Conflict);
}

#[test]
fn only_fatal_pages_a_human() {
    for e in Error::all() {
        assert_eq!(
            e.class().is_pageable(),
            e.class() == Class::Fatal,
            "{} paging behaviour disagrees with its class",
            e.code()
        );
    }
}

#[test]
fn source_endangerment_is_fatal() {
    // INV-2. If this is ever downgraded, SANKHYA could quietly keep running while
    // the database it depends on fills its log volume.
    assert_eq!(
        Error::SourceEndangered { detail: None }.class(),
        Class::Fatal
    );
}

#[test]
fn coverage_gap_is_fatal_not_user_error() {
    // A tier-coverage gap means the splice could not be proven safe. Answering
    // partially would be a silent wrong answer, so it is refused and treated as
    // a correctness event.
    assert_eq!(Error::CoverageGap { detail: None }.class(), Class::Fatal);
}

#[test]
fn display_includes_code_and_detail() {
    let e = Error::InvalidQuery("column 'nope' not found");
    let rendered = e.to_string();
    assert!(rendered.contains("SNK-C0001"), "{rendered}");
    assert!(rendered.contains("column 'nope' not found"), "{rendered}");
}

// --- the protocol mapping -------------------------------------------------

use sankhya_error::protocol::{
    sqlstate, statuses_for, statuses_for_denied, statuses_for_unauthenticated, GrpcStatus,
    WireError,
};

/// Every class, so a new one cannot be added without deciding what it looks like on a wire.
const EVERY_CLASS: &[Class] = &[
    Class::User,
    Class::Retryable { after: None },
    Class::Conflict,
    Class::Resource,
    Class::Cancelled,
    Class::Fatal,
];

#[test]
fn every_class_maps_to_all_three_protocols() {
    // A class with no mapping would fall back to whatever the last arm happens to be, and
    // the fallback is what a client acts on.
    for class in EVERY_CLASS {
        let status = statuses_for(*class);
        assert!(
            (100..600).contains(&status.http),
            "{class:?} has an implausible HTTP status {}",
            status.http
        );
        assert_eq!(
            status.sqlstate.as_str().len(),
            5,
            "{class:?} has a SQLSTATE that is not five characters"
        );
    }
}

#[test]
fn the_three_wires_agree_about_whether_to_retry() {
    // A gRPC client retrying while a SQL client gives up, for the same condition, is a bug
    // nobody finds until it matters. The statuses are derived from one class each, so they
    // cannot disagree — this asserts that the derivation actually holds.
    for class in EVERY_CLASS {
        let status = statuses_for(*class);
        let grpc_retries = status.client_should_retry();
        let http_retries = status.http == 503;
        assert_eq!(
            grpc_retries, http_retries,
            "{class:?}: gRPC says retry={grpc_retries}, HTTP says retry={http_retries}"
        );
        assert_eq!(
            grpc_retries,
            class.is_retryable(),
            "{class:?}: the wire disagrees with the class about retryability"
        );
    }
}

#[test]
fn a_conflict_is_aborted_rather_than_unavailable() {
    // The gRPC specification says Aborted means retry at a higher level, which is exactly
    // right: the same plan loses again, a new one may not. Unavailable would tell a client
    // to retry the identical request, which is the one thing that cannot work.
    let status = statuses_for(Class::Conflict);
    assert_eq!(status.grpc, GrpcStatus::Aborted);
    assert_eq!(status.sqlstate, sqlstate::SERIALIZATION_FAILURE);
    assert!(
        !status.client_should_retry(),
        "a blind retry of a conflict loses again"
    );
}

#[test]
fn a_permission_refusal_does_not_look_like_a_syntax_error() {
    // Its own mapping rather than a Class variant, because a driver receiving 42601 for a
    // permission problem reports a syntax error to a confused user.
    let denied = statuses_for_denied();
    assert_eq!(denied.sqlstate, sqlstate::INSUFFICIENT_PRIVILEGE);
    assert_eq!(denied.grpc, GrpcStatus::PermissionDenied);
    assert_eq!(denied.http, 403);
    assert_ne!(denied.sqlstate, statuses_for(Class::User).sqlstate);
}

#[test]
fn a_failed_authentication_is_distinguishable_from_a_refused_action() {
    // 401 and 403 mean different things to a client: reconnect with credentials, versus
    // do not bother.
    let unauthenticated = statuses_for_unauthenticated();
    assert_eq!(unauthenticated.http, 401);
    assert_eq!(unauthenticated.sqlstate, sqlstate::INVALID_AUTHORIZATION);
    assert_ne!(unauthenticated.http, statuses_for_denied().http);
}

#[test]
fn every_sqlstate_comes_from_a_standard_class() {
    // An invented SQLSTATE is worse than a wrong one: a driver that does not recognise the
    // class treats it as a generic failure, and the fallback is usually "do not retry".
    const STANDARD_CLASSES: &[&str] = &[
        "00", "01", "02", "07", "08", "09", "0A", "21", "22", "23", "24", "25", "26", "27", "28",
        "2B", "2D", "2F", "34", "38", "39", "3B", "3D", "3F", "40", "42", "44", "53", "54", "55",
        "57", "58", "F0", "HV", "P0", "XX",
    ];
    let every_state = [
        sqlstate::DATA_EXCEPTION,
        sqlstate::INVALID_AUTHORIZATION,
        sqlstate::SERIALIZATION_FAILURE,
        sqlstate::INSUFFICIENT_PRIVILEGE,
        sqlstate::SYNTAX_ERROR,
        sqlstate::INSUFFICIENT_RESOURCES,
        sqlstate::CONFIGURATION_LIMIT_EXCEEDED,
        sqlstate::QUERY_CANCELED,
        sqlstate::IO_ERROR,
        sqlstate::INTERNAL_ERROR,
    ];
    for state in every_state {
        assert!(
            STANDARD_CLASSES.contains(&state.class()),
            "{state} is not in a standard SQLSTATE class"
        );
        assert!(state.as_str().chars().all(|c| c.is_ascii_alphanumeric()));
    }
}

#[test]
fn grpc_status_numbers_match_the_specification() {
    // These are wire values. Getting one wrong produces a client that branches on the
    // wrong condition, silently.
    assert_eq!(GrpcStatus::Cancelled.number(), 1);
    assert_eq!(GrpcStatus::InvalidArgument.number(), 3);
    assert_eq!(GrpcStatus::DeadlineExceeded.number(), 4);
    assert_eq!(GrpcStatus::PermissionDenied.number(), 7);
    assert_eq!(GrpcStatus::ResourceExhausted.number(), 8);
    assert_eq!(GrpcStatus::Aborted.number(), 10);
    assert_eq!(GrpcStatus::Internal.number(), 13);
    assert_eq!(GrpcStatus::Unavailable.number(), 14);
}

#[test]
fn every_catalogued_error_has_a_wire_form_and_a_remediation() {
    // The exhaustive version: a new catalogue entry cannot be added without both.
    for error in Error::all() {
        let wire = WireError::of(&error, "context");
        assert_eq!(wire.code, error.code());
        assert!(
            !wire.remediation.is_empty(),
            "{} has no remediation, so the runbook it appears in cannot say what to do",
            wire.code
        );
        assert_eq!(wire.status, statuses_for(error.class()));
    }
}

#[test]
fn a_wire_error_carries_its_code_and_its_remediation() {
    // The code is what a runbook references and the remediation is what the person reading
    // it needs. An error that carries neither is a message somebody has to escalate.
    let error = Error::CommitConflict { detail: None };
    let wire = WireError::of(&error, "the snapshot moved");

    assert_eq!(wire.code, error.code());
    assert_eq!(wire.remediation, error.remediation());
    assert!(!wire.remediation.is_empty());
    assert!(wire.to_string().contains(wire.code.as_str()));
    assert!(wire.to_string().contains(wire.status.sqlstate.as_str()));
}

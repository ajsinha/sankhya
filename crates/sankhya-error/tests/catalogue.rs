//! The catalogue must stay classifiable, unique and documented.
//!
//! Without these tests a newly added variant silently inherits whatever the fallback
//! happens to be — which is how a fatal condition ends up being retried forever.

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
        assert_eq!(actual, expected, "{code} prefix disagrees with its class {:?}", e.class());
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
    assert_eq!(Error::SourceEndangered { detail: None }.class(), Class::Fatal);
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

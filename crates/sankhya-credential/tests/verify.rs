//! A password is checked against something, and the something is checkable.
//!
//! `SEC-01`: there was no credential store, no hash and no comparison anywhere in the
//! workspace. The entire check was that a password had been *presented* and was non-empty, and
//! because the username is self-asserted, any client connected as any user with any bytes.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use sankhya_credential::{fresh_salt, make, Malformed, Verifier, ITERATIONS};

#[test]
fn the_right_password_verifies_and_a_wrong_one_does_not() {
    let salt = fresh_salt().expect("the system has randomness");
    // A low count here and nowhere else: this test runs on every build, and 600,000 iterations
    // per assertion is a second of gate time to prove a property that does not depend on the
    // number. The default is asserted separately, from the constant.
    let stored = make(b"correct horse", &salt, 4096).expect("a verifier");
    let verifier = Verifier::parse(&stored).expect("what we just wrote is readable");

    assert!(verifier.verifies(b"correct horse"));
    assert!(!verifier.verifies(b"correct hors"), "a prefix is not the password");
    assert!(!verifier.verifies(b"correct horsee"), "nor is an extension");
    assert!(!verifier.verifies(b""), "nor is nothing");
    assert!(!verifier.verifies(b"Correct horse"), "nor is a different case");
}

#[test]
fn two_verifiers_of_one_password_differ() {
    // The whole point of a salt. Identical verifiers would tell anybody holding the file which
    // users share a password, before any of them is cracked.
    let one = make(b"same", &fresh_salt().expect("randomness"), 4096).expect("a verifier");
    let two = make(b"same", &fresh_salt().expect("randomness"), 4096).expect("a verifier");
    assert_ne!(one, two, "two verifiers of one password are identical");

    // And both verify it, so the difference is the salt rather than a broken derivation.
    assert!(Verifier::parse(&one).expect("readable").verifies(b"same"));
    assert!(Verifier::parse(&two).expect("readable").verifies(b"same"));
}

#[test]
fn the_stored_line_says_how_it_was_made() {
    // An operator raising the iteration count must not invalidate every credential already
    // written down. Reading the count from the line is what lets old verifiers keep working at
    // the count they were made with while new ones are made at the current default.
    let stored = make(b"pw", &fresh_salt().expect("randomness"), 4096).expect("a verifier");
    assert!(stored.starts_with("pbkdf2-sha256$4096$"), "{stored}");
    assert_eq!(Verifier::parse(&stored).expect("readable").iterations(), 4096);

    let stronger = make(b"pw", &fresh_salt().expect("randomness"), 9001).expect("a verifier");
    assert_eq!(Verifier::parse(&stronger).expect("readable").iterations(), 9001);
    assert!(
        Verifier::parse(&stored).expect("readable").verifies(b"pw"),
        "raising the count invalidated a credential made under the old one"
    );
}

#[test]
fn a_verifier_that_is_not_one_is_refused_with_the_reason() {
    // The reasons are distinguished because they send an operator to different places: a
    // scheme this build does not know is a version to upgrade, and a shape that is wrong is a
    // line to retype.
    assert_eq!(Verifier::parse("").unwrap_err(), Malformed::Shape);
    assert_eq!(Verifier::parse("a$b$c").unwrap_err(), Malformed::Shape);
    assert_eq!(Verifier::parse("a$b$c$d$e").unwrap_err(), Malformed::Shape);
    assert_eq!(
        Verifier::parse("scrypt$1$AAAA$AAAA").unwrap_err(),
        Malformed::Scheme("scrypt".to_string())
    );
    assert_eq!(
        Verifier::parse("pbkdf2-sha256$0$AAAA$AAAA").unwrap_err(),
        Malformed::Iterations
    );
    assert_eq!(
        Verifier::parse("pbkdf2-sha256$x$AAAA$AAAA").unwrap_err(),
        Malformed::Iterations
    );
    assert_eq!(
        Verifier::parse("pbkdf2-sha256$1$not base64!$AAAA").unwrap_err(),
        Malformed::Encoding
    );
}

#[test]
fn a_verifier_cannot_be_made_without_a_salt_or_a_count() {
    // Both would produce something that looks like a verifier and is not one: an empty salt
    // makes every verifier of a password identical, and zero iterations is no derivation.
    assert_eq!(make(b"pw", b"", 4096), None);
    assert_eq!(make(b"pw", b"salt", 0), None);
}

#[test]
fn the_default_count_is_the_one_the_documentation_names() {
    // A number in a comment and a number in the code drift. This is the assertion that stops
    // the configuration file describing a strength the binary does not use.
    assert_eq!(ITERATIONS, 600_000);
}

#[test]
fn a_verifier_round_trips_through_its_text() {
    // The format is a file format: it is written by one run and read by another, and a salt
    // that does not survive base64 is a credential that silently never verifies again.
    let salt = fresh_salt().expect("randomness");
    let stored = make(b"round trip", &salt, 4096).expect("a verifier");
    let read = Verifier::parse(&stored).expect("readable");
    assert!(read.verifies(b"round trip"));

    // Every byte value through the encoder, not just the ones a random salt happened to hold.
    let all: Vec<u8> = (0..=255).collect();
    let stored = make(b"round trip", &all, 4096).expect("a verifier");
    assert!(Verifier::parse(&stored).expect("readable").verifies(b"round trip"));
}

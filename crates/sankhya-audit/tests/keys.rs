//! Envelope encryption: rotation that does not rewrite data, and what retiring a key costs.
//!
//! The claim under test is a performance one with a correctness consequence. Rotating a key
//! that encrypted data directly means re-encrypting every byte — a multi-day job on a
//! warehouse, which cannot be interrupted and must not be run twice. With an envelope,
//! rotation re-wraps a few thousand small keys and the data is never touched.
//!
//! `rotation_is_idempotent` matters because rotation is exactly the kind of job that gets
//! run twice.

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

use sankhya_audit::keys::{Envelope, KeyError, KeyId, KeyProvider, NoEncryption};

const EMAIL_KEY: &[u8] = b"a-data-key-for-the-email-column";
const TOTAL_KEY: &[u8] = b"a-data-key-for-the-total-column";

fn protected() -> (Envelope, NoEncryption) {
    let provider = NoEncryption::new("tenant-master");
    let mut envelope = Envelope::new();
    envelope
        .protect("email", EMAIL_KEY, &provider)
        .expect("wrapping under the current key");
    envelope
        .protect("total", TOTAL_KEY, &provider)
        .expect("wrapping under the current key");
    (envelope, provider)
}

#[test]
fn a_protected_column_yields_its_data_key_again() {
    let (envelope, provider) = protected();
    assert!(envelope.protects("email"));
    assert_eq!(
        envelope.data_key("email", &provider).expect("unwraps"),
        Some(EMAIL_KEY.to_vec())
    );
}

#[test]
fn an_unprotected_column_is_absent_rather_than_an_error() {
    // Not every column is encrypted, and asking about one that is not is an ordinary
    // question rather than a failure.
    let (envelope, provider) = protected();
    assert!(!envelope.protects("id"));
    assert_eq!(envelope.data_key("id", &provider).expect("no error"), None);
}

#[test]
fn rotation_rewraps_the_keys_and_leaves_the_data_alone() {
    // The whole reason for the envelope. Note what this test does *not* do: it never
    // touches encrypted data, because rotation never touches encrypted data.
    let (mut envelope, mut provider) = protected();
    assert_eq!(envelope.wrapped_by("email").map(|k| k.version), Some(1));

    let new_key = provider.rotate_key();
    assert_eq!(new_key.version, 2);

    let rotation = envelope.rotate(&provider).expect("rotation succeeds");
    assert_eq!(rotation.rewrapped, 2);
    assert_eq!(rotation.already_current, 0);
    assert_eq!(rotation.to.version, 2);
    assert!(rotation.summary().contains("no data was read"));

    assert_eq!(envelope.wrapped_by("email").map(|k| k.version), Some(2));
    assert_eq!(
        envelope.data_key("email", &provider).expect("unwraps"),
        Some(EMAIL_KEY.to_vec()),
        "the data key is unchanged, so the data it protects is still readable"
    );
}

#[test]
fn rotation_is_idempotent() {
    // Rotation is exactly the kind of job that gets run twice — by a retry, by two
    // operators, by a scheduler that did not see the first one finish.
    let (mut envelope, mut provider) = protected();
    provider.rotate_key();

    let first = envelope.rotate(&provider).expect("first rotation");
    assert_eq!(first.rewrapped, 2);
    assert!(first.changed_anything());

    let second = envelope.rotate(&provider).expect("second rotation");
    assert_eq!(second.rewrapped, 0);
    assert_eq!(second.already_current, 2);
    assert!(!second.changed_anything());

    assert_eq!(
        envelope.data_key("total", &provider).expect("unwraps"),
        Some(TOTAL_KEY.to_vec())
    );
}

#[test]
fn data_wrapped_under_an_older_version_stays_readable_during_rotation() {
    // A provider that can only unwrap the current version turns every rotation into an
    // outage for everything not yet re-wrapped. Here one column is rotated and the other
    // is not, which is the state a rotation is in for most of its duration.
    let (mut envelope, mut provider) = protected();
    provider.rotate_key();

    // Rotate only 'email', by protecting it afresh under the new current key.
    envelope
        .protect("email", EMAIL_KEY, &provider)
        .expect("re-protecting");
    assert_eq!(envelope.wrapped_by("email").map(|k| k.version), Some(2));
    assert_eq!(envelope.wrapped_by("total").map(|k| k.version), Some(1));

    assert_eq!(
        envelope.data_key("total", &provider).expect("unwraps"),
        Some(TOTAL_KEY.to_vec()),
        "a column still on the old version must remain readable mid-rotation"
    );
}

#[test]
fn retiring_a_key_version_before_its_data_is_rewrapped_destroys_that_data() {
    // Not a configuration mistake — data destruction. The error says so, because whoever
    // reads it needs to understand that restoring the setting will not restore the data.
    let (envelope, mut provider) = protected();
    let version_one = KeyId::new("tenant-master", 1);
    provider.rotate_key();
    provider.retire(&version_one);

    let Err(error) = envelope.data_key("email", &provider) else {
        panic!("a key wrapped under a retired version must not unwrap");
    };
    assert_eq!(error, KeyError::UnknownKey { key: version_one });
    assert!(error.to_string().contains("destroys that data"), "{error}");
}

#[test]
fn a_key_wrapped_under_one_version_does_not_unwrap_under_another() {
    // The property the rotation tests rest on. Without it, a version mismatch would be
    // silent and rotation could appear to work while producing keys that decrypt nothing.
    let provider = NoEncryption::new("k");
    let first = provider
        .wrap(&KeyId::new("k", 1), EMAIL_KEY)
        .expect("wraps");

    let mut later = NoEncryption::new("k");
    later.rotate_key();
    let second = later.wrap(&KeyId::new("k", 2), EMAIL_KEY).expect("wraps");

    assert_ne!(
        first.bytes, second.bytes,
        "the same data key wrapped under two versions must not produce the same bytes"
    );
}

#[test]
fn wrapping_under_an_unknown_key_is_refused() {
    let provider = NoEncryption::new("k");
    assert_eq!(
        provider.wrap(&KeyId::new("k", 99), EMAIL_KEY),
        Err(KeyError::UnknownKey {
            key: KeyId::new("k", 99)
        })
    );
}

#[test]
fn the_test_provider_says_in_its_own_description_that_it_encrypts_nothing() {
    // An installation running this by accident has no protection at all, so the
    // description is written to look wrong in a production log.
    let description = NoEncryption::new("k").describe();
    assert!(description.contains("NO ENCRYPTION"));
    assert!(description.contains("must not be used"));
}

#[test]
fn a_key_version_orders_and_advances() {
    let first = KeyId::new("master", 1);
    let second = first.next_version();
    assert_eq!(second.version, 2);
    assert_eq!(second.name, "master");
    assert!(
        first < second,
        "versions order so the current one is the greatest"
    );
    assert_eq!(second.to_string(), "master:v2");
}

#[test]
fn the_columns_an_envelope_protects_are_listed_in_a_stable_order() {
    // So two nodes reporting their encryption state produce comparable output.
    let (envelope, _) = protected();
    assert_eq!(envelope.protected_columns(), vec!["email", "total"]);
}

//! Mirror-naming tests.
//!
//! The refusals carry most of the weight. A scheme that always produces *a* name is
//! easy; one that refuses to produce a misleading name is the point.

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

use proptest::prelude::*;
use sankhya_schema::{segment_for, CollisionCheck, NameClass, NamingError, TableLocation};

#[test]
fn ordinary_identifiers_pass_through_untouched() {
    // The overwhelmingly common case: the source folds unquoted identifiers to lower
    // case, so they are already valid path segments and nothing happens at all.
    for name in [
        "orders",
        "order_lines",
        "device_readings",
        "t2",
        "shipment_scans_2025",
    ] {
        let segment = segment_for(name).expect("maps");
        assert_eq!(segment.as_str(), name, "{name} should be untouched");
        assert!(
            segment.is_identity(),
            "{name} should require no transformation"
        );
    }
}

#[test]
fn every_acceptance_table_is_identity_mapped() {
    // If any of the ten fixture tables needed transforming, the warehouse layout would
    // stop being self-explanatory for the very dataset we test against.
    for name in [
        "shipment_scans",
        "device_readings",
        "order_lines",
        "inventory_levels",
        "support_tickets",
        "media_assets",
        "energy_intervals",
        "route_legs",
        "sensor_calibrations",
        "access_events",
    ] {
        let location = TableLocation::resolve("public", name).expect("resolves");
        assert!(
            location.is_fully_relatable(),
            "{name} should map with no transformation at all"
        );
        assert_eq!(location.relative_path(), format!("public/{name}"));
    }
}

#[test]
fn awkward_identifiers_stay_legible() {
    // The transformation exists to keep names readable, not merely unique.
    for (input, expected) in [
        ("Orders", "orders"),
        ("Order Items", "order-items"),
        ("customer.profile", "customer-profile"),
        ("weird//name", "weird-name"),
        ("  padded  ", "padded"),
        ("MiXeD_Case_42", "mixed_case_42"),
    ] {
        let segment = segment_for(input).expect("maps");
        assert_eq!(segment.as_str(), expected, "{input:?}");
        assert_eq!(segment.class(), NameClass::Transformed);
    }
}

#[test]
fn the_separator_signals_that_a_transformation_happened() {
    // The separator is not legal in an unquoted source identifier, so its presence is
    // itself evidence — an operator seeing it knows to check the recorded original.
    let segment = segment_for("Order Items").expect("maps");
    assert!(segment.as_str().contains('-'));
    assert!(!segment.is_identity());
}

#[test]
fn bare_reserved_names_are_refused() {
    // A table whose identifier IS a reserved segment cannot be stored there, and no
    // transformation of it would be legible, so it is refused.
    for name in ["metadata", "data", "con", "lpt1", "graveyard", "nul"] {
        let err = segment_for(name).expect_err("must refuse");
        assert!(
            matches!(err, NamingError::Reserved { .. }),
            "{name} should be refused as reserved, got {err:?}"
        );
    }
}

#[test]
fn hidden_prefixed_names_are_relocated_rather_than_refused() {
    // A table genuinely named `_delta_log` is unusual but legitimate. Storing it at a
    // safe, visible segment is better than refusing it: the transformation is legible,
    // it cannot collide with the real metadata directory, and the original is recorded.
    //
    // Refusing here would block a valid table for no benefit.
    for (input, expected) in [
        ("_delta_log", "t_delta_log"),
        ("_sankhya", "t_sankhya"),
        ("_internal", "t_internal"),
    ] {
        let segment = segment_for(input).expect("should be relocated, not refused");
        assert_eq!(segment.as_str(), expected);
        assert!(!segment.as_str().starts_with('_'), "must not remain hidden");
        assert_eq!(segment.class(), NameClass::Transformed);
    }
}

#[test]
fn a_hidden_prefix_is_never_produced() {
    // Whole families of external readers skip paths beginning with an underscore — it
    // is exactly how format metadata directories stay invisible to them. A table so
    // named would be unreadable by the engines the layout exists to serve.
    for input in ["_internal", "__private", "_"] {
        match segment_for(input) {
            Ok(segment) => assert!(
                !segment.as_str().starts_with('_'),
                "{input:?} produced {:?}, which external readers would skip",
                segment.as_str()
            ),
            Err(NamingError::Reserved { .. } | NamingError::Empty { .. }) => {}
            Err(e) => panic!("{input:?}: unexpected {e:?}"),
        }
    }
}

#[test]
fn an_identifier_with_nothing_legible_is_refused() {
    for input in ["", "!!!", "///", "   "] {
        let err = segment_for(input).expect_err("must refuse");
        assert!(
            matches!(err, NamingError::Empty { .. }),
            "{input:?} gave {err:?}"
        );
    }
}

#[test]
fn a_collision_is_refused_and_names_both_sides() {
    // The central decision: refuse rather than disambiguate. A generated suffix would
    // produce a unique name that is no longer relatable, defeating the whole scheme.
    let mut check = CollisionCheck::new();
    let first = TableLocation::resolve("public", "Order Items").expect("resolves");
    let second = TableLocation::resolve("public", "order.items").expect("resolves");
    assert_eq!(
        first.relative_path(),
        second.relative_path(),
        "these should collide"
    );

    check.insert(&first).expect("first insert succeeds");
    let err = check
        .insert(&second)
        .expect_err("the collision must be refused");

    let NamingError::Collision {
        identifier,
        existing,
        segment,
    } = err
    else {
        panic!("expected a collision, got {err:?}");
    };
    // An operator needs to know exactly what conflicts, not merely that something did.
    assert!(identifier.contains("order.items"), "{identifier}");
    assert!(existing.contains("Order Items"), "{existing}");
    assert!(segment.contains("order-items"), "{segment}");
}

#[test]
fn case_only_differences_collide_and_are_refused() {
    // Legal as distinct tables in the source, fatal on a case-insensitive filesystem.
    // Caught at onboarding rather than at the first write.
    let mut check = CollisionCheck::new();
    check
        .insert(&TableLocation::resolve("public", "Orders").expect("resolves"))
        .expect("first");
    let err = check
        .insert(&TableLocation::resolve("public", "orders").expect("resolves"))
        .expect_err("must refuse");
    assert!(matches!(err, NamingError::Collision { .. }));
}

#[test]
fn re_registering_the_same_table_is_not_a_collision() {
    let mut check = CollisionCheck::new();
    let location = TableLocation::resolve("public", "orders").expect("resolves");
    check.insert(&location).expect("first");
    check
        .insert(&location)
        .expect("the same table is not a conflict");
    assert_eq!(check.len(), 1);
}

#[test]
fn the_collision_message_explains_the_refusal() {
    let mut check = CollisionCheck::new();
    check
        .insert(&TableLocation::resolve("s", "A B").expect("resolves"))
        .expect("first");
    let err = check
        .insert(&TableLocation::resolve("s", "a.b").expect("resolves"))
        .expect_err("must refuse");
    let message = err.to_string();
    assert!(
        message.contains("will not disambiguate"),
        "the message must explain why it refuses rather than fixing it: {message}"
    );
}

proptest! {
    /// Mapping never panics, whatever an identifier contains.
    #[test]
    fn mapping_never_panics(s in ".{0,200}") {
        let _ = segment_for(&s);
    }

    /// Any produced segment is a safe path component.
    #[test]
    fn produced_segments_are_always_safe(s in ".{0,120}") {
        if let Ok(segment) = segment_for(&s) {
            let text = segment.as_str();
            prop_assert!(!text.is_empty());
            prop_assert!(text.len() <= 63, "segment {text:?} is too long");
            prop_assert!(!text.starts_with('_'), "{text:?} would be hidden from readers");
            prop_assert!(!text.starts_with('.'), "{text:?} would be hidden from readers");
            prop_assert!(!text.ends_with('-'), "{text:?} has a trailing separator");
            prop_assert!(
                text.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-'),
                "{text:?} contains an unsafe character"
            );
        }
    }

    /// Mapping is deterministic: the same identifier always yields the same segment.
    #[test]
    fn mapping_is_deterministic(s in ".{0,80}") {
        let a = segment_for(&s).map(|p| p.as_str().to_string());
        let b = segment_for(&s).map(|p| p.as_str().to_string());
        prop_assert_eq!(a, b);
    }

    /// An identity mapping is byte-identical to its identifier, always.
    #[test]
    fn identity_means_identical(s in "[a-z][a-z0-9_]{0,40}") {
        if let Ok(segment) = segment_for(&s) {
            if segment.is_identity() {
                prop_assert_eq!(segment.as_str(), s.as_str());
            }
        }
    }
}

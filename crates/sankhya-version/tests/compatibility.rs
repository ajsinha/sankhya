//! What a reader does with an artefact it did not write.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_version::{format, Axis, Compatibility, Format, Rollback, FORMATS};

fn at(current: u32, oldest_readable: u32) -> Format {
    Format {
        name: "test artefact",
        path: "<data-dir>/test",
        current,
        oldest_readable,
        rollback: Rollback::Safe,
    }
}

// --- the distinction the crate exists for -------------------------------

#[test]
fn an_artefact_from_the_future_is_refused_by_name() {
    // The whole point. An artefact written by a newer release otherwise fails somewhere in
    // the middle of parsing — "invalid type: string, expected u64 at line 14" — which an
    // operator reads as corruption and acts on as corruption, when the answer is "upgrade".
    let Compatibility::Refused { why } = at(2, 1).admits(3) else {
        panic!("format 3 must be refused by a build that writes 2");
    };
    assert!(why.contains("format 3"), "it names what it found: {why}");
    assert!(why.contains("up to 2"), "and what it understands: {why}");
    assert!(
        why.contains("newer release"),
        "and says which direction the problem is in: {why}"
    );
    assert!(
        why.contains("Upgrade"),
        "and what to do, because that is the thing being got wrong: {why}"
    );
}

#[test]
fn an_artefact_too_old_to_read_says_so_rather_than_guessing() {
    let Compatibility::Refused { why } = at(4, 3).admits(1) else {
        panic!("format 1 is below the floor");
    };
    assert!(why.contains("below 3"), "{why}");
    assert!(
        why.contains("will not guess"),
        "reading an unsupported old format by assuming it resembles a newer one is how a \
         migration silently produces wrong data: {why}"
    );
}

#[test]
fn an_older_but_supported_artefact_is_read_and_not_written_back() {
    // FR-OPS-12: report read-only degradation rather than silently misreading. Rewriting it
    // would change its format without anybody asking, and the next reader would find a file
    // whose version does not match what the rest of the system believes it holds.
    let Compatibility::ReadOnly { why } = at(3, 1).admits(2) else {
        panic!("format 2 is readable by a build that writes 3");
    };
    assert!(why.contains("format 2"));
    assert!(why.contains("not written back"), "{why}");
}

#[test]
fn the_current_format_is_fully_usable() {
    assert_eq!(at(2, 1).admits(2), Compatibility::Full);
}

#[test]
fn readable_and_writable_are_separate_questions() {
    // A build that can read something it must not write is the normal case during an
    // upgrade, and collapsing the two into one boolean loses exactly that state.
    let older = at(3, 1).admits(2);
    assert!(older.readable());
    assert!(!older.writable());

    let future = at(3, 1).admits(9);
    assert!(!future.readable());
    assert!(!future.writable());
}

// --- the declared table -------------------------------------------------

#[test]
fn every_format_this_build_writes_is_declared() {
    // A format with no declaration has no stated rollback consequence, which means upgrading
    // past it is a decision nobody was offered.
    for name in [
        "backup manifest",
        "restore-drill evidence",
        "diagnostic history",
    ] {
        assert!(format(name).is_some(), "{name} is written and not declared");
    }
}

#[test]
fn every_declared_format_is_coherent() {
    for declared in FORMATS.iter().copied() {
        assert!(declared.current >= 1, "{}", declared.name);
        assert!(
            declared.oldest_readable <= declared.current,
            "{} cannot read anything it writes",
            declared.name
        );
        assert!(
            declared.path.contains('/') || declared.path.contains('<'),
            "{} does not say where it lives",
            declared.name
        );
    }
}

#[test]
fn a_one_way_format_is_visibly_one_way() {
    // Recorded on the format rather than discovered during an incident: an upgrade that
    // cannot be reversed is a different decision from one that can, and the difference has
    // to be visible before it is made.
    let one_way = Format {
        rollback: Rollback::OneWay {
            because: "the previous release refuses the new column",
        },
        ..at(2, 1)
    };
    assert!(!one_way.reversible());
    assert!(at(2, 1).reversible());
    for declared in FORMATS.iter().copied() {
        assert!(
            declared.reversible(),
            "{} is one-way and nothing in the release notes says so",
            declared.name
        );
    }
}

#[test]
fn the_four_axes_are_named_for_a_person() {
    // FR-OPS-11 requires them managed independently. A single product version covering all
    // four means a wire-protocol change reads as a storage change — and worse, a genuine
    // storage break hides inside a release that looked like a wire change.
    let axes = [
        Axis::InternalSchema,
        Axis::Database,
        Axis::TableProtocol,
        Axis::WireApi,
    ];
    let mut named: Vec<&str> = axes.iter().map(|axis| axis.as_str()).collect();
    assert!(named.iter().all(|name| name.len() > 4));
    named.sort_unstable();
    let before = named.len();
    named.dedup();
    assert_eq!(named.len(), before, "two axes share a name");
}

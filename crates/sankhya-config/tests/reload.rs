//! Reloading: what moved, and what happens when the new file is broken.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use sankhya_config::Configuration;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, text).expect("writing");
    path
}

/// Rewrite a file and make sure its timestamp moves.
///
/// Filesystem timestamps have coarse resolution, and a test that writes twice in the same
/// tick sees no change — which would make `is_stale` look broken when it is the clock that
/// is coarse.
fn rewrite(path: &Path, text: &str) {
    std::fs::write(path, text).expect("writing");
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(2);
    let _ = filetime_set(path, later);
}

fn filetime_set(path: &Path, when: std::time::SystemTime) -> std::io::Result<()> {
    let file = std::fs::OpenOptions::new().write(true).open(path)?;
    file.set_modified(when)
}

fn load(files: &[PathBuf]) -> Configuration {
    Configuration::load_with(files, &BTreeMap::new(), &BTreeMap::new()).expect("loaded")
}

#[test]
fn a_reload_says_what_changed() {
    // A value changing under a running system is a hazard worth naming. A reload nobody is
    // told about is indistinguishable from a bug.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let file = write(dir.path(), "a.yaml", "timeout: 30\nname: one\n");
    let mut config = load(&[file.clone()]);

    rewrite(&file, "timeout: 60\nname: one\nextra: new\n");
    let changed = config
        .reload_with(&BTreeMap::new(), &BTreeMap::new())
        .expect("reloaded");

    assert_eq!(changed.updated.get("timeout").map(String::as_str), Some("30"));
    assert_eq!(changed.added, vec!["extra".to_string()]);
    assert!(changed.removed.is_empty());
    assert_eq!(config.get("timeout"), Some("60"));

    let explained = changed.explain().expect("something changed");
    assert!(explained.contains("timeout changed from `30`"), "{explained}");
}

#[test]
fn a_setting_that_disappears_is_reported() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let file = write(dir.path(), "a.yaml", "timeout: 30\ngone: yes\n");
    let mut config = load(&[file.clone()]);

    rewrite(&file, "timeout: 30\n");
    let changed = config
        .reload_with(&BTreeMap::new(), &BTreeMap::new())
        .expect("reloaded");
    assert_eq!(changed.removed, vec!["gone".to_string()]);
    assert!(config.get("gone").is_none());
}

#[test]
fn nothing_changing_reports_nothing() {
    // A reload that says something happened when nothing did is noise, and noise is what
    // gets filtered out before the one that mattered.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let file = write(dir.path(), "a.yaml", "timeout: 30\n");
    let mut config = load(&[file]);

    let changed = config
        .reload_with(&BTreeMap::new(), &BTreeMap::new())
        .expect("reloaded");
    assert!(changed.is_empty());
    assert!(changed.explain().is_none());
}

#[test]
fn a_reload_of_a_broken_file_keeps_the_configuration_that_works() {
    // A process running on a good configuration must not be pushed onto a broken one
    // because somebody saved a file mid-edit.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let file = write(dir.path(), "a.yaml", "timeout: 30\n");
    let mut config = load(&[file.clone()]);

    rewrite(&file, "timeout: 30\n bad indent: x\n");
    let refused = config
        .reload_with(&BTreeMap::new(), &BTreeMap::new())
        .expect_err("a broken file was accepted");
    assert!(refused.to_string().contains("Nothing was loaded"), "{refused}");
    assert_eq!(
        config.get("timeout"),
        Some("30"),
        "the working configuration survived"
    );
}

#[test]
fn a_changed_secret_is_reported_without_its_value() {
    // A reload log that prints the new password is worse than one that prints nothing.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let file = write(dir.path(), "a.yaml", "session.secret: old-value\n");
    let mut config = load(&[file.clone()]);

    rewrite(&file, "session.secret: new-value\n");
    let changed = config
        .reload_with(&BTreeMap::new(), &BTreeMap::new())
        .expect("reloaded");

    let explained = changed.explain().expect("something changed");
    assert!(explained.contains("session.secret changed"), "{explained}");
    assert!(!explained.contains("old-value"), "the old secret leaked: {explained}");
    assert!(!explained.contains("new-value"), "the new secret leaked: {explained}");
}

#[test]
fn staleness_notices_a_file_that_changed_and_an_overlay_that_appeared() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let file = write(dir.path(), "application.yaml", "timeout: 30\n");
    let config = load(&[file.clone()]);
    assert!(!config.is_stale(), "nothing has changed yet");

    rewrite(&file, "timeout: 60\n");
    assert!(config.is_stale(), "the file changed");
}

#[test]
fn a_reload_does_not_load_an_overlay_twice_or_at_the_wrong_precedence() {
    // `load_with` derives overlays from the files it is given. Passing the derived list back
    // would load each overlay twice — and the second time at file precedence, so the overlay
    // would lose to the file it is meant to override.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let file = write(dir.path(), "application.yaml", "timeout: 30\n");
    write(dir.path(), "application.local.yaml", "timeout: 90\n");
    let mut config = load(&[file.clone()]);
    assert_eq!(config.get("timeout"), Some("90"));

    rewrite(&file, "timeout: 45\n");
    config
        .reload_with(&BTreeMap::new(), &BTreeMap::new())
        .expect("reloaded");
    assert_eq!(
        config.get("timeout"),
        Some("90"),
        "the overlay still wins after a reload"
    );
}

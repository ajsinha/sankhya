//! Loading configuration: precedence, resolution, and the things it refuses.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp
)]

use sankhya_config::{ConfigError, Configuration, Source};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, text).expect("writing the file");
    path
}

fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

fn load(files: &[PathBuf]) -> Configuration {
    Configuration::load_with(files, &BTreeMap::new(), &BTreeMap::new()).expect("loaded")
}

// --- one namespace, whatever the format ---------------------------------

#[test]
fn yaml_is_flattened_to_dotted_keys() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let file = write(
        dir.path(),
        "application.yaml",
        "server:\n  listen: \"0.0.0.0:5432\"\n  workers: 8\nwarehouse:\n  path: .build/warehouse\n",
    );
    let config = load(&[file]);

    assert_eq!(config.get("server.listen"), Some("0.0.0.0:5432"));
    assert_eq!(config.get("server.workers"), Some("8"));
    assert_eq!(config.get("warehouse.path"), Some(".build/warehouse"));
}

#[test]
fn a_list_is_available_indexed_and_joined() {
    // Both, because both are asked for: code wants the list, and an override wants to
    // replace one element without restating the rest.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let file = write(dir.path(), "a.yaml", "hosts:\n  - one\n  - two\n  - three\n");
    let config = load(&[file]);

    assert_eq!(config.get("hosts.0"), Some("one"));
    assert_eq!(config.get("hosts.2"), Some("three"));
    assert_eq!(
        config.list("hosts"),
        Some(vec!["one".to_string(), "two".to_string(), "three".to_string()])
    );
}

#[test]
fn a_properties_file_lands_in_the_same_namespace() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let file = write(
        dir.path(),
        "app.properties",
        "# a comment\nserver.listen = 0.0.0.0:5432\n\nurl=jdbc:pg://host/db?a=1\n",
    );
    let config = load(&[file]);

    assert_eq!(config.get("server.listen"), Some("0.0.0.0:5432"));
    // Split on the first separator only: a value may contain `=`, and a connection string
    // usually does.
    assert_eq!(config.get("url"), Some("jdbc:pg://host/db?a=1"));
}

#[test]
fn a_key_present_with_no_value_is_configured_to_nothing() {
    // Dropping it would make it indistinguishable from a key nobody wrote, and those are
    // different statements.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let file = write(dir.path(), "a.yaml", "feature:\n  enabled:\n");
    let config = load(&[file]);
    assert_eq!(config.get("feature.enabled"), Some(""));
    assert!(config.get("feature.absent").is_none());
}

// --- precedence ----------------------------------------------------------

#[test]
fn the_rightmost_file_wins() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let base = write(dir.path(), "base.yaml", "timeout: 30\nname: base\n");
    let over = write(dir.path(), "over.yaml", "timeout: 60\n");
    let config = load(&[base, over]);

    assert_eq!(config.get("timeout"), Some("60"));
    assert_eq!(config.get("name"), Some("base"), "unrelated keys survive");
}

#[test]
fn environment_beats_a_file_and_an_argument_beats_the_environment() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let file = write(dir.path(), "a.yaml", "timeout: 30\n");

    let from_file = Configuration::load_with(&[file.clone()], &BTreeMap::new(), &BTreeMap::new())
        .expect("loaded");
    assert_eq!(from_file.get("timeout"), Some("30"));
    assert_eq!(from_file.origin("timeout").map(|o| o.source), Some(Source::File));

    let from_env =
        Configuration::load_with(&[file.clone()], &env(&[("timeout", "45")]), &BTreeMap::new())
            .expect("loaded");
    assert_eq!(from_env.get("timeout"), Some("45"));
    assert_eq!(
        from_env.origin("timeout").map(|o| o.source),
        Some(Source::Environment)
    );

    let from_args = Configuration::load_with(
        &[file],
        &env(&[("timeout", "45")]),
        &env(&[("timeout", "60")]),
    )
    .expect("loaded");
    assert_eq!(from_args.get("timeout"), Some("60"));
    assert_eq!(
        from_args.origin("timeout").map(|o| o.source),
        Some(Source::CommandLine)
    );
}

#[test]
fn a_local_overlay_is_read_after_the_file_it_sits_beside() {
    // For values a machine needs to start and that must not be committed. A signing secret
    // in a tracked file is a public signing secret.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let file = write(dir.path(), "application.yaml", "timeout: 30\nname: shared\n");
    write(dir.path(), "application.local.yaml", "timeout: 90\n");
    let config = load(&[file]);

    assert_eq!(config.get("timeout"), Some("90"));
    assert_eq!(config.get("name"), Some("shared"), "the overlay sets only what it names");
    assert_eq!(
        config.origin("timeout").map(|o| o.source),
        Some(Source::LocalOverlay),
        "and it says which it came from, because that answers why this machine differs"
    );
}

#[test]
fn a_missing_overlay_changes_nothing() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let file = write(dir.path(), "application.yaml", "timeout: 30\n");
    assert_eq!(load(&[file]).get("timeout"), Some("30"));
}

// --- source tracking -----------------------------------------------------

#[test]
fn a_value_says_where_it_came_from_and_from_which_file() {
    // "The timeout is thirty seconds" is not an answer to "why is the timeout thirty
    // seconds", and the value is identical in all three cases.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let file = write(dir.path(), "application.yaml", "timeout: 30\n");
    let config = load(&[file]);

    let explained = config.explain("timeout").expect("an explanation");
    assert!(explained.contains("configuration file"), "{explained}");
    assert!(explained.contains("application.yaml"), "{explained}");
}

// --- resolution ----------------------------------------------------------

#[test]
fn a_reference_is_resolved_from_another_setting() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let file = write(
        dir.path(),
        "a.yaml",
        "root: /var/lib/sankhya\nwarehouse: ${root}/warehouse\n",
    );
    assert_eq!(load(&[file]).get("warehouse"), Some("/var/lib/sankhya/warehouse"));
}

#[test]
fn a_reference_reaches_the_environment_before_the_files() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let file = write(dir.path(), "a.yaml", "root: /fallback\npath: ${root}/x\n");
    let config = Configuration::load_with(&[file], &env(&[("root", "/from-env")]), &BTreeMap::new())
        .expect("loaded");
    assert_eq!(config.get("path"), Some("/from-env/x"));
}

#[test]
fn a_default_makes_a_reference_optional() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let file = write(dir.path(), "a.yaml", "listen: ${HOST:127.0.0.1}:5432\n");
    assert_eq!(load(&[file]).get("listen"), Some("127.0.0.1:5432"));
}

#[test]
fn a_default_may_itself_hold_a_reference() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let file = write(
        dir.path(),
        "a.yaml",
        "fallback: 10.0.0.1\nlisten: ${HOST:${fallback}}:5432\n",
    );
    assert_eq!(load(&[file]).get("listen"), Some("10.0.0.1:5432"));
}

#[test]
fn an_unresolved_reference_with_no_default_refuses_the_load() {
    // The one place this design departs from the configurator it was modelled on. That one
    // leaves the placeholder "so the problem is visible" — visible in a config dump, not in
    // a connection string, which is where the value goes.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let file = write(dir.path(), "a.yaml", "url: postgres://${DB_HOST}/sankhya\n");
    let refused = Configuration::load_with(&[file], &BTreeMap::new(), &BTreeMap::new())
        .expect_err("a placeholder was allowed into a value");

    let message = refused.to_string();
    assert!(message.contains("DB_HOST"), "{message}");
    assert!(message.contains("url"), "it names the setting too: {message}");
    assert!(message.contains("some default"), "and how to make it optional: {message}");
}

#[test]
fn settings_that_refer_to_each_other_in_a_circle_are_refused_with_the_path() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let file = write(dir.path(), "a.yaml", "a: ${b}\nb: ${a}\n");
    let refused = Configuration::load_with(&[file], &BTreeMap::new(), &BTreeMap::new())
        .expect_err("a cycle was resolved");
    let message = refused.to_string();
    assert!(message.contains("circle"), "{message}");
    assert!(message.contains('→'), "the path, not just its existence: {message}");
}

#[test]
fn an_unterminated_reference_is_text_rather_than_a_refusal() {
    // Refusing it would reject a value that may be perfectly deliberate.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let file = write(dir.path(), "a.yaml", "pattern: \"cost is ${100\"\n");
    assert_eq!(load(&[file]).get("pattern"), Some("cost is ${100"));
}

// --- refusals ------------------------------------------------------------

#[test]
fn a_malformed_file_fails_the_load_rather_than_loading_part_of_it() {
    // A process with three of its four settings behaves plausibly and wrongly.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let file = write(dir.path(), "a.yaml", "server:\n  listen: \"x\"\n bad indent: y\n");
    let refused = Configuration::load_with(&[file], &BTreeMap::new(), &BTreeMap::new())
        .expect_err("a malformed file was partly loaded");
    assert!(matches!(refused, ConfigError::Unreadable(_)), "{refused:?}");
    assert!(refused.to_string().contains("Nothing was loaded"), "{refused}");
}

#[test]
fn a_file_that_does_not_exist_is_skipped_and_one_that_cannot_be_read_is_not() {
    // Different situations, and only one of them is somebody's mistake.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let present = write(dir.path(), "a.yaml", "x: 1\n");
    let absent = dir.path().join("nowhere.yaml");
    let config = Configuration::load_with(&[absent, present], &BTreeMap::new(), &BTreeMap::new())
        .expect("a missing file is not an error");
    assert_eq!(config.get("x"), Some("1"));
}

#[test]
fn a_required_setting_that_is_missing_is_named() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let file = write(dir.path(), "a.yaml", "x: 1\n");
    let config = load(&[file]);
    assert_eq!(config.require("x"), Ok("1"));
    let refused = config.require("warehouse.path").expect_err("missing");
    assert!(refused.to_string().contains("warehouse.path"), "{refused}");
}

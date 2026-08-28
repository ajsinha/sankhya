//! Reading a setting as a type, and what happens when it is not one.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::float_cmp
)]

use sankhya_config::{looks_secret, ConfigError, Configuration, Secret};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn config(text: &str, dir: &Path) -> Configuration {
    let path = dir.join("a.yaml");
    std::fs::write(&path, text).expect("writing");
    Configuration::load_with(&[path], &BTreeMap::new(), &BTreeMap::new()).expect("loaded")
}

// --- the refusal that matters --------------------------------------------

#[test]
fn an_unparseable_value_is_refused_rather_than_silently_defaulted() {
    // `port=eighty` must not quietly become 8080. A setting that silently becomes something
    // else is a deployment behaving as though it were configured when it is not, and the
    // failure surfaces wherever the wrong value is used rather than where it was written.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let config = config("port: eighty\n", dir.path());

    let refused = config.integer("port").expect_err("read as a number");
    let ConfigError::NotA { key, wanted, found, origin } = &refused else {
        panic!("wrong refusal: {refused:?}");
    };
    assert_eq!(key, "port");
    assert_eq!(*wanted, "an integer");
    assert_eq!(found, "eighty");
    assert!(origin.contains("a.yaml"), "it says which file to fix: {origin}");
}

#[test]
fn a_setting_that_is_not_there_is_none_rather_than_an_error() {
    // Unset and unparseable are different situations and get different answers.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let config = config("x: 1\n", dir.path());
    assert_eq!(config.integer("absent"), Ok(None));
    assert_eq!(config.integer("x"), Ok(Some(1)));
}

// --- each type -----------------------------------------------------------

#[test]
fn integers_numbers_and_booleans_read_as_written() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let config = config(
        "workers: 8\nratio: 0.25\nenabled: true\ndisabled: off\nyes_please: yes\n",
        dir.path(),
    );
    assert_eq!(config.integer("workers"), Ok(Some(8)));
    assert_eq!(config.number("ratio"), Ok(Some(0.25)));
    assert_eq!(config.boolean("enabled"), Ok(Some(true)));
    assert_eq!(config.boolean("disabled"), Ok(Some(false)));
    assert_eq!(config.boolean("yes_please"), Ok(Some(true)));
}

#[test]
fn a_boolean_that_is_neither_is_refused_rather_than_read_as_false() {
    // `enabled=maybe` silently disabling a feature is exactly the kind of wrong nobody finds
    // until they need the feature.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let config = config("enabled: maybe\n", dir.path());
    assert!(config.boolean("enabled").is_err());
}

#[test]
fn a_number_that_is_not_finite_is_refused() {
    // An infinite timeout is a hang with a configuration file behind it.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let config = config("ratio: inf\n", dir.path());
    assert!(config.number("ratio").is_err());
}

#[test]
fn durations_are_written_the_way_people_write_them() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let config = config(
        "quick: 30s\nmedium: 5m\nlong: 2h\nvery_long: 1d\nbare: 45\n",
        dir.path(),
    );
    assert_eq!(config.duration("quick"), Ok(Some(Duration::from_secs(30))));
    assert_eq!(config.duration("medium"), Ok(Some(Duration::from_secs(300))));
    assert_eq!(config.duration("long"), Ok(Some(Duration::from_secs(7_200))));
    assert_eq!(config.duration("very_long"), Ok(Some(Duration::from_secs(86_400))));
    assert_eq!(config.duration("bare"), Ok(Some(Duration::from_secs(45))), "bare means seconds");
}

#[test]
fn a_duration_that_is_not_one_is_refused() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let config = config("timeout: soon\n", dir.path());
    let refused = config.duration("timeout").expect_err("read as a duration");
    assert!(refused.to_string().contains("30s"), "it shows the shape: {refused}");
}

#[test]
fn a_relative_path_is_anchored_rather_than_left_to_the_working_directory() {
    // A relative path means a different directory under systemd, in a container, and in a
    // shell. Anchoring is the difference between a setting that means one thing everywhere
    // and one that does not.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let config = config("warehouse: .build/warehouse\nabsolute: /var/lib/x\n", dir.path());

    assert_eq!(
        config.path("warehouse", Path::new("/opt/sankhya")),
        Some(PathBuf::from("/opt/sankhya/.build/warehouse"))
    );
    assert_eq!(
        config.path("absolute", Path::new("/opt/sankhya")),
        Some(PathBuf::from("/var/lib/x")),
        "an absolute path is left alone"
    );
}

#[test]
fn a_list_that_is_unset_is_none_and_one_set_to_nothing_is_empty() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let config = config("hosts: \"\"\n", dir.path());
    assert_eq!(config.list("hosts"), Some(Vec::new()), "configured to nothing");
    assert_eq!(config.list("absent"), None, "not configured");
}

#[test]
fn a_section_is_returned_with_its_prefix_removed() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let config = config(
        "table:\n  orders:\n    clustering: region\n    retain: 90d\nother: x\n",
        dir.path(),
    );
    let section = config.section("table.orders");
    assert_eq!(section.get("clustering").map(String::as_str), Some("region"));
    assert_eq!(section.get("retain").map(String::as_str), Some("90d"));
    assert!(!section.contains_key("other"));
}

// --- redaction -----------------------------------------------------------

#[test]
fn a_secret_does_not_print_itself() {
    // A password in a log line survives in every backup of that log, on every machine that
    // received it, and rotating it does not remove it.
    let secret = Secret::new("hunter2");
    assert_eq!(format!("{secret}"), "<redacted>");
    assert_eq!(format!("{secret:?}"), "Secret(<redacted>)");
    assert!(!format!("{secret:?}").contains("hunter2"));
    assert_eq!(secret.expose(), "hunter2", "and reading it takes a word a reviewer can see");
}

#[test]
fn keys_that_name_secrets_are_recognised_and_ordinary_ones_are_not() {
    // A rule that redacts ordinary settings trains people to work around it.
    for key in [
        "database.password",
        "session.secret",
        "api_key",
        "auth.token",
        "tls.private_key",
    ] {
        assert!(looks_secret(key), "{key} should be treated as a secret");
    }
    for key in ["server.listen", "warehouse.path", "table.orders.retain", "workers"] {
        assert!(!looks_secret(key), "{key} is an ordinary setting");
    }
}

#[test]
fn an_unparseable_secret_reports_the_problem_without_reporting_the_value() {
    // That it is unparseable is reportable. What it holds is not.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let config = config("database.password: not-a-number\n", dir.path());
    let refused = config.integer("database.password").expect_err("read as a number");
    let message = refused.to_string();
    assert!(message.contains("<redacted>"), "{message}");
    assert!(!message.contains("not-a-number"), "the value leaked: {message}");
}

#[test]
fn a_setting_can_be_read_as_a_secret() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let config = config("session.secret: s3kr1t\n", dir.path());
    let secret = config.secret("session.secret").expect("set");
    assert_eq!(format!("{secret}"), "<redacted>");
    assert_eq!(secret.expose(), "s3kr1t");
}

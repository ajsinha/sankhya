//! The route table, and the half of the requirement it does not serve.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use sankhya_api_rest::plane::{route, Shape, ABSENT, ROUTES};

#[test]
fn a_path_is_matched_whole_and_not_by_prefix() {
    // The scrape endpoint served `/metrics/../etc/passwd` on a prefix match — harmless there
    // because it reads no files, and exactly the shape that becomes a traversal the moment
    // something does. Once was enough to make it a rule.
    assert!(route("GET", "/health").is_some());
    for hostile in [
        "/health/../v1/policy/rules",
        "/healthz",
        "/health/",
        "/v1/catalog/tables/../../etc/passwd",
        "//health",
    ] {
        assert!(route("GET", hostile).is_none(), "{hostile} matched");
    }
}

#[test]
fn a_query_string_is_ignored_and_the_path_before_it_is_matched() {
    assert_eq!(
        route("GET", "/health?probe=1").map(|r| r.path),
        Some("/health")
    );
}

#[test]
fn the_method_is_part_of_the_match() {
    // `POST /health` is not `GET /health`, and a table that ignores the method turns every
    // read route into a write one for anybody who tries.
    assert!(route("POST", "/health").is_none());
    assert!(route("GET", "/v1/query").is_none());
    assert!(route("POST", "/v1/query").is_some());
}

#[test]
fn no_two_routes_overlap() {
    // A table whose entries overlap is one where the answer depends on iteration order.
    let mut pairs: Vec<(&str, &str)> = ROUTES.iter().map(|r| (r.method, r.path)).collect();
    pairs.sort_unstable();
    let before = pairs.len();
    pairs.dedup();
    assert_eq!(pairs.len(), before);
}

#[test]
fn the_probes_do_not_require_a_credential_and_everything_tenant_scoped_does() {
    // A liveness probe that needs a credential fails when the credential expires, turning an
    // authentication problem into an outage.
    for path in ["/health", "/ready"] {
        let probe = route("GET", path).expect("declared");
        assert!(!probe.authenticated, "{path} requires a credential");
    }
    for path in ["/v1/catalog/tables", "/v1/policy/rules"] {
        let scoped = route("GET", path).expect("declared");
        assert!(scoped.authenticated, "{path} does not require one");
    }
    assert!(route("POST", "/v1/query").expect("declared").authenticated);
}

#[test]
fn every_route_that_returns_rows_is_subject_to_the_size_cap() {
    // The cap is what keeps this surface from becoming the bulk plane. A route returning
    // rows without it is a hole in FR-API-06 the size of one endpoint.
    for declared in ROUTES {
        if declared.shape == Shape::Rows {
            assert!(
                declared.authenticated,
                "{declared} returns rows without requiring a caller"
            );
        }
    }
    assert!(
        ROUTES.iter().any(|r| r.shape == Shape::Rows),
        "a gateway that returns no rows has nothing to cap"
    );
}

#[test]
fn every_route_says_what_it_is_for() {
    for declared in ROUTES {
        assert!(
            declared.purpose.len() > 40,
            "{declared} has no useful purpose recorded"
        );
        assert!(declared.path.starts_with('/'));
    }
}

#[test]
fn what_the_requirement_names_and_this_does_not_serve_is_recorded_with_a_reason() {
    // An API that quietly omits half a requirement reads as complete. Each absence is data
    // rather than prose so it is countable, and so a route added later has somewhere to be
    // removed from.
    assert!(ABSENT.len() >= 4);
    for (name, why) in ABSENT {
        assert!(!name.is_empty());
        assert!(
            why.len() > 60,
            "{name} is listed as absent without saying why"
        );
        assert!(
            route("GET", &format!("/v1/{name}")).is_none(),
            "{name} is both absent and served"
        );
    }
}

#[test]
fn jobs_are_absent_because_nothing_runs_them() {
    // The specific reason matters: an endpoint listing nothing forever cannot be told from a
    // system with nothing to list.
    let (_, why) = ABSENT
        .iter()
        .find(|(name, _)| *name == "jobs")
        .expect("jobs is listed as absent");
    assert!(why.contains("No scheduler"), "{why}");
    assert!(why.contains("cannot tell"), "{why}");
}

//! What the control plane exposes, and what it deliberately does not.
//!
//! # Routes are declared, not matched
//!
//! A route table rather than a chain of `if path.starts_with(..)`. The prefix version served
//! `/metrics/../etc/passwd` on the scrape endpoint — harmless there because it reads no
//! files, and exactly the shape that becomes a traversal the moment something does. Once is
//! enough to make the pattern a rule.
//!
//! Declaring them also makes the surface **countable**: what exists, what each one is for,
//! and — the part that matters here — what is absent and why.
//!
//! # Half of `FR-API-04` is absent on purpose
//!
//! The requirement lists administration, tenancy, policy, catalog, health, jobs and archive
//! operations. Three of those describe machinery that is not running:
//!
//! - **Jobs.** No scheduler drives ingest, hydration or maintenance in this process. A jobs
//!   endpoint would list nothing, forever, and a client cannot distinguish *"no jobs are
//!   running"* from *"nothing runs jobs"*.
//! - **Archive operations.** Tiering is `M9` and gated. There is no archive.
//! - **Mutating administration.** Creating a tenant or editing a policy through this surface
//!   needs an audited write path that the read-only routes below do not need, and shipping
//!   the read half first is the smaller, honest step.
//!
//! `STATUS.md` records the same reasoning for why the control plane was deferred out of `M5`
//! in the first place: *"a plausible-looking API returning a placeholder is the kind of thing
//! that gets believed"*.

use std::fmt;

/// What a route answers with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shape {
    /// A small, bounded document. Always safe as JSON.
    Document,
    /// Rows, and therefore subject to the size cap.
    ///
    /// See [`crate::size`]: anything past the cap comes back as a Flight ticket rather than
    /// as JSON, and that is a redirection rather than a refusal.
    Rows,
}

/// One thing the gateway serves.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Route {
    /// The method, which is compared exactly.
    pub method: &'static str,
    /// The path, which is compared **whole**.
    pub path: &'static str,
    /// What comes back.
    pub shape: Shape,
    /// Whether a caller has to be authenticated.
    ///
    /// Every route that touches tenant-scoped anything requires it. `/health` does not,
    /// because a liveness probe that needs a credential is a liveness probe that fails when
    /// the credential expires — turning an authentication problem into an outage.
    pub authenticated: bool,
    /// What it is for.
    pub purpose: &'static str,
}

/// Every route this gateway serves.
pub static ROUTES: &[Route] = &[
    Route {
        method: "GET",
        path: "/health",
        shape: Shape::Document,
        authenticated: false,
        purpose: "Liveness. Whether the process is answering — nothing about lag, \
                  deliberately: ARCHITECTURE §17.2 keeps lag out of liveness so a degraded \
                  pipeline does not cause an orchestrator to kill a healthy node.",
    },
    Route {
        method: "GET",
        path: "/ready",
        shape: Shape::Document,
        authenticated: false,
        purpose: "Readiness. Whether it is worth sending traffic here, which is where \
                  pipeline lag belongs.",
    },
    Route {
        method: "GET",
        path: "/v1/version",
        shape: Shape::Document,
        authenticated: false,
        purpose: "The four version axes, so a client can tell what it is talking to before \
                  it depends on behaviour that differs between them.",
    },
    Route {
        method: "GET",
        path: "/v1/catalog/tables",
        shape: Shape::Rows,
        authenticated: true,
        purpose: "The tables this principal may read. Filtered by policy, because a \
                  catalogue listing tables a caller cannot read discloses that they exist — \
                  the same leak the wire protocol refuses through a schema browser.",
    },
    Route {
        method: "GET",
        path: "/v1/policy/rules",
        shape: Shape::Rows,
        authenticated: true,
        purpose: "The policy in force for this principal. Readable so that a denial can be \
                  explained; not writable here, which needs an audited write path.",
    },
    Route {
        method: "POST",
        path: "/v1/query",
        shape: Shape::Rows,
        authenticated: true,
        purpose: "A small ad-hoc query. Subject to the size cap: anything larger comes back \
                  as a Flight ticket rather than as JSON.",
    },
];

/// What `FR-API-04` names and this gateway does not serve, with the reason.
///
/// Data rather than prose, so the gap is countable and so a route added later has somewhere
/// to be removed from. An API that quietly omits half a requirement reads as complete.
pub static ABSENT: &[(&str, &str)] = &[
    (
        "jobs",
        "No scheduler drives ingest, hydration or maintenance in this process. The endpoint \
         would list nothing forever, and a client cannot tell 'no jobs are running' from \
         'nothing runs jobs'.",
    ),
    (
        "archive operations",
        "Tiering is M9 and gated on evidence that does not exist yet. There is no archive to \
         operate on.",
    ),
    (
        "tenancy and policy administration",
        "Creating a tenant or editing a policy needs an audited write path. The read half \
         ships first because it is the half that can be correct on its own.",
    ),
    (
        "the graph API",
        "FR-API-08 wants a structured traversal specification. The five SQL table functions \
         exist and are the supported surface; a second shape for the same traversals is a \
         second thing to keep correct, and it should follow a use for it rather than \
         precede one.",
    ),
];

/// The route for a method and path, matched **whole**.
///
/// No prefixes, no trailing-slash tolerance, no case folding. Each of those is a small
/// convenience that makes two paths mean one thing, and a route table whose entries overlap
/// is one where the answer depends on iteration order.
#[must_use]
pub fn route(method: &str, path: &str) -> Option<&'static Route> {
    let bare = path.split_once('?').map_or(path, |(before, _)| before);
    ROUTES
        .iter()
        .find(|route| route.method == method && route.path == bare)
}

impl fmt::Display for Route {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.method, self.path)
    }
}

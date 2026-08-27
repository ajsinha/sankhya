//! Generating the metric and error catalogues into documentation, and checking they agree.
//!
//! # Why generated rather than written
//!
//! `M6`'s sixth exit criterion asks that every user-reachable error have documented
//! remediation **generated from the same source as the catalog**. The clause matters more
//! than it reads: a hand-written table of error codes is correct on the day it is written
//! and wrong by the second release, and nothing tells you which entry went stale.
//!
//! So the documents are produced from the declarations and the check regenerates them and
//! compares. A drift is a failed build with a diff, rather than a support engineer reading a
//! remediation for a code that no longer exists.
//!
//! # The check that is easy to leave out
//!
//! Generating documentation from a catalogue proves the documentation matches the
//! catalogue. It says nothing about whether the catalogue matches reality --- so a metric
//! declared and never recorded is documented, dashboarded, and permanently absent.
//!
//! That is checked separately, by requiring every declared metric to appear somewhere in the
//! source outside the catalogue itself. It is a grep, and a grep is a blunt instrument; it
//! is also the difference between a catalogue and a wishlist.

use sankhya_error::{Class, Classify, Error};
use sankhya_metrics::catalogue::{ALL, NOT_YET_EMITTED};
use sankhya_metrics::metric::{Kind, Values};
use std::fmt::Write as _;
use std::path::Path;

/// Where the generated documents live.
pub const METRICS_DOC: &str = "docs/METRICS.md";
/// Where the generated error catalogue lives.
pub const ERRORS_DOC: &str = "docs/ERRORS.md";
/// Where a runbook for a pageable condition lives.
pub const RUNBOOKS: &str = "docs/runbooks";
/// Where the generated platform matrix lives.
pub const PLATFORMS_DOC: &str = "docs/PLATFORMS.md";

/// The header every generated file carries.
///
/// Blunt on purpose. A generated file that does not say so gets edited by hand, and the edit
/// disappears at the next regeneration without anybody connecting the two events.
fn generated_header(source: &str) -> String {
    format!(
        "<!-- GENERATED FILE — DO NOT EDIT.\n     Produced by `cargo xtask \
         write-catalogues` from {source}.\n     `cargo xtask check-catalogues` fails the \
         build if this file and that source disagree. -->\n\n"
    )
}

/// The metric catalogue, as a document.
#[must_use]
pub fn metrics_markdown() -> String {
    let mut out = generated_header("crates/sankhya-metrics/src/catalogue.rs");
    out.push_str("# SANKHYA — Metric catalogue\n\n");
    out.push_str(
        "Every metric this build exports. A metric absent from this document is not merely \
         undocumented — it cannot be recorded, because recording one requires passing its \
         declaration.\n\n**No label may carry tenant data.** A label is either restricted to \
         a named set of values, in which case anything else is refused, or it holds a \
         deployment-scoped identifier under a cap. There is no third kind, so a label that \
         varies per row has no way to be declared.\n\n",
    );

    for metric in ALL {
        let _ = writeln!(out, "## `{}`\n", metric.name);
        let _ = writeln!(out, "{}\n", metric.help);
        let _ = writeln!(out, "| | |\n|---|---|");
        let _ = writeln!(out, "| Type | {} |", metric.kind.as_str());
        let _ = writeln!(out, "| Unit | {} |", metric.unit.as_str());
        let _ = writeln!(out, "| Group | {} |", metric.group.as_str());
        if let Kind::Histogram { buckets } = metric.kind {
            let bounds: Vec<String> = buckets.iter().map(ToString::to_string).collect();
            let _ = writeln!(out, "| Buckets | {} (`+Inf` implicit) |", bounds.join(", "));
        }
        if metric.labels.is_empty() {
            let _ = writeln!(out, "| Labels | none |");
        }
        for label in metric.labels {
            match label.values {
                Values::Closed(values) => {
                    let quoted: Vec<String> =
                        values.iter().map(|v| format!("`{v}`")).collect();
                    let _ = writeln!(
                        out,
                        "| Label `{}` | one of {} — anything else is refused |",
                        label.name,
                        quoted.join(", ")
                    );
                }
                Values::Identifier { cap } => {
                    let _ = writeln!(
                        out,
                        "| Label `{}` | a deployment-scoped name, at most {cap} distinct \
                         values; past that, new series are refused and counted |",
                        label.name
                    );
                }
            }
        }
        match metric.alert {
            Some(alert) => {
                let _ = writeln!(
                    out,
                    "| **Pages** | yes — [`{}`](runbooks/{}.md) |",
                    alert.runbook, alert.runbook
                );
                let _ = writeln!(out, "| Consequence | {} |", alert.consequence);
                let _ = writeln!(out, "| Lead time | {} |", alert.lead_time);
            }
            None => {
                let _ = writeln!(out, "| Pages | no |");
            }
        }
        out.push('\n');
    }

    out.push_str("---\n\n## Named in the architecture and not emitted\n\n");
    out.push_str(
        "`ARCHITECTURE.md` §17.1 names four metrics that receive paging alerts. One of them \
         — compaction debt, above — is emitted. The other three are listed here rather than \
         declared, because a metric permanently reading zero is indistinguishable from a \
         healthy subsystem.\n\n",
    );
    for (name, why) in NOT_YET_EMITTED {
        let _ = writeln!(out, "**{name}.** {why}\n");
    }
    out
}

/// The platform matrix, as a document.
#[must_use]
pub fn platforms_markdown() -> String {
    use crate::package::{Baseline, Support, SUPPORTED};
    let mut out = generated_header("xtask/src/package.rs");
    out.push_str("# SANKHYA — Platforms\n\n");
    out.push_str("Where the server runs, where it does not, and what a build has to satisfy.\n\n");
    out.push_str("**The number of build targets is the number of things that can silently ");
    out.push_str("break.** Each one is declared here once and the packaging tooling iterates ");
    out.push_str("this table, rather than a script per platform drifting from its siblings ");
    out.push_str("until an artifact behaves unlike the rest for a reason nobody can find.\n\n");
    let _ = writeln!(out, "| Platform | Target | Support | Baseline | Published as |");
    let _ = writeln!(out, "|---|---|---|---|---|");
    for target in SUPPORTED {
        let support = match target.support {
            Support::Server => "**server**",
            Support::ClientOnly => "client only",
        };
        let baseline = match target.baseline {
            Baseline::Glibc(major, minor) => format!("`GLIBC_{major}.{minor}`"),
            Baseline::Musl => "static (musl)".to_string(),
            Baseline::MacOs(major, minor) => format!("macOS {major}.{minor}"),
            Baseline::None => "—".to_string(),
        };
        let formats = if target.formats.is_empty() {
            "—".to_string()
        } else {
            target.formats.join(", ")
        };
        let _ = writeln!(
            out,
            "| {} | `{}` | {support} | {baseline} | {formats} |",
            target.called, target.triple
        );
    }
    out.push('\n');
    for target in SUPPORTED {
        let _ = writeln!(out, "## {}\n", target.called);
        let _ = writeln!(out, "{}\n", target.note);
    }
    out.push_str("---\n\n## How an old baseline is met\n\n");
    out.push_str("A binary built on a current distribution silently acquires that ");
    out.push_str("distribution's symbol versions. The symbols are present locally, so it ");
    out.push_str("links, runs and tests clean, and the failure appears the first time ");
    out.push_str("somebody on an enterprise distribution tries to start it — which is why ");
    out.push_str("`cargo xtask check-package` reads what the binary *requires* rather than ");
    out.push_str("trusting what the build intended.\n\n");
    out.push_str("Three ways to hit an older one, and they are not equivalent:\n\n");
    out.push_str("| | |\n|---|---|\n");
    out.push_str("| **`cargo-zigbuild`** | Targets a chosen `glibc` directly — ");
    out.push_str("`--target x86_64-unknown-linux-gnu.2.28`. No container, no sysroot to ");
    out.push_str("maintain. The simplest answer for the Rust half |\n");
    out.push_str("| **A build container or sysroot** | The only answer for the *bundled ");
    out.push_str("PostgreSQL*, which is a C build and acquires its baseline the same way. A ");
    out.push_str("container is excluded from **running** this system, never from building ");
    out.push_str("it |\n");
    out.push_str("| **musl, statically linked** | Removes the question entirely, and is only ");
    out.push_str("available to the artifact that does not bundle PostgreSQL. A static binary ");
    out.push_str("containing a database is not achievable |\n\n");
    out.push_str("**So the baseline of the self-contained artifact is set by PostgreSQL, not ");
    out.push_str("by the Rust binary.** That is worth stating plainly, because tuning the ");
    out.push_str("Rust build alone and declaring victory is the obvious mistake.\n\n");
    out.push_str("## One artifact or one per distribution\n\n");
    out.push_str("Both, answering different questions. A **tarball built at the oldest ");
    out.push_str("baseline** is one file that runs everywhere newer, which is what an ");
    out.push_str("air-gapped install needs. **Native packages** integrate with the ");
    out.push_str("distribution — the service unit, the user, the upgrade path — at the cost ");
    out.push_str("of a build and a test per distribution. The matrix above is what keeps ");
    out.push_str("that cost visible.\n");
    out
}

/// The error catalogue, as a document.
#[must_use]
pub fn errors_markdown() -> String {
    let mut out = generated_header("crates/sankhya-error/src/lib.rs");
    out.push_str("# SANKHYA — Error catalogue\n\n");
    out.push_str(
        "Every error code this system can produce, with what to do about it.\n\n**Codes are \
         permanent.** Removing or renumbering one breaks every runbook, alert rule and \
         support script that references it, so this catalogue only ever grows.\n\nThe class \
         is the load-bearing part: one classification drives retry policy, protocol status, \
         SQL state, log level, metric labelling and alerting. Without it each call site \
         decides independently, and the decisions drift until an operator cannot tell from a \
         log line whether to wake somebody.\n\n",
    );

    let mut by_class: Vec<(&str, &str, Vec<Error>)> = vec![
        (
            "The caller's request was wrong",
            "Do not retry unchanged, and do not page. The detail names what to fix.",
            Vec::new(),
        ),
        (
            "A limit was reached",
            "Shed load and apply backpressure. Retrying immediately makes it worse.",
            Vec::new(),
        ),
        (
            "A concurrent writer won",
            "Re-plan against the new state and retry. A blind retry loses again.",
            Vec::new(),
        ),
        (
            "Transient",
            "Retry after the hint. Persistent failure means the underlying resource is \
             genuinely unavailable.",
            Vec::new(),
        ),
        (
            "Abandoned deliberately",
            "No action. A deadline expired, a client disconnected, or the server is draining.",
            Vec::new(),
        ),
        (
            "An invariant does not hold",
            "**These page.** Each has a runbook. Fail fast is deliberate: continuing past a \
             broken invariant turns a detectable fault into a silent wrong answer.",
            Vec::new(),
        ),
    ];
    for error in Error::all() {
        let index = match error.class() {
            Class::User => 0,
            Class::Resource => 1,
            Class::Conflict => 2,
            Class::Retryable { .. } => 3,
            Class::Cancelled => 4,
            Class::Fatal => 5,
        };
        if let Some(bucket) = by_class.get_mut(index) {
            bucket.2.push(error);
        }
    }

    for (heading, note, errors) in &by_class {
        if errors.is_empty() {
            continue;
        }
        let _ = writeln!(out, "## {heading}\n");
        let _ = writeln!(out, "{note}\n");
        for error in errors {
            let _ = writeln!(out, "### `{}`\n", error.code());
            // The message is the `Display` text with the code and detail stripped, which is
            // what a client sees before the detail is appended.
            let _ = writeln!(out, "{}\n", message_of(error));
            if let Class::Retryable { after: Some(delay) } = error.class() {
                let _ = writeln!(out, "*Retry after {} ms.*\n", delay.as_millis());
            }
            if error.class().is_pageable() {
                let _ = writeln!(
                    out,
                    "**Pages.** Runbook: [`{}`](runbooks/{}.md)\n",
                    runbook_of(error),
                    runbook_of(error)
                );
            }
            let _ = writeln!(out, "{}\n", error.remediation());
        }
    }
    out
}

/// The human-readable half of an error's `Display`, without the code or detail.
fn message_of(error: &Error) -> String {
    let rendered = error.to_string();
    rendered
        .split_once("] ")
        .map_or(rendered.clone(), |(_, rest)| rest.to_string())
}

/// The runbook stem for a pageable error: its code, lowercased.
///
/// Derived rather than declared, so a new fatal error cannot be added without the check
/// immediately demanding its runbook.
#[must_use]
pub fn runbook_of(error: &Error) -> String {
    error.code().as_str().to_lowercase()
}

/// Write both documents.
///
/// # Errors
///
/// When either file cannot be written.
pub fn write(root: &Path) -> std::io::Result<()> {
    std::fs::write(root.join(METRICS_DOC), metrics_markdown())?;
    std::fs::write(root.join(PLATFORMS_DOC), platforms_markdown())?;
    std::fs::write(root.join(ERRORS_DOC), errors_markdown())
}

/// Everything the catalogues must satisfy.
#[must_use]
pub fn check(root: &Path) -> bool {
    println!("== check-catalogues ==");
    let mut ok = true;

    ok &= up_to_date(root, METRICS_DOC, &metrics_markdown());
    ok &= up_to_date(root, ERRORS_DOC, &errors_markdown());
    ok &= up_to_date(root, PLATFORMS_DOC, &platforms_markdown());
    ok &= every_metric_is_recorded(root);
    ok &= every_pageable_thing_has_a_runbook(root);

    if ok {
        println!(
            "   {} metric(s), {} error code(s) and {} platform(s) documented, recorded \
             and runbooked",
            ALL.len(),
            Error::all().len(),
            crate::package::SUPPORTED.len()
        );
    }
    ok
}

/// The file on disk is what the catalogue would generate.
fn up_to_date(root: &Path, relative: &str, expected: &str) -> bool {
    match std::fs::read_to_string(root.join(relative)) {
        Ok(found) if found == expected => true,
        Ok(_) => {
            eprintln!(
                "  STALE  {relative} disagrees with its source — run `cargo xtask \
                 write-catalogues`"
            );
            false
        }
        Err(_) => {
            eprintln!("  MISSING  {relative} — run `cargo xtask write-catalogues`");
            false
        }
    }
}

/// Every declared metric is recorded somewhere outside the catalogue.
///
/// The identifier is derived from the metric's name by the convention the catalogue keeps,
/// and `sankhya-metrics` has a test asserting the derivation holds --- so this check cannot
/// quietly start passing because a name stopped matching its constant.
fn every_metric_is_recorded(root: &Path) -> bool {
    let mut sources = String::new();
    collect_sources(&root.join("crates"), &mut sources);

    let mut ok = true;
    for metric in ALL {
        let ident = ident_for(metric.name);
        if !sources.contains(&ident) {
            eprintln!(
                "  NEVER RECORDED  {} is declared and nothing emits it; a metric \
                 permanently absent looks like a healthy subsystem",
                metric.name
            );
            ok = false;
        }
    }
    ok
}

/// `sankhya_queries_total` becomes `QUERIES_TOTAL`.
#[must_use]
pub fn ident_for(name: &str) -> String {
    name.strip_prefix("sankhya_")
        .unwrap_or(name)
        .to_uppercase()
}

/// Read every source file outside the metrics crate into one buffer.
fn collect_sources(dir: &Path, out: &mut String) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            // The catalogue declares every metric, so counting it as a recording site would
            // make the check pass unconditionally.
            if path.file_name().is_some_and(|n| n == "sankhya-metrics") {
                continue;
            }
            collect_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            if let Ok(text) = std::fs::read_to_string(&path) {
                out.push_str(&text);
            }
        }
    }
}

/// Every alert that can page has a runbook, and every runbook says enough to act on.
fn every_pageable_thing_has_a_runbook(root: &Path) -> bool {
    let mut ok = true;
    let mut wanted: Vec<String> = Vec::new();

    for metric in ALL {
        if let Some(alert) = metric.alert {
            wanted.push(alert.runbook.to_string());
        }
    }
    for error in Error::all() {
        if error.class().is_pageable() {
            wanted.push(runbook_of(&error));
        }
    }

    for stem in &wanted {
        let path = root.join(RUNBOOKS).join(format!("{stem}.md"));
        let Ok(text) = std::fs::read_to_string(&path) else {
            eprintln!(
                "  NO RUNBOOK  {stem} can page and has no docs/runbooks/{stem}.md — M6 exit \
                 criterion 5 requires one for every alert that can page"
            );
            ok = false;
            continue;
        };
        // A runbook that exists and says nothing satisfies the file check and fails the
        // person reading it at 03:00, which is the only moment it is ever opened.
        for required in ["## Symptom", "## What is actually wrong", "## What to do"] {
            if !text.contains(required) {
                eprintln!("  THIN RUNBOOK  docs/runbooks/{stem}.md has no `{required}` section");
                ok = false;
            }
        }
    }
    ok
}

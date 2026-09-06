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
/// Where the generated version and rollback table lives.
pub const VERSIONS_DOC: &str = "docs/VERSIONS.md";

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
    // Not per-target, and generated rather than written beside the table.
    //
    // This section existed as a hand edit to `PLATFORMS.md` --- a file whose own header says
    // DO NOT EDIT --- so `check-catalogues` had been failing, and the next `write-catalogues`
    // would have deleted an owner decision without anybody noticing which. The lesson is the
    // one the generator exists for: prose that has to survive belongs in the source.
    out.push_str("## What the warehouse requires of a filesystem\n\n");
    out.push_str("**Hard links.** A commit claims its version with `link(2)`, which fails ");
    out.push_str("when the name is taken --- that refusal is the whole of the protocol's ");
    out.push_str("concurrency control, and `rename` cannot provide it because it replaces ");
    out.push_str("its destination silently. ext4, xfs, btrfs, zfs, APFS and NTFS all support ");
    out.push_str("hard links. **FAT and exFAT do not, and are not supported.** Owner ");
    out.push_str("decision, 2026-08-29.\n\n");
    out.push_str("Some network filesystems implement `link` unreliably. The failure there is ");
    out.push_str("at least loud: `link` returns an error and the commit reports it, rather ");
    out.push_str("than a lost update that nobody is told about. A warehouse on such a mount ");
    out.push_str("will refuse to commit rather than silently lose one.\n\n");
    out.push_str("Object stores are a separate story with the same requirement: the ");
    out.push_str("equivalent primitive is a conditional put --- `If-None-Match: *` on S3 and ");
    out.push_str("Azure, `ifGenerationMatch=0` on GCS --- and a store that does not offer ");
    out.push_str("one cannot host a warehouse safely. See ");
    out.push_str("[ADR-0013](adr/0013-concurrency-and-data-safety.md).\n\n");
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

/// The on-disk formats and what rolling back does to them.
#[must_use]
pub fn versions_markdown() -> String {
    use sankhya_version::{Rollback, FORMATS};
    let mut out = generated_header("crates/sankhya-version/src/lib.rs");
    out.push_str("# SANKHYA — Versions and rollback\n\n");
    out.push_str("Four things version independently, and every artefact this system writes ");
    out.push_str("says which format it is in.\n\n");
    out.push_str("## The four axes\n\n");
    out.push_str("`FR-OPS-11` requires them managed independently, and *independently* is ");
    out.push_str("the load-bearing word. One product version covering all four means every ");
    out.push_str("change to any of them is a change to all of them — so an upgrade that ");
    out.push_str("only touches the wire protocol reads as a storage-format change and gets ");
    out.push_str("the caution one deserves, and, worse, the reverse: a genuine storage ");
    out.push_str("break hides inside a release that looked like a wire change.\n\n");
    out.push_str("| Axis | What moves it |\n|---|---|\n");
    out.push_str("| internal schema | This system's own on-disk artefacts — the table below |\n");
    out.push_str("| database major version | PostgreSQL. Moving it needs `pg_upgrade` and both binaries present |\n");
    out.push_str("| table format protocol | The Delta reader and writer versions. `FR-OPS-12`: a table whose protocol this build does not fully support is **read-only**, never written |\n");
    out.push_str("| wire API | The PostgreSQL wire protocol and Flight SQL |\n\n");

    out.push_str("## On-disk formats\n\n");
    out.push_str("| Format | Where | Writes | Reads from | Rollback |\n|---|---|---|---|---|\n");
    for declared in FORMATS.iter().copied() {
        let rollback = match declared.rollback {
            Rollback::Safe => "**safe** — the previous release reads it unchanged".to_string(),
            Rollback::Tolerated { ignoring } => format!("tolerated — {ignoring}"),
            Rollback::OneWay { because } => format!("**ONE-WAY** — {because}"),
        };
        let _ = writeln!(
            out,
            "| {} | `{}` | {} | {} | {rollback} |",
            declared.name, declared.path, declared.current, declared.oldest_readable
        );
    }
    out.push('\n');

    out.push_str("## What a reader does with an artefact it did not write\n\n");
    out.push_str("| It found | What happens |\n|---|---|\n");
    out.push_str("| A newer format | **Refused, by name.** Not attempted |\n");
    out.push_str("| An older but supported format | Read, and **not written back** |\n");
    out.push_str("| Older than the floor | Refused. Migrate it with a release that still understood it |\n");
    out.push_str("| The current format | Read and written |\n\n");
    out.push_str("**A parse error and \"this is from the future\" are different facts, and ");
    out.push_str("only one of them says what to do.** An artefact from a newer release read ");
    out.push_str("by an older one otherwise fails somewhere in the middle of parsing — an ");
    out.push_str("unknown field, a number that will not fit — and the error reads as ");
    out.push_str("*corruption*. An operator goes looking for a damaged disk. The answer was ");
    out.push_str("\"upgrade the binary\", and nothing in front of them said so.\n\n");
    out.push_str("So the version sits first in every file, is read before anything else is ");
    out.push_str("understood, and a refusal names both versions.\n\n");

    out.push_str("## Rolling back\n\n");
    out.push_str("**Backwards is the direction that decides whether you can roll back.** A ");
    out.push_str("new release reading old data is the easy direction and the one everybody ");
    out.push_str("tests. Whether the *old* release can read what the new one wrote is the ");
    out.push_str("question, and the moment to answer it is not after the upgrade.\n\n");
    out.push_str("The procedure, when every format above says **safe**:\n\n");
    out.push_str("1. **Stop the new binary.** It drains in-flight connections; see the ");
    out.push_str("termination grace in `packaging/`.\n");
    out.push_str("2. **Prove the backup first.** `sankhya-server drill`. A rollback with an ");
    out.push_str("unproven backup is two unknowns at once.\n");
    out.push_str("3. **Start the previous binary against the same data directory.** No ");
    out.push_str("migration step, because none of these formats moved.\n");
    out.push_str("4. **Run `sankhya-server doctor`.** It reads every artefact and reports ");
    out.push_str("what it could not — which is how a format problem surfaces as a sentence ");
    out.push_str("rather than as a failed query later.\n\n");
    out.push_str("When any format says **ONE-WAY**, steps 3 and 4 do not apply and the only ");
    out.push_str("route back is a restore. That is why the column exists: the decision has ");
    out.push_str("to be visible *before* the upgrade, not discovered during the rollback.\n\n");
    out.push_str("## What is not tested\n\n");
    out.push_str("**Running the previous binary.** One release exists, so there is no ");
    out.push_str("earlier one to run. What is tested is the thing that does not need it: a ");
    out.push_str("corpus of artefacts as earlier releases wrote them, checked into the ");
    out.push_str("repository and read by every build. A fixture is an old binary's ");
    out.push_str("behaviour preserved — and unlike the binary it never stops building and is ");
    out.push_str("legible in a diff. The fixtures are hand-written rather than generated, ");
    out.push_str("because a generated fixture regenerates when the format changes, agrees ");
    out.push_str("with the current code by construction, and proves nothing.\n");
    out
}

/// The error catalogue, as a document.
#[must_use]
pub fn errors_markdown() -> String {
    let mut out = generated_header("crates/sankhya-error/src/lib.rs");
    out.push_str("# SANKHYA — Error catalogue\n\n");
    out.push_str(
        "Every error code this build defines, with what to do about it. Twelve of them are marked **not produced by this build**, and that marking is checked: `cargo xtask check-catalogues` fails when a code nothing constructs is not declared, and fails again when a declared one starts being produced and the note is left behind. An alert rule written from an undeclared code will fire; one written from a declared code will not, and now says so.\n\n**Codes are permanent.** Removing or renumbering one breaks every runbook, alert rule and support script that references it, so this catalogue only ever grows.\n\nThe class is the load-bearing part: one classification drives retry policy, protocol status, SQL state, log level, metric labelling and alerting. Without it each call site decides independently, and the decisions drift until an operator cannot tell from a log line whether to wake somebody.\n\n",
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
            if let Some((_, why)) =
                UNREACHABLE.iter().find(|(code, _)| *code == error.code().as_str())
            {
                let _ = writeln!(
                    out,
                    "**Not produced by this build.** {why}. The code is kept because codes are permanent: removing one would break every runbook and alert rule that references it. An alert on it will not fire until the gap named above is closed.\n"
                );
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
    std::fs::write(root.join(VERSIONS_DOC), versions_markdown())?;
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
    ok &= up_to_date(root, VERSIONS_DOC, &versions_markdown());
    ok &= every_metric_is_recorded(root);
    ok &= every_code_is_reachable_or_declared(root);
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
/// Codes this build cannot yet produce, with why and what will change it.
///
/// # Why a list rather than deleting them
///
/// `OPS-25`. Twelve of twenty-one documented codes were constructed by nothing, and the
/// catalogue said each one was something "this system can produce". Four of the six that
/// page were among them, so an alert rule written from the document was permanently silent
/// --- and a silent alert is indistinguishable from a healthy system right up until it is
/// not.
///
/// Deleting them is wrong for the reason the catalogue's own preamble gives: codes are
/// permanent, because removing one breaks every runbook and alert rule that references it.
/// What was wrong was the *claim*, so the claim is what changed --- the generated document
/// marks these as not produced by this build, and this list is what it reads.
///
/// Every entry names the subsystem, because that is the thing that has to exist before the
/// code can fire. When one is built, the check below fails until its entry is removed.
const UNREACHABLE: &[(&str, &str)] = &[
    // Nine whose subsystem does not exist. Nothing can produce these until it does.
    ("SNK-C0003", "nothing carries an exactness requirement for a statement to breach"),
    ("SNK-C0004", "there is no archived tier to target: `sankhya-tiering` plans and does not run"),
    ("SNK-C0005", "the source identifiers that could collide arrive on the ingest path, and `ING-00` records that there is no change-capture runtime"),
    ("SNK-R0002", "tenant quotas are `sankhya-governor`, which is called with a zeroed request against `u64::MAX` ceilings and decides nothing"),
    ("SNK-R0003", "the arrival buffer is part of the change-capture runtime (`ING-00`)"),
    ("SNK-T0002", "a source that could be unavailable is the change-capture runtime (`ING-00`)"),
    ("SNK-T0003", "staleness against a freshness objective needs the replication a change-capture runtime would provide (`ING-00`)"),
    ("SNK-S0002", "an archive to conflict with is `sankhya-tiering`, which does not run"),
    ("SNK-S0004", "an endangered source is the change-capture runtime (`ING-00`)"),
    // And three whose condition happens today and is reported as a different type.
    // These are the worse half: the subsystem exists, the failure occurs, and an
    // operator alerting on the documented code sees nothing. Mapping them is what is
    // left of `OPS-25`, and `docs/REMEDIATION.md` records it rather than leaving it
    // to be rediscovered.
    ("SNK-F0001", "commit conflicts do occur, and `sankhya-publish` reports them as its own `CommitError`, which nothing maps onto this code"),
    ("SNK-X0001", "cancellation does occur, as `sankhya_governor::Stopped` and as a statement timeout, and nothing maps either onto this code"),
    ("SNK-S0003", "backup verification does run, and reports through `sankhya-backup`’s own types rather than raising this code"),
];

/// `Error::InvalidQuery { detail: None }` becomes `InvalidQuery`.
fn variant_of(error: &Error) -> String {
    let rendered = format!("{error:?}");
    rendered
        .split_once(' ')
        .map_or(rendered.clone(), |(name, _)| name.to_string())
}

/// Every documented code is either produced somewhere or declared unreachable.
fn every_code_is_reachable_or_declared(root: &Path) -> bool {
    let mut sources = String::new();
    collect_error_sites(&root.join("crates"), &mut sources);

    let declared: std::collections::BTreeSet<&str> =
        UNREACHABLE.iter().map(|(code, _)| *code).collect();
    let mut ok = true;

    for error in Error::all() {
        let code = error.code().as_str();
        // The constructor, not the bare variant name: a doc comment or a match arm naming
        // the variant is not a site that can produce it.
        let produced = sources.contains(&format!("Error::{}(", variant_of(&error)))
            || sources.contains(&format!("Error::{} {{", variant_of(&error)));
        match (produced, declared.contains(code)) {
            (false, false) => {
                eprintln!(
                    "  NEVER PRODUCED  {code} is documented as producible and nothing constructs it. Emit it, or add it to UNREACHABLE with the reason: an alert rule written from this catalogue is otherwise permanently silent"
                );
                ok = false;
            }
            // The same stale-permission guard `check-unsafety` and `check-mutation-coverage`
            // carry: an excuse that outlives its reason is a lie the build keeps telling.
            (true, true) => {
                eprintln!(
                    "  STALE EXCUSE    {code} is listed as unreachable and is now produced. Remove it from UNREACHABLE so the catalogue stops saying otherwise"
                );
                ok = false;
            }
            _ => {}
        }
    }
    ok
}

/// Read every source file outside the error crate into one buffer.
///
/// `sankhya-error` is excluded for the reason `sankhya-metrics` is: it declares every
/// variant, so counting it as a construction site would make the check pass unconditionally.
fn collect_error_sites(dir: &Path, out: &mut String) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "sankhya-error") {
                continue;
            }
            collect_error_sites(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            if let Ok(text) = std::fs::read_to_string(&path) {
                out.push_str(&text);
            }
        }
    }
}

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

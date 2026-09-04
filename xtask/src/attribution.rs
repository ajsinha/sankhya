//! Third-party attribution, generated from the dependency graph rather than maintained.
//!
//! # Why this exists
//!
//! Every licence in this graph --- MIT, Apache-2.0, BSD, ISC, Unicode-3.0 --- requires that
//! the copyright notice be reproduced with a **binary** distribution, and Apache-2.0 §4(d)
//! additionally requires propagating upstream `NOTICE` files. `arrow`, `parquet`,
//! `datafusion` and `object_store` all ship one.
//!
//! There was no such file. That is not a *may we use it* problem --- the graph is
//! permissively licensed and compatible --- it is a *may we ship it* problem, and it was the
//! only thing in the dependency graph that blocked shipping at all.
//!
//! # Why it is generated and gated rather than written
//!
//! A hand-written attribution file is correct on the day it is written. This one is derived
//! from `cargo metadata`, so adding a dependency and forgetting the notice is a build
//! failure rather than a licence breach discovered by somebody else.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Where the generated file lives.
pub const FILE: &str = "THIRD-PARTY-NOTICES.md";

/// One third-party package, as it will be attributed.
struct Package {
    name: String,
    version: String,
    license: String,
    repository: String,
    /// The upstream `NOTICE` file, where the package ships one.
    notice: Option<String>,
}

/// Read the resolved dependency graph.
///
/// `--all-features`, because a licence obligation does not depend on which features this
/// build happened to enable.
fn packages(root: &Path) -> Result<Vec<Package>, String> {
    let output = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["metadata", "--format-version", "1", "--all-features"])
        .current_dir(root)
        .output()
        .map_err(|why| format!("running cargo metadata: {why}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|why| format!("parsing metadata: {why}"))?;

    // Workspace members are ours, and a project does not attribute itself.
    let ours: Vec<&str> = value
        .get("workspace_members")
        .and_then(|m| m.as_array())
        .map(|a| a.iter().filter_map(serde_json::Value::as_str).collect())
        .unwrap_or_default();

    let mut out = Vec::new();
    for package in value
        .get("packages")
        .and_then(|p| p.as_array())
        .unwrap_or(&Vec::new())
    {
        let id = package.get("id").and_then(|i| i.as_str()).unwrap_or_default();
        if ours.contains(&id) {
            continue;
        }
        let name = package.get("name").and_then(|n| n.as_str()).unwrap_or_default();
        let manifest = package
            .get("manifest_path")
            .and_then(|m| m.as_str())
            .map(PathBuf::from);
        out.push(Package {
            name: name.to_string(),
            version: package
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            // A package with no declared licence is reported rather than assumed. Assuming
            // permissive is how a copyleft dependency ships unnoticed.
            license: package
                .get("license")
                .and_then(|l| l.as_str())
                .unwrap_or("UNDECLARED")
                .to_string(),
            repository: package
                .get("repository")
                .and_then(|r| r.as_str())
                .unwrap_or_default()
                .to_string(),
            notice: manifest.as_deref().and_then(|m| m.parent()).and_then(upstream_notice),
        });
    }
    out.sort_by(|a, b| (a.name.to_lowercase(), a.version.clone()).cmp(&(b.name.to_lowercase(), b.version.clone())));
    Ok(out)
}

/// The `NOTICE` a package ships beside its manifest, if it ships one.
///
/// Apache-2.0 §4(d) requires these to travel with the distribution; nothing else does.
fn upstream_notice(directory: &Path) -> Option<String> {
    for name in ["NOTICE", "NOTICE.txt", "NOTICE.md"] {
        if let Ok(text) = std::fs::read_to_string(directory.join(name)) {
            let text = text.trim();
            if !text.is_empty() {
                return Some(text.to_string());
            }
        }
    }
    None
}

/// Render the attribution document.
fn render(packages: &[Package]) -> String {
    let mut licenses: BTreeMap<&str, usize> = BTreeMap::new();
    for package in packages {
        *licenses.entry(package.license.as_str()).or_default() += 1;
    }

    let mut out = String::new();
    out.push_str("# Third-party notices\n\n");
    out.push_str(
        "SANKHYA is built on the crates listed here. Every one is reproduced with its licence\n\
         because MIT, Apache-2.0, BSD, ISC and Unicode-3.0 each require the copyright notice to\n\
         travel with a binary distribution, and Apache-2.0 §4(d) requires upstream `NOTICE`\n\
         files to travel with it too.\n\n",
    );
    out.push_str(
        "**This file is generated.** Run `cargo run -p xtask -- write-attribution` after\n\
         changing a dependency; `cargo run -p xtask -- check-attribution` fails the build when it\n\
         is stale, so an added dependency cannot ship unattributed.\n\n",
    );
    out.push_str(&format!("## Summary\n\n{} third-party packages.\n\n", packages.len()));
    out.push_str("| Licence | Packages |\n|---|---|\n");
    let mut by_count: Vec<_> = licenses.iter().collect();
    by_count.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    for (license, count) in by_count {
        out.push_str(&format!("| `{license}` | {count} |\n"));
    }

    out.push_str("\n## Packages\n\n| Package | Version | Licence | Source |\n|---|---|---|---|\n");
    for package in packages {
        let source = if package.repository.is_empty() {
            format!("<https://crates.io/crates/{}>", package.name)
        } else {
            format!("<{}>", package.repository)
        };
        out.push_str(&format!(
            "| `{}` | {} | `{}` | {source} |\n",
            package.name, package.version, package.license
        ));
    }

    let with_notices: Vec<&Package> = packages.iter().filter(|p| p.notice.is_some()).collect();
    out.push_str(&format!(
        "\n## Upstream NOTICE files\n\nApache-2.0 §4(d). {} package(s) ship one, reproduced in full.\n",
        with_notices.len()
    ));
    for package in with_notices {
        out.push_str(&format!("\n### {} {}\n\n```\n", package.name, package.version));
        for line in package.notice.as_deref().unwrap_or_default().lines() {
            out.push_str(line);
            out.push('\n');
        }
        out.push_str("```\n");
    }
    out
}

/// Write the attribution file.
///
/// # Errors
///
/// Returns an error if the dependency graph cannot be read or the file cannot be written.
pub fn write(root: &Path) -> Result<usize, String> {
    let packages = packages(root)?;
    let rendered = render(&packages);
    std::fs::write(root.join(FILE), &rendered).map_err(|why| format!("writing {FILE}: {why}"))?;
    Ok(packages.len())
}

/// The attribution file exists and matches the dependency graph.
#[must_use]
pub fn check(root: &Path) -> bool {
    println!("== check-attribution ==");
    let packages = match packages(root) {
        Ok(packages) => packages,
        Err(why) => {
            eprintln!("  COULD NOT READ  the dependency graph: {why}");
            return false;
        }
    };
    let expected = render(&packages);
    let path = root.join(FILE);
    let Ok(actual) = std::fs::read_to_string(&path) else {
        eprintln!(
            "  MISSING  {FILE} does not exist. Every licence in this graph requires the \
             copyright notice to be reproduced with a binary distribution — shipping without \
             it breaches all {} of them at once. `cargo run -p xtask -- write-attribution`",
            packages.len()
        );
        return false;
    };
    if actual != expected {
        eprintln!(
            "  STALE  {FILE} does not match the dependency graph. A dependency was added, \
             removed or moved and the notices did not follow. \
             `cargo run -p xtask -- write-attribution`"
        );
        return false;
    }
    let notices = packages.iter().filter(|p| p.notice.is_some()).count();
    let undeclared = packages.iter().filter(|p| p.license == "UNDECLARED").count();
    if undeclared > 0 {
        eprintln!(
            "  UNDECLARED LICENCE  {undeclared} package(s) declare no licence. Reported rather \
             than assumed permissive: assuming is how a copyleft dependency ships unnoticed"
        );
        return false;
    }
    println!(
        "   {} third-party package(s) attributed, {notices} upstream NOTICE file(s) reproduced",
        packages.len()
    );
    true
}

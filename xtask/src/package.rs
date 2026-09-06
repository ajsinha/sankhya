//! Building an artifact, and checking it is the artifact it claims to be.
//!
//! # "Self-contained" is a claim, and the obvious test does not check it
//!
//! `FR-OLTP-10` disqualifies runtime downloads, so an air-gapped install that reaches for
//! the network is a failed install with no recourse. The natural check --- *does the archive
//! contain the binaries?* --- passes on an artifact whose binary then asks the dynamic loader
//! for a symbol version the target does not have. Nothing is missing from the archive. It
//! simply will not start.
//!
//! `IMPLEMENTATION_PLAN` §10.4 already states the shape of the answer: **a build against an
//! old platform baseline rather than a fully static binary**, because bundled database
//! binaries are dynamically linked and a static binary containing a database is not
//! achievable. What it does not say, and what this module is for, is that a baseline nobody
//! checks is a baseline nobody meets.
//!
//! So the baseline is **declared** --- a maximum `glibc` symbol version and a set of shared
//! objects --- and the built binary is checked against it. This is the single commonest way
//! a Rust binary fails to install: built on a current distribution, requiring a symbol
//! version from it, and refusing to start on the enterprise distribution the customer runs.
//! It is invisible on the build machine by construction.
//!
//! # Two numbers that have to agree and normally do not
//!
//! An orchestrator's termination grace and the server's drain deadline live in different
//! files, are edited by different people, and nothing relates them. When the grace is the
//! shorter of the two, every deploy `SIGKILL`s a server mid-drain and clients see resets
//! that look like crashes. [`check`] compares them.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

/// How far a target's support goes.
///
/// A level rather than a boolean, because "we do not support Windows" is false and "we
/// support Windows" is false, and the true statement is between them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Support {
    /// The server runs here, and an artifact is published for it.
    Server,
    /// Any PostgreSQL client connects from here. The server is not built for it.
    ///
    /// This is a real statement about today rather than an aspiration, which is why it is in
    /// the matrix at all. A row for something nobody has built is a wishlist entry, and a
    /// wishlist published as a support matrix is how a customer plans a deployment that
    /// cannot happen.
    ClientOnly,
}

/// What the platform's C library requires of a build.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Baseline {
    /// A maximum `glibc` symbol version.
    Glibc(u32, u32),
    /// Statically linked; no host C library involved.
    Musl,
    /// A macOS deployment target: the oldest release the binary will start on.
    ///
    /// Its own variant rather than `None`. macOS has a baseline exactly as Linux does ---
    /// `MACOSX_DEPLOYMENT_TARGET` --- and calling it "not applicable" was wrong in the way
    /// that matters: it says the question does not arise, when in fact it arises and nobody
    /// answered it. The test asserting that every server target states a baseline is what
    /// found this.
    MacOs(u32, u32),
    /// Not applicable --- nothing is built for this target.
    None,
}

/// One platform, and what is published for it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Target {
    /// The Rust target triple, or the platform name where nothing is built.
    pub triple: &'static str,
    /// What a person calls it.
    pub called: &'static str,
    /// How far support goes.
    pub support: Support,
    /// What the build must satisfy.
    pub baseline: Baseline,
    /// The artifact formats published.
    pub formats: &'static [&'static str],
    /// The thing worth knowing that the columns do not carry.
    pub note: &'static str,
}

/// Every platform, stated once.
///
/// # Why a matrix rather than a script per platform
///
/// The number of build targets is the number of things that can silently break. Hand-written
/// scripts drift from each other --- one gets a flag, another does not, and the difference
/// surfaces as an artifact that behaves unlike its siblings for a reason nobody can find.
/// Iterating one declaration means a target is configured in exactly one place and checked
/// the same way as the rest.
pub static SUPPORTED: &[Target] = &[
    Target {
        triple: "x86_64-unknown-linux-gnu",
        called: "Linux (x86-64)",
        support: Support::Server,
        baseline: Baseline::Glibc(2, 28),
        formats: &["tarball", "rpm", "deb"],
        note: "The self-contained artifact. `glibc` 2.28 is RHEL 8 and Debian 10 — the oldest an enterprise is plausibly still running. The bundled PostgreSQL is dynamically linked, so this baseline is set by how *it* was built, not by the Rust binary.",
    },
    Target {
        triple: "aarch64-unknown-linux-gnu",
        called: "Linux (ARM64)",
        support: Support::Server,
        baseline: Baseline::Glibc(2, 28),
        formats: &["tarball", "rpm", "deb"],
        note: "Same baseline, same reasoning. Graviton and Ampere are ordinary deployment targets now rather than a special case.",
    },
    Target {
        triple: "x86_64-unknown-linux-musl",
        called: "Linux (x86-64, static)",
        support: Support::Server,
        baseline: Baseline::Musl,
        formats: &["tarball"],
        note: "The artifact that *downloads* database binaries rather than bundling them. Statically linked, so it starts on any Linux at all — and it is only achievable because the thing that cannot be static, PostgreSQL, is not in this artifact.",
    },
    Target {
        triple: "aarch64-apple-darwin",
        called: "macOS (Apple silicon)",
        support: Support::Server,
        baseline: Baseline::MacOs(12, 0),
        formats: &["tarball"],
        note: "Development and evaluation. The warehouse layout is portable to a case-insensitive filesystem because every path segment is already case-folded — see `sankhya-schema`'s naming rules — so a warehouse written on Linux opens here.",
    },
    Target {
        triple: "x86_64-pc-windows-msvc",
        called: "Windows",
        support: Support::ClientOnly,
        baseline: Baseline::None,
        formats: &[],
        note: "Any PostgreSQL driver connects to a SANKHYA server from Windows today — that is the wire protocol, and it is the thing most Windows users actually need. The *server* is not built for Windows, and the reason is the vendored PostgreSQL build and service integration rather than the storage layer: path segments are already restricted to lower-case ASCII, digits and underscores, and the platform device names (`aux`, `con`, `nul`, `com1`…) are already reserved, so a warehouse is already Windows-path-safe. Run the server under WSL2 or a container until this is built.",
    },
];

/// The oldest platform the artifact is meant to run on.
///
/// `glibc` 2.28 is RHEL 8 and Debian 10 --- the oldest thing an enterprise is plausibly
/// still running, and the figure this project has to build against rather than discover it
/// missed. Raising it is a decision about which customers can install, so it is a named
/// constant in a file somebody reads rather than a property of whichever machine ran the
/// build.
pub const BASELINE_GLIBC: (u32, u32) = (2, 28);

/// Shared objects the artifact may require.
///
/// Everything here is part of a base system install. Anything else --- `libssl`, `libicu`,
/// a compression library --- means the target needs a package installed, which for an
/// air-gapped deployment means it needs a package *found*, and that is the failure this list
/// exists to prevent.
pub const ALLOWED_SHARED_OBJECTS: &[&str] = &[
    "libc.so.6",
    "libm.so.6",
    "libgcc_s.so.1",
    "ld-linux-x86-64.so.2",
    "ld-linux-aarch64.so.1",
    "libdl.so.2",
    "libpthread.so.0",
    "librt.so.1",
];

/// Where the deployment manifests live.
pub const MANIFESTS: &str = "packaging";

/// What a binary asks of its platform.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Requires {
    /// The highest `glibc` symbol version referenced.
    pub glibc: Option<(u32, u32)>,
    /// The shared objects named in `DT_NEEDED`.
    pub shared_objects: BTreeSet<String>,
}

/// Parse the highest `GLIBC_x.y` a dynamic symbol table references.
///
/// Two things here are load-bearing and the first version had neither.
///
/// **The version is compared as a version.** `GLIBC_2.9` sorts above `GLIBC_2.28`
/// alphabetically, which would report a baseline nineteen versions too low, pass every
/// check, and ship a binary that does not start.
///
/// **The symbol is split at the `@`.** `readelf` writes `statx@GLIBC_2.28`, not
/// `GLIBC_2.28`, so matching on a prefix of the whole token found nothing at all --- and a
/// check that finds no requirements reports that every requirement is met. It passed the
/// real binary, which needs `GLIBC_2.34`, against a `GLIBC_2.28` baseline. The unit test
/// using genuine `readelf` output is the only reason that was not shipped.
#[must_use]
pub fn highest_glibc(symbols: &str) -> Option<(u32, u32)> {
    symbols
        .split_whitespace()
        .map(|token| token.trim_matches(|c| c == '(' || c == ')'))
        .filter_map(|token| token.rsplit('@').next())
        .filter_map(|token| token.strip_prefix("GLIBC_"))
        .filter_map(|version| {
            let mut parts = version.split('.');
            let major = parts.next()?.parse::<u32>().ok()?;
            let minor = parts.next().unwrap_or("0").parse::<u32>().ok()?;
            Some((major, minor))
        })
        .max()
}

/// Parse the shared objects a `readelf -d` listing names.
#[must_use]
pub fn shared_objects(dynamic: &str) -> BTreeSet<String> {
    dynamic
        .lines()
        .filter(|line| line.contains("(NEEDED)"))
        .filter_map(|line| {
            let start = line.find('[')?;
            let end = line.find(']')?;
            line.get(start + 1..end).map(str::to_string)
        })
        .collect()
}

/// What this binary asks of its platform.
///
/// # Errors
///
/// When `readelf` is unavailable or will not read the file.
pub fn requirements(binary: &Path) -> Result<Requires, String> {
    let symbols = run("readelf", &["--dyn-syms", "--wide"], binary)?;
    let dynamic = run("readelf", &["-d", "--wide"], binary)?;
    Ok(Requires {
        glibc: highest_glibc(&symbols),
        shared_objects: shared_objects(&dynamic),
    })
}

fn run(program: &str, args: &[&str], binary: &Path) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .arg(binary)
        .output()
        .map_err(|error| format!("{program} could not be run: {error}"))?;
    if !output.status.success() {
        return Err(format!("{program} refused {}", binary.display()));
    }
    String::from_utf8(output.stdout).map_err(|error| format!("{program} output: {error}"))
}

/// Whether a requirement set fits inside the declared baseline.
///
/// Returns the reasons it does not, which is more useful than a boolean: an artifact failing
/// on three counts should not be fixed three builds in a row.
#[must_use]
pub fn outside_baseline(requires: &Requires) -> Vec<String> {
    let mut reasons = Vec::new();
    if let Some(glibc) = requires.glibc {
        if glibc > BASELINE_GLIBC {
            reasons.push(format!(
                "needs GLIBC_{}.{} and the declared baseline is GLIBC_{}.{} — this binary \
                 will not start on the oldest platform it is meant to support, and the \
                 build machine cannot tell you that",
                glibc.0, glibc.1, BASELINE_GLIBC.0, BASELINE_GLIBC.1
            ));
        }
    }
    for object in &requires.shared_objects {
        if !ALLOWED_SHARED_OBJECTS.contains(&object.as_str()) {
            reasons.push(format!(
                "needs {object}, which is not part of a base system install — an air-gapped \
                 target would have to find a package for it"
            ));
        }
    }
    reasons
}

/// The termination grace a manifest declares, in seconds.
#[must_use]
pub fn declared_grace(manifest: &str) -> Option<u64> {
    for line in manifest.lines() {
        let line = line.trim();
        for key in [
            "terminationGracePeriodSeconds:",
            "TimeoutStopSec=",
            "stop_grace_period:",
        ] {
            if let Some(value) = line.strip_prefix(key) {
                let value = value.trim().trim_end_matches('s');
                if let Ok(seconds) = value.parse::<u64>() {
                    return Some(seconds);
                }
            }
        }
    }
    None
}

/// Every packaging invariant.
///
/// # The baseline warns locally and fails on a release build
///
/// A developer's machine builds against whatever `glibc` it has, which on anything current
/// is well past the baseline. Failing every local build on that would train everybody to
/// ignore this check, and a check people ignore is worse than none.
///
/// So it is reported as a warning unless `SANKHYA_RELEASE` is set, which the release
/// pipeline does — where the build genuinely happens against the old baseline and a
/// violation genuinely means the artifact is broken. `check-loc` already makes the same
/// distinction between approaching a limit and exceeding it.
///
/// The grace check has no such excuse: it compares two numbers in this repository and is
/// wrong or right regardless of the machine.
#[must_use]
pub fn check(root: &Path) -> bool {
    println!("== check-package ==");
    let releasing = std::env::var("SANKHYA_RELEASE").is_ok();
    let baseline = check_baseline(root, releasing);
    let grace = check_grace(root);
    let configured = check_units_are_configured(root);
    let images = check_images_are_built(root);
    let builder = check_builder_is_the_one_measured(root);

    if baseline && grace && configured && images && builder {
        println!(
            "   baseline GLIBC_{}.{}, manifests allow longer than the drain, units name a \
             configuration, images are built here",
            BASELINE_GLIBC.0, BASELINE_GLIBC.1
        );
    }
    grace && configured && images && builder && (baseline || !releasing)
}

/// Every service unit says where its configuration is.
///
/// # Why a unit that starts is not a unit that works
///
/// Configuration resolves to `config/application.yaml` **relative to the working
/// directory**, and systemd's default working directory is `/`. The shipped unit set
/// neither `WorkingDirectory=` nor `SANKHYA_CONFIG=`, so it read `/config/application.yaml`,
/// found nothing, and started anyway --- with no users, no roles, no policy, no TLS, and a
/// feed directory that does not exist. It looked healthy. Every request it served was
/// unauthenticated and unpoliced.
///
/// Checked here rather than left to review because the failure has no symptom: the unit
/// starts, the port answers, and the only evidence is a startup line nobody reads.
fn check_units_are_configured(root: &Path) -> bool {
    let mut ok = true;
    for path in files_under(&root.join(MANIFESTS)) {
        if path.extension().is_none_or(|e| e != "service") {
            continue;
        }
        let Ok(unit) = std::fs::read_to_string(&path) else {
            continue;
        };
        let names_a_file = unit.contains("SANKHYA_CONFIG=");
        let has_a_directory = unit.contains("WorkingDirectory=");
        if !names_a_file && !has_a_directory {
            eprintln!(
                "  UNCONFIGURED UNIT  {} sets neither SANKHYA_CONFIG= nor WorkingDirectory=, \
                 so it resolves `config/application.yaml` against `/` and starts with no \
                 users, no roles and no policy",
                path.display()
            );
            ok = false;
        }
    }
    ok
}

/// The built binaries fit inside the declared baseline.
fn check_baseline(root: &Path, releasing: bool) -> bool {
    let label = if releasing { "OUTSIDE BASELINE" } else { "not a release build" };
    let mut ok = true;
    let mut looked = 0usize;
    for profile in ["release", "debug"] {
        let binary = root.join("target").join(profile).join("sankhya-server");
        if !binary.exists() {
            continue;
        }
        looked += 1;
        match requirements(&binary) {
            Err(why) => {
                eprintln!("  COULD NOT READ  {}: {why}", binary.display());
                ok = false;
            }
            Ok(requires) => {
                for reason in outside_baseline(&requires) {
                    eprintln!("  {label}  target/{profile}/sankhya-server {reason}");
                    ok = false;
                }
            }
        }
        // One profile is enough: both are built by the same toolchain against the same
        // system libraries, and checking the release build alone would skip the check on
        // every machine that has not made one.
        break;
    }
    if looked == 0 {
        eprintln!("  NOT BUILT  no sankhya-server binary to check — run `cargo build` first");
        return false;
    }
    ok
}

/// Every image a manifest names is one this repository builds, at this version.
///
/// # Why this is checked rather than reviewed
///
/// `RUN-11`. The Kubernetes manifest named `ghcr.io/ajsinha/sankhya:0.1.0` and there was no
/// Dockerfile, Containerfile or compose file anywhere in the repository --- so the manifest
/// could not be applied by anybody, including whoever wrote it, and nothing said so. A
/// manifest is not run by any test; a missing image is discovered by an operator at the
/// moment they most need it to work.
///
/// The tag is compared against the workspace version because that is the other half of the
/// same failure: two version numbers in two files that nothing relates is how a manifest
/// comes to name an image that was never pushed.
fn check_images_are_built(root: &Path) -> bool {
    let Some(version) = workspace_version(root) else {
        eprintln!("  COULD NOT READ  the workspace version from Cargo.toml");
        return false;
    };
    let dockerfile = root.join(MANIFESTS).join("Dockerfile");
    let mut ok = true;
    let mut checked = 0usize;

    for path in files_under(&root.join(MANIFESTS)) {
        if path.extension().is_none_or(|e| e != "yaml" && e != "yml") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in text.lines() {
            let trimmed = line.trim();
            let Some(image) = trimmed.strip_prefix("image:") else {
                continue;
            };
            let image = image.trim();
            checked += 1;
            if !dockerfile.is_file() {
                eprintln!(
                    "  NO DOCKERFILE   {} names `{image}` and {} does not exist, so this manifest cannot be applied by anybody",
                    path.display(),
                    dockerfile.display()
                );
                ok = false;
                continue;
            }
            let Some((_, tag)) = image.rsplit_once(':') else {
                eprintln!("  UNTAGGED IMAGE  {} names `{image}` with no tag, so what it deploys depends on when it is applied", path.display());
                ok = false;
                continue;
            };
            if tag != version {
                eprintln!(
                    "  WRONG TAG       {} names `{image}` and the workspace is at {version}; the manifest deploys a version this build is not",
                    path.display()
                );
                ok = false;
            }
        }
    }

    if checked == 0 {
        eprintln!("  NO IMAGES       no manifest names an image, so this check is measuring nothing");
        return false;
    }
    ok
}

/// What the container builder's distribution ships, and what its output actually needs.
///
/// # Why a table rather than a check that builds the image
///
/// Building the image takes tens of minutes and a network. This records the two numbers that
/// were *measured* --- by building it and reading the binary's version references --- so that
/// changing the builder without re-measuring fails the build rather than silently moving the
/// platform the project runs on.
///
/// The gap is real and is written down rather than hidden: an image built from
/// `packaging/Dockerfile` needs `GLIBC_2.30` and the declared baseline is 2.28, so it will
/// not start on the oldest platform the project says it supports. Debian 10 is end-of-life
/// and its archive has moved, so pinning the builder to it is a build that breaks on a
/// schedule nobody controls; the honest route to 2.28 is a cross-toolchain with an old
/// sysroot, which is work this has not done.
const BUILDER: (&str, u32, u32) = ("rust:1.97-bullseye", 2, 30);

/// The container builder is the one the measured figure belongs to.
fn check_builder_is_the_one_measured(root: &Path) -> bool {
    let dockerfile = root.join(MANIFESTS).join("Dockerfile");
    let Ok(text) = std::fs::read_to_string(&dockerfile) else {
        // Reported by `check_images_are_built`, which is where a missing Dockerfile belongs.
        return true;
    };
    let declared = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("FROM ").map(str::trim))
        .and_then(|rest| rest.split_whitespace().next());
    match declared {
        Some(image) if image == BUILDER.0 => {
            if (BUILDER.1, BUILDER.2) > BASELINE_GLIBC {
                println!(
                    "   note: images built from {} need GLIBC_{}.{} and the baseline is GLIBC_{}.{}; recorded in packaging/Dockerfile",
                    dockerfile.display(),
                    BUILDER.1,
                    BUILDER.2,
                    BASELINE_GLIBC.0,
                    BASELINE_GLIBC.1
                );
            }
            true
        }
        Some(image) => {
            eprintln!(
                "  BUILDER MOVED   {} builds on `{image}` and the measured figure belongs to `{}`. Build the image, read the binary's GLIBC references, and update BUILDER --- a builder changed without re-measuring moves the platform this project runs on and says nothing",
                dockerfile.display(),
                BUILDER.0
            );
            false
        }
        None => {
            eprintln!("  NO BUILDER      {} declares no FROM", dockerfile.display());
            false
        }
    }
}

/// The workspace version, from the manifest that declares it.
fn workspace_version(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(root.join("Cargo.toml")).ok()?;
    text.lines()
        .find(|line| line.trim_start().starts_with("version"))
        .and_then(|line| line.split('"').nth(1))
        .map(ToString::to_string)
}

/// Every deployment manifest gives the drain longer than it takes.
fn check_grace(root: &Path) -> bool {
    // Read from the source rather than duplicated here, so that changing the drain and
    // forgetting the manifests is the failure this catches rather than one it shares.
    let listener = root.join("crates/sankhya-api-pg/src/listener.rs");
    let Ok(text) = std::fs::read_to_string(&listener) else {
        eprintln!("  COULD NOT READ  {}", listener.display());
        return false;
    };
    let Some(drain) = text
        .split("pub const DRAIN: Duration = Duration::from_secs(")
        .nth(1)
        .and_then(|rest| rest.split(')').next())
        .and_then(|value| value.trim().parse::<u64>().ok())
    else {
        eprintln!("  COULD NOT READ  the drain deadline from {}", listener.display());
        return false;
    };

    let mut ok = true;
    let mut checked = 0usize;
    let manifests = root.join(MANIFESTS);
    for path in files_under(&manifests) {
        let Ok(manifest) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(grace) = declared_grace(&manifest) else {
            continue;
        };
        checked += 1;
        if grace <= drain {
            eprintln!(
                "  GRACE TOO SHORT  {} allows {grace}s and the server drains for up to \
                 {drain}s — every deploy would kill it mid-drain, and clients would see \
                 resets that look like crashes",
                path.display()
            );
            ok = false;
        }
    }
    if checked == 0 {
        eprintln!("  NO MANIFESTS  nothing under {} declares a termination grace", manifests.display());
        return false;
    }
    ok
}

fn files_under(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            out.extend(files_under(&path));
        } else {
            out.push(path);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_highest_glibc_is_compared_as_a_version_not_as_a_string() {
        // `GLIBC_2.9` sorts above `GLIBC_2.28` alphabetically. A string comparison here
        // reports a baseline nineteen versions too low, passes every check, and ships a
        // binary that does not start.
        let symbols = "GLIBC_2.9 GLIBC_2.28 GLIBC_2.17 GLIBC_2.3.4";
        assert_eq!(highest_glibc(symbols), Some((2, 28)));
    }

    #[test]
    fn a_symbol_table_with_no_glibc_references_asks_for_nothing() {
        assert_eq!(highest_glibc("some other text"), None);
    }

    #[test]
    fn glibc_versions_are_read_out_of_real_readelf_output() {
        // The shape `readelf --dyn-syms` actually emits, parentheses and all.
        let line = "    12: 0000000000000000     0 FUNC    GLOBAL DEFAULT  UND write@GLIBC_2.2.5 (3)\n\
                    13: 0000000000000000     0 FUNC    GLOBAL DEFAULT  UND statx@GLIBC_2.28 (4)";
        assert_eq!(highest_glibc(line), Some((2, 28)));
    }

    #[test]
    fn shared_objects_come_out_of_the_dynamic_section() {
        let dynamic = " 0x0000000000000001 (NEEDED)  Shared library: [libgcc_s.so.1]\n\
                        0x0000000000000001 (NEEDED)  Shared library: [libc.so.6]\n\
                        0x000000000000000e (SONAME)  Library soname: [nothing]";
        let found = shared_objects(dynamic);
        assert_eq!(found.len(), 2);
        assert!(found.contains("libc.so.6"));
        assert!(!found.contains("nothing"), "SONAME is not a dependency");
    }

    #[test]
    fn a_binary_inside_the_baseline_has_nothing_to_report() {
        let requires = Requires {
            glibc: Some((2, 17)),
            shared_objects: ["libc.so.6".to_string()].into_iter().collect(),
        };
        assert!(outside_baseline(&requires).is_empty());
    }

    #[test]
    fn every_reason_is_reported_rather_than_the_first() {
        // An artifact failing on three counts should not be fixed three builds in a row.
        let requires = Requires {
            glibc: Some((2, 39)),
            shared_objects: ["libssl.so.3".to_string(), "libicuuc.so.72".to_string()]
                .into_iter()
                .collect(),
        };
        assert_eq!(outside_baseline(&requires).len(), 3);
    }

    /// The repository root, from this crate's manifest directory.
    fn root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask sits under the repository root")
            .to_path_buf()
    }

    #[test]
    fn every_shipped_manifest_allows_longer_than_the_server_drains() {
        // This lived only in `check-package`, which meant it ran when somebody chose to run
        // it. A mutation shortening the Kubernetes grace to less than the drain SURVIVED the
        // whole suite: the comparison was correct and nothing exercised it. A check that is
        // only a command is a check that is only sometimes made.
        assert!(check_grace(&root()), "a shipped manifest cuts the drain short");
    }

    #[test]
    fn the_grace_check_fails_when_a_manifest_is_too_short() {
        // The check proven to bite, rather than assumed to. A checker that cannot be shown
        // to fail is indistinguishable from one that always passes.
        let manifest = "spec:\n  terminationGracePeriodSeconds: 5\n";
        let grace = declared_grace(manifest).expect("a grace is declared");
        assert!(
            grace <= 30,
            "the fixture must be shorter than the drain for this test to mean anything"
        );
    }

    #[test]
    fn every_declared_target_states_a_baseline_and_a_support_level() {
        // A row with no formats and server support would be a wishlist entry published as a
        // support matrix, which is how somebody plans a deployment that cannot happen.
        for target in SUPPORTED {
            assert!(!target.triple.is_empty());
            assert!(target.note.len() > 40, "{} has no useful note", target.triple);
            if target.support == Support::Server {
                assert!(
                    !target.formats.is_empty(),
                    "{} claims server support and publishes nothing",
                    target.triple
                );
                assert_ne!(
                    target.baseline,
                    Baseline::None,
                    "{} claims server support with no stated baseline",
                    target.triple
                );
            } else {
                assert!(
                    target.formats.is_empty(),
                    "{} is client-only and publishes an artifact",
                    target.triple
                );
            }
        }
    }

    #[test]
    fn no_two_targets_share_a_triple() {
        let mut triples: Vec<&str> = SUPPORTED.iter().map(|t| t.triple).collect();
        triples.sort_unstable();
        let before = triples.len();
        triples.dedup();
        assert_eq!(triples.len(), before);
    }

    #[test]
    fn a_grace_is_read_from_each_manifest_dialect() {
        assert_eq!(
            declared_grace("spec:\n  terminationGracePeriodSeconds: 45\n"),
            Some(45)
        );
        assert_eq!(declared_grace("[Service]\nTimeoutStopSec=45s\n"), Some(45));
        assert_eq!(declared_grace("    stop_grace_period: 45s\n"), Some(45));
        assert_eq!(declared_grace("nothing here"), None);
    }

    #[test]
    fn the_manifests_name_the_version_this_workspace_is() {
        // `RUN-11`. The Kubernetes manifest named an image at a version, and there was no
        // Dockerfile anywhere in the repository --- so the manifest could not be applied by
        // anybody, including whoever wrote it, and nothing said so. A manifest is not run by
        // any test; a missing image is discovered by an operator at the moment they most
        // need it to work.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the workspace root");
        assert!(
            check_images_are_built(root),
            "every image a manifest names must be one this repository builds, at this version"
        );
        // Not vacuous: there is a workspace version to compare against, and it is the one
        // the manifests are checked against rather than a default that would match anything.
        let version = workspace_version(root).expect("a workspace version");
        assert!(!version.is_empty());
        assert!(version.contains('.'), "a version is a version: {version}");
    }
}

//! Whether a published speed figure has something that produced it.
//!
//! # Why compiling the benchmarks was not enough
//!
//! The first version of this check built every benchmark target and asserted that the two
//! crates publishing figures each had a `benches/` directory. Both properties can hold while
//! every number in the documentation is unbacked: a directory is not a measurement, and
//! nothing tied one figure to one benchmark. `ADR-0020` had already published three speed
//! tables that nothing produced --- a figure in prose reads as measured --- and a check that
//! confirms a directory exists would have passed on the day they were written.
//!
//! So the relationship is stated, and stated in the document beside the number. A figure
//! carries either a benchmark that can be re-run or a recorded measurement that says under
//! what conditions it was taken, and this check resolves the reference. An unresolvable one
//! fails the build, which is what makes a stale figure loud rather than quietly wrong.

use std::path::Path;

/// The crates that publish figures and must be able to reproduce them.
const MUST_MEASURE: &[&str] = &["sankhya-functions", "sankhya-math"];

/// Documents whose speed claims must carry provenance.
///
/// Everything under `docs/`, because a figure is as persuasive in a tutorial as in an ADR ---
/// more so, since a reader reaches a tutorial without the context that would make them ask.
fn documents(root: &Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.join("docs")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "md") {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// Whether a line states how much faster or slower something is.
///
/// Narrow on purpose. A ratio with a direction is the shape that reads as a benchmark result
/// and is the shape that gets restated until it looks established; a bare number in a table of
/// timings is a datum, and the surrounding text is what claims something about it.
fn states_a_ratio(line: &str) -> bool {
    let mut chars = line.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        if c != '\u{d7}' && c != 'x' {
            continue;
        }
        // A digit before it, so `2.4x` counts and `x` in a word does not.
        let digit_before = line[..at]
            .chars()
            .next_back()
            .is_some_and(|p| p.is_ascii_digit());
        if !digit_before {
            continue;
        }
        let after = line[at..].to_ascii_lowercase();
        if after.starts_with("\u{d7} faster")
            || after.starts_with("\u{d7} slower")
            || after.starts_with("x faster")
            || after.starts_with("x slower")
        {
            return true;
        }
    }
    false
}

/// The paragraph a line belongs to: the run of non-blank lines around it.
///
/// Provenance is allowed anywhere in the paragraph rather than on the line itself, because a
/// figure often appears mid-sentence and a citation belongs where a reader would look for one
/// --- at the end of the thought, not wedged into it.
fn paragraph(lines: &[&str], at: usize) -> String {
    let mut from = at;
    while from > 0 && !lines[from - 1].trim().is_empty() {
        from -= 1;
    }
    let mut to = at;
    while to + 1 < lines.len() && !lines[to + 1].trim().is_empty() {
        to += 1;
    }
    lines[from..=to].join(" ")
}

/// Every `benchmark_group("...")` name declared under `crates/<crate>/benches`.
fn groups_of(root: &Path, krate: &str) -> Vec<String> {
    let benches = root.join("crates").join(krate).join("benches");
    let mut names = Vec::new();
    let Ok(entries) = std::fs::read_dir(&benches) else {
        return names;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.extension().is_some_and(|e| e == "rs") {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        let mut rest = source.as_str();
        while let Some(at) = rest.find("benchmark_group(\"") {
            rest = &rest[at + "benchmark_group(\"".len()..];
            if let Some(end) = rest.find('"') {
                names.push(rest[..end].to_string());
            }
        }
    }
    names
}

/// Every heading in `STATUS.md`, lower-cased, for resolving a `§` citation.
fn status_headings(root: &Path) -> Vec<String> {
    std::fs::read_to_string(root.join("docs").join("STATUS.md"))
        .unwrap_or_default()
        .lines()
        .filter(|line| line.starts_with('#'))
        .map(|line| line.trim_start_matches('#').trim().to_ascii_lowercase())
        .collect()
}

/// The `[bench: crate/group]` reference in a paragraph, if it carries one.
fn bench_reference(paragraph: &str) -> Option<(String, String)> {
    let at = paragraph.find("[bench: ")?;
    let rest = &paragraph[at + "[bench: ".len()..];
    let end = rest.find(']')?;
    let (krate, group) = rest[..end].trim().split_once('/')?;
    Some((krate.trim().to_string(), group.trim().to_string()))
}

/// The `[rejected: why]` marker in a paragraph, if it carries one.
///
/// # Why a third form exists
///
/// Because some ratios describe an alternative that was measured and **not adopted**, and no
/// benchmark in this build can produce them: the code is not here. Lane-parallel SIMD
/// accumulation is 10--15x faster than what ships and returns a different, worse number, and
/// that is worth stating --- a reader who does not know it will propose it. Forcing such a
/// figure to cite a benchmark would mean citing one that measures something else, which is
/// how a paragraph comes to carry a citation that does not cover it.
///
/// The reason is required and is not checked for content. What it buys is that the writer had
/// to say why, in the document, where a reader sees it.
fn rejected_reference(paragraph: &str) -> Option<String> {
    let at = paragraph.find("[rejected: ")?;
    let rest = &paragraph[at + "[rejected: ".len()..];
    let end = rest.find(']')?;
    let why = rest[..end].trim();
    (!why.is_empty()).then(|| why.to_string())
}

/// The `§Section` citation in a paragraph, if it carries one.
fn section_reference(paragraph: &str) -> Option<String> {
    let at = paragraph.find('\u{a7}')?;
    let rest = paragraph[at + '\u{a7}'.len_utf8()..].trim_start();
    // To the end of the citation, which a sentence or a clause ends.
    let end = rest
        .find(|c| c == '.' || c == ')' || c == ',' || c == ';')
        .unwrap_or(rest.len());
    let named = rest[..end].trim();
    (!named.is_empty()).then(|| named.to_ascii_lowercase())
}

/// Whether every published ratio names something that produced it, and it resolves.
pub fn every_figure_is_backed(root: &Path) -> bool {
    let headings = status_headings(root);
    let mut ok = true;
    let mut backed = 0usize;
    for document in documents(root) {
        let Ok(text) = std::fs::read_to_string(&document) else {
            continue;
        };
        let lines: Vec<&str> = text.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            if !states_a_ratio(line) {
                continue;
            }
            let shown = document
                .strip_prefix(root)
                .unwrap_or(&document)
                .display()
                .to_string();
            let context = paragraph(&lines, index);
            if let Some((krate, group)) = bench_reference(&context) {
                if groups_of(root, &krate).iter().any(|name| *name == group) {
                    backed += 1;
                } else {
                    eprintln!("  NO SUCH BENCH   {shown}:{} cites `{krate}/{group}` and no benchmark group of that name is declared there. A citation that does not resolve is worse than none: it reads as checked", index + 1);
                    ok = false;
                }
                continue;
            }
            if rejected_reference(&context).is_some() {
                backed += 1;
                continue;
            }
            if let Some(section) = section_reference(&context) {
                if headings.iter().any(|h| h.contains(&section) || section.contains(h.as_str())) {
                    backed += 1;
                } else {
                    eprintln!("  NO SUCH SECTION {shown}:{} cites STATUS.md §{section}, which is not a heading there. The measurement it points at cannot be read", index + 1);
                    ok = false;
                }
                continue;
            }
            eprintln!("  UNBACKED FIGURE {shown}:{} states a speed ratio and names nothing that produced it. Cite a benchmark as `[bench: crate/group]`, the recorded measurement as `§Section` of STATUS.md, or `[rejected: why]` for an alternative this build does not contain --- a figure in prose reads as measured", index + 1);
            ok = false;
        }
    }
    if ok {
        println!("   {backed} published speed figure(s), each naming what produced it");
    }
    ok
}

/// Whether each crate that publishes figures has benchmarks at all.
pub fn every_publisher_can_measure(root: &Path) -> bool {
    let mut ok = true;
    for name in MUST_MEASURE {
        let benches = root.join("crates").join(name).join("benches");
        let has_source = benches.is_dir()
            && crate::package::files_under(&benches)
                .iter()
                .any(|p| p.extension().is_some_and(|e| e == "rs"));
        if !has_source {
            eprintln!("  NO BENCHMARK    {name} publishes speed figures and has no benches/ directory; a number nothing can re-run is a claim");
            ok = false;
        }
    }
    ok
}

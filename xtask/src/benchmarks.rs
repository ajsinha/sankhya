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
///
/// And `README.md`, `sdk/`, `packaging/` and the deck generator, because a figure published
/// outside `docs/` is published just the same. This scanned `docs/` alone for one round, and
/// the README, a slide in `tools/deck/` and a doc comment in `rows.rs` --- *the* place
/// `TESTING.md` names as where the retracted tables were restated --- all sat outside it.
fn documents(root: &Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    if root.join("README.md").is_file() {
        found.push(root.join("README.md"));
    }
    let mut stack = vec![
        root.join("docs"),
        root.join("sdk"),
        root.join("packaging"),
        root.join("tools").join("deck"),
    ];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .extension()
                .is_some_and(|e| e == "md" || e == "py")
            {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// Whether a line states how much faster or slower something is.
///
/// # What this catches, and what it deliberately does not
///
/// A ratio with a direction is the shape that reads as a benchmark result and the shape that
/// gets restated until it looks established. The first version of this asked for
/// `<digit>x faster` with exactly one space, on one line, and that saw **fourteen** lines in
/// the whole of `docs/` while well over a hundred published ratios went unchecked --- among
/// them `"10 to 15 times faster"`, which is one of the three retracted tables this gate was
/// written for, sitting uncited while its *restatement* elsewhere was made to carry a marker.
///
/// So: the direction word may be anywhere in the same line rather than adjacent, a ratio may
/// be spelled in words, and a table cell counts. What is still not caught is a direction word
/// that wraps to the next line, and that is stated in `TESTING.md` rather than implied ---
/// widening the window to the paragraph would make every row of a timings table a claim.
fn states_a_ratio(line: &str) -> bool {
    let lowered = line.to_ascii_lowercase();
    // `faster` and `slower` only. `cost`, `price`, `worse` and `cheaper` were in this list for
    // one run and matched ordinary prose everywhere --- "the price of pinning down", "costs
    // differ" --- which is how a gate becomes something people add markers to in order to
    // silence rather than something they read.
    let directed = lowered.contains("faster") || lowered.contains("slower");
    if !directed {
        return false;
    }
    // A ratio in figures: a digit immediately before a multiplier sign.
    for (at, c) in lowered.char_indices() {
        if c != '\u{d7}' && c != 'x' {
            continue;
        }
        if lowered[..at].chars().next_back().is_some_and(|p| p.is_ascii_digit()) {
            return true;
        }
    }
    // A ratio in words, and the direction word has to be adjacent to the magnitude ---
    // "three times in one sitting" is a repetition and not a factor, and it was caught by an
    // earlier version of this that asked only whether both a number word and `times` appeared
    // anywhere on the line.
    lowered.contains("times faster")
        || lowered.contains("times slower")
        || lowered.contains("magnitude faster")
        || lowered.contains("magnitude slower")
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
    // Recursive, and through the same helper `every_publisher_can_measure` uses --- a
    // non-recursive `read_dir` here and a recursive walk there is two answers to "what is a
    // benchmark file", and a citation to a group in `benches/sub/x.rs` would fail as NO SUCH
    // BENCH while the crate counted as having benchmarks.
    for path in crate::package::files_under(&benches) {
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

/// The `[historical: why]` marker in a paragraph, if it carries one.
///
/// # A fourth form, and the failure that needed it
///
/// Some ratios are observations from a run that happened once, against code that has since
/// been replaced --- an audit reconstructing a retracted table, a figure from a different
/// machine and a different build. No benchmark here can produce them, and a benchmark that
/// *resolves* is worse than none for exactly those: `[bench: sankhya-functions/row-access]`
/// resolves, and that group measures borrowing against copying in the **current** build, not
/// the ratio between an audit's reconstruction and a table nothing ever produced. A citation
/// that resolves and does not measure the figure is the failure this gate exists to stop,
/// arriving through the gate.
///
/// So a historical figure says so, and says why, and the reason is required.
fn historical_reference(paragraph: &str) -> Option<String> {
    let at = paragraph.find("[historical: ")?;
    let rest = &paragraph[at + "[historical: ".len()..];
    let end = rest.find(']')?;
    let why = rest[..end].trim();
    (!why.is_empty()).then(|| why.to_string())
}

/// The `§Section` citation in a paragraph, if it carries one.
fn section_reference(paragraph: &str) -> Option<String> {
    let at = paragraph.find('\u{a7}')?;
    let rest = paragraph[at + '\u{a7}'.len_utf8()..].trim_start();
    // To the end of the citation. A sentence or a clause ends it --- but **not** a full stop
    // inside a section number, because stopping at the first `.` turned a citation of
    // "10.8" into the bare string "10", and a heading containing "10" resolved it.
    let mut end = rest.len();
    for (at, c) in rest.char_indices() {
        let numeric = c == '.'
            && rest[..at].chars().next_back().is_some_and(|p| p.is_ascii_digit())
            && rest[at + 1..].chars().next().is_some_and(|n| n.is_ascii_digit());
        if numeric {
            continue;
        }
        if c == '.' || c == ')' || c == ',' || c == ';' {
            end = at;
            break;
        }
    }
    let named = rest[..end].trim();
    // Four characters, because a citation shorter than that is not a section name --- it is
    // a fragment that will match something. This is the second half of the same defect: the
    // matcher below asks whether a heading *contains* the citation **or** the citation
    // contains the heading, and against a document with headings like `cost` and `m9` that
    // second direction resolves almost anything.
    (named.chars().count() >= 4).then(|| named.to_ascii_lowercase())
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
            if historical_reference(&context).is_some() {
                backed += 1;
                continue;
            }
            if let Some((krate, group)) = bench_reference(&context) {
                if groups_of(root, &krate).iter().any(|name| *name == group) {
                    backed += 1;
                } else {
                    eprintln!("  NO SUCH BENCH   {shown}:{} cites `{krate}/{group}` and no benchmark group of that name is declared there. A citation that does not resolve is worse than none: it reads as checked", index + 1);
                    ok = false;
                }
                continue;
            }
            // Before the bench form, so a paragraph carrying both is scored by the narrower
            // claim rather than by whichever the scanner happened to find first.
            if historical_reference(&context).is_some() || rejected_reference(&context).is_some() {
                backed += 1;
                continue;
            }
            if let Some(section) = section_reference(&context) {
                // One direction only. A heading may be longer than the citation --- "§A
                // required setting that was costing 2.6x" abbreviates a longer heading --- but
                // a *citation* longer than a heading resolving against it is how "§10.8's size
                // decision" came to be satisfied by a heading containing "m10".
                if headings.iter().any(|h| h.contains(&section)) {
                    backed += 1;
                } else {
                    eprintln!("  NO SUCH SECTION {shown}:{} cites STATUS.md §{section}, which is not a heading there. The measurement it points at cannot be read", index + 1);
                    ok = false;
                }
                continue;
            }
            eprintln!("  UNBACKED FIGURE {shown}:{} states a speed ratio and names nothing that produced it. Cite a benchmark as `[bench: crate/group]`, the recorded measurement as `§Section` of STATUS.md, `[rejected: why]` for an alternative this build does not contain, or `[historical: why]` for a run against code that no longer exists --- a figure in prose reads as measured", index + 1);
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

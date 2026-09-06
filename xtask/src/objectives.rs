//! Every service-level objective appears in the table that reports on it.
//!
//! # Why omission is the failure worth catching
//!
//! `PERF-05`. Eighteen `NFR-PERF-*` objectives are stated in `docs/REQUIREMENTS.md`. The table
//! that reports their state listed eleven, and **seven were not there at all** --- not recorded
//! as unmet, not recorded as untested, simply absent. Among them was `NFR-PERF-06`, which is
//! the function catalogue's own requirement and the workload its performance claims are about.
//!
//! An objective reported as *unmet* is a decision somebody took. An objective that is not in
//! the table is one nobody has to think about, and it reads --- to anybody scanning the table
//! for red --- exactly like an objective that is fine. That is the same shape as a metric
//! nothing emits and an alert that can never fire: absence rendering as health.
//!
//! So the requirement here is completeness, not achievement. **This check has no opinion about
//! whether an objective is met.** It fails only when one is missing from the table, which is
//! the state that cannot be seen by reading the table.

use std::collections::BTreeSet;
use std::path::Path;

/// Where the objectives are stated.
const STATED: &str = "docs/REQUIREMENTS.md";
/// Where their state is reported.
const REPORTED: &str = "docs/book/part5/24-requirements.md";

/// Every `NFR-PERF-nn` identifier in `text`, including both ends of a `nn`–`mm` range.
///
/// Ranges are expanded because the table writes `NFR-PERF-09`–`14` for six objectives that
/// share a verdict, which is reasonable prose and would otherwise read as two entries.
#[must_use]
pub fn objectives(text: &str) -> BTreeSet<u32> {
    let mut found = BTreeSet::new();
    let mut rest = text;
    while let Some(at) = rest.find("NFR-PERF-") {
        let after = &rest[at + "NFR-PERF-".len()..];
        let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(number) = digits.parse::<u32>() {
            found.insert(number);
            // A range: `NFR-PERF-09`–`14`. The closing tick, a dash of some kind, an opening
            // tick, then a bare number. Written out rather than matched with a regular
            // expression so that the three dash characters a writer may use are visible.
            let tail = &after[digits.len()..];
            let tail = tail.trim_start_matches('`');
            let tail = tail
                .trim_start_matches(['\u{2013}', '\u{2014}', '-'])
                .trim_start_matches('`');
            let upper: String = tail.chars().take_while(char::is_ascii_digit).collect();
            if let Ok(upper) = upper.parse::<u32>() {
                if upper > number && upper - number < 32 {
                    for n in number..=upper {
                        found.insert(n);
                    }
                }
            }
        }
        rest = &rest[at + "NFR-PERF-".len()..];
    }
    found
}

/// Every stated objective is reported on.
#[must_use]
pub fn check(root: &Path) -> bool {
    println!("== check-objectives ==");

    let Ok(stated_text) = std::fs::read_to_string(root.join(STATED)) else {
        eprintln!("  COULD NOT READ  {STATED}");
        return false;
    };
    let Ok(reported_text) = std::fs::read_to_string(root.join(REPORTED)) else {
        eprintln!("  COULD NOT READ  {REPORTED}");
        return false;
    };

    let stated = objectives(&stated_text);
    // Only the table, not the whole chapter: an objective discussed in a paragraph is not an
    // objective whose state a reader can find, and counting the prose would let the check
    // pass on a table that still omits it.
    let table: String = reported_text
        .lines()
        .filter(|line| line.trim_start().starts_with('|'))
        .collect::<Vec<_>>()
        .join("\n");
    let reported = objectives(&table);

    if stated.is_empty() {
        eprintln!("  NO OBJECTIVES   {STATED} states none, so this check is measuring nothing");
        return false;
    }

    let mut ok = true;
    for number in stated.difference(&reported) {
        eprintln!("  NOT REPORTED    NFR-PERF-{number:02} is stated in {STATED} and is absent from the table in {REPORTED}. An objective nobody records is one nobody has to decide about, and it reads like one that is fine");
        ok = false;
    }
    for number in reported.difference(&stated) {
        eprintln!("  NOT STATED      NFR-PERF-{number:02} is reported on in {REPORTED} and is not stated in {STATED}; one of the two is wrong about what this system promises");
        ok = false;
    }

    if ok {
        println!("   {} objective(s) stated, and every one is reported on", stated.len());
    }
    ok
}

#[cfg(test)]
mod tests {
    use super::objectives;

    #[test]
    fn a_range_counts_as_every_objective_in_it() {
        // The table writes `NFR-PERF-09`–`14` for six objectives sharing one verdict. Reading
        // that as two would let four go unreported while the check passed.
        let found = objectives("| `NFR-PERF-09`\u{2013}`14` | graph | bounded |");
        assert_eq!(found.len(), 6);
        assert!(found.contains(&9) && found.contains(&14) && found.contains(&11));
    }

    #[test]
    fn a_plain_identifier_is_one_objective() {
        assert_eq!(objectives("`NFR-PERF-06` is the catalogue's own"), [6].into());
    }

    #[test]
    fn prose_after_an_identifier_is_not_a_range() {
        // `NFR-PERF-02` followed by a sentence that happens to contain a number must not
        // swallow it: an over-eager range makes the check pass on a table missing entries.
        let found = objectives("`NFR-PERF-02` — met at 13 ms");
        assert_eq!(found, [2].into());
    }
}

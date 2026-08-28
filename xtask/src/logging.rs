//! The tenant-data prohibition, checked rather than asserted.
//!
//! `ARCHITECTURE` §17.1 states it plainly:
//!
//! > **No log line, trace attribute or metric label may contain tenant data.** Query text is
//! > data: a normalized plan hash is logged by default, with full text only under explicit
//! > policy and routed to the audit store rather than to standard output.
//!
//! Metric labels are already structural --- a label is a closed value set or a capped
//! identifier, so it cannot carry a row. Logs had nothing but the sentence.
//!
//! # The dangerous case is the one nobody writes
//!
//! An explicit `tracing::info!(%sql, "ran a query")` is visible in review. What is not is
//! **`#[instrument]`**, which logs *every argument of the function it decorates* by default.
//! Put it on `fn query(&self, sql: &str)` and every statement any client ever sends is in
//! the log, including the predicate values, and nothing at the call site says so.
//!
//! That is the failure this check is really for. The explicit form is caught too, because it
//! costs nothing to catch and somebody will eventually write it.
//!
//! # There is no suppression comment
//!
//! Deliberately. A prohibition with an escape hatch becomes a prohibition with escapes in
//! it, and the reviewer who adds the third one is not thinking about tenants. If a field is
//! genuinely needed, the answer is a hash, a shape, or a count --- `statement_shape` in the
//! server does exactly that, logging the first two words of a statement rather than the
//! statement.

use std::path::Path;

/// Field names that carry caller data in this codebase.
///
/// Not a general-purpose list of scary words. Each of these names something a user supplied:
/// a statement, a value from a row, a credential. `table` and `tenant` are deliberately
/// absent --- an identifier is not data, and forbidding them would make the check useless
/// for the thing it exists to catch by making it fire constantly.
pub const CARRIES_DATA: &[&str] = &[
    "sql",
    "statement",
    "query",
    "text",
    "row",
    "rows",
    "value",
    "values",
    "payload",
    "body",
    "secret",
    "password",
    "token",
    "credential",
];

/// The log macros.
const MACROS: &[&str] = &[
    "tracing::trace!",
    "tracing::debug!",
    "tracing::info!",
    "tracing::warn!",
    "tracing::error!",
    "trace!(",
    "debug!(",
    "info!(",
    "warn!(",
    "error!(",
];

/// One thing that would put caller data into a log.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Leak {
    /// Which file.
    pub file: String,
    /// Which line, one-indexed.
    pub line: usize,
    /// What is wrong.
    pub why: String,
}

/// Whether a line records a field named after something a caller supplied.
///
/// Matches the three shapes `tracing` accepts --- `%name`, `?name` and `name = ...` --- and
/// requires a word boundary, so `rows_returned` and `value_not_permitted` do not fire. A
/// check that fires on every second line is a check somebody switches off.
#[must_use]
pub fn logged_fields(line: &str) -> Vec<&'static str> {
    if !MACROS.iter().any(|macro_name| line.contains(macro_name)) {
        return Vec::new();
    }
    CARRIES_DATA
        .iter()
        .copied()
        .filter(|field| {
            [format!("%{field}"), format!("?{field}"), format!("{field} =")]
                .iter()
                .any(|shape| contains_whole(line, shape))
        })
        .collect()
}

/// `needle` in `haystack`, not as part of a longer identifier.
fn contains_whole(haystack: &str, needle: &str) -> bool {
    let mut from = 0usize;
    while let Some(at) = haystack.get(from..).and_then(|rest| rest.find(needle)) {
        let start = from + at;
        let end = start + needle.len();
        let before_ok = start == 0
            || !haystack
                .get(..start)
                .and_then(|s| s.chars().next_back())
                .is_some_and(|c| c.is_alphanumeric() || c == '_');
        let after_ok = !haystack
            .get(end..)
            .and_then(|s| s.chars().next())
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
        if before_ok && after_ok {
            return true;
        }
        from = end;
    }
    false
}

/// Whether an `#[instrument]` attribute logs every argument.
///
/// `#[instrument]` with no `skip_all` and no `skip(..)` records **all** of them, which on a
/// function taking a statement puts every query any client sends into the log. The attribute
/// is three words long and the consequence is not visible in it.
#[must_use]
pub fn instruments_everything(line: &str) -> bool {
    let trimmed = line.trim();
    if !trimmed.starts_with("#[instrument") && !trimmed.starts_with("#[tracing::instrument") {
        return false;
    }
    !trimmed.contains("skip_all") && !trimmed.contains("skip(")
}

/// Every leak under `root`.
#[must_use]
pub fn scan(root: &Path) -> Vec<Leak> {
    let mut found = Vec::new();
    walk(&root.join("crates"), &mut found);
    found
}

fn walk(dir: &Path, found: &mut Vec<Leak>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, found);
            continue;
        }
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        // Tests choose their own data, so a test logging a fixture leaks nothing. The rule
        // is about what a *server* writes about a caller.
        if path.components().any(|c| c.as_os_str() == "tests") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (index, line) in text.lines().enumerate() {
            let file = path.display().to_string();
            if line.trim_start().starts_with("//") {
                continue;
            }
            for field in logged_fields(line) {
                found.push(Leak {
                    file: file.clone(),
                    line: index + 1,
                    why: format!(
                        "logs a field named `{field}`, which in this codebase carries what a \
                         caller supplied. ARCHITECTURE §17.1: no log line may contain tenant \
                         data. Log a hash, a shape or a count instead — `statement_shape` in \
                         the server logs the first two words of a statement rather than the \
                         statement"
                    ),
                });
            }
            if instruments_everything(line) {
                found.push(Leak {
                    file: file.clone(),
                    line: index + 1,
                    why: "`#[instrument]` without `skip_all` or `skip(..)` records every \
                          argument of the function it decorates. On anything taking a \
                          statement or a row that is every value a client ever sent, and \
                          nothing at the call site says so"
                        .to_string(),
                });
            }
        }
    }
}

/// The check.
#[must_use]
pub fn check(root: &Path) -> bool {
    println!("== check-logging ==");
    let leaks = scan(root);
    for leak in &leaks {
        eprintln!("  TENANT DATA  {}:{}: {}", leak.file, leak.line, leak.why);
    }
    if leaks.is_empty() {
        println!("   no log statement records what a caller supplied");
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_interpolated_statement_is_caught_in_every_shape_tracing_accepts() {
        assert_eq!(
            logged_fields(r#"tracing::info!(%sql, "ran a query");"#),
            ["sql"]
        );
        assert_eq!(
            logged_fields(r#"tracing::debug!(?statement, "planning");"#),
            ["statement"]
        );
        assert_eq!(
            logged_fields(r#"tracing::warn!(query = %text, "slow");"#),
            ["query", "text"]
        );
    }

    #[test]
    fn a_longer_identifier_containing_a_forbidden_word_does_not_fire() {
        // `rows_returned` is a count and `value_not_permitted` is a reason. A check that
        // fires on every second line is a check somebody switches off, and then it is not a
        // check.
        for line in [
            r#"tracing::info!(rows_returned = 12, "served");"#,
            r#"tracing::info!(value_not_permitted = 3, "refused");"#,
            r#"tracing::debug!(query_id = %id, "planned");"#,
        ] {
            assert!(logged_fields(line).is_empty(), "{line}");
        }
    }

    #[test]
    fn a_forbidden_shape_inside_a_longer_identifier_does_not_fire() {
        // The case the word-boundary check actually exists for, and the one nothing tested:
        // `"rows ="` is a genuine substring of `"myrows = 12"`, so only the boundary saves
        // it. The earlier tests were all rejected by plain substring absence — `rows =` is
        // simply not present in `rows_returned = 12` — so the boundary logic was unexercised
        // and a mutation removing half of it survived.
        assert!(logged_fields(r#"tracing::info!(myrows = 12, "served");"#).is_empty());
        assert!(logged_fields(r#"tracing::info!(subquery = 1, "planned");"#).is_empty());
        assert!(logged_fields(r#"tracing::info!(rows = 12, "served");"#) == ["rows"]);
        assert!(logged_fields(r#"tracing::info!(sqlx = 1, "x");"#).is_empty());
        // And the boundary *after* the name, which is a separate rejection: here `%sql` is a
        // real substring of `%sqlx` and the character before it is `(`, so only the trailing
        // check refuses it. Every earlier case was rejected by the leading one, which left
        // half the boundary logic untested and a mutation removing it alive.
        assert!(logged_fields(r#"tracing::info!(%sqlx, "x");"#).is_empty());
        assert!(logged_fields(r#"tracing::info!(?queryable, "x");"#).is_empty());
        assert!(logged_fields(r#"tracing::info!(%sql, "x");"#) == ["sql"]);
    }

    #[test]
    fn a_line_that_is_not_a_log_statement_is_ignored() {
        assert!(logged_fields("let sql = statement.to_string();").is_empty());
        assert!(logged_fields(r#"failure(state, &format!("{sql}"))"#).is_empty());
    }

    #[test]
    fn instrument_without_a_skip_records_every_argument() {
        // The dangerous case, and the one nobody writes deliberately: three words, and every
        // statement any client sends is in the log.
        assert!(instruments_everything("#[instrument]"));
        assert!(instruments_everything("    #[tracing::instrument(level = \"info\")]"));
    }

    #[test]
    fn instrument_that_skips_is_allowed() {
        assert!(!instruments_everything("#[instrument(skip_all)]"));
        assert!(!instruments_everything("#[instrument(skip(sql))]"));
        assert!(!instruments_everything("#[instrument(skip_all, fields(rows = 1))]"));
    }

    #[test]
    fn the_forbidden_list_names_data_and_not_identifiers() {
        // `table` and `tenant` are deliberately absent: an identifier is not data, and
        // forbidding them would make the check fire constantly and therefore useless for
        // what it exists to catch.
        assert!(!CARRIES_DATA.contains(&"table"));
        assert!(!CARRIES_DATA.contains(&"tenant"));
        assert!(CARRIES_DATA.contains(&"sql"));
        assert!(CARRIES_DATA.contains(&"password"));
    }

    #[test]
    fn this_repository_leaks_nothing() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask sits under the repository root")
            .to_path_buf();
        let leaks = scan(&root);
        assert!(leaks.is_empty(), "{leaks:#?}");
    }
}

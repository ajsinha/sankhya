//! What a `SET` statement names and what it sets it to.
//!
//! # Why this is one function rather than two parses
//!
//! It was two. The wire layer parsed a `SET` to remember it on the session, and the server
//! parsed the same statement again to decide whether the value was allowed — and both split on
//! whitespace, so both read `SET SNAPSHOT='eod'` as a verb and **one** word.
//!
//! The consequences differed and compounded. The wire layer stored a setting called
//! `snapshot='eod'`, which nothing ever reads. The server's check asked whether the name was
//! `snapshot`, decided it was not, and let the statement fall through to the handler that
//! accepts any `SET` as a no-op. So the statement was acknowledged with no validation, no
//! effect, and no symptom: `CLI-07`. `SET VERSION OF sales.orders=2` broke the same way one
//! token further along.
//!
//! Two parsers agreeing is worth nothing when they are wrong together, and they were written
//! by the same hand within a week. One function is what makes the wire layer's memory and the
//! server's validation talk about the same statement by construction.

/// A `SET` or `RESET`, as its parts.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Setting {
    /// The setting's name, lower-cased.
    ///
    /// `SET VERSION OF <table> = <n>` names two words before the value, so this is
    /// `version of <table>` rather than `version`. Kept general rather than special-cased: a
    /// name ending at the first word would make every `... OF <x>` the same setting, and the
    /// last one written would win.
    pub name: String,
    /// What it was set to, unquoted. Empty for a `RESET`.
    pub value: String,
    /// Whether this was a `RESET` rather than a `SET`.
    pub reset: bool,
}

/// Read a `SET` or `RESET`, or `None` if the statement is neither.
///
/// The three spellings a client sends — `name = value`, `name value`, `name TO value` — are one
/// shape here, whatever the spacing, because the split on `=` happens **before** the split on
/// whitespace. That order is the whole point: a whitespace splitter cannot see the `=` inside
/// `SNAPSHOT='eod'`, and everything downstream then talks about a setting nobody named.
#[must_use]
pub fn parse(sql: &str) -> Option<Setting> {
    let compact = sql.trim().trim_end_matches(';').trim();

    let (head, assigned) = match compact.split_once('=') {
        Some((head, tail)) => (head.trim(), Some(tail.trim())),
        None => (compact, None),
    };

    let mut words = head.split_whitespace();
    let verb = words.next().unwrap_or_default().to_uppercase();
    if verb != "SET" && verb != "RESET" {
        return None;
    }
    let mut name = words.next()?.trim_matches('"').to_lowercase();

    let mut rest: Vec<&str> = words.collect();
    if name == "version" && rest.first().is_some_and(|word| word.eq_ignore_ascii_case("OF")) {
        if let Some(table) = rest.get(1) {
            name = format!("version of {}", table.trim_matches('"').to_lowercase());
            rest.drain(..2);
        }
    }

    if verb == "RESET" {
        return Some(Setting { name, value: String::new(), reset: true });
    }

    let value = assigned.map_or_else(
        || {
            rest.iter()
                .filter(|word| !word.eq_ignore_ascii_case("TO"))
                .copied()
                .collect::<Vec<&str>>()
                .join(" ")
        },
        ToOwned::to_owned,
    );
    let value = value.trim().trim_matches(|c| c == '\'' || c == '"').to_owned();
    Some(Setting { name, value, reset: false })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

    use super::*;

    fn named(sql: &str) -> (String, String) {
        let setting = parse(sql).unwrap_or_else(|| panic!("not read as a setting: {sql}"));
        (setting.name, setting.value)
    }

    #[test]
    fn every_spelling_of_one_statement_reads_the_same() {
        // `CLI-07`, as the spellings a driver actually sends. The one with no spaces is the
        // one that was acknowledged and did nothing.
        for sql in [
            "SET SNAPSHOT = 'eod'",
            "SET SNAPSHOT='eod'",
            "SET SNAPSHOT 'eod'",
            "SET SNAPSHOT TO 'eod'",
            "set snapshot='eod';",
            "  SET   SNAPSHOT   =   'eod'  ",
        ] {
            assert_eq!(
                named(sql),
                ("snapshot".to_owned(), "eod".to_owned()),
                "read differently: {sql}"
            );
        }
    }

    #[test]
    fn a_version_setting_carries_the_table_in_its_name() {
        for sql in [
            "SET VERSION OF sales.orders = 2",
            "SET VERSION OF sales.orders=2",
            "SET VERSION OF sales.orders 2",
        ] {
            assert_eq!(
                named(sql),
                ("version of sales.orders".to_owned(), "2".to_owned()),
                "read differently: {sql}"
            );
        }
    }

    #[test]
    fn two_tables_are_two_settings_rather_than_one() {
        // A name ending at the first word would make these the same setting, and the last one
        // written would decide what both tables are read as.
        assert_ne!(
            named("SET VERSION OF sales.orders = 2").0,
            named("SET VERSION OF hr.people = 3").0
        );
    }

    #[test]
    fn a_value_may_contain_the_character_the_statement_is_split_on() {
        // Split on the **first** `=`, which is the assignment; any later one is the value's.
        assert_eq!(named("SET SEARCH_PATH = 'a=b'").1, "a=b");
    }

    #[test]
    fn a_reset_names_what_it_clears_and_sets_nothing() {
        let setting = parse("RESET SNAPSHOT").expect("a reset");
        assert!(setting.reset);
        assert_eq!(setting.name, "snapshot");
        assert!(setting.value.is_empty());
        assert_eq!(parse("RESET ALL").expect("a reset").name, "all");
    }

    #[test]
    fn a_statement_that_is_not_a_setting_is_not_read_as_one() {
        assert!(parse("SELECT 1").is_none());
        assert!(parse("").is_none());
        assert!(parse("SET").is_none(), "a bare SET names nothing");
    }
}

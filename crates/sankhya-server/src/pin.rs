//! What "where this session reads from" is, as a value.
//!
//! `COR-20`: the hydration cache key held the table's present version and nothing about the
//! position the session had chosen, so a pinned session's cells went in under the present
//! version and the next unpinned session was served them. A pinned read's whole promise is that
//! it does not move.
//!
//! Its own file rather than a `mod tests` inside `wiring.rs`, because that file is at the
//! 1500-line hard limit `check-loc` enforces --- and a test module is the wrong thing to spend
//! the last lines of a budget on.

#[cfg(test)]
mod tests {

    #![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

    use crate::wiring::pin_digest;
    use sankhya_api_pg::session::Caller;
    use std::collections::BTreeMap;

    fn caller_with(settings: &BTreeMap<String, String>) -> u64 {
        // No startup parameters: a pin is something a session `SET`, and deliberately not
        // something a connection string can assert — see `Caller::setting`.
        static NONE: &[(String, String)] = &[];
        pin_digest(&Caller::with_settings(NONE, settings))
    }

    #[test]
    fn a_session_reading_the_present_has_no_pin() {
        // Zero, and it must stay zero: every session reading the present shares cache entries
        // with every other one, and a non-zero digest here would make the ordinary case cold.
        assert_eq!(caller_with(&BTreeMap::new()), 0);
        assert_eq!(
            caller_with(&BTreeMap::from([("search_path".to_string(), "sales".to_string())])),
            0,
            "an ordinary setting is not a position"
        );
        assert_eq!(
            caller_with(&BTreeMap::from([("snapshot".to_string(), String::new())])),
            0,
            "a cleared snapshot is reading the present again"
        );
    }

    #[test]
    fn a_pinned_session_is_not_an_unpinned_one() {
        // `COR-20`. Without this the pinned session's cells go into the hydration cache under
        // the present version, and the next unpinned session is served them.
        let pinned = caller_with(&BTreeMap::from([
            ("snapshot".to_string(), "eod".to_string()),
        ]));
        assert_ne!(pinned, 0, "a session reading a snapshot looks unpinned");
    }

    #[test]
    fn two_positions_are_two_digests_and_one_position_is_one() {
        let eod = caller_with(&BTreeMap::from([("snapshot".to_string(), "eod".to_string())]));
        let month = caller_with(&BTreeMap::from([
            ("snapshot".to_string(), "month_end".to_string()),
        ]));
        assert_ne!(eod, month, "two snapshots digest the same, so one serves the other's cells");

        // And the same position twice is the same digest, or every pinned read is a cold one.
        assert_eq!(
            eod,
            caller_with(&BTreeMap::from([("snapshot".to_string(), "eod".to_string())]))
        );
    }

    #[test]
    fn a_version_pin_names_the_table_it_pins() {
        // `SET VERSION OF` names a different setting per table, which is why the digest walks
        // every setting rather than asking for one by name.
        let orders = caller_with(&BTreeMap::from([
            ("version of sales.orders".to_string(), "7".to_string()),
        ]));
        let people = caller_with(&BTreeMap::from([
            ("version of hr.people".to_string(), "7".to_string()),
        ]));
        assert_ne!(orders, 0);
        assert_ne!(
            orders, people,
            "two tables pinned at the same version digest the same, so one session's position \
             is served to another's"
        );

        // Two tables pinned together is a third position again, not either one of them.
        let both = caller_with(&BTreeMap::from([
            ("version of sales.orders".to_string(), "7".to_string()),
            ("version of hr.people".to_string(), "7".to_string()),
        ]));
        assert_ne!(both, orders);
        assert_ne!(both, people);
    }
}

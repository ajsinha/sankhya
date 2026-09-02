-- SANKHYA from SQL: declared ingest, quarantine, and resuming a halted feed.
--
--   psql -h 127.0.0.1 -p 5432 -U you -d sankhya -f 05-feeds.sql
--
-- Read-only. Declaring a feed is a file under `config/feeds/`, not a statement -- see
-- `config/feeds/README.md`. What a *client* can do is see them and resume them.

\echo '== every declared feed, and what it is doing =='
-- One row per feed: whether it is running or halted, when it halted and why, and how much it
-- has published, quarantined and skipped. A feed that has never managed to run is listed too
-- -- the case an operator most needs to see, and the one a "list of running feeds" omits.
SHOW FEEDS;

\echo ''
\echo '== records a feed refused =='
-- A record that does not fit is quarantined whole, exactly as it arrived, into a TABLE -- not
-- a directory of rejected files -- with the reason, a stable code, the position it arrived at,
-- and a fingerprint of the declaration that refused it.
--
-- Whole, because a record reduced to an error message cannot be replayed, and replay is the
-- only actual remedy.
SELECT feed, source, position, reason_code, reason, payload
FROM sank_quarantine
LIMIT 10;

\echo ''
\echo '== what refused, and how often =='
SELECT feed, reason_code, count(*) AS occurrences
FROM sank_quarantine
GROUP BY feed, reason_code
ORDER BY occurrences DESC;

\echo ''
\echo '== resuming a halted feed =='
-- A statement, not a restart: restarting the server to resume one feed takes an outage on
-- every other feed and every open connection.
--
-- Resuming does not forget -- the halt count survives it, because a feed that halted, was
-- resumed and halted again for the same reason is not in the situation a feed that halted
-- once is in.
--
--   RESUME FEED orders;

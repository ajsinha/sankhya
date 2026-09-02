-- SANKHYA from SQL: a name for one instant across many tables.
--
--   psql -h 127.0.0.1 -p 5432 -U you -d sankhya -f 08-snapshots.sql
--
-- Creates and drops its own snapshot. Safe to re-run.

\set ON_ERROR_STOP off
DROP SNAPSHOT IF EXISTS example_eod;

\echo '== why a snapshot and not a clone =='
-- A clone freezes a *thing*: one table, one version, a name in the catalogue.
-- A snapshot freezes a *moment*: many tables, one consistent position, quoted by a query.
--
-- A calculation that reads a population of records, a set of rates, a set of curves and the
-- hierarchy they roll up through must read all four AS OF ONE INSTANT -- or the reconciliation
-- problem this system exists to remove reappears inside a single query.

\echo ''
\echo '== take one =='
-- The expiry is REQUIRED and there is no `EXPIRE NEVER`. A snapshot pins files, so one that
-- never expired would hold a whole warehouse's versions alive, and the storage cost would fall
-- on somebody who did not ask for it.
CREATE SNAPSHOT example_eod EXPIRE AFTER 7 DAYS;

\echo ''
\echo '== what exists, what each pins, and who is paying for it =='
SHOW SNAPSHOTS;

\echo ''
\echo '== read as of it =='
-- A SESSION setting, because a run reads one instant across many statements. Another
-- connection is unaffected.
SET SNAPSHOT = 'example_eod';
SELECT count(*) AS as_of_the_snapshot FROM sales.orders;

\echo ''
\echo '== and the present again =='
RESET SNAPSHOT;
SELECT count(*) AS right_now FROM sales.orders;

\echo ''
\echo '== a snapshot that does not exist is refused AT THE SET =='
-- Not at the next query. A `SET` that succeeded and a query that then failed sends somebody to
-- look at the query.
SET SNAPSHOT = 'no_such_snapshot';

\echo ''
\echo '== the expiry cannot be omitted, and cannot be forever =='
CREATE SNAPSHOT no_expiry;
CREATE SNAPSHOT too_long EXPIRE AFTER 5000 DAYS;

\echo ''
\echo '== and it goes away cleanly, releasing what it pinned =='
DROP SNAPSHOT example_eod;
SHOW SNAPSHOTS;

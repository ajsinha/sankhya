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
-- REFUSES
SET SNAPSHOT = 'no_such_snapshot';

\echo ''
\echo '== the expiry cannot be omitted, and cannot be forever =='
-- REFUSES
CREATE SNAPSHOT no_expiry;
-- REFUSES
CREATE SNAPSHOT too_long EXPIRE AFTER 5000 DAYS;

\echo ''
\echo '== the log underneath the tag =='
-- A snapshot is a name for a version. This is where the versions come from.
--
-- `changed_data` is the WRITER'S OWN DECLARATION, not a guess from the file counts. A
-- compaction rewrites files and changes not one row, so it reports `f` -- a column that called
-- that a change would say your table moved every time maintenance ran.
--
-- `kept_by` names the snapshot or clone holding that version alive, and is EMPTY for versions
-- nothing is keeping. That emptiness is the point: history is readable only where something is
-- keeping it alive. Retirement deletes the files a merge replaced. The commit stays in the log
-- forever; its data does not.
SHOW HISTORY OF sales.orders;

\echo ''
\echo '== one table, at a version =='
-- Per table, per session, and independent of SET SNAPSHOT. This answers "what did THIS table
-- look like then"; a snapshot answers "what did EVERYTHING look like then".
SET VERSION OF sales.orders = 1;
SELECT count(*) AS at_version_one FROM sales.orders;
RESET VERSION OF sales.orders;
SELECT count(*) AS right_now FROM sales.orders;

\echo ''
\echo '== a version the table does not have is refused, and names what it does have =='
-- Replaying a log stops at its end, so this used to hand back the NEWEST version -- a version
-- nobody has, served as though they had it.
-- REFUSES
SET VERSION OF sales.orders = 9999;

\echo ''
\echo '== and a table that does not exist is refused at the SET, not at the query =='
-- REFUSES
SET VERSION OF sales.no_such_table = 1;

\echo ''
\echo '== what this is NOT =='
-- Not version control. There is no diff between two versions and no way to restore one,
-- because the log records FILES, not rows: a compaction replaces every file and changes
-- nothing, so a file-level diff would report a maintenance job as a total rewrite. A row-level
-- difference needs a decision before it needs code.
--
-- What you have is closer to a tag than a branch: name a moment, read it back, and know that
-- the naming is what keeps it readable.

\echo ''
\echo '== and it goes away cleanly, releasing what it pinned =='
DROP SNAPSHOT example_eod;
SHOW SNAPSHOTS;

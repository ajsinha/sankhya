-- SANKHYA from SQL: zero-copy cloning, lineage, and the drop that refuses.
--
--   psql -h 127.0.0.1 -p 5432 -U you -d sankhya -f 03-cloning.sql
--
-- Creates and drops its own tables. Safe to re-run.

\set ON_ERROR_STOP off
DROP TABLE IF EXISTS sales.q3_audit;
DROP TABLE IF EXISTS sales.q3_frozen;

\echo '== a clone is a reference, not a copy =='
-- It costs the same whether the origin holds a thousand rows or a billion, and it adds no
-- files of its own: the log records an origin and a version, and a read splices the origin's
-- live set at that version with the clone's own log.
CREATE TABLE q3_frozen CLONE sales.orders;

\echo ''
\echo '== and it reads the same rows =='
SELECT
  (SELECT count(*) FROM sales.orders)    AS origin,
  (SELECT count(*) FROM sales.q3_frozen) AS clone;

\echo ''
\echo '== a clone lands in its origin schema =='
-- Not a convention. A clone is authorized *through its origin*, so one placed under another
-- schema would have its name governed by one policy and its data by another.
SELECT table_schema, table_name FROM information_schema.tables WHERE table_name = 'q3_frozen';

\echo ''
\echo '== what is this a clone of? =='
-- Nearest first. The first row answers "what was this cloned from?" and the last answers
-- "what is it ultimately a snapshot of?", which one step cannot.
SHOW LINEAGE OF sales.q3_frozen;

\echo ''
\echo '== a clone of a clone =='
CREATE TABLE q3_audit CLONE sales.q3_frozen;
SHOW LINEAGE OF sales.q3_audit;
SELECT count(*) AS rows_through_two_levels FROM sales.q3_audit;

\echo ''
\echo '== what still reads this table? =='
-- Ask BEFORE dropping anything. `may_drop` names the clones that would break, but only after
-- the attempt -- which is no use to somebody who had no way to ask first.
SHOW DEPENDENTS OF sales.orders;

\echo ''
\echo '== the drop that refuses, and names what would break =='
-- REFUSES
DROP TABLE sales.q3_frozen;

\echo ''
\echo '== drop the leaf first, then its origin =='
DROP TABLE sales.q3_audit;
DROP TABLE sales.q3_frozen;
SHOW DEPENDENTS OF sales.orders;

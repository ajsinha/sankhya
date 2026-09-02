-- SANKHYA from SQL: one refusal per path, and what each tells you.
--
--   psql -h 127.0.0.1 -p 5432 -U you -d sankhya -f 06-refusals.sql
--
-- EVERY statement below is expected to fail. That is the point: a refusal is a feature, and
-- one that does not say what to do is a defect. `ON_ERROR_STOP` is off so the file runs
-- through.

\set ON_ERROR_STOP off

\echo '== a table that is not there =='
SELECT * FROM sales.no_such_table;

\echo ''
\echo '== a column that is not there =='
SELECT no_such_column FROM sales.orders;

\echo ''
\echo '== a write, against a read path =='
-- This server is a read path over a published warehouse. Writes arrive through capture or
-- through a declared feed, and the refusal names the route rather than merely saying no.
INSERT INTO sales.orders (id) VALUES (1);

\echo ''
\echo '== cloning something that is not there =='
CREATE TABLE nope CLONE sales.no_such_table;

\echo ''
\echo '== cloning into another schema =='
-- Refused: a clone is authorized through its origin, so one placed elsewhere would have its
-- name governed by one policy and its data by another.
CREATE TABLE archive.q3 CLONE sales.orders;

\echo ''
\echo '== a feed command with no feed named =='
RESUME FEED;

\echo ''
\echo '== resuming a feed nobody declared =='
-- Named rather than reported as success. An operator who mistypes a feed name and is told it
-- resumed will go away believing it did.
RESUME FEED no_such_feed;

\echo ''
\echo '== a clone question with no table =='
SHOW LINEAGE;

\echo ''
\echo '== division by zero =='
SELECT 1 / 0;

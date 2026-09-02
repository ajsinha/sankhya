-- SANKHYA from SQL: declaring a cube and navigating it.
--
--   psql -h 127.0.0.1 -p 5432 -U you -d sankhya -f 04-cubes.sql
--
-- Creates and drops its own cube. Safe to re-run.

\set ON_ERROR_STOP off
DROP CUBE IF EXISTS sales_by_region;

\echo '== what cubes exist =='
-- Answers with no rows on a warehouse that has none, rather than failing. "None yet" and
-- "this server does not do cubes" are opposite facts with opposite responses.
SELECT * FROM cubes();

\echo ''
\echo '== declaring one =='
-- A measure must say how it composes ALONG each dimension, with no default. That is the whole
-- point: a measure that cannot be derived from its parts -- a ratio, a percentile -- must say
-- so, and the server then refuses to roll it up rather than summing it into a plausible wrong
-- number nobody notices.
CREATE CUBE sales_by_region FROM sales.orders
  DIMENSION region FROM sales.regions ON region (LEVEL area = region)
  MEASURE amount (SUM ALONG region);

\echo ''
\echo '== it is discoverable, so a client can offer a picker =='
SELECT * FROM cubes();
SELECT * FROM cube_dimensions('sales_by_region');
SELECT * FROM cube_measures('sales_by_region');

\echo ''
\echo '== roll up: aggregate a dimension AWAY =='
-- Read `completeness` and `withheld`. A roll-up over a dimension with null members leaves
-- those rows out, and those two columns are how you learn that value is missing from an
-- otherwise plausible total.
-- The second argument is the MEASURE, not the dimension: a cube holds many measures and a
-- cell holds one measure's values, so naming a dimension here asks for cells that do not
-- exist. The dimension to keep is `by=`.
SELECT * FROM cube_rollup('sales_by_region', 'amount', 'by=region');

\echo ''
\echo '== the grand total: every dimension rolled away =='
SELECT * FROM cube_rollup('sales_by_region', 'amount');

\echo ''
\echo '== slice: fix a member and look at the rest =='
-- `where=` is required, spelled `dimension:member`. A slice with nothing fixed is a roll-up,
-- and answering it as one would give the right number to the wrong question.
SELECT * FROM cube_slice('sales_by_region', 'amount', 'where=region:north');

\echo ''
\echo '== and it goes away cleanly =='
DROP CUBE sales_by_region;
SELECT * FROM cubes();

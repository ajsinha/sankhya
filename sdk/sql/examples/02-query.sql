-- SANKHYA from SQL: querying.
--
--   psql -h 127.0.0.1 -p 5432 -U you -d sankhya -f 02-query.sql

\echo '== naming a table =='
-- Both forms work. The qualified name always resolves; the bare one resolves while only one
-- schema holds a table of that name, and stops the day a second one does -- refused, naming
-- both candidates, rather than answering from whichever registered first.
SELECT count(*) AS qualified FROM sales.orders;
SELECT count(*) AS bare      FROM orders;

\echo ''
\echo '== selection and projection =='
SELECT id, region, amount FROM sales.orders WHERE id < 5 ORDER BY id;

\echo ''
\echo '== aggregation =='
SELECT region, count(*) AS orders, sum(amount) AS total
FROM sales.orders
GROUP BY region
ORDER BY region;

\echo ''
\echo '== nulls are not the empty string =='
-- Two different values, and they stay different end to end. Conflating them is a wrong
-- answer, not a formatting choice.
SELECT
  count(*)                                AS rows_total,
  count(region)                           AS region_not_null,
  sum(CASE WHEN region IS NULL THEN 1 ELSE 0 END) AS region_null
FROM sales.orders;

\echo ''
\echo '== a join =='
SELECT o.region, count(*) AS n
FROM sales.orders o
JOIN sales.orders p ON o.region = p.region AND p.id < 10
GROUP BY o.region
ORDER BY o.region;

\echo ''
\echo '== the date axis =='
-- Every table carries `sank_data_date`, of type DATE, and is partitioned on it. That one
-- guaranteed column is what makes partitioning, retention and tiering writable once rather
-- than per table.
SELECT sank_data_date, count(*) FROM sales.orders GROUP BY sank_data_date ORDER BY 1;

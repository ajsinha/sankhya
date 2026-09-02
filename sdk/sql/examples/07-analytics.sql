-- SANKHYA from SQL: the analytical function surface.
--
--   psql -h 127.0.0.1 -p 5432 -U you -d sankhya -f 07-analytics.sql
--
-- Vectors and matrices are ordinary SQL values here, so these compose with everything else.

\echo '== vector construction and arithmetic =='
SELECT vec(1.0, 2.0, 3.0) AS v;
SELECT vec_add(vec(1.0, 2.0), vec(3.0, 4.0)) AS sum;
SELECT vec_dot(vec(1.0, 2.0, 3.0), vec(4.0, 5.0, 6.0)) AS dot;
SELECT vec_norm(vec(3.0, 4.0)) AS norm;

\echo ''
\echo '== distances, which is what a similarity search is made of =='
SELECT vec_l2(vec(0.0, 0.0), vec(3.0, 4.0))       AS euclidean;
SELECT vec_cosine(vec(1.0, 0.0), vec(0.0, 1.0))   AS cosine_orthogonal;

\echo ''
\echo '== matrices =='
SELECT mat(2, 2, 1.0, 2.0, 3.0, 4.0) AS m;
SELECT mat_determinant(mat(2, 2, 1.0, 2.0, 3.0, 4.0)) AS determinant;
SELECT mat_transpose(mat(2, 2, 1.0, 2.0, 3.0, 4.0))   AS transposed;

\echo ''
\echo '== statistics over a real column =='
SELECT
  count(amount) AS n,
  min(amount)   AS smallest,
  max(amount)   AS largest,
  avg(amount)   AS mean,
  sum(amount)   AS total
FROM sales.orders;

\echo ''
\echo '== and they compose with grouping =='
SELECT region, round(avg(amount)::numeric, 2) AS mean_amount
FROM sales.orders
GROUP BY region
ORDER BY region;

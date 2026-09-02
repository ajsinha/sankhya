-- SANKHYA from SQL: the analytical function surface.
--
--   psql -h 127.0.0.1 -p 5432 -U you -d sankhya -f 07-analytics.sql
--
-- Vectors and matrices are ordinary SQL values, so these compose with everything else.
-- A column can be one too: ADR-0005 stores a vector as `FixedSizeList<Float64, N>` where the
-- dimension is known, and a matrix as Arrow's `fixed_shape_tensor` extension.
--
-- Every name below was checked against the registered functions. An earlier version of this
-- file used `vec`, `vec_add`, `vec_norm` and `mat` -- none of which exist -- and it was found
-- by an adversarial review rather than by running it. An example that does not run is
-- documentation that lies, and it lies to the person least able to tell.

\echo '== constructing =='
SELECT vec_of(1.0, 2.0, 3.0) AS v;
SELECT mat_of(2, 2, 1.0, 2.0, 3.0, 4.0) AS m;
SELECT mat_identity(3) AS i;

\echo ''
\echo '== vector algebra =='
SELECT vec_sum(vec_of(1.0, 2.0, 3.0))                        AS sum_of_elements;
SELECT vec_dot(vec_of(1.0, 2.0, 3.0), vec_of(4.0, 5.0, 6.0)) AS dot;
SELECT vec_norm_l2(vec_of(3.0, 4.0))                         AS euclidean_norm;
SELECT vec_norm_l1(vec_of(3.0, -4.0))                        AS manhattan_norm;

\echo ''
\echo '== distance and similarity, which is what a vector search is made of =='
SELECT vec_euclidean(vec_of(0.0, 0.0), vec_of(3.0, 4.0))            AS euclidean;
SELECT vec_cosine_similarity(vec_of(1.0, 0.0), vec_of(0.0, 1.0))    AS orthogonal_similarity;
SELECT vec_cosine_distance(vec_of(1.0, 0.0), vec_of(1.0, 0.0))      AS identical_distance;

\echo ''
\echo '== statistics over a vector, exactly =='
-- Exact, not approximate. `approx_percentile_cont` and its relatives are one autocomplete away
-- from an exact aggregate, and a result that is approximate where exactness was required is a
-- wrong answer nobody can see -- so this engine can be told to refuse them.
SELECT vec_mean(vec_of(1.0, 2.0, 3.0, 4.0))     AS mean;
SELECT vec_median(vec_of(1.0, 2.0, 3.0, 4.0))   AS median;
SELECT vec_stddev(vec_of(1.0, 2.0, 3.0, 4.0))   AS stddev;
SELECT vec_variance(vec_of(1.0, 2.0, 3.0, 4.0)) AS variance;
SELECT vec_skewness(vec_of(1.0, 2.0, 3.0, 9.0)) AS skewness;
SELECT vec_kurtosis(vec_of(1.0, 2.0, 3.0, 9.0)) AS kurtosis;

\echo ''
\echo '== two vectors together =='
SELECT vec_covariance(vec_of(1.0, 2.0, 3.0), vec_of(2.0, 4.0, 6.0))  AS covariance;
SELECT vec_correlation(vec_of(1.0, 2.0, 3.0), vec_of(2.0, 4.0, 6.0)) AS correlation;

\echo ''
\echo '== calculus =='
SELECT vec_integral(vec_of(0.0, 1.0, 2.0, 3.0)) AS trapezoidal_integral;

\echo ''
\echo '== linear algebra =='
SELECT mat_multiply(mat_of(2, 2, 1.0, 2.0, 3.0, 4.0), mat_identity(2)) AS product;
SELECT mat_vec(mat_of(2, 2, 1.0, 2.0, 3.0, 4.0), vec_of(1.0, 1.0))     AS matrix_times_vector;
SELECT mat_transpose(mat_of(2, 2, 1.0, 2.0, 3.0, 4.0))                 AS transposed;
SELECT mat_determinant(mat_of(2, 2, 1.0, 2.0, 3.0, 4.0))               AS determinant;
SELECT mat_trace(mat_of(2, 2, 1.0, 2.0, 3.0, 4.0))                     AS trace;
SELECT mat_inverse(mat_of(2, 2, 4.0, 7.0, 2.0, 6.0))                   AS inverse;
SELECT mat_solve(mat_of(2, 2, 2.0, 1.0, 1.0, 3.0), vec_of(5.0, 10.0))  AS solution;

\echo ''
\echo '== and ordinary aggregates over a real column =='
SELECT
  count(amount) AS n,
  min(amount)   AS smallest,
  max(amount)   AS largest,
  avg(amount)   AS mean,
  sum(amount)   AS total
FROM sales.orders;

\echo ''
\echo '== which compose with grouping =='
SELECT region, round(avg(amount)::numeric, 2) AS mean_amount
FROM sales.orders
GROUP BY region
ORDER BY region;

-- What is deliberately NOT here: QR, SVD and eigendecomposition. They are where an in-house
-- implementation is worse than none -- a subtly wrong SVD produces plausible singular values,
-- and nothing downstream can tell.

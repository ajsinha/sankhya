<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/wordmark-dice-dark.png">
    <img src="assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>

# The function catalogue

**Document ID:** SNK-FUNC-001 · **Milestone:** M21 · **Governed by:** [ADR-0020](adr/0020-the-built-in-function-catalogue.md)

Every function SANKHYA offers, and every function it is going to offer.

The list of *planned* functions is deliberately kept here rather than in the ADR, so that adding
one is an edit to a catalogue and not an amendment to a decision.

## Vectors and matrices are column types, not only expression types

A column can be declared a vector: `FixedSizeList<Float64, n>` on the analytical side,
`float8[]` (or `pgvector`'s `vector(n)`) on the transactional one, decided in
[ADR-0021](adr/0021-vectors-matrices-across-the-tiers.md). A matrix is that same flat array
plus a shape in the column's metadata, so there is no third storage type for capture, the read
path, the backup checker and the sweeper to learn.

**A vector crosses the wire as `float8[]`**, PostgreSQL's own array type, rendered `{1,2.5,3}`
— not as the text `[1.0, 2.5, 3.0]`, which is what it was until 2026-09-02. Every PostgreSQL
driver already decodes `float8[]`, so this is the rare case where doing the correct thing also
deletes code from every client. A client that parsed a rendering would have got it wrong on a
null element, on a locale rendering a decimal comma, and on an empty array against a null one.

**The width is part of the type.** It is what lets a kernel take a contiguous slice rather than
copy per row, and a similarity between a 384-dimensional embedding and a 512-dimensional one is
not a near miss — it is a different question. A row whose vector is the wrong width is refused
rather than widening the column to a variable-length list.

## Where each function is reachable from, today

| Surface | State |
|---|---|
| **OLAP SQL** (`psql`, Flight SQL) | **All of them.** |
| **SDK, as raw SQL** (`db.sql("SELECT norm_inv(0.975)")`) | **All of them.** |
| **SDK, as a named method** (`db.fn.norm_inv(0.975)`) | **All 155** SANKHYA-specific ones |
| **OLTP SQL** | No query path exists yet — but the rule that governs it is built |

### How the wiring works

Both wirings read from **one place**: the server's own catalogue, served as
`SELECT * FROM functions()`. Before it existed, *"which functions does this server have"* was
answerable only by reading Rust — and that single fact caused both gaps. A binding cannot
generate what it cannot enumerate, and a router cannot classify a statement without a list of
what the built-ins are.

**The SDK** reads that catalogue once per connection and offers exactly what came back:

```python
db.fn.norm_inv(0.975)                                   # 1.9599639845400367
db.fn.mat_cholesky([4, 12, -16, 12, 37, -43, -16, -43, 98])
db.fn.regress_stderr(x, y)
db.functions(category="linear algebra")                 # the catalogue, as records
```

No stub is written per function. A hundred and twenty-eight of them would be two thousand lines
whose only job is to agree with the server, and they would stop agreeing the first time one was
added and a stub was not — silently, because a missing method is not an error until somebody
calls it. A function added to the server appears in the binding with no change to the binding.

It stays thin. It validates nothing — not the argument count, not the domains — so a wrong call
produces the server's own refusal, which is the one that knows why. What it adds is *encoding*:
a Python list becomes a SQL array going out and a list coming back. A **typo in a name** is the
binding's to catch, because that one it can answer without a round trip.

**The transactional tier** has no query path yet, so the honest answer for every function is the
same: none, because nothing routes there. What is built is the rule that will govern it when it
does — `tier_for(sql, catalogue)`, which reports that a statement naming a built-in is an
analytical statement.

That is built **now, before the router**, because it is unaffordable to retrofit: by the time
both tiers answer queries, the second implementation of every function is already written, and
the day two implementations disagree the answer depends on a routing decision nobody can see.
It is also the half that can be checked without a cluster — whether a statement names a built-in
is a property of its text, and it is the half that will be wrong. `erfc(x)` must not read as a
call to `erf`; a column called `erf` must not route a point lookup down the slow path;
`norm_cdf (x)` with a space must still be a call.

The match is deliberately **eager**: a string literal that reads like a call routes analytically,
and nothing about the answer changes — one query takes the slower path. The other direction is
not survivable, because missing a call means a refusal for a function the catalogue says exists.

## The two rules this list obeys

**A function is not delivered until a binding can call it.** A function on the SQL surface and
absent from the SDKs is half-shipped, and the missing half is the one most users have. Every row
below that is marked shipped is reachable from `psql` *and* from the Python SDK, with a runnable
example that is gated as a test.

**A function works on every query, whoever runs it.** A user does not know whether the
transactional tier or the analytical one answered, and never needs to: a statement calling a
built-in is routed to the tier that has it. There is no statement that is refused, or answered
differently, because of a routing decision nobody can see.

---

# Part 1 — What ships today

**347 functions.** 192 from the query engine, 155 written for SANKHYA.

## 1.1 Vectors and per-row series — SANKHYA

A vector is one row's series: an embedding, a window of readings, a term structure, a yield
curve. These describe **one row**, where SQL's aggregates describe a column. `stddev(x)` is the
spread of a column; `vec_stddev(v)` is the spread inside a single row's vector.

| Function | What it gives |
|---|---|
| `vec_of(...)` | Build a vector from scalars |
| `vec_sum` · `vec_mean` · `vec_median` | Total, mean, median of one row's series |
| `vec_min` · `vec_max` · `vec_range` | The ends, and the spread between them |
| `vec_variance` · `vec_stddev` | Sample forms (divide by *n−1*) |
| `vec_variance_pop` · `vec_stddev_pop` | Population forms (divide by *n*) |
| `vec_skewness` · `vec_kurtosis` | Third and fourth moments; kurtosis is excess |
| `vec_covariance` · `vec_covariance_pop` · `vec_correlation` | Between two vectors |
| `vec_regression_slope` · `vec_regression_intercept` · `vec_regression_r2` | Least-squares fit of one vector on another |
| `vec_dot` | Dot product |
| `vec_norm_l1` · `vec_norm_l2` | Manhattan and Euclidean norms |
| `vec_euclidean` | Distance between two vectors |
| `vec_cosine_similarity` · `vec_cosine_distance` | Angle-based similarity, for embeddings |
| `vec_integral` · `vec_integral_simpson` | Area under a sampled curve, trapezoid and Simpson |
| `vec_quantile` | Any quantile of one row's series, by linear interpolation |
| `vec_add` · `vec_subtract` · `vec_multiply` · `vec_divide` · `vec_scale` | Element-wise, between two series or a series and a number |
| `vec_differences` · `vec_derivative` · `vec_second_derivative` | Rate of change across a series |
| `vec_cumulative_sum` · `vec_cumulative_integral` | Running total, running area |
| `vec_standardise` | Centre and scale to unit variance, so two series in different units compare |

> **Why both a sample and a population form.** Which divisor a variance uses is a statement
> about what the data *is*, not a preference. A sample variance of a complete population
> overstates the spread, and the difference is invisible in the number — so both are named and
> you choose the one you mean.

> **Why Simpson sits beside the trapezoid rather than replacing it.** Simpson's rule is exact
> for a cubic where the trapezoid is exact only for a line, but it needs an even number of
> intervals and refuses otherwise. A function that silently changed rule to accommodate its
> input would return two different approximations under one name.

## 1.2 Matrices — SANKHYA

| Function | What it gives |
|---|---|
| `mat_of(rows, cols, ...)` | Build a matrix |
| `mat_identity(n)` | The identity |
| `mat_multiply` · `mat_vec` | Matrix–matrix and matrix–vector products |
| `mat_transpose` · `mat_trace` | Transpose, and the sum of the diagonal |
| `mat_determinant` · `mat_inverse` | Determinant and inverse |
| `mat_solve` | Solve *Ax = b* |

## 1.3 Cubes — SANKHYA

| Function | What it gives |
|---|---|
| `cubes()` | Every declared cube |
| `cube_dimensions(cube)` · `cube_measures(cube)` | A cube's structure, so a client can offer a picker |
| `cube_rollup(cube, measure, 'by=…')` | Aggregate a dimension away |
| `cube_slice(cube, measure, 'where=dim:member')` | Fix one member, look at the rest |

Both navigations return `completeness` and `withheld`. **Read them:** a roll-up over a dimension
with null members leaves those rows out, and those two columns are how you learn that a third of
the value is missing from an otherwise plausible total.

## 1.4 Graph — SANKHYA

| Function | What it gives |
|---|---|
| `graph_reachable(graph, from, …)` | What is reachable |
| `graph_shortest_path(graph, from, to, …)` | The cheapest route, and the *k* cheapest loopless ones |
| `graph_time_respecting(graph, from, …)` | Routes whose edges existed **in the order you traverse them** |
| `graph_cycles(graph, …)` | Cycles |
| `graph_influence(graph, from, …)` | Influence from a node |

`graph_time_respecting` is the one a plain reachability query cannot express: a route through a
graph is only a route if its edges existed in the order you walk them.

## 1.11 Distributions and special functions — SANKHYA

Four suffixes, the same four everywhere, so a caller who has met one family can guess the rest.

| Family | Functions |
|---|---|
| Normal | `norm_pdf` · `norm_cdf` · `norm_sf` · `norm_inv` · `normal_pdf` · `normal_cdf` · `normal_inv` |
| Lognormal | `lognorm_cdf` · `lognorm_inv` |
| Student's *t* | `t_pdf` · `t_cdf` · `t_sf` · `t_inv` · `t_two_sided` |
| Chi-squared | `chisq_cdf` · `chisq_sf` · `chisq_inv` |
| *F* | `f_cdf` · `f_sf` · `f_inv` |
| Binomial, Poisson | `binom_pmf` · `binom_cdf` · `poisson_pmf` · `poisson_cdf` |
| Exponential, gamma, beta, uniform | `expon_cdf` · `gamma_cdf` · `beta_cdf` · `uniform_cdf` |
| Special functions | `erf` · `erfc` · `gammaln` · `gamma_p` · `gamma_q` · `beta_i` |

> **`sf` is not `1 - cdf`.** A p-value of `1e-20` subtracted from one is zero, and a test
> reporting `p = 0` where the truth is `1e-20` has thrown away the only digits anybody was
> going to read. Every family with a tail worth asking for offers it directly.

> **A count must be whole.** `binom_pmf(2.7, 10, 0.5)` is refused rather than rounded —
> rounding answers a question about a different number of events, silently.

Accuracy: the cumulatives iterate to `3e-16`, so they are good to the last few bits. `erf` and
`erfc` come from the incomplete gamma rather than a rational fit, because the usual fit gives
`erf(0) = -5.9e-8` — a standard normal whose median is not zero.

## 1.12 Linear algebra — SANKHYA

| Function | What it gives |
|---|---|
| `mat_cholesky` | The factor `L` with `L·Lᵀ = A`, flat and lower-triangular |
| `mat_eigenvalues` · `mat_eigenvectors` | Symmetric eigendecomposition, eigenvalues descending |
| `mat_singular_values` | Singular values, descending |
| `mat_qr_q` · `mat_qr_r` | The QR factors, each returned flat |
| `mat_is_symmetric` · `mat_is_positive_definite` · `mat_is_square` | Questions answered `1` or `0` |

> **A matrix's shape, and when it is deduced.** A function defined *only* on a square matrix —
> a determinant, a trace, an inverse, a Cholesky, an eigendecomposition — takes its order from
> the array's length, because any other shape is not that function's argument and there is
> nothing to guess wrong. A function defined on a **rectangular** matrix — a transpose, a
> multiply — requires a shape declared by `mat_of`, because sixteen values are a 4×4 or a 2×8
> and the wrong one produces numbers from values that were never in the same row. A declared
> shape always wins where there is one.

> **A Cholesky failure is the useful part.** It succeeds exactly on the positive-definite
> matrices, so a covariance matrix that will not factor is not a numerical accident — it is one
> no data could have produced, usually a correlation somebody interpolated by hand.

> **Jacobi rather than the faster algorithms.** A shifted QR iteration is several times faster
> and picks its pivots from the current iterate, so a matrix perturbed in its last bit can
> converge to eigenvalues differing in their last several. An eigenvalue is a figure, and a
> figure that moves when the machine is busier is the thing this system is arranged against.

## 1.13 Inference and regression — SANKHYA

| Function | What it gives |
|---|---|
| `ttest_1samp_t` · `_p` · `_df` | One sample against a hypothesised mean |
| `ttest_2samp_t` · `_p` · `_df` | Two samples, by **Welch's** test |
| `ttest_paired_t` · `_p` | Paired observations |
| `chisq_test_t` · `_p` | Pearson goodness of fit |
| `f_test_t` · `_p` | Two variances compared, two-sided |
| `jarque_bera_t` · `_p` | Normality, from skewness and kurtosis |
| `regress_slope` · `_intercept` · `_stderr` · `_tstat` · `_pvalue` · `_r2` · `_adj_r2` · `_residual_error` | Simple regression, one part per name |
| `regress_multiple_r2` · `_f_p` | Multiple least squares over a flat design matrix |
| `regress_ridge_norm` | Ridge, whose coefficients carry **no** *t*-statistic — see below |
| `ttest_df_welch` | Welch's degrees of freedom from summary statistics |

> **Welch's, not Student's pooled test.** The pooled form assumes the two populations share a
> variance, and when they do not it rejects too often — it finds differences that are not
> there. Welch is correct either way, so there is no case where the pooled form is the better
> default and a caller has to know which they have.

> **A slope with no standard error is a number nobody can act on.** Every regression names its
> uncertainty beside its estimate. Ridge is the exception and deliberately so: the penalty
> invalidates the unpenalised standard errors, so offering a significance test beside a shrunk
> coefficient would invite one the arithmetic does not support.

> **Adjusted `R²` is reported beside `R²`** because plain `R²` never falls when a predictor is
> added — including a predictor of pure noise — so comparing two models by it always prefers
> the larger one.

The solve goes through QR rather than the normal equations. Forming `XᵀX` squares the condition
number: a design at `1e8` — ordinary when one predictor is a level and another is its square —
becomes `1e16`, which a double cannot resolve, and the coefficients come back looking plausible
and are noise.

## 1.5 Aggregates — engine

`sum` · `avg` · `mean` · `count` · `min` · `max` · `median` · `any_value` · `first_value` ·
`last_value` · `nth_value` · `array_agg` · `string_agg` · `grouping`

**Spread and shape:** `stddev` · `stddev_pop` · `stddev_samp` · `var` · `var_pop` · `var_samp` ·
`var_population` · `var_sample` · `covar` · `covar_pop` · `covar_samp` · `corr`

**Regression:** `regr_slope` · `regr_intercept` · `regr_r2` · `regr_count` · `regr_avgx` ·
`regr_avgy` · `regr_sxx` · `regr_syy` · `regr_sxy`

**Quantiles:** `percentile_cont` · `quantile_cont` · `approx_median` · `approx_percentile_cont` ·
`approx_percentile_cont_with_weight` · `approx_distinct`

**Boolean and bitwise:** `bool_and` · `bool_or` · `bit_and` · `bit_or` · `bit_xor`

> Every `approx_*` name is a promise about *exactness*, not speed. Approximation here is
> declared and visible, never chosen for you.

## 1.6 Window — engine

`row_number` · `rank` · `dense_rank` · `percent_rank` · `cume_dist` · `ntile` · `lag` · `lead` ·
`first_value` · `last_value` · `nth_value`

## 1.7 Scalar mathematics — engine

**Arithmetic:** `abs` · `ceil` · `floor` · `round` · `trunc` · `signum` · `factorial` · `gcd` ·
`lcm` · `pow` · `power` · `sqrt` · `cbrt` · `exp` · `ln` · `log` · `log2` · `log10` · `pi` ·
`nanvl` · `isnan` · `iszero` · `random` · `rand`

**Trigonometry:** `sin` · `cos` · `tan` · `cot` · `asin` · `acos` · `atan` · `atan2` · `sinh` ·
`cosh` · `tanh` · `asinh` · `acosh` · `atanh` · `degrees` · `radians`

## 1.8 Text — engine

`length` · `char_length` · `character_length` · `bit_length` · `octet_length` · `lower` ·
`upper` · `initcap` · `trim` · `btrim` · `ltrim` · `rtrim` · `lpad` · `rpad` · `left` · `right` ·
`substr` · `substring` · `substr_index` · `substring_index` · `split_part` · `concat` ·
`concat_ws` · `repeat` · `replace` · `translate` · `overlay` · `reverse` · `starts_with` ·
`ends_with` · `contains` · `position` · `strpos` · `instr` · `find_in_set` · `levenshtein` ·
`ascii` · `chr` · `to_hex` · `encode` · `decode` · `uuid`

**Regular expressions:** `regexp_match` · `regexp_like` · `regexp_replace` · `regexp_count` ·
`regexp_instr`

## 1.9 Dates and times — engine

`now` · `today` · `current_date` · `current_time` · `current_timestamp` · `make_date` ·
`make_time` · `date_part` · `datepart` · `date_trunc` · `datetrunc` · `date_bin` ·
`date_format` · `to_date` · `to_time` · `to_char` · `to_timestamp` · `to_timestamp_seconds` ·
`to_timestamp_millis` · `to_timestamp_micros` · `to_timestamp_nanos` · `from_unixtime` ·
`to_unixtime` · `to_local_time`

## 1.10 Conditionals, structs and types — engine

`coalesce` · `nullif` · `ifnull` · `nvl` · `nvl2` · `greatest` · `least` · `named_struct` ·
`struct` · `row` · `get_field` · `union_extract` · `union_tag` · `arrow_cast` ·
`arrow_try_cast` · `arrow_typeof` · `arrow_field` · `arrow_metadata` · `with_metadata` ·
`cast_to_type` · `try_cast_to_type` · `version` · `input_file_name` · `file_row_index`

---

# Part 2 — What is coming

Ordered by what is closest. Everything here is **M21** unless marked otherwise.

## 2.1 The kernels that had no name — closed 2026-09-02

Twelve kernels existed in `sankhya-math`, unit-tested and mutation-tested, with **no SQL name**.
Every check in the repository passed the whole time, because every check looked at the code
rather than at the surface.

All twelve now have names, and `check-kernels` fails the build for the next one:

`vec_add` · `vec_subtract` · `vec_multiply` · `vec_divide` · `vec_scale` · `vec_differences` ·
`vec_derivative` · `vec_second_derivative` · `vec_cumulative_sum` · `vec_cumulative_integral` ·
`vec_standardise` · `vec_quantile`

Eleven of them return a **vector** rather than a number, which is why they were missed: the
wrapper every other vector function used returns one value. The series wrapper returns a
`List<Float64>` rather than a `FixedSizeList`, because these kernels change the width — a first
difference of *n* values has *n−1* — and a fixed width would make `vec_differences` of a
384-dimensional embedding a different function from `vec_differences` of a 3-dimensional one.

## 2.2 Linear algebra

**Decompositions:** `mat_lu` · `mat_qr` · `mat_cholesky` · `mat_svd` · `mat_eigenvalues` ·
`mat_eigenvectors`

**Properties:** `mat_rank` · `mat_condition_number` · `mat_norm_frobenius` · `mat_norm_l1` ·
`mat_norm_inf` · `mat_is_symmetric` · `mat_is_positive_definite`

**Operations:** `mat_pseudo_inverse` · `mat_least_squares` · `mat_kronecker` · `mat_power` ·
`mat_exp` · `mat_diagonal` · `mat_submatrix` · `mat_concat_rows` · `mat_concat_cols`

Cholesky and eigen-decomposition are what a covariance matrix needs, which is what a risk
calculation needs — this is the group with the clearest demand.

## 2.3 Statistics

**Distributions — shipped 2026-09-02.** Moved to Part 1; see §1.11.

**Hypothesis tests — shipped 2026-09-02**, except `ks_test`, `shapiro_wilk`, `ljung_box` and
`adf_test`. See §1.13.

**Rank and association:** `spearman` · `kendall_tau` · `mutual_information` · `cramers_v`

**Regression beyond simple — partly shipped 2026-09-02.** Multiple least squares and ridge
are in §1.13; `regress_lasso`, `regress_logistic` and `regress_quantile` remain.

**Robust statistics:** `mad` · `trimmed_mean` · `winsorised_mean` · `iqr` · `huber_mean`

**Sampling and resampling:** `bootstrap_ci` · `jackknife` · `permutation_test`

## 2.4 Calculus and numerical methods

**Interpolation:** `interp_linear` · `interp_cubic` · `interp_spline` · `interp_pchip` ·
`interp_akima`

**Root finding and optimisation:** `root_bisect` · `root_newton` · `root_brent` ·
`minimise_golden` · `minimise_nelder_mead`

**Integration:** `integrate_romberg` · `integrate_gauss_legendre` · `integrate_adaptive`

**Smoothing and filtering:** `smooth_moving_average` · `smooth_exponential` ·
`smooth_savitzky_golay` · `smooth_loess` · `filter_hodrick_prescott` · `filter_kalman`

**Transforms:** `fft` · `ifft` · `dct` · `wavelet_haar` · `autocorrelation` ·
`partial_autocorrelation` · `cross_correlation` · `convolve`

## 1.14 Time series, finance and risk — SANKHYA

| Function | What it gives |
|---|---|
| `ts_rolling_mean` · `_std` · `_min` · `_max` | Rolling statistics over a window |
| `ts_ewma` | Exponentially weighted moving average |
| `ts_returns` · `ts_log_returns` | Simple and logarithmic period returns |
| `ts_drawdown` · `ts_max_drawdown` | Fall from the running peak, and the worst of them |
| `ts_cumulative_return` | Period returns **compounded**, which is not their sum |
| `ts_autocorrelation` | Correlation of a series with itself at a lag |
| `npv` · `npv_from_now` · `irr` | Net present value under either convention, and the rate that zeroes it |
| `pv` · `fv` · `pmt` | Annuity present value, future value and level payment |
| `sln` · `syd` | Straight-line and sum-of-years depreciation |
| `var_historical` · `expected_shortfall` | Value-at-risk, and the mean of what lies beyond it |
| `sharpe` · `sortino` | Excess return per unit of total, and of downside, deviation |
| `black_scholes_call` · `_put` · `greeks_delta` · `greeks_vega` | European option prices and two sensitivities |

> **A rolling window reports nothing where it does not reach.** The leading positions come back
> as **nulls inside the array**, not zeros. Zero is a number somebody acts on; the series mean
> pretends to information that is not there; repeating the first value makes a flat start that
> reads as low volatility.

> **A value-at-risk is negative for a loss**, because the outcomes are. It is not flipped to a
> positive "amount at risk" — one quoted positive gets added to a profit somewhere, and the sign
> is the only thing between a report and a number twice as wrong as it looks.

> **Both discounting conventions are named**, rather than selected by a flag. `npv` discounts
> from period one as a spreadsheet does; `npv_from_now` leaves the first flow undiscounted. A
> boolean deciding which of two definitions applies is a boolean somebody passes wrongly, and
> the result is plausible.

> **An internal rate of return refuses more than it converges.** A cash flow of one sign has no
> rate at which its value is zero, and one with several sign changes has several — all correct,
> none of them *the* answer.

## 2.5 Time series

**Shipped 2026-09-02**: see §1.14. Remaining: `ts_lag` · `ts_lead` · `ts_diff` ·
`ts_pct_change` · `ts_rolling_quantile` · `ts_rolling_corr` · `ts_ewmstd` · `ts_resample` ·
`ts_seasonal_decompose` · `ts_detrend` · `ts_stl`

**Forecasting:** `ts_arima` · `ts_holt_winters` · `ts_theta`

## 2.6 Financial

**Money over time:** `npv` · `xnpv` · `irr` · `xirr` · `mirr` · `pv` · `fv` · `pmt` · `ipmt` ·
`ppmt` · `nper` · `rate` · `cumipmt` · `cumprinc`

**Depreciation:** `sln` · `syd` · `db` · `ddb` · `vdb`

**Fixed income:** `price` · `yield` · `duration` · `mduration` · `convexity` · `accrint` ·
`coupdays` · `coupncd` · `couppcd` · `yieldmat` · `disc` · `intrate`

**Risk:** `var_historical` · `var_parametric` · `var_cornish_fisher` · `expected_shortfall` ·
`sharpe` · `sortino` · `information_ratio` · `beta` · `tracking_error` · `omega_ratio`

**Options:** `black_scholes` · `black_76` · `implied_volatility` · `greeks_delta` ·
`greeks_gamma` · `greeks_vega` · `greeks_theta` · `greeks_rho` · `binomial_tree`

## 2.7 Excel-compatible

The group with the most users and the least tolerance for approximation, because the person
checking has the spreadsheet open beside them.

**Lookup:** `xl_vlookup` · `xl_hlookup` · `xl_xlookup` · `xl_index` · `xl_match` · `xl_choose` ·
`xl_offset`

**Logical and text:** `xl_iferror` · `xl_ifs` · `xl_switch` · `xl_textjoin` · `xl_text` ·
`xl_value` · `xl_proper` · `xl_clean` · `xl_substitute` · `xl_rept` · `xl_dollar`

**Statistical:** `xl_averageif` · `xl_averageifs` · `xl_countif` · `xl_countifs` · `xl_sumif` ·
`xl_sumifs` · `xl_sumproduct` · `xl_rank` · `xl_percentile` · `xl_quartile` · `xl_large` ·
`xl_small` · `xl_frequency` · `xl_forecast` · `xl_trend` · `xl_linest` · `xl_growth`

**Dates:** `xl_edate` · `xl_eomonth` · `xl_networkdays` · `xl_workday` · `xl_yearfrac` ·
`xl_datedif` · `xl_weeknum`

**Engineering:** `xl_convert` · `xl_bin2dec` · `xl_dec2hex` · `xl_bitand` · `xl_complex` ·
`xl_imabs` · `xl_besseli` · `xl_erf` · `xl_gammaln`

> **The rule these obey.** A function named after an Excel one must **agree with Excel**,
> including where Excel is arguably wrong: `IRR`'s iteration and starting guess, `NPV`
> discounting from period one rather than zero, the 1900 leap-year bug in date serials. A
> function that is *nearly* `XIRR` and is called `XIRR` is worse than one called something
> else, because the disagreement is found by somebody reconciling to four decimal places at a
> month-end.
>
> Where agreement is not achievable the function takes a different name and says why. Named
> differently is honest; named identically and subtly different is not.

## 2.8 Vectors and arrays, widened

`vec_normalise` · `vec_clip` · `vec_softmax` · `vec_argmin` · `vec_argmax` · `vec_sort` ·
`vec_reverse` · `vec_slice` · `vec_concat` · `vec_zip` · `vec_map_add` · `vec_map_multiply` ·
`vec_elementwise_add` · `vec_elementwise_multiply` · `vec_hamming` · `vec_jaccard` ·
`vec_manhattan` · `vec_minkowski` · `vec_chebyshev` · `vec_mahalanobis` · `vec_quantile` ·
`vec_mode` · `vec_entropy` · `vec_gini`

## 2.9 Graph, widened

`graph_centrality_degree` · `graph_centrality_betweenness` · `graph_centrality_closeness` ·
`graph_pagerank` · `graph_components` · `graph_topological_order` · `graph_min_spanning_tree` ·
`graph_max_flow` · `graph_communities` · `graph_triangles` · `graph_clustering_coefficient` ·
`graph_diameter` · `graph_neighbours` · `graph_subgraph`

## 2.10 Discovery

`functions()` — every function this server offers, with name, category, arity, argument types,
return type and a one-line description.

It exists for the reason `cubes()` exists: **a capability nobody can enumerate is a reference
manual nobody reads**, and a client that cannot list the catalogue cannot offer it. Today there
is no way to ask a running SANKHYA what functions it has — this document had to be produced by
dumping a session from a test.

---

## How this list is kept honest

Every function marked shipped in Part 1 was read out of a live session, not written from memory.
Part 2 is a plan and is marked as one.

The gate that keeps it true is the same one that keeps the examples true: each category ships
with runnable examples in every SDK, executed against a real server on every build. **An example
that does not run is documentation that lies**, and a catalogue is a very large example.

---

<p align="center"><sub>SANKHYA — to count is to make completely known.</sub></p>

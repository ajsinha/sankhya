# ============================================================ 4 · DETERMINISM
chapter("22", "One catalogue, on every path")

sl, top = content("The problem, in four numbers",
                  kicker="FOUNDATIONS · WHY IT MATTERS")
code(sl, ML, top, CW * 0.52, [
    "SELECT sum(amount) FROM postings;",
    "",
    "# the same four values, three arrival orders",
    "  1e16,  1, -1e16,  1   ->   ?",
    " -1e16,  1,  1e16,  1   ->   ?",
    "  1,  1e16,  1, -1e16   ->   ?",
], fs=10, title="the question")
h = table(sl, [
    ["Engine", "Answer"],
    ["Ordinary floating-point sum, order A", "0"],
    ["Ordinary floating-point sum, order B", "2"],
    ["Ordinary floating-point sum, order C", "1"],
    ["SANKHYA, any order", "**18**"],
], ML + CW * 0.55, top, CW * 0.45, col_w=[3.1, 2.1])

y = top + max(h, 1.8) + 0.3
tf = txt(sl, ML, y, CW, 1.6)
runs(tf, [("Eighteen is the correct answer, and the first three are all floating-point "
           "addition working exactly as specified. ", INK, True),
          ("Addition is not associative in binary floating point, so a total depends on the "
           "order the rows arrived in — which depends on which partition finished first, "
           "which depends on how busy the machine was. A warehouse whose totals move when the "
           "machine is busier is not one anybody can reconcile against.",
           SLATE, False)], size=12, line=1.3)

sl, top = content("How, and what it costs",
                  kicker="FOUNDATIONS · THE MECHANISM AND THE PRICE")
tf = txt(sl, ML, top, CW, 1.2)
para(tf, "The values are scaled to a common power of two and accumulated in a 127-bit "
         "integer. Integer addition is associative, so the order cannot matter — this is "
         "order-independence by construction rather than by sorting. Where no common scale "
         "exists, the reduction declines and falls back to an exact expansion over a "
         "canonical order; it never approximates.",
     size=12, color=INK, first=True, line=1.3)
y = top + 1.2
h = table(sl, [
    ["Values", "`iter().sum()`", "`deterministic_sum`", "Price"],
    ["8", "1.30 ns", "74.1 ns", "**57×**"],
    ["64", "14.5 ns", "508 ns", "**35×**"],
    ["512", "552 ns", "5.89 µs", "**10.7×**"],
    ["4,096", "4.45 µs", "54.4 µs", "**12.2×**"],
], ML, y, CW * 0.62, col_w=[1.5, 2.0, 2.2, 1.5])
tf = txt(sl, ML + CW * 0.65, y, CW * 0.35, 2.4)
para(tf, "Measured, not asserted", size=11, color=DEEP, bold=True, first=True, space_after=6)
para(tf, "`crates/sankhya-math/benches/reduce.rs`, criterion medians of a hundred samples, "
         "AMD Ryzen AI 9 HX 370, rustc 1.97.1, thin LTO.",
     size=9.5, color=SLATE, line=1.25, space_after=6)
para(tf, "The narrow case is the expensive one, and narrow is the common one here — a window "
         "of readings, a short curve. An i128 accumulator cannot be autovectorised.",
     size=9.5, color=SLATE, line=1.25)
tf = txt(sl, ML, y + h + 0.26, CW, 0.8)
runs(tf, [("This table did not exist until an audit asked for it. ", DEEP, True),
          ("What was published was a ratio against this project's own previous code, so a "
           "reader concluded the kernels were fast. They were faster than they had been.",
           SLATE, False)], size=11, line=1.28)

sl, top = content("And what the guarantee does not cover",
                  kicker="FOUNDATIONS · THE BOUNDARY")
h = table(sl, [
    ["Kernel family", "Bit-reproducible", "Compensated"],
    ["`vec_dot`, `sum`, `mean`, `norm_*`, `euclidean`", "yes", "**yes**"],
    ["`mat_multiply`, `mat_trace`", "yes", "**yes**"],
    ["statistics, calculus, regression, time series, finance", "yes", "**yes**"],
    ["LU — `mat_determinant`, `mat_solve`, `mat_inverse`", "yes", "no"],
    ["`mat_cholesky`, QR, eigen, singular values", "yes", "no"],
    ["`gamma_p`, `gamma_q`, `beta_i` and what builds on them", "yes", "no"],
], ML, top, CW, col_w=[6.2, 2.7, 2.7])
tf = txt(sl, ML, top + h + 0.26, CW, 1.5)
runs(tf, [("Every kernel is reproducible run to run; not every kernel is compensated. ", INK, True),
          ("The decomposition family accumulates with ordinary addition in a fixed order, so "
           "two runs agree and a longer computation still loses precision the compensated "
           "path would have kept. That distinction was documented as one property until a "
           "reviewer read the code — and the decomposition family is the one a risk "
           "calculation actually uses.",
           SLATE, False)], size=11.5, line=1.3)

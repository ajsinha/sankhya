# ------------------------------------------------------------ CH 14
chapter("14", "Cuboids")

sl, top = content("A cache that cannot be stale, because the key says so",
                  kicker="CUBOIDS · THE KEY IS THE WHOLE STORY")
h = table(sl, [
    ["The key includes", "What it defends against"],
    ["the **definition**", "an edited cube. A fingerprint of validated content --- there is no field to forget to bump, and reformatting the file changes nothing"],
    ["the **snapshot**", "a new commit. Files are immutable, so a commit produces a **miss**, never a stale hit"],
    ["the **scope**", "disclosure through arithmetic. Two entitlement sets are two *tables* --- separate files, separate names"],
    ["the **measure**", "one cuboid answering for two measures. Without it the first measure wrote each shape and every later read was labelled with whatever came back"],
    ["the **cuboid shape**", "answering the wrong grain"],
], ML, top, CW, col_w=[2.1, 8.5], fs=9.5, hfs=10)
tf = txt(sl, ML, top + h + 0.24, CW, 1.6)
runs(tf, [("There is no invalidation protocol, no TTL to tune, and no window in "
           "which something old is served. ", CRIMSON, True),
          ("That is what makes automatic selection safe: being wrong about what to cache "
           "costs latency, never correctness. A cuboid past its staleness target is not "
           "served as though it were fresh --- the answer falls back to live aggregation and "
           "says `materialised = false` on the row.", SLATE, False)],
     size=11.5, first=True, space_after=10, line=1.28)
runs(tf, [("And a cuboid is an ordinary published table. ", DEEP, True),
          ("The fast path gets no exception from the open-format rule --- it is written by "
           "maintenance, through the same publish path as everything else, and an external "
           "reader can read it.", SLATE, False)], size=11.5, space_after=0, line=1.28)

sl, top = content("The invariant, and what it cost to hold",
                  kicker="CUBOIDS · IDENTICAL, NOT CLOSE")
tf = txt(sl, ML, top, CW, 1.15)
runs(tf, [("Every query returns bit-identical results with materialisation on and "
           "off. ", CRIMSON, True),
          ("Not close --- identical. Materialisation changes **where** an answer is computed, "
           "never what it is. It was not free: a cube rolls up in stages and every stage "
           "rounds, so rounding a sum of rounded partials is not rounding the whole sum. "
           "Fixing the summation *order* does nothing about **associativity**, and a stored "
           "cuboid is precisely a re-association.", SLATE, False)],
     size=11.5, first=True, space_after=0, line=1.28)
note(sl, ML, top + 1.30, CW, 1.0,
     "The measured difference was one unit in the last place. ",
     "Which is the worst possible size --- large enough for two reports to disagree by a "
     "penny, small enough that nobody can point at a defect. A sum is therefore stored "
     "**unrounded**, as an exact expansion, and rounded once when it is read.")
h = table(sl, [
    ["The test", "What it pins"],
    ["`a_session_may_ask_for_the_base_data_and_get_the_same_answer`", "the invariant itself --- it compares the raw bits of the two paths"],
    ["`a_session_that_asks_for_the_base_data_is_not_served_a_cuboid`", "that the test above is not trivial: it deletes the fact files, so the base path must **fail** rather than be quietly handed the cuboid"],
    ["`a_maintained_measure_answers_its_rule_and_not_the_sum`", "the same cube declared both ways, compared for MAX, MIN and MEAN"],
], ML, top + 2.45, CW, col_w=[4.5, 6.1], fs=9, hfs=9.5)
tf = txt(sl, ML, top + 2.45 + h + 0.20, CW, 0.6)
runs(tf, [("The habit to teach: ", DEEP, True),
          ("run any figure you are going to defend twice, once with materialisation off. If "
           "they differ you have found a defect, not a tuning question.", SLATE, False)],
     size=11, first=True, space_after=0, line=1.26)

# ------------------------------------------------------------ CH 15
chapter("15", "Cubes you define yourself")

sl, top = content("A merge function of your own, behind a boundary built first",
                  kicker="EXTENSION · THE SURFACE")
code(sl, ML, top, CW * 0.52, [
    "CREATE AGGREGATION rms LANGUAGE PYTHON AS $$",
    "def merge(partials):",
    "    total = sum(p * p for p in partials)",
    "    return (total / len(partials)) ** 0.5",
    "$$;",
    "",
    "CREATE CUBE risk_q FROM risk.positions",
    "  DIMENSION book FROM risk.positions ON book",
    "            (LEVEL desk = book)",
    "  MEASURE exposure (AGGREGATION rms ALONG book);",
], fs=9, title="a measure whose rule along one dimension is yours")
tf = txt(sl, ML + CW * 0.56, top, CW * 0.44, 3.4)
para(tf, "The order the work was done in is the point.", size=12, color=DEEP,
     bold=True, first=True, space_after=8)
bullets(tf, [
    "The sandbox a user-supplied function runs in was **decided and built "
    "before** the surface that uses it --- namespaces, mounts and resource "
    "limits applied between fork and exec.",
    "Accepting such a statement is off unless an operator turns it on. The safe "
    "posture is the one you get by not thinking.",
    "A named aggregation must exist on **this** server at the moment the cube is "
    "declared --- a cube referring to a function nobody has loaded is refused at "
    "declaration rather than at the first query.",
    "A rule with no reduction from partials is **not materialised at all**, "
    "rather than written as a zero. Materialisation must not turn a refusal into "
    "a number.",
], size=10, gap=7)

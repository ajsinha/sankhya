# ------------------------------------------------------------ CH 12
chapter("12", "A worked example")

sl, top = content("The fact table, and the cube declared over it",
                  kicker="WORKED · THE DECLARATION")
code(sl, ML, top, CW * 0.52, [
    "CREATE CUBE sales_q FROM sales.orders",
    "  DIMENSION region FROM sales.regions ON region",
    "            (LEVEL area = region)",
    "  DIMENSION period FROM sales.orders  ON period",
    "            (LEVEL quarter = period)",
    "  MEASURE amount     (SUM  ALONG region,",
    "                      SUM  ALONG period)",
    "  MEASURE margin_pct (NONE ALONG region,",
    "                      NONE ALONG period);",
], fs=9.5, title="runnable against the quickstart warehouse")
tf = txt(sl, ML + CW * 0.56, top, CW * 0.44, 3.3)
para(tf, "Read it as three sentences.", size=12, color=DEEP, bold=True,
     first=True, space_after=8)
bullets(tf, [
    "The facts are in `sales.orders`.",
    "`region` takes its members from the fact table's `region` column, and is "
    "authorized against `sales.regions`.",
    "`amount` adds along both axes. `margin_pct` is a ratio and composes along "
    "**nothing** --- declared, not discovered.",
], size=11, gap=8)
para(tf, "Clauses are not comma-separated; the rules inside a MEASURE's "
         "parentheses are. There is no CREATE OR REPLACE CUBE --- replacing a cube "
         "retires everything it materialised, and that should not happen because "
         "somebody re-ran a script.",
     size=10, color=MUTED, line=1.3, space_after=0)

y = top + 3.45
tf = txt(sl, ML, y, CW, 0.5)
runs(tf, [("It is not SQL, and it is not parsed as SQL. ", CRIMSON, True),
          ("`CREATE CUBE` goes through a hand-written parser, because the engine would "
           "reject the statement before any hook could see it --- `crates/sankhya-cube-sql/src/ddl.rs`.",
           SLATE, False)], size=11, first=True, space_after=0, line=1.25)

sl, top = content("Twelve rows, and the four cells they make",
                  kicker="WORKED · THE BASE GRAIN")
code(sl, ML, top, CW * 0.44, [
    " id | region | period | amount",
    "----+--------+--------+-------",
    "  0 | north  | q1     |    0.0",
    "  1 | south  | q2     |    1.5",
    "  2 | (null) | q1     |    3.0",
    "  3 | north  | q2     |    4.5",
    "  4 | south  | q1     |    6.0",
    "  5 | (null) | q2     |    7.5",
    "  6 | north  | q1     |    9.0",
    "  7 | south  | q2     |   10.5",
    "  8 | (null) | q1     |   12.0",
    "  9 | north  | q2     |   13.5",
    " 10 | south  | q1     |   15.0",
    " 11 | (null) | q2     |   16.5",
], fs=9, title="the fixture generator: region = id % 3, period = id % 2, amount = id x 1.5")

x2 = ML + CW * 0.48
tf = txt(sl, x2, top, CW * 0.52, 0.4)
para(tf, "The base grain --- 4 cells, 8 rows placed, 4 unplaced",
     size=11.5, color=DEEP, bold=True, first=True, space_after=6)
h = table(sl, [
    ["", "q1", "q2", "row total"],
    ["**north**", "0 + 9 = **9.0**", "4.5 + 13.5 = **18.0**", "27.0"],
    ["**south**", "6 + 15 = **21.0**", "1.5 + 10.5 = **12.0**", "33.0"],
    ["column total", "30.0", "30.0", "**60.0**"],
], x2, top + 0.38, CW * 0.52, col_w=[1.15, 1.35, 1.55, 1.05], fs=10, hfs=10)
tf = txt(sl, x2, top + 0.38 + h + 0.22, CW * 0.52, 1.9)
runs(tf, [("A null key is not a member named \u201c\u201d. ", CRIMSON, True),
          ("Four rows have no region. They are **counted and never dropped** --- they are "
           "not in any cell, and the completeness column on every answer says so. That is "
           "why 27 + 33 = 60 and the twelve rows total 99.0.", SLATE, False)],
     size=11, first=True, space_after=8, line=1.28)
runs(tf, [("Every number on the next three slides can be checked by hand against "
           "this table.", DEEP, True)], size=11, space_after=0)

sl, top = content("Roll up rolls a dimension AWAY",
                  kicker="WORKED · ROLL UP")
tf = txt(sl, ML, top, CW, 0.55)
runs(tf, [("`by=` names the dimensions to KEEP. ", CRIMSON, True),
          ("Every dimension not named is aggregated out, one at a time, under the rule the "
           "measure declares **for the dimension being removed** --- not for the one you kept.",
           SLATE, False)], size=11.5, first=True, space_after=0, line=1.25)
y = top + 0.62
code(sl, ML, y, CW * 0.47, [
    "SELECT region, amount",
    "FROM cube_rollup('sales_q', 'amount', 'by=region');",
    "",
    " region | amount",
    "--------+--------",
    " north  |   27.0",
    " south  |   33.0",
], fs=9.5, title="2 axes -> 1 axis   ·   4 cells -> 2 cells")
code(sl, ML + CW * 0.51, y, CW * 0.49, [
    "SELECT amount",
    "FROM cube_rollup('sales_q', 'amount');",
    "",
    " amount",
    "--------",
    "   60.0",
], fs=9.5, title="no by= : the grand total   ·   4 cells -> 1 cell")
y2 = y + 1.62
tf = txt(sl, ML, y2, CW, 1.6)
runs(tf, [("`period` is not filtered, not hidden, and not present. ", DEEP, True),
          ("It is gone, and its facts are inside the numbers. That is the difference between "
           "a roll-up and a `WHERE` clause, and it is the difference a spreadsheet cannot "
           "express.", SLATE, False)], size=11.5, first=True, space_after=8, line=1.28)
runs(tf, [("Check it: ", CRIMSON, True),
          ("27 + 33 = 60 = 30 + 30. The four null-region rows (3.0 + 7.5 + 12.0 + 16.5 = "
           "39.0) are **not** in it --- twelve rows total 99.0, and the answer says so on the "
           "completeness column.", SLATE, False)], size=11, space_after=0, line=1.28)

sl, top = content("Slice REMOVES an axis; dice narrows one",
                  kicker="WORKED · SLICE AND DICE")
y = top
code(sl, ML, y, CW * 0.47, [
    "SELECT period, amount FROM cube_slice(",
    "  'sales_q', 'amount', 'where=region:north');",
    "",
    " period | amount",
    "--------+--------",
    " q1     |    9.0",
    " q2     |   18.0",
], fs=9.5, title="SLICE  ·  4 cells -> 2 cells on 1 axis")
code(sl, ML + CW * 0.51, y, CW * 0.49, [
    "SELECT region, period, amount FROM cube_rollup(",
    "  'sales_q', 'amount',",
    "  'by=region|period, where=region:north');",
    "",
    " region | period | amount",
    "--------+--------+--------",
    " north  | q1     |    9.0",
    " north  | q2     |   18.0",
], fs=9.5, title="DICE  ·  4 cells -> 2 cells on 2 axes")
y2 = y + 1.92
tf = txt(sl, ML, y2, CW * 0.60, 1.5)
runs(tf, [("There is no `region` column in the slice. ", CRIMSON, True),
          ("The axis goes because it no longer distinguishes anything --- every remaining "
           "cell carries the same member. Keeping it produces a degenerate axis, and a later "
           "roll-up along it silently does nothing.", SLATE, False)],
     size=11, first=True, space_after=8, line=1.26)
runs(tf, [("Dice keeps every dimension, so a later roll-up still composes. ", DEEP, True),
          ("Dice is an option inside `where=` rather than a fourth navigation --- there is no "
           "`cube_dice` and no `cube_pivot`. Two navigations are registered, and three "
           "documents claimed four or five until somebody counted.", SLATE, False)],
     size=11, space_after=0, line=1.26)
note(sl, ML + CW * 0.63, y2, CW * 0.37, 1.5,
     "Note the pipe. ",
     "The options string is itself comma-separated, so a list of dimensions uses `|`. "
     "An unknown option is refused rather than defaulted: a misspelled option that quietly "
     "takes its default produces a result that is wrong in a way the query text does not "
     "reveal.")

sl, top = content("The same navigation on the whole thousand-row table",
                  kicker="WORKED · AT SIZE")
code(sl, ML, top, CW * 0.62, [
    "$ psql ... -c \"SELECT region, amount, completeness, withheld,",
    "                      materialised",
    "               FROM cube_rollup('sales','amount','by=region');\"",
    "",
    " region |  amount  | completeness | withheld | materialised",
    "--------+----------+--------------+----------+--------------",
    " north  | 250249.5 |        0.667 |      333 | f",
    " south  | 249250.5 |        0.667 |      333 | f",
], fs=9, title="docs/QUICKSTART.md, run against the shipped fixture")
tf = txt(sl, ML, top + 1.85, CW, 2.3)
runs(tf, [("Checkable in closed form. ", CRIMSON, True),
          ("North is the ids divisible by three: 1.5 x 3 x (0+1+...+333) = 4.5 x 55,611 = "
           "**250,249.5**. South is the ids congruent to 1 mod 3: 1.5 x 166,167 = "
           "**249,250.5**. The grand total 499,500 is exactly `sum(amount) WHERE region IS "
           "NOT NULL`, and 333 of 1,000 rows have no region --- so completeness is 0.667 and "
           "`withheld` is 333.", SLATE, False)],
     size=11.5, first=True, space_after=10, line=1.28)
runs(tf, [("`completeness` and `withheld` come from the point of enforcement, never from "
           "the result. ", DEEP, True),
          ("A withheld row leaves no trace, so an aggregate counting what arrived and "
           "dividing by what arrived reports itself complete however much policy removed. "
           "They are computed at hydration, not from the rows that survived a dice.",
           SLATE, False)], size=11.5, space_after=0, line=1.28)

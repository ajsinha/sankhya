# ============================================================ PART III
part_of("III")

# ------------------------------------------------------------ CH 10
chapter("10", "Why GROUP BY CUBE is not a cube")

sl, top = content("A grouping construct is not a model",
                  kicker="THE CUBE · WHAT IS BEING BUILT")
h = table(sl, [
    ["", "`GROUP BY CUBE`", "A cube"],
    ["What it is", "a set of column combinations you enumerate", "a declared model of dimensions and measures"],
    ["Dimensions", "none --- columns", "named, with levels coarse-to-fine"],
    ["Hierarchies", "none", "a level path, walked one grain to the next"],
    ["Members", "whatever values appear", "resolved from a member table, authorized"],
    ["Combination rule", "the aggregate you typed, everywhere", "declared **per measure per dimension**"],
    ["A wrong combination", "computes and returns", "refused, naming the measure and the axis"],
], ML, top, CW, col_w=[2.0, 4.6, 5.0])
note(sl, ML, top + h + 0.24, CW, 1.15,
     "The distinction that matters is the last row. ",
     "`GROUP BY CUBE` will happily average a percentage across regions and sum a closing "
     "balance across twelve months. Both produce a number of the right magnitude and the "
     "right sign; neither means anything; and nothing about either looks wrong. A model is "
     "what lets the engine know the difference.")

sl, top = content("Three properties this system already had",
                  kicker="THE CUBE · WHY HERE")
CARDS = [
    ("01", "A consolidation path is a graph traversal.",
     "Walking a hierarchy from one grain to the next is exactly what the graph engine "
     "already does over a typed adjacency. The cube did not need a second traversal."),
    ("02", "Consolidation is floating-point reduction at its worst.",
     "A roll-up re-associates a sum by construction, and re-association is precisely what "
     "moves a floating-point total. The determinism work in Part V is the prerequisite, "
     "not a neighbour."),
    ("03", "Every table already carries a business-date axis.",
     "A time dimension is the one every cube has and the one every implementation gets "
     "wrong, because arrival time and business time are different questions. Chapter 7 is "
     "why that was settled first."),
]
yy = top + 0.05
for num, title, body in CARDS:
    card(sl, ML, yy, CW, 1.28, num, title, body)
    yy += 1.42

# ------------------------------------------------------------ CH 11
chapter("11", "The model")

sl, top = content("A cube is a declared view over a published table",
                  kicker="THE MODEL · WHAT IT IS")
h = table(sl, [
    ["Part", "What it is", "Written as"],
    ["Fact source", "a published table, or a parenthesised query", "`FROM sales.orders`"],
    ["Dimension", "a name, a member table, the fact column it joins on, and levels", "`DIMENSION region FROM sales.regions ON region (LEVEL area = region)`"],
    ["Measure", "a name and **one rule per dimension, with no default**", "`MEASURE amount (SUM ALONG region, SUM ALONG period)`"],
    ["Rule", "`SUM` `MIN` `MAX` `FIRST` `LAST` `MEAN` `NONE` `AGGREGATION <name>`", "`crates/sankhya-cube-algo/src/measure.rs`"],
    ["Composes?", "`SUM` `MIN` `MAX` `FIRST` `LAST` yes · `MEAN` `NONE` no", "`measure.rs`, and it is a property of the operator"],
], ML, top, CW, col_w=[1.7, 4.6, 5.3])
tf = txt(sl, ML, top + h + 0.24, CW, 1.5)
runs(tf, [("There is no load step, no build step and no second store. ", CRIMSON, True),
          ("`CREATE CUBE` writes one JSON document to `<warehouse>/_cubes/<name>.json` and "
           "nothing else --- no data, no cells. A cell exists because rows exist. The version "
           "is not a field somebody remembers to bump: it is an FNV-1a fingerprint of the "
           "validated content, so changing a rule changes the key and reformatting the file "
           "does not.", SLATE, False)],
     size=11.5, first=True, space_after=0, line=1.28)

sl, top = content("The rule is per measure PER DIMENSION, and that is the part people skip",
                  kicker="THE MODEL · THE RULE")
tf = txt(sl, ML, top, CW, 1.3)
runs(tf, [("\u201cThis measure is semi-additive\u201d is not a usable statement. ", DEEP, True),
          ("Semi-additive *over what?* A balance sums across accounts and takes the last "
           "value across time. At query time the planner asks one question --- may this "
           "measure be combined along **this** axis, and with what? --- and a per-measure "
           "answer cannot answer it.", SLATE, False)],
     size=12, first=True, space_after=0, line=1.28)
code(sl, ML, top + 1.05, CW * 0.54, [
    "SELECT * FROM cube_measures('sales_q');",
    "",
    "  measure   | dimension | rule | composes",
    "------------+-----------+------+---------",
    " amount     | region    | sum  | t",
    " amount     | period    | sum  | t",
    " margin_pct | region    | none | f",
    " margin_pct | period    | none | f",
], fs=9.5, title="one row per (measure, dimension)")
tf = txt(sl, ML + CW * 0.58, top + 1.05, CW * 0.42, 2.6)
para(tf, "That shape is the argument.", size=12, color=DEEP, bold=True,
     first=True, space_after=8)
para(tf, "A rule declared per dimension is what makes a semi-additive measure "
         "expressible at all --- and `composes` is on the row so a client does not "
         "offer \u201croll up by time\u201d as a button that cannot work.",
     size=11, color=SLATE, line=1.3, space_after=8)
para(tf, "Asserted over the wire on every build, booleans rendered `t`/`f` as "
         "PostgreSQL does --- `crates/sankhya-server/tests/cube_queries.rs`.",
     size=10, color=MUTED, line=1.28)

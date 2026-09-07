# ============================================================ 6 · WHAT A CUBE REFUSES
chapter("13", "Completeness, and cubes under policy")

sl, top = content("Three measures, three answers",
                  kicker="FOUNDATIONS · ADDITIVITY")
h = table(sl, [
    ["Measure", "Rolls up how", "Asked to roll up"],
    ["`amount` — a flow", "summed", "summed, across every dimension"],
    ["`balance` — a stock", "last, along time; summed elsewhere", "the closing balance, not twelve of them added"],
    ["`margin_pct` — a ratio", "**it does not**", "**refused by name**, with the reason"],
], ML, top, CW, col_w=[2.8, 4.0, 4.8])
tf = txt(sl, ML, top + h + 0.26, CW, 2.4)
runs(tf, [("A ratio of sums is not the sum of ratios, and no rule makes it one. ", INK, True),
          ("So a cube declaring `margin_pct` with no additivity rule does not sum it, does "
           "not average it, and does not quietly return the first value. It refuses the "
           "roll-up and says which measure and why.",
           SLATE, False)], size=12, space_after=10, line=1.3)
runs(tf, [("The alternative is the one every spreadsheet takes: ", DEEP, True),
          ("average the percentages. That produces a number, it is wrong by an amount nobody "
           "can bound, and nothing about it looks wrong. This is the clearest case in the "
           "system of a refusal being worth more than an answer.",
           SLATE, False)], size=12, line=1.3)

sl, top = content("A total that says what it covered",
                  kicker="FOUNDATIONS · COMPLETENESS")
code(sl, ML, top, CW * 0.56, [
    "SELECT * FROM cube_rollup('sales', 'region');",
    "",
    " region  |  amount  | completeness | withheld",
    "---------+----------+--------------+---------",
    " north   |   412000 |        1.000 |        0",
    " south   |   380500 |        1.000 |        0",
    " east    |   201750 |        0.667 |      333",
], fs=9.5, title="a roll-up over a partly-covered grain")
tf = txt(sl, ML + CW * 0.60, top, CW * 0.40, 2.6)
para(tf, "The last row is the point.", size=12, color=DEEP, bold=True, first=True, space_after=8)
para(tf, "`east` is a total over two of three days. The number is real, and on its own it is "
         "indistinguishable from a complete one — so it does not travel on its own.",
     size=11, color=SLATE, line=1.3, space_after=8)
para(tf, "`completeness` is the fraction of the grain actually covered; `withheld` is what "
         "was not. A consumer can decide; a consumer without them cannot.",
     size=11, color=SLATE, line=1.3)
y = top + 2.75
note(sl, ML, y, CW, 0.95,
     "Two navigations, not four. ",
     "`cube_rollup` and `cube_slice` are registered. Dice is an option inside slice rather "
     "than a navigation of its own, and drill-down is not built — three documents claimed "
     "four or five until a reviewer counted the registrations.")

# ============================================================ 2 · THE ESTATE
divider("2", "The Estate, and What It Costs to Keep",
        "Three engines, three copies, and a reconciliation function with no analytical output.",
        [])

sl, top = content("What an estate actually holds",
                  kicker="THE PROBLEM · WHAT IS THERE")
h = table(sl, [
    ["The engine", "What it is good at", "What it holds", "Kept in step by"],
    ["A transactional store", "one row, now, correctly", "the operational truth", "being the source"],
    ["An analytical engine", "a hundred million rows, columnar", "a copy, batched in", "a pipeline"],
    ["A graph store", "traversal and reachability", "a second copy, reshaped", "another pipeline"],
    ["A search index", "text and ranking", "a third copy", "a third pipeline"],
], ML, top, CW, col_w=[2.6, 3.3, 2.9, 2.8])
tf = txt(sl, ML, top + h + 0.24, CW, 1.6)
para(tf, "Each is the right tool. None of them is the problem. The problem is the arithmetic "
         "that follows from having them.",
     size=12.5, color=INK, bold=True, first=True, space_after=10, line=1.3)
runs(tf, [("Every pair of copies is a place they can disagree. ", INK, True),
          ("Four copies is six pairs; six copies is fifteen. The reconciliation function that "
           "grows to cover them produces no analytical output of its own — it exists entirely "
           "to answer whether two systems still agree, and it is never finished, because a "
           "system that stops reconciling is not a system that has converged.",
           SLATE, False)], size=12, line=1.3)

sl, top = content("What the answer has to be, and what it cannot be",
                  kicker="THE PROBLEM · THE SHAPE OF AN ANSWER")
h = table(sl, [
    ["An answer that does not work", "Why not"],
    ["Make the copies converge faster", "Halving the lag halves the window in which they disagree and removes none of the pairs. The reconciliation function is unchanged."],
    ["Make one engine do everything", "A row store scanning a hundred million rows and a column store fetching one row are both the wrong shape. The specialisation is real."],
    ["Put a query federator in front", "The copies still exist and still disagree; the federator now has to decide which one is right, at query time, with no basis for deciding."],
    ["One governed copy, several engines over it", "The pairs go to zero because there is one copy. This is the design, and §III says what it costs."],
], ML, top, CW, col_w=[3.6, 8.0])
tf = txt(sl, ML, top + h + 0.24, CW, 1.2)
runs(tf, [("The last row is a claim about today, and it is not true yet. ", DEEP, True),
          ("One engine runs — the analytical one, read-only. The transactional tier is a "
           "supervised child process that nothing in the server starts; the graph tier is "
           "registered against an empty catalogue and can never answer. Part V says so at "
           "length, and `docs/STATUS.md` is the document that must be believed over this one.",
           SLATE, False)], size=11.5, line=1.3)

# ============================================================ 8 · WHAT BOUNDS IT
divider("8", "What Bounds a Statement",
        "Four limits, one of which was a lie until recently.",
        [])

sl, top = content("Four bounds, and which door each applies to",
                  kicker="THE SYSTEM · LIMITS")
h = table(sl, [
    ["Bound", "Default", "Set by", "Applies to"],
    ["Memory, **shared between all queries**", "1 GiB", "`SANKHYA_QUERY_MEMORY_BYTES`", "every statement"],
    ["Rows returned", "10,000", "compiled in", "the wire door"],
    ["Statement deadline", "30 minutes", "`SANKHYA_STATEMENT_TIMEOUT_SECONDS`", "the wire door"],
    ["Concurrent connections", "1,024", "compiled in", "the wire door"],
], ML, top, CW, col_w=[3.5, 1.5, 3.6, 3.0])
tf = txt(sl, ML, top + h + 0.26, CW, 2.6)
runs(tf, [("The pool is fair rather than greedy, and that is the whole argument for sharing it. ", INK, True),
          ("A greedy pool serves whoever asks first and starves the rest, turning one "
           "expensive query into an outage for everybody. A fair one makes the expensive "
           "query fail *itself* — which is the query that should get the error.",
           SLATE, False)], size=11.5, space_after=10, line=1.3)
runs(tf, [("Spilling is not a silver bullet. ", DEEP, True),
          ("A sort or a grouping past the bound finishes slowly on disk. A hash join cannot "
           "spill at all, so it is refused whatever the limit is — worth knowing before "
           "somebody raises the limit expecting the join to start working.",
           SLATE, False)], size=11.5, line=1.3)

sl, top = content("The bound that was not a bound",
                  kicker="THE SYSTEM · A CORRECTION")
tf = txt(sl, ML, top, CW, 2.2)
para(tf, "The setting says what a server's queries may use **between them**. The first "
         "implementation built a fresh pool on every call.",
     size=13, color=INK, bold=True, first=True, space_after=10, line=1.3)
runs(tf, [("So each statement got its own gibibyte, ten concurrent statements got ten, and "
           "the machine died exactly as it had before — while the setting, its help text and "
           "the remediation log all said otherwise. ", SLATE, False),
          ("A pool that is not shared is not a bound; it is a per-statement allowance wearing "
           "a bound's name, which is worse than no bound because it reads as solved.",
           INK, True)], size=11.5, space_after=10, line=1.3)
runs(tf, [("And fairness was the entire reason for choosing that pool. ", DEEP, True),
          ("With a pool each, there is nothing to be fair about.", SLATE, False)],
     size=11.5, line=1.3)
y = top + 2.4
note(sl, ML, y, CW, 1.15,
     "The test written to prove it passed either way. ",
     "Two sessions and two hash joins against a megabyte are refused whether the pool is "
     "shared or per-statement — each is refused against a megabyte of its own. It was thrown "
     "away for one that asks the question directly: the two runtimes, and the pools inside "
     "them, must be the same object.")

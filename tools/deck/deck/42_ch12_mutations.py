# ============================================================ 12 · MUTATIONS
divider("12", "Would the Tests Notice?",
        "Nine hundred deliberate defects, and what surviving one means.",
        [])

sl, top = content("The only mechanism that asks the right question",
                  kicker="EVIDENCE · MUTATION")
tf = txt(sl, ML, top, CW, 1.1)
para(tf, "A defect is applied to the source, the suite is run, and the entry passes only if "
         "the suite **fails**. A mutation that survives is a hole in the tests, named and "
         "located.",
     size=12.5, color=INK, first=True, line=1.3)
y = top + 1.1
h = table(sl, [
    ["What surviving revealed", "How many"],
    ["Equivalent mutants no test could ever have caught", "5"],
    ["Entries inert until corrected — two did not compile; one patched the harmless copy of a guard that appears twice", "6"],
    ["Tests that did not test what their names claimed", "4"],
    ["Defects in **tests** rather than in code — all three the same defect: an unbounded wait, so removing a deadline hung the build rather than failing it", "3"],
], ML, y, CW, col_w=[9.4, 2.2])
tf = txt(sl, ML, y + h + 0.24, CW, 1.2)
runs(tf, [("A hang is strictly worse than a failure — it takes the build with it and reports "
           "nothing. ", INK, True),
          ("Every wait now goes through one bounded helper. And a mutation that hangs the "
           "build is worse than one that survives: one sandbox entry made a spawn block on a "
           "full pipe and was removed, with the reasoning recorded.",
           SLATE, False)], size=11.5, line=1.3)

sl, top = content("The discipline turned on itself",
                  kicker="EVIDENCE · BAD MUTATIONS")
h = table(sl, [
    ["A mutation that proved nothing", "Why it survived"],
    ["Changed a log message's wording", "the assertion matched a substring of both wordings"],
    ["Asserted that the log cache caches", "true before the fix and after it — the property was whether the *caller* used it"],
    ["Added a `println!` beside the `tracing` call it replaced", "nothing was taken away, so nothing could notice"],
    ["Reverted a parse to skip a bad line", "a *different* mechanism — the empty-body seal — returned the same error variant"],
], ML, top, CW, col_w=[4.4, 7.2])
tf = txt(sl, ML, top + h + 0.26, CW, 1.6)
runs(tf, [("Each of those survived, each was rewritten, and each is in the log. ", INK, True),
          ("The last one is the sharpest: the only test guarding *a malformed line is "
           "reported* passed because the commit it wrote had **nothing else in it**, so an "
           "unrelated check fired first. A line that is valid JSON but not a valid action was "
           "skipped silently — the commit replaying as though it did less than it did.",
           SLATE, False)], size=11.5, line=1.3)

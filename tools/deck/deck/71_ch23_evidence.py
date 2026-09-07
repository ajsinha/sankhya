# ============================================================ 11 · EVIDENCE
chapter("23", "Four kinds of evidence")

sl, top = content("Four questions, four mechanisms",
                  kicker="EVIDENCE · THE LADDER")
h = table(sl, [
    ["Mechanism", "Answers", "Fails when", "Count"],
    ["Tests", "does it do the thing?", "behaviour changes", "2,843 tests"],
    ["Gates", "does the repository still hold its own rules?", "a rule breaks anywhere, **including in a document**", "27"],
    ["Mutations", "would the tests notice if it stopped?", "a deliberate defect survives the suite", "921 deliberate defects"],
    ["Benchmarks", "is the number real?", "a published figure has nothing that produces it", "2 suites"],
], ML, top, CW, col_w=[1.9, 3.6, 4.5, 1.6])
tf = txt(sl, ML, top + h + 0.26, CW, 2.0)
runs(tf, [("A test proves that code does what a test says. ", INK, True),
          ("It does not prove the test would notice if the code stopped. Nor does a green "
           "build prove the checks ran, that they measured anything, or that what they "
           "measured is what a document claims.",
           SLATE, False)], size=12, space_after=10, line=1.3)
runs(tf, [("Twelve audits found 129 things in a repository whose gate was green. ", DEEP, True),
          ("Not one was found by a failing test, because none of them made a test fail.",
           SLATE, False)], size=12, line=1.3)

sl, top = content("Cheapest first, and why that is the whole trick",
                  kicker="EVIDENCE · THE GATE")
h = table(sl, [
    ["", "Checks", "Time"],
    ["Read files and decide", "21", "**about two seconds, for all of them**"],
    ["Build something", "6", "twenty to twenty-five minutes"],
], ML, top, CW * 0.72, col_w=[3.4, 1.6, 3.4])
tf = txt(sl, ML, top + h + 0.3, CW, 2.2)
runs(tf, [("The expensive one used to run first. ", INK, True),
          ("So a stale figure, a file fifteen lines over the length ceiling, and a banned "
           "word in a comment — combined computation under five seconds — were reported "
           "**twenty-five minutes** into a run. That happened three times in one sitting.",
           SLATE, False)], size=12, space_after=10, line=1.3)
runs(tf, [("Cheapest-first does not make the gate faster. ", DEEP, True),
          ("It makes the loop faster, which is the thing that was slow: the answer arrives "
           "while the person who caused it is still looking at what they changed. "
           "`cargo xtask check-fast` is the same set on its own.",
           SLATE, False)], size=12, line=1.3)

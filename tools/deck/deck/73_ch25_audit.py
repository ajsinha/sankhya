# ============================================================ 14 · THE AUDIT
chapter("25", "What twelve reviewers found")

sl, top = content("The number, and how to read it",
                  kicker="THE AUDIT · THE FINDING")
statbar(sl, top, [
    ("129", "findings"),
    ("12", "reviewers"),
    ("3", "days"),
    ("0", "found by a failing test"),
])
tf = txt(sl, ML, top + 1.35, CW, 2.6)
para(tf, "The correct reading of that number is that it is a **lower bound**.",
     size=14, color=DEEP, bold=True, first=True, space_after=10, line=1.3)
runs(tf, [("Not one was found by a failing test, because none of them made a test fail. ",
           INK, True),
          ("They were found by people reading code against the documents that described it — "
           "which is a method that scales with attention and not with hardware, and which "
           "nobody was applying.",
           SLATE, False)], size=12, space_after=10, line=1.3)
runs(tf, [("What closing them produced is not a clean bill of health. ", INK, True),
          ("It is a system where the next hundred findings would be visible: a gate that "
           "fails when a document contradicts the code, a catalogue that says which alerts "
           "cannot fire, a benchmark behind every published figure.",
           SLATE, False)], size=12, line=1.3)

sl, top = content("The four causes, and what each explains",
                  kicker="THE AUDIT · WHY THEY WERE THERE")
h = table(sl, [
    ["", "The cause", "What it explains"],
    ["R1", "The test fixture and the documented recipe silently diverged", "every doc-versus-reality finding — the transcripts were real, taken from a warehouse the instructions no longer built"],
    ["R2", "There is no CI — the gate runs when somebody chooses", "the fact that all 129 survived"],
    ["R3", "Gates report green when they measure nothing", "fifteen e2e tests skipping to green, a coverage gate counting nothing, a soak whose verdict is printed rather than asserted"],
    ["R4", "Documents assert; nothing checks the assertion", "three published speed tables nothing ever produced, and a milestone line that was wrong in thirteen files"],
], ML, top, CW, col_w=[0.7, 4.2, 6.7])
tf = txt(sl, ML, top + h + 0.26, CW, 1.3)
runs(tf, [("R4 survived inside the fix for R4. ", DEEP, True),
          ("The check comparing status lines only looked at documents that *declared* one, "
           "and not one of the book's twenty-nine chapters did — so the book opted out of the "
           "check written to stop drift, for free. That is how the section headed *\"the one "
           "to read before believing anything else\"* still carried a retracted milestone claim.",
           SLATE, False)], size=11.5, line=1.3)

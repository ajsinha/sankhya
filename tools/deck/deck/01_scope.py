# ============================================================ FRONT MATTER
# The five things a reader should leave with, before the argument for any of
# them. The previous deck opened on a routing table --- a reader who read the
# first three slides left with a bibliography rather than a thesis.
_state["chapter"] = "Front matter"

sl, top = content("Five things, before the argument for any of them",
                  kicker="FRONT MATTER · THE SUMMARY")
CARDS = [
    ("01", "The integration is in the substrate, and the substrate is built.",
     "One address space, one catalogue, one enforcement point at plan construction, one "
     "business-date axis on every table, one open storage format, one hash-chained audit. "
     "Three data models are designed against that layer rather than bolted to each other."),
    ("02", "Of the three engines, one answers today.",
     "The analytical engine runs: start a binary, connect with `psql` or Arrow Flight SQL, "
     "query real Parquet. The graph engine is a built, tested, zero-dependency library that "
     "nothing hydrates on a timer. The transactional tier is a supervisor that is a "
     "dev-dependency of the server, and the server never starts a database."),
    ("03", "What does run is a product on its own.",
     "A single-node analytical warehouse over an open format, with a declared "
     "multidimensional model answered on demand, zero-copy clones, named snapshots across "
     "many tables, time travel, statistics-driven pruning, maintenance on a timer, file "
     "feeds, and 155 functions over vector and matrix columns."),
    ("04", "The interesting behaviour is what it refuses.",
     "A measure with no declared way to combine along a dimension is refused at planning, "
     "never defaulted to SUM. A total over part of a grain returns `completeness` and "
     "`withheld` as columns on the row. A hash join too large for the shared pool is an "
     "error, not a slow answer."),
    ("05", "A documented claim that nothing keeps is the worst defect class here.",
     "Twelve audits found 129 findings in a repository whose gate was green, and **not one "
     "was found by a failing test.** They were found by people reading code against the "
     "documents describing it. The count is a lower bound."),
]
yy = top
for num, title, body in CARDS:
    rect(sl, ML, yy, CW, 0.86, fill=WHITE, line=RULE)
    rect(sl, ML, yy, 0.045, 0.86, fill=CRIMSON)
    tfn = txt(sl, ML + 0.18, yy + 0.10, 0.5, 0.4)
    para(tfn, num, size=15, color=CRIMSON, bold=True, font=SERIF, first=True,
         space_after=0)
    tfc = txt(sl, ML + 0.72, yy + 0.09, CW - 0.92, 0.70)
    para(tfc, title, size=11.5, color=DEEP, bold=True, first=True, space_after=2,
         line=1.15)
    para(tfc, body, size=9.5, color=SLATE, space_after=0, line=1.22)
    yy += 0.94

sl, top = content("What this deck is, and what it is not",
                  kicker="FRONT MATTER · SCOPE")
h = table(sl, [
    ["Document", "Level", "Answers"],
    ["`README.md`", "one page", "what it is, what runs today, where to go next"],
    ["`QUICKSTART.md`", "a session", "install, start, query, tear down --- every transcript reproducible"],
    ["`ARCHITECTURE.md`", "section by section", "what each part does, marked **[Built]**, **[Built, with a named gap]** or **[Designed]**"],
    ["`STATUS.md`", "the ground truth", "what works today, and the canonical list of what does not. **Where this deck and that document disagree, that document is right**"],
    ["`TESTING.md`", "the evidence", "the gates, the mutations, and what the build only *intends*"],
    ["`REMEDIATION.md`", "the log", "129 findings, and what closing each one cost"],
    ["**This deck**", "the whole system", "why the design is this shape, what it refuses, and which third of it you can run"],
], ML, top, CW, col_w=[2.3, 2.0, 7.3])
note(sl, ML, top + h + 0.22, CW, 1.5,
     "Every figure in this deck names what produced it. ",
     "A number with no benchmark beside it is a claim, and this project once published three "
     "speed tables that nothing in its history had ever produced --- restated across other "
     "documents until they read as established. Part VI is about that, and it is why the "
     "tense of every sentence here is load-bearing: *runs*, *is built and reaches no door*, "
     "and *is designed* are three different states and are never printed as one.",
     " This deck does not replace the documents above; it is the shortest of them. It is not "
     "a benchmark report --- three of eighteen performance objectives are measured. And it is "
     "not a claim of production readiness: twelve audits returned *not production-ready*, and "
     "nothing since has re-run that verdict.")

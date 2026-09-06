# ============================================================ FRONT MATTER
_state["chapter"] = "Front matter"

# ---- who this is for
sl, top = content("Who this is for, and where to stop reading",
             kicker="FRONT MATTER · SCOPE")
h = table(sl, [
    ["If you are", "Read", "The last part you need"],
    ["evaluating whether to use this",
     "Part I for the argument, then Part V for what an audit found",
     "V — it is the honest one"],
    ["an engineer who wants to run a query today",
     "Part III, then `docs/QUICKSTART.md`",
     "III"],
    ["a data scientist deciding whether numbers tie out",
     "Part II — determinism and the date axis, as tests rather than claims",
     "II"],
    ["responsible for operating it",
     "Part III §III.6 onward, then `docs/OPERATIONS.md` and `docs/SECURITY.md`",
     "III"],
    ["auditing it, or deciding whether to trust it",
     "Part IV on the evidence, then all of Part V",
     "V"],
], ML, top, CW, col_w=[3.0, 5.4, 3.2])
tf = txt(sl, ML, top + h + 0.22, CW, 0.9)
para(tf, "This deck is one of eighteen documents and it is the shortest. It does not replace "
         "them --- a deck that claimed to would be the mistake this project has already made "
         "once, when a twenty-seven-chapter book said it replaced the working documents it "
         "was assembled from and then went stale wherever they disagreed.",
     size=11, color=SLATE, first=True, line=1.3)

# ---- what it is and is not
sl, top = content("What this deck is, and what it is not",
             kicker="FRONT MATTER · SCOPE")
h = table(sl, [
    ["Document", "Level", "Answers"],
    ["`README.md`", "one page", "what it is, what runs today, where to go"],
    ["`QUICKSTART.md`", "a session", "install, start, query, tear down --- every transcript real"],
    ["`ARCHITECTURE.md`", "the system", "what a query does, and what is designed and not running"],
    ["`TESTING.md`", "the evidence", "the gates, the mutations, and what none of it catches"],
    ["`REMEDIATION.md`", "the log", "129 findings, and what closing each one cost"],
    ["This deck", "the argument", "why the design is this shape, and what it refuses"],
], ML, top, CW, col_w=[2.6, 1.9, 7.1])
tf = txt(sl, ML, top + h + 0.22, CW, 1.0)
runs(tf, [("Every figure in this deck names what produced it. ", INK, True),
          ("A number without a benchmark beside it is a claim, and this project published "
           "three speed tables that nothing in its history had ever produced --- restated "
           "across other documents until they read as established fact. Part V is about that.",
           SLATE, False)], size=11, line=1.3)

# ---- vocabulary
sl, top = content("Nine terms, defined before they are used",
             kicker="FRONT MATTER · VOCABULARY")
table(sl, [
    ["Term", "What it means here"],
    ["Warehouse", "A directory of tables in open Delta format. Any engine that reads Delta reads it, with no SANKHYA process in the path."],
    ["Live set", "The files a table consists of at a version, after replaying its log. Not what is on disk --- a retired file outlives the commit that replaced it."],
    ["`sank_data_date`", "The business date a row belongs to, carried by every published table. Distinct from when it arrived, which is what makes a restatement expressible."],
    ["Cube", "A declared aggregation over a fact table, with dimensions, measures and an additivity rule per measure. A measure with no rule is refused rather than summed."],
    ["Cuboid", "One materialised roll-up of a cube at one grain. A cache: an unreadable one costs a slower answer, never a wrong one."],
    ["Completeness", "The fraction of the grain a roll-up actually covered, returned beside the number. A total over 2 of 3 regions says so."],
    ["Snapshot", "A named pin at a version, so files it needs are not reclaimed. Distinct from a version, which is a position in the log."],
    ["Shape (of a statement)", "The first word, and a second only when it is a keyword. `select`, `create table`. Never `select nosuchcolumn` --- that is the caller's own text."],
    ["Window (of the audit)", "The most recent 1,024 records held in memory. The file is the chain; the window is what a running process can show you."],
], ML, top, CW, col_w=[2.3, 9.3], fs=9.5)

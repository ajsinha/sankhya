# ============================================================ 3 · POSITIONS
chapter("3", "Four positions, and what each costs")

sl, top = content("Four positions, and their cost",
                  kicker="THE STANCE · WHAT IS CHOSEN")
table(sl, [
    ["Position", "What it means", "What it costs"],
    ["Refuse rather than approximate",
     "A statement that cannot be answered exactly is refused by name. A measure with no additivity rule is not summed; a hash join too large for the pool is an error, not a slow answer.",
     "Statements fail that other engines would answer. Every refusal has to be worth reading, or it becomes a thing people route around."],
    ["Determinism over speed",
     "A reduction is order-independent by construction, so two runs of the same query agree bit for bit however the rows arrived.",
     "**57× against an ordinary sum at eight values**, 12.2× at four thousand. Measured, and published beside the claim rather than instead of it."],
    ["Open storage, no exception for the fast path",
     "Tables are Delta. Any engine that reads Delta reads them with no SANKHYA process in the path, including the materialised cuboids.",
     "The format bounds what can be expressed, and a protocol version this build cannot honour is refused rather than read approximately."],
    ["Evidence over assertion",
     "Every rule has a check that fails the build. Every speed figure has a benchmark that produces it. Every claim about what is built is compared against one canonical line.",
     "The checks are a system in themselves — twenty-seven of them — and Part V is the record of what they did not catch."],
], ML, top, CW, col_w=[2.5, 5.0, 4.1], fs=9.5)

sl, top = content("Why governance has to be in the path, not beside it",
                  kicker="THE STANCE · WHERE THE RULE LIVES")
steps(sl, ML, top, CW, [
    ("01", "A caller asks", "for a table by name, under a principal"),
    ("02", "The session is built", "with only the tables that principal may read — a refused table is absent, not present-and-filtered"),
    ("03", "The plan is checked", "row predicates and column masks are physical operators, not advice"),
    ("04", "The answer is recorded", "who, what shape, how many rows, under what restriction — in a hash-linked chain"),
], h=1.6)
tf = txt(sl, ML, top + 1.85, CW, 1.6)
runs(tf, [("A table the caller may not read is not registered in the session at all. ", INK, True),
          ("So a query naming it fails to resolve rather than planning and returning nothing "
           "— which would be indistinguishable from an empty table, and is the difference "
           "between a refusal and a wrong answer. The same reasoning governs the catalogue: a "
           "listing that showed tables the caller cannot read would disclose their existence "
           "through the back door of a schema browser.",
           SLATE, False)], size=11.5, line=1.3)
tf = txt(sl, ML, top + 3.35, CW, 0.9)
runs(tf, [("And the cost. ", DEEP, True),
          ("Building a session per statement is work, and this design does it on every one. "
           "What it buys is that authorization cannot be bypassed by a path that forgot to "
           "check — there is no such path, because the check is what constructs the session.",
           SLATE, False)], size=11.5, line=1.3)

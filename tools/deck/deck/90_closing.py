# ============================================================ CLOSING
_state["chapter"] = "Closing"

sl, top = content("What is true today", kicker="CLOSING · WHAT THIS COMMITS TO")
CARDS = [
    ("01", "One substrate, and it is the product.",
     "One catalogue, one enforcement point at plan construction, one business-date axis, one "
     "open format, one hash-chained audit, one address space. Every guarantee here is a "
     "property of that layer, not of the engine on it."),
    ("02", "An analytical warehouse you can start and query.",
     "The wire protocol and Arrow Flight SQL over real Parquet, planning from the table log "
     "alone, pruning nine files in ten on a point lookup, with maintenance on a timer."),
    ("03", "A declared multidimensional model, answered on demand.",
     "A combination rule per measure per dimension; a measure with no rule refused at "
     "definition time; completeness carried on every row, as a column."),
    ("04", "Copies that cost nothing, and positions that hold.",
     "A clone at constant cost and zero new files; snapshots pinning one position across many "
     "tables; a diff computed from the log, not estimated."),
    ("05", "Arithmetic that does not depend on the schedule.",
     "Every reduction bit-deterministic under permutation; 155 functions identical from SQL, "
     "Flight SQL and the SDK, compared on every build."),
    ("06", "An inventory of the gap that a build check keeps.",
     "Ten crates unreached, ten error codes nothing constructs --- both lists failing the "
     "build in **both** directions."),
]
# Two columns. Six cards down one column does not fit above the footer at any
# size a reader would accept, and shrinking the body until it does is how a
# summary slide becomes six sentences nobody finishes.
BW = (CW - 0.30) / 2
for i, (num, title, body) in enumerate(CARDS):
    x = ML + (i % 2) * (BW + 0.30)
    y = top + (i // 2) * 1.72
    card(sl, x, y, BW, 1.58, num, title, body)

sl, top = content("What is next, and what it is waiting on",
                  kicker="CLOSING · THE ORDER OF WORK")
table(sl, [
    ["Next", "What already exists", "What is missing"],
    ["**The change-capture runtime** --- the largest single gap", "A `pgoutput` decoder validated against a live 17.11 stream; an apply path with a property-tested transaction invariant; reconciliation, idempotence, crash safety, schema evolution, and a five-rung safety ladder", "**The driver that runs them on a timer.** One absence, not six"],
    ["**Wiring the transactional tier**", "A supervisor owning initdb, start, readiness, health and shutdown of a vendored 17.11", "A configuration surface. The settings have no transactional key, and the server never starts a database"],
    ["**A timer that hydrates a graph epoch**", "Every algorithm, bounded by construction and correct against brute force", "Anything that publishes into the catalogue. The throughput benchmark is carried as unmet rather than reinterpreted"],
    ["**The Python SDK, then Java and Rust**", "The client contract decided and placed in the architecture; the function catalogue already answers *which functions does this server have* as data", "The bindings themselves, written against a contract designed for three rather than retrofitted to them"],
    ["**Scale-out, HA, production-like acceptance**", "The shard-set seam, the version claim as a conditional put, the concurrency controls", "**A second machine.** Twelve hours, two nodes, 100 GB, 50 readers, 20 writers --- parked rather than quietly dropped"],
], ML, top, CW, col_w=[2.4, 5.0, 4.2], row_h=0.72, fs=9, hfs=9.5,
    bold_col0=True, first_col_color=CRIMSON)
note(sl, ML, top + 4.05, CW, 0.85,
     "The order is not arbitrary. ",
     "Everything above the last row is a **driver** over machinery that already exists and is "
     "tested; the last row is new work and needs hardware. That is a deliberately unglamorous "
     "next milestone, and it is the one that turns two designed engines into two running ones.")

sl, top = content("The one thing to hold against everything else in here",
                  kicker="CLOSING · THE HONEST HALF")
tf = txt(sl, ML, top, CW, 4.4)
runs(tf, [("This deck is a document, and documents are the defect class this project "
           "is worst at. ", CRIMSON, True),
          ("Twelve audits returned a hundred and twenty-nine findings against a repository "
           "whose gate was green, and **not one of them was found by a failing test** --- "
           "because none of them made a test fail. They were found by people reading code "
           "against the documents that described it. Three of the four root causes were "
           "about documents rather than code: a recipe that had silently diverged from its "
           "fixture, gates reporting green while measuring nothing, and assertions with "
           "nothing checking the assertion.", SLATE, False)],
     size=12, first=True, space_after=12, line=1.3)
runs(tf, [("So the claim this deck makes for itself is narrow. ", DEEP, True),
          ("Every figure here names what produced it. Every *built* names a file. Every "
           "*designed* is a section of the architecture document that exists specifically to "
           "hold things that do not run. A build check verifies that every path named still "
           "exists --- and **it cannot verify that the prose still describes what the code "
           "does.** Saying so is the point rather than an apology.", SLATE, False)],
     size=12, space_after=12, line=1.3)
runs(tf, [("The ground-truth document is the one to believe over this one. ", CRIMSON, True),
          ("It is four thousand lines and nobody reads it on their first day, which is why "
           "its first two screens are *what works today* and the canonical list of *what is "
           "not built*. Where it and this deck disagree, it is right, and this deck is the "
           "newer defect.", SLATE, False)],
     size=12, space_after=0, line=1.3)

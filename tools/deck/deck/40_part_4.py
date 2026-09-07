# ============================================================ PART IV
part_of("IV")

# ------------------------------------------------------------ CH 16
chapter("16", "Time travel and named snapshots")

sl, top = content("A position, on one table and across many",
                  kicker="VERSIONS · TIME TRAVEL")
code(sl, ML, top, CW * 0.47, [
    "-- one table, one position",
    "SET VERSION OF sales.orders = 4;",
    "SHOW HISTORY OF sales.orders;",
    "",
    "-- one position across many tables,",
    "-- durable across restart",
    "CREATE SNAPSHOT month_end",
    "  OF sales.orders, sales.regions, risk.positions",
    "  EXPIRE AFTER 90 DAYS;",
], fs=9.5, title="two scopes, one idea")
tf = txt(sl, ML + CW * 0.51, top, CW * 0.49, 3.4)
para(tf, "A snapshot is a position, not a table.", size=12, color=DEEP,
     bold=True, first=True, space_after=8)
bullets(tf, [
    "A market-risk run reads trades, rates, curves and hierarchy **as of one "
    "moment** --- or the reconciliation problem this system exists to remove "
    "reappears inside a single query.",
    "A table the snapshot does not name is **refused**, not answered from the "
    "present. A partly-as-of answer is the worst available shape.",
    "Taking one is a two-pass confirmation rather than a loop: read every "
    "version, read them all again, accept only if identical --- five attempts, "
    "then a serialization failure.",
    "Expiry is bounded at 730 days, because it governs how far ahead one person "
    "may commit storage somebody else will pay for.",
], size=10.5, gap=7)
note(sl, ML, top + 3.55, CW, 0.9,
     "Travel by version, not by timestamp. ",
     "`AS OF TIMESTAMP` does not exist, and could not be bolted on cheaply: this writer's "
     "commit records carry no clock at all --- they are a truncation seal, not a timestamp. "
     "The one time the system does report is derived from file modification times, and is "
     "deliberately absent for a metadata-only commit, because reporting zero would be a date "
     "in 1970 presented as a fact.")

# ------------------------------------------------------------ CH 17
chapter("17", "Zero-copy clone, and the premise it breaks")

sl, top = content("A copy that costs the same at a thousand rows and a billion",
                  kicker="CLONE · WHAT IT WRITES")
code(sl, ML, top, CW * 0.46, [
    "CREATE TABLE sales.orders_wip",
    "  CLONE sales.orders AT VERSION 4;",
    "",
    "$ find warehouse/sales/orders_wip -type f",
    "warehouse/sales/orders_wip/_delta_log/",
    "        00000000000000000000.json",
    "",
    "# three NDJSON lines. no Parquet.",
    "# protocol, metadata, lineage.",
], fs=9.5, title="constant time, constant space")
tf = txt(sl, ML + CW * 0.50, top, CW * 0.50, 3.5)
para(tf, "The clone's log names none of the origin's files.", size=12,
     color=DEEP, bold=True, first=True, space_after=8)
bullets(tf, [
    "Not by omission. An absolute URI breaks restore-to-a-different-path; a "
    "`../` path bets on resolution the format does not define. So the clone "
    "records **what it inherits from**, and the reader composes.",
    "A read resolves the origin's live set **at the pinned version**, then the "
    "clone's own log. Two logs, one answer.",
    "The origin committing afterwards changes nothing the clone sees --- the "
    "pin is a version, and files are immutable.",
    "A clone of a clone would splice against a log naming nothing, so the walk "
    "flattens the chain to the first ancestor that holds files, bounded at 64 "
    "against a hand-edited cycle.",
], size=10.5, gap=7)
note(sl, ML, top + 3.62, CW, 0.85,
     "The guard worth showing. ",
     "An empty file set is indistinguishable from version zero of a real table, and version "
     "zero is legitimately empty --- so the presence of a **log** is the signal, not the "
     "presence of files. A missing origin is refused by name rather than treated as an empty "
     "one, because that would answer with the clone's own writes and nothing else: a short "
     "answer wearing the shape of a whole one.")

sl, top = content("A file belongs to exactly one table. Under cloning that is false",
                  kicker="CLONE · THE PREMISE IT BREAKS")
tf = txt(sl, ML, top, CW, 0.65)
runs(tf, [("Three reclamation mechanisms were written against that premise, ", CRIMSON, True),
          ("and each becomes a way to delete data a clone is the only remaining reader of. "
           "The orphan sweep is the clearest: every step is correct, and the outcome is "
           "data loss in a table nobody was touching.", SLATE, False)],
     size=11.5, first=True, space_after=0, line=1.26)
steps(sl, ML, top + 0.72, CW, [
    ("1", "Listed", "The sweeper lists every file under the origin's directory."),
    ("2", "Not named", "The origin's log no longer names this one --- it was compacted away."),
    ("3", "Not reachable", "Nothing in the origin's history reaches it."),
    ("4", "Old enough", "It is past the age threshold. Every check has passed."),
    ("5", "Removed", "And it was the clone's. Nothing failed, nothing logged, nothing noticed."),
], h=1.55)
y = top + 2.45
tf = txt(sl, ML, y, CW * 0.48, 2.0)
para(tf, "The fix: clone-family reachability.", size=12, color=DEEP, bold=True,
     first=True, space_after=7)
para(tf, "A file is reclaimable when no table in the clone family reaches it --- the "
         "origin, its clones, their clones. The family is the unit, not the table, "
         "because the table was never the unit once a clone existed.",
     size=10.5, color=SLATE, line=1.28, space_after=0)
listbox(sl, ML + CW * 0.52, y, CW * 0.48, "Two mechanisms refused, and why", [
    ("Reference counting --- a count that drifts **low** deletes a file a clone still",
     SLATE, ""),
    ("reads, silently, in a table nobody was touching. There is no operation that", SLATE, ""),
    ("would notice, and no repair that does not require reading every log.", SLATE, ""),
    ("", SLATE, ""),
    ("Copy-on-maintenance --- correct, and it makes a zero-copy clone cost a copy", SLATE, ""),
    ("at the first compaction. That is the feature, deferred by one tick.", SLATE, ""),
], accent=CRIMSON, row=0.215)

sl, top = content("What it cost, stated plainly",
                  kicker="CLONE · THE PRICE")
CARDS = [
    ("01", "The open-storage claim holds for ordinary tables and not for clones.",
     "A foreign reader pointed at a clone's directory sees only what the clone wrote. The "
     "inherited rows are in the origin's directory and the clone's log does not name them. "
     "This is the one place the format's openness stops, and it is a consequence of "
     "Decision 1a rather than an oversight."),
    ("02", "The decision cost a read path, and the ADR did not say so.",
     "Until the composing reader existed, a clone read as **empty** --- and the statement "
     "that created it succeeded. `SHOW LINEAGE` saw the clone, re-issuing the create refused "
     "it as already there, and every `SELECT` answered *table not found* until the next "
     "restart."),
    ("03", "Nothing in the server writes into a clone today.",
     "The library composes a clone's own writes correctly --- 300 inherited rows plus 100 of "
     "its own, with the origin still seeing 300. But no SQL statement writes into one, and "
     "the ancestor flattening is only correct **because** that is true."),
    ("04", "One refusal is built and unwired.",
     "Reading a clone as of a moment **before** it was taken is refused by a function with "
     "no production caller. The plan document says time travel is refused there; the "
     "function exists, the tests exercise it, and nothing in the server calls it."),
]
yy = top
for num, title, body in CARDS:
    card(sl, ML, yy, CW, 1.24, num, title, body)
    yy += 1.32

# ------------------------------------------------------------ CH 18
chapter("18", "Lineage, dependents and diffs")

sl, top = content("Asking what still reads a table, before a drop refuses",
                  kicker="VERSIONS · THE CLIENT-FACING HALF")
code(sl, ML, top, CW * 0.49, [
    "SHOW LINEAGE OF sales.orders_wip;",
    "SHOW DEPENDENTS OF sales.orders;",
    "",
    "SHOW CHANGES BETWEEN 4 AND 9 OF sales.orders;",
    "",
    " commits | rows_added | rows_removed",
    "---------+------------+--------------",
    "       5 |      12400 |          380",
    "",
    " files_added | files_removed | compactions",
    "-------------+---------------+-------------",
    "           7 |             3 |           1",
], fs=9, title="every number read from the log, none of it estimated")
tf = txt(sl, ML + CW * 0.53, top, CW * 0.47, 3.6)
para(tf, "Compactions are counted separately, and that is the whole design.",
     size=12, color=DEEP, bold=True, first=True, space_after=8)
bullets(tf, [
    "A compaction adds files and removes files and changes **no rows**. Folding "
    "it into `files_added` would make a quiet warehouse look busy and a busy one "
    "look quiet.",
    "A drop that would break a clone refuses --- and `SHOW DEPENDENTS` is how you "
    "find that out **before** you type it, rather than by reading the refusal.",
    "A lineage that cannot be read takes opposite branches in two places: the "
    "sweeper treats it as a clone of nothing, keeping reclamation conservative; "
    "the read path treats it as not a clone and serves the table empty. That is "
    "an inconsistency, and it is written down rather than smoothed over.",
], size=10.5, gap=8)

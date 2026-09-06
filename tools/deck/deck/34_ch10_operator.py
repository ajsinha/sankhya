# ============================================================ 10 · THE OPERATOR
divider("10", "What an Operator Can Find Out",
        "Two records, one diagnostic, and the things that cannot be seen.",
        [])

sl, top = content("Two records of the same event, for two people",
                  kicker="THE SYSTEM · WHAT IS RECORDED")
h = table(sl, [
    ["", "The audit chain", "The query log"],
    ["Read by", "an investigator", "whoever is looking at a slow server"],
    ["Form", "hash-linked records, appended to a file", "one structured line per statement"],
    ["Carries", "who, which table, what shape, how many rows, under what restriction, at what version", "who, the shape, tables scanned, rows, **milliseconds**, refused or answered"],
    ["Does not carry", "the statement text — a second place the data lives", "the statement text, and not the refusal's reason either"],
], ML, top, CW, col_w=[2.1, 4.7, 4.8])
tf = txt(sl, ML, top + h + 0.24, CW, 1.4)
runs(tf, [("A refusal's detail is withheld deliberately. ", INK, True),
          ("A planner's message frequently quotes what the caller typed — that was a real "
           "disclosure, where a misspelt column returned the list of every real one. So the "
           "log says a statement was refused, and the audit says which statement it was.",
           SLATE, False)], size=11.5, line=1.3)

sl, top = content("The shape, and why it is one word",
                  kicker="THE SYSTEM · WHAT A SHAPE IS")
code(sl, ML, top, CW * 0.58, [
    "SELECT region FROM orders            ->  select",
    "SELECT nosuchcolumn FROM orders      ->  select",
    "SELECT 'a-secret-value' FROM orders  ->  select",
    "CREATE TABLE q3 CLONE sales.orders   ->  create table",
    "SHOW FEEDS                           ->  show feeds",
], fs=10, title="statement in, shape out")
tf = txt(sl, ML + CW * 0.62, top, CW * 0.38, 2.4)
para(tf, "The second word survives only when it is one of ours.",
     size=11.5, color=DEEP, bold=True, first=True, space_after=8)
para(tf, "`create table` and `show feeds` are worth telling apart. `select nosuchcolumn` is a "
         "column the caller named, and `select 'a-secret-value'` is a literal.",
     size=10.5, color=SLATE, line=1.28, space_after=8)
para(tf, "It took the first two words until a query log was written and the same helper was "
         "read again — it had been putting caller text into the durable audit since the day "
         "the audit was made durable.",
     size=10.5, color=SLATE, line=1.28)

sl, top = content("What an operator still cannot see",
                  kicker="THE SYSTEM · THE GAPS")
h = table(sl, [
    ["", "State"],
    ["Maintenance counters — ticks, failures, bytes reclaimed, tables held", "counted on the handle and **exported nowhere**"],
    ["Four of the six error codes that page", "cannot fire; the subsystems that would raise them do not run"],
    ["Replication lag", "the check is written, tested, exported and called by nothing"],
    ["The audit chain's head, continuously", "printed once at startup; nothing queries it, so truncation is not detectable the way the design assumes"],
], ML, top, CW, col_w=[6.0, 5.6])
tf = txt(sl, ML, top + h + 0.24, CW, 1.3)
runs(tf, [("A metric nothing emits and an alert that can never fire are the same failure: ",
           DEEP, True),
          ("absence rendering as health. `check-catalogues` fails when a documented code has "
           "no construction site, and the catalogue marks the twelve that cannot fire — "
           "because an alert rule written from a document should be able to fire.",
           SLATE, False)], size=11.5, line=1.3)

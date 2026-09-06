# ============================================================ 5 · THE DATE AXIS
divider("5", "The Date Axis",
        "One column, on every table, meaning one thing.",
        [])

sl, top = content("Two dates, and the question that separates them",
                  kicker="FOUNDATIONS · WHAT A DATE MEANS")
h = table(sl, [
    ["Column", "Answers", "Moves when"],
    ["`sank_data_date`", "which business day this row belongs to", "never — a restatement is a new row, not an edit"],
    ["the commit position", "when this row became visible", "every publication"],
], ML, top, CW, col_w=[2.4, 4.6, 4.6])
tf = txt(sl, ML, top + h + 0.26, CW, 2.6)
para(tf, "Every published table carries the first. That is not a convention — it is the "
         "column that makes a restatement expressible at all.",
     size=12.5, color=INK, bold=True, first=True, space_after=10, line=1.3)
runs(tf, [("Without it there is one date and it is doing two jobs. ", INK, True),
          ("A correction to Monday's figures, published on Wednesday, is either filed under "
           "Monday — losing the fact that Monday's number changed — or under Wednesday, "
           "losing the fact that it is about Monday. Neither is recoverable afterwards from "
           "the data, and both are the sort of thing an auditor asks about a year later.",
           SLATE, False)], size=12, space_after=10, line=1.3)
runs(tf, [("With both, the question has an answer: ", DEEP, True),
          ("*what did we believe about Monday, as of Tuesday?* — which is a read at a "
           "position, filtered on a business date, and is the query a reconciliation is made "
           "of.", SLATE, False)], size=12, line=1.3)

sl, top = content("What it costs, and where it is not yet true",
                  kicker="FOUNDATIONS · THE PRICE AND THE GAP")
h = table(sl, [
    ["", "State"],
    ["Carried by every table the publish path writes", "**held**, and refused at publication if absent"],
    ["Carried by rows arriving on the streaming path", "**not held** — `FR-STORE-20` is unmet there"],
    ["`_sankhya_commit_ts` on ingested rows", "written as literal `0` for every row"],
], ML, top, CW, col_w=[5.6, 6.0])
tf = txt(sl, ML, top + h + 0.26, CW, 1.8)
runs(tf, [("The gap is in the arm that does not run. ", DEEP, True),
          ("There is no change-capture runtime, so nothing arrives on the streaming path "
           "today — which makes the unmet requirement harmless now and a defect the moment "
           "capture is wired. It is recorded here rather than discovered then.",
           SLATE, False)], size=11.5, space_after=10, line=1.3)
runs(tf, [("And the cost of the column itself: ", INK, True),
          ("every table is partitioned by it whether or not a query filters on it, so a "
           "query that does not mention the date still pays for the partitioning that makes "
           "one that does fast.", SLATE, False)], size=11.5, line=1.3)

# ------------------------------------------------------------ CH 5
chapter("5", "What a statement does, end to end")

sl, top = content("One SELECT, from socket to audit",
                  kicker="THE PATH · NINE STAGES")
code(sl, ML, top, CW * 0.42,
     ["SELECT region, sum(amount)",
      "FROM sales.orders",
      "GROUP BY 1;"], fs=11, title="the statement")
tf = txt(sl, ML + CW * 0.46, top + 0.04, CW * 0.54, 1.0)
runs(tf, [("Every stage names a file. ", CRIMSON, True),
          ("This is the slide the previous deck did not have, and the one an "
           "architect asks for first: not what the system contains, but what "
           "actually happens, in order, and where to go and read it.", SLATE, False)],
     size=11, first=True, space_after=0, line=1.26)
table(sl, [
    ["#", "Stage", "Where", "What happens"],
    ["1", "Accept", "`api-pg/src/listener.rs`", "One task per connection. Past 1,024 the accept branch is *disabled*, so callers wait in the kernel backlog rather than being dropped"],
    ["2", "Protocol", "`api-pg/src/session.rs`", "A pure state machine --- no I/O, no planner. One `Handler` trait is the entire seam to the engine"],
    ["3", "Principal", "`server/src/wiring.rs`", "Empty subject refused; password verified; roles read from `server.users.<name>`. An unattributable connection cannot be audited"],
    ["4", "Pin, then admit", "`sankhya-leases`", "The lease is taken around the **resolve**, not the scan --- the window opens when a log becomes file paths"],
    ["5", "Live set", "`readpath/src/provider.rs`", "Files come from the log, never a directory listing. Coverage is synthesised as one interval"],
    ["6", "Policy, once", "`server/src/execute.rs`", "No guard, no registration --- an unreadable table fails to resolve exactly like one that never existed"],
    ["7", "Secured provider", "`catalog/src/secured.rs`", "The predicate is offered to the provider; unless it declares itself exact, a filter sits above the scan where nothing can decline it"],
    ["8", "Plan and prune", "`server/src/execute.rs`", "DDL and DML rejected on the **planned** plan, not the text. Provably irrelevant files never enter it"],
    ["9", "Stream and record", "`server/src/audit.rs`", "Shared fair pool, row cap enforced mid-stream, deadline. One audit entry per table --- on success and on refusal"],
], ML, top + 1.15, CW, col_w=[0.32, 1.35, 2.15, 7.78], row_h=0.335, fs=9,
    hfs=9.5, bold_col0=True, first_col_color=CRIMSON)


sl, top = content("What is open, and what that costs",
                  kicker="THE SYSTEM · THE FORMAT")
h = table(sl, [
    ["", "What follows"],
    ["Tables are Delta, including materialised cuboids", "Any engine that reads Delta reads them, with no SANKHYA process in the path. The fast path gets no exception — a cuboid is a published table on purpose."],
    ["The log is the table", "A commit is a file appended to `_delta_log`. Concurrency control in its entirety is: pick the next version, fail if somebody took it."],
    ["A protocol version this build cannot honour is refused", "Reader version 2 is column mapping — every column would read null. Version 3 is deletion vectors — deleted rows would be served as live. Both are answers rather than errors."],
], ML, top, CW, col_w=[4.0, 7.6])
tf = txt(sl, ML, top + h + 0.26, CW, 1.4)
runs(tf, [("The last row was not true until recently. ", DEEP, True),
          ("`Action::Protocol` was parsed out of every log and thrown away, with no ceiling "
           "anywhere in the workspace — so a table another engine had upgraded was read "
           "anyway, with this reader understanding only the parts of it that happened to "
           "look like version 1.", SLATE, False)], size=11.5, line=1.3)

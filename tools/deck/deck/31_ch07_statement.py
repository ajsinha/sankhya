# ============================================================ 7 · A STATEMENT
divider("7", "What a Statement Does",
        "One query, end to end, with nothing left out.",
        [])

sl, top = content("A statement, end to end",
                  kicker="THE SYSTEM · THE PATH")
rows = [
    ("01", "The connection", "PostgreSQL wire protocol, hand-written codec, a pure state machine. TLS negotiated inside it. Past 1,024 connections the loop stops accepting and callers wait in the kernel backlog."),
    ("02", "The principal", "Established from the startup packet. There is no anonymous construction — a principal that cannot say how it was authenticated cannot be audited."),
    ("03", "The tables", "Re-discovered if the warehouse moved, through a log cache that reads only commits arriving since the last statement."),
    ("04", "The session", "Built with **only** the tables this principal may read. A refused table is absent, not present-and-filtered."),
    ("05", "The plan", "DataFusion, on a runtime shared by every statement, with row predicates and column masks as physical operators."),
    ("06", "The answer", "Streamed and stopped at the row limit — the bound is checked while it runs, not after everything is in memory."),
    ("07", "The record", "Two of them: a hash-linked audit entry for an investigator, and one log line for whoever is looking at a slow server."),
]
y = top
for num, head, body in rows:
    rect(sl, ML, y, 0.42, 0.42, fill=DEEP)
    tf = txt(sl, ML, y + 0.06, 0.42, 0.32, align=PP_ALIGN.CENTER)
    para(tf, num, size=11, color=WHITE, bold=True, first=True, space_after=0)
    tf = txt(sl, ML + 0.58, y - 0.02, CW - 0.58, 0.5)
    runs(tf, [(head + "  ", INK, True), (body, SLATE, False)], size=10.5, line=1.22)
    y += 0.62

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

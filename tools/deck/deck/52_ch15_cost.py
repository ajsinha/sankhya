# ============================================================ 15 · THE COST
divider("15", "What It Cost, and What Is Still Open",
        "Six defects found while fixing five, and the list nobody has closed.",
        [])

sl, top = content("Fixing it found more of it",
                  kicker="THE AUDIT · WHAT THE WORK TURNED UP")
table(sl, [
    ["Found while fixing something else", "What it was"],
    ["An unreadable log replayed as an **empty table**", "`Path::exists` answers `false` for every failure, including *this directory cannot be searched*. A `_delta_log` under a half-mounted export ended the commit walk at version zero — so a `SELECT` returned no rows and **succeeded**. One `chmod` away."],
    ["The audit stopped writing to disk after 1,024 records", "Silently, with its own alert at zero. The record index was the whole-chain count; the buffer was the 1,024 window. On restart it was immediate rather than eventual."],
    ["A memory bound that was per-statement", "Ten concurrent statements got ten gibibytes while the setting said *between them*. Introduced by the item that added the bound."],
    ["A shape that leaked the caller's own text", "`select nosuchcolumn` — a column the caller named — recorded in the durable audit since the day it was made durable."],
    ["The container image misses its own baseline", "Built on Debian 11, needs `GLIBC_2.30`, declares 2.28. Found by building the image rather than shipping the Dockerfile unbuilt."],
], ML, top, CW, col_w=[3.6, 8.0], fs=9.5)

sl, top = content("What is still open, named",
                  kicker="THE AUDIT · THE REMAINDER")
h = table(sl, [
    ["", "State"],
    ["No write path from SQL", "the analytical door is read-only; a warehouse is published to by `sankhya-publish`"],
    ["No change-capture runtime", "the CDC crate carries no client dependency — it cannot open a connection"],
    ["The graph cannot answer", "registered against a freshly constructed empty catalogue on every session"],
    ["Packs cannot load", "the loader is not wired; two of the reference packs are non-financial and the flagship pair never existed"],
    ["Cube hierarchies are validated and ignored", ""],
    ["Checkpoints erase partitioning and configuration", "and an unknown action variant still ends a table"],
    ["`check-performance` is not in `check-all`", "so no automated build has ever failed on a performance budget"],
], ML, top, CW, col_w=[4.4, 7.2], fs=9.5)
tf = txt(sl, ML, top + h + 0.24, CW, 1.0)
runs(tf, [("Every one of these is written down in `docs/STATUS.md`, once. ", DEEP, True),
          ("It used to be written down in fourteen places, each holding a different piece — "
           "which is how a reader could meet six of them and conclude they had the list.",
           SLATE, False)], size=11.5, line=1.3)

# ---- closing
sl = blank()
rect(sl, 0, 0, SW, SH, fill=DEEP)
rect(sl, 0, 0, 0.20, SH, fill=DEEP_D)
tf = txt(sl, ML + 0.4, 2.5, CW - 0.8, 2.0)
para(tf, "सांख्य", size=44, color=WHITE, font=SERIF, first=True, space_after=10)
para(tf, "To count is to make completely known.", size=20, color=RGBColor(0xB9, 0xC6, 0xDD),
     font=SERIF, italic=True, space_after=18)
para(tf, "An enumeration with a gap in it is not a smaller enumeration. It is a different "
         "claim about the world, made silently — and everything in this deck is an attempt to "
         "make that impossible to do by accident.",
     size=13, color=RGBColor(0xDD, 0xE4, 0xEF), line=1.35)

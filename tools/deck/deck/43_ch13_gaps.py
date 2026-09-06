# ============================================================ 13 · THE GAPS
divider("13", "What None of It Catches",
        "The section to read if you are deciding how much to trust the rest.",
        [])

sl, top = content("Where the evidence does not reach",
                  kicker="EVIDENCE · THE HONEST LIST")
table(sl, [
    ["", "What it means"],
    ["Documentation is gated **syntactically**", "Links resolve, pins match, crate names exist, status lines agree. No gate checks a behaviour, a config key, an environment variable, a command name, or a claim that something is enforced. Every one of the 129 findings lived in that space."],
    ["A crate name inside a code fence is invisible", "`check-docs` pairs single backticks, and a fenced block is where stale architecture diagrams live."],
    ["Numbers propagate from comments into prose", "*\"299 source files publish through the atomic writer\"* travelled from a comment in the gate's own source into a chapter. Ten files import it."],
    ["Nine gate modules have no rejection test", "A check that has never been shown to fail is one nobody has shown to work."],
    ["Fifteen of eighteen objectives are unmeasured", "And the gate measuring the other three drives the engine directly — never crossing the server or the wire — so every per-statement cost is outside the measured path."],
    ["Fourteen end-to-end tests pass without running", "When PostgreSQL is not configured, which is the CI configuration. One variable makes it a failure; CI does not set it."],
], ML, top, CW, col_w=[3.5, 8.1], fs=9.5)

sl, top = content("And the gate that measured nothing",
                  kicker="EVIDENCE · THE SHARPEST ONE")
tf = txt(sl, ML, top, CW, 2.4)
para(tf, "`check-catalogues` fails when a documented error code has no construction site. It "
         "asked whether the sources contain `Error::CoverageGap {`.",
     size=12.5, color=INK, first=True, space_after=10, line=1.3)
code(sl, ML, top + 0.9, CW * 0.8, [
    "crates/sankhya-plan/src/splice.rs:151",
    "",
    "    return Err(SpliceError::CoverageGap {",
    "                     ^^^^^^^^^^^^^^^^^^^^^",
    "                     contains \"Error::CoverageGap {\"",
], fs=10, title="a different type, in a different crate")
y = top + 2.5
tf = txt(sl, ML, y, CW, 1.8)
runs(tf, [("So `SNK-S0001` read as producible while nothing in the workspace could raise it. ",
           INK, True),
          ("It is `Class::Fatal` — it pages — and a runbook had been written for it. Four of "
           "the six codes that page cannot fire; the catalogue said three.",
           SLATE, False)], size=12, space_after=10, line=1.3)
runs(tf, [("The check built to find exactly this failed at exactly this, ", DEEP, True),
          ("and it was found by a reviewer reading the gate rather than trusting it. It "
           "requires a word boundary now, with a test that another type's variant of the same "
           "name does not count.", SLATE, False)], size=12, line=1.3)

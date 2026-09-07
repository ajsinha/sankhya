# ============================================================ PART II
part_of("II")

# ------------------------------------------------------------ CH 4
chapter("4", "The shape of the whole thing")

sl, top = content("Six layers, and a rule that only points down",
                  kicker="ARCHITECTURE · THE INVENTORY")
LAYERS = [
    ("L5 · Composition root", DEEP,
     ["sankhya-server", "sankhya-cli"]),
    ("L4 · Doors and SQL surfaces", CRIMSON,
     ["api-pg", "api-flight", "api-grpc", "api-rest", "cube-sql", "graph-sql"]),
    ("L3 · Engines and subsystems", RGBColor(0x2D, 0x50, 0x16),
     ["publish", "maintenance", "cube", "graph", "olap", "functions",
      "ingest", "tiering"]),
    ("L2 · Storage and security services", RGBColor(0x1F, 0x3A, 0x5F),
     ["catalog", "readpath", "table-delta", "audit", "authz", "session",
      "snapshot", "clone"]),
    ("L1 · Pure algebra and configuration", RGBColor(0x4A, 0x3A, 0x1F),
     ["cube-algo", "graph-algo", "math", "stats", "plan", "governor",
      "config", "credential"]),
    ("L0 · Vocabulary and primitives", SLATE,
     ["types", "error", "schema", "atomicfs", "leases", "accept",
      "alloc", "version"]),
]
yy = top + 0.02
for name, col, mods in LAYERS:
    hh = 0.66 if len(mods) > 4 else 0.46
    rect(sl, ML, yy, CW, hh, fill=WHITE, line=RULE)
    rect(sl, ML, yy, 0.05, hh, fill=col)
    tf = txt(sl, ML + 0.22, yy + 0.09, 2.9, hh - 0.18)
    para(tf, name, size=10.5, color=col, bold=True, first=True, space_after=0,
         line=1.1)
    per = 4
    box = (CW - 3.35) / per - 0.09
    for j, m in enumerate(mods):
        mx = ML + 3.22 + (j % per) * ((CW - 3.35) / per)
        my = yy + 0.08 + (j // per) * 0.29
        rect(sl, mx, my, box, 0.24, fill=PARCH)
        tfm = txt(sl, mx, my + 0.035, box, 0.20, align=PP_ALIGN.CENTER)
        para(tfm, m, size=8.2, color=INK, first=True, space_after=0)
    yy += hh + 0.08
tf = txt(sl, ML, yy + 0.04, CW, 0.6)
runs(tf, [("The layer is a number in each crate's own manifest, so this stack is "
           "derived rather than maintained. ", CRIMSON, True),
          ("`cargo xtask check-layers` walks every `Cargo.toml` under `crates/` and "
           "`packs/` and fails on five distinct violations --- a missing layer, an upward "
           "dependency, core reaching into a pack, anything reaching into tooling, and a "
           "cycle. Same-layer edges are legal, so acyclicity is not implied and is checked "
           "separately.", SLATE, False)],
     size=10, first=True, space_after=0, line=1.24)

sl, top = content("Responsibility boundaries", kicker="ARCHITECTURE · WHO OWNS WHAT")
table(sl, [
    ["Component", "Owns", "Does **not** own"],
    ["Wire door", "Codec, protocol state machine, TLS negotiation, drain and connection caps", "Any session, catalogue or planner --- one `Handler` trait is the whole seam"],
    ["Columnar door", "Ticket issue and redeem, tenant and expiry admission, streamed Arrow", "Row cap, deadline, lease pin, audit record --- `execute::run` is not on this path"],
    ["Read path", "The live set from the log; statistics pruning before any footer opens", "Reading or decoding Parquet --- that is the engine's source"],
    ["Write path", "The only supported write: stage, fsync, rename, fsync the directory", "Ingestion decisions, and compaction"],
    ["Catalog", "`Guard` with no public constructor; the predicate conjoined above the scan", "A name-to-provider map. There is no catalogue proper"],
    ["Cube", "Lattice and additivity algebra, validated definitions, completeness columns", "Its own read path --- hydration goes through the caller's secured session"],
    ["Maintenance", "Tiered compaction, one budget on a strict class ladder, retirement", "Deletion during a merge. A merge never deletes"],
    ["Governor", "Admission, bounded refusal queue, tenant floors and caps, the ladder", "Any query in this build --- called with a zeroed request against `u64::MAX`"],
    ["Policy", "`Principal`, `decide`, deny-wins, filters OR'd, masks intersected", "Enforcement. The provider wrapper is the only runtime control"],
    ["Audit", "Hash-linked chain, fsynced per append, one entry per table scanned", "The statement text and the duration --- those are the query log"],
    ["Tiering", "Purge state machine, archival registry, quarantine, rehydration", "Any call site. Zero dependents, by design, gated to M9 and M11"],
    ["Graph", "Bounded typed temporal traversal, truncation reported as columns", "A write path, durability --- and, in this build, its own catalogue contents"],
], ML, top, CW, col_w=[1.55, 5.0, 5.05], row_h=0.335, fs=9, hfs=9.5,
    bold_col0=True, first_col_color=CRIMSON)

sl, top = content("Three data models, and which of them answers today",
                  kicker="ARCHITECTURE · WHAT IS REAL")
BW = (CW - 0.44) / 3
MODELS = [
    ("OLAP", "its own columnar store", "REAL", RGBColor(0x2D, 0x50, 0x16),
     ["Single-node warehouse over an open format; the Delta log written by hand "
      "with a length seal per commit, so a truncated commit is refused rather "
      "than replayed short.",
      "Planning does **no file I/O** --- 800 files in 1.37 ms against 10.33 ms "
      "for a directory listing. Statistics live in the log, so a cold process "
      "prunes like a warm one.",
      "Time travel, cross-table snapshots, zero-copy clones, cubes, maintenance "
      "on a timer, 155 functions, and the security path opposite."],
     ["**The governor bounds nothing.** The one query-path call passes a zeroed "
      "request against ceilings of `u64::MAX`.",
      "**Partition-predicate pruning is unsatisfiable** by any query, so one "
      "performance objective has an unreachable precondition.",
      "**The change-log fold is orphaned** --- correct, tested, constructed by "
      "nothing outside its own tests."]),
    ("GRAPH", "typed, temporal, bounded", "BUILT, EMPTY", RGBColor(0x8A, 0x6A, 0x12),
     ["~4,500 lines with **zero dependencies of any kind**: shortest path, "
      "k-shortest loopless, cycles, strong components, community detection, "
      "rank, betweenness, influence.",
      "Typed adjacency in both directions, half-open validity intervals, sorted "
      "by source and by time --- the edges of a vertex as of time *t* is a "
      "binary search and a slice.",
      "Every traversal hard-bounded at planning time, reporting its own "
      "truncation as columns on the row.",
      "Five table functions genuinely registered into every session."],
     ["**The catalogue is constructed empty, per session.** `GraphCatalog::new` "
      "returns an empty map, and `register`/`publish` have zero call sites in "
      "the server.",
      "So every call resolves to *no such graph*, unconditionally, for the life "
      "of the process.",
      "Carried as an **unmet** exit criterion rather than reinterpreted: correct "
      "against brute force, never timed at scale."]),
    ("OLTP", "PostgreSQL, supervised", "NOT WIRED", CRIMSON,
     ["Not linked-in PostgreSQL --- a **child process** whose whole lifecycle "
      "SANKHYA owns. 323 lines, zero dependencies, Unix socket only.",
      "Three fatal modes handled: two processes over one data directory, an "
      "orphaned child, and a dead supervisor with a live child.",
      "Capture correctness is real: a `pgoutput` decoder tested against a "
      "captured **live 17.11 stream**, a property-tested transaction invariant, "
      "and a five-rung source-safety ladder."],
     ["The supervisor is a **dev-dependency** of the server. Its only caller is "
      "a soak test that prints `SKIPPED ... nothing wires it in`.",
      "**The capture crate has zero dependents of any kind**, dev-dependencies "
      "included. Its name appears in exactly one manifest: its own.",
      "**There is no configuration surface to wire it through** --- no "
      "transactional key exists anywhere in the config crate or the shipped YAML."]),
]
for i, (name, sub, verdict, col, built, missing) in enumerate(MODELS):
    x = ML + i * (BW + 0.22)
    rect(sl, x, top, BW, 0.60, fill=col)
    tf = txt(sl, x + 0.14, top + 0.07, BW - 0.28, 0.5)
    para(tf, name, size=16, color=WHITE, bold=True, font=SERIF, first=True,
         space_after=0)
    para(tf, sub, size=8.5, color=RGBColor(0xE8, 0xE4, 0xDE), space_after=0)
    tfv = txt(sl, x + BW - 1.55, top + 0.16, 1.42, 0.3, align=PP_ALIGN.RIGHT)
    para(tfv, verdict, size=9.5, color=WHITE, bold=True, first=True, space_after=0)
    yy = top + 0.68
    rect(sl, x, yy, BW, 0.22, fill=PARCH)
    tfh = txt(sl, x + 0.12, yy + 0.02, BW - 0.24, 0.19)
    para(tfh, "BUILT", size=8, color=col, bold=True, first=True, space_after=0)
    yy += 0.28
    tfb = txt(sl, x + 0.10, yy, BW - 0.20, 2.3)
    bullets(tfb, built, size=8.2, gap=5, bullet_color=col)
    yy += 2.32 if len(built) > 3 else 1.92
    rect(sl, x, yy, BW, 0.22, fill=RGBColor(0xF4, 0xE4, 0xE4))
    tfh = txt(sl, x + 0.12, yy + 0.02, BW - 0.24, 0.19)
    para(tfh, "NOT WIRED", size=8, color=CRIMSON, bold=True, first=True,
         space_after=0)
    tfm = txt(sl, x + 0.10, yy + 0.28, BW - 0.20, 1.7)
    bullets(tfm, missing, size=8.2, gap=5, bullet_color=CRIMSON)

sl, top = content("Eight rules, and the check that enforces each",
                  kicker="ARCHITECTURE · WHAT BINDS IT")
RULES = [
    ["", "Rule", "In one sentence", "Enforced by"],
    ["R1", "One-way dependencies", "A crate depends on a lower layer or its own; never upward, never on a pack or on tooling; acyclic", "`check-layers`"],
    ["R2", "Nothing ships unreached", "Every crate is reachable from a shipping binary, or listed with a milestone and a reason", "`check-surfaces`"],
    ["R3", "One writer to a warehouse", "Only the publish and maintenance crates may write or commit --- **tests included**", "`check-writers`"],
    ["R4", "Visible only when durable", "Publish stages and renames; every writer a commit points at syncs its bytes **and** its directory entry", "`check-atomic-writes`, `check-durability`"],
    ["R5", "No caller data in logs", "No log statement records what a caller supplied, and an instrumented function must skip its arguments", "`check-logging`"],
    ["R6", "Every number has a producer", "A speed ratio in any document names a resolving benchmark, a recorded measurement, or why it cannot be re-run", "`check-benchmarks`"],
    ["R7", "No paper-only catalogue entry", "A documented error code is constructible or declared unreachable with a printed reason; every metric is recorded", "`check-catalogues`"],
    ["R8", "Grace outlives drain", "A deployment manifest's termination grace must exceed the wire door's drain deadline", "`check-package`, which parses the constant out of the source"],
]
h = table(sl, RULES, ML, top, CW,
          col_w=[0.5, 2.15, 6.25, 2.7], row_h=0.40, fs=9.5, hfs=10,
          bold_col0=True, first_col_color=CRIMSON)
note(sl, ML, top + h + 0.22, CW, 0.95,
     "The gate table in the testing document is generated, not written. ",
     "`cargo xtask gate-table` prints it from the dispatch arms, and `check-docs` fails when "
     "the document disagrees with what that prints --- which is how three documents came to "
     "say twenty checks when there were twenty-seven, and how they stopped.")

sl, top = content("The seams, and what crosses each",
                  kicker="ARCHITECTURE · WHERE IT MEETS")
SEAMS = [
    ("Cube \u2194 read path", "a secured session, not a second reader",
     "Cube hydration calls `context.table(...)` on the caller's **already-secured** session "
     "and never opens a Parquet file. So a cube cell is filtered by the same wrapper that "
     "filters a plain `SELECT` --- one implementation of the rule, with no second one to "
     "disagree with it. It streams rather than collects, a fix worth 5.6 GB against 779 MB, "
     "and results re-enter SQL as ordinary table providers joinable in a `FROM` clause."),
    ("Maintenance \u2194 read path", "three kinds of pin, all failing safe",
     "A statement takes a **lease** around the resolve; the sweeper asks one question --- "
     "has every reader that started before this mark finished? --- against the **same** "
     "registry object, because two registries would let the sweeper consult one nobody "
     "announces into. A snapshot pins a *position*; a clone pins a *table version*. A merge "
     "never deletes: retirement needs the replacement's row count re-read from its own "
     "footer, no pin resolving to the input, and a grace period elapsed."),
    ("Clone \u2194 read path", "an ancestor walk, then two logs",
     "A clone's log names none of the origin's files, so a read resolves the origin's live "
     "set at the pinned version and then the clone's own log. A clone of a clone would "
     "splice against a log naming nothing, so the walk **flattens the chain** to the first "
     "ancestor that holds files --- bounded at 64 against a hand-edited cycle. An empty file "
     "set is indistinguishable from version zero, so the presence of a *log* is the signal."),
    ("The splice", "present, called, and given one interval",
     "A greedy provably-minimal interval cover runs on **every query**, with a debug "
     "assertion that the chosen tiers cover the span exactly once. The server synthesises "
     "what it feeds in --- a single interval from zero to the target --- at seven call sites. "
     "The algorithm has never had more than one interval to cover, and the crate that would "
     "supply the second has zero dependents in the workspace, by design, gated to M9 and M11."),
]
yy = top
for name, sub, body in SEAMS:
    rect(sl, ML, yy, CW, 1.15, fill=WHITE, line=RULE)
    rect(sl, ML, yy, 0.045, 1.15, fill=CRIMSON)
    tfn = txt(sl, ML + 0.20, yy + 0.11, 2.65, 0.85)
    para(tfn, name, size=11.5, color=DEEP, bold=True, font=SERIF, first=True,
         space_after=2, line=1.12)
    para(tfn, sub, size=8.8, color=CRIMSON, italic=True, space_after=0, line=1.15)
    tfb = txt(sl, ML + 3.00, yy + 0.11, CW - 3.20, 0.95)
    para(tfb, body, size=9.2, color=SLATE, first=True, space_after=0, line=1.24)
    yy += 1.24

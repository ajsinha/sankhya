# ============================================================ PART V
part_of("V")

# ------------------------------------------------------------ CH 20
chapter("20", "The graph engine")

sl, top = content("There is no graph write path, and that is the design",
                  kicker="GRAPH · THE SHAPE")
tf = txt(sl, ML, top, CW, 0.62)
runs(tf, [("An epoch is hydrated by scanning published tables, and records the "
           "snapshot it was built from. ", CRIMSON, True),
          ("No separate store, no write path, no durability of its own --- and four "
           "consequences, each answering a question a graph database has to keep answering.",
           SLATE, False)], size=11.5, first=True, space_after=0, line=1.26)
steps(sl, ML, top + 0.70, CW, [
    ("01", "No dual write", "The graph cannot disagree with the tables, because it *is* the tables."),
    ("02", "No reindex", "A stale epoch is replaced, never repaired. Rebuilding is the only operation."),
    ("03", "As-of is free", "An epoch names its snapshot, so a historical traversal is a different epoch."),
    ("04", "No graph backup", "Backing up the tables backs up the graph. There is nothing else to lose."),
], h=1.5)
y = top + 2.36
tf = txt(sl, ML, y, CW * 0.49, 2.1)
para(tf, "The adjacency, and why the traversal is cheap.", size=11.5,
     color=DEEP, bold=True, first=True, space_after=7)
para(tf, "Typed vertices, typed edges, per-edge-type adjacency **in both "
         "directions**, half-open validity intervals sorted by source and by time. "
         "So *the edges of this vertex as of time t* is a binary search and a "
         "slice --- not a scan, and not an index to maintain.",
     size=10.5, color=SLATE, line=1.3, space_after=0)
tf = txt(sl, ML + CW * 0.53, y, CW * 0.47, 2.1)
para(tf, "Time-respecting traversal is a function, not a flag.", size=11.5,
     color=DEEP, bold=True, first=True, space_after=7)
para(tf, "Static reachability over-reports --- always in that direction --- because "
         "it will happily use an edge that closed before the one leading to it "
         "opened. **The optimistic answer looks exactly like the correct one.** A "
         "flag on a function makes the wrong answer the default for anybody who "
         "does not know to set it, so it is a separate name.",
     size=10.5, color=SLATE, line=1.3, space_after=0)

sl, top = content("Complete, correct, bounded --- and structurally empty",
                  kicker="GRAPH · WHAT RUNS")
h = table(sl, [
    ["What is built", "Where"],
    ["Shortest path, k-shortest loopless, cycles", "`graph-algo/src/paths.rs`"],
    ["Weakly and strongly connected components", "`graph-algo/src/components.rs`"],
    ["Community detection, modularity", "`graph-algo/src/community.rs`"],
    ["Degree, rank, betweenness estimate", "`graph-algo/src/centrality.rs`"],
    ["Influence, influence between", "`graph-algo/src/product.rs`"],
    ["CSR adjacency, interning, and the budget model", "`csr.rs` `ids.rs` `budget.rs`"],
], ML, top, CW * 0.52, col_w=[3.6, 2.0], fs=9.5, hfs=10)
tf = txt(sl, ML, top + h + 0.20, CW * 0.52, 1.1)
runs(tf, [("~4,500 lines with zero dependencies of any kind. ", DEEP, True),
          ("Every traversal is hard-bounded at planning time --- default a thousand "
           "results --- and reports its own truncation as columns on the row, so a "
           "partial answer cannot be mistaken for a complete one.", SLATE, False)],
     size=10, first=True, space_after=0, line=1.26)

x2 = ML + CW * 0.56
rect(sl, x2, top, CW * 0.44, 0.42, fill=CRIMSON)
tfh = txt(sl, x2 + 0.14, top + 0.08, CW * 0.44 - 0.28, 0.3)
para(tfh, "AND YET IT ANSWERS NOTHING", size=10.5, color=WHITE, bold=True,
     first=True, space_after=0)
tf = txt(sl, x2, top + 0.55, CW * 0.44, 3.4)
bullets(tf, [
    "Five table functions are **genuinely registered** into every session. The "
    "graph crates are real dependencies of the server, which is why neither "
    "appears on the unreached list.",
    "But the session registers them against a **freshly constructed, empty "
    "catalogue**, and the two methods that would put a graph into it have zero "
    "call sites in the server.",
    "So every call reaches the same branch and returns *no such graph*, "
    "unconditionally, for the life of the process.",
    "The server's own suite concedes it: the guide test marks the graph "
    "functions **needs a hydrated graph**.",
    "The throughput benchmark is carried as an **unmet exit criterion** rather "
    "than reinterpreted. Correct against brute force, bounded by construction, "
    "never timed at scale.",
], size=10, gap=7, bullet_color=CRIMSON)

# ------------------------------------------------------------ CH 21
chapter("21", "The transactional tier and the capture bridge")

sl, top = content("Not embedded PostgreSQL --- a child process it owns",
                  kicker="OLTP · THE WORD THAT WAS WRONG")
tf = txt(sl, ML, top, CW, 0.85)
runs(tf, [("\u201cEmbedded\u201d was the wrong word and was settled as a decision "
           "rather than edited away. ", CRIMSON, True),
          ("PostgreSQL is not linked into this binary; it is a child process whose "
           "whole lifecycle SANKHYA owns --- locate the binaries, initialise, launch on a "
           "Unix socket with no TCP surface at all, poll until ready, shut down. 323 lines, "
           "one file, zero dependencies.", SLATE, False)],
     size=11.5, first=True, space_after=0, line=1.26)
h = table(sl, [
    ["The fatal mode", "What it does"],
    ["Two processes over one data directory", "Takes the lock first. The second refuses to start rather than corrupting"],
    ["An orphaned child from a previous run", "Adopts it, restarts it, or clears it --- decided, not guessed"],
    ["A dead supervisor with a live child", "Parent-death signalling **and** a boot check, because the signal is not enough alone"],
], ML, top + 0.95, CW * 0.60, col_w=[2.6, 4.0], fs=9.5, hfs=10)
tf = txt(sl, ML, top + 0.95 + h + 0.22, CW * 0.60, 0.9)
runs(tf, [("Connection pooling and a client library are deliberately absent, ", DEEP, True),
          ("recorded as an open decision rather than one made here.", SLATE, False)],
     size=10.5, first=True, space_after=0, line=1.26)

x2 = ML + CW * 0.64
listbox(sl, x2, top + 0.95, CW * 0.36, "The bridge, all of it built", [
    ("A `pgoutput` decoder, tested against a captured", SLATE, ""),
    ("**live PostgreSQL 17.11 replication stream**", DEEP, ""),
    ("An apply path with a property-tested", SLATE, ""),
    ("transaction invariant", SLATE, ""),
    ("Lossless type mapping over 73 columns", SLATE, ""),
    ("Reconciliation, idempotence, crash safety", SLATE, ""),
    ("Schema evolution", SLATE, ""),
    ("A five-rung source-safety ladder,", SLATE, ""),
    ("validated against a live replication slot", SLATE, ""),
], accent=RGBColor(0x2D, 0x50, 0x16), row=0.225)

sl, top = content("One absence, not six",
                  kicker="OLTP · THE GAP, PRECISELY")
tf = txt(sl, ML, top, CW, 0.95)
runs(tf, [("Everything on the previous slide is written, tested, and driven by nothing. ",
           CRIMSON, True),
          ("It is tempting to read that as six missing subsystems. It is one: there is no "
           "driver that runs them on a timer. That is the whole distance between this system "
           "and the one the architecture describes --- and it is a deliberately unglamorous "
           "next milestone.", SLATE, False)],
     size=12, first=True, space_after=0, line=1.28)
CARDS = [
    ("01", "The supervisor is a dev-dependency of the server.",
     "Its only caller is a soak test, whose output reads `SKIPPED ... nothing wires it in`."),
    ("02", "The capture crate has zero dependents of any kind.",
     "Dev-dependencies included. Its name appears in exactly one manifest: its own."),
    ("03", "There is no configuration surface to wire it through.",
     "No transactional key exists in the configuration crate or the shipped YAML."),
    ("04", "The consequences chain, and must not be blurred.",
     "No capture runtime means no arrival tier, so no read-your-own-writes, so no strong "
     "read mode. One absence, four capabilities."),
]

yy = top + 0.58
for num, title, body in CARDS:
    card(sl, ML, yy, CW, 1.00, num, title, body)
    yy += 1.10

# ------------------------------------------------------------ CH 22
chapter("22", "One catalogue, on every path")

sl, top = content("A position with a vector and a matrix in the same row",
                  kicker="ONE CATALOGUE · COLUMNS THAT ARE NOT SCALARS")
code(sl, ML, top, CW * 0.53, [
    "SELECT position_id, book,",
    "       vec_quantile(pnl, vec_of(0.05)) AS var_95,",
    "       vec_quantile(pnl, vec_of(0.01)) AS var_99,",
    "       vec_stddev(pnl)                 AS volatility,",
    "       vec_min(pnl)                    AS worst_outcome",
    "FROM risk.positions ORDER BY var_95;",
    "",
    "-- portfolio variance through the stored matrix",
    "SELECT vec_dot(w, mat_vec(covariance, w)) AS variance,",
    "       mat_is_positive_definite(covariance) AS could_be_data",
    "FROM risk.positions, (SELECT vec_of(.25,.25,.25,.25) w);",
], fs=8.8, title="risk.positions: a 64-outcome P&L vector and a 4x4 covariance, per row")
tf = txt(sl, ML + CW * 0.57, top, CW * 0.43, 3.6)
para(tf, "Shape is part of the type.", size=12, color=DEEP, bold=True,
     first=True, space_after=8)
bullets(tf, [
    "A width survives storage in a metadata key of its own, so a kernel takes a "
    "**contiguous slice** rather than copying per row --- 14.3x at width 8.",
    "A matrix's shape survives in Arrow's canonical tensor extension, so an "
    "engine that understands tensors understands these columns.",
    "`mat_is_positive_definite` answers by **attempting the factorisation**, "
    "which is the definition rather than a proxy. A covariance matrix that will "
    "not factor is one no data could have produced.",
    "Vectors cross the wire as `float8[]`, the type every PostgreSQL driver "
    "already decodes --- so the correct thing also deletes code from every client.",
], size=10, gap=7)

sl, top = content("The defect this found, and why it read as three awkward functions",
                  kicker="ONE CATALOGUE · A STORED MATRIX THAT WAS NOT ONE")
tf = txt(sl, ML, top, CW, 1.3)
runs(tf, [("The Delta writer kept one metadata key of its own and discarded every "
           "other key the column declared. ", CRIMSON, True),
          ("So the tensor extension was written and dropped, and a stored matrix came back "
           "not being a matrix. The failure had no symptom worth noticing: the table read "
           "perfectly, and every function that can **deduce** a square order from the length "
           "--- determinant, trace, inverse, solve, Cholesky, eigen --- answered correctly.",
           SLATE, False)], size=11.5, first=True, space_after=10, line=1.28)
runs(tf, [("Only `mat_transpose`, `mat_multiply` and `mat_vec` refused: the three "
           "rectangular operations that need a shape they could not have. ", DEEP, True),
          ("Which reads as three awkward functions, not as a storage defect.", SLATE, False)],
     size=11.5, space_after=0, line=1.28)
note(sl, ML, top + 1.45, CW, 1.05,
     "Found by the parity soak, which is the point of this chapter. ",
     "It calls every function over a **stored column** and against a literal of the same "
     "values, on three paths --- SQL, Flight SQL and the SDK. The two routes disagreed, and "
     "only one of them could be right. A user does not care whether an answer came from the "
     "transactional tier, the analytical one or the binding, and the experience must not "
     "depend on it --- so the comparison is the test.")
tf = txt(sl, ML, top + 2.70, CW, 1.5)
runs(tf, [("The rule that came out of it: a column's metadata is part of the column. ",
           CRIMSON, True),
          ("Written with the schema and restored with it, whatever the key. The fixed length "
           "is the one deliberate exception, and only because it is a **rendering of the "
           "type** --- derived on write, dropped on read, so the two cannot come to disagree "
           "about how wide a column is.", SLATE, False)],
     size=11.5, first=True, space_after=0, line=1.28)

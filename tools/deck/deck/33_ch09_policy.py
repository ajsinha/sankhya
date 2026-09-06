# ============================================================ 9 · WHAT IT MAY SEE
divider("9", "What a Statement Is Allowed to See",
        "Rows, columns, and the catalogue — one mechanism, three surfaces.",
        [])

sl, top = content("Three things a policy governs",
                  kicker="THE SYSTEM · POLICY")
h = table(sl, [
    ["Surface", "How", "Why it is not advice"],
    ["Which tables exist", "an unauthorised table is never registered in the session", "a query naming it fails to resolve, rather than planning and returning nothing — which is indistinguishable from an empty table"],
    ["Which rows", "a predicate spliced into the plan as a physical operator", "a filter the optimiser could reorder past is a filter that sometimes does not run"],
    ["Which columns", "a masking expression wrapping the projection", "`concat` treats null as empty string, so it would turn a null e-mail into `***` — a value where there was none"],
], ML, top, CW, col_w=[2.4, 3.6, 5.6])
tf = txt(sl, ML, top + h + 0.26, CW, 1.5)
runs(tf, [("A masked column also stops being a filter target. ", INK, True),
          ("Pushing a predicate on a masked column down to the scan would test the real value "
           "and reveal it through which rows came back — so the table reports those filters as "
           "unsupported and the plan keeps them above the mask.",
           SLATE, False)], size=11.5, line=1.3)

sl, top = content("The posture as shipped, stated plainly",
                  kicker="THE SYSTEM · WHAT IS ON BY DEFAULT")
h = table(sl, [
    ["", "Shipped default", "Consequence"],
    ["TLS", "**off**", "the wire is plain unless a certificate is configured; there is no environment variable for it, so a container configured purely by environment cannot turn it on"],
    ["Passwords", "required, verifier list empty", "no credential matches, so every login is refused until one is configured — the safe direction, and not obvious"],
    ["`/metrics`", "**unauthenticated**", "bound to loopback in the shipped unit; a collector reaches it through the pod boundary or a sidecar"],
    ["The columnar door", "identity from an unverified header", "Flight takes a `sankhya-user` header and never binds it to a client certificate, even under mutual TLS"],
], ML, top, CW, col_w=[1.9, 2.4, 7.3])
tf = txt(sl, ML, top + h + 0.24, CW, 1.0)
runs(tf, [("Each of these was documented honestly, and separately. ", DEEP, True),
          ("A security reviewer needs them on one page, which is what `docs/SECURITY.md` is "
           "for — the assembly is the finding, not any single row.",
           SLATE, False)], size=11.5, line=1.3)

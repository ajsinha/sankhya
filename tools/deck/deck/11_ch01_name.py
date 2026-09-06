# ============================================================ 1 · THE NAME
divider("1", "The Name, and What It Is For",
        "सांख्य --- to count is to make completely known.",
        [])

sl, top = content("सांख्य — enumeration, not arithmetic",
                  kicker="THE NAME · WHAT IT MEANS")
rect(sl, ML, top, 2.6, 1.5, fill=PARCH, line=RULE)
tf = txt(sl, ML + 0.16, top + 0.28, 2.28, 1.0, align=PP_ALIGN.CENTER)
para(tf, "सांख्य", size=40, color=DEEP, font=SERIF, first=True, space_after=0)

tf = txt(sl, ML + 2.9, top, CW - 2.9, 2.4)
para(tf, "Sāṅkhya is the oldest of the six darśanas — the enumerationist school. Its method "
         "is not calculation. It is the exhaustive listing of what there is, on the argument "
         "that a thing is known when everything it is composed of has been named and nothing "
         "has been left out.",
     size=12.5, color=INK, first=True, line=1.32, space_after=8)
para(tf, "The root साङ्ख्य carries both senses at once: to count, and to make completely "
         "known. They are the same act. An enumeration with a gap in it is not a smaller "
         "enumeration — it is a different claim about the world, made silently.",
     size=12.5, color=INK, line=1.32, space_after=8)
para(tf, "Which is what an analytical warehouse is for, and what goes wrong with one.",
     size=12.5, color=DEEP, bold=True, line=1.32)

y = top + 2.6
rect(sl, ML, y, CW, 0.012, fill=RULE)
tf = txt(sl, ML, y + 0.18, CW, 1.4)
runs(tf, [("A total that omits a partition is not a smaller total. ", INK, True),
          ("It is a different number, and it does not say so. Every position in this deck "
           "follows from taking that seriously: a system that counts must be able to say what "
           "it counted over, and must refuse rather than answer when it cannot.",
           SLATE, False)], size=12, line=1.32)

sl, top = content("The mark, and what it is reading",
                  kicker="THE NAME · THE MARK")
MARK = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "..",
                    "docs", "assets", "mark-dice.png")
if os.path.exists(MARK):
    sl.shapes.add_picture(MARK, In(ML + 0.5), In(top + 0.15), In(2.0), In(2.0))

tf = txt(sl, ML + 3.1, top, CW - 3.1, 2.6)
para(tf, "A die, read three ways.", size=13, color=DEEP, bold=True, first=True, space_after=10)
for lead, rest in [
    ("The faces are a cube. ",
     "Which is the shape of the analytical model here: dimensions crossed with measures, "
     "sliced and rolled up."),
    ("The pips are a count. ",
     "The oldest counting mark there is, and the thing the name means."),
    ("The die is also a refusal. ",
     "It has six faces and no seventh. A shape that cannot express a value does not "
     "approximate it."),
]:
    runs(tf, [(lead, INK, True), (rest, SLATE, False)], size=11.5, space_after=8, line=1.28)

y = top + 2.75
note(sl, ML, y, CW, 0.95,
     "The mark is generated, not drawn. ",
     "`tools/make-logo.py` produces the wordmark, the favicon, the dark variant and this "
     "deck's palette from one source, for the same reason the metric catalogue is generated: "
     "so that they cannot drift apart. Change a proportion and every artefact changes with it.")

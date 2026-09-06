# ============================================================ TITLE
_state["n"] = 0
sl = blank()
rect(sl, 0, 0, SW, SH, fill=WHITE)
rect(sl, 0, 0, SW, 4.35, fill=DEEP)
rect(sl, 0, 4.35, SW, 0.06, fill=AMBER)
rect(sl, 0, 0, 0.20, 4.35, fill=DEEP_D)

# The mark the README already carries, in its dark variant, because the band
# behind it is dark. A deck drawing its own mark would be a second identity.
MARK = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..",
                    "..", "docs", "assets", "mark-dice.png")
if os.path.exists(MARK):
    sl.shapes.add_picture(MARK, In(ML + 0.30), In(0.70), In(0.80), In(0.80))

PALE = RGBColor(0xB9, 0xC6, 0xDD)
FAINT = RGBColor(0xDD, 0xE4, 0xEF)

tf = txt(sl, ML + 1.24, 0.99, CW - 1.04, 0.34)
para(tf, "SANKHYA  ·  सांख्य  ·  AN ANALYTICAL WAREHOUSE THAT COUNTS",
     size=11, color=PALE, bold=True, first=True, space_after=0)

tf = txt(sl, ML + 0.3, 1.52, CW * 0.86, 2.2)
para(tf, "Architecture and Evidence", size=38, color=WHITE, font=SERIF,
     first=True, space_after=2)
para(tf, "What it does, and how you can tell", size=38, color=WHITE, font=SERIF,
     space_after=4)

rect(sl, ML + 0.3, 3.32, 1.7, 0.035, fill=PALE)
tf = txt(sl, ML + 0.3, 3.56, CW * 0.84, 0.8)
para(tf, "The name and what it commits to, the mathematics that has to hold, "
         "the system that holds it, the evidence for every claim in here — and "
         "an audit that found a hundred and twenty-nine things anyway",
     size=13.5, color=FAINT, italic=True, first=True, space_after=0, line=1.25)

tf = txt(sl, ML + 0.3, 4.86, CW * 0.50, 1.2)
para(tf, "Ashutosh Sinha", size=20, color=INK, bold=True, font=SERIF,
     first=True, space_after=3)
para(tf, "Independent Researcher", size=12, color=DEEP, space_after=1)
para(tf, "September 2026", size=10.5, color=MUTED)

x0 = ML + CW * 0.50
tf = txt(sl, x0, 4.72, CW * 0.50, 2.4)
para(tf, "IN FIVE PARTS", size=9.5, color=DEEP, bold=True, first=True,
     space_after=6)
for numeral, name, detail in [
    ("I", "The name", "what counting commits you to, and the price"),
    ("II", "Foundations", "determinism and the date axis, as tests not claims"),
    ("III", "The system", "storage, the read path, cubes, policy, operability"),
    ("IV", "Evidence", "the gates, the mutations, and what is measured"),
    ("V", "The audit", "a hundred and twenty-nine findings, and what they cost"),
]:
    runs(tf, [(f"{numeral:<4}", DEEP, True), (f"{name}  ", INK, True),
              (detail, SLATE, False)], size=10, space_after=4)

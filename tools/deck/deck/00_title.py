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
para(tf, "SANKHYA  ·  सांख्य  ·  ONE BINARY  ·  THREE DATA MODELS  ·  ONE COPY OF THE DATA",
     size=10, color=PALE, bold=True, first=True, space_after=0)

tf = txt(sl, ML + 0.3, 1.42, CW * 0.90, 2.3)
para(tf, "An Integrated OLTP, OLAP", size=33, color=WHITE, font=SERIF,
     first=True, space_after=2)
para(tf, "and Graph Data Platform", size=33, color=WHITE, font=SERIF,
     space_after=6)
para(tf, "Concepts, Architecture and Implementation", size=17, color=PALE,
     font=SERIF, space_after=4)

rect(sl, ML + 0.3, 3.42, 1.7, 0.035, fill=PALE)
tf = txt(sl, ML + 0.3, 3.62, CW * 0.88, 0.7)
para(tf, "One substrate — one catalogue, one enforcement point, one date axis, "
         "one open format, one audit chain — and the three data models built over "
         "it. What a statement does, what a cube actually is, what a clone costs, "
         "and the machine-checked list of what does not run yet",
     size=12, color=FAINT, italic=True, first=True, space_after=0, line=1.22)

tf = txt(sl, ML + 0.3, 4.86, CW * 0.50, 1.2)
para(tf, "Ashutosh Sinha", size=20, color=INK, bold=True, font=SERIF,
     first=True, space_after=3)
para(tf, "Independent Researcher", size=12, color=DEEP, space_after=1)
para(tf, "September 2026", size=10.5, color=MUTED)

x0 = ML + CW * 0.50
tf = txt(sl, x0, 4.72, CW * 0.50, 2.4)
para(tf, "IN SIX PARTS", size=9.5, color=DEEP, bold=True, first=True,
     space_after=5)
# The one source for this list and for every part divider's chapter list.
# Two hand-kept lists is how the previous deck came to promise a storage
# chapter in Part III and put cubes in Part II.
for numeral, name, detail in PARTS:
    runs(tf, [(f"{numeral:<5}", DEEP, True), (f"{name}  ", INK, True),
              (detail, SLATE, False)], size=9.5, space_after=3)

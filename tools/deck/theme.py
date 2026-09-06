"""
SANKHYA — to count is to make completely known
Copyright © 2026 Ashutosh Sinha <ajsinha@gmail.com>. All rights reserved.
Proprietary and confidential. See LICENSE and NOTICE at the repository root.
"""
# -*- coding: utf-8 -*-
"""The deck design system.

# Why the layout primitives are here rather than at the call sites

`python-pptx` neither measures nor wraps text, and a PowerPoint shape does not clip what
overflows it --- text simply spills over whatever is beneath. Hand-estimated box sizes
therefore fail *silently*, which is the same failure mode this repository spent a hundred
and twenty-nine findings on: something that looks right and is not, with nothing checking.

So the primitives measure. `est_lines` simulates greedy wrapping, `table` returns the
height it actually rendered so a caller can place what follows from it rather than from a
guess, and `card` shrinks type until it fits. `audit.py` re-derives every shape's geometry
afterwards and must report nothing.

# The palette is the one the mark already uses

`tools/make-logo.py` established ink, deep blue, teal, amber and paper, and the wordmark on
the README is drawn in them. A deck in a different palette would be a second identity for
the same system, which is the drift these generators exist to prevent.
"""
from pptx import Presentation
from pptx.util import Inches as In, Pt, Emu
from pptx.dml.color import RGBColor
from pptx.enum.text import PP_ALIGN, MSO_ANCHOR
from pptx.enum.shapes import MSO_SHAPE

# ---------------------------------------------------------------- palette
# The mark's palette, from `tools/make-logo.py`. Changed there, changed here.
DEEP      = RGBColor(0x1E, 0x3A, 0x6E)   # the primary: deep blue
DEEP_D    = RGBColor(0x14, 0x28, 0x4E)   # deeper, for fills behind light type
DEEP_L    = RGBColor(0x5C, 0x77, 0xA8)   # lighter, for secondary rules
INK       = RGBColor(0x0E, 0x17, 0x26)
SLATE     = RGBColor(0x44, 0x4F, 0x60)
MUTED     = RGBColor(0x78, 0x83, 0x95)
RULE      = RGBColor(0xD9, 0xD8, 0xD3)
PARCH     = RGBColor(0xFB, 0xFA, 0xF7)   # paper
PARCH_D   = RGBColor(0xEE, 0xEC, 0xE6)
WHITE     = RGBColor(0xFF, 0xFF, 0xFF)
TEAL      = RGBColor(0x2F, 0x8F, 0x86)   # secondary data colour
AMBER     = RGBColor(0xE0, 0xA3, 0x3C)

# The generator was adapted from a companion deck built in Harvard Crimson, and these three
# names are what its layout primitives refer to. Aliased rather than renamed at four hundred
# call sites: the alias is one line and says what happened, and a bulk rename is a diff
# nobody can read against the original.
CRIMSON   = DEEP
CRIMSON_D = DEEP_D
CRIMSON_L = DEEP_L
NAVY      = TEAL
GOLD      = AMBER

SERIF = "Georgia"
SANS  = "Calibri"

SW, SH = 13.333, 7.5
ML, MR = 0.85, 0.85
CW = SW - ML - MR

prs = Presentation()
prs.slide_width  = In(SW)
prs.slide_height = In(SH)
BLANK = prs.slide_layouts[6]

_state = {"chapter": "", "n": 0}

# ---------------------------------------------------------------- helpers
# ------------------------------------------------------------- text metrics
SAFETY = 0.94        # treat boxes as slightly narrower than they are

def est_lines(text, width_in, fs, bold=False, font=SANS):
    """Greedy word-wrap simulation. Returns the number of rendered lines."""
    text = str(text)
    if not text:
        return 1
    # empirical average glyph advance as a fraction of point size
    frac = 0.505 if font == SANS else 0.545
    if bold:
        frac += 0.022
    char_in = fs * frac / 72.0
    cpl = max(4, int(width_in / char_in))
    lines, cur = 1, 0
    for w in text.split():
        need = len(w) + (1 if cur else 0)
        if cur + need > cpl:
            lines += 1
            cur = len(w)
            while cur > cpl:          # a single very long token
                lines += 1
                cur -= cpl
        else:
            cur += need
    return lines

def text_h(text, width_in, fs, bold=False, font=SANS, line=1.22):
    """Rendered height in inches. `line` multiplies the intrinsic line box (~1.2 x size),
    which is how PowerPoint applies line_spacing -- not the raw point size."""
    return est_lines(text, width_in, fs, bold, font) * fs * line * 1.10 / 72.0

def fit_size(text, width_in, height_in, start, floor=8.0, bold=False, font=SANS, line=1.22):
    """Largest font size <= start at which `text` fits in the given box."""
    fs = start
    while fs > floor and text_h(text, width_in, fs, bold, font, line) > height_in:
        fs -= 0.5
    return fs

def blank():
    return prs.slides.add_slide(BLANK)

def rect(sl, x, y, w, h, fill=None, line=None, lw=1.0):
    s = sl.shapes.add_shape(MSO_SHAPE.RECTANGLE, In(x), In(y), In(w), In(h))
    if fill is None:
        s.fill.background()
    else:
        s.fill.solid(); s.fill.fore_color.rgb = fill
    if line is None:
        s.line.fill.background()
    else:
        s.line.color.rgb = line; s.line.width = Pt(lw)
    s.shadow.inherit = False
    if s.has_text_frame:
        s.text_frame.word_wrap = True
        s.text_frame.margin_left = s.text_frame.margin_right = In(0.12)
        s.text_frame.margin_top = s.text_frame.margin_bottom = In(0.07)
    return s

def txt(sl, x, y, w, h, align=PP_ALIGN.LEFT, anchor=MSO_ANCHOR.TOP):
    tb = sl.shapes.add_textbox(In(x), In(y), In(w), In(h))
    tf = tb.text_frame
    tf.word_wrap = True
    tf.margin_left = tf.margin_right = tf.margin_top = tf.margin_bottom = 0
    tf.vertical_anchor = anchor
    tf.paragraphs[0].alignment = align
    return tf

def para(tf, text, size=14, color=INK, bold=False, font=SANS, italic=False,
         space_before=0, space_after=6, first=False, align=None, line=None):
    p = tf.paragraphs[0] if first else tf.add_paragraph()
    if align is not None: p.alignment = align
    p.space_before = Pt(space_before); p.space_after = Pt(space_after)
    if line: p.line_spacing = line
    r = p.add_run(); r.text = text
    r.font.size = Pt(size); r.font.bold = bold; r.font.italic = italic
    r.font.color.rgb = color; r.font.name = font
    return p

def runs(tf, parts, size=14, space_before=0, space_after=6, first=False, line=None):
    """parts = [(text, color, bold, italic|None)]"""
    p = tf.paragraphs[0] if first else tf.add_paragraph()
    p.space_before = Pt(space_before); p.space_after = Pt(space_after)
    if line: p.line_spacing = line
    for t in parts:
        text, color, bold = t[0], t[1], t[2]
        ital = t[3] if len(t) > 3 else False
        fnt  = t[4] if len(t) > 4 else SANS
        r = p.add_run(); r.text = text
        r.font.size = Pt(size); r.font.bold = bold; r.font.italic = ital
        r.font.color.rgb = color; r.font.name = fnt
    return p

def bullets(tf, items, size=14, gap=9, color=INK, bullet_color=CRIMSON, indent_size=None):
    for it in items:
        if isinstance(it, tuple):
            head, body = it
            p = tf.add_paragraph(); p.space_after = Pt(3); p.space_before = Pt(gap)
            r = p.add_run(); r.text = "▪  "
            r.font.size = Pt(size); r.font.color.rgb = bullet_color; r.font.name = SANS; r.font.bold = True
            r = p.add_run(); r.text = head
            r.font.size = Pt(size); r.font.bold = True; r.font.color.rgb = INK; r.font.name = SANS
            p2 = tf.add_paragraph(); p2.space_after = Pt(0); p2.space_before = Pt(0)
            p2.level = 1
            r = p2.add_run(); r.text = body
            r.font.size = Pt(indent_size or size - 1.5); r.font.color.rgb = SLATE; r.font.name = SANS
        else:
            p = tf.add_paragraph(); p.space_after = Pt(gap); p.space_before = Pt(0)
            r = p.add_run(); r.text = "▪  "
            r.font.size = Pt(size); r.font.color.rgb = bullet_color; r.font.name = SANS; r.font.bold = True
            r = p.add_run(); r.text = it
            r.font.size = Pt(size); r.font.color.rgb = color; r.font.name = SANS

def content(title, kicker=None, rule=True):
    """Standard content slide chrome. Returns (slide, body_top)."""
    _state["n"] += 1
    sl = blank()
    rect(sl, 0, 0, SW, SH, fill=WHITE)
    # crimson accent tab
    rect(sl, 0, 0.62, 0.30, 0.055, fill=CRIMSON)
    y = 0.52
    if kicker:
        tf = txt(sl, ML, y, CW, 0.24)
        para(tf, kicker.upper(), size=10.5, color=CRIMSON, bold=True, first=True, space_after=0)
        y += 0.30
    tsz = 27
    tlines = est_lines(title, CW * SAFETY, tsz, False, SERIF)
    th = tlines * tsz * 1.20 / 72.0
    tf = txt(sl, ML, y, CW, th + 0.08)
    para(tf, title, size=tsz, color=INK, bold=False, font=SERIF, first=True, space_after=0, line=1.20)
    body_top = y + th + 0.21
    if rule:
        rect(sl, ML, body_top - 0.16, CW, 0.012, fill=RULE)
    footer(sl)
    return sl, body_top

def footer(sl):
    rect(sl, ML, SH - 0.55, CW, 0.008, fill=RULE)
    tf = txt(sl, ML, SH - 0.46, CW * 0.7, 0.24)
    para(tf, _state["chapter"], size=8.5, color=MUTED, first=True, space_after=0)
    tf = txt(sl, ML + CW * 0.7, SH - 0.46, CW * 0.3, 0.24, align=PP_ALIGN.RIGHT)
    para(tf, str(_state["n"]), size=8.5, color=MUTED, first=True, space_after=0, bold=True)

def divider(num, title, sub, points):
    _state["chapter"] = f"{num} · {title}"
    _state["n"] += 1
    sl = blank()
    rect(sl, 0, 0, SW, SH, fill=CRIMSON)
    rect(sl, 0, 0, 0.18, SH, fill=CRIMSON_D)
    tf = txt(sl, ML + 0.25, 2.05, CW, 0.5)
    para(tf, f"CHAPTER {num}", size=12, color=RGBColor(0xE8,0xB8,0xC0), bold=True, first=True, space_after=0)
    tw = CW * 0.58                      # never encroach on the contents strip
    tsz = 46
    while tsz > 26 and est_lines(title, tw * SAFETY, tsz, False, SERIF) > 1:
        tsz -= 2
    tf = txt(sl, ML + 0.25, 2.55 + (46 - tsz) * 0.008, tw, 1.3)
    para(tf, title, size=tsz, color=WHITE, font=SERIF, first=True, space_after=0)
    rect(sl, ML + 0.25, 4.05, 1.5, 0.035, fill=RGBColor(0xE8,0xB8,0xC0))
    tf = txt(sl, ML + 0.25, 4.35, CW * 0.62, 0.8)
    para(tf, sub, size=15, color=RGBColor(0xF2,0xD8,0xDC), italic=True, first=True, space_after=0, line=1.3)
    # right-hand contents strip
    x = ML + CW * 0.66
    tf = txt(sl, x, 2.05, CW * 0.34, 3.4)
    para(tf, "IN THIS CHAPTER", size=9.5, color=RGBColor(0xE0,0xA8,0xB2), bold=True, first=True, space_after=10)
    for pnt in points:
        para(tf, pnt, size=11.5, color=RGBColor(0xF6,0xE6,0xE9), space_after=7, line=1.15)
    return sl

def table(sl, data, x, y, w, col_w=None, header=True, fs=11.5, hfs=11,
          row_h=0.34, head_h=0.36, zebra=True, align=None, bold_col0=False,
          head_fill=CRIMSON, first_col_color=None):
    """Renders a table whose row heights are computed from wrapped text.
    Returns the total rendered height in inches so callers can place what follows."""
    rows, cols = len(data), len(data[0])
    if col_w:
        tot = sum(col_w)
        widths = [w * c / tot for c in col_w]
    else:
        widths = [w / cols] * cols
    PAD = 0.16                      # cell top+bottom margins plus breathing room

    heights = []
    for r in range(rows):
        size = hfs if (r == 0 and header) else fs
        bold = (r == 0 and header) or (bold_col0 and False)
        need = 0.0
        for c in range(cols):
            cell_bold = bold or (bold_col0 and c == 0)
            need = max(need, text_h(data[r][c], widths[c] - 0.18, size, cell_bold) + PAD)
        base = head_h if (r == 0 and header) else row_h
        heights.append(max(base, need))
    h = sum(heights)

    gf = sl.shapes.add_table(rows, cols, In(x), In(y), In(w), In(h))
    tbl = gf.table
    tbl.first_row = header
    tbl.horz_banding = False
    for i, cwd in enumerate(widths):
        tbl.columns[i].width = Emu(int(In(cwd)))
    for r in range(rows):
        tbl.rows[r].height = In(heights[r])
    for r in range(rows):
        for c in range(cols):
            cell = tbl.cell(r, c)
            cell.margin_left = In(0.09); cell.margin_right = In(0.07)
            cell.margin_top = In(0.035); cell.margin_bottom = In(0.035)
            cell.vertical_anchor = MSO_ANCHOR.MIDDLE
            cell.fill.solid()
            if r == 0 and header:
                cell.fill.fore_color.rgb = head_fill
            elif zebra and r % 2 == 0:
                cell.fill.fore_color.rgb = PARCH
            else:
                cell.fill.fore_color.rgb = WHITE
            tf = cell.text_frame; tf.word_wrap = True
            p = tf.paragraphs[0]
            p.alignment = (align[c] if align else PP_ALIGN.LEFT)
            run = p.add_run(); run.text = str(data[r][c])
            run.font.name = SANS
            if r == 0 and header:
                run.font.size = Pt(hfs); run.font.bold = True; run.font.color.rgb = WHITE
            else:
                run.font.size = Pt(fs)
                is0 = (c == 0)
                run.font.bold = bold_col0 and is0
                run.font.color.rgb = (first_col_color if (is0 and first_col_color) else INK)
    return h

def statbar(sl, y, stats, w=None, gap=0.22):
    w = w or CW
    n = len(stats)
    bw = (w - gap * (n - 1)) / n
    for i, (big, lab) in enumerate(stats):
        x = ML + i * (bw + gap)
        rect(sl, x, y, bw, 1.18, fill=PARCH)
        rect(sl, x, y, 0.045, 1.18, fill=CRIMSON)
        tf = txt(sl, x + 0.22, y + 0.16, bw - 0.34, 0.5)
        para(tf, big, size=27, color=CRIMSON, bold=True, font=SERIF, first=True, space_after=0)
        tf = txt(sl, x + 0.22, y + 0.68, bw - 0.34, 0.42)
        para(tf, lab, size=10, color=SLATE, first=True, space_after=0, line=1.12)

def card(sl, x, y, w, h, num, title, body, accent=CRIMSON,
         title_size=13.5, body_size=10.5):
    """Bordered card that guarantees its contents stay inside the border."""
    rect(sl, x, y, w, h, fill=WHITE, line=RULE, lw=0.9)
    rect(sl, x, y, w, 0.055, fill=accent)
    inner = w - 0.48
    PADT, PADB = 0.24, 0.20

    # kicker
    tf = txt(sl, x + 0.24, y + PADT, inner, 0.26)
    para(tf, num, size=10, color=accent, bold=True, first=True, space_after=0)

    # title — shrink until it occupies at most 2 lines
    ts = title_size
    while ts > 11.0 and est_lines(title, inner * SAFETY, ts, True, SERIF) > 2:
        ts -= 0.5
    tlines = est_lines(title, inner * SAFETY, ts, True, SERIF)
    th = tlines * ts * 1.12 / 72.0
    ty = y + PADT + 0.30
    tf = txt(sl, x + 0.24, ty, inner, th + 0.06)
    para(tf, title, size=ts, color=INK, bold=True, font=SERIF, first=True,
         space_after=0, line=1.12)

    # body — shrink until it fits the remaining space
    by = ty + th + 0.13
    avail = (y + h - PADB) - by
    bs = fit_size(body, inner * SAFETY, avail, body_size, floor=8.5, line=1.22)
    tf = txt(sl, x + 0.24, by, inner, avail)
    para(tf, body, size=bs, color=SLATE, first=True, space_after=0, line=1.22)

def quote(sl, x, y, w, text, source):
    rect(sl, x, y, 0.045, 1.0, fill=CRIMSON)
    tf = txt(sl, x + 0.28, y - 0.02, w - 0.3, 0.72)
    para(tf, text, size=13, color=INK, italic=True, font=SERIF, first=True, space_after=5, line=1.25)
    para(tf, source, size=9.5, color=MUTED, bold=True)


def connect(sl, x1, y1, x2, y2, color=SLATE, width=1.5):
    """Straight connector. Elbow connectors (type 2) auto-route into large
    rectangular detours that collide with nodes -- do not use them here."""
    c = sl.shapes.add_connector(1, In(x1), In(y1), In(x2), In(y2))
    c.line.color.rgb = color
    c.line.width = Pt(width)
    return c

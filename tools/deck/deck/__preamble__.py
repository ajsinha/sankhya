"""
SANKHYA — to count is to make completely known
Copyright © 2026 Ashutosh Sinha <ajsinha@gmail.com>. All rights reserved.
Proprietary and confidential. See LICENSE and NOTICE at the repository root.
"""
# -*- coding: utf-8 -*-
"""SANKHYA — Architecture and Evidence deck. The mark's palette; see theme.py."""
# theme.py is executed by the driver before this file, into the same namespace,
# so everything it defines is already here.

MONO = "Consolas"

def code(sl, x, y, w, lines, fs=9.5, title=None):
    """Monospace block sized from its own content."""
    LINE = 1.34
    lh = fs * LINE * 1.26 / 72.0   # measured against the render; over-sizing is harmless, under-sizing is a defect
    head = 0.32 if title else 0.0
    h = head + len(lines) * lh + 0.30
    rect(sl, x, y, w, h, fill=RGBColor(0xF4, 0xF2, 0xEF))
    rect(sl, x, y, 0.045, h, fill=SLATE)
    if title:
        tf = txt(sl, x + 0.22, y + 0.10, w - 0.4, 0.24)
        para(tf, title, size=9, color=CRIMSON, bold=True, first=True, space_after=0)
    tf = txt(sl, x + 0.22, y + head + 0.11, w - 0.4, h - head - 0.2)
    for i, ln in enumerate(lines):
        col = MUTED if ln.strip().startswith("#") else INK
        para(tf, ln if ln else " ", size=fs, color=col, font=MONO,
             first=(i == 0), space_after=0, line=1.34)
    return h

def steps(sl, x, y, w, items, h=1.45):
    n = len(items); gap = 0.20
    bw = (w - gap * (n - 1)) / n
    for i, (num, t, d) in enumerate(items):
        xx = x + i * (bw + gap)
        rect(sl, xx, y, bw, h, fill=WHITE, line=RULE)
        rect(sl, xx, y, bw, 0.05, fill=CRIMSON)
        tf = txt(sl, xx + 0.16, y + 0.16, bw - 0.32, h - 0.3)
        para(tf, num, size=14, color=CRIMSON, bold=True, font=SERIF, first=True, space_after=2)
        para(tf, t, size=11, color=INK, bold=True, space_after=4)
        para(tf, d, size=8.5, color=SLATE, line=1.15)


# ---------------------------------------------------------------------------
# Helpers from the model-and-feature-engineering deck, merged in with it.
# `code` and `steps` were defined in both and are the same function; only the
# ones this deck did not already have are brought across.
# ---------------------------------------------------------------------------
def arrow(sl, x, y, w, h, fill, line=None):
    """A right-pointing chevron. A process reads left to right, and a shape
    that points is worth more than a line that has to be followed."""
    sh = sl.shapes.add_shape(MSO_SHAPE.CHEVRON, In(x), In(y), In(w), In(h))
    sh.fill.solid()
    sh.fill.fore_color.rgb = fill
    if line is None:
        sh.line.fill.background()
    else:
        sh.line.color.rgb = line
        sh.line.width = Pt(1.0)
    sh.shadow.inherit = False
    sh.adjustments[0] = 0.16
    sh.text_frame.word_wrap = True
    sh.text_frame.margin_left = In(0.20)
    sh.text_frame.margin_right = In(0.10)
    sh.text_frame.margin_top = sh.text_frame.margin_bottom = In(0.05)
    return sh


def down(sl, x, y, h, color=CRIMSON, w=0.22):
    """A short downward arrow: one lane handing something to the next."""
    sh = sl.shapes.add_shape(MSO_SHAPE.DOWN_ARROW, In(x), In(y), In(w), In(h))
    sh.fill.solid()
    sh.fill.fore_color.rgb = color
    sh.line.fill.background()
    sh.shadow.inherit = False
    return sh


def lane(sl, x, y, w, h, label, sub, tint, accent):
    """A swimlane: who is doing this, and the band their work happens in."""
    rect(sl, x, y, w, h, fill=tint)
    rect(sl, x, y, 0.05, h, fill=accent)
    tf = txt(sl, x + 0.16, y + 0.14, 1.02, h - 0.28)
    para(tf, label, size=10.5, color=accent, bold=True, font=SERIF,
         first=True, space_after=2)
    para(tf, sub, size=7.6, color=SLATE, line=1.12, space_after=0)


def stage(sl, x, y, w, h, title, body, fill, edge, text=INK):
    """One step in a lane, drawn as a chevron so the direction is the shape."""
    sh = arrow(sl, x, y, w, h, fill, edge)
    tf = sh.text_frame
    para(tf, title, size=9.5, color=text, bold=True, font=SERIF, first=True,
         space_after=1)
    para(tf, body, size=7.4, color=text if text != INK else SLATE, line=1.10,
         space_after=0)


def listbox(sl, x, y, w, title, items, sub="", accent=CRIMSON, fill=WHITE,
            row=0.235, title_size=11):
    """A named box holding a list of members, each optionally marked.

    ``items`` is a list of (text, colour, marker). The marker is a short label
    printed on the right — 'new', 'gone', 'was UKRPI' — because the interesting
    thing about a composed object is never the list, it is which entries somebody
    here decided.
    """
    h = 0.44 + len(items) * row + (0.20 if sub else 0.0)
    rect(sl, x, y, w, h, fill=fill, line=RULE)
    rect(sl, x, y, w, 0.05, fill=accent)
    tf = txt(sl, x + 0.14, y + 0.13, w - 0.28, 0.26)
    para(tf, title, size=title_size, color=INK, bold=True, font=SERIF,
         first=True, space_after=0)
    yy = y + 0.40
    if sub:
        tf = txt(sl, x + 0.14, yy, w - 0.28, 0.20)
        para(tf, sub, size=7.8, color=SLATE, italic=True, first=True,
             space_after=0)
        yy += 0.20
    for text, colour, marker in items:
        tf = txt(sl, x + 0.14, yy, w - 0.28, row)
        parts = [(text, colour, colour is not SLATE)]
        if marker:
            parts.append((f"   {marker}", CRIMSON, False))
        runs(tf, parts, size=8.8, first=True, space_after=0)
        yy += row
    return h


def note(sl, x, y, w, h, lead, rest, tail=""):
    """A parchment block with a crimson lead-in — the deck's aside."""
    rect(sl, x, y, w, h, fill=PARCH)
    rect(sl, x, y, 0.045, h, fill=CRIMSON)
    tf = txt(sl, x + 0.26, y + 0.13, w - 0.5, h - 0.24)
    parts = [(lead, CRIMSON, True), (rest, INK, False)]
    if tail:
        parts.append((tail, INK, True))
    runs(tf, parts, size=10.5, first=True, space_after=0, line=1.24)




def part(number, title, subtitle, chapters):
    """A PART divider — a heavier break than a chapter.

    The deck has two halves that serve different readers. Part I is what these
    objects *are*, and a model developer or a validator can stop at the end of
    it; Part II is how the platform is built, and it is for whoever builds or
    audits the platform rather than the models. Marking the seam is the whole
    reason this exists: a reader who does not know they have crossed from
    concepts into engineering reads the second half as though it were still
    telling them how to do their job.
    """
    _state["chapter"] = f"Part {number}"
    _state["n"] += 1
    sl = blank()
    rect(sl, 0, 0, SW, SH, fill=INK)
    rect(sl, 0, 0, SW, 0.10, fill=CRIMSON)

    tf = txt(sl, ML + 0.25, 1.55, CW, 0.5)
    para(tf, f"PART {number}", size=13, color=CRIMSON, bold=True, first=True,
         space_after=0)

    tsz = 52
    while tsz > 30 and est_lines(title, CW * 0.62 * SAFETY, tsz, False, SERIF) > 1:
        tsz -= 2
    tf = txt(sl, ML + 0.25, 2.15, CW * 0.64, 1.4)
    para(tf, title, size=tsz, color=WHITE, font=SERIF, first=True, space_after=0)

    rect(sl, ML + 0.25, 3.75, 1.8, 0.04, fill=CRIMSON)
    tf = txt(sl, ML + 0.25, 4.05, CW * 0.60, 1.1)
    para(tf, subtitle, size=16, color=RGBColor(0xC8, 0xC2, 0xBA), italic=True,
         first=True, space_after=0, line=1.3)

    x = ML + CW * 0.68
    tf = txt(sl, x, 1.60, CW * 0.32, 4.6)
    para(tf, "CHAPTERS", size=9.5, color=CRIMSON, bold=True, first=True,
         space_after=12)
    for chapter in chapters:
        para(tf, chapter, size=11.5, color=RGBColor(0xE4, 0xDE, 0xD6),
             space_after=8, line=1.15)
    return sl

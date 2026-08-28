#!/usr/bin/env python3
"""Draw the SANKHYA marks.

The mark is generated rather than drawn by hand for the same reason the metric catalogue
is generated: so that the wordmark, the favicon, the dark variant and the contact sheet
cannot drift apart. Change a proportion here and every artefact changes together.

Five concepts, each rooted in something the name or the system actually is:

  shirorekha  The headline that binds separate Devanagari letters into one word, with the
              three engines hanging from it. One substrate, three shapes.
  convergence Three strands merging into one stem on a foundation.
  tally       saṅkhyā means number. The counting mark, with three strokes bound by a fourth.
  interlock   Three that remain three while being one.
  aperture    Three beams in, one column out.
"""

import io
import math

import cairo
from PIL import Image, ImageDraw, ImageFont

# Shapes in cairo, type in Pillow.
#
# Cairo's own text API does no shaping --- it maps code points to glyphs one at a time,
# which is invisible for Latin and renders सांख्य as ख् followed by य rather than the ख्य
# conjunct. Pango would shape it and cannot draw onto a pycairo context here, because the
# gi-cairo bridge is not installed. Pillow built against libraqm shapes correctly and
# composites over the cairo output, so each library does the half it is good at.
FONT_SANS_BOLD = "/usr/share/fonts/truetype/noto/NotoSans-Bold.ttf"
FONT_SANS = "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf"
FONT_DEVANAGARI = "/usr/share/fonts/truetype/noto/NotoSansDevanagari-Regular.ttf"

INK = (0x0E / 255, 0x17 / 255, 0x26 / 255)
DEEP = (0x1E / 255, 0x3A / 255, 0x6E / 255)
TEAL = (0x2F / 255, 0x8F / 255, 0x86 / 255)
AMBER = (0xE0 / 255, 0xA3 / 255, 0x3C / 255)
PAPER = (0xFB / 255, 0xFA / 255, 0xF7 / 255)

S = 1024          # canvas
M = 150           # margin
W = S - 2 * M     # working box


def setup(width=S, height=S, bg=PAPER):
    surface = cairo.ImageSurface(cairo.FORMAT_ARGB32, width, height)
    c = cairo.Context(surface)
    if bg is not None:
        c.set_source_rgb(*bg)
        c.paint()
    c.set_line_cap(cairo.LINE_CAP_ROUND)
    c.set_line_join(cairo.LINE_JOIN_ROUND)
    return surface, c


def stroke(c, colour, width):
    c.set_source_rgb(*colour)
    c.set_line_width(width)
    c.stroke()


def begin(c):
    """Clear the current point before starting a mark.

    `cairo.arc` draws a line from the current point to the arc's start if there is one, and
    `show_text` leaves one behind. On the contact sheet that produced a stray rule running
    out of one cell's label and across the next cell's artwork --- a bug that is invisible
    when a mark is drawn on its own canvas and obvious the moment two share one.
    """
    c.new_path()


# --- 1. shirorekha ------------------------------------------------------
def shirorekha(c, x=M, y=M, w=W, dark=False):
    """The binding headline, with three engines hanging beneath it.

    Devanagari joins the letters of a word with a line across the top --- the shirorekha.
    Separate signs, one stroke binding them into a single thing you read at once. That is
    the architecture, and it happens to be the script the name comes from.
    """
    begin(c)
    ink = PAPER if dark else INK
    bar = w * 0.085
    top = y + w * 0.14
    thin = w * 0.075

    c.move_to(x, top)
    c.line_to(x + w, top)
    stroke(c, ink, bar)

    body_top = top + bar * 0.5 + thin * 0.7
    body_bot = y + w * 0.84
    span = body_bot - body_top

    # Equal cells with a generous gutter. The first version packed them tight and the rows
    # ran into the columns: at a tab's size the three groups have to read as three, and the
    # gutter is the only thing doing that.
    gutter = w * 0.065
    cell = (w - 2 * gutter) / 3.0
    cells = [x + i * (cell + gutter) for i in range(3)]

    # Left --- rows. Whole records, one after another.
    left = cells[0]
    for i in range(3):
        ry = body_top + span * (0.12 + 0.44 * i)
        c.move_to(left + thin * 0.5, ry)
        c.line_to(left + cell - thin * 0.5, ry)
        stroke(c, DEEP, thin)

    # Middle --- columns. One field, read deep.
    left = cells[1]
    for i, depth in enumerate((0.52, 1.0, 0.74)):
        cx = left + thin * 0.5 + (cell - thin) * (i / 2.0)
        c.move_to(cx, body_top)
        c.line_to(cx, body_top + span * depth)
        stroke(c, TEAL, thin)

    # Right --- three things joined to each other.
    left = cells[2]
    dot = thin * 0.80
    # A wide, low triangle. A narrow cell makes it a tall spike that reads as a letter
    # rather than as three joined things.
    apex_y = body_top + span * 0.18
    base_y = body_bot - dot
    pts = [
        (left + cell / 2, apex_y),
        (left + dot, base_y),
        (left + cell - dot, base_y),
    ]
    for i in range(3):
        c.move_to(*pts[i])
        c.line_to(*pts[(i + 1) % 3])
        stroke(c, AMBER, thin * 0.60)
    for px, py in pts:
        begin(c)
        c.arc(px, py, dot, 0, 2 * math.pi)
        c.set_source_rgb(*AMBER)
        c.fill()


# --- 2. convergence -----------------------------------------------------
def convergence(c, x=M, y=M, w=W, dark=False):
    """Three strands merging into one stem, standing on a foundation.

    The thesis in one shape: three engines, one deployable artifact, and something solid
    underneath it. The base bar is not decoration --- a convergence with nothing to stand on
    is a merge, and a merge is what everyone else already has.
    """
    begin(c)
    ink = PAPER if dark else INK
    thick = w * 0.095
    cx = x + w / 2
    top = y + w * 0.06
    join = y + w * 0.54
    base = y + w * 0.88
    reach = w * 0.33

    # Drawn as a mirrored pair plus a centre, so the two outer strands are the same curve
    # rather than two curves that happen to look alike.
    for side, colour in ((-1, DEEP), (1, AMBER)):
        c.move_to(cx + side * reach, top)
        c.curve_to(
            cx + side * reach,
            top + (join - top) * 0.46,
            cx + side * reach * 0.30,
            join - (join - top) * 0.22,
            cx,
            join,
        )
        stroke(c, colour, thick)
    c.move_to(cx, top)
    c.line_to(cx, join)
    stroke(c, TEAL, thick)

    c.move_to(cx, join - thick * 0.3)
    c.line_to(cx, base)
    stroke(c, ink, thick * 1.4)
    c.move_to(x + w * 0.17, base)
    c.line_to(x + w * 0.83, base)
    stroke(c, ink, thick * 1.1)


# --- 3. tally -----------------------------------------------------------
def tally(c, x=M, y=M, w=W, dark=False):
    """saṅkhyā — number. Three strokes bound by a fourth."""
    begin(c)
    ink = PAPER if dark else INK
    thick = w * 0.095
    top = y + w * 0.12
    bot = y + w * 0.80
    xs = [x + w * 0.20, x + w * 0.48, x + w * 0.76]
    for sx, colour in zip(xs, (DEEP, TEAL, ink)):
        c.move_to(sx, top)
        c.line_to(sx, bot)
        stroke(c, colour, thick)
    # The binding stroke, kept inside the box. The first version ran from 0.02w to 0.86w with
    # a round cap half a stroke wide on each end, so it left the artwork --- which is
    # invisible on its own canvas and draws over the neighbour on a contact sheet.
    inset = thick * 0.6
    c.move_to(x + w * 0.08 + inset, bot - w * 0.04)
    c.line_to(x + w * 0.88 - inset, top + w * 0.04)
    stroke(c, AMBER, thick)


# --- 4. interlock -------------------------------------------------------
def interlock(c, x=M, y=M, w=W, dark=False):
    """Three that remain three while being one."""
    begin(c)
    ink = PAPER if dark else INK
    r = w * 0.28
    thick = w * 0.080
    cx, cy = x + w / 2, y + w / 2 + w * 0.01
    # Far enough apart that each ring is legible on its own, close enough that all three
    # genuinely overlap in the middle. At 0.62r they read as a blur.
    d = r * 0.52
    centres = [
        (cx, cy - d),
        (cx - d * math.cos(math.radians(30)), cy + d * math.sin(math.radians(30))),
        (cx + d * math.cos(math.radians(30)), cy + d * math.sin(math.radians(30))),
    ]
    for (ox, oy), colour in zip(centres, (DEEP, TEAL, AMBER)):
        c.arc(ox, oy, r, 0, 2 * math.pi)
        stroke(c, colour, thick)
    # The core: where all three hold at once.
    c.arc(cx, cy, r * 0.30, 0, 2 * math.pi)
    c.set_source_rgb(*ink)
    c.fill()


# --- 5. akshara --------------------------------------------------------
def akshara(c, x=M, y=M, w=W, dark=False):
    """A geometric स --- the first letter of सांख्य --- under its headline.

    Not a faithful glyph and not trying to be. A logotype that renders a real letter badly
    is worse than one that abstracts it honestly, and this is the second: the headline, the
    right stem, the open bowl and the crossing stroke, at the proportions the letter has,
    without pretending to be type.
    """
    begin(c)
    ink = PAPER if dark else INK
    thick = w * 0.105
    top = y + w * 0.14
    bot = y + w * 0.86
    left = x + w * 0.10
    right = x + w * 0.90

    # The headline every Devanagari letter hangs from.
    c.move_to(left, top)
    c.line_to(right, top)
    stroke(c, ink, thick)

    # The right stem: the letter's spine, and the strongest vertical in the mark.
    c.move_to(right - thick * 0.5, top)
    c.line_to(right - thick * 0.5, bot)
    stroke(c, ink, thick)

    # The bowl, open to the right.
    bowl_r = (bot - top) * 0.30
    bx = left + bowl_r + thick * 0.3
    by = top + (bot - top) * 0.60
    begin(c)
    c.arc(bx, by, bowl_r, math.radians(-55), math.radians(215))
    stroke(c, DEEP, thick * 0.92)

    # The stroke that crosses from the bowl to the spine.
    c.move_to(bx + bowl_r * 0.30, by - bowl_r * 0.75)
    c.line_to(right - thick * 1.1, by + bowl_r * 0.15)
    stroke(c, AMBER, thick * 0.80)


# --- the cube family ---------------------------------------------------
#
# A cube is the right shape for this system twice over, which is rare enough to be worth
# using. It is the OLAP hypercube --- the thing you slice, dice, roll up and consolidate ---
# and it is three faces of **one solid**, which is the architectural claim in a single
# figure. Three engines are not three things bolted together; they are three views of the
# same data, and a cube is what that looks like.

COS30 = math.cos(math.radians(30))


def _lerp(a, b, t):
    return (a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t)


def _grid(c, corners, n, ink, weight):
    """Rule a parallelogram into an n by n grid.

    What separates a data cube from a generic isometric box, which is one of the most
    over-used marks there is. A cube with cells is a cube you can address: this many along
    one dimension, that many along another. Without them it is a picture of a box.
    """
    origin, along, down = corners
    for k in range(1, n):
        t = k / n
        begin(c)
        a = _lerp(origin, along, t)
        c.move_to(*a)
        c.line_to(a[0] + (down[0] - origin[0]), a[1] + (down[1] - origin[1]))
        stroke(c, ink, weight)
        begin(c)
        b = _lerp(origin, down, t)
        c.move_to(*b)
        c.line_to(b[0] + (along[0] - origin[0]), b[1] + (along[1] - origin[1]))
        stroke(c, ink, weight)


def _cell(c, corners, n, i, j, colour):
    """Fill one cell of a gridded face --- the dice, the sub-cube actually asked for."""
    origin, along, down = corners
    u = ((along[0] - origin[0]) / n, (along[1] - origin[1]) / n)
    v = ((down[0] - origin[0]) / n, (down[1] - origin[1]) / n)
    base = (origin[0] + u[0] * i + v[0] * j, origin[1] + u[1] * i + v[1] * j)
    begin(c)
    c.move_to(*base)
    c.line_to(base[0] + u[0], base[1] + u[1])
    c.line_to(base[0] + u[0] + v[0], base[1] + u[1] + v[1])
    c.line_to(base[0] + v[0], base[1] + v[1])
    c.close_path()
    c.set_source_rgb(*colour)
    c.fill()


def _prism(c, cx, cy, half, tall, faces, ink, edge, cells=0, dice=None):
    """An isometric prism: a rhombus top and two side faces.

    `half` is the top rhombus's half-width, `tall` the vertical extent. A cube is the case
    where `tall` equals the side length; a slab is any smaller value, and a slab is what a
    roll-up level looks like.
    """
    hw = half * COS30
    q = half * 0.5
    n = (cx, cy - q)
    e = (cx + hw, cy)
    sth = (cx, cy + q)
    w = (cx - hw, cy)
    down = lambda p: (p[0], p[1] + tall)  # noqa: E731

    top, left, right = faces
    # (origin, along, down) for each face, so the grid and a filled cell can be placed on
    # any of them with the same arithmetic.
    frames = (
        (w, n, sth),
        (w, sth, down(w)),
        (sth, e, down(sth)),
    )
    for polygon, colour in (
        ([n, e, sth, w], top),
        ([w, sth, down(sth), down(w)], left),
        ([sth, e, down(e), down(sth)], right),
    ):
        begin(c)
        c.move_to(*polygon[0])
        for point in polygon[1:]:
            c.line_to(*point)
        c.close_path()
        c.set_source_rgb(*colour)
        c.fill_preserve()
        stroke(c, ink, edge)

    if cells:
        if dice is not None:
            face, i, j = dice
            _cell(c, frames[face], cells, i, j, PAPER)
        # The top face is ruled both ways --- it is the plane you slice along, and a cell on
        # it is an addressable region. The sides get horizontal layers only.
        #
        # Ruling all three faces into squares was the first attempt and it is a Rubik's
        # cube: an extremely well-known object that a viewer recognises before they read
        # anything else, which is the worst thing a mark can do.
        _grid(c, frames[0], cells, ink, edge * 0.52)
        for frame in frames[1:]:
            origin, along, down_to = frame
            for k in range(1, cells):
                t = k / cells
                begin(c)
                point = _lerp(origin, down_to, t)
                c.move_to(*point)
                c.line_to(
                    point[0] + (along[0] - origin[0]),
                    point[1] + (along[1] - origin[1]),
                )
                stroke(c, ink, edge * 0.52)
        # The outline again, over the grid, so the silhouette stays the strongest line.
        for polygon in (
            [n, e, sth, w],
            [w, sth, down(sth), down(w)],
            [sth, e, down(e), down(sth)],
        ):
            begin(c)
            c.move_to(*polygon[0])
            for point in polygon[1:]:
                c.line_to(*point)
            c.close_path()
            stroke(c, ink, edge)


def cube(c, x=M, y=M, w=W, dark=False):
    """One solid, three faces.

    The unification claim and the data cube in the same figure.
    """
    begin(c)
    ink = PAPER if dark else INK
    half = w * 0.42
    _prism(c, x + w / 2, y + w * 0.30, half, half * 0.86, (AMBER, DEEP, TEAL), ink, w * 0.045)


def dice(c, x=M, y=M, w=W, dark=False):
    """The cube, addressable --- and one cell picked out of it.

    Cells are what make it a *data* cube rather than a box: a cube with cells is a cube you
    can name a region of. The lit cell is the dice --- the sub-cube actually asked for, which
    is the operation this system exists to answer without moving the data anywhere first.
    """
    begin(c)
    ink = PAPER if dark else INK
    half = w * 0.42
    _prism(
        c,
        x + w / 2,
        y + w * 0.30,
        half,
        half * 0.86,
        (AMBER, DEEP, TEAL),
        ink,
        w * 0.045,
        cells=3,
        dice=(0, 1, 1),
    )


def rollup(c, x=M, y=M, w=W, dark=False):
    """Detail, grouped, consolidated --- three levels of one hierarchy.

    Roll-up is not a smaller copy of the data. It is the same solid seen at a coarser grain,
    so the levels are the same shape narrowing rather than three unrelated blocks.
    """
    begin(c)
    ink = PAPER if dark else INK
    cx = x + w / 2
    edge = w * 0.040
    levels = ((0.44, 0.155, 0.74), (0.32, 0.135, 0.44), (0.20, 0.115, 0.19))
    for half_f, tall_f, cy_f in levels:
        _prism(
            c,
            cx,
            y + w * cy_f,
            w * half_f,
            w * tall_f,
            (AMBER, DEEP, TEAL),
            ink,
            edge,
        )


def slice_(c, x=M, y=M, w=W, dark=False):
    """The cube with one level drawn off: slice and dice, on demand.

    The gap is the whole idea. A cube you cannot take apart is a picture of storage; a cube
    with a slab lifted clear of it is a query.
    """
    begin(c)
    ink = PAPER if dark else INK
    cx = x + w / 2
    half = w * 0.38
    edge = w * 0.042
    # The body, still whole.
    _prism(c, cx, y + w * 0.60, half, w * 0.24, (AMBER, DEEP, TEAL), ink, edge)
    # The slice, lifted clear. The gap has to be wide enough that the two silhouettes never
    # touch --- at a smaller offset they read as one stepped solid, which is a join and the
    # opposite of what is being shown.
    _prism(
        c,
        cx + w * 0.12,
        y + w * 0.16,
        half,
        w * 0.10,
        (AMBER, DEEP, TEAL),
        ink,
        edge,
    )


MARKS = {
    "shirorekha": shirorekha,
    "convergence": convergence,
    "tally": tally,
    "interlock": interlock,
    "akshara": akshara,
    "cube": cube,
    "dice": dice,
    "rollup": rollup,
    "slice": slice_,
}


def to_pillow(surface):
    """A cairo surface as a Pillow image."""
    buffer = io.BytesIO()
    surface.write_to_png(buffer)
    buffer.seek(0)
    return Image.open(buffer).convert("RGBA")


def rgb(colour):
    return tuple(int(round(component * 255)) for component in colour)


def font(path, size):
    return ImageFont.truetype(path, size, layout_engine=ImageFont.Layout.RAQM)


def draw_text(image, text, x, top, size, colour, path=FONT_SANS_BOLD, spacing=0.0):
    """Draw `text` with its cap-top at `top`, returning the width it occupied."""
    face = font(path, size)
    draw = ImageDraw.Draw(image)
    if spacing == 0.0:
        draw.text((x, top), text, font=face, fill=rgb(colour), anchor="la")
        return int(draw.textlength(text, font=face))
    cursor = x
    for character in text:
        draw.text((cursor, top), character, font=face, fill=rgb(colour), anchor="la")
        cursor += draw.textlength(character, font=face) + spacing
    return int(cursor - x - spacing)


def text_width(text, size, path=FONT_SANS_BOLD, spacing=0.0):
    face = font(path, size)
    draw = ImageDraw.Draw(Image.new("RGBA", (8, 8)))
    if spacing == 0.0:
        return int(draw.textlength(text, font=face))
    return int(
        sum(draw.textlength(ch, font=face) + spacing for ch in text) - spacing
    )


def mark_png(name, dark=False):
    bg = INK if dark else PAPER
    surface, c = setup(bg=bg)
    MARKS[name](c, dark=dark)
    out = f"docs/assets/mark-{name}{'-dark' if dark else ''}.png"
    surface.write_to_png(out)
    return out


def wordmark_png(name, dark=False):
    """The mark beside the name, which is how it is actually used.

    Measured rather than guessed: the canvas is sized to what it ends up holding, so a
    change to the mark or the type does not leave a band of empty pixels every consumer has
    to crop around.
    """
    bg = INK if dark else PAPER
    ink = PAPER if dark else INK
    pad, box, gap = 90, 420, 130
    size, origin_size = 168, 68
    spacing = size * 0.06

    name_w = text_width("SANKHYA", size, spacing=spacing)
    origin_w = text_width("सांख्य", origin_size, path=FONT_DEVANAGARI)

    width = pad + box + gap + max(name_w, origin_w) + pad
    height = box + 2 * pad
    surface, c = setup(width, height, bg)
    MARKS[name](c, x=pad, y=pad, w=box, dark=dark)
    image = to_pillow(surface)

    x = pad + box + gap
    # Placed from the ink boxes rather than from a fraction of the point size. Guessing the
    # offset put the anusvara of सां through the S: Devanagari carries marks well above the
    # Latin cap height, so the two scripts' visual extents are not related by any constant.
    probe = ImageDraw.Draw(image)
    latin = probe.textbbox((x, 0), "SANKHYA", font=font(FONT_SANS_BOLD, size), anchor="la")
    origin_box = probe.textbbox(
        (x, 0), "सांख्य", font=font(FONT_DEVANAGARI, origin_size), anchor="la"
    )
    latin_height = latin[3] - latin[1]
    origin_height = origin_box[3] - origin_box[1]
    breathing = origin_size * 0.42
    block = latin_height + breathing + origin_height
    top = (height - block) // 2 - latin[1]

    draw_text(image, "SANKHYA", x, top, size, ink, spacing=spacing)
    # The Devanagari beneath, small. The name has an origin and the mark should say so.
    draw_text(
        image,
        "सांख्य",
        x + 4,
        int(top + latin[1] + latin_height + breathing) - origin_box[1],
        origin_size,
        AMBER,
        path=FONT_DEVANAGARI,
    )
    out = f"docs/assets/wordmark-{name}{'-dark' if dark else ''}.png"
    image.save(out)
    return out


def contact_sheet(names=None, out="docs/assets/logo-concepts.png", dark=False):
    """Several marks side by side, which is the only way to choose between them."""
    names = list(names or MARKS)
    cell, pad, label = 520, 60, 100
    cols = len(names)
    width = cols * cell + (cols + 1) * pad
    height = cell + 2 * pad + label
    surface, c = setup(width, height, INK if dark else PAPER)
    for i, name in enumerate(names):
        ox = pad + i * (cell + pad)
        MARKS[name](c, x=ox + cell * 0.14, y=pad + cell * 0.14, w=cell * 0.72, dark=dark)
    image = to_pillow(surface)
    for i, name in enumerate(names):
        ox = pad + i * (cell + pad)
        size = 40
        w = text_width(name, size, path=FONT_SANS)
        draw_text(
            image,
            name,
            ox + (cell - w) // 2,
            pad + cell + 34,
            size,
            PAPER if dark else INK,
            path=FONT_SANS,
        )
    image.save(out)
    return out


if __name__ == "__main__":
    made = [
        contact_sheet(
            ["shirorekha", "convergence", "tally", "interlock", "akshara"],
            "docs/assets/logo-concepts.png",
        ),
        contact_sheet(
            ["cube", "dice", "rollup", "slice"],
            "docs/assets/logo-concepts-cube.png",
        ),
    ]
    for name in MARKS:
        made.append(mark_png(name))
        made.append(wordmark_png(name))
        made.append(wordmark_png(name, dark=True))
    for path in made:
        print(path)

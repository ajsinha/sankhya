# The deck generator

Regenerates `docs/SANKHYA-Architecture-and-Evidence.pptx` from source, so the deck is
reproducible rather than a binary nobody can edit safely.

```bash
python3 -m venv .venv && .venv/bin/pip install python-pptx
.venv/bin/python tools/deck/slides.py              # -> docs/SANKHYA-Architecture-and-Evidence.pptx
.venv/bin/python tools/deck/audit.py <deck>        # must report no geometry issues
```

| File | Purpose |
|---|---|
| `theme.py` | The design system: the mark's palette, typography, and the layout primitives |
| `deck/` | One file per chapter, executed in numeric order into one namespace |
| `audit.py` | Geometry checker. Run it after every change |

The palette is the one `tools/make-logo.py` already establishes — ink, deep blue, teal, amber,
paper — because the wordmark on the README is drawn in them. A deck in a different palette
would be a second identity for the same system, which is the drift these generators exist to
prevent.

## Why there is a text-fitting layer

`python-pptx` does not measure or wrap text, and PowerPoint shapes do not clip overflow — text
simply spills outside its box and over whatever is beneath it. Hand-estimated box sizes
therefore fail **silently**, which is the same failure this repository spent a hundred and
twenty-nine findings on: something that looks right, is not, and has nothing checking.

So the primitives measure:

- **`est_lines()`** simulates greedy word wrapping to predict rendered line counts, with a
  safety margin so borderline cases round to the safe side.
- **`table()`** computes each row's height from its wrapped cell text and **returns the total
  rendered height**. Callers position what follows using that return value — never a hardcoded
  offset, which is the bug class that produces captions sitting on top of their own tables.
- **`card()`** shrinks the title until it fits, then the body, so content stays inside its border.
- **`content()`** returns `(slide, body_top)` and draws the footer itself. A caller that
  hardcodes a top margin instead of using the returned one is the same defect as above.

`row_h` is a **floor, not a height**: `table()` adds padding plus a text line on top of it, so
an author's mental `rows × row_h` is always short. Take the height the function returns.

## The audit

`audit.py` re-derives the geometry of every shape on every slide and reports three failure
classes: a shape outside the slide bounds, estimated text height exceeding its shape, and
content crossing the footer rule. It also measures tables — a `GraphicFrame` has no text frame,
so a loop written naively skips every table before measuring anything, and an opaque shape drawn
over a table does not crowd the reader, it deletes a row from the page.

It must report **no geometry issues** before the deck ships. It caught one overlap while this
deck was being written, on the slide where a caption was placed from a guessed offset rather
than from the height the table returned.

---

Copyright © 2026 Ashutosh Sinha <ajsinha@gmail.com>. All rights reserved.
Proprietary and confidential. See [LICENSE](../../LICENSE).

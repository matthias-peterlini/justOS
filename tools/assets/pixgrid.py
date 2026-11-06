"""Pixel icons on a grid, in the JustOS style.

An icon is a few layers of grid cells, each layer one colour. Drawn, every
cell is a square whose outer corners are rounded by half a cell where both
neighbours beside that corner are empty (a lone cell becomes a round dot),
and every inside corner (an empty cell whose two neighbours and the cell
between them, diagonally, are filled) gets a small fillet of 0.38 of a
cell, so the cells look fused. Both rules look at all the layers
together; a fillet takes the colour of the cells it joins. Seen
from afar the icons look soft; close up the grid shows.

This module reads the icons drawn as SVG on that grid (the ones in
tools/assets/src/icone-justos) back into cells, and draws cells at any
size with Pillow.
"""
import re

from PIL import Image, ImageDraw

FILLET = 0.38

# ----------------------------------------------------------------------------
# Reading the SVG icons


def _subpaths(d):
    """The subpaths of a path made of M, H, V, L, A and Z (absolute)."""
    tokens = re.findall(r"[MHVLAZ]|-?\d*\.?\d+(?:e-?\d+)?", d)
    out, cur, i, pos = [], [], 0, (0.0, 0.0)
    while i < len(tokens):
        t = tokens[i]
        i += 1
        if t == "M":
            if cur:
                out.append(cur)
            pos = (float(tokens[i]), float(tokens[i + 1]))
            i += 2
            cur = [("M", pos)]
        elif t == "H":
            pos = (float(tokens[i]), pos[1])
            i += 1
            cur.append(("L", pos))
        elif t == "V":
            pos = (pos[0], float(tokens[i]))
            i += 1
            cur.append(("L", pos))
        elif t == "L":
            pos = (float(tokens[i]), float(tokens[i + 1]))
            i += 2
            cur.append(("L", pos))
        elif t == "A":
            # rx ry rotation large sweep x y
            pos = (float(tokens[i + 5]), float(tokens[i + 6]))
            i += 7
            cur.append(("A", pos))
        elif t == "Z":
            pass
        else:
            raise ValueError(f"unexpected {t!r} in path")
    if cur:
        out.append(cur)
    return out


def _cells(d):
    """The grid cells of a path (subpaths a cell wide; fillets are skipped)."""
    cells = set()
    for sp in _subpaths(d):
        xs = [p[0] for _, p in sp]
        ys = [p[1] for _, p in sp]
        w, h = max(xs) - min(xs), max(ys) - min(ys)
        if abs(w - 1) < 1e-6 and abs(h - 1) < 1e-6:
            cells.add((round(min(xs)), round(min(ys))))
        elif w > 0.5 or h > 0.5:
            raise ValueError(f"subpath {w}x{h} is neither a cell nor a fillet")
    return cells


def _color(s):
    s = s.strip()
    if s == "currentColor":
        return (255, 255, 255, 255)
    m = re.fullmatch(r"rgba\((\d+),\s*(\d+),\s*(\d+),\s*([\d.]+)\)", s)
    if m:
        return (int(m[1]), int(m[2]), int(m[3]), round(float(m[4]) * 255))
    s = s.lstrip("#")
    if len(s) == 3:
        s = "".join(ch * 2 for ch in s)
    return (int(s[0:2], 16), int(s[2:4], 16), int(s[4:6], 16), 255)


def read_svg(path):
    """(grid size, background, layers) of an icon drawn on the grid.

    background is None, ("gradient", top, bottom) or ("flat", colour), with
    the corner radius in cells; layers are (colour, cells) in drawing order.
    """
    s = open(path, encoding="utf-8").read()
    size = int(float(re.search(r'viewBox="0 0 ([\d.]+)', s)[1]))
    bg = None
    g = re.search(r'<linearGradient id="g"[^>]*>(.*?)</linearGradient>', s)
    stops = re.findall(r'stop-color="([^"]+)"', g[1]) if g else []
    m = re.search(r'<rect width="[\d.]+" height="[\d.]+" rx="([\d.]+)" fill="([^"]+)"', s)
    if m:
        rx = float(m[1])
        if m[2].startswith("url("):
            bg = ("gradient", _color(stops[0]), _color(stops[-1]), rx)
        else:
            bg = ("flat", _color(m[2]), rx)
    layers = []
    for fill, d in re.findall(r'<path fill="([^"]+)" d="([^"]+)"', s):
        layers.append((_color(fill), _cells(d)))
    return size, bg, layers


# ----------------------------------------------------------------------------
# Bitmaps written as text


def parse_art(rows, palette):
    """Layers from rows of characters: each character in `palette` is a
    layer (in palette order), anything else is empty."""
    layers = []
    for ch, color in palette.items():
        cells = {(x, y) for y, row in enumerate(rows) for x, c in enumerate(row) if c == ch}
        if cells:
            layers.append((color, cells))
    return layers


def offset(layers, dx, dy):
    return [(c, {(x + dx, y + dy) for x, y in cells}) for c, cells in layers]


# ----------------------------------------------------------------------------
# Drawing


_CELLS = {}


def _cell_shape(unit, corners):
    """A cell `unit` pixels wide with the given corners (tl, tr, br, bl)
    rounded by half a cell."""
    key = (unit, corners)
    if key not in _CELLS:
        im = Image.new("L", (unit, unit), 255)
        if any(corners):
            disc = Image.new("L", (unit, unit), 0)
            ImageDraw.Draw(disc).ellipse([0, 0, unit - 1, unit - 1], fill=255)
            h = (unit + 1) // 2
            spots = ((0, 0), (unit - h, 0), (unit - h, unit - h), (0, unit - h))
            for on, (x, y) in zip(corners, spots):
                if on:
                    im.paste(disc.crop((x, y, x + h, y + h)), (x, y))
        _CELLS[key] = im
    return _CELLS[key]


def _cell(mask, x, y, x0, y0, unit, allc):
    e = lambda dx, dy: (x + dx, y + dy) not in allc  # noqa: E731
    corners = (e(-1, 0) and e(0, -1), e(1, 0) and e(0, -1), e(1, 0) and e(0, 1), e(-1, 0) and e(0, 1))
    mask.paste(255, (x0 + x * unit, y0 + y * unit), _cell_shape(unit, corners))


def _fillet(n, sx, sy):
    """A fillet n pixels wide whose corner point is towards (sx, sy)."""
    im = Image.new("L", (n, n), 255)
    cx = 0 if sx > 0 else n
    cy = 0 if sy > 0 else n
    ImageDraw.Draw(im).ellipse([cx - n, cy - n, cx + n - 1, cy + n - 1], fill=0)
    return im


def fillets(layers, allc):
    """The inside corners: {layer index: [(x, y, sx, sy)]}, where (x, y) is
    the empty cell and (sx, sy) points to the corner."""
    out = {}
    empty = {(x + dx, y + dy) for x, y in allc for dx, dy in ((1, 0), (-1, 0), (0, 1), (0, -1))} - allc
    for x, y in empty:
        for sx in (-1, 1):
            for sy in (-1, 1):
                h, v, dg = (x + sx, y), (x, y + sy), (x + sx, y + sy)
                if h in allc and v in allc and dg in allc:
                    li = next((i for i, (_, c) in enumerate(layers) if h in c and v in c), None)
                    if li is None:
                        li = next(i for i, (_, c) in enumerate(layers) if h in c)
                    out.setdefault(li, []).append((x, y, sx, sy))
    return out


def draw_layers(img, layers, x0, y0, unit, solid=None):
    """Draws `layers` on `img` (RGBA) with the grid's (0, 0) at (x0, y0) and
    `unit` pixels per cell (an integer). Corners are rounded against the
    union of all layers (or of `solid`, when given). A layer's colour is an
    RGBA tuple, or a pair of them for a vertical gradient over the layer."""
    allc = set(solid) if solid is not None else set().union(*(c for _, c in layers)) if layers else set()
    n = round(unit * FILLET)
    shapes = {(sx, sy): _fillet(n, sx, sy) for sx in (-1, 1) for sy in (-1, 1)}
    fl = fillets(layers, allc)
    for li, (color, cells) in enumerate(layers):
        if not cells:
            continue
        mask = Image.new("L", img.size, 0)
        d = ImageDraw.Draw(mask)
        for x, y in cells:
            _cell(mask, x, y, x0, y0, unit, allc)
        for x, y, sx, sy in fl.get(li, []):
            px = x0 + (x + (1 if sx > 0 else 0)) * unit
            py = y0 + (y + (1 if sy > 0 else 0)) * unit
            bx = px - n if sx > 0 else px
            by = py - n if sy > 0 else py
            mask.paste(255, (bx, by), shapes[(sx, sy)])
        if isinstance(color[0], tuple):
            top = y0 + min(y for _, y in cells) * unit
            bot = y0 + (max(y for _, y in cells) + 1) * unit
            fill = vgradient(img.size, color[0], color[1], top, bot)
            alpha = fill.getchannel("A")
            from PIL import ImageChops
            fill.putalpha(ImageChops.multiply(alpha, mask))
        else:
            fill = Image.new("RGBA", img.size, color)
            fill.putalpha(mask.point(lambda v, a=color[3]: v * a // 255))
        img.alpha_composite(fill)


def vgradient(size, top, bottom, y0=0, y1=None):
    """An image of `size` going from `top` at row y0 to `bottom` at row y1."""
    w, h = size
    y1 = h if y1 is None else y1
    col = Image.new("RGBA", (1, h))
    px = col.load()
    for y in range(h):
        k = min(1.0, max(0.0, (y - y0) / max(1, y1 - y0 - 1)))
        px[0, y] = tuple(round(top[i] * (1 - k) + bottom[i] * k) for i in range(4))
    return col.resize((w, h), Image.NEAREST)


def ascii_dump(size, layers):
    """The layers as text, one character per layer ('a', 'b', ...)."""
    rows = [["." for _ in range(size)] for _ in range(size)]
    for i, (_, cells) in enumerate(layers):
        for x, y in cells:
            rows[y][x] = "abcdefgh"[i]
    return ["".join(r) for r in rows]

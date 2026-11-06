#!/usr/bin/env python3
"""Draws the JustOS icon set into one atlas image, on a pixel grid.

The style comes from the icons in tools/assets/src/icone-justos (drawn as
SVG on the grid): apps are a rounded square of 20x20 cells with a vertical
gradient and a faint sheen on the rim, holding a glyph of about 12x12 cells
in white (orange only as an accent); system glyphs are 12x12 cells in
white. pixgrid.py rounds the outer corners and fills the inner ones, so the
icons look soft from afar and show the grid up close. The icons in that
folder are read as they are; the others are drawn here as text, one
character per cell, in the same rules.

Output: rootfs/system/gui/icons.qoi (a grid of 128 px cells, 8 per row) and
rootfs/system/gui/icons.idx (the icon names, one per line, in grid order).

Usage: tools/assets/gen_pixicons.py [output folder]
       tools/assets/gen_pixicons.py --store <folder>   (icons of the games)
       PREVIEW=sheet.png tools/assets/gen_pixicons.py   (also a preview)
"""
import math
import os
import sys

from PIL import Image, ImageChops, ImageDraw, ImageFilter

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import pixgrid  # noqa: E402
import qoi  # noqa: E402

SRC = os.path.join(HERE, "src", "icone-justos")
OUT = 128          # cell size in the atlas
K = 10             # drawn at K times the size, then reduced
BIG = OUT * K
COLS = 8


def rgba(h, a=255):
    h = h.lstrip("#")
    return (int(h[0:2], 16), int(h[2:4], 16), int(h[4:6], 16), a)


WHITE = rgba("#ffffff")
SOFT = rgba("#ffffff", 140)      # the second white of the glyphs
ORANGE = rgba("#ff5a1f")
INK = rgba("#1f2328")
GREY = rgba("#9aa6b2")
CREAM = rgba("#f2efe6")

# Characters of the drawings: one layer each, in this order.
PALETTE = {
    ":": SOFT,
    "#": WHITE,
    "k": INK,
    "g": GREY,
    "b": rgba("#2b6de8"),
    "B": rgba("#6fa3f2"),
    "r": rgba("#ff3b30"),
    "G": rgba("#34c759"),
    "p": rgba("#7d5cf5"),
    "P": rgba("#ff4f8b"),
    "c": CREAM,
    "o": ORANGE,
}


def art(*rows, **extra):
    """Layers from rows of text (see PALETTE; `extra` adds characters)."""
    pal = dict(PALETTE, **extra)
    return pixgrid.parse_art(list(rows), pal)


def bbox(layers):
    cells = set().union(*(c for _, c in layers))
    xs = [x for x, _ in cells]
    ys = [y for _, y in cells]
    return min(xs), min(ys), max(xs) + 1, max(ys) + 1


def centred(layers, grid):
    """`layers` moved to the middle of a grid of `grid` cells."""
    x0, y0, x1, y1 = bbox(layers)
    return pixgrid.offset(layers, (grid - (x1 - x0)) // 2 - x0, (grid - (y1 - y0)) // 2 - y0)


def recolor(layers, colors):
    """The same cells in other colours: {old colour: new colour}."""
    return [(colors.get(c, c), cells) for c, cells in layers]


def svg(name):
    return pixgrid.read_svg(os.path.join(SRC, name + ".svg"))


# ----------------------------------------------------------------------------
# Drawing a cell of the atlas


def shadow(mask, dy, blur, alpha):
    sh = Image.new("L", mask.size, 0)
    sh.paste(mask, (0, dy))
    sh = sh.filter(ImageFilter.GaussianBlur(blur)).point(lambda v: v * alpha // 255)
    im = Image.new("RGBA", mask.size, (0, 0, 0, 0))
    im.putalpha(sh)
    return im


def tile(top, bottom, layers, grid=20, rx=5.0, flat=None):
    """An app icon: the rounded square with its gradient, the sheen on the
    rim, a soft shadow, and `layers` on a grid of `grid` cells."""
    unit = 57 if grid == 20 else 1140 // grid
    size = unit * grid
    x0 = (BIG - size) // 2
    y0 = x0 - 8
    img = Image.new("RGBA", (BIG, BIG), (0, 0, 0, 0))
    mask = Image.new("L", (BIG, BIG), 0)
    rad = round(rx * unit)
    ImageDraw.Draw(mask).rounded_rectangle([x0, y0, x0 + size - 1, y0 + size - 1], radius=rad, fill=255)
    img.alpha_composite(shadow(mask, 22, 30, 80))
    bg = Image.new("RGBA", (BIG, BIG), flat) if flat else pixgrid.vgradient((BIG, BIG), top, bottom, y0, y0 + size)
    bg.putalpha(mask)
    img.alpha_composite(bg)
    # The sheen: a thin rim, white at the top fading out by the middle.
    w = max(2, round(unit * 0.3))
    ring = Image.new("L", (BIG, BIG), 0)
    d = ImageDraw.Draw(ring)
    d.rounded_rectangle([x0, y0, x0 + size - 1, y0 + size - 1], radius=rad, fill=255)
    d.rounded_rectangle([x0 + w, y0 + w, x0 + size - 1 - w, y0 + size - 1 - w], radius=rad - w, fill=0)
    fade = pixgrid.vgradient((BIG, BIG), (255, 255, 255, 72), (255, 255, 255, 0), y0, y0 + size // 2)
    fade.putalpha(ImageChops.multiply(fade.getchannel("A"), ring))
    img.alpha_composite(fade)
    pixgrid.draw_layers(img, layers, x0, y0, unit)
    return img


def glyph(layers, grid=12, frame=14):
    """A system glyph: `layers` on a grid of `grid` cells, in the middle of a
    frame of `frame` cells."""
    unit = BIG // frame
    x0 = (BIG - unit * grid) // 2
    img = Image.new("RGBA", (BIG, BIG), (0, 0, 0, 0))
    pixgrid.draw_layers(img, layers, x0, x0, unit)
    return img


def loose(layers, grid=20, unit=54):
    """A shape of its own (folders, documents) with a soft shadow."""
    x0 = (BIG - unit * grid) // 2
    top = Image.new("RGBA", (BIG, BIG), (0, 0, 0, 0))
    pixgrid.draw_layers(top, layers, x0, x0, unit)
    img = shadow(top.getchannel("A"), 18, 24, 70)
    img.alpha_composite(top)
    return img


def reduce(img):
    return img.resize((OUT, OUT), Image.BOX)


# ----------------------------------------------------------------------------
# Helpers for shapes


def ring(cx, cy, r_out, r_in, gap=None):
    """Cells whose centres lie between two circles around (cx, cy); `gap`
    leaves out an angle range in degrees (0 = right, counter-clockwise)."""
    cells = set()
    n = int(r_out) + 2
    for y in range(int(cy) - n, int(cy) + n + 1):
        for x in range(int(cx) - n, int(cx) + n + 1):
            dx, dy = x + 0.5 - cx, y + 0.5 - cy
            if r_in * r_in <= dx * dx + dy * dy <= r_out * r_out:
                a = math.degrees(math.atan2(-dy, dx)) % 360
                if gap and (gap[0] <= a <= gap[1]):
                    continue
                cells.add((x, y))
    return cells


def stroke(points, width=2):
    """Cells along a polyline, `width` cells thick."""
    cells = set()
    for (xa, ya), (xb, yb) in zip(points, points[1:]):
        n = max(abs(xb - xa), abs(yb - ya)) * 4 + 1
        for i in range(n + 1):
            t = i / n
            x = xa + (xb - xa) * t
            y = ya + (yb - ya) * t
            for dx in range(width):
                for dy in range(width):
                    cells.add((round(x) + dx, round(y) + dy))
    return cells


def layers_of(*pairs):
    return [(c, set(cells)) for c, cells in pairs if cells]


def tri_right(h):
    """A triangle pointing right, `h` rows (odd)."""
    m = h // 2
    w = m + 1
    return ["#" * (w - abs(r - m)) + "." * abs(r - m) for r in range(h)]


def hcat(*blocks, gap=0):
    rows = max(len(b) for b in blocks)
    out = []
    for r in range(rows):
        parts = []
        for b in blocks:
            w = max(len(x) for x in b)
            pad = (rows - len(b)) // 2
            line = b[r - pad] if 0 <= r - pad < len(b) else ""
            parts.append(line.ljust(w, "."))
        out.append(("." * gap).join(parts))
    return out


def mirror(rows):
    return [r[::-1] for r in rows]


# ----------------------------------------------------------------------------
# Glyphs from the SVG set (12x12)


def sys_glyph(name, fix=None):
    size, _, layers = svg("sistema/" + name)
    if fix:
        cells = set(layers[0][1])
        for y, row in fix.items():
            cells = {(x, yy) for x, yy in cells if yy != y}
            cells |= {(x, y) for x, ch in enumerate(row) if ch != "."}
        layers = [(layers[0][0], cells)]
    return layers


G_WIFI = sys_glyph("wifi")
G_BATTERY = sys_glyph("batteria")
G_VOLUME = sys_glyph("volume")
G_MUTE = sys_glyph("suono-off")
G_POWER = sys_glyph("spegni")
G_SEARCH = sys_glyph("cerca")
G_SUN = sys_glyph("luminosita")
G_MOON = sys_glyph("notte")
G_BLUETOOTH = sys_glyph("bluetooth")
G_LOCK = sys_glyph("blocco")
# Two rows of these had a stray cell on the left; drawn as meant.
G_PLANE = sys_glyph("aereo", {5: ".aaaaaaaaaa."})
G_BELL = sys_glyph("notifiche", {8: "aaaaaaaaaaaa"})


def app_svg(name):
    size, bg, layers = svg("app/" + name)
    _, top, bottom, rx = bg
    return lambda: tile(top, bottom, layers, size, rx)


def info_icon():
    size, bg, layers = svg("justos-j-tondo")
    return tile(None, None, layers, size, bg[2], flat=bg[1])


# ----------------------------------------------------------------------------
# App icons drawn here


def t(top, bottom, layers):
    return lambda: tile(rgba(top), rgba(bottom), centred(layers, 20))


def t_glyph(top, bottom, layers):
    """A tile around a 12x12 system glyph, kept where it is on its grid."""
    return lambda: tile(rgba(top), rgba(bottom), pixgrid.offset(layers, 4, 4))


LIGHT = ("#ffffff", "#e6eaee")

ARC = art(
    "##........##",
    "##........##",
    ".##......##.",
    ".###....###.",
    "..########..",
    "....####....",
)

LT = ["...##", "..##.", ".##..", "##...", ".##..", "..##.", "...##"]
CODE = art(*hcat(LT, ["..", "..", "oo", "oo", "oo", "..", ".."], mirror(LT), gap=1))

TEXT_EDITOR = art(
    "kkkkkkkkkkkk",
    "kkkkkkkkkkkk",
    "............",
    "kkkkkkkkk...",
    "kkkkkkkkk...",
    "............",
    "kkkkkkkkkkkk",
    "kkkkkkkkkkkk",
    "..........oo",
    "kkkkkk....oo",
    "kkkkkk....oo",
    "..........oo",
)

CALC = art(*(
    hcat(["..##..", "..##..", "######", "######", "..##..", "..##.."],
         ["##..##", "######", ".####.", ".####.", "######", "##..##"], gap=1)
    + ["............."]
    + hcat(["......", "......", "######", "######", "......", "......"],
           ["oooooo", "oooooo", "......", "......", "oooooo", "oooooo"], gap=1)
))

STORE = art(
    "...######...",
    "..##....##..",
    "..##....##..",
    "############",
    "#####oo#####",
    "#####oo#####",
    "#####oo#####",
    "###oooooo###",
    "####oooo####",
    "#####oo#####",
    "############",
    "############",
)

ACTIVITY = [(rgba("#3ddc84"), stroke([(0, 6), (3, 6), (5, 1), (8, 10), (10, 6), (13, 6)]))]

SCREENSHOT = art(
    "kkkk....kkkk",
    "kkkk....kkkk",
    "kk........kk",
    "kk..oooo..kk",
    "....oooo....",
    "....oooo....",
    "kk..oooo..kk",
    "kk........kk",
    "kkkk....kkkk",
    "kkkk....kkkk",
)

COMPUTER = art(
    "kkkkkkkkkkkkkk",
    "kbbbbbbbbbbbbk",
    "kbbbbbbbbbbbbk",
    "kbbbbbbbbbbbbk",
    "kbbbbbbbbbbbbk",
    "kbbbbbbbbbbbbk",
    "kkkkkkkkkkkkkk",
    "kkkkkkkkkkkkkk",
    "......kk......",
    "....kkkkkk....",
)

TRASH = art(
    "....kkk....",
    "kkkkkkkkkkk",
    "kkkkkkkkkkk",
    "...........",
    ".kk.k.k.kk.",
    ".kk.k.k.kk.",
    ".kk.k.k.kk.",
    ".kk.k.k.kk.",
    ".kk.k.k.kk.",
    ".kkkkkkkkk.",
    "..kkkkkkk..",
)

HELP = art(
    "..######..",
    ".##....##.",
    ".##....##.",
    ".......##.",
    ".....###..",
    "....##....",
    "....##....",
    "..........",
    "....##....",
    "....##....",
)

IMAGE = art(
    "##############",
    "#............#",
    "#........oo..#",
    "#........oo..#",
    "#....#.......#",
    "#...###......#",
    "#..#####..#..#",
    "#.#######.##.#",
    "##############",
)

DOWNLOAD = art(
    "....##....",
    "....##....",
    "....##....",
    ".########.",
    "..######..",
    "...####...",
    "....##....",
    "..........",
    "##......##",
    "##########",
)

GAMES = art(
    "..##########..",
    ".############.",
    "###.######o###",
    "##...####o#o##",
    "###.######o###",
    "##############",
    "#####....#####",
    ".###......###.",
)

APPS = art(*(["#####..#####"] * 5 + ["............"] * 2 + ["#####..#####"] * 5))

USB = art(
    "...gggg...",
    "...g..g...",
    "...gggg...",
    "..bbbbbb..",
    "..bbbbbb..",
    "..bbbbbb..",
    "..bbbbbb..",
    "..bbbbbb..",
    "..bbbbbb..",
    "...bbbb...",
)

WINDOW = art(
    "##########oo",
    "##########oo",
    "............",
    "::::::::::::",
    "::::::::::::",
    "::::::::::::",
    "::::::::::::",
    "::::::::::::",
    "::::::::::::",
    "::::::::::::",
)

USER = art(
    "....####....",
    "...######...",
    "...######...",
    "...######...",
    "....####....",
    "............",
    "..########..",
    ".##########.",
    "############",
    "############",
)

KEYBOARD = art(
    "kkkkkkkkkkkkkk",
    "k.k.k.k.k.k.kk",
    "kk.k.k.k.k.k.k",
    "k.k.k.k.k.k.kk",
    "kkk........kkk",
    "kkkkkkkkkkkkkk",
)

REMINDERS = art(
    "oo.ggggggggg",
    "oo.ggggggggg",
    "............",
    "bb.ggggggggg",
    "bb.ggggggggg",
    "............",
    "rr.ggggggggg",
    "rr.ggggggggg",
)

CLOCK = art(
    "....kkkk....",
    "..kkkkkkkk..",
    ".kkk....kkk.",
    ".kk..kk..kk.",
    "kk...kk...kk",
    "kk...ookk.kk",
    "kk...ookk.kk",
    "kk........kk",
    ".kk......kk.",
    ".kkk....kkk.",
    "..kkkkkkkk..",
    "....kkkk....",
)

REFRESH = [(WHITE, ring(6, 6, 6.2, 4.2, gap=(15, 75)) | {(9, 0), (10, 0), (11, 0), (10, 1), (11, 1), (11, 2), (11, 3)})]

ICON_LOCK = G_LOCK
ICON_WIFI = G_WIFI


# ----------------------------------------------------------------------------
# Folders and documents


FOLDER_BACK = rgba("#a9d3ff")
FOLDER_FRONT = (rgba("#6cb6ff"), rgba("#2b6de8"))
EMBLEM = rgba("#ffffff", 235)

EMBLEMS = {
    None: [],
    "documents": ["####..", "#####.", "######", "######", "######", "######"],
    "downloads": ["..##..", "..##..", "######", ".####.", "..##..", "......", "######"],
    "pictures": ["....##", "....##", "..#...", ".###..", "######"],
    "music": [".#####", ".#...#", ".#...#", "##..##", "##..##"],
    "videos": ["##....", "####..", "######", "####..", "##...."],
    "home": ["..##..", ".####.", "######", "##..##", "##..##"],
    "desktop": ["######", "######", "######", "..##..", ".####."],
}


def folder(emblem=None):
    tab = {(x, 3) for x in range(2, 8)}
    back = {(x, 4) for x in range(2, 18)}
    front = {(x, y) for x in range(2, 18) for y in range(5, 17)}
    em = set()
    rows = EMBLEMS[emblem]
    if rows:
        w, h = len(rows[0]), len(rows)
        ox, oy = 10 - w // 2, 11 - h // 2
        em = {(ox + x, oy + y) for y, r in enumerate(rows) for x, ch in enumerate(r) if ch == "#"}
    layers = [(FOLDER_BACK, tab | back), (FOLDER_FRONT, front - em), (EMBLEM, em)]
    return lambda: loose(layers)


PAGE = (rgba("#ffffff"), rgba("#eef1f5"))
FOLD = rgba("#c9ced6")


def page(content=None, **colors):
    rows = ["#########..."] + ["#########f.."] + ["#########ff."] + ["#########fff"] + ["############"] * 12
    cells = {(x, y) for y, r in enumerate(rows) for x, ch in enumerate(r) if ch == "#"}
    fold = {(x, y) for y, r in enumerate(rows) for x, ch in enumerate(r) if ch == "f"}
    inner = []
    if content:
        pal = dict(PALETTE, **colors)
        for color, cs in pixgrid.parse_art(content, pal):
            inner.append((color, {(x + 2, y + 6) for x, y in cs}))
    used = set().union(*(c for _, c in inner)) if inner else set()
    layers = [(PAGE, cells - used), (FOLD, fold)] + inner
    return lambda: loose(pixgrid.offset(layers, 4, 2))


def lines(ch):
    return [ch * 8, "........", ch * 8, "........", ch * 8, "........", ch * 5 + "..."]


DOC_PDF = ["gggggggg", "........", "gggggggg", "........", "rrrrrrrr", "rrrrrrrr", "rrrrrrrr"]
DOC_IMAGE = ["......oo", "......oo", "..k.....", ".kkk..g.", "kkkkkggg", "kkkkkkkk"]
DOC_ZIP = ["...gg...", "....gg..", "...gg...", "....gg..", "...gg...", "...oo...", "...oo..."]
DOC_CODE = hcat(["..p", ".p.", "p..", ".p.", "..p"], ["..o", "..o", ".o.", "o..", "o.."], ["p..", ".p.", "..p", ".p.", "p.."])
DOC_AUDIO = ["...PPPP.", "...P..P.", "...P..P.", ".PPP.PPP", ".PPP.PPP"]
DOC_SCRIPT = ["kk......", ".kk.....", "..kk....", ".kk.....", "kk..oooo"]
DOC_EXE = ["bbbbbbbb", "BBBBBBBB", "BBBBBBBB", "BBBBBBBB", "BBBBBBBB"]


# ----------------------------------------------------------------------------
# Glyphs drawn here (12x12, white)


def g(*rows):
    return centred(art(*rows), 12)


G_DOWN = g("##......##", ".##....##.", "..##..##..", "...####...", "....##....")
G_MAX = [(WHITE, {(x, y) for x in range(6, 12) for y in (0, 1)} | {(x, y) for x in (10, 11) for y in range(0, 6)}
          | {(x, y) for x in range(0, 6) for y in (10, 11)} | {(x, y) for x in (0, 1) for y in range(6, 12)}
          | {(x, y) for x in range(3, 9) for y in range(3, 9) if x + y in (11, 12)})]
G_CLOSE = g("##......##", ".##....##.", "..##..##..", "...####...", "....##....", "...####...", "..##..##..", ".##....##.", "##......##")
FOLDER_GLYPH = [(SOFT, {(x, y) for x in range(0, 5) for y in (0,)} | {(x, 1) for x in range(0, 12)}),
                (WHITE, {(x, y) for x in range(0, 12) for y in range(2, 10)})]
G_FOLDER = centred(FOLDER_GLYPH, 12)
G_BACK = g(*LT)
G_FORWARD = g(*mirror(LT))
G_CLOUD = g(
    ".....###....",
    "....#####...",
    ".##.######..",
    "###########.",
    "############",
    "############",
    ".##########.",
)
G_RELOAD = centred(REFRESH, 12)
G_HOME = g(
    ".....##.....",
    "....####....",
    "...######...",
    "..########..",
    ".##########.",
    "############",
    ".##########.",
    ".####..####.",
    ".####..####.",
    ".####..####.",
)
G_MORE = g("##", "##", "..", "##", "##", "..", "##", "##")
G_PLUS = g(*(["....##...."] * 4 + ["##########"] * 2 + ["....##...."] * 4))
G_TRASH = centred(recolor(TRASH, {INK: WHITE}), 12)
G_GRID = g(*(["####..####"] * 4 + [".........."] * 2 + ["####..####"] * 4))
G_NOTE = centred(svg("app/musica")[2], 12)
PLAY = ["##......", "####....", "#####...", "#######.", "########", "#######.", "#####...", "####....", "##......"]
G_PLAY = g(*PLAY)
G_PAUSE = g(*(["###..###"] * 9))
DARK = rgba("#1c1c1e")
G_PLAY_DARK = recolor(G_PLAY, {WHITE: DARK})
G_PAUSE_DARK = recolor(G_PAUSE, {WHITE: DARK})
G_NEXT = g(*hcat(tri_right(9), tri_right(9)))
G_PREV = g(*mirror(hcat(tri_right(9), tri_right(9))))


# ----------------------------------------------------------------------------
# The atlas

ICONS = [
    ("folder", folder()),
    ("folder-documents", folder("documents")),
    ("folder-downloads", folder("downloads")),
    ("folder-pictures", folder("pictures")),
    ("folder-music", folder("music")),
    ("folder-videos", folder("videos")),
    ("folder-home", folder("home")),
    ("folder-desktop", folder("desktop")),
    ("trash", t("#ffffff", "#dde1e6", recolor(TRASH, {INK: rgba("#7a838e")}))),
    ("lock", t_glyph("#ffc85a", "#ff9500", ICON_LOCK)),
    ("wifi", t_glyph("#5ac8fa", "#1a8cff", ICON_WIFI)),
    ("computer", t(*LIGHT, COMPUTER)),
    ("calendar", app_svg("calendario")),
    ("activity", t("#3c4046", "#16181b", ACTIVITY)),
    ("screenshot", t(*LIGHT, SCREENSHOT)),
    ("search", t_glyph("#c58cff", "#8e44ec", G_SEARCH)),
    ("mail", app_svg("posta")),
    ("calculator", t("#4a4f55", "#1f2226", CALC)),
    ("music", app_svg("musica")),
    ("photos", app_svg("foto")),
    ("store", t("#7a9cff", "#3f51e6", STORE)),
    ("text-editor", t(*LIGHT, TEXT_EDITOR)),
    ("browser", app_svg("browser")),
    ("settings", app_svg("impostazioni")),
    ("terminal", app_svg("terminale")),
    ("files", app_svg("file")),
    ("arc", t("#5ac8fa", "#1a7fe0", ARC)),
    ("messages", app_svg("messaggi")),
    ("camera", app_svg("fotocamera")),
    ("info", info_icon),
    ("code", t("#8b7cff", "#4b3fd8", CODE)),
    ("help", t("#4fd1c5", "#14a39a", HELP)),
    ("power", t_glyph("#ff6b5e", "#e5352b", G_POWER)),
    ("refresh", t("#6be3a0", "#22b35e", REFRESH)),
    ("image-viewer", t("#6cb6ff", "#2b6de8", IMAGE)),
    ("clock", t(*LIGHT, CLOCK)),
    ("download", t("#5ee08a", "#1a9e4a", DOWNLOAD)),
    ("games", t("#ff7aa8", "#e8326f", GAMES)),
    ("archive", app_svg("pacchetti")),
    ("apps", t("#4a4f55", "#1f2226", APPS)),
    ("usb", t(*LIGHT, USB)),
    ("window", t("#6cb6ff", "#2b6de8", WINDOW)),
    ("user", t("#7fa8ff", "#3d6fe0", USER)),
    ("keyboard", t("#eef0f3", "#b8bec6", KEYBOARD)),
    ("file", page(lines("g"))),
    ("file-text", page(lines("B"))),
    ("file-pdf", page(DOC_PDF)),
    ("file-image", page(DOC_IMAGE)),
    ("file-archive", page(DOC_ZIP)),
    ("file-code", page(DOC_CODE)),
    ("file-audio", page(DOC_AUDIO)),
    ("file-script", page(DOC_SCRIPT)),
    ("file-exe", page(DOC_EXE)),
    ("g-volume", lambda: glyph(G_VOLUME)),
    ("g-wifi", lambda: glyph(G_WIFI)),
    ("g-battery", lambda: glyph(G_BATTERY)),
    ("g-power", lambda: glyph(G_POWER)),
    ("g-search", lambda: glyph(G_SEARCH)),
    ("g-down", lambda: glyph(G_DOWN)),
    ("g-max", lambda: glyph(G_MAX)),
    ("g-close", lambda: glyph(G_CLOSE)),
    ("g-folder", lambda: glyph(G_FOLDER)),
    ("g-back", lambda: glyph(G_BACK)),
    ("g-cloud", lambda: glyph(G_CLOUD)),
    ("g-reload", lambda: glyph(G_RELOAD)),
    ("g-home", lambda: glyph(G_HOME)),
    ("g-more", lambda: glyph(G_MORE)),
    ("notes", app_svg("note")),
    ("reminders", t(*LIGHT, REMINDERS)),
    ("g-mute", lambda: glyph(G_MUTE)),
    ("g-plus", lambda: glyph(G_PLUS)),
    ("g-trash", lambda: glyph(G_TRASH)),
    ("g-forward", lambda: glyph(G_FORWARD)),
    ("g-grid", lambda: glyph(G_GRID)),
    ("g-note", lambda: glyph(G_NOTE)),
    ("g-play", lambda: glyph(G_PLAY)),
    ("g-pause", lambda: glyph(G_PAUSE)),
    ("g-play-dark", lambda: glyph(G_PLAY_DARK)),
    ("g-pause-dark", lambda: glyph(G_PAUSE_DARK)),
    ("g-prev", lambda: glyph(G_PREV)),
    ("g-next", lambda: glyph(G_NEXT)),
    ("g-sun", lambda: glyph(G_SUN)),
    ("g-moon", lambda: glyph(G_MOON)),
    ("g-bluetooth", lambda: glyph(G_BLUETOOTH)),
    ("g-plane", lambda: glyph(G_PLANE)),
    ("g-lock", lambda: glyph(G_LOCK)),
    ("g-bell", lambda: glyph(G_BELL)),
]


# Icons of the games in the store, shipped with each program.
GAME_2048 = art(
    "ooooo.:::::",
    "ooooo.:::::",
    "ooooo.:::::",
    "ooooo.:::::",
    "ooooo.:::::",
    "...........",
    "#####.ooooo",
    "#####.ooooo",
    "#####.ooooo",
    "#####.ooooo",
    "#####.ooooo",
)
MINES = [(INK, ring(6, 6, 4.6, 0) | {(5, 0), (6, 0), (5, 11), (6, 11), (0, 5), (0, 6), (11, 5), (11, 6),
                                       (2, 2), (9, 2), (2, 9), (9, 9)}),
         (WHITE, {(4, 4), (4, 5)})]
SNAKE = art(
    "############",
    "############",
    "..........##",
    "..........##",
    "##########..",
    "##########..",
    "##..........",
    "##.......oo.",
    "##########..",
    "##########..",
)
PAINT = art(
    "......#####.",
    ".....######.",
    "....######..",
    "...######...",
    "..######....",
    ".oo####.....",
    "ooo##.......",
    "oooo........",
    "ooo.........",
)
SUDOKU = art(
    "bbb.kkk.kkk",
    "bbb.kkk.kkk",
    "bbb.kkk.kkk",
    "...........",
    "kkk.kkk.kkk",
    "kkk.ooo.kkk",
    "kkk.kkk.kkk",
    "...........",
    "kkk.kkk.bbb",
    "kkk.kkk.bbb",
    "kkk.kkk.bbb",
)
TRIS = art(
    "oo..oo.#####",
    ".oooo..#...#",
    "..oo...#...#",
    ".oooo..#...#",
    "oo..oo.#####",
    "............",
    "#####.oo..oo",
    "#...#..oooo.",
    "#...#...oo..",
    "#...#..oooo.",
    "#####.oo..oo",
)

STORE_ICONS = [
    ("gioco2048", t("#ffd36b", "#f5a524", GAME_2048)),
    ("campominato", t("#e9edf1", "#b9c1ca", MINES)),
    ("serpente", t("#5ee08a", "#1a9e4a", SNAKE)),
    ("disegno", t("#ff9ec4", "#e8467f", PAINT)),
    ("sudoku", t(*LIGHT, SUDOKU)),
    ("tris", t("#7a9cff", "#3f51e6", TRIS)),
]


def sheet(icons, path, cols=COLS):
    rows = (len(icons) + cols - 1) // cols
    bg = Image.new("RGBA", (cols * OUT, rows * OUT), rgba("#7a7880"))
    for i, im in enumerate(icons):
        bg.alpha_composite(im, ((i % cols) * OUT, (i // cols) * OUT))
    bg.save(path)


def main():
    if len(sys.argv) > 2 and sys.argv[1] == "--store":
        for name, draw in STORE_ICONS:
            icon = reduce(draw())
            path = os.path.join(sys.argv[2], name, "icon.qoi")
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "wb") as f:
                f.write(qoi.encode(OUT, OUT, icon.tobytes()))
            print(path)
        if os.environ.get("PREVIEW"):
            sheet([reduce(d()) for _, d in STORE_ICONS], os.environ["PREVIEW"])
        return
    out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(HERE, "..", "..", "rootfs", "system", "gui")
    os.makedirs(out, exist_ok=True)
    icons = [reduce(draw()) for _, draw in ICONS]
    rows = (len(ICONS) + COLS - 1) // COLS
    atlas = Image.new("RGBA", (COLS * OUT, rows * OUT), (0, 0, 0, 0))
    for i, icon in enumerate(icons):
        atlas.paste(icon, ((i % COLS) * OUT, (i // COLS) * OUT))
    data = qoi.encode(atlas.width, atlas.height, atlas.tobytes())
    with open(os.path.join(out, "icons.qoi"), "wb") as f:
        f.write(data)
    with open(os.path.join(out, "icons.idx"), "w") as f:
        f.write("\n".join(n for n, _ in ICONS) + "\n")
    if os.environ.get("PREVIEW"):
        sheet(icons, os.environ["PREVIEW"])
    print(f"{out}/icons.qoi: {len(ICONS)} icons, {atlas.width}x{atlas.height}, {len(data)} bytes")


if __name__ == "__main__":
    main()

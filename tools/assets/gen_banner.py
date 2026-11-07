#!/usr/bin/env python3
"""Paints the README banner: the "pixel" wallpaper (gen_pixel.py) with the
justOS wordmark in its sky, drawn on the same grid as the landscape (two
cells per pixel of the wordmark) with a soft drop shadow.

The wordmark is the one of the boot splash (system/kernel/src/gui/splash.rs):
"JUSTOS" in a pixel font seven pixels tall, then the orange cursor block.

Output: docs/brand/banner.png (1280x720). Only Pillow is needed.

Usage: tools/assets/gen_banner.py [output.png]
"""
import os
import sys

from PIL import Image

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import gen_pixel  # noqa: E402

WORD = [
    "#####.#...#..####.#####..###...####.",
    "...#..#...#.#.......#...#...#.#.....",
    "...#..#...#.#.......#...#...#.#.....",
    "...#..#...#..###....#...#...#..###..",
    "...#..#...#.....#...#...#...#.....#.",
    "#..#..#...#.....#...#...#...#.....#.",
    ".##....###..####....#....###..####..",
]
# The cursor fills columns 36..39, after a column of space.
COLS = 40
CREAM = (0xF2, 0xEF, 0xE6)
ORANGE = (0xFF, 0x5A, 0x1F)

# The rows of the landscape kept (of 100), where the wordmark goes (in
# landscape cells), how many cells one of its pixels takes, and how many
# screen pixels one cell becomes.
TOP, BOTTOM = 2, 92
AT_X, AT_Y = 8, 14
K = 2
CELL = 8


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else "docs/brand/banner.png"
    rows = [list(r) for r in gen_pixel.paint()[TOP:BOTTOM]]
    h, w = len(rows), gen_pixel.GW
    marks = [(c, r, ORANGE if c >= 36 else CREAM) for r, line in enumerate(WORD) for c in range(COLS) if c >= 36 or line[c] == "#"]

    def cells(c, r, dx=0, dy=0):
        for y in range(K):
            for x in range(K):
                px, py = AT_X + c * K + x + dx, AT_Y + r * K + y + dy
                if 0 <= px < w and 0 <= py < h:
                    yield px, py

    # The shadow first, one cell down and right, then the letters.
    for c, r, _ in marks:
        for x, y in cells(c, r, 1, 1):
            rows[y][x] = tuple(int(v * 0.45) for v in rows[y][x])
    for c, r, color in marks:
        for x, y in cells(c, r):
            rows[y][x] = color
    data = bytes(v for row in rows for px in row for v in px)
    img = Image.frombytes("RGB", (w, h), data).resize((w * CELL, h * CELL), Image.NEAREST)
    os.makedirs(os.path.dirname(out) or ".", exist_ok=True)
    img.save(out, optimize=True)
    print(f"{out}: {img.width}x{img.height}, {os.path.getsize(out)} bytes")


if __name__ == "__main__":
    main()

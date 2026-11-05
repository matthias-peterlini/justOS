#!/usr/bin/env python3
"""Renders the GUI fonts into an anti-aliased glyph atlas for the kernel.

The kernel has no font rasterizer: it draws text from alpha masks rendered
here, once, with Pillow. Output: system/kernel/assets/fonts.bin, and with
--sharp the same faces at twice the size for screens with two pixels per
unit (system/kernel/assets/fonts@2x.bin.gz, gzipped).

Format (little-endian):
    "ARCF" u16 version=1, u16 font count
    per font:  u16 size, u16 line height, i16 ascent, u16 glyph count,
               then per glyph: u32 code point, u8 width, u8 height,
               i8 x offset, i8 y offset (top of the mask relative to the
               baseline, negative is up), u8 advance, u8 pad,
               u32 offset of the mask in the data area
    data area: one byte of coverage (0-255) per mask pixel, row by row

Fonts: Google Sans Flex (SIL Open Font License, in tools/assets/fonts, the
typeface of the arc design; a variable font, rendered at the weight and
optical size each use asks for) and DejaVu Sans Mono (Bitstream Vera
licence); see system/kernel/assets/LICENSES.md.

Usage: tools/assets/gen_fonts.py [--web | --sharp] [output]
"""
import struct
import sys

from PIL import Image, ImageDraw, ImageFont

import os

SANS = os.path.join(os.path.dirname(os.path.abspath(__file__)), "fonts", "GoogleSansFlex.ttf")
DEJAVU = "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"

TEXT = [chr(c) for c in range(0x20, 0x7F)] + list("àèéìòùÀÈÉÌÒÙçÇ°·…–—€«»‘’“”×÷±²³•©−★→›")
DIGITS = list(" 0123456789:.,-")

# (file, pixel size, characters, weight) — the order is the font id in the
# kernel. The weight applies to the variable Google Sans Flex only.
FONTS = [
    (SANS, 14, TEXT, 400),                  # 0 body
    (SANS, 14, TEXT, 560),                  # 1 body, strong
    (SANS, 12, TEXT, 400),                  # 2 small
    (SANS, 18, TEXT, 500),                  # 3 heading
    (SANS, 32, TEXT, 500),                  # 4 title
    (SANS, 64, DIGITS, 400),                # 5 big clock
    (DEJAVU, 14, TEXT, None),               # 6 terminal
    (SANS, 22, TEXT, 500),                  # 7 logo / buttons
    (SANS, 11, TEXT, 600),                  # 8 spaced capitals ("OGGI")
]


def load(path, size, weight, optical=None):
    font = ImageFont.truetype(path, size)
    if weight is not None:
        axes = []
        for a in font.get_variation_axes():
            name = a["name"].decode() if isinstance(a["name"], bytes) else a["name"]
            if "Weight" in name:
                axes.append(weight)
            elif "Optical" in name:
                # The optical size is the size the text is read at: twice the
                # pixels on a sharper screen are the same size to the eye.
                axes.append(max(a["minimum"], min(a["maximum"], optical or size)))
            else:
                axes.append(a["default"])
        font.set_variation_by_axes(axes)
    return font


def render(path, size, chars, weight=None, optical=None):
    font = load(path, size, weight, optical)
    ascent, descent = font.getmetrics()
    glyphs = []
    for ch in chars:
        left, top, right, bottom = font.getbbox(ch, anchor="ls")
        advance = round(font.getlength(ch))
        if ch == " ":
            # Spaces rounded down to whole pixels run words together at the
            # small sizes.
            advance = max(advance, round(size * 0.28))
        w, h = max(0, right - left), max(0, bottom - top)
        if w == 0 or h == 0:
            glyphs.append((ord(ch), 0, 0, 0, 0, advance, b""))
            continue
        img = Image.new("L", (w, h), 0)
        ImageDraw.Draw(img).text((-left, -top), ch, font=font, fill=255, anchor="ls")
        glyphs.append((ord(ch), w, h, left, top, advance, img.tobytes()))
    return ascent, ascent + descent, glyphs


# The faces of web pages (the browser), in sys:/system/webfonts.bin: every
# size a page is likely to ask for, regular and bold, and a monospaced face.
# The size field carries the style in its high bits (0x1000 bold, 0x2000
# monospaced); the file is gzipped.
WEB_TEXT = ([chr(c) for c in range(0x20, 0x7F)] + [chr(c) for c in range(0xA0, 0x180)]
            + list("ƒˆ˜‐‑‒–—―‘’‚‛“”„†‡•…‰′″‹›€™←↑→↓↔−∞≈≠≤≥✓✔✕✗★☆♥◆●○■□▲▶▼◀"))
WEB_SIZES = [10, 11, 12, 13, 14, 15, 16, 17, 18, 20, 22, 24, 26, 28, 30, 32, 36, 40, 44, 48]
WEB_FONTS = ([(SANS, s, WEB_TEXT, s, 400) for s in WEB_SIZES]
             + [(SANS, s, WEB_TEXT, 0x1000 | s, 700) for s in WEB_SIZES]
             + [(DEJAVU, s, WEB_TEXT, 0x2000 | s, None) for s in (11, 12, 13, 14, 16)])


def main():
    mode = sys.argv[1] if len(sys.argv) > 1 and sys.argv[1] in ("--web", "--sharp") else None
    web, sharp = mode == "--web", mode == "--sharp"
    args = sys.argv[2:] if mode else sys.argv[1:]
    default = {"--web": "rootfs/system/webfonts.bin", "--sharp": "system/kernel/assets/fonts@2x.bin.gz"}
    out = args[0] if args else default.get(mode, "system/kernel/assets/fonts.bin")
    if web:
        fonts = [(p, s, c, tag, wt, None) for p, s, c, tag, wt in WEB_FONTS]
    elif sharp:
        fonts = [(p, s * 2, c, s * 2, wt, s) for p, s, c, wt in FONTS]
    else:
        fonts = [(p, s, c, s, wt, None) for p, s, c, wt in FONTS]
    header = bytearray(b"ARCF" + struct.pack("<HH", 1, len(fonts)))
    data = bytearray()
    tables = bytearray()
    for path, size, chars, tag, weight, optical in fonts:
        ascent, line, glyphs = render(path, size, chars, weight, optical)
        tables += struct.pack("<HHhH", tag, line, ascent, len(glyphs))
        for code, w, h, x, y, adv, mask in glyphs:
            assert w < 256 and h < 256 and -128 <= x < 128 and -128 <= y < 128 and adv < 256, (path, size, chr(code))
            tables += struct.pack("<IBBbbBBI", code, w, h, x, y, adv, 0, len(data))
            data += mask
    blob = bytes(header + tables + data)
    if web or sharp:
        import gzip
        blob = gzip.compress(blob, 9, mtime=0)
    with open(out, "wb") as f:
        f.write(blob)
    print(f"{out}: {len(fonts)} fonts, {len(blob)} bytes")


if __name__ == "__main__":
    main()

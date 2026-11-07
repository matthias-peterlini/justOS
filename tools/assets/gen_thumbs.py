#!/usr/bin/env python3
"""Makes the small previews of the wallpapers that Settings and the first
setup show, so they do not have to decode every full-size picture.

Input: every <name>.qoi in the wallpaper folder.
Output: <folder>/thumbs/<name>.qoi (320x200).

Usage: tools/assets/gen_thumbs.py [wallpaper folder]
"""
import os
import sys

from PIL import Image

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import qoi  # noqa: E402

W, H = 320, 200


def main():
    folder = sys.argv[1] if len(sys.argv) > 1 else "rootfs/system/gui/wallpapers"
    os.makedirs(os.path.join(folder, "thumbs"), exist_ok=True)
    for f in sorted(os.listdir(folder)):
        if not f.endswith(".qoi"):
            continue
        with open(os.path.join(folder, f), "rb") as src:
            w, h, rgba = qoi.decode(src.read())
        img = Image.frombytes("RGBA", (w, h), rgba).resize((W, H), Image.LANCZOS)
        with open(os.path.join(folder, "thumbs", f), "wb") as out:
            out.write(qoi.encode(W, H, img.tobytes()))
        print(f)


if __name__ == "__main__":
    main()

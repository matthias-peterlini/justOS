#!/usr/bin/env python3
"""Downloads the photo wallpapers of the Dolomites from Unsplash.

Each photo is free to use under the Unsplash License
(https://unsplash.com/license), which allows copying and distributing it
without asking; the photographers are credited anyway. The script crops
each one to 16:10, scales it to 1920x1200 and writes it as <name>.qoi, plus
CREDITS.txt with photographer and source of each one.

Output: rootfs/system/gui/wallpapers/<name>.qoi and CREDITS.txt.

Usage: tools/assets/fetch_photos.py [output folder]
"""
import io
import os
import sys
import urllib.request

from PIL import Image

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import qoi  # noqa: E402

W, H = 1920, 1200
UA = {"User-Agent": "JustOS-wallpapers/1.0 (https://github.com/matthias-peterlini/justOS)"}

# name: (Unsplash photo page, its picture on images.unsplash.com, label,
# photographer, where the 16:10 crop sits: 0 = top/left, 0.5 = centre,
# 1 = bottom/right)
PHOTOS = {
    "cadini": ("EsZoWxyzBpg", "photo-1727946283836-cec378342537", "Cadini di Misurina", "Samuel Quek", 0.3),
    "braies": ("O453M2Liufs", "photo-1476514525535-07fb3b4ae5f1", "Lago di Braies", "Luca Bravo", 0.5),
    "seceda": ("nMdaUdsswl4", "photo-1502464229295-67f6289dddfc", "Seceda all'alba", "Justin Luebke", 0.5),
    "siusi": ("p7S6AxykCl0", "photo-1534106474077-f9e9c6f5a47c", "Alpe di Siusi", "Michiel Ronde", 0.5),
    "brenta": ("O-YEpluawy0", "photo-1580735252908-42cdf8e0a8f3", "Dolomiti di Brenta", "Andrea Belussi", 0.7),
}


def fetch(url):
    return urllib.request.urlopen(urllib.request.Request(url, headers=UA), timeout=120).read()


def crop(img, at):
    w, h = img.size
    if w * H > h * W:  # wider than 16:10: cut the sides
        cw = h * W // H
        x = round((w - cw) * at)
        box = (x, 0, x + cw, h)
    else:  # taller: cut top and bottom
        ch = w * H // W
        y = round((h - ch) * at)
        box = (0, y, w, y + ch)
    return img.crop(box).resize((W, H), Image.LANCZOS)


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else "rootfs/system/gui/wallpapers"
    os.makedirs(out, exist_ok=True)
    credits = ["Photo wallpapers (from Unsplash), cropped and scaled.",
               "Free to use under the Unsplash License: https://unsplash.com/license", ""]
    for name, (photo, picture, label, author, at) in PHOTOS.items():
        data = fetch(f"https://images.unsplash.com/{picture}?w=3840&q=90&fm=jpg")
        img = crop(Image.open(io.BytesIO(data)).convert("RGBA"), at)
        with open(os.path.join(out, f"{name}.qoi"), "wb") as f:
            f.write(qoi.encode(W, H, img.tobytes()))
        credits += [f"{name}.qoi: {label}", f"  {author}", f"  https://unsplash.com/photos/{photo}", ""]
        print(f"{name}: {label}, {author}")
    with open(os.path.join(out, "CREDITS.txt"), "w", encoding="utf-8") as f:
        f.write("\n".join(credits))


if __name__ == "__main__":
    main()

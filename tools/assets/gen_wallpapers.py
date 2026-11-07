#!/usr/bin/env python3
"""Paints the justOS wallpapers.

Every wallpaper is generated here from noise and gradients: a sky, a far
range in the haze, a massif of Dolomite towers whose rock is lit from the
heightfield of its gullies, a sea of fog and rows of firs in front. Two
moods share the same landscape: "dolomiti" (alpenglow at sunset) and
"notte" (moonlight). The default wallpaper, "pixel", is painted by
gen_pixel.py.

Output: rootfs/system/gui/wallpapers/<name>.qoi (1440x900).

Usage: tools/assets/gen_wallpapers.py [output folder]
"""
import os
import sys

import numpy as np
from PIL import Image, ImageDraw, ImageFilter

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import qoi  # noqa: E402

W, H = 1440, 900
XS = np.arange(W)[None, :] / W
YS = np.arange(H)[:, None] / H
X = np.arange(W) / W


def upscale(g, w, h):
    return np.asarray(Image.fromarray(g.astype(np.float32), "F").resize((w, h), Image.BICUBIC), float)


def fbm(h, w, sx, sy, seed, octaves=5, ridged=False, gain=0.5):
    """Fractal noise with features about `sx` x `sy` pixels at the first octave."""
    rng = np.random.default_rng(seed)
    out = np.zeros((h, w))
    amp, total = 1.0, 0.0
    for o in range(octaves):
        gw, gh = max(2, int(w / sx * 2 ** o)), max(2, int(h / sy * 2 ** o))
        n = upscale(rng.random((gh, gw)), w, h)
        if ridged:
            n = 1 - np.abs(2 * n - 1)
        out += amp * n
        total += amp
        amp *= gain
    return out / total


def noise1d(n, scale, seed, octaves=6, ridged=False, gain=0.5):
    return fbm(1, n, scale, 1, seed, octaves, ridged, gain)[0]


def rgb(h):
    h = h.lstrip("#")
    return np.array([int(h[i:i + 2], 16) for i in (0, 2, 4)], float)


def lerp(a, b, t):
    return a + (b - a) * t


def vgrad(stops):
    col = np.zeros((H, 3))
    for c in range(3):
        col[:, c] = np.interp(np.linspace(0, 1, H), [p for p, _ in stops], [rgb(s)[c] for _, s in stops])
    return np.repeat(col[:, None, :], W, 1)


MOODS = {
    "dolomiti": dict(
        sky=[(0, "#5d5370"), (0.25, "#8f7486"), (0.45, "#c48d86"), (0.6, "#d9a28e")],
        clouds="#f0c0b0", far="#9a7f8e", dark="#3a2427", rock="#b4532c", glow="#f08a46",
        shadow="#2c2028", haze="#7d6070", fog="#d8c8cc", mist=(200, 180, 186),
        trees=["#5e5560", "#3c3a40", "#1d2224", "#111517"], stars=False),
    "notte": dict(
        sky=[(0, "#060a1c"), (0.3, "#121b40"), (0.5, "#24305e"), (0.6, "#2f3a68")],
        clouds="#3a4677", far="#2a3360", dark="#121731", rock="#4c5886", glow="#9fb0de",
        shadow="#0d1128", haze="#2b3463", fog="#4a5686", mist=(70, 82, 126),
        trees=["#1d2546", "#131a33", "#0a0f1f", "#05080f"], stars=True),
}


def fir(draw, rng, x, base, h, color):
    w = h * 0.22
    tiers = int(8 + h / 25)
    for t in range(tiers):
        f = t / tiers
        ty = base - h * f
        tw = w * (1 - f) ** 1.1 + 1.5
        j = (rng.random() - 0.5) * tw * 0.25
        draw.polygon([(x - tw + j, ty), (x + tw + j, ty + 2), (x, ty - h / tiers * 2.2)], fill=color)
    draw.line([(x, base - h - 3), (x, base)], fill=color, width=max(1, int(h / 60)))


def paint(mood):
    m = MOODS[mood]
    img = vgrad(m["sky"])
    cl = fbm(H, W, 420, 160, 3, 6)
    cl = np.clip((cl - 0.5) * 3, 0, 1) * np.clip(1 - YS * 1.8, 0, 1)
    img = lerp(img, rgb(m["clouds"]), cl[..., None] * 0.5)

    if m["stars"]:
        rng = np.random.default_rng(3)
        n = 900
        sy, sx, br = rng.integers(0, int(H * 0.55), n), rng.integers(0, W, n), rng.random(n) ** 3
        for y, x, b in zip(sy, sx, br):
            img[y, x] = lerp(img[y, x], np.array([255, 255, 255.]), 0.35 + 0.65 * b)
        # The moon, with a soft halo.
        d = np.hypot(XS - 0.22, (YS - 0.16) * H / W)
        img = lerp(img, rgb("#8a97c8"), (np.clip(1 - d / 0.12, 0, 1) ** 2 * 0.5)[..., None])
        img = np.where((d < 0.022)[..., None], rgb("#f2f0e6"), img)

    # Far range, hazy.
    far = (0.58 - noise1d(W, 160, 5, 8, ridged=True, gain=0.55) * 0.09
           - np.exp(-((X - 0.18) / 0.10) ** 2) * 0.10 - np.exp(-((X - 0.33) / 0.05) ** 2) * 0.05)
    img = np.where((YS > far[None, :])[..., None], lerp(img, rgb(m["far"]), 0.85), img)

    # The massif: a few towers with broken tops.
    env = np.maximum.reduce([
        np.exp(-((X - 0.66) / 0.10) ** 4) * 0.95, np.exp(-((X - 0.80) / 0.05) ** 4) * 0.82,
        np.exp(-((X - 0.90) / 0.035) ** 2) * 0.6, np.exp(-((X - 0.52) / 0.05) ** 4) * 0.55,
        np.exp(-((X - 0.70) / 0.22) ** 2) * 0.5])
    towers = noise1d(W, 40, 11, 6, ridged=True, gain=0.55)
    crest = 0.66 - env * 0.50 - env * (towers - 0.5) * 0.16 - (noise1d(W, 8, 12, 4) - 0.5) * 0.015 * env
    crest = np.minimum(crest, 0.64)
    mask = YS > crest[None, :]
    depth = np.clip((YS - crest[None, :]) / 0.45, 0, 1)
    # Gullies as a heightfield; its slope across gives the light from the west.
    relief = fbm(H, W, 30, 120, 21, 6, ridged=True, gain=0.55) * 0.45 + fbm(H, W, 140, 260, 22, 4) * 0.55
    gx = np.gradient(relief, axis=1) * W / 40
    strata = fbm(H, W, 900, 6, 23, 3)
    light = np.clip(0.55 - gx * 0.9 + (0.70 - X[None, :]) * 0.6, 0, 1) * (0.75 + 0.5 * strata)
    glow = np.clip(1 - depth * 1.9, 0, 1) ** 1.3
    lit = lerp(rgb(m["dark"]), rgb(m["rock"]), light[..., None])
    lit = lerp(lit, rgb(m["glow"]), (np.clip(light - 0.6, 0, 1) * 2.2 * glow)[..., None])
    rock = lerp(rgb(m["shadow"]), lit, (0.35 + 0.65 * glow)[..., None])
    rock = lerp(rock, rgb(m["haze"]), (np.clip(depth - 0.35, 0, 1) * 0.9)[..., None])
    img = np.where(mask[..., None], rock, img)

    # A sea of fog with a rolling top.
    edge = 0.62 + (noise1d(W, 180, 33, 6) - 0.5) * 0.08
    fn = fbm(H, W, 300, 60, 31, 5)
    band = (np.exp(-((YS - edge[None, :] - 0.02) / 0.06) ** 2) * (0.45 + 0.8 * fn)
            + np.clip((YS - edge[None, :]) / 0.10, 0, 1))
    img = lerp(img, rgb(m["fog"]), np.clip(band, 0, 0.97)[..., None])

    out = Image.fromarray(np.clip(img, 0, 255).astype(np.uint8)).convert("RGBA")
    rng = np.random.default_rng(17)
    for i, (y0, size, blur, gap) in enumerate([(0.80, 55, 1.5, 0.5), (0.85, 85, 1.0, 0.55), (0.91, 140, 0.5, 0.6), (1.0, 230, 0, 0.7)]):
        layer = Image.new("RGBA", (W, H), (0, 0, 0, 0))
        draw = ImageDraw.Draw(layer)
        color = tuple(int(v) for v in rgb(m["trees"][i])) + (255,)
        x = -30.0
        while x < W + 30:
            h = size * (0.55 + 0.7 * rng.random())
            fir(draw, rng, x, H * y0 + rng.random() * 12, h, color)
            x += h * 0.22 * (gap + rng.random() * 0.9)
        if H * y0 + 6 < H:
            draw.rectangle((0, int(H * y0) + 6, W, H), fill=color)
        if blur:
            layer = layer.filter(ImageFilter.GaussianBlur(blur))
        out.alpha_composite(layer)
        if i < 3:
            a = np.exp(-((np.arange(H) / H - (y0 + 0.03)) / 0.035) ** 2) * 90
            mist = Image.new("RGBA", (W, H), m["mist"] + (0,))
            mist.putalpha(Image.fromarray(np.repeat(a[:, None], W, 1).astype(np.uint8), "L"))
            out.alpha_composite(mist)
    a = np.asarray(out.convert("RGB"), float)
    vignette = 1 - 0.30 * (((XS - 0.5) ** 2) * 1.5 + ((YS - 0.45) ** 2) * 1.3)
    a = a * vignette[..., None] + (np.random.default_rng(9).random((H, W, 1)) - 0.5) * 4
    return Image.fromarray(np.clip(a, 0, 255).astype(np.uint8)).filter(ImageFilter.GaussianBlur(0.5))


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else "rootfs/system/gui/wallpapers"
    os.makedirs(out, exist_ok=True)
    for name in MOODS:
        img = paint(name).convert("RGBA")
        data = qoi.encode(W, H, img.tobytes())
        with open(os.path.join(out, name + ".qoi"), "wb") as f:
            f.write(data)
        if os.environ.get("PREVIEW"):
            img.convert("RGB").save(os.path.join(os.environ["PREVIEW"], name + ".png"))
        print(f"{out}/{name}.qoi: {len(data)} bytes")


if __name__ == "__main__":
    main()

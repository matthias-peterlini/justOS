#!/usr/bin/env python3
"""Paints "pixel", the default JustOS wallpaper: Dolomites at sunset in big
pixels, the sky and the rock shaded with ordered (Bayer) dithering between a
few warm colours around the JustOS orange (#ff5a1f).

It is drawn on a 160x100 grid and each cell becomes a 16x16 square, so the
picture is 2560x1600 and its flat runs compress well in QOI. Only Pillow
is needed.

Output: rootfs/system/gui/wallpapers/pixel.qoi

Usage: tools/assets/gen_pixel.py [output folder] [preview.png]
"""
import os
import random
import sys

from PIL import Image

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import qoi  # noqa: E402

GW, GH = 160, 100
B = 16

BAYER = [[(v + 0.5) / 16 for v in row] for row in [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]]]


def rgb(h):
    h = h.lstrip("#")
    return tuple(int(h[i:i + 2], 16) for i in (0, 2, 4))


def ramp(t, x, y, colors):
    """The colour at `t` (0..1) along `colors`, dithered with the Bayer matrix."""
    t = min(max(t, 0.0), 1.0)
    pos = t * (len(colors) - 1)
    i = int(pos)
    if i >= len(colors) - 1:
        return colors[-1]
    f = pos - i
    return colors[i + 1] if f > BAYER[y % 4][x % 4] else colors[i]


def noise1d(n, seed, scale, octaves=4):
    """Smooth noise in 0..1, features about `scale` cells wide."""
    rng = random.Random(seed)
    out = [0.0] * n
    amp, total = 1.0, 0.0
    for o in range(octaves):
        k = max(2, int(n / scale * 2 ** o)) + 1
        pts = [rng.random() for _ in range(k)]
        for x in range(n):
            p = x * (k - 1) / (n - 1)
            i = min(int(p), k - 2)
            f = p - i
            f = f * f * (3 - 2 * f)
            out[x] += amp * (pts[i] + (pts[i + 1] - pts[i]) * f)
        total += amp
        amp *= 0.5
    return [v / total for v in out]


SKY = [rgb(c) for c in ["#140c0c", "#2a0f0d", "#521610", "#8a2110", "#c93512", "#f2501c", "#ff6e21", "#ff9226", "#ffb732", "#ffd977", "#f5ebd2"]]
SUN = [rgb(c) for c in ["#fff7e0", "#ffeaa8", "#ffd25a"]]
FAR = [rgb(c) for c in ["#f0743a", "#e0582a", "#c94222"]]
LIT = [rgb(c) for c in ["#ffc26a", "#ff9a3a", "#ff7426", "#ff5a1f", "#d9431b"]]
SHADE = [rgb(c) for c in ["#a8341c", "#7c2618", "#561c15", "#3a1511"]]
GULLY = rgb("#2c110f")
SUMMIT = rgb("#ffe3a8")
FOG = [rgb(c) for c in ["#ffd99a", "#ffb45a", "#ff8f3e", "#e5652c"]]
GROUND = [rgb(c) for c in ["#3a1511", "#24110f", "#161212"]]
TREE = rgb("#120e0d")


def paint():
    img = [[None] * GW for _ in range(GH)]
    horizon = 70
    sun = (36, 58, 14)
    # Sky, and the sun low on the left.
    for y in range(GH):
        for x in range(GW):
            img[y][x] = ramp(y / horizon, x, y, SKY)
            dx, dy = x - sun[0], y - sun[1]
            if dx * dx + dy * dy <= sun[2] * sun[2]:
                t = (y - (sun[1] - sun[2])) / (2 * sun[2])
                # Stripes cut through its lower half.
                if not (t > 0.5 and (y - sun[1]) % 3 == 0):
                    img[y][x] = ramp(t, x, y, SUN)
    base = 76
    # The far range in the haze.
    far = [horizon - 4 - int(v * 22) + int(w * 6) for v, w in zip(noise1d(GW, 3, 30), noise1d(GW, 5, 6))]
    for x in range(GW):
        for y in range(far[x], base):
            img[y][x] = ramp((y - far[x]) / 14, x, y, FAR)
    # The massif: peaks as steep cones. (centre, top, steepness)
    peaks = [
        (60, 50, 1.8, 0.30), (76, 38, 2.2, 0.45), (91, 30, 2.6, 0.25), (103, 19, 3.0, 0.40), (112, 25, 3.3, 0.20),
        (121, 14, 2.8, 0.35), (137, 31, 2.4, 0.50), (153, 42, 2.0, 0.30),
    ]

    rng = random.Random(7)
    jag = [rng.randint(-1, 1) for _ in range(GW)]
    gully_cols = {x for x in range(GW) if rng.random() < 0.14}
    ledge = [rng.randrange(5) for _ in range(GW)]

    def drop(c, top, k, x):
        # Steep near the summit, flaring into scree towards the base.
        d = abs(x - c)
        return top + k * d ** 0.72 + (jag[x] if d > 1 else 0)

    for x in range(GW):
        for y in range(base + 1):
            # The cell belongs to the peak whose ridge (running down and to
            # the right from the summit) is nearest, among those whose
            # outline holds it.
            owner, near = None, 1e9
            for c, top, k, slant in peaks:
                ridge = c + (y - top) * slant
                if y >= drop(c, top, k, x) and abs(x - ridge) < near:
                    owner, near = (c, top, k, slant), abs(x - ridge)
            if owner is None:
                continue
            c, top, k, slant = owner
            t = (y - top) / max(1, base - top)
            lit = x < c + (y - top) * slant or (x == c and y < top + 2)
            if lit:
                img[y][x] = ramp(t * 1.1, x, y, LIT)
                # Ledges: short darker steps across the lit faces.
                if (y + ledge[x // 3]) % 6 == 0 and y > top + 2:
                    img[y][x] = ramp(t * 1.1 + 0.3, x, y, LIT)
            else:
                img[y][x] = ramp(t * 0.85, x, y, SHADE)
            if x in gully_cols and y > drop(c, top, k, x) + 3 and (y + x) % 4:
                img[y][x] = GULLY if not lit else LIT[-1]
            # The last light on the summits.
            if lit and y < top + 3:
                img[y][x] = SUMMIT
    # Fog in the valley.
    for y in range(base - 3, GH):
        for x in range(GW):
            t = (y - (base - 3)) / 6
            if t > BAYER[y % 4][x % 4] * 0.9:
                img[y][x] = ramp((y - base + 3) / 12, x, y, FOG)
    # The ground, and firs along its edge.
    ground = [84 + int(v * 7) for v in noise1d(GW, 11, 50)]
    for x in range(GW):
        for y in range(ground[x], GH):
            img[y][x] = ramp((y - ground[x]) / 9, x, y, GROUND)
    xs = sorted(rng.sample(range(GW), 30))
    for tx in xs:
        h = rng.randint(6, 12)
        gy = ground[tx] + 1
        for k in range(h):
            y = gy - k
            w = (h - k) // 3 if k > 1 else 0
            for x in range(tx - w, tx + w + 1):
                if 0 <= x < GW and 0 <= y < GH:
                    img[y][x] = TREE
    return img


def render(img):
    data = bytes(v for row in img for px in row for v in px)
    return Image.frombytes("RGB", (GW, GH), data).resize((GW * B, GH * B), Image.NEAREST)


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else "rootfs/system/gui/wallpapers"
    os.makedirs(out, exist_ok=True)
    img = render(paint())
    if len(sys.argv) > 2:
        img.save(sys.argv[2])
    data = qoi.encode(img.width, img.height, img.convert("RGBA").tobytes())
    with open(os.path.join(out, "pixel.qoi"), "wb") as f:
        f.write(data)
    print(f"{out}/pixel.qoi: {len(data)} bytes")


if __name__ == "__main__":
    main()

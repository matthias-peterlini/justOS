#!/bin/sh
# Checks a GTK 3 program on the desktop: Debian's gtk3-widget-factory (in
# the image, from LINUX_PKGS) is started from the serial console; its window
# has to appear, drawn with the justOS theme (the orange of checks, switches
# and bars, none of Adwaita's blue), and go away with Ctrl+Q.
# The GUI is driven with scripts/gui-shots.py, through the setup as in
# scripts/gui/tour.txt. The wallpaper has the same orange, so the window's
# orange is what a screenshot before it does not have.
#
# Usage: scripts/test-gtk.sh <image>    (VERBOSE=1 prints the log,
# SHOTS=<folder> keeps the screenshots)
set -u
IMAGE=$(realpath "${1:?usage: test-gtk.sh <image>}")
HERE=$(cd "$(dirname "$0")" && pwd)
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
cd "$WORK"
cp --sparse=always "$IMAGE" justos.img || exit 2

cat > steps.txt <<'EOF'
wait 10
click 512 543
wait 1
type Arc
key ret
wait 1
key ret
wait 1
key ret
wait 1.5
key ret
wait 4
serial cd sys:/usr/bin
wait 1
shot before
serial ./gtk3-widget-factory
until 120 of process
wait 4
shot window
key ctrl-q
wait 4
shot closed
EOF
OUT=${SHOTS:-$WORK/shots}
"$HERE/gui-shots.py" justos.img steps.txt "$OUT" > shots.txt 2>&1
LOG=$OUT/serial.log

FAILED=0
sed 's/\x1b\[[0-9;]*m//g' "$LOG" 2>/dev/null | grep -q 'wayland: window ".*" [0-9]*x[0-9]* of process' ||
    { echo "  missing: the program's window (wayland: window ...)"; FAILED=1; }
grep -q "stopped:\|panic" "$LOG" 2>/dev/null && { grep "stopped:\|panic" "$LOG" | sed 's/^/  /' | head -5; FAILED=1; }

python3 - "$OUT" > colours.txt <<'EOF' || FAILED=1
import struct, sys, zlib
def pixels(path):
    data = open(path, "rb").read()
    w, h = struct.unpack(">II", data[16:24])
    pos, idat = 8, b""
    while pos < len(data):
        n, kind = struct.unpack(">I4s", data[pos:pos + 8])
        if kind == b"IDAT":
            idat += data[pos + 8:pos + 8 + n]
        pos += 12 + n
    raw = zlib.decompress(idat)
    rows = b"".join(raw[y * (w * 3 + 1) + 1:(y + 1) * (w * 3 + 1)] for y in range(h))
    return [rows[i:i + 3] for i in range(0, len(rows), 3)]
out, ok = sys.argv[1], True
try:
    before, window, closed = (pixels(f"{out}/{s}.png") for s in ("before", "window", "closed"))
except OSError as e:
    print(f"  missing: a screenshot ({e.filename})"); sys.exit(1)
orange = bytes.fromhex("ff5a1f")
# Adwaita's accent: its checks, radios, switches, bars and links.
blue = {bytes.fromhex(c) for c in ("1f76e1", "1b6acb", "3584e4", "15539e")}
added = sum(1 for b, w in zip(before, window) if w == orange and b != orange)
left = sum(1 for b, c in zip(before, closed) if c == orange and b != orange)
adwaita = sum(1 for w in window if w in blue)
for what, n, good in (("orange of the window", added, added >= 2000), ("Adwaita blue", adwaita, adwaita < 50),
                      ("orange left after Ctrl+Q", left, left < 300)):
    print(f"  {what}: {n}" + ("" if good else "  <- wrong"))
    ok &= good
sys.exit(0 if ok else 1)
EOF
grep -q "wrong\|missing" colours.txt && cat colours.txt
[ "${VERBOSE:-0}" = 1 ] && { cat shots.txt; cat colours.txt; cat "$LOG"; }
[ $FAILED = 0 ] && echo "  ok    gtk (the window of gtk3-widget-factory, its theme, closed with Ctrl+Q)"
exit $FAILED

#!/bin/sh
# Checks the Wayland compositor: a client built here against Debian's
# libwayland-client and libxkbcommon (scripts/linux/wl-test.c, with the
# xdg-shell and xdg-decoration code wayland-scanner makes from Debian's
# wayland-protocols) opens a window on the desktop. The GUI is driven with
# scripts/gui-shots.py: through the setup, the client started from the
# serial console, then clicks, a right click (a popup), typing (with the
# Italian layout the setup picks: an accented letter too), full screen and
# back (F11), and Ctrl+Q.
# The client's lines in the serial log and the colours in the screenshots
# are checked.
#
# The headers, libraries and wayland-scanner come from Debian's packages
# (tools/linux/debroot.py, into build/wldev; downloads are kept in
# build/debcache).
#
# Usage: scripts/test-wayland.sh <image>    (VERBOSE=1 prints the log,
# SHOTS=<folder> keeps the screenshots)
set -u
IMAGE=$(realpath "${1:?usage: test-wayland.sh <image>}")
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$HERE/..
JUSTTOOL=${JUSTTOOL:-$ROOT/tools/target/release/justtool}
DEV=$ROOT/build/wldev
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

if ! command -v gcc >/dev/null 2>&1; then
    echo "  note: gcc missing, skipping the Wayland test"
    exit 0
fi
if [ ! -x "$DEV/usr/bin/wayland-scanner" ]; then
    "$ROOT/tools/linux/debroot.py" --out "$DEV" --cache "$ROOT/build/debcache" --dist trixie \
        libwayland-dev libxkbcommon-dev wayland-protocols >/dev/null || exit 2
fi
LIB=$DEV/usr/lib/x86_64-linux-gnu
PROTO=$DEV/usr/share/wayland-protocols
# wayland-scanner runs here with this machine's libraries, and the tree's
# libxml2 if there is no other (not the tree's glibc: it is not for here).
cd "$WORK"
mkdir scanlib && ln -s "$LIB/libxml2.so.2" scanlib/
scan() { LD_LIBRARY_PATH=$WORK/scanlib "$DEV/usr/bin/wayland-scanner" "$@"; }
scan client-header "$PROTO/stable/xdg-shell/xdg-shell.xml" xdg-shell-client-protocol.h &&
scan private-code "$PROTO/stable/xdg-shell/xdg-shell.xml" xdg-shell-protocol.c &&
scan client-header "$PROTO/unstable/xdg-decoration/xdg-decoration-unstable-v1.xml" xdg-decoration-unstable-v1-client-protocol.h &&
scan private-code "$PROTO/unstable/xdg-decoration/xdg-decoration-unstable-v1.xml" xdg-decoration-protocol.c || exit 2
gcc -std=gnu17 -O2 -I. -I"$DEV/usr/include" -o wt "$HERE/linux/wl-test.c" xdg-shell-protocol.c xdg-decoration-protocol.c \
    -L"$LIB" -Wl,-rpath-link,"$LIB" -lwayland-client -lxkbcommon || exit 2
strip wt

cp --sparse=always "$IMAGE" justos.img || exit 2
"$JUSTTOOL" fs justos.img mkdir /home/wl >/dev/null 2>&1
"$JUSTTOOL" fs justos.img put wt /home/wl/wt >/dev/null || exit 2

# The setup (as in scripts/gui/tour.txt), then the client. Its window is
# green (20b060), with a magenta (c02080) subsurface at 20,20; "find" puts
# the origin at the window's top-left corner.
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
serial cd sys:/home/wl
wait 1
serial ./wt
wait 5
shot window
find 20b060
move @150 @150
wait 0.5
click @150 @150
wait 0.5
right @150 @150
wait 1.5
shot popup
click @40 @150
wait 1
type Ciao
key bracket_left
key ret
wait 0.5
key ctrl-c
wait 0.5
key f11
wait 2.5
shot full
key f11
wait 2
key ctrl-q
wait 3
shot closed
EOF
OUT=${SHOTS:-$WORK/shots}
"$HERE/gui-shots.py" justos.img steps.txt "$OUT" > shots.txt 2>&1
LOG=$OUT/serial.log

cat > expect.txt <<'EOF'
wl: connected
wl: globals compositor=1 sub=1 shm=1 seat=1 output=1 wm=1 deco=1 kde=1 data=1
wl: shm argb=1 xrgb=1
wl: output 1024x768 scale=1 done=1
wl: bounds 1008x644
wl: configured 0x0 decoration=2
wl: keymap ok format=1
window "Prova Wayland" 300x200
wl: frame done 1 released 2
wl: pointer enter window
wl: keyboard enter
wl: button 0x110
wl: button 0x111
wl: popup at 201,100 80x50
wl: popup done
wl: typed Ciaoè
wl: ctrl+c
wl: resized to 1008x716
wl: resized to 300x200
wl: closed by the desktop
wl: bye
EOF
FAILED=0
while IFS= read -r pattern; do
    grep -Fq -- "$pattern" "$LOG" 2>/dev/null || { printf '  missing: %s\n' "$pattern"; FAILED=1; }
done < expect.txt
grep -q "unimplemented syscall\|stopped:\|wayland.*client.*:" "$LOG" 2>/dev/null &&
    grep "unimplemented syscall\|stopped:\|wayland.*client.*:" "$LOG" | sed 's/^/  /' | head -5

# The colours on the screen: the window with its subsurface, then the
# popup too, then neither.
python3 - "$OUT" > colours.txt <<'EOF' || FAILED=1
import struct, sys, zlib
def count(path, rgb):
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
    want, n = bytes.fromhex(rgb), 0
    i = rows.find(want)
    while i >= 0:
        if i % 3 == 0:
            n += 1
        i = rows.find(want, i + 1)
    return n
out, ok = sys.argv[1], True
for shot, green, magenta in (("window", 50000, 2000), ("popup", 50000, 5500), ("full", 600000, 2000), ("closed", -1, -1)):
    try:
        g, m = count(f"{out}/{shot}.png", "20b060"), count(f"{out}/{shot}.png", "c02080")
    except OSError:
        print(f"  missing: the {shot} screenshot"); ok = False; continue
    good = (g < 100 and m < 100) if green < 0 else (g >= green and magenta <= m < magenta + 2000)
    print(f"  {shot}: green {g} magenta {m}" + ("" if good else "  <- wrong"))
    ok &= good
sys.exit(0 if ok else 1)
EOF
grep -q "wrong\|missing" colours.txt && cat colours.txt
[ "${VERBOSE:-0}" = 1 ] && { cat shots.txt; cat colours.txt; cat "$LOG"; }
[ $FAILED = 0 ] && echo "  ok    wayland ($(grep -c . expect.txt) checks, and the colours of 4 screenshots)"
exit $FAILED

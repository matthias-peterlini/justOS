#!/bin/sh
# Boots a justOS image once per display controller QEMU emulates, switches
# the resolution from the shell and checks the screen QEMU shows: it must
# have the new size and show the desktop (not a black or stale picture).
#
# Usage: scripts/test-display.sh <image> [case...]
# Cases: std (QEMU VGA), bochs (bochs-display), qxl, vmware, virtio, cirrus
# (800x600 at most in 32-bit colour), ati (no driver of its own: the generic
# driver keeps the 1024x768 the loader set up).
# std-sleep, vmware-sleep, virtio-sleep and cirrus-sleep put the computer to
# sleep instead (QEMU's firmware leaves the card in text mode on waking) and
# check the picture comes back in the mode it had.
set -u
IMAGE=${1:?usage: test-display.sh <image> [case...]}
shift
HERE=$(dirname "$0")
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
CASES=${*:-std bochs qxl vmware virtio cirrus ati std-sleep vmware-sleep virtio-sleep cirrus-sleep}
DEFAULT_MODE=${MODE:-1280x720}

commands() {
    if [ -n "$SLEEP" ]; then
        cat > "$WORK/cmds.txt" <<EOT
@sleep 4
suspend 3
@sleep 12
display
@sleep 1
shutdown
EOT
    else
        cat > "$WORK/cmds.txt" <<EOT
@sleep 4
display
display $MODE
@sleep 6
display
@sleep 2
shutdown
EOT
    fi
}

# Takes screenshots through QMP into $2, $3... seconds after the start; the
# last one stays. Without a window QEMU draws the screen only for a
# screenshot, and a VGA that never drew before sleep shows QEMU's own
# "not initialized" picture after waking up, whatever the guest did: the
# sleep cases look once before sleeping too, as a real screen would.
cat > "$WORK/shot.py" <<'EOT'
import json, socket, sys, time
start = time.time()
sock, out, delays = sys.argv[1], sys.argv[2], [float(d) for d in sys.argv[3:]]
time.sleep(delays[0])
s = socket.socket(socket.AF_UNIX)
s.connect(sock)
f = s.makefile("rw")
f.readline()
def cmd(c):
    f.write(json.dumps(c) + "\n")
    f.flush()
    while "event" in json.loads(f.readline()):
        pass
cmd({"execute": "qmp_capabilities"})
for d in delays:
    time.sleep(max(0, d - (time.time() - start)))
    cmd({"execute": "screendump", "arguments": {"filename": out}})
EOT

# Width, height and number of distinct colours of a PPM screenshot.
cat > "$WORK/look.py" <<'EOT'
import sys
data = open(sys.argv[1], "rb").read()
parts = data.split(maxsplit=4)
w, h = int(parts[1]), int(parts[2])
px = parts[4]
colours = len({px[i:i + 3] for i in range(0, len(px), 3 * 97)})
print(w, h, colours)
EOT

FAILED=0
for c in $CASES; do
    MODE=$DEFAULT_MODE
    SLEEP=
    DELAY=16
    case $c in
        *-sleep)
            # The mode the system started in: the loader's, or the one the
            # driver chose (cirrus), kept across sleep.
            SLEEP=1 DELAY="6 19" MODE=1024x768
            c=${c%-sleep}
            [ $c = cirrus ] && MODE=800x600 ;;
    esac
    case $c in
        std) ARGS="-vga std" ;;
        bochs) ARGS="-vga none -device bochs-display" ;;
        qxl) ARGS="-vga qxl" ;;
        vmware) ARGS="-vga vmware" ;;
        virtio) ARGS="-vga virtio" ;;
        cirrus) ARGS="-vga cirrus"; MODE=800x600 ;;
        ati) ARGS="-vga none -device ati-vga"; MODE=1024x768 ;;
        *) echo "unknown case: $c" >&2; exit 2 ;;
    esac
    commands
    rm -f "$WORK/shot.ppm"
    python3 "$WORK/shot.py" "$WORK/qmp.sock" "$WORK/shot.ppm" $DELAY &
    SHOT=$!
    "$HERE/test.sh" "$IMAGE" "$WORK/cmds.txt" $ARGS -qmp "unix:$WORK/qmp.sock,server,nowait" > "$WORK/out.txt" 2>&1
    STATUS=$?
    wait $SHOT
    [ -n "$SLEEP" ] && c=$c-sleep
    problems=
    [ $STATUS -ne 0 ] && problems="$problems [kernel stopped]"
    grep -q "^screen $MODE" "$WORK/out.txt" || problems="$problems [no \"screen $MODE\"]"
    if [ -n "$SLEEP" ]; then
        grep -q "awake again" "$WORK/out.txt" || problems="$problems [did not wake]"
    fi
    if [ -f "$WORK/shot.ppm" ]; then
        set -- $(python3 "$WORK/look.py" "$WORK/shot.ppm")
        [ "${1}x${2}" = "$MODE" ] || problems="$problems [screen is ${1}x${2}]"
        [ "$3" -gt 50 ] || problems="$problems [blank screen: $3 colours]"
    else
        problems="$problems [no screenshot]"
    fi
    if [ -z "$problems" ]; then
        printf '  ok    %s\n' "$c"
    else
        printf '  FAIL  %s:%s\n' "$c" "$problems"
        [ "${VERBOSE:-0}" = 1 ] && cat "$WORK/out.txt"
        FAILED=1
    fi
    [ -n "${SHOTS:-}" ] && cp "$WORK/shot.ppm" "$SHOTS/$c.ppm" 2>/dev/null
done
exit $FAILED

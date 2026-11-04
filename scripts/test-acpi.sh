#!/bin/sh
# Boots an arcOS image on a classic PC (PIIX4) and on a modern one (Q35),
# presses the power button through QMP and checks that arcOS turns itself
# off cleanly. Then boots a "laptop": Q35 with the battery, adapter, lid
# and thermal zone of scripts/acpi/laptop.aml, checks that arcOS reads
# them through the firmware's AML, empties the battery and checks that
# arcOS turns itself off.
#
# Usage: scripts/test-acpi.sh <image> [case...]   (cases: pc, q35, laptop)
set -u
IMAGE=${1:?usage: test-acpi.sh <image> [case...]}
shift
HERE=$(dirname "$0")
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
CASES=${*:-pc q35 laptop}

printf '@sleep 3\ndev\n@sleep 8\necho still-on\n' > "$WORK/cmds.txt"

# The battery drops to 2% (1000 of 50000 mWh) with the adapter unplugged.
cat > "$WORK/laptop.txt" <<'EOT'
@sleep 3
acpi
sysinfo
acpi eval \_SB.BAT0.SETC 1000 0
@sleep 8
echo still-on
EOT

# Presses the power button through QMP after $2 seconds.
cat > "$WORK/press.py" <<'EOT'
import json, socket, sys, time
time.sleep(float(sys.argv[2]))
s = socket.socket(socket.AF_UNIX)
s.connect(sys.argv[1])
f = s.makefile("rw")
f.readline()
for cmd in ({"execute": "qmp_capabilities"}, {"execute": "system_powerdown"}):
    f.write(json.dumps(cmd) + "\n")
    f.flush()
    while "event" in json.loads(f.readline()):
        pass
EOT

FAILED=0
for c in $CASES; do
    problems=
    case $c in
        pc | q35)
            ARGS=
            [ $c = q35 ] && ARGS="-machine q35"
            python3 "$WORK/press.py" "$WORK/qmp.sock" 9 &
            PRESS=$!
            DISK=$([ $c = q35 ] && echo ahci || echo ide) "$HERE/test.sh" "$IMAGE" "$WORK/cmds.txt" $ARGS \
                -qmp "unix:$WORK/qmp.sock,server,nowait" > "$WORK/out.txt" 2>&1
            STATUS=$?
            wait $PRESS
            [ $STATUS -ne 0 ] && problems="$problems [kernel stopped]"
            grep -q "ACPI events" "$WORK/out.txt" || problems="$problems [no ACPI events device]"
            grep -q "power button pressed" "$WORK/out.txt" || problems="$problems [the press was not seen]"
            ;;
        laptop)
            DISK=ahci "$HERE/test.sh" "$IMAGE" "$WORK/laptop.txt" -machine q35 \
                -acpitable file="$HERE/acpi/laptop.aml" > "$WORK/out.txt" 2>&1
            [ $? -ne 0 ] && problems="$problems [kernel stopped]"
            for want in "AML ready" "on mains" "80%, charging" "Arc Cells ARC57 LION" "now 40000 of 50000 mWh (design 57000)" \
                "health 87%, 123 cycles" "lid.* open" "TZ01 40.0 °C.*critical 100 °C" "battery *80% (on mains)" \
                "battery empty: turning off"; do
                grep -q "$want" "$WORK/out.txt" || problems="$problems [missing: $want]"
            done
            ;;
        *) echo "unknown case: $c" >&2; exit 2 ;;
    esac
    # The last command comes after the machine should have turned off.
    grep -q "^still-on" "$WORK/out.txt" && problems="$problems [the machine did not turn off]"
    if [ -z "$problems" ]; then
        printf '  ok    %s\n' "$c"
    else
        printf '  FAIL  %s:%s\n' "$c" "$problems"
        [ "${VERBOSE:-0}" = 1 ] && cat "$WORK/out.txt"
        FAILED=1
    fi
done
exit $FAILED

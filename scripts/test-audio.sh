#!/bin/sh
# Boots an arcOS image once per sound card QEMU emulates, plays a 440 Hz
# tone and a WAV file from the shell and checks what QEMU recorded: the
# tone must be there, at the right pitch and for its whole length.
#
# Usage: scripts/test-audio.sh <image> [case...]
# Cases: hda (ICH9 + line out), hda-duplex (ICH6), hda-speaker, ac97.
# SLEEP=1 puts the machine to sleep (S3) for a few seconds first: the
# drivers must set the card up again on waking.
set -u
IMAGE=${1:?usage: test-audio.sh <image> [case...]}
shift
HERE=$(dirname "$0")
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
CASES=${*:-hda hda-duplex hda-speaker ac97}

SLEEP_CMDS=
[ "${SLEEP:-0}" = 1 ] && SLEEP_CMDS='suspend 3
@sleep 9'
cat > "$WORK/cmds.txt" <<EOT
@sleep 3
$SLEEP_CMDS
display
tone 440 2000
@sleep 3
play sys:/system/sounds/avvio.wav
@sleep 3
shutdown
EOT

# Pitch of the first 1.9 s of sound, and the length of all of it. QEMU
# records only while the card plays, so the tone (2 s) and the WAV file
# (1.9 s) come one right after the other.
cat > "$WORK/look.py" <<'EOT'
import struct, sys
d = open(sys.argv[1], "rb").read()[44:]
s = struct.unpack("<%dh" % (len(d) // 2), d[:len(d) // 2 * 2])[::2]
rate = 44100
loud = [i for i, v in enumerate(s) if abs(v) > 300]
if not loud:
    print("0 0")
    sys.exit()
a = loud[0]
seg = s[a:a + rate * 19 // 10]
crossings = sum(1 for i in range(1, len(seg)) if seg[i - 1] < 0 <= seg[i])
print(round((loud[-1] - a) / rate * 1000), round(crossings / (len(seg) / rate)))
EOT

FAILED=0
for c in $CASES; do
    case $c in
        hda) ARGS="-device ich9-intel-hda -device hda-output,audiodev=snd" ;;
        hda-duplex) ARGS="-device intel-hda -device hda-duplex,audiodev=snd" ;;
        hda-speaker) ARGS="-device ich9-intel-hda -device hda-micro,audiodev=snd" ;;
        ac97) ARGS="-device AC97,audiodev=snd" ;;
        *) echo "unknown case: $c" >&2; exit 2 ;;
    esac
    rm -f "$WORK/out.wav"
    "$HERE/test.sh" "$IMAGE" "$WORK/cmds.txt" -audiodev "wav,id=snd,path=$WORK/out.wav" $ARGS > "$WORK/out.txt" 2>&1
    STATUS=$?
    problems=
    [ $STATUS -ne 0 ] && problems="$problems [kernel stopped]"
    grep -q "^sound: audio0" "$WORK/out.txt" || problems="$problems [no sound output]"
    grep -q "^playing sys:/system/sounds/avvio.wav" "$WORK/out.txt" || problems="$problems [the WAV file did not play]"
    if [ "${SLEEP:-0}" = 1 ]; then
        grep -q "awake again after" "$WORK/out.txt" || problems="$problems [did not wake]"
    fi
    if [ -f "$WORK/out.wav" ]; then
        set -- $(python3 "$WORK/look.py" "$WORK/out.wav")
        { [ "$2" -ge 435 ] && [ "$2" -le 445 ]; } || problems="$problems [tone at $2 Hz]"
        { [ "$1" -ge 3500 ] && [ "$1" -le 4200 ]; } || problems="$problems [$1 ms of sound, not about 3.9 s]"
    else
        problems="$problems [nothing recorded]"
    fi
    if [ -z "$problems" ]; then
        printf '  ok    %s\n' "$c"
    else
        printf '  FAIL  %s:%s\n' "$c" "$problems"
        [ "${VERBOSE:-0}" = 1 ] && cat "$WORK/out.txt"
        FAILED=1
    fi
done
exit $FAILED

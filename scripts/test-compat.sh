#!/bin/sh
# Builds a small Linux program and three Windows programs (scripts/compat:
# console, files and keyboard, and one with a window), puts them on a copy of
# the image together with the store programs and two Java programs (a DEX
# file and an Android app, already built: system/libs/justdex/tests), boots
# justOS and checks that they run: the Linux ABI, the Windows loader (winrun),
# the Dalvik interpreter, `install` and the BusyBox applets.
#
# Needs musl-gcc (musl-tools) and x86_64-w64-mingw32-gcc (mingw-w64). If either
# is missing, that half is skipped (and noted), so the script still checks what
# it can.
#
# Usage: scripts/test-compat.sh <image>
# Prints the lines missing from the session and exits non-zero if any;
# VERBOSE=1 prints the whole session.
set -u
IMAGE=${1:?usage: test-compat.sh <image>}
HERE=$(dirname "$0")
JUSTTOOL=${JUSTTOOL:-$HERE/../tools/target/release/justtool}
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
cp --sparse=always "$IMAGE" "$WORK/justos.img" || exit 2
"$JUSTTOOL" fs "$WORK/justos.img" mkdir /home/compat >/dev/null 2>&1

have_linux=0
have_windows=0

if command -v musl-gcc >/dev/null 2>&1; then
    cat > "$WORK/hello.c" <<'EOF'
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
int main(int argc, char **argv) {
    printf("linux: argc=%d\n", argc);
    for (int i = 0; i < argc; i++) printf("linux: argv[%d]=%s\n", i, argv[i]);
    char *m = malloc(32);
    strcpy(m, "linux: heap ok");
    puts(m);
    free(m);
    printf("linux: float=%.3f\n", 3.14159);
    FILE *f = fopen("out.txt", "w");
    fputs("written by a linux program\n", f);
    fclose(f);
    f = fopen("out.txt", "r");
    char buf[64];
    if (fgets(buf, sizeof buf, f)) printf("linux: read back: %s", buf);
    fclose(f);
    return 0;
}
EOF
    if musl-gcc -static -Os -o "$WORK/lhello" "$WORK/hello.c" 2>/dev/null; then
        strip "$WORK/lhello" 2>/dev/null
        "$JUSTTOOL" fs "$WORK/justos.img" put "$WORK/lhello" /home/compat/lhello >/dev/null && have_linux=1
        # The same program in a Debian package, for `install <file>.deb`.
        mkdir -p "$WORK/deb/usr/bin" && cp "$WORK/lhello" "$WORK/deb/usr/bin/debhello"
        printf '2.0\n' > "$WORK/debian-binary"
        tar -czf "$WORK/control.tar.gz" -C "$WORK" debian-binary
        tar -czf "$WORK/data.tar.gz" -C "$WORK/deb" ./usr
        (cd "$WORK" && ar rc debhello_1.0_amd64.deb debian-binary control.tar.gz data.tar.gz) &&
            "$JUSTTOOL" fs "$WORK/justos.img" put "$WORK/debhello_1.0_amd64.deb" /home/compat/debhello_1.0_amd64.deb >/dev/null
    fi
fi
[ $have_linux = 0 ] && echo "  note: musl-gcc not available, skipping the Linux program"

if command -v x86_64-w64-mingw32-gcc >/dev/null 2>&1; then
    cat > "$WORK/whello.c" <<'EOF'
#include <stdio.h>
#include <stdlib.h>
int main(int argc, char **argv) {
    printf("windows: argc=%d\n", argc);
    for (int i = 0; i < argc; i++) printf("windows: argv[%d]=%s\n", i, argv[i]);
    int *m = malloc(4 * sizeof(int));
    for (int i = 0; i < 4; i++) m[i] = i * i;
    printf("windows: squares %d %d %d %d\n", m[0], m[1], m[2], m[3]);
    free(m);
    printf("windows: float=%.3f\n", 2.71828);
    return 0;
}
EOF
    if x86_64-w64-mingw32-gcc -O2 -o "$WORK/whello.exe" "$WORK/whello.c" 2>/dev/null &&
        x86_64-w64-mingw32-gcc -O2 -o "$WORK/wtest.exe" "$HERE/compat/wtest.c" 2>/dev/null &&
        x86_64-w64-mingw32-gcc -O2 -mwindows -o "$WORK/wgui.exe" "$HERE/compat/wgui.c" 2>/dev/null; then
        for p in whello wtest wgui; do
            "$JUSTTOOL" fs "$WORK/justos.img" put "$WORK/$p.exe" /home/compat/$p.exe >/dev/null || break
        done && have_windows=1
    fi
fi
[ $have_windows = 0 ] && echo "  note: x86_64-w64-mingw32-gcc not available, skipping the Windows program"

cat > "$WORK/t.lua" <<'EOF'
print("lua: " .. _VERSION)
local s = 0
for i = 1, 100 do s = s + i end
print("lua: sum 1..100 = " .. s)
print(string.format("lua: float=%.3f", 1.0 / 3.0))
EOF
"$JUSTTOOL" fs "$WORK/justos.img" put "$WORK/t.lua" /home/compat/t.lua >/dev/null
DEX=$HERE/../system/libs/justdex/tests
"$JUSTTOOL" fs "$WORK/justos.img" put "$DEX/Basics.dex" /home/compat/Basics.dex >/dev/null
"$JUSTTOOL" fs "$WORK/justos.img" put "$DEX/SelfTest.apk" /home/compat/SelfTest.apk >/dev/null

# Build the session depending on what is available.
{
    echo '@sleep 1'
    echo 'cd sys:/home/compat'
    echo 'install busybox'
    echo 'install lua'
    echo 'echo busybox-echo-works'
    echo 'seq 1 3'
    echo 'grep linux t.lua'
    [ $have_linux = 1 ] && { echo './lhello one two'; echo 'install debhello_1.0_amd64.deb'; echo 'debhello from-deb'; }
    # wtest reads a number from the keyboard: the line after it.
    [ $have_windows = 1 ] && { echo 'whello.exe alpha beta'; echo 'wtest.exe'; echo '21'; echo 'wgui.exe'; echo '@sleep 2'; }
    echo 'lua t.lua'
    echo 'Basics.dex'
    echo '@sleep 2'
    echo 'SelfTest.apk'
    echo '@sleep 2'
    echo 'shutdown'
} > "$WORK/session.txt"

{
    echo 'busybox-echo-works'
    echo 'lua: sum 1..100 = 5050'
    echo 'lua: float=0.333'
    echo 'Ciao da Java! args=0'
    echo '0.30000000000000004'
    echo '[cerchio(3.14), quadrato(4.00), rettangolo(6.00)] made=3'
    echo '{la=3, neve=2, roccia=1} la 2 false'
    echo '30 63 -1360544799 false 0.30871945533265976'
    echo 'I/selftest: window open'
    echo 'I/selftest: label clic 2'
    echo 'I/selftest: destroyed after 2 clicks'
    [ $have_linux = 1 ] && { echo 'linux: heap ok'; echo 'linux: float=3.142'; echo 'linux: read back: written by a linux program'; echo 'linux: argv[1]=from-deb'; }
    [ $have_windows = 1 ] && cat <<'EOF'
windows: squares 0 1 4 9
windows: float=2.718
wtest: first line 'line one'
wtest: scanned 42 word 3.50
wtest: size 22
wtest: sorted 1 3 5 7 9
wtest: HELLO, JUSTOS 13
wtest: sqrt2=1.4142 sin1=0.841 pow=1024 neg=-17
wtest: strtol 31 atoi -250
wtest: 00042|ab  |ff
wtest: sscanf 46
wtest: win32 read 10 'win32 file' size 10
wtest: missing file error 2
wtest: you typed 21, doubled 42
wgui: create
wgui: user message 42
wgui: painted 1 pixel 0000ff
wgui: quit 7
EOF
} > "$WORK/expect.txt"

STEP_WAIT=${STEP_WAIT:-2} "$HERE/test.sh" "$WORK/justos.img" "$WORK/session.txt" > "$WORK/out.txt" 2>&1
STATUS=$?
FAILED=0
while IFS= read -r pattern; do
    [ -z "$pattern" ] && continue
    if ! grep -Fq -- "$pattern" "$WORK/out.txt"; then
        printf '  missing: %s\n' "$pattern"
        FAILED=1
    fi
done < "$WORK/expect.txt"
[ $STATUS -ne 0 ] && { echo "  the kernel stopped"; FAILED=1; }
[ "${VERBOSE:-0}" = 1 ] && cat "$WORK/out.txt"
if [ $FAILED = 0 ]; then
    echo "  ok    compat ($(grep -c . "$WORK/expect.txt") checks, linux=$have_linux windows=$have_windows)"
fi
exit $FAILED

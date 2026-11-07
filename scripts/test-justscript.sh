#!/bin/sh
# Runs JustScript on justOS: the examples in sys:/home/justscript, the notes
# program, a project built into a .jpk package, and the test programs of
# scripts/justscript (copied into the image first), with the compiler
# (justc) and the runner (just), checking their output and the package's
# "JPK\0" header.
#
# Usage: scripts/test-justscript.sh <image>
# Prints the lines missing from the session and exits non-zero if any; VERBOSE=1 prints the
# whole session.
set -u
IMAGE=${1:?usage: test-justscript.sh <image>}
HERE=$(dirname "$0")
JUSTTOOL=${JUSTTOOL:-$HERE/../tools/target/release/justtool}
# The runtime of standalone programs, built for this machine.
STUB=${STUB:-$HERE/../tools/target/release/just-stub}
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

cp --sparse=always "$IMAGE" "$WORK/justos.img" || exit 2
"$JUSTTOOL" fs "$WORK/justos.img" mkdir /home/tests >/dev/null || exit 2
for f in "$HERE"/justscript/*.jt; do
    "$JUSTTOOL" fs "$WORK/justos.img" put "$f" "/home/tests/$(basename "$f")" >/dev/null || exit 2
done

cat > "$WORK/session.txt" <<'EOT'
@sleep 1
cd sys:/home/justscript
hello.jt
notes add Buy milk #home
notes add "Write the docs" #work
notes done 1
notes list
justc run library
justc build library -r
justc info library/build/release/library.jpk
library/build/release/library.jpk
just --info library/build/release/library.jpk
justc build library -r -t windows
justc info library/build/release/library.exe
justc build library -t mac
justc todo.jt
justc new sys:/home/demo
justc check sys:/home/demo
cd sys:/home/tests
just args.jt a "b c"
just
just --version
justc math.jt
@sleep 2
justc files.jt one "two words"
@sleep 2
errors.jt
typo.jt
justc run input.jt
Ada
justc disasm sys:/home/justscript/hello.jt
justc nothing.jt
shutdown
EOT

# What the session has to show, one line of text each.
cat > "$WORK/expect.txt" <<'EOT'
fib(25) = 75025
sum of odd squares = 84
added #2: Write the docs
done: Buy milk
[x]   1  Buy milk #home
summary: {"available": 1, "lent": 1, "lent (long)": 1, "lost": 1}
Finished library/build/release/library.jpk
Finished library/build/release/library.exe
target     windows
unknown target `mac`
compiler   justc 0.5.0 (justOS)
target     jpk
args: ["a", "b c"]
just - runs JustScript programs on justOS
just ended with code 2
just 0.5.0 (justOS)
== Todo — 1 open ==
[x] Design JustVM
Created project `demo`
Checked src/main.jt - no errors, 0 warning(s)
math: 26 of 26 agree
0.3333333333333333 0.30000000000000004 10.0 3
Ok(nil) Ok(nil) Ok(nil)
Ok("one\ntwo\n") Ok(["notes.txt"]) true true true false
Ok(nil) Ok(nil) Ok(["copy.txt", "moved.txt"])
true true
Ok(nil) false Ok(nil)
a.tar.gz sys:/home ram:/y
Hello from user mode!
justos ["one", "two words"] true 0
justc ended with code 3
runtime error: index 5 out of bounds (length 2)
    at inner (line 3)
    at outer (line 6)
errors.jt ended with code 70
error: cannot find value `cuont` in this scope
typo.jt ended with code 1
name? Ada
hello, Ada
GetGlobal
nothing.jt: not found
EOT

KEEP_DISK="$WORK/after.img" STEP_WAIT=${STEP_WAIT:-2} "$HERE/test.sh" "$WORK/justos.img" "$WORK/session.txt" > "$WORK/out.txt" 2>&1
STATUS=$?
FAILED=0
while IFS= read -r pattern; do
    if ! grep -Fq -- "$pattern" "$WORK/out.txt"; then
        printf '  missing: %s\n' "$pattern"
        FAILED=1
    fi
done < "$WORK/expect.txt"
if grep -q "math: .* expected" "$WORK/out.txt"; then
    grep "math: .* expected" "$WORK/out.txt"
    FAILED=1
fi
[ $STATUS -ne 0 ] && { echo "  the kernel stopped"; FAILED=1; }
# The package justc built, read back from the disk: "JPK\0" first.
"$JUSTTOOL" fs "$WORK/after.img" get /home/justscript/library/build/release/library.jpk "$WORK/library.jpk" >/dev/null 2>&1
MAGIC=$(od -An -tx1 -N4 "$WORK/library.jpk" 2>/dev/null | tr -d ' \n')
if [ "$MAGIC" != 4a504b00 ]; then
    echo "  library.jpk starts with '${MAGIC:-nothing}', not 4a504b00 (JPK\\0)"
    FAILED=1
fi
# The Windows program: the runtime, then the package and its trailer.
"$JUSTTOOL" fs "$WORK/after.img" get /home/justscript/library/build/release/library.exe "$WORK/library.exe" >/dev/null 2>&1
HEAD=$(od -An -tx1 -N2 "$WORK/library.exe" 2>/dev/null | tr -d ' \n')
TAIL=$(tail -c 8 "$WORK/library.exe" 2>/dev/null | od -An -tx1 | tr -d ' \n')
if [ "$HEAD" != 4d5a ] || [ "$TAIL" != 4a504b424e444c00 ]; then
    echo "  library.exe is not a Windows program with a package (MZ ... JPKBNDL\\0)"
    FAILED=1
fi
# The package inside it runs with the runtime built for this machine.
if ! "$STUB" "$WORK/library.exe" 2>&1 | grep -Fq 'summary: {"available": 1'; then
    echo "  the package in library.exe does not run"
    FAILED=1
fi
[ "${VERBOSE:-0}" = 1 ] && cat "$WORK/out.txt"
if [ $FAILED = 0 ]; then
    echo "  ok    justscript ($(wc -l < "$WORK/expect.txt") checks)"
fi
exit $FAILED

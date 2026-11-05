#!/bin/sh
# Runs ArcScript on JustOS: the examples in sys:/home/arcscript, the notes
# program, a project built into a .arc package, and the test programs of
# scripts/arcscript (copied into the image first), checking their output.
#
# Usage: scripts/test-arcscript.sh <image>
# Prints the lines missing from the session and exits non-zero if any; VERBOSE=1 prints the
# whole session.
set -u
IMAGE=${1:?usage: test-arcscript.sh <image>}
HERE=$(dirname "$0")
JUSTTOOL=${JUSTTOOL:-$HERE/../tools/target/release/justtool}
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

cp --sparse=always "$IMAGE" "$WORK/justos.img" || exit 2
"$JUSTTOOL" fs "$WORK/justos.img" mkdir /home/tests >/dev/null || exit 2
for f in "$HERE"/arcscript/*.as; do
    "$JUSTTOOL" fs "$WORK/justos.img" put "$f" "/home/tests/$(basename "$f")" >/dev/null || exit 2
done

cat > "$WORK/session.txt" <<'EOT'
@sleep 1
cd sys:/home/arcscript
hello.as
notes add Buy milk #home
notes add "Write the docs" #work
notes done 1
notes list
arcc run library
arcc build library -r
arcc info library/build/release/library.arc
library/build/release/library.arc
arcc todo.as
arcc new sys:/home/demo
arcc check sys:/home/demo
cd sys:/home/tests
arcc math.as
@sleep 2
arcc files.as one "two words"
@sleep 2
errors.as
typo.as
arcc run input.as
Ada
arcc disasm sys:/home/arcscript/hello.as
arcc nothing.as
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
Finished library/build/release/library.arc
compiler   arcc 0.5.0 (JustOS)
== Todo — 1 open ==
[x] Design ArcVM
Created project `demo`
Checked src/main.as - no errors, 0 warning(s)
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
arcc ended with code 3
runtime error: index 5 out of bounds (length 2)
    at inner (line 3)
    at outer (line 6)
errors.as ended with code 70
error: cannot find value `cuont` in this scope
typo.as ended with code 1
name? Ada
hello, Ada
GetGlobal
nothing.as: not found
EOT

STEP_WAIT=${STEP_WAIT:-2} "$HERE/test.sh" "$WORK/justos.img" "$WORK/session.txt" > "$WORK/out.txt" 2>&1
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
[ "${VERBOSE:-0}" = 1 ] && cat "$WORK/out.txt"
if [ $FAILED = 0 ]; then
    echo "  ok    arcscript ($(wc -l < "$WORK/expect.txt") checks)"
fi
exit $FAILED

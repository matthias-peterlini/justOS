#!/bin/sh
# Checks the Linux ABI with programs that use threads, futexes, pipes, poll,
# memory maps and sockets: one in C (musl), one in Rust and one in Go, all
# static, and one in C linked dynamically against glibc (the image carries
# Debian's glibc, see tools/linux/debroot.py), built here from
# scripts/linux/. Each is skipped (and noted) when its compiler is missing. A web server on this machine answers their
# requests at 10.0.2.2.
#
# EXTRA=<folder> also copies the programs in it (for example ripgrep, fd,
# bat built for x86_64-unknown-linux-musl) and runs `<name> --version`.
#
# Usage: scripts/test-linux.sh <image>    (VERBOSE=1 prints the session)
set -u
IMAGE=$(realpath "${1:?usage: test-linux.sh <image>}")
HERE=$(cd "$(dirname "$0")" && pwd)
JUSTTOOL=${JUSTTOOL:-$HERE/../tools/target/release/justtool}
WORK=$(mktemp -d)
PID=
cleanup() { [ -n "$PID" ] && kill "$PID" 2>/dev/null; rm -rf "$WORK"; }
trap cleanup EXIT
cp --sparse=always "$IMAGE" "$WORK/justos.img" || exit 2
cd "$WORK"
"$JUSTTOOL" fs justos.img mkdir /home/lt >/dev/null 2>&1
put() { "$JUSTTOOL" fs justos.img put "$1" "/home/lt/$2" >/dev/null; }

PORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')
mkdir www && echo "hello from the host" > www/hello.txt
(cd www && exec python3 -m http.server "$PORT" --bind 127.0.0.1 >/dev/null 2>&1) & PID=$!

C=0; R=0; G=0; D=0
if command -v musl-gcc >/dev/null 2>&1 && musl-gcc -static -O2 -o ct "$HERE/linux/threads-net.c" -lpthread 2>/dev/null; then
    strip ct; put ct ct && C=1
fi
if command -v gcc >/dev/null 2>&1 && gcc -O2 -o dt "$HERE/linux/dyn-test.c" 2>/dev/null; then
    strip dt; put dt dt && D=1
fi
if command -v cargo >/dev/null 2>&1 && rustup target list --installed 2>/dev/null | grep -q x86_64-unknown-linux-musl; then
    mkdir -p rt/src && cp "$HERE/linux/rust-test.rs" rt/src/main.rs
    printf '[package]\nname = "rt"\nversion = "0.1.0"\nedition = "2021"\n[profile.release]\nstrip = true\n' > rt/Cargo.toml
    (cd rt && cargo build --release --target x86_64-unknown-linux-musl -q 2>/dev/null) && put rt/target/x86_64-unknown-linux-musl/release/rt rtest && R=1
fi
if command -v go >/dev/null 2>&1; then
    mkdir -p gt && cp "$HERE/linux/go-test.go" gt/main.go && printf 'module gt\ngo 1.21\n' > gt/go.mod
    (cd gt && CGO_ENABLED=0 go build -ldflags="-s -w" -o ../gtest . 2>/dev/null) && put gtest gtest && G=1
fi
[ $C = 0 ] && echo "  note: musl-gcc missing, skipping the C program"
[ $R = 0 ] && echo "  note: Rust with x86_64-unknown-linux-musl missing, skipping the Rust program"
[ $G = 0 ] && echo "  note: Go missing, skipping the Go program"
[ $D = 0 ] && echo "  note: gcc missing, skipping the dynamically linked program"

{
    echo '@sleep 2'
    echo 'cd sys:/home/lt'
    [ $C = 1 ] && { echo "./ct 10.0.2.2 $PORT"; echo '@sleep 3'; }
    [ $R = 1 ] && { echo "./rtest 10.0.2.2:$PORT"; echo '@sleep 4'; }
    [ $G = 1 ] && { echo "./gtest http://10.0.2.2:$PORT/hello.txt"; echo '@sleep 8'; }
    [ $D = 1 ] && { echo "./dt"; echo '@sleep 6'; }
    if [ -n "${EXTRA:-}" ]; then
        for f in "$EXTRA"/*; do
            n=$(basename "$f"); put "$f" "$n"; echo "./$n --version"; echo '@sleep 3'
        done
    fi
    echo 'shutdown'
} > session.txt

{
    [ $C = 1 ] && printf '%s\n' 'ct: threads counter=400000' 'ct: poll=1 revents=1 data=pipe-ok' 'ct: mmap=mapped file contents' 'ct: big reservation ok 3' 'ct: http body=hello from the host' 'ct: getaddrinfo localhost=0'
    [ $R = 1 ] && printf '%s\n' 'rt: threads total=4999999' 'rt: file rust file' 'rt: http hello from the host' 'rt: slept true'
    [ $G = 1 ] && printf '%s\n' 'gt: goroutines sum 3599982' 'gt: slept true' 'gt: http 200 hello from the host'
    [ $D = 1 ] && printf '%s\n' 'dt: glibc 2.' 'dt: threads 400000' 'dt: dlopen cos(0)=1.0' 'dt: argv0 dt' \
        'dt: fork pipe=hello exit=7 global=1' 'dt: child says vforked home=/home' 'dt: vfork exit=3' \
        'dt: child says spawned home=/home' 'dt: spawn err=0 exit=3' 'dt: spawn missing=2' \
        'dt: shebang ./dt-script' 'dt: script err=0 exit=4' 'dt: no children -1 errno=10' \
        'dt: signal usr1=1' 'dt: siginfo pid=1' 'dt: blocked 0 pending 1 after 1' 'dt: sigtimedwait 12 handler 1' \
        'dt: segv caught 1 addr=0x10' 'dt: signal in a loop 1' 'dt: read interrupted 1' 'dt: read restarted R' \
        'dt: sigchld 1 suspend 1 exit=5' 'dt: killed by 15, abort 6'
} > expect.txt

STEP_WAIT=${STEP_WAIT:-2} "$HERE/test.sh" justos.img session.txt > out.txt 2>&1
STATUS=$?
FAILED=0
while IFS= read -r pattern; do
    [ -z "$pattern" ] && continue
    grep -Fq -- "$pattern" out.txt || { printf '  missing: %s\n' "$pattern"; FAILED=1; }
done < expect.txt
grep -q "unimplemented syscall\|stopped:" out.txt && grep "unimplemented syscall\|stopped:" out.txt | sed 's/^/  /' | head -5
[ $STATUS -ne 0 ] && { echo "  the kernel stopped"; FAILED=1; }
[ "${VERBOSE:-0}" = 1 ] && cat out.txt
[ $FAILED = 0 ] && echo "  ok    linux ($(grep -c . expect.txt) checks, c=$C rust=$R go=$G glibc=$D)"
exit $FAILED

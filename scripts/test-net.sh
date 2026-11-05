#!/bin/sh
# Checks TCP, HTTP and HTTPS. Servers run on this machine; JustOS reaches
# them at 10.0.2.2 through QEMU's user network:
#
#   - an HTTP server with a 3 MiB file (TCP windows, many segments), checked
#     with sha256 on JustOS
#   - an HTTPS server speaking TLS 1.3 and one limited to TLS 1.2, with a
#     test authority that is added to the image (sys:/system/certs)
#   - a server whose certificate names another host (must be refused)
#
# If an HTTPS proxy is configured (HTTPS_PROXY, with its CA in
# /root/.ccr/ca-bundle.crt) the session also fetches from api.github.com
# through it.
#
# Usage: scripts/test-net.sh <image>    (VERBOSE=1 prints the session)
set -u
IMAGE=$(realpath "${1:?usage: test-net.sh <image>}")
HERE=$(cd "$(dirname "$0")" && pwd)
JUSTTOOL=${JUSTTOOL:-$HERE/../tools/target/release/justtool}
WORK=$(mktemp -d)
PIDS=
cleanup() { for p in $PIDS; do kill "$p" 2>/dev/null; done; rm -rf "$WORK"; }
trap cleanup EXIT
cp --sparse=always "$IMAGE" "$WORK/justos.img" || exit 2
cd "$WORK"

# A test authority and a certificate for 10.0.2.2.
openssl ecparam -name prime256v1 -genkey -noout -out ca.key 2>/dev/null
openssl req -x509 -new -key ca.key -days 30 -subj "/CN=JustOS Test CA" -addext "basicConstraints=critical,CA:TRUE" -out ca.pem 2>/dev/null
openssl x509 -in ca.pem -outform der -out ca.der
openssl ecparam -name prime256v1 -genkey -noout -out srv.key 2>/dev/null
openssl req -new -key srv.key -subj "/CN=10.0.2.2" -out srv.csr 2>/dev/null
printf "subjectAltName=IP:10.0.2.2\n" > srv.ext
openssl x509 -req -in srv.csr -CA ca.pem -CAkey ca.key -CAcreateserial -days 30 -extfile srv.ext -out srv.pem 2>/dev/null
openssl req -new -key srv.key -subj "/CN=other" -out other.csr 2>/dev/null
printf "subjectAltName=DNS:other.example\n" > other.ext
openssl x509 -req -in other.csr -CA ca.pem -CAkey ca.key -CAcreateserial -days 30 -extfile other.ext -out other.pem 2>/dev/null

port() { python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])'; }
P_HTTP=$(port); P_TLS13=$(port); P_TLS12=$(port); P_BAD=$(port)

mkdir www
head -c 3145728 /dev/urandom > www/big.bin
SUM=$(sha256sum www/big.bin | cut -d' ' -f1)
echo "hello from the host" > www/hello.txt
(cd www && exec python3 -m http.server "$P_HTTP" --bind 127.0.0.1 >/dev/null 2>&1) & PIDS="$PIDS $!"
openssl s_server -accept "$P_TLS13" -www -quiet -cert srv.pem -key srv.key >/dev/null 2>&1 & PIDS="$PIDS $!"
openssl s_server -accept "$P_TLS12" -www -quiet -tls1_2 -cert srv.pem -key srv.key >/dev/null 2>&1 & PIDS="$PIDS $!"
openssl s_server -accept "$P_BAD" -www -quiet -cert other.pem -key srv.key >/dev/null 2>&1 & PIDS="$PIDS $!"
sleep 1

"$JUSTTOOL" fs justos.img mkdir /system/certs >/dev/null 2>&1
"$JUSTTOOL" fs justos.img put ca.der /system/certs/test-ca.der >/dev/null || exit 2

WEB=0
PROXY=$(echo "${HTTPS_PROXY:-}" | sed -E 's#^https?://##; s#.*@##; s#/$##')
if [ -n "$PROXY" ] && [ -f /root/.ccr/ca-bundle.crt ]; then
    # The proxy listens on this machine; JustOS reaches it at 10.0.2.2.
    PPORT=${PROXY##*:}
    echo "10.0.2.2:$PPORT" > proxy
    # The bundle may hold several authorities: each becomes a file.
    awk '/BEGIN CERT/{n++} {print > ("proxy-ca-" n ".pem")}' /root/.ccr/ca-bundle.crt
    for f in proxy-ca-*.pem; do openssl x509 -in "$f" -outform der -out "${f%.pem}.der" 2>/dev/null; done
    ls proxy-ca-*.der >/dev/null 2>&1 && WEB=1
fi

{
    echo '@sleep 2'
    echo "fetch http://10.0.2.2:$P_HTTP/hello.txt"
    echo "fetch http://10.0.2.2:$P_HTTP/big.bin sys:/home/big.bin"
    echo '@sleep 8'
    echo 'sha256 sys:/home/big.bin'
    echo "fetch https://10.0.2.2:$P_TLS13/"
    echo '@sleep 2'
    echo "fetch https://10.0.2.2:$P_TLS12/"
    echo '@sleep 2'
    echo "fetch https://10.0.2.2:$P_BAD/"
    echo '@sleep 2'
    echo "fetch http://10.0.2.2:1/"
    echo '@sleep 2'
    echo 'shutdown'
} > session.txt

{
    echo 'hello from the host'
    echo "$SUM  sys:/home/big.bin"
    echo "200 https://10.0.2.2:$P_TLS13/"
    echo "200 https://10.0.2.2:$P_TLS12/"
    echo 'the certificate is for another site'
    echo 'connection refused'
} > expect.txt

STEP_WAIT=${STEP_WAIT:-2} "$HERE/test.sh" justos.img session.txt > out.txt 2>&1
STATUS=$?

if [ $WEB = 1 ]; then
    cp --sparse=always "$IMAGE" web.img
    "$JUSTTOOL" fs web.img mkdir /system/certs >/dev/null 2>&1
    for f in proxy-ca-*.der; do "$JUSTTOOL" fs web.img put "$f" "/system/certs/$f" >/dev/null; done
    "$JUSTTOOL" fs web.img put proxy /system/proxy >/dev/null
    printf '@sleep 2\nfetch https://api.github.com/repos/matthias-peterlini/justOS\n@sleep 6\nshutdown\n' > web.txt
    STEP_WAIT=2 "$HERE/test.sh" web.img web.txt >> out.txt 2>&1
    echo '200 https://api.github.com/repos/matthias-peterlini/justOS' >> expect.txt
fi

FAILED=0
while IFS= read -r pattern; do
    [ -z "$pattern" ] && continue
    if ! grep -Fq -- "$pattern" out.txt; then
        printf '  missing: %s\n' "$pattern"
        FAILED=1
    fi
done < expect.txt
[ $STATUS -ne 0 ] && { echo "  the kernel stopped"; FAILED=1; }
[ "${VERBOSE:-0}" = 1 ] && cat out.txt
[ $FAILED = 0 ] && echo "  ok    net ($(grep -c . expect.txt) checks, web=$WEB)"
exit $FAILED

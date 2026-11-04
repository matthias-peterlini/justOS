#!/bin/sh
# System updates with two slots, end to end. Usage: scripts/test-update.sh <image>
#
# On a copy of the image (kept across restarts):
#
# BIOS  1. install an update by hand (a signature is asked for; cancel,
#          a refused rollback, install again)
#       2. it starts on trial, uses up a try, and is confirmed
#       3. a broken update (the kernel is all int3: the CPU resets) uses up
#          its tries and the loader goes back to the system that worked
# UEFI  1. install, 2. start on trial and confirm, roll back, 3. the old
#          version starts again
#
# KERNEL, DRIVERS and ARCTOOL default to the build's. Exits non-zero if a
# check failed; the sessions are kept in $KEEP if set.
set -eu
IMAGE=${1:?usage: test-update.sh <image>}
QEMU=${QEMU:-qemu-system-x86_64}
ARCTOOL=${ARCTOOL:-tools/target/release/arctool}
KERNEL=${KERNEL:-system/target/x86_64-unknown-none/release/arc-kernel}
DRIVERS=${DRIVERS:-build/drivers}
OVMF_CODE=${OVMF_CODE:-/usr/share/OVMF/OVMF_CODE_4M.fd}
OVMF_VARS=${OVMF_VARS:-/usr/share/OVMF/OVMF_VARS_4M.fd}
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' system/Cargo.toml | head -1)
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
FAILED=0

# The update, and a broken one.
"$ARCTOOL" slot "$T/new.slot" --kernel "$KERNEL" --drivers "$DRIVERS" --version 9.9.9 > /dev/null
python3 - "$T/new.slot" "$T/broken.slot" 6.6.6 <<'EOF'
import hashlib, struct, sys
src, dst, version = sys.argv[1:4]
b = bytearray(open(src, 'rb').read())
kernel = struct.unpack_from('<I', b, 8)[0]
b[512:512 + kernel * 512] = b'\xcc' * (kernel * 512)
b[0x30:0x50] = hashlib.sha256(bytes(b[512:])).digest()
v = version.encode()
b[0x50:0x70] = v + b'\0' * (32 - len(v))
struct.pack_into('<I', b, 0x1FC, 0)
total = sum(struct.unpack_from('<128I', b, 0)) & 0xFFFFFFFF
struct.pack_into('<I', b, 0x1FC, (-total) & 0xFFFFFFFF)
open(dst, 'wb').write(b)
EOF

# A fresh disk with both files on its system volume.
fresh() {
    cp --sparse=always "$IMAGE" "$T/disk.img"
    "$ARCTOOL" fs "$T/disk.img" put "$T/new.slot" /new.slot > /dev/null
    "$ARCTOOL" fs "$T/disk.img" put "$T/broken.slot" /broken.slot > /dev/null
    cp "$OVMF_VARS" "$T/vars.fd"
}

# run <name> bios|uefi <boot wait> <commands...>: one start (with every
# restart it makes) typing the commands; "@sleep N" pauses.
run() {
    name=$1 fw=$2 wait=$3
    shift 3
    firmware=
    if [ "$fw" = uefi ]; then
        firmware="-drive if=pflash,format=raw,readonly=on,file=$OVMF_CODE -drive if=pflash,format=raw,file=$T/vars.fd"
    fi
    {
        sleep "$wait"
        for c in "$@"; do
            case "$c" in
                '@sleep '*) sleep "${c#@sleep }" ;;
                *) printf '%s\r' "$c"; sleep 2 ;;
            esac
        done
        sleep 2
    } | timeout 180 "$QEMU" -m 512M -display none -monitor none -serial stdio \
        $firmware -drive file="$T/disk.img",format=raw,if=ide > "$T/$name.raw" 2>&1 || true
    sed 's/\x1b\[[0-9;]*[A-Za-z]//g; s/\r//g' "$T/$name.raw" > "$T/$name.log"
    if [ -n "${KEEP:-}" ]; then
        mkdir -p "$KEEP" && cp "$T/$name.log" "$KEEP/"
    fi
}

# expect <name> <pattern> <what>; refuse <name> <pattern> <what>
expect() {
    if grep -qaE -- "$2" "$T/$1.log"; then echo "ok    $1: $3"; else echo "FAIL  $1: $3 (no \"$2\")"; FAILED=1; fi
}
refuse() {
    if grep -qaE -- "$2" "$T/$1.log"; then echo "FAIL  $1: $3 (\"$2\")"; FAILED=1; else echo "ok    $1: $3"; fi
}

echo "== BIOS"
fresh
run bios-1 bios 5 \
    "sysupdate" \
    "sysupdate install sys:/new.slot" \
    "sysupdate install --unsigned sys:/new.slot" "@sleep 12" \
    "sysupdate cancel" \
    "sysupdate rollback" \
    "sysupdate install --unsigned sys:/new.slot" "@sleep 12" \
    "sysupdate" \
    "shutdown"
expect bios-1 "system slot A$" "started from slot A"
expect bios-1 "slot A: $VERSION · works · running" "slot A runs"
expect bios-1 "slot B: $VERSION · works" "slot B holds the same system"
expect bios-1 "new.slot.sig is missing" "a signature is asked for"
expect bios-1 "arcOS 9.9.9 is installed in the other slot" "installed into slot B"
expect bios-1 "this version starts again" "cancelled"
expect bios-1 "non c'è una versione che ha funzionato" "no rollback to a slot being replaced"
expect bios-1 "slot B: 9.9.9 · not confirmed · starts next · 2 tries left" "slot B starts next, on trial"
refuse bios-1 "arcOS stopped" "no panic"

run bios-2 bios 5 \
    "sysupdate" "@sleep 22" \
    "sysupdate" \
    "sysupdate install --unsigned sys:/broken.slot" "@sleep 12" \
    "sysupdate" \
    "shutdown"
expect bios-2 "system slot B, on trial" "started from slot B, on trial"
expect bios-2 "slot B: 9.9.9 · not confirmed · running · 1 try left" "the loader used up a try"
expect bios-2 "works: slot B confirmed" "confirmed"
expect bios-2 "slot B: 9.9.9 · works · running" "slot B works"
expect bios-2 "arcOS 6.6.6 is installed in the other slot" "the broken update installed into slot A"
expect bios-2 "slot A: 6.6.6 · not confirmed · starts next · 2 tries left" "slot A starts next"
refuse bios-2 "arcOS stopped" "no panic"

# Slot A resets the CPU twice; then the loader goes back to B.
run bios-3 bios 14 \
    "@sleep 20" \
    "sysupdate" \
    "shutdown"
expect bios-3 "system slot B, the other one did not start" "back to slot B"
expect bios-3 "did not start: back to arcOS" "it says so"
expect bios-3 "slot A: 6.6.6 · not confirmed$" "slot A is left alone"
expect bios-3 "slot B: 9.9.9 · works · running" "slot B runs"
refuse bios-3 "arcOS stopped" "no panic"

echo "== UEFI"
fresh
run uefi-1 uefi 12 \
    "sysupdate install --unsigned sys:/new.slot" "@sleep 12" \
    "shutdown"
expect uefi-1 "system slot A$" "started from slot A"
expect uefi-1 "arcOS 9.9.9 is installed in the other slot" "installed into slot B"

run uefi-2 uefi 12 \
    "sysupdate" "@sleep 22" \
    "sysupdate" \
    "sysupdate rollback" \
    "shutdown"
expect uefi-2 "system slot B, on trial" "started from slot B, on trial"
expect uefi-2 "slot B: 9.9.9 · not confirmed · running · 1 try left" "the loader used up a try"
expect uefi-2 "slot B: 9.9.9 · works · running" "slot B works"
expect uefi-2 "arcOS $VERSION starts at the next restart" "rolled back"
refuse uefi-2 "arcOS stopped" "no panic"

run uefi-3 uefi 12 \
    "sysupdate" \
    "shutdown"
expect uefi-3 "system slot A$" "slot A again, not on trial"
expect uefi-3 "slot A: $VERSION · works · running" "slot A runs"
expect uefi-3 "slot B: 9.9.9 · works$" "slot B stays, for later"

exit $FAILED

#!/bin/sh
# Boots a justOS image headless and types a list of commands on the serial
# console. Usage: scripts/test.sh <image> [commands-file] [qemu options...]
#
# Lines starting with @sleep N pause; @raw TEXT sends TEXT as a printf
# format without a trailing Enter (\\023 is Ctrl+S).
#
# DISK selects how the image is attached: ide (default), ahci, nvme, virtio
# (virtio-modern and virtio-legacy force one of its two interfaces), or
# cdrom for a live CD (an .iso).
# UEFI=1 starts it with UEFI firmware (OVMF_CODE, OVMF_VARS) instead of a BIOS.
#
# The image is not changed: the session runs on a copy. KEEP_DISK=FILE saves
# that copy as it is after the session, to look at what the session wrote.
#
# QEMU_TIMEOUT (seconds, default 120) ends the machine however far the session got.
#
# Prints the whole session; exits non-zero if the kernel stopped.
set -eu
IMAGE=${1:?usage: test.sh <image> [commands-file] [qemu options...]}
shift
COMMANDS=${1:-scripts/session.txt}
[ $# -gt 0 ] && shift
QEMU=${QEMU:-qemu-system-x86_64}
BOOT_WAIT=${BOOT_WAIT:-4}
STEP_WAIT=${STEP_WAIT:-1}
WORK=$(mktemp)
cp --sparse=always "$IMAGE" "$WORK"
IMAGE=$WORK
case "${DISK:-ide}" in
    ide)    DRIVE="-drive file=$IMAGE,format=raw,if=ide" ;;
    ahci)   DRIVE="-machine q35 -drive file=$IMAGE,format=raw,if=none,id=sys -device ahci,id=ahci -device ide-hd,drive=sys,bus=ahci.0,bootindex=0" ;;
    nvme)   DRIVE="-machine q35 -drive file=$IMAGE,format=raw,if=none,id=sys -device nvme,drive=sys,serial=just0001,bootindex=0" ;;
    virtio) DRIVE="-drive file=$IMAGE,format=raw,if=none,id=sys -device virtio-blk-pci,drive=sys,bootindex=0" ;;
    virtio-modern) DRIVE="-drive file=$IMAGE,format=raw,if=none,id=sys -device virtio-blk-pci,drive=sys,bootindex=0,disable-legacy=on" ;;
    virtio-legacy) DRIVE="-drive file=$IMAGE,format=raw,if=none,id=sys -device virtio-blk-pci,drive=sys,bootindex=0,disable-modern=on" ;;
    cdrom)  DRIVE="-cdrom $IMAGE" ;;
    *) echo "unknown DISK=$DISK" >&2; exit 2 ;;
esac
OUT=$(mktemp)
VARS=
FIRMWARE=
if [ "${UEFI:-0}" = 1 ]; then
    # A fresh copy of the firmware variables for every run.
    VARS=$(mktemp)
    cp "${OVMF_VARS:-/usr/share/OVMF/OVMF_VARS_4M.fd}" "$VARS"
    FIRMWARE="-drive if=pflash,format=raw,readonly=on,file=${OVMF_CODE:-/usr/share/OVMF/OVMF_CODE_4M.fd} -drive if=pflash,format=raw,file=$VARS"
    BOOT_WAIT=${BOOT_WAIT_UEFI:-10}
fi
{
    sleep "$BOOT_WAIT"
    while IFS= read -r line; do
        case "$line" in
            '#'*|'') continue ;;
            '@sleep '*) sleep "${line#@sleep }"; continue ;;
            '@raw '*) printf "${line#@raw }"; sleep "$STEP_WAIT"; continue ;;
        esac
        printf '%s\r' "$line"
        sleep "$STEP_WAIT"
    done < "$COMMANDS"
    sleep 1
} | timeout "${QEMU_TIMEOUT:-120}" "$QEMU" -m 512M -display none -no-reboot -monitor none -serial stdio \
    $FIRMWARE $DRIVE "$@" > "$OUT" 2>&1 || true
# Strip colours for readability.
sed 's/\x1b\[[0-9;]*[A-Za-z]//g; s/\r//g' "$OUT"
STATUS=0
grep -q "justOS stopped" "$OUT" && STATUS=1
if [ -n "${KEEP_DISK:-}" ]; then cp --sparse=always "$WORK" "$KEEP_DISK"; fi
rm -f "$OUT" "$WORK" $VARS
exit $STATUS

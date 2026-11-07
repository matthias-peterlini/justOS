#!/bin/sh
# Boots a justOS image once for every kind of hardware QEMU can emulate
# and checks that the matching driver installs and works: disks (a file is
# written and the file system checked), network cards (DHCP and ping) and
# USB controllers (a keyboard and a USB stick behind each one) and SD card
# readers (a file is written on the card). A ping
# slower than 20 ms fails too: replies then come from the drivers' polling
# thread, so the card's interrupts are broken.
#
# The rtl8169, rtl8168 and rtl8125 cases need a QEMU with the models from
# tools/qemu (scripts/build-qemu.sh builds one; QEMU_MODELS names another)
# and are skipped without it. Those models report what the real chip would
# not accept; any such report fails the case.
#
# Usage: scripts/test-drivers.sh <image> [case...]
# With no case, all of them run. Prints one line per case; exits non-zero
# if any failed. VERBOSE=1 prints the whole session of failed cases.
# SLEEP=1 puts the machine to sleep (S3) for a few seconds before each
# check: the drivers must bring their devices back on waking.
set -u
IMAGE=${1:?usage: test-drivers.sh <image> [case...]}
shift
HERE=$(dirname "$0")
JUSTTOOL=${JUSTTOOL:-$HERE/../tools/target/release/justtool}
QEMU_MODELS=${QEMU_MODELS:-$HERE/../build/qemu/build/qemu-system-x86_64}
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

ALL="ide ahci nvme virtio virtio-modern virtio-legacy
e1000 e1000e igb rtl8139 pcnet virtio-net virtio-net-modern multi-nic rtl8169 rtl8168 rtl8125
xhci ehci uhci ohci sdhci sdhci-sdma sdhci-pio"
CASES=${*:-$ALL}

# A USB stick: one JustFS partition labelled "Stick".
"$JUSTTOOL" mkfs "$WORK/stick.img" --size 16M --label Stick >/dev/null || exit 2

# SD cards: 64 MiB (byte addressing) and 4 GiB (SDHC, sector addressing).
"$JUSTTOOL" mkfs "$WORK/sd.img" --size 64M --label Card >/dev/null || exit 2
"$JUSTTOOL" mkfs "$WORK/sdhc.img" --size 4G --label Card >/dev/null || exit 2

# Commands for each kind of case, and what the session has to show.
cat > "$WORK/disk.txt" <<'EOT'
@sleep 1
write sys:/home/t.txt written on this disk
cat sys:/home/t.txt
check sys
shutdown
EOT
cat > "$WORK/net.txt" <<'EOT'
@sleep 3
net
ping 10.0.2.2 3
shutdown
EOT
cat > "$WORK/sd.txt" <<'EOT'
@sleep 3
dev
write card:/t.txt written on the card
cat card:/t.txt
check card
shutdown
EOT
cat > "$WORK/usb.txt" <<'EOT'
@sleep 3
dev
cp sys:/home/welcome.txt stick:/w.txt
cat stick:/w.txt
check stick
shutdown
EOT
if [ "${SLEEP:-0}" = 1 ]; then
    for f in disk net sd usb; do
        { head -n 1 "$WORK/$f.txt"; printf 'suspend 3\n@sleep 9\n'; tail -n +2 "$WORK/$f.txt"; } > "$WORK/$f.new"
        mv "$WORK/$f.new" "$WORK/$f.txt"
    done
fi

STICK="-drive if=none,id=stick,format=raw,file=$WORK/stick.img"
EHCI="-device ich9-usb-ehci1,id=u,addr=1d.7,multifunction=on
 -device ich9-usb-uhci1,masterbus=u.0,firstport=0,addr=1d.0,multifunction=on
 -device ich9-usb-uhci2,masterbus=u.0,firstport=2,addr=1d.1
 -device ich9-usb-uhci3,masterbus=u.0,firstport=4,addr=1d.2"

FAILED=0
for c in $CASES; do
    DISK=ide
    ARGS=
    CMDS=disk
    RUN_QEMU=${QEMU:-qemu-system-x86_64}
    case $c in
        ide|ahci|nvme|virtio|virtio-modern|virtio-legacy) DISK=$c; EXPECT="written on this disk|sys: no problems found" ;;
        e1000|rtl8139|pcnet) ARGS="-nic user,model=$c"; CMDS=net ;;
        e1000e) DISK=ahci; ARGS="-nic user,model=e1000e"; CMDS=net ;;
        igb) DISK=ahci; ARGS="-nic user,model=igb"; CMDS=net ;;
        virtio-net) ARGS="-nic user,model=virtio-net-pci"; CMDS=net ;;
        virtio-net-modern) ARGS="-netdev user,id=n -device virtio-net-pci,netdev=n,disable-legacy=on"; CMDS=net ;;
        multi-nic) ARGS="-nic user,model=e1000 -nic user,model=rtl8139,mac=52:54:00:00:00:02 -nic user,model=pcnet,mac=52:54:00:00:00:03"; CMDS=net ;;
        rtl8169|rtl8168|rtl8125)
            if [ ! -x "$QEMU_MODELS" ]; then
                printf '  skip  %s: no QEMU with the model (scripts/build-qemu.sh)\n' "$c"
                continue
            fi
            RUN_QEMU=$QEMU_MODELS
            ARGS="-d guest_errors -netdev user,id=n -device $c,netdev=n"; CMDS=net ;;
        xhci) ARGS="-device qemu-xhci,id=u $STICK -device usb-hub,bus=u.0,port=1 -device usb-kbd,bus=u.0,port=1.1 -device usb-storage,bus=u.0,port=2,drive=stick"; CMDS=usb ;;
        ehci) ARGS="$EHCI $STICK -device usb-kbd,bus=u.0,port=1 -device usb-storage,bus=u.0,port=2,drive=stick"; CMDS=usb ;;
        uhci) ARGS="-device piix3-usb-uhci,id=u $STICK -device usb-hub,bus=u.0,port=1 -device usb-kbd,bus=u.0,port=1.1 -device usb-storage,bus=u.0,port=2,drive=stick"; CMDS=usb ;;
        ohci) ARGS="-device pci-ohci,id=u $STICK -device usb-hub,bus=u.0,port=1 -device usb-kbd,bus=u.0,port=1.1 -device usb-storage,bus=u.0,port=2,drive=stick"; CMDS=usb ;;
        # The reader's capabilities choose how data moves: ADMA2 (QEMU's
        # default), SDMA, or the data port.
        sdhci|sdhci-sdma|sdhci-pio)
            img=sd caps=
            case $c in
                sdhci) img=sdhc ;;
                sdhci-sdma) caps=,capareg=0x057034b4 ;;
                sdhci-pio) caps=,capareg=0x053034b4 ;;
            esac
            case $c in
                sdhci) EXPECT_SD="ADMA2.*|SDHC card" ;;
                sdhci-sdma) EXPECT_SD="SDMA|SD card" ;;
                *) EXPECT_SD="no DMA|SD card" ;;
            esac
            ARGS="-device sdhci-pci$caps -drive if=none,id=card,format=raw,file=$WORK/$img.img -device sd-card,drive=card"; CMDS=sd ;;
        *) echo "unknown case: $c" >&2; exit 2 ;;
    esac
    case $CMDS in
        net) EXPECT="3 of 3 replies" ;;
        sd) EXPECT="sdhci 1.1.1 installed|$EXPECT_SD|written on the card|card: no problems found" ;;
        usb) EXPECT="usbhid 1.0.0 installed|usbstorage 1.0.0 installed|Benvenuto in justOS|stick: no problems found" ;;
    esac
    [ "$c" = multi-nic ] && EXPECT="$EXPECT|^net2 "
    [ "${SLEEP:-0}" = 1 ] && EXPECT="$EXPECT|awake again after"
    QEMU=$RUN_QEMU DISK=$DISK "$HERE/test.sh" "$IMAGE" "$WORK/$CMDS.txt" $ARGS > "$WORK/out.txt" 2>&1
    STATUS=$?
    missing=
    IFS='|'
    for e in $EXPECT; do
        grep -q -- "$e" "$WORK/out.txt" || missing="$missing [no \"$e\"]"
    done
    unset IFS
    [ $STATUS -ne 0 ] && missing="$missing [kernel stopped]"
    if [ $CMDS = net ] && grep -Eq 'time ([2-9][0-9]|[0-9]{3,}) ms' "$WORK/out.txt"; then
        missing="$missing [interrupts: slow replies]"
    fi
    grep -q '^rtl8169: ' "$WORK/out.txt" && missing="$missing [the model reported errors]"
    if [ -z "$missing" ]; then
        printf '  ok    %s\n' "$c"
    else
        printf '  FAIL  %s:%s\n' "$c" "$missing"
        [ "${VERBOSE:-0}" = 1 ] && cat "$WORK/out.txt"
        FAILED=1
    fi
done
exit $FAILED

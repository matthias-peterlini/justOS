# justOS build.
#
#   make            build the bootable disk image (build/justos.img), with
#                   Wine for Windows programs; WINE=0: without it, 1 GiB
#   make iso        build a live CD (build/justos.iso), BIOS and UEFI
#   make run-iso    start the live CD in QEMU, with no hard disk
#   make run        start it in QEMU (PC with IDE disk)
#   make run-q35    start it in QEMU (modern PC: AHCI/SATA disk)
#   make run-nvme   start it in QEMU with the system disk on NVMe
#   make run-uefi   start it in QEMU with UEFI firmware (OVMF) instead of a BIOS
#   make test       boot headless and run a scripted session. The tests boot
#                   an image without Wine (build/justos-test.img, or the
#                   IMAGE given on the command line), not build/justos.img
#   make test-uefi  the same, started by UEFI firmware
#   make test-iso   the same session on the live CD, with BIOS and with UEFI
#   make test-drivers  boot once per emulated disk, network card and USB
#                   controller and check that its driver works (SLEEP=1:
#                   after the machine slept and woke up; test-audio too)
#   make test-compat   run a Linux and a Windows program on justOS and check them
#   make test-audio    play sounds on every emulated sound card and check them
#   make test-display  switch the resolution on every emulated display, and
#                   wake up with the picture back
#   make test-acpi     power button, a laptop's battery and sensors (AML),
#                   sleep and wake-up
#   make test-net      TCP, HTTP and HTTPS against servers on this machine
#   make test-linux    Linux programs with threads, sockets, poll (C, Rust, Go)
#   make test-wayland  a Wayland client's window on the desktop: drawing,
#                   pointer, keyboard, a popup
#   make test-gtk   a GTK 3 program's window, with the justOS theme
#   make shots      boot headless, drive the GUI and save screenshots (build/shots)
#   make assets     regenerate fonts, icons, wallpapers and sounds (Python, Pillow, numpy)
#   make photos     download the photo wallpapers from Unsplash
#   make check      unit tests of the shared libraries and the host tool
#   make test-update   install a system update into the other slot, restart
#                   into it; a broken one goes back by itself (BIOS and UEFI)
#   make updates    the signed index of updates (build/updates): the store's
#                   apps and this justOS as a system image, signed with the
#                   private key in UPDATES_KEY; scripts/publish-updates.sh
#                   puts it online

PROFILE  ?= release
CARGO    ?= cargo
QEMU     ?= qemu-system-x86_64
MEMORY   ?= 1G
# Wine 10 runs Windows programs (a double click on a .exe): it is in the
# disk image unless WINE=0.
WINE     ?= 1
# The disk image's size (SIZE, below, after WINE): the EFI partition holds
# two system slots of SLOT_SIZE.
SLOT_SIZE ?= 24M
# UEFI firmware for QEMU: code (read-only) and a template of its variables.
OVMF_CODE ?= /usr/share/OVMF/OVMF_CODE_4M.fd
OVMF_VARS ?= /usr/share/OVMF/OVMF_VARS_4M.fd

BUILD    := build
TARGET   := system/target/x86_64-unknown-none/$(PROFILE)
JUSTTOOL  := tools/target/release/justtool
EFI_APP  := boot/uefi/target/x86_64-unknown-uefi/release/BOOTX64.efi
DRIVERS  := $(notdir $(wildcard system/drivers/*))
PACKAGES := $(DRIVERS:%=$(BUILD)/drivers/%.jdp)
APPS     := $(notdir $(wildcard system/apps/*))
# Programs of the store (sys:/store), installed with the Negozio app.
STORE    := $(notdir $(wildcard system/store/*))
# What dynamically linked Linux programs need, from Debian's packages
# (tools/linux/debroot.py): the dynamic loader and glibc (with its C.UTF-8
# locale, in libc-bin), what programs with windows link to talk to the
# Wayland compositor, and GTK 3 (with the SVG loader for its icons, and
# its demo programs gtk3-widget-factory and gtk3-demo), and Firefox, which
# comes with the system (it is in the launcher, from its .desktop file).
LINUX_PKGS ?= dash libc6 libc-bin libwayland-client0 libxkbcommon0 libgtk-3-0t64 librsvg2-common gtk-3-examples firefox-esr
LINUX_DIST ?= trixie
# Packages left out although others depend on them (comma-separated):
# libraries the programs run without.
LINUX_SKIP ?=
# Wine 10 for Windows programs (unless WINE=0), 64-bit; debroot.py adds
# the DLLs for 32-bit programs (WoW64) from libwine's i386 package. It
# runs without the multimedia, camera, scanner, smart card, sound server
# and OpenCL libraries its packages pull in.
WINE_PKGS := wine wine64 libwine fonts-wine
WINE_SKIP := libavcodec61,libavformat61,libavutil59,libcapi20-3t64,libgphoto2-6t64,libgphoto2-port12t64,libgstreamer-plugins-base1.0-0,libgstreamer1.0-0,libpcap0.8t64,libpcsclite1,libpulse0,libusb-1.0-0,ocl-icd-libopencl1,libopencl1,libopencl-1.2-1
comma    := ,
# With Wine and without, the Linux tree and the system volume's files are
# kept apart, so that the image and the tests' image (without Wine) do not
# undo each other's, even when make builds both at once (make -j image
# test). With Wine the image needs room for the files (1.7 GiB, 1.3 of
# them Wine's DLLs) and for the C: drive Wine makes at its first start,
# which copies the DLLs again (1.4 GiB more, measured): 5 GiB leaves about
# 1.8 GiB for the user's files after that.
ifeq ($(WINE),0)
SIZE     ?= 1G
LINUX    := $(BUILD)/linux
ROOT     := $(BUILD)/root
LIVE_ROOT := $(BUILD)/liveroot-nowine
PKGS     := $(LINUX_PKGS)
SKIP     := $(LINUX_SKIP)
else
SIZE     ?= 5G
LINUX    := $(BUILD)/winelinux
ROOT     := $(BUILD)/wineroot
LIVE_ROOT := $(BUILD)/liveroot
PKGS     := $(LINUX_PKGS) $(WINE_PKGS)
SKIP     := $(if $(LINUX_SKIP),$(LINUX_SKIP)$(comma))$(WINE_SKIP)
endif
IMAGE    := $(BUILD)/justos.img
# The tests boot an image without Wine: quicker to build and to copy, and
# they need nothing of it. An IMAGE given on the command line ("make test
# IMAGE=build/justos-t.img") is the one the tests build and boot.
ifeq ($(origin IMAGE),command line)
TEST_IMAGE := $(IMAGE)
else
TEST_IMAGE := $(BUILD)/justos-test.img
endif
ISO      := $(BUILD)/justos.iso
# Size of the live CD's system volume (it lives in memory).
LIVE_SIZE ?= 48M
# App updates: the signing key (kept out of the repository) and where the
# index is published (kernel/src/appupdate.rs looks there).
UPDATES_KEY ?= $(HOME)/.justos-keys/updates.key
UPDATES_URL ?= https://raw.githubusercontent.com/matthias-peterlini/justOS/updates
VERSION  := $(shell sed -n 's/^version = "\(.*\)"/\1/p' system/Cargo.toml | head -1)

ifeq ($(PROFILE),release)
CARGO_PROFILE := --release
endif

.PHONY: photos all uefi system tools live-root root linux-root image wine-image test-image iso run-iso test-iso run run-q35 run-nvme run-uefi test test-uefi test-drivers test-compat test-audio test-display test-acpi test-net test-linux test-wayland test-gtk test-update shots assets check updates clean $(PACKAGES)

all: image

$(BUILD)/stage1.bin: boot/bios/stage1.asm
	@mkdir -p $(BUILD)
	nasm -f bin -o $@ $<

$(BUILD)/stage2.bin: boot/bios/stage2.asm
	@mkdir -p $(BUILD)
	nasm -f bin -o $@ $<

uefi:
	cd boot/uefi && $(CARGO) build --release

system:
	cd system && $(CARGO) build $(CARGO_PROFILE) --workspace

tools:
	cd tools && $(CARGO) build --release

$(PACKAGES): $(BUILD)/drivers/%.jdp: system tools
	@mkdir -p $(BUILD)/drivers
	$(JUSTTOOL) pack system/drivers/$*/driver.manifest $(TARGET)/$* $@

# The Linux libraries, unpacked from Debian's packages (downloads are kept
# in build/debcache, so this runs offline once they are there).
$(LINUX)/usr/PACKAGES: tools/linux/debroot.py Makefile
	tools/linux/debroot.py --out $(LINUX) --cache $(BUILD)/debcache --dist $(LINUX_DIST) \
		$(if $(SKIP),--skip $(SKIP)) $(PKGS)

linux-root:
	rm -f $(LINUX)/usr/PACKAGES
	$(MAKE) $(LINUX)/usr/PACKAGES

# justOS's own files: rootfs/, the programs, the store and the fonts. They
# are the whole system volume of the live CD ($(LIVE_ROOT)), without the
# Linux libraries: it is read into memory at boot, and Debian's tree (GTK,
# Firefox, Wine) is many times the rest.
live-root: system tools
	rm -rf $(LIVE_ROOT)
	mkdir -p $(LIVE_ROOT)/apps
	cp -r rootfs/. $(LIVE_ROOT)/
	for app in $(APPS); do cp $(TARGET)/$$app $(LIVE_ROOT)/apps/$$app.app; done
	for p in $(STORE); do mkdir -p $(LIVE_ROOT)/store/$$p && cp $(TARGET)/$$p $(LIVE_ROOT)/store/$$p/app \
		&& cp system/store/$$p/info system/store/$$p/icon.qoi $(LIVE_ROOT)/store/$$p/; done
	cp system/kernel/assets/fonts.bin $(LIVE_ROOT)/system/fonts.bin
	mkdir -p $(LIVE_ROOT)/usr/share/fonts/truetype/justos
	cp tools/assets/fonts/GoogleSansFlex.ttf tools/assets/fonts/OFL.txt $(LIVE_ROOT)/usr/share/fonts/truetype/justos/
	gzip -dc system/kernel/assets/fonts@2x.bin.gz > $(LIVE_ROOT)/system/fonts@2x.bin

# The disk image's system volume: justOS's files and the Linux libraries.
root: live-root $(LINUX)/usr/PACKAGES
	rm -rf $(ROOT)
	cp -a $(LIVE_ROOT) $(ROOT)
	# Debian's files, under justOS's: rootfs/ wins where both have one.
	cp -a $(LINUX)/. $(ROOT)/
	cp -r rootfs/. $(ROOT)/
	# Windows programs (Wine, when there) take justOS's look too.
	tools/linux/winelook.py $(ROOT)

image: $(BUILD)/stage1.bin $(BUILD)/stage2.bin uefi system tools $(PACKAGES) root
	$(JUSTTOOL) image $(IMAGE) --size $(SIZE) --slot-size $(SLOT_SIZE) --version $(VERSION) \
		--stage1 $(BUILD)/stage1.bin --stage2 $(BUILD)/stage2.bin --efi $(EFI_APP) \
		--kernel $(TARGET)/just-kernel --drivers $(BUILD)/drivers --files $(ROOT)

# Wine is in the usual image now; the old name still builds it.
wine-image: image

# The image the tests boot: without Wine, 1 GiB.
test-image:
	$(MAKE) image WINE=0 IMAGE=$(TEST_IMAGE)

# This justOS as a system update (a slot image).
$(BUILD)/justos-$(VERSION).slot: system tools $(PACKAGES)
	$(JUSTTOOL) slot $@ --kernel $(TARGET)/just-kernel --drivers $(BUILD)/drivers --version $(VERSION)

iso: $(BUILD)/stage1.bin $(BUILD)/stage2.bin uefi system tools $(PACKAGES) live-root
	$(JUSTTOOL) iso $(ISO) --size $(LIVE_SIZE) --version $(VERSION) \
		--stage1 $(BUILD)/stage1.bin --stage2 $(BUILD)/stage2.bin --efi $(EFI_APP) \
		--kernel $(TARGET)/just-kernel --drivers $(BUILD)/drivers --files $(LIVE_ROOT)

QEMU_COMMON := -m $(MEMORY) -serial stdio -no-reboot -rtc base=localtime

run: image
	$(QEMU) $(QEMU_COMMON) -drive file=$(IMAGE),format=raw,if=ide

run-q35: image
	$(QEMU) $(QEMU_COMMON) -machine q35 -drive file=$(IMAGE),format=raw,if=none,id=sys -device ahci,id=ahci -device ide-hd,drive=sys,bus=ahci.0

run-nvme: image
	$(QEMU) $(QEMU_COMMON) -machine q35 -drive file=$(IMAGE),format=raw,if=none,id=sys -device nvme,drive=sys,serial=just0001

# The firmware keeps its settings (boot entries) in build/ovmf-vars.fd.
run-iso: iso
	$(QEMU) $(QEMU_COMMON) -cdrom $(ISO)

run-uefi: image
	@test -f $(BUILD)/ovmf-vars.fd || cp $(OVMF_VARS) $(BUILD)/ovmf-vars.fd
	$(QEMU) $(QEMU_COMMON) -drive if=pflash,format=raw,readonly=on,file=$(OVMF_CODE) \
		-drive if=pflash,format=raw,file=$(BUILD)/ovmf-vars.fd -drive file=$(IMAGE),format=raw,if=ide

test: test-image
	scripts/test.sh $(TEST_IMAGE)

test-uefi: test-image
	UEFI=1 OVMF_CODE=$(OVMF_CODE) OVMF_VARS=$(OVMF_VARS) scripts/test.sh $(TEST_IMAGE)

test-drivers: test-image
	scripts/test-drivers.sh $(TEST_IMAGE)

test-iso: iso
	DISK=cdrom scripts/test.sh $(ISO)
	UEFI=1 DISK=cdrom OVMF_CODE=$(OVMF_CODE) OVMF_VARS=$(OVMF_VARS) scripts/test.sh $(ISO)

test-compat: test-image
	scripts/test-compat.sh $(TEST_IMAGE)

test-audio: test-image
	scripts/test-audio.sh $(TEST_IMAGE)

test-display: test-image
	scripts/test-display.sh $(TEST_IMAGE)

test-acpi: test-image
	scripts/test-acpi.sh $(TEST_IMAGE)

test-net: test-image
	scripts/test-net.sh $(TEST_IMAGE)

test-linux: test-image
	scripts/test-linux.sh $(TEST_IMAGE)

test-wayland: test-image
	scripts/test-wayland.sh $(TEST_IMAGE)

test-gtk: test-image
	scripts/test-gtk.sh $(TEST_IMAGE)

test-update: test-image
	OVMF_CODE=$(OVMF_CODE) OVMF_VARS=$(OVMF_VARS) scripts/test-update.sh $(TEST_IMAGE)

shots: test-image
	scripts/gui-shots.py $(TEST_IMAGE) scripts/gui/tour.txt $(BUILD)/shots

assets:
	tools/assets/gen_fonts.py system/kernel/assets/fonts.bin
	tools/assets/gen_fonts.py --sharp system/kernel/assets/fonts@2x.bin.gz
	tools/assets/gen_certs.py rootfs
	tools/assets/gen_pixicons.py rootfs/system/gui
	tools/assets/gen_pixicons.py --store system/store
	tools/assets/gen_wallpapers.py rootfs/system/gui/wallpapers
	tools/assets/gen_pixel.py rootfs/system/gui/wallpapers
	tools/assets/gen_thumbs.py rootfs/system/gui/wallpapers
	tools/assets/gen_sounds.py rootfs/system/sounds rootfs/home/Musica

# The photo wallpapers, downloaded from Unsplash (needs the network).
photos:
	tools/assets/fetch_photos.py rootfs/system/gui/wallpapers
	tools/assets/gen_thumbs.py rootfs/system/gui/wallpapers

check:
	cd system && $(CARGO) test --target x86_64-unknown-linux-gnu -p justdk -p justfs -p justgfx -p justimage -p justmath -p justtls -p justzip -p justjson -p justdex -p justaml
	cd tools && $(CARGO) test --release

updates: live-root tools $(BUILD)/justos-$(VERSION).slot
	rm -rf $(BUILD)/updates
	$(JUSTTOOL) updates $(LIVE_ROOT)/store $(BUILD)/updates --key $(UPDATES_KEY) --base-url $(UPDATES_URL) --system $(VERSION) \
		--slot $(BUILD)/justos-$(VERSION).slot

clean:
	rm -rf $(BUILD)
	cd system && $(CARGO) clean
	cd tools && $(CARGO) clean
	cd boot/uefi && $(CARGO) clean

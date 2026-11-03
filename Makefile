# arcOS build.
#
#   make            build the bootable disk image (build/arcos.img)
#   make iso        build a live CD (build/arcos.iso), BIOS and UEFI
#   make run-iso    start the live CD in QEMU, with no hard disk
#   make run        start it in QEMU (PC with IDE disk)
#   make run-q35    start it in QEMU (modern PC: AHCI/SATA disk)
#   make run-nvme   start it in QEMU with the system disk on NVMe
#   make run-uefi   start it in QEMU with UEFI firmware (OVMF) instead of a BIOS
#   make test       boot headless and run a scripted session
#   make test-uefi  the same, started by UEFI firmware
#   make test-iso   the same session on the live CD, with BIOS and with UEFI
#   make test-drivers  boot once per emulated disk, network card and USB
#                   controller and check that its driver works
#   make test-arcscript  run ArcScript programs on arcOS and check them
#   make test-compat   run a Linux and a Windows program on arcOS and check them
#   make test-audio    play sounds on every emulated sound card and check them
#   make test-display  switch the resolution on every emulated display
#   make test-acpi     press the power button and check that arcOS turns off
#   make test-net      TCP, HTTP and HTTPS against servers on this machine
#   make test-linux    Linux programs with threads, sockets, poll (C, Rust, Go)
#   make shots      boot headless, drive the GUI and save screenshots (build/shots)
#   make assets     regenerate fonts, icons, wallpapers and sounds (Python, Pillow, numpy)
#   make photos     download the photo wallpapers from Wikimedia Commons
#   make check      unit tests of the shared libraries and the host tool
#   make updates    the signed index of app updates (build/updates), with the
#                   private key in UPDATES_KEY; scripts/publish-updates.sh
#                   puts it online

PROFILE  ?= release
CARGO    ?= cargo
QEMU     ?= qemu-system-x86_64
MEMORY   ?= 512M
SIZE     ?= 128M
# UEFI firmware for QEMU: code (read-only) and a template of its variables.
OVMF_CODE ?= /usr/share/OVMF/OVMF_CODE_4M.fd
OVMF_VARS ?= /usr/share/OVMF/OVMF_VARS_4M.fd

BUILD    := build
TARGET   := system/target/x86_64-unknown-none/$(PROFILE)
ARCTOOL  := tools/target/release/arctool
EFI_APP  := boot/uefi/target/x86_64-unknown-uefi/release/BOOTX64.efi
DRIVERS  := $(notdir $(wildcard system/drivers/*))
PACKAGES := $(DRIVERS:%=$(BUILD)/drivers/%.adp)
APPS     := $(notdir $(wildcard system/apps/*))
# Programs of the store (sys:/store), installed with the Negozio app.
STORE    := $(notdir $(wildcard system/store/*))
ROOT     := $(BUILD)/root
IMAGE    := $(BUILD)/arcos.img
ISO      := $(BUILD)/arcos.iso
# Size of the live CD's system volume (it lives in memory).
LIVE_SIZE ?= 24M
# App updates: the signing key (kept out of the repository) and where the
# index is published (kernel/src/appupdate.rs looks there).
UPDATES_KEY ?= $(HOME)/.arcos-keys/updates.key
UPDATES_URL ?= https://raw.githubusercontent.com/matthias-peterlini/ArcOs/updates
VERSION  := $(shell sed -n 's/^version = "\(.*\)"/\1/p' system/Cargo.toml | head -1)

ifeq ($(PROFILE),release)
CARGO_PROFILE := --release
endif

.PHONY: photos all uefi system tools root image iso run-iso test-iso run run-q35 run-nvme run-uefi test test-uefi test-drivers test-arcscript test-compat test-audio test-display test-acpi test-net test-linux shots assets check updates clean $(PACKAGES)

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

$(PACKAGES): $(BUILD)/drivers/%.adp: system tools
	@mkdir -p $(BUILD)/drivers
	$(ARCTOOL) pack system/drivers/$*/driver.manifest $(TARGET)/$* $@

# The files of the system volume: rootfs/ plus the programs.
root: system
	rm -rf $(ROOT)
	mkdir -p $(ROOT)/apps
	cp -r rootfs/. $(ROOT)/
	for app in $(APPS); do cp $(TARGET)/$$app $(ROOT)/apps/$$app.app; done
	for p in $(STORE); do mkdir -p $(ROOT)/store/$$p && cp $(TARGET)/$$p $(ROOT)/store/$$p/app \
		&& cp system/store/$$p/info system/store/$$p/icon.qoi $(ROOT)/store/$$p/; done
	cp system/kernel/assets/fonts.bin $(ROOT)/system/fonts.bin

image: $(BUILD)/stage1.bin $(BUILD)/stage2.bin uefi system tools $(PACKAGES) root
	$(ARCTOOL) image $(IMAGE) --size $(SIZE) \
		--stage1 $(BUILD)/stage1.bin --stage2 $(BUILD)/stage2.bin --efi $(EFI_APP) \
		--kernel $(TARGET)/arc-kernel --drivers $(BUILD)/drivers --files $(ROOT)

iso: $(BUILD)/stage1.bin $(BUILD)/stage2.bin uefi system tools $(PACKAGES) root
	$(ARCTOOL) iso $(ISO) --size $(LIVE_SIZE) \
		--stage1 $(BUILD)/stage1.bin --stage2 $(BUILD)/stage2.bin --efi $(EFI_APP) \
		--kernel $(TARGET)/arc-kernel --drivers $(BUILD)/drivers --files $(ROOT)

QEMU_COMMON := -m $(MEMORY) -serial stdio -no-reboot -rtc base=localtime

run: image
	$(QEMU) $(QEMU_COMMON) -drive file=$(IMAGE),format=raw,if=ide

run-q35: image
	$(QEMU) $(QEMU_COMMON) -machine q35 -drive file=$(IMAGE),format=raw,if=none,id=sys -device ahci,id=ahci -device ide-hd,drive=sys,bus=ahci.0

run-nvme: image
	$(QEMU) $(QEMU_COMMON) -machine q35 -drive file=$(IMAGE),format=raw,if=none,id=sys -device nvme,drive=sys,serial=arc0001

# The firmware keeps its settings (boot entries) in build/ovmf-vars.fd.
run-iso: iso
	$(QEMU) $(QEMU_COMMON) -cdrom $(ISO)

run-uefi: image
	@test -f $(BUILD)/ovmf-vars.fd || cp $(OVMF_VARS) $(BUILD)/ovmf-vars.fd
	$(QEMU) $(QEMU_COMMON) -drive if=pflash,format=raw,readonly=on,file=$(OVMF_CODE) \
		-drive if=pflash,format=raw,file=$(BUILD)/ovmf-vars.fd -drive file=$(IMAGE),format=raw,if=ide

test: image
	scripts/test.sh $(IMAGE)

test-uefi: image
	UEFI=1 OVMF_CODE=$(OVMF_CODE) OVMF_VARS=$(OVMF_VARS) scripts/test.sh $(IMAGE)

test-drivers: image
	scripts/test-drivers.sh $(IMAGE)

test-iso: iso
	DISK=cdrom scripts/test.sh $(ISO)
	UEFI=1 DISK=cdrom OVMF_CODE=$(OVMF_CODE) OVMF_VARS=$(OVMF_VARS) scripts/test.sh $(ISO)

test-arcscript: image
	scripts/test-arcscript.sh $(IMAGE)

test-compat: image
	scripts/test-compat.sh $(IMAGE)

test-audio: image
	scripts/test-audio.sh $(IMAGE)

test-display: image
	scripts/test-display.sh $(IMAGE)

test-acpi: image
	scripts/test-acpi.sh $(IMAGE)

test-net: image
	scripts/test-net.sh $(IMAGE)

test-linux: image
	scripts/test-linux.sh $(IMAGE)

shots: image
	scripts/gui-shots.py $(IMAGE) scripts/gui/tour.txt $(BUILD)/shots

assets:
	tools/assets/gen_fonts.py system/kernel/assets/fonts.bin
	tools/assets/gen_fonts.py --web rootfs/system/webfonts.bin
	tools/assets/gen_logo.py system/kernel/assets/logo.bin
	tools/assets/gen_certs.py rootfs
	tools/assets/gen_icons.py rootfs/system/gui
	tools/assets/gen_icons.py --store system/store
	tools/assets/gen_wallpapers.py rootfs/system/gui/wallpapers
	tools/assets/gen_thumbs.py rootfs/system/gui/wallpapers
	tools/assets/gen_sounds.py rootfs/system/sounds rootfs/home/Musica

# The photo wallpapers, downloaded from Wikimedia Commons (needs the network).
photos:
	tools/assets/fetch_photos.py rootfs/system/gui/wallpapers
	tools/assets/gen_thumbs.py rootfs/system/gui/wallpapers

check:
	cd system && $(CARGO) test --target x86_64-unknown-linux-gnu -p arcdk -p arcfs -p arcgfx -p arctls -p arczip -p arcjson -p arcc-core -p arc-vm -p arcweb -p arcjs -p arcdex
	cd tools && $(CARGO) test --release

updates: root tools
	rm -rf $(BUILD)/updates
	$(ARCTOOL) updates $(ROOT)/store $(BUILD)/updates --key $(UPDATES_KEY) --base-url $(UPDATES_URL) --system $(VERSION)

clean:
	rm -rf $(BUILD)
	cd system && $(CARGO) clean
	cd tools && $(CARGO) clean
	cd boot/uefi && $(CARGO) clean

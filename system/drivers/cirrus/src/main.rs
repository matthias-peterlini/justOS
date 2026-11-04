//! Display driver for the Cirrus Logic GD5446, the older default display of
//! QEMU (`-vga cirrus`) and of many virtual machines set up years ago.
//!
//! The controller has no mode-setting interface of its own: the driver
//! programs the VGA timing registers (CRTC) for the resolution, and the Cirrus
//! extension registers for 32-bit colour and the longer line pitch, through
//! the usual VGA ports. The framebuffer is the first BAR. A line is at most
//! 4088 bytes, so in 32-bit colour the screen is at most 1022 pixels wide.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;
use arcdrv::port::{inb, outb};
use arcdrv::{DeviceInfo, DisplayDesc, DisplayMode, Framebuffer, Pci, Status, E_INVAL, E_IO, E_NODEV, OK, RES_MEMORY};
use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, Ordering};

arcdrv::driver! {
    name: "cirrus",
    version: "1.1.0",
    probe: probe,
}

const ATTR: u16 = 0x3C0;
const SEQ_INDEX: u16 = 0x3C4;
const SEQ_DATA: u16 = 0x3C5;
const DAC_MASK: u16 = 0x3C6;
const DAC_WRITE_INDEX: u16 = 0x3C8;
const GFX_INDEX: u16 = 0x3CE;
const GFX_DATA: u16 = 0x3CF;
const CRT_INDEX: u16 = 0x3D4;
const CRT_DATA: u16 = 0x3D5;
const INPUT_STATUS: u16 = 0x3DA;

/// The longest line the offset register can describe.
const MAX_PITCH: u32 = 0x1FF << 3;

const MODES: &[(u32, u32)] = &[(640, 480), (800, 600), (1024, 768), (1280, 720), (1280, 1024)];

struct Cirrus {
    fb_phys: u64,
    vram: u64,
    /// The mode set (width, height; 0 before the first).
    mode: (AtomicU32, AtomicU32),
}

const MISC_WRITE: u16 = 0x3C2;

fn seq(reg: u8, v: u8) {
    outb(SEQ_INDEX, reg);
    outb(SEQ_DATA, v);
}
fn seq_read(reg: u8) -> u8 {
    outb(SEQ_INDEX, reg);
    inb(SEQ_DATA)
}
fn gfx(reg: u8, v: u8) {
    outb(GFX_INDEX, reg);
    outb(GFX_DATA, v);
}
fn crt(reg: u8, v: u8) {
    outb(CRT_INDEX, reg);
    outb(CRT_DATA, v);
}
fn crt_read(reg: u8) -> u8 {
    outb(CRT_INDEX, reg);
    inb(CRT_DATA)
}

/// The hidden DAC register: reached by reading the pixel mask four times.
fn hidden_dac(v: u8) {
    inb(DAC_WRITE_INDEX);
    for _ in 0..4 {
        inb(DAC_MASK);
    }
    outb(DAC_MASK, v);
}

impl Cirrus {
    fn fits(&self, w: u32, h: u32) -> bool {
        w * 4 <= MAX_PITCH && w as u64 * h as u64 * 4 <= self.vram
    }

    fn program(&self, w: u32, h: u32) {
        // Timings: only the visible area matters to a virtual display, the
        // blanking and sync values just have to be consistent.
        let htotal = (w + 160) / 8 - 5;
        let hdisp = w / 8 - 1;
        let hsync_start = (w + 24) / 8 + 1;
        let hsync_end = (w + 120) / 8 + 1;
        let vtotal = h + 30 - 2;
        let vdisp = h - 1;

        // The plain VGA side of a graphics mode, for a chip just out of
        // reset (after sleep, when the firmware did not set it up): colour
        // addressing (the CRTC at 0x3D4), screen on, all planes, chained
        // linear memory, the attribute controller in graphics mode.
        outb(MISC_WRITE, 0xE3);
        seq(0x01, 0x01);
        seq(0x02, 0x0F);
        seq(0x04, 0x0E);
        inb(INPUT_STATUS);
        for (i, v) in [(0x10u8, 0x41u8), (0x11, 0), (0x12, 0x0F), (0x13, 0), (0x14, 0)] {
            outb(ATTR, i);
            outb(ATTR, v);
        }

        // Unlock the extension registers, then CRTC 0-7.
        seq(0x06, 0x12);
        crt(0x11, 0x20);
        crt(0x00, htotal as u8);
        crt(0x01, hdisp as u8);
        crt(0x04, hsync_start as u8);
        crt(0x05, hsync_end as u8);
        crt(0x06, vtotal as u8);
        crt(0x12, vdisp as u8);
        // Maximum scan line: one scan line per row; bit 6 is bit 9 of the
        // line compare, bit 5 bit 9 of the vertical blank start.
        let mut max_scan = 0x40;
        if (vdisp + 1) & 0x200 != 0 {
            max_scan |= 0x20;
        }
        crt(0x09, max_scan);
        let mut overflow = 0x10; // bit 8 of the line compare
        if vtotal & 0x100 != 0 {
            overflow |= 0x01;
        }
        if vdisp & 0x100 != 0 {
            overflow |= 0x02;
        }
        if (vdisp + 1) & 0x100 != 0 {
            overflow |= 0x08;
        }
        if vtotal & 0x200 != 0 {
            overflow |= 0x20;
        }
        if vdisp & 0x200 != 0 {
            overflow |= 0x40;
        }
        crt(0x07, overflow);
        crt(0x18, 0xFF); // line compare at the end: no split screen
        let mut ext = 0;
        if (htotal + 5) & 0x40 != 0 {
            ext |= 0x10;
        }
        if (htotal + 5) & 0x80 != 0 {
            ext |= 0x20;
        }
        if vtotal & 0x100 != 0 {
            ext |= 0x40;
        }
        if vtotal & 0x200 != 0 {
            ext |= 0x80;
        }
        crt(0x1A, ext);
        crt(0x17, 0x03); // no CGA/Hercules addressing

        // 32-bit colour: extended sequencer mode, high-colour graphics and
        // the hidden DAC in 32-bit mode.
        let sr07 = (seq_read(0x07) & 0xE0) | 0x19;
        seq(0x07, sr07);
        gfx(0x05, 0x40);
        gfx(0x06, 0x01);
        hidden_dac(0xC5);

        // Pitch in units of 8 bytes, bit 8 in CR1B; start address 0.
        let offset = w * 4 / 8;
        crt(0x13, offset as u8);
        crt(0x1B, 0x22 | ((offset >> 4) & 0x10) as u8);
        crt(0x0C, 0);
        crt(0x0D, 0);
        let cr1d = crt_read(0x1D) & 0x7F;
        crt(0x1D, cr1d);

        // Turn the picture back on (the attribute controller blanks it while
        // its index register is being written).
        inb(INPUT_STATUS);
        outb(ATTR, 0x20);
    }
}

unsafe extern "C" fn modes(ctx: *mut c_void, out: *mut DisplayMode, max: usize) -> usize {
    // SAFETY: ctx is the leaked Cirrus; out holds max modes.
    let c = unsafe { &*(ctx as *const Cirrus) };
    let list: Vec<DisplayMode> =
        MODES.iter().filter(|&&(w, h)| c.fits(w, h)).map(|&(width, height)| DisplayMode { width, height }).collect();
    let n = list.len().min(max);
    for (i, m) in list.iter().take(n).enumerate() {
        // SAFETY: i < max.
        unsafe { out.add(i).write(*m) };
    }
    n
}

unsafe extern "C" fn set_mode(ctx: *mut c_void, mode: DisplayMode, fb: *mut Framebuffer) -> Status {
    // SAFETY: ctx is the leaked Cirrus; fb is valid for the call.
    let c = unsafe { &*(ctx as *const Cirrus) };
    if !c.fits(mode.width, mode.height) || !mode.width.is_multiple_of(8) {
        return E_INVAL;
    }
    c.program(mode.width, mode.height);
    if crt_read(0x01) as u32 != mode.width / 8 - 1 {
        return E_IO;
    }
    c.mode.0.store(mode.width, Ordering::Relaxed);
    c.mode.1.store(mode.height, Ordering::Relaxed);
    // SAFETY: as above.
    unsafe {
        *fb = Framebuffer {
            phys: c.fb_phys,
            virt: 0,
            width: mode.width,
            height: mode.height,
            pitch: mode.width * 4,
            red: 16,
            green: 8,
            blue: 0,
            _pad: 0,
        };
    }
    OK
}

/// After sleep the chip lost its mode: the same one again (the picture in
/// video memory is lost too; the interface draws it anew).
unsafe extern "C" fn resume(ctx: *mut c_void) -> Status {
    // SAFETY: ctx is the leaked Cirrus.
    let c = unsafe { &*(ctx as *const Cirrus) };
    let (w, h) = (c.mode.0.load(Ordering::Relaxed), c.mode.1.load(Ordering::Relaxed));
    if w == 0 {
        return OK; // still in the firmware's mode
    }
    c.program(w, h);
    if crt_read(0x01) as u32 != w / 8 - 1 {
        return E_IO;
    }
    OK
}

fn probe(dev: &DeviceInfo) -> Status {
    let bar0 = dev.resources[0];
    if bar0.kind != RES_MEMORY || bar0.base == 0 {
        return E_NODEV;
    }
    Pci(dev.id).enable(true, true, false);
    // The chip identifies itself in CR27 (0xB8 is the GD5446).
    seq(0x06, 0x12);
    let id = crt_read(0x27);
    if id & 0xFC != 0xB8 {
        arcdrv::info!("not a GD5446 (id {id:#x})");
        return E_NODEV;
    }
    // SR0F bit 7: 4 MiB of video memory, else 2 MiB.
    let vram: u64 = if seq_read(0x0F) & 0x80 != 0 { 4 << 20 } else { 2 << 20 };
    let c = Cirrus { fb_phys: bar0.base, vram: vram.min(bar0.size), mode: (AtomicU32::new(0), AtomicU32::new(0)) };
    arcdrv::info!("Cirrus GD5446: {} MiB of video memory", c.vram >> 20);
    let c: &'static Cirrus = Box::leak(Box::new(c));
    let desc: &'static DisplayDesc = Box::leak(Box::new(DisplayDesc {
        ctx: c as *const Cirrus as *mut c_void,
        modes,
        set_mode,
        flush: None,
        name: arcdrv::model_name("Cirrus Logic GD5446"),
    }));
    let n = arcdrv::display_register(dev.id, desc);
    if n < 0 {
        return n;
    }
    arcdrv::power_hooks(dev.id, c as *const Cirrus as *mut c_void, None, resume);
    OK
}

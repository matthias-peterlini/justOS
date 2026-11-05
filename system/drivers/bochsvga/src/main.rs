//! Display driver for the Bochs/QEMU "DISPI" interface: QEMU's standard VGA
//! and bochs-display, QXL (QEMU with SPICE), Bochs, and VirtualBox's VBoxVGA
//! and VBoxSVGA.
//!
//! Its registers (resolution, depth, enable) are reached through the I/O
//! ports 0x1CE/0x1CF or, on newer QEMU devices, in the second memory BAR.
//! The framebuffer is the first BAR; any mode that fits in video memory
//! works.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;
use justdrv::port::{inw, outw};
use justdrv::{DeviceInfo, DisplayDesc, DisplayMode, Framebuffer, Mmio, Pci, Status, E_INVAL, E_IO, E_NODEV, OK, RES_MEMORY};
use core::ffi::c_void;

justdrv::driver! {
    name: "bochsvga",
    version: "1.1.0",
    probe: probe,
}

const INDEX_PORT: u16 = 0x1CE;
const DATA_PORT: u16 = 0x1CF;
/// The registers in the MMIO BAR.
const MMIO_DISPI: usize = 0x500;

const ID: u16 = 0;
const XRES: u16 = 1;
const YRES: u16 = 2;
const BPP: u16 = 3;
const ENABLE: u16 = 4;
const VIRT_WIDTH: u16 = 6;
const X_OFFSET: u16 = 8;
const Y_OFFSET: u16 = 9;
const VIDEO_MEMORY_64K: u16 = 0xA;

const ENABLED: u16 = 1 << 0;
const GET_CAPS: u16 = 1 << 1;
const LINEAR: u16 = 1 << 6;

/// Resolutions offered when they fit.
const MODES: &[(u32, u32)] = &[
    (800, 600),
    (1024, 768),
    (1152, 864),
    (1280, 720),
    (1280, 800),
    (1280, 1024),
    (1366, 768),
    (1440, 900),
    (1600, 900),
    (1600, 1200),
    (1680, 1050),
    (1920, 1080),
    (1920, 1200),
    (2560, 1440),
    (2560, 1600),
    (3840, 2160),
];

enum Regs {
    Ports,
    Mem(Mmio),
}

struct Dispi {
    regs: Regs,
    fb_phys: u64,
    vram: u64,
    max: (u32, u32),
    /// The mode registers while the machine sleeps.
    saved: justdrv::IrqLock<[u16; 7]>,
}

/// The registers that make up the mode, in the order they are written back.
const MODE_REGS: [u16; 7] = [XRES, YRES, BPP, VIRT_WIDTH, X_OFFSET, Y_OFFSET, ENABLE];
/// ENABLE: keep the picture in video memory when the mode is set.
const NO_CLEAR: u16 = 1 << 7;

/// Before sleep: remembers the mode.
unsafe extern "C" fn suspend(ctx: *mut c_void) -> Status {
    // SAFETY: ctx is the leaked Dispi.
    let d = unsafe { &*(ctx as *const Dispi) };
    let mut s = d.saved.lock();
    for (v, &r) in s.iter_mut().zip(MODE_REGS.iter()) {
        *v = d.read(r);
    }
    OK
}

/// After sleep: the controller is back in text mode; the same mode again.
unsafe extern "C" fn resume(ctx: *mut c_void) -> Status {
    // SAFETY: ctx is the leaked Dispi.
    let d = unsafe { &*(ctx as *const Dispi) };
    let s = *d.saved.lock();
    d.write(ENABLE, 0);
    for (&v, &r) in s.iter().zip(MODE_REGS.iter()) {
        d.write(r, if r == ENABLE { (v & !GET_CAPS) | if v & ENABLED != 0 { NO_CLEAR } else { 0 } } else { v });
    }
    if d.read(XRES) == s[0] && d.read(YRES) == s[1] {
        OK
    } else {
        E_IO
    }
}

impl Dispi {
    fn read(&self, reg: u16) -> u16 {
        match &self.regs {
            Regs::Ports => {
                outw(INDEX_PORT, reg);
                inw(DATA_PORT)
            }
            Regs::Mem(m) => m.read16(MMIO_DISPI + reg as usize * 2),
        }
    }

    fn write(&self, reg: u16, v: u16) {
        match &self.regs {
            Regs::Ports => {
                outw(INDEX_PORT, reg);
                outw(DATA_PORT, v);
            }
            Regs::Mem(m) => m.write16(MMIO_DISPI + reg as usize * 2, v),
        }
    }

    fn fits(&self, w: u32, h: u32) -> bool {
        w <= self.max.0 && h <= self.max.1 && w as u64 * h as u64 * 4 <= self.vram
    }
}

unsafe extern "C" fn modes(ctx: *mut c_void, out: *mut DisplayMode, max: usize) -> usize {
    // SAFETY: ctx is the leaked Dispi; out holds max modes.
    let d = unsafe { &*(ctx as *const Dispi) };
    let list: Vec<DisplayMode> =
        MODES.iter().filter(|&&(w, h)| d.fits(w, h)).map(|&(width, height)| DisplayMode { width, height }).collect();
    let n = list.len().min(max);
    for (i, m) in list.iter().take(n).enumerate() {
        // SAFETY: i < max.
        unsafe { out.add(i).write(*m) };
    }
    n
}

unsafe extern "C" fn set_mode(ctx: *mut c_void, mode: DisplayMode, fb: *mut Framebuffer) -> Status {
    // SAFETY: ctx is the leaked Dispi; fb is valid for the call.
    let d = unsafe { &*(ctx as *const Dispi) };
    if !d.fits(mode.width, mode.height) || mode.width > 0xFFFF || mode.height > 0xFFFF {
        return E_INVAL;
    }
    d.write(ENABLE, 0);
    d.write(XRES, mode.width as u16);
    d.write(YRES, mode.height as u16);
    d.write(BPP, 32);
    d.write(VIRT_WIDTH, mode.width as u16);
    d.write(X_OFFSET, 0);
    d.write(Y_OFFSET, 0);
    d.write(ENABLE, ENABLED | LINEAR);
    if d.read(XRES) as u32 != mode.width || d.read(YRES) as u32 != mode.height || d.read(BPP) != 32 {
        return E_IO;
    }
    let pitch = (d.read(VIRT_WIDTH) as u32).max(mode.width) * 4;
    // SAFETY: as above.
    unsafe {
        *fb = Framebuffer { phys: d.fb_phys, virt: 0, width: mode.width, height: mode.height, pitch, red: 16, green: 8, blue: 0, _pad: 0 };
    }
    OK
}

fn probe(dev: &DeviceInfo) -> Status {
    let bar0 = dev.resources[0];
    if bar0.kind != RES_MEMORY || bar0.base == 0 {
        return E_NODEV;
    }
    let pci = Pci(dev.id);
    // QEMU's VGA and bochs-display have the registers in memory too (BAR 2);
    // the others only on the VGA ports (on QXL, BAR 2 is its ROM).
    let mmio = dev.resources[2];
    let regs = if dev.vendor == 0x1234 && mmio.kind == RES_MEMORY && mmio.base != 0 && mmio.size >= 0x1000 {
        pci.enable(true, true, false);
        match Mmio::map(mmio.base, 0x1000) {
            Some(m) => Regs::Mem(m),
            None => Regs::Ports,
        }
    } else {
        pci.enable(true, true, false);
        Regs::Ports
    };
    let mut d = Dispi { regs, fb_phys: bar0.base, vram: bar0.size, max: (0, 0), saved: justdrv::IrqLock::new([0; 7]) };
    let id = d.read(ID);
    if !(0xB0C0..=0xB0CF).contains(&id) {
        justdrv::info!("no DISPI interface (id {id:#x})");
        return E_NODEV;
    }
    if id >= 0xB0C5 {
        let reported = d.read(VIDEO_MEMORY_64K) as u64 * 64 * 1024;
        if reported != 0 {
            d.vram = d.vram.min(reported);
        }
    }
    // The largest resolution: read with GET_CAPS, keeping the current mode.
    let enable = d.read(ENABLE);
    d.write(ENABLE, enable | GET_CAPS);
    d.max = (d.read(XRES) as u32, d.read(YRES) as u32);
    d.write(ENABLE, enable);
    if d.max.0 < 640 {
        d.max = (2560, 1600); // older interface without the capability query
    }
    let name = match dev.vendor {
        0x80EE => "VirtualBox display",
        0x1B36 => "QXL display",
        _ if matches!(d.regs, Regs::Mem(_)) => "QEMU display",
        _ => "Bochs display",
    };
    justdrv::info!("{name}: {} MiB of video memory, up to {}x{}", d.vram >> 20, d.max.0, d.max.1);
    let d: &'static Dispi = Box::leak(Box::new(d));
    let desc: &'static DisplayDesc = Box::leak(Box::new(DisplayDesc {
        ctx: d as *const Dispi as *mut c_void,
        modes,
        set_mode,
        flush: None,
        name: justdrv::model_name(name),
    }));
    let n = justdrv::display_register(dev.id, desc);
    if n < 0 {
        return n;
    }
    justdrv::power_hooks(dev.id, d as *const Dispi as *mut c_void, Some(suspend), resume);
    OK
}

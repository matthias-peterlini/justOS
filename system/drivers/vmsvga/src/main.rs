//! VMware SVGA II display driver: VMware, VirtualBox's VMSVGA and QEMU's
//! "vmware" display.
//!
//! The device has an index/value register pair in I/O space, its
//! framebuffer in the second BAR and a command FIFO in the third. JustOS
//! draws into the framebuffer and tells the device what changed with an
//! UPDATE command in the FIFO.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;
use justdrv::port::{inl, outl};
use justdrv::{DeviceInfo, DisplayDesc, DisplayMode, Framebuffer, Mmio, Mutex, Pci, Status, E_INVAL, E_IO, E_NODEV, E_NOMEM, OK};
use core::ffi::c_void;

justdrv::driver! {
    name: "vmsvga",
    version: "1.1.0",
    probe: probe,
}

const SVGA_ID_2: u32 = 0x9000_0002;

const REG_ID: u32 = 0;
const REG_ENABLE: u32 = 1;
const REG_WIDTH: u32 = 2;
const REG_HEIGHT: u32 = 3;
const REG_MAX_WIDTH: u32 = 4;
const REG_MAX_HEIGHT: u32 = 5;
const REG_BITS_PER_PIXEL: u32 = 7;
const REG_RED_MASK: u32 = 9;
const REG_GREEN_MASK: u32 = 10;
const REG_BLUE_MASK: u32 = 11;
const REG_BYTES_PER_LINE: u32 = 12;
const REG_FB_START: u32 = 13;
const REG_FB_OFFSET: u32 = 14;
const REG_VRAM_SIZE: u32 = 15;
const REG_MEM_START: u32 = 18;
const REG_MEM_SIZE: u32 = 19;
const REG_CONFIG_DONE: u32 = 20;
const REG_SYNC: u32 = 21;
const REG_BUSY: u32 = 22;

// FIFO registers (32-bit words at the start of the FIFO memory).
const FIFO_MIN: usize = 0;
const FIFO_MAX: usize = 1;
const FIFO_NEXT_CMD: usize = 2;
const FIFO_STOP: usize = 3;
/// Bytes of FIFO registers this driver uses; commands follow.
const FIFO_REGS: u32 = 16;

const CMD_UPDATE: u32 = 1;

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
];

struct Svga {
    port: u16,
    fifo: Mmio,
    fifo_size: u32,
    fb_start: u64,
    vram: u64,
    max: (u32, u32),
    /// Serialises register and FIFO access; holds the mode set (width,
    /// height; 0 before the first).
    lock: Mutex<(u32, u32)>,
}

/// Agrees on version II of the interface.
fn negotiate(port: u16) -> bool {
    outl(port, REG_ID);
    outl(port + 1, SVGA_ID_2);
    outl(port, REG_ID);
    inl(port + 1) == SVGA_ID_2
}

impl Svga {
    /// An empty FIFO, then the device told it is set up.
    fn init_fifo(&self) {
        self.set_fifo_reg(FIFO_MIN, FIFO_REGS);
        self.set_fifo_reg(FIFO_MAX, self.fifo_size);
        self.set_fifo_reg(FIFO_NEXT_CMD, FIFO_REGS);
        self.set_fifo_reg(FIFO_STOP, FIFO_REGS);
        self.write(REG_CONFIG_DONE, 1);
    }

    fn program(&self, w: u32, h: u32) -> bool {
        self.write(REG_WIDTH, w);
        self.write(REG_HEIGHT, h);
        self.write(REG_BITS_PER_PIXEL, 32);
        self.write(REG_ENABLE, 1);
        self.read(REG_WIDTH) == w && self.read(REG_HEIGHT) == h
    }

    fn read(&self, reg: u32) -> u32 {
        outl(self.port, reg);
        inl(self.port + 1)
    }

    fn write(&self, reg: u32, v: u32) {
        outl(self.port, reg);
        outl(self.port + 1, v);
    }

    fn fifo_reg(&self, i: usize) -> u32 {
        self.fifo.read32(i * 4)
    }

    fn set_fifo_reg(&self, i: usize, v: u32) {
        self.fifo.write32(i * 4, v);
    }

    /// Waits until the device has processed the whole FIFO.
    fn sync(&self) {
        self.write(REG_SYNC, 1);
        justdrv::wait_until(100, || self.read(REG_BUSY) == 0);
    }

    /// Appends one command (a list of words) to the FIFO.
    fn command(&self, words: &[u32]) {
        let (min, max) = (self.fifo_reg(FIFO_MIN), self.fifo_reg(FIFO_MAX));
        for &w in words {
            let next = self.fifo_reg(FIFO_NEXT_CMD);
            let mut after = next + 4;
            if after >= max {
                after = min;
            }
            if after == self.fifo_reg(FIFO_STOP) {
                self.sync(); // full: let the device catch up
            }
            self.fifo.write32(next as usize, w);
            self.set_fifo_reg(FIFO_NEXT_CMD, after);
        }
    }

    fn fits(&self, w: u32, h: u32) -> bool {
        w <= self.max.0 && h <= self.max.1 && w as u64 * h as u64 * 4 <= self.vram
    }
}

fn shift(mask: u32) -> u8 {
    if mask == 0 {
        0
    } else {
        mask.trailing_zeros() as u8
    }
}

unsafe extern "C" fn modes(ctx: *mut c_void, out: *mut DisplayMode, max: usize) -> usize {
    // SAFETY: ctx is the leaked Svga; out holds max modes.
    let s = unsafe { &*(ctx as *const Svga) };
    let list: Vec<DisplayMode> =
        MODES.iter().filter(|&&(w, h)| s.fits(w, h)).map(|&(width, height)| DisplayMode { width, height }).collect();
    let n = list.len().min(max);
    for (i, m) in list.iter().take(n).enumerate() {
        // SAFETY: i < max.
        unsafe { out.add(i).write(*m) };
    }
    n
}

unsafe extern "C" fn set_mode(ctx: *mut c_void, mode: DisplayMode, fb: *mut Framebuffer) -> Status {
    // SAFETY: ctx is the leaked Svga; fb is valid for the call.
    let s = unsafe { &*(ctx as *const Svga) };
    if !s.fits(mode.width, mode.height) {
        return E_INVAL;
    }
    let mut current = s.lock.lock();
    if !s.program(mode.width, mode.height) {
        return E_IO;
    }
    *current = (mode.width, mode.height);
    let pitch = s.read(REG_BYTES_PER_LINE);
    let offset = s.read(REG_FB_OFFSET) as u64;
    // SAFETY: as above.
    unsafe {
        *fb = Framebuffer {
            phys: s.fb_start + offset,
            virt: 0,
            width: mode.width,
            height: mode.height,
            pitch,
            red: shift(s.read(REG_RED_MASK)),
            green: shift(s.read(REG_GREEN_MASK)),
            blue: shift(s.read(REG_BLUE_MASK)),
            _pad: 0,
        };
    }
    OK
}

unsafe extern "C" fn flush(ctx: *mut c_void, x: u32, y: u32, w: u32, h: u32) {
    // SAFETY: ctx is the leaked Svga.
    let s = unsafe { &*(ctx as *const Svga) };
    let _l = s.lock.lock();
    s.command(&[CMD_UPDATE, x, y, w, h]);
}

/// Before sleep: the device finishes the commands in its FIFO.
unsafe extern "C" fn suspend(ctx: *mut c_void) -> Status {
    // SAFETY: ctx is the leaked Svga.
    let s = unsafe { &*(ctx as *const Svga) };
    let _l = s.lock.lock();
    s.sync();
    OK
}

/// After sleep the device starts over: interface, FIFO, mode.
unsafe extern "C" fn resume(ctx: *mut c_void) -> Status {
    // SAFETY: ctx is the leaked Svga.
    let s = unsafe { &*(ctx as *const Svga) };
    let current = s.lock.lock();
    if !negotiate(s.port) {
        return E_IO;
    }
    s.init_fifo();
    let (w, h) = *current;
    if w != 0 && !s.program(w, h) {
        return E_IO;
    }
    OK
}

fn probe(dev: &DeviceInfo) -> Status {
    let (Some(port), fb, fifo) = (dev.io(0), dev.resources[1], dev.resources[2]) else { return E_NODEV };
    Pci(dev.id).enable(true, true, false);
    if !negotiate(port) {
        justdrv::info!("the device does not speak SVGA II");
        return E_NODEV;
    }
    let read = |reg: u32| {
        outl(port, reg);
        inl(port + 1)
    };
    let fb_start = match read(REG_FB_START) {
        0 => fb.base,
        v => v as u64,
    };
    let fifo_start = match read(REG_MEM_START) {
        0 => fifo.base,
        v => v as u64,
    };
    let fifo_size = read(REG_MEM_SIZE).max(64 * 1024).min(fifo.size.max(64 * 1024) as u32);
    let Some(fifo_map) = Mmio::map(fifo_start, fifo_size as usize) else { return E_NOMEM };
    let s = Svga {
        port,
        fifo: fifo_map,
        fifo_size,
        fb_start,
        vram: read(REG_VRAM_SIZE) as u64,
        max: (read(REG_MAX_WIDTH), read(REG_MAX_HEIGHT)),
        lock: Mutex::new((0, 0)),
    };
    s.init_fifo();
    justdrv::info!("{} MiB of video memory, up to {}x{}", s.vram >> 20, s.max.0, s.max.1);
    let s: &'static Svga = Box::leak(Box::new(s));
    let desc: &'static DisplayDesc = Box::leak(Box::new(DisplayDesc {
        ctx: s as *const Svga as *mut c_void,
        modes,
        set_mode,
        flush: Some(flush),
        name: justdrv::model_name("VMware SVGA II display"),
    }));
    let n = justdrv::display_register(dev.id, desc);
    if n < 0 {
        return n;
    }
    justdrv::power_hooks(dev.id, s as *const Svga as *mut c_void, Some(suspend), resume);
    OK
}

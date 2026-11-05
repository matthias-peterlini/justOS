//! AC'97 audio driver (Intel ICH and compatible bus masters): plays 48 kHz,
//! 16-bit stereo sound through the codec's main output.
//!
//! The controller has two register windows: the codec's mixer (volumes)
//! and the bus master, which plays a list of 32 buffers in a loop. The list
//! here points at the four periods of one ring, eight times over, and its
//! last valid entry is kept one behind the current one so it never stops.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use justdrv::pcm::{Hardware, PcmRing};
use justdrv::port::{inb, inw, outb, outl, outw};
use justdrv::{AudioDesc, DeviceInfo, Dma, Pci, Status, E_IO, E_NODEV, E_NOMEM, OK};
use core::ffi::c_void;

justdrv::driver! {
    name: "ac97",
    version: "1.1.0",
    probe: probe,
}

// Mixer (NAMBAR)
const MIX_RESET: u16 = 0x00;
const MIX_MASTER: u16 = 0x02;
const MIX_HEADPHONE: u16 = 0x04;
const MIX_PCM_OUT: u16 = 0x18;
const MIX_EXT_ID: u16 = 0x28;
const MIX_EXT_CTRL: u16 = 0x2A;
const MIX_FRONT_RATE: u16 = 0x2C;

// Bus master (NABMBAR): the PCM out box and global registers.
const PO: u16 = 0x10;
const BDBAR: u16 = 0x00;
const CIV: u16 = 0x04;
const LVI: u16 = 0x05;
const SR: u16 = 0x06;
const PICB: u16 = 0x08;
const CR: u16 = 0x0B;
const GLOB_CNT: u16 = 0x2C;
const GLOB_STA: u16 = 0x30;

const CR_RUN: u8 = 1 << 0;
const CR_RESET: u8 = 1 << 1;
const CR_IOCE: u8 = 1 << 4;

const ENTRIES: usize = 32;
const PERIODS: usize = 4;
const PERIOD_BYTES: usize = 4096 * 4;
const RATE: u32 = 48_000;

struct Box97 {
    bm: u16,
    /// Physical address of the buffer list.
    bdl: u32,
}

impl Box97 {
    fn keep_running(&self) {
        let civ = inb(self.bm + PO + CIV);
        outb(self.bm + PO + LVI, (civ.wrapping_add(ENTRIES as u8 - 1)) % ENTRIES as u8);
    }

    fn reset_box(&self) {
        outb(self.bm + PO + CR, 0);
        outb(self.bm + PO + CR, CR_RESET);
        justdrv::wait_until(10, || inb(self.bm + PO + CR) & CR_RESET == 0);
    }
}

impl Hardware for Box97 {
    fn position(&self) -> usize {
        self.keep_running();
        let civ = inb(self.bm + PO + CIV) as usize;
        let left = inw(self.bm + PO + PICB) as usize * 2;
        (civ % PERIODS) * PERIOD_BYTES + PERIOD_BYTES.saturating_sub(left).min(PERIOD_BYTES - 1)
    }

    fn start(&self) {
        self.reset_box();
        outl(self.bm + PO + BDBAR, self.bdl);
        self.keep_running();
        outw(self.bm + PO + SR, 0x1C);
        outb(self.bm + PO + CR, CR_RUN | CR_IOCE);
    }

    fn stop(&self) {
        self.reset_box();
    }
}

struct Device {
    ring: PcmRing<Box97>,
    mixer: u16,
}

/// The codec out of cold reset, its outputs at full volume and 48 kHz.
fn init_codec(mixer: u16, bm: u16) -> Status {
    let glob = justdrv::port::inl(bm + GLOB_CNT);
    outl(bm + GLOB_CNT, (glob | 2) & !(1 << 2));
    if !justdrv::wait_until(1000, || justdrv::port::inl(bm + GLOB_STA) & (1 << 8) != 0) {
        justdrv::warn!("the codec is not ready");
        return E_IO;
    }
    outw(mixer + MIX_RESET, 1);
    justdrv::sleep_ms(10);
    outw(mixer + MIX_MASTER, 0x0000); // 0 dB, unmuted
    outw(mixer + MIX_HEADPHONE, 0x0000);
    outw(mixer + MIX_PCM_OUT, 0x0808); // 0 dB
    if inw(mixer + MIX_EXT_ID) & 1 != 0 {
        // Variable rate: make sure it is 48 kHz.
        outw(mixer + MIX_EXT_CTRL, inw(mixer + MIX_EXT_CTRL) | 1);
        outw(mixer + MIX_FRONT_RATE, RATE as u16);
    }
    OK
}

/// Before sleep: playing stops.
unsafe extern "C" fn suspend(ctx: *mut c_void) -> Status {
    // SAFETY: ctx is the leaked Device.
    unsafe { &*(ctx as *const Device) }.ring.pause();
    OK
}

/// After sleep: the codec set up again; the next sound plays.
unsafe extern "C" fn resume(ctx: *mut c_void) -> Status {
    // SAFETY: ctx is the leaked Device.
    let d = unsafe { &*(ctx as *const Device) };
    let st = init_codec(d.mixer, d.ring.hw.bm);
    d.ring.hw.reset_box();
    d.ring.unpause();
    st
}

unsafe extern "C" fn write(ctx: *mut c_void, samples: *const i16, frames: usize) -> Status {
    // SAFETY: ctx is the leaked Device; samples hold frames * 2 values.
    let d = unsafe { &*(ctx as *const Device) };
    d.ring.write(unsafe { core::slice::from_raw_parts(samples as *const u8, frames * 4) });
    OK
}

unsafe extern "C" fn drain(ctx: *mut c_void) -> Status {
    // SAFETY: ctx is the leaked Device.
    unsafe { &*(ctx as *const Device) }.ring.drain();
    OK
}

unsafe extern "C" fn irq(ctx: *mut c_void) {
    // SAFETY: ctx is the leaked Device.
    let d = unsafe { &*(ctx as *const Device) };
    let hw = &d.ring.hw;
    let sr = inw(hw.bm + PO + SR);
    if sr & 0x1C != 0 {
        outw(hw.bm + PO + SR, sr & 0x1C);
        hw.keep_running();
        d.ring.progress.signal();
    }
}

fn probe(dev: &DeviceInfo) -> Status {
    let (Some(mixer), Some(bm)) = (dev.io(0), dev.io(1)) else { return E_NODEV };
    Pci(dev.id).enable(true, false, true);
    let st = init_codec(mixer, bm);
    if st != OK {
        return st;
    }

    let (Some(buf), Some(bdl)) = (Dma::new(PERIODS * PERIOD_BYTES), Dma::new(ENTRIES * 8)) else { return E_NOMEM };
    for i in 0..ENTRIES {
        let p = i % PERIODS;
        bdl.write::<u32>(i * 8, (buf.phys() + (p * PERIOD_BYTES) as u64) as u32);
        // Length in 16-bit samples; interrupt on completion.
        bdl.write::<u16>(i * 8 + 4, (PERIOD_BYTES / 2) as u16);
        bdl.write::<u16>(i * 8 + 6, 1 << 15);
    }
    let hw = Box97 { bm, bdl: bdl.phys() as u32 };
    // The list must outlive the device.
    core::mem::forget(bdl);
    hw.reset_box();
    let d: &'static Device = Box::leak(Box::new(Device { ring: PcmRing::new(hw, buf), mixer }));
    let ctx = d as *const Device as *mut c_void;
    justdrv::irq_attach_any(dev, irq, ctx);
    let desc: &'static AudioDesc =
        Box::leak(Box::new(AudioDesc { ctx, rate: RATE, channels: 2, write, drain, name: justdrv::model_name("AC'97 audio") }));
    let n = justdrv::audio_register(dev.id, desc);
    if n < 0 {
        return n;
    }
    justdrv::power_hooks(dev.id, ctx, Some(suspend), resume);
    OK
}

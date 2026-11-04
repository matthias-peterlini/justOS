//! Intel High Definition Audio driver: plays sound through the analog
//! outputs of the first codec that has them.
//!
//! The controller talks to its codecs through two rings in memory (CORB for
//! commands, RIRB for answers). The driver walks the codec's widgets, finds
//! the output pins (speakers, headphones, line out) and, for each, a path
//! back to a digital-to-analog converter through mixers and selectors. Every
//! amplifier on the way is unmuted at 0 dB and every converter listens to
//! one output stream: 48 kHz, 16-bit stereo, played from a ring of four
//! periods.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::format;
use alloc::vec::Vec;
use arcdrv::pcm::{Hardware, PcmRing};
use arcdrv::{AudioDesc, DeviceInfo, Dma, Mmio, Pci, Status, E_IO, E_NODEV, E_NOMEM, OK, RES_MEMORY};
use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, Ordering};

arcdrv::driver! {
    name: "hda",
    version: "1.1.0",
    probe: probe,
}

const GCAP: usize = 0x00;
const GCTL: usize = 0x08;
const STATESTS: usize = 0x0E;
const INTCTL: usize = 0x20;
const INTSTS: usize = 0x24;
const CORBLBASE: usize = 0x40;
const CORBWP: usize = 0x48;
const CORBRP: usize = 0x4A;
const CORBCTL: usize = 0x4C;
const CORBSIZE: usize = 0x4E;
const RIRBLBASE: usize = 0x50;
const RIRBWP: usize = 0x58;
const RINTCNT: usize = 0x5A;
const RIRBCTL: usize = 0x5C;
const RIRBSTS: usize = 0x5D;
const RIRBSIZE: usize = 0x5E;
const ICOI: usize = 0x60;
const ICII: usize = 0x64;
const ICIS: usize = 0x68;

// Stream descriptor registers, from the descriptor's base.
const SD_CTL: usize = 0x00;
const SD_STS: usize = 0x03;
const SD_LPIB: usize = 0x04;
const SD_CBL: usize = 0x08;
const SD_LVI: usize = 0x0C;
const SD_FMT: usize = 0x12;
const SD_BDPL: usize = 0x18;
const SD_BDPU: usize = 0x1C;

const CTL_RESET: u32 = 1 << 0;
const CTL_RUN: u32 = 1 << 1;
const CTL_IOCE: u32 = 1 << 2;
const STS_BCIS: u8 = 1 << 2;

// Verbs
const GET_PARAMETER: u32 = 0xF00;
const GET_CONNECTION: u32 = 0xF02;
const GET_PIN_SENSE: u32 = 0xF09;
const GET_CONFIG_DEFAULT: u32 = 0xF1C;
const SET_SELECT: u32 = 0x701;
const SET_POWER: u32 = 0x705;
const SET_STREAM: u32 = 0x706;
const SET_PIN_CONTROL: u32 = 0x707;
const SET_EAPD: u32 = 0x70C;
/// 4-bit verbs, with a 16-bit payload.
const SET_FORMAT: u32 = 0x2;
const SET_AMP: u32 = 0x3;

// Parameters
const P_NODE_COUNT: u32 = 0x04;
const P_FUNCTION_TYPE: u32 = 0x05;
const P_WIDGET_CAPS: u32 = 0x09;
const P_PIN_CAPS: u32 = 0x0C;
const P_CONNECTIONS: u32 = 0x0E;
const P_OUT_AMP: u32 = 0x12;
const P_IN_AMP: u32 = 0x0D;
const P_VENDOR: u32 = 0x00;

const W_OUTPUT: u32 = 0;
const W_MIXER: u32 = 2;
const W_SELECTOR: u32 = 3;
const W_PIN: u32 = 4;

/// 48 kHz, 16 bits, 2 channels.
const FORMAT: u16 = 0x0011;
const RATE: u32 = 48_000;
const STREAM_TAG: u32 = 1;
const PERIODS: usize = 4;
const PERIOD_BYTES: usize = 4096 * 4;

struct Controller {
    regs: Mmio,
    corb: Dma,
    rirb: Dma,
    rirb_read: core::cell::Cell<u16>,
    immediate: core::cell::Cell<bool>,
}

impl Controller {
    /// Sends a verb to a codec and returns its answer.
    fn command(&self, codec: u32, node: u32, verb: u32) -> Option<u32> {
        let word = codec << 28 | node << 20 | verb;
        if self.immediate.get() {
            return self.command_immediate(word);
        }
        let wp = (self.regs.read16(CORBWP) as usize + 1) % 256;
        self.corb.write::<u32>(wp * 4, word);
        self.regs.write16(CORBWP, wp as u16);
        let want = (self.rirb_read.get() + 1) % 256;
        if !arcdrv::wait_until(100, || self.regs.read16(RIRBWP) & 0xFF == want) {
            // The rings do not work on this controller: use the immediate
            // command registers from now on.
            arcdrv::warn!("command rings do not answer; using immediate commands");
            self.immediate.set(true);
            return self.command_immediate(word);
        }
        self.rirb_read.set(want);
        // Some controllers hold further answers until the status is cleared.
        self.regs.write8(RIRBSTS, 0x05);
        Some(self.rirb.read::<u32>(want as usize * 8))
    }

    fn command_immediate(&self, word: u32) -> Option<u32> {
        if !arcdrv::wait_until(100, || self.regs.read16(ICIS) & 1 == 0) {
            return None;
        }
        self.regs.write16(ICIS, 2); // clear "answer valid"
        self.regs.write32(ICOI, word);
        self.regs.write16(ICIS, 1);
        arcdrv::wait_until(100, || self.regs.read16(ICIS) & 3 == 2).then(|| self.regs.read32(ICII))
    }

    fn param(&self, codec: u32, node: u32, p: u32) -> u32 {
        self.command(codec, node, GET_PARAMETER << 8 | p).unwrap_or(0)
    }

    /// The nodes a widget takes its input from.
    fn connections(&self, codec: u32, node: u32) -> Vec<u32> {
        let info = self.param(codec, node, P_CONNECTIONS);
        let long = info & 0x80 != 0;
        let count = info & 0x7F;
        let (per_word, bits) = if long { (2, 16) } else { (4, 8) };
        let mut out: Vec<u32> = Vec::new();
        let mut i = 0;
        while i < count {
            let word = self.command(codec, node, GET_CONNECTION << 8 | i).unwrap_or(0);
            for k in 0..per_word {
                if i + k >= count {
                    break;
                }
                let e = (word >> (k * bits)) & ((1 << bits) - 1);
                let range = e & (1 << (bits - 1)) != 0;
                let id = e & ((1 << (bits - 1)) - 1);
                match (range, out.last().copied()) {
                    // A range: every node from the previous entry to this one.
                    (true, Some(prev)) if id > prev && id - prev < 64 => out.extend(prev + 1..=id),
                    _ => out.push(id),
                }
            }
            i += per_word;
        }
        out
    }
}

/// The widgets of an audio function group.
struct Codec<'a> {
    hda: &'a Controller,
    addr: u32,
    afg: u32,
    first: u32,
    count: u32,
}

impl Codec<'_> {
    fn caps(&self, node: u32) -> u32 {
        self.hda.param(self.addr, node, P_WIDGET_CAPS)
    }

    fn kind(&self, node: u32) -> u32 {
        (self.caps(node) >> 20) & 0xF
    }

    fn verb(&self, node: u32, verb: u32, payload: u32) {
        self.hda.command(self.addr, node, verb << 8 | payload);
    }

    /// Unmutes an amplifier at 0 dB: output (`input` None) or one input.
    fn unmute(&self, node: u32, input: Option<u32>) {
        let caps = self.caps(node);
        let (present, p) = match input {
            None => (caps & (1 << 2) != 0, P_OUT_AMP),
            Some(_) => (caps & (1 << 1) != 0, P_IN_AMP),
        };
        if !present {
            return;
        }
        // Amplifier capabilities of the widget, or of the group by default.
        let amp = if caps & (1 << 3) != 0 { self.hda.param(self.addr, node, p) } else { self.hda.param(self.addr, self.afg, p) };
        let zero_db = amp & 0x7F;
        let payload = match input {
            None => 0xB000 | zero_db,
            Some(i) => 0x7000 | i << 8 | zero_db,
        };
        self.hda.command(self.addr, node, SET_AMP << 16 | payload);
    }

    /// Finds a path from `node` back to a converter: the nodes on the way,
    /// each with the index of the input taken.
    fn path_to_dac(&self, node: u32, depth: u32, path: &mut Vec<(u32, u32)>) -> Option<u32> {
        if depth > 6 {
            return None;
        }
        for (i, c) in self.hda.connections(self.addr, node).into_iter().enumerate() {
            if c < self.first || c >= self.first + self.count || path.iter().any(|&(n, _)| n == c) {
                continue;
            }
            let caps = self.caps(c);
            if caps & (1 << 9) != 0 {
                continue; // digital (S/PDIF, HDMI)
            }
            path.push((node, i as u32));
            match (caps >> 20) & 0xF {
                W_OUTPUT => return Some(c),
                W_MIXER | W_SELECTOR => {
                    if let Some(dac) = self.path_to_dac(c, depth + 1, path) {
                        return Some(dac);
                    }
                }
                _ => {}
            }
            path.pop();
        }
        None
    }

    /// Sets up every analog output pin. Returns the pins' kinds and the
    /// converters used.
    fn setup_outputs(&self) -> (Vec<&'static str>, Vec<u32>) {
        let mut kinds = Vec::new();
        let mut dacs: Vec<u32> = Vec::new();
        for node in self.first..self.first + self.count {
            if self.kind(node) != W_PIN || self.caps(node) & (1 << 9) != 0 {
                continue;
            }
            let pin_caps = self.hda.param(self.addr, node, P_PIN_CAPS);
            if pin_caps & (1 << 4) == 0 {
                continue; // cannot output
            }
            let config = self.hda.command(self.addr, node, GET_CONFIG_DEFAULT << 8).unwrap_or(0);
            if config >> 30 == 1 {
                continue; // nothing is connected to it
            }
            let kind = match (config >> 20) & 0xF {
                0 => "line out",
                1 => "speaker",
                2 => "headphones",
                _ => continue,
            };
            let mut path = Vec::new();
            let Some(dac) = self.path_to_dac(node, 0, &mut path) else { continue };
            // Power, route and unmute everything from the converter to the pin.
            self.verb(dac, SET_POWER, 0);
            self.unmute(dac, None);
            for &(n, input) in &path {
                self.verb(n, SET_POWER, 0);
                if self.hda.connections(self.addr, n).len() > 1 && self.kind(n) != W_MIXER {
                    self.verb(n, SET_SELECT, input);
                }
                if self.kind(n) == W_MIXER {
                    self.unmute(n, Some(input));
                }
                self.unmute(n, None);
            }
            // Output on, headphone amplifier for headphones, external
            // amplifier (EAPD) on where there is one.
            let control = if kind == "headphones" { 0xC0 } else { 0x40 };
            self.verb(node, SET_PIN_CONTROL, control);
            if pin_caps & (1 << 16) != 0 {
                self.verb(node, SET_EAPD, 2);
            }
            if !dacs.contains(&dac) {
                dacs.push(dac);
            }
            if !kinds.contains(&kind) {
                kinds.push(kind);
            }
        }
        for &dac in &dacs {
            self.verb(dac, SET_STREAM, STREAM_TAG << 4);
            self.hda.command(self.addr, dac, SET_FORMAT << 16 | FORMAT as u32);
        }
        (kinds, dacs)
    }

    /// Is something plugged into a jack? (Not used to route: every output
    /// plays; logged for information.)
    #[allow(dead_code)]
    fn sensed(&self, pin: u32) -> bool {
        self.hda.command(self.addr, pin, GET_PIN_SENSE << 8).is_some_and(|v| v & (1 << 31) != 0)
    }
}

/// The output stream, for the PCM ring.
struct Stream {
    regs: Mmio,
    base: usize,
}

impl Stream {
    fn ctl(&self) -> u32 {
        self.regs.read32(self.base + SD_CTL) & 0x00FF_FFFF
    }

    fn set_ctl(&self, v: u32) {
        // CTL is 3 bytes; the fourth is the status register.
        self.regs.write16(self.base + SD_CTL, v as u16);
        self.regs.write8(self.base + SD_CTL + 2, (v >> 16) as u8);
    }
}

impl Hardware for Stream {
    fn position(&self) -> usize {
        self.regs.read32(self.base + SD_LPIB) as usize
    }

    fn start(&self) {
        self.regs.write8(self.base + SD_STS, 0x1C);
        self.set_ctl(self.ctl() | CTL_RUN | CTL_IOCE);
    }

    fn stop(&self) {
        self.set_ctl(self.ctl() & !CTL_RUN);
        arcdrv::wait_until(10, || self.ctl() & CTL_RUN == 0);
    }
}

// SAFETY: MMIO registers; the ring serialises use.
unsafe impl Sync for Stream {}

struct Device {
    ring: PcmRing<Stream>,
    /// Used at probe and after sleep only (one thread at a time).
    hda: Controller,
    pci: Pci,
    intel: bool,
    /// The codec and audio function group playing.
    codec: (u32, u32),
    inputs: u16,
    bdl: u64,
    has_irq: AtomicBool,
}

/// Before sleep: playing stops, so do the controller's interrupts and rings.
unsafe extern "C" fn suspend(ctx: *mut c_void) -> Status {
    // SAFETY: ctx is the leaked Device.
    let d = unsafe { &*(ctx as *const Device) };
    d.ring.pause();
    let regs = &d.hda.regs;
    regs.write32(INTCTL, 0);
    regs.write8(CORBCTL, 0);
    regs.write8(RIRBCTL, 0);
    OK
}

/// After sleep the codec forgot everything: the controller, the codec's
/// paths and the stream are set up again.
unsafe extern "C" fn resume(ctx: *mut c_void) -> Status {
    // SAFETY: ctx is the leaked Device.
    let d = unsafe { &*(ctx as *const Device) };
    let regs = &d.hda.regs;
    if d.intel {
        set_tcsel(d.pci);
    }
    let st = if reset(regs) {
        start_rings(regs, &d.hda.corb, &d.hda.rirb);
        d.hda.rirb_read.set(0);
        let (addr, afg) = d.codec;
        d.hda.command(addr, afg, SET_POWER << 8);
        let widgets = d.hda.param(addr, afg, P_NODE_COUNT);
        let codec = Codec { hda: &d.hda, addr, afg, first: (widgets >> 16) & 0xFF, count: widgets & 0xFF };
        let (_, dacs) = codec.setup_outputs();
        setup_stream(&d.ring.hw, d.bdl);
        if d.has_irq.load(Ordering::Relaxed) {
            regs.write32(INTCTL, 1 << 31 | 1 << d.inputs as u32);
        }
        if dacs.is_empty() {
            E_IO
        } else {
            OK
        }
    } else {
        E_IO
    };
    d.ring.unpause();
    st
}

/// Intel: traffic class 0 for the stream DMA (TCSEL).
fn set_tcsel(pci: Pci) {
    let v = pci.read32(0x44);
    pci.write32(0x44, v & !7);
}

/// The output stream reset and pointed at its buffer list.
fn setup_stream(stream: &Stream, bdl: u64) {
    let (regs, base) = (&stream.regs, stream.base);
    stream.set_ctl(CTL_RESET);
    arcdrv::wait_until(10, || stream.ctl() & CTL_RESET != 0);
    stream.set_ctl(0);
    arcdrv::wait_until(10, || stream.ctl() & CTL_RESET == 0);
    regs.write32(base + SD_BDPL, bdl as u32);
    regs.write32(base + SD_BDPU, (bdl >> 32) as u32);
    regs.write32(base + SD_CBL, (PERIODS * PERIOD_BYTES) as u32);
    regs.write16(base + SD_LVI, (PERIODS - 1) as u16);
    regs.write16(base + SD_FMT, FORMAT);
    stream.set_ctl(STREAM_TAG << 20);
}

unsafe extern "C" fn write(ctx: *mut c_void, samples: *const i16, frames: usize) -> Status {
    // SAFETY: ctx is the leaked Device; samples hold frames * 2 values.
    let d = unsafe { &*(ctx as *const Device) };
    let bytes = unsafe { core::slice::from_raw_parts(samples as *const u8, frames * 4) };
    d.ring.write(bytes);
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
    let s = &d.ring.hw;
    let sts = s.regs.read8(s.base + SD_STS);
    if sts & STS_BCIS != 0 {
        s.regs.write8(s.base + SD_STS, sts & 0x1C);
        d.ring.progress.signal();
    }
    let int = s.regs.read32(INTSTS);
    if int & (1 << 30) != 0 {
        s.regs.write8(RIRBSTS, 0x05); // the answers are polled
    }
}

fn reset(regs: &Mmio) -> bool {
    regs.write32(GCTL, regs.read32(GCTL) & !1);
    if !arcdrv::wait_until(100, || regs.read32(GCTL) & 1 == 0) {
        return false;
    }
    arcdrv::sleep_ms(1);
    regs.write32(GCTL, regs.read32(GCTL) | 1);
    if !arcdrv::wait_until(100, || regs.read32(GCTL) & 1 != 0) {
        return false;
    }
    // Codecs announce themselves within 521 µs.
    arcdrv::sleep_ms(2);
    true
}

fn start_rings(regs: &Mmio, corb: &Dma, rirb: &Dma) {
    regs.write8(CORBCTL, 0);
    regs.write8(RIRBCTL, 0);
    arcdrv::wait_until(10, || regs.read8(CORBCTL) & 2 == 0 && regs.read8(RIRBCTL) & 2 == 0);
    regs.write8(CORBSIZE, 2); // 256 entries
    regs.write32(CORBLBASE, corb.phys() as u32);
    regs.write32(CORBLBASE + 4, (corb.phys() >> 32) as u32);
    regs.write16(CORBRP, 1 << 15);
    arcdrv::wait_until(10, || regs.read16(CORBRP) & (1 << 15) != 0);
    regs.write16(CORBRP, 0);
    arcdrv::wait_until(10, || regs.read16(CORBRP) & (1 << 15) == 0);
    regs.write16(CORBWP, 0);
    regs.write8(RIRBSIZE, 2);
    regs.write32(RIRBLBASE, rirb.phys() as u32);
    regs.write32(RIRBLBASE + 4, (rirb.phys() >> 32) as u32);
    regs.write16(RIRBWP, 1 << 15);
    regs.write16(RINTCNT, 0xFF);
    regs.write8(CORBCTL, 2);
    regs.write8(RIRBCTL, 2);
}

fn probe(dev: &DeviceInfo) -> Status {
    let bar = dev.resources[0];
    if bar.kind != RES_MEMORY || bar.base == 0 {
        return E_NODEV;
    }
    let pci = Pci(dev.id);
    pci.enable(false, true, true);
    let intel = dev.vendor == 0x8086;
    if intel {
        set_tcsel(pci);
    }
    let Some(regs) = Mmio::map(bar.base, 0x4000) else { return E_NOMEM };
    if !reset(&regs) {
        arcdrv::warn!("the controller does not come out of reset");
        return E_IO;
    }
    let gcap = regs.read16(GCAP);
    let (outputs, inputs) = ((gcap >> 12) & 0xF, (gcap >> 8) & 0xF);
    if outputs == 0 {
        arcdrv::warn!("no output stream");
        return E_NODEV;
    }
    let (Some(corb), Some(rirb)) = (Dma::new(1024), Dma::new(2048)) else { return E_NOMEM };
    start_rings(&regs, &corb, &rirb);
    let hda = Controller { regs, corb, rirb, rirb_read: core::cell::Cell::new(0), immediate: core::cell::Cell::new(false) };

    let present = regs.read16(STATESTS);
    let mut chosen = None;
    for addr in 0..15u32 {
        if present & (1 << addr) == 0 {
            continue;
        }
        let vendor = hda.param(addr, 0, P_VENDOR);
        let nodes = hda.param(addr, 0, P_NODE_COUNT);
        for fg in (nodes >> 16) & 0xFF..((nodes >> 16) & 0xFF) + (nodes & 0xFF) {
            if hda.param(addr, fg, P_FUNCTION_TYPE) & 0xFF != 1 {
                continue; // not an audio function group
            }
            hda.command(addr, fg, SET_POWER << 8);
            let widgets = hda.param(addr, fg, P_NODE_COUNT);
            let codec = Codec { hda: &hda, addr, afg: fg, first: (widgets >> 16) & 0xFF, count: widgets & 0xFF };
            let (kinds, dacs) = codec.setup_outputs();
            if !dacs.is_empty() {
                chosen = Some((vendor, kinds, (addr, fg)));
                break;
            }
        }
        if chosen.is_some() {
            break;
        }
    }
    let Some((vendor, kinds, codec)) = chosen else {
        arcdrv::info!("no analog output on the codecs (only HDMI or digital?)");
        return E_NODEV;
    };

    // The first output stream.
    let base = 0x80 + inputs as usize * 0x20;
    let stream = Stream { regs, base };
    let (Some(buf), Some(bdl)) = (Dma::new(PERIODS * PERIOD_BYTES), Dma::new(4096)) else { return E_NOMEM };
    for i in 0..PERIODS {
        bdl.write::<u64>(i * 16, buf.phys() + (i * PERIOD_BYTES) as u64);
        bdl.write::<u32>(i * 16 + 8, PERIOD_BYTES as u32);
        bdl.write::<u32>(i * 16 + 12, 1); // interrupt on completion
    }
    setup_stream(&stream, bdl.phys());
    let bdl_phys = bdl.phys();
    // The descriptor table must outlive the stream.
    core::mem::forget(bdl);

    let d: &'static Device = Box::leak(Box::new(Device {
        ring: PcmRing::new(stream, buf),
        hda,
        pci,
        intel,
        codec,
        inputs,
        bdl: bdl_phys,
        has_irq: AtomicBool::new(false),
    }));
    let ctx = d as *const Device as *mut c_void;
    if arcdrv::irq_attach_any(dev, irq, ctx) {
        d.has_irq.store(true, Ordering::Relaxed);
        regs.write32(INTCTL, 1 << 31 | 1 << inputs as u32);
    }
    let what = kinds.join(", ");
    let name = format!("HD Audio, {what}");
    let desc: &'static AudioDesc =
        Box::leak(Box::new(AudioDesc { ctx, rate: RATE, channels: 2, write, drain, name: arcdrv::model_name(&name) }));
    arcdrv::info!("codec {:04x}:{:04x}: {what}", vendor >> 16, vendor & 0xFFFF);
    let n = arcdrv::audio_register(dev.id, desc);
    if n < 0 {
        return n;
    }
    arcdrv::power_hooks(dev.id, ctx, Some(suspend), resume);
    OK
}

//! Intel PRO/1000 (e1000) Ethernet driver.
//!
//! Descriptor rings for receive and transmit, 2 KiB buffers, receive
//! interrupts by MSI or the legacy line (with a polling thread as a
//! fallback). Every card found gets its own state. After sleep the card is
//! reset and set up again with the same rings.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use arcdrv::{DeviceInfo, Dma, IrqLock, Mmio, NetDesc, Status, E_INVAL, E_IO, E_NODEV, E_NOMEM, OK, RES_MEMORY};
use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicI32, Ordering};

arcdrv::driver! {
    name: "e1000",
    version: "1.1.0",
    probe: probe,
}

const CTRL: usize = 0x0000;
const STATUS: usize = 0x0008;
const EERD: usize = 0x0014;
const CTRL_EXT: usize = 0x0018;
const ICR: usize = 0x00C0;
const IMS: usize = 0x00D0;
const IMC: usize = 0x00D8;
const RCTL: usize = 0x0100;
const TCTL: usize = 0x0400;
const TIPG: usize = 0x0410;
const RDBAL: usize = 0x2800;
const RDBAH: usize = 0x2804;
const RDLEN: usize = 0x2808;
const RDH: usize = 0x2810;
const RDT: usize = 0x2818;
const TDBAL: usize = 0x3800;
const TDBAH: usize = 0x3804;
const TDLEN: usize = 0x3808;
const TDH: usize = 0x3810;
const TDT: usize = 0x3818;
const MTA: usize = 0x5200;
const RAL: usize = 0x5400;
const RAH: usize = 0x5404;
/// Interrupt vector allocation (82574 only).
const IVAR: usize = 0x00E4;

// Room for a burst of a full TCP window (slirp and real links send many
// segments back to back; a short ring drops them and TCP stalls).
const RX_COUNT: usize = 256;
const TX_COUNT: usize = 64;
const BUF: usize = 2048;

struct Rings {
    rx_ring: Dma,
    rx_bufs: Dma,
    rx_next: usize,
    tx_ring: Dma,
    tx_bufs: Dma,
    tx_next: usize,
}

struct Nic {
    regs: Mmio,
    rings: IrqLock<Rings>,
    mac: [u8; 6],
    iface: AtomicI32,
    has_irq: AtomicBool,
    /// The 82574, whose interrupt causes need routing.
    is_82574: bool,
    /// Asleep: the card is stopped and the poller leaves it alone.
    paused: AtomicBool,
}

impl Rings {
    /// Hands every completed receive descriptor to the kernel.
    fn receive(&mut self, regs: &Mmio, iface: i32) {
        loop {
            let at = self.rx_next * 16;
            let status = self.rx_ring.read::<u8>(at + 12);
            if status & 1 == 0 {
                break; // descriptor not done
            }
            let len = self.rx_ring.read::<u16>(at + 8) as usize;
            if status & 2 != 0 && iface >= 0 && len <= BUF {
                let frame = &self.rx_bufs.as_slice()[self.rx_next * BUF..self.rx_next * BUF + len];
                arcdrv::net_receive(iface as u32, frame);
            }
            self.rx_ring.write::<u8>(at + 12, 0);
            regs.write32(RDT, self.rx_next as u32);
            self.rx_next = (self.rx_next + 1) % RX_COUNT;
        }
    }

    fn send(&mut self, regs: &Mmio, frame: &[u8]) -> Status {
        if frame.len() > BUF || frame.is_empty() {
            return E_INVAL;
        }
        let i = self.tx_next;
        let at = i * 16;
        // Wait for the descriptor to be free (done or never used).
        if !arcdrv::wait_until(100, || {
            let st = self.tx_ring.read::<u8>(at + 12);
            st & 1 != 0 || self.tx_ring.read::<u16>(at + 8) == 0
        }) {
            return E_IO;
        }
        self.tx_bufs.as_mut_slice()[i * BUF..i * BUF + frame.len()].copy_from_slice(frame);
        self.tx_ring.write::<u64>(at, self.tx_bufs.phys() + (i * BUF) as u64);
        self.tx_ring.write::<u16>(at + 8, frame.len() as u16);
        self.tx_ring.write::<u8>(at + 11, 0b1011); // EOP | IFCS | RS
        self.tx_ring.write::<u8>(at + 12, 0);
        self.tx_next = (i + 1) % TX_COUNT;
        regs.write32(TDT, self.tx_next as u32);
        OK
    }

    /// Empty rings, given to the card; receive and transmit on.
    fn start(&mut self, regs: &Mmio) {
        self.rx_ring.as_mut_slice().fill(0);
        self.tx_ring.as_mut_slice().fill(0);
        for i in 0..RX_COUNT {
            self.rx_ring.write::<u64>(i * 16, self.rx_bufs.phys() + (i * BUF) as u64);
        }
        self.rx_next = 0;
        self.tx_next = 0;
        regs.write32(RDBAL, self.rx_ring.phys() as u32);
        regs.write32(RDBAH, (self.rx_ring.phys() >> 32) as u32);
        regs.write32(RDLEN, (RX_COUNT * 16) as u32);
        regs.write32(RDH, 0);
        regs.write32(RDT, (RX_COUNT - 1) as u32);
        // Enable, accept broadcast, 2 KiB buffers, strip the CRC.
        regs.write32(RCTL, (1 << 1) | (1 << 15) | (1 << 26));

        regs.write32(TDBAL, self.tx_ring.phys() as u32);
        regs.write32(TDBAH, (self.tx_ring.phys() >> 32) as u32);
        regs.write32(TDLEN, (TX_COUNT * 16) as u32);
        regs.write32(TDH, 0);
        regs.write32(TDT, 0);
        regs.write32(TCTL, (1 << 1) | (1 << 3) | (0x0F << 4) | (0x40 << 12));
        regs.write32(TIPG, 0x0060_200A);
    }
}

impl Nic {
    /// Receive timer, overrun, link change (and the 82574's MSI-X causes,
    /// all routed to the first vector).
    fn unmask(&self) {
        let mut causes = (1 << 7) | (1 << 4) | (1 << 2);
        if self.is_82574 {
            self.regs.write32(IVAR, 0x8008_8888);
            self.regs.write32(CTRL_EXT, self.regs.read32(CTRL_EXT) & !(1 << 27));
            causes |= (1 << 20) | (1 << 24);
        }
        if self.has_irq.load(Ordering::Relaxed) {
            self.regs.write32(IMS, causes);
        }
    }
}

unsafe extern "C" fn send(ctx: *mut c_void, frame: *const u8, len: usize) -> Status {
    // SAFETY: ctx is the leaked Nic; the kernel passes a valid frame.
    let nic = unsafe { &*(ctx as *const Nic) };
    if nic.paused.load(Ordering::Acquire) {
        return E_IO;
    }
    nic.rings.lock().send(&nic.regs, unsafe { core::slice::from_raw_parts(frame, len) })
}

unsafe extern "C" fn irq(ctx: *mut c_void) {
    // SAFETY: the leaked Nic.
    let nic = unsafe { &*(ctx as *const Nic) };
    let mut rings = nic.rings.lock();
    let cause = nic.regs.read32(ICR); // reading clears it
    if cause != 0 && cause != u32::MAX {
        // With MSI-X the 82574 does not clear on read.
        nic.regs.write32(ICR, cause);
        if !nic.paused.load(Ordering::Acquire) {
            rings.receive(&nic.regs, nic.iface.load(Ordering::Relaxed));
        }
    }
}

unsafe extern "C" fn poller(ctx: *mut c_void) {
    // SAFETY: the leaked Nic.
    let nic = unsafe { &*(ctx as *const Nic) };
    loop {
        if !nic.paused.load(Ordering::Acquire) {
            nic.rings.lock().receive(&nic.regs, nic.iface.load(Ordering::Relaxed));
        }
        arcdrv::sleep_ms(if nic.has_irq.load(Ordering::Relaxed) { 50 } else { 5 });
    }
}

/// Before sleep: interrupts off, receive and transmit stopped.
unsafe extern "C" fn suspend(ctx: *mut c_void) -> Status {
    // SAFETY: the leaked Nic.
    let nic = unsafe { &*(ctx as *const Nic) };
    nic.paused.store(true, Ordering::Release);
    let _rings = nic.rings.lock();
    nic.regs.write32(IMC, u32::MAX);
    nic.regs.write32(RCTL, 0);
    nic.regs.write32(TCTL, 0);
    OK
}

/// After sleep the card has lost its setup: reset, the same address, the
/// rings from the start.
unsafe extern "C" fn resume(ctx: *mut c_void) -> Status {
    // SAFETY: the leaked Nic.
    let nic = unsafe { &*(ctx as *const Nic) };
    nic.paused.store(true, Ordering::Release);
    if !reset(&nic.regs) {
        return E_IO;
    }
    let m = nic.mac;
    nic.regs.write32(RAL, u32::from_le_bytes([m[0], m[1], m[2], m[3]]));
    nic.regs.write32(RAH, m[4] as u32 | (m[5] as u32) << 8 | 1 << 31);
    for i in 0..128 {
        nic.regs.write32(MTA + i * 4, 0);
    }
    nic.rings.lock().start(&nic.regs);
    nic.paused.store(false, Ordering::Release);
    nic.unmask();
    OK
}

/// Resets the card, then link up with automatic speed. False if the reset
/// never finished.
fn reset(regs: &Mmio) -> bool {
    regs.write32(IMC, u32::MAX);
    regs.write32(CTRL, regs.read32(CTRL) | (1 << 26));
    arcdrv::sleep_ms(10);
    let done = arcdrv::wait_until(100, || regs.read32(CTRL) & (1 << 26) == 0);
    regs.write32(IMC, u32::MAX);
    regs.write32(CTRL, (regs.read32(CTRL) | (1 << 6) | (1 << 5)) & !(1 << 3));
    done
}

fn read_mac(regs: &Mmio) -> [u8; 6] {
    let lo = regs.read32(RAL);
    let hi = regs.read32(RAH);
    if hi & (1 << 31) != 0 && (lo != 0 || hi & 0xFFFF != 0) {
        let l = lo.to_le_bytes();
        let h = hi.to_le_bytes();
        return [l[0], l[1], l[2], l[3], h[0], h[1]];
    }
    // Not in the receive address registers: read the EEPROM.
    let mut mac = [0u8; 6];
    for word in 0..3u32 {
        regs.write32(EERD, 1 | word << 8);
        let mut v = 0;
        arcdrv::wait_until(10, || {
            v = regs.read32(EERD);
            v & (1 << 4) != 0
        });
        let data = (v >> 16) as u16;
        mac[word as usize * 2] = data as u8;
        mac[word as usize * 2 + 1] = (data >> 8) as u8;
    }
    mac
}

fn probe(dev: &DeviceInfo) -> Status {
    if dev.vendor != 0x8086 {
        return E_NODEV;
    }
    let bar = dev.resources[0];
    if bar.kind != RES_MEMORY || bar.base == 0 {
        return E_IO;
    }
    arcdrv::Pci(dev.id).enable(false, true, true);
    let Some(regs) = Mmio::map(bar.base, 0x20000) else { return E_NOMEM };

    reset(&regs);
    let mac = read_mac(&regs);
    for i in 0..128 {
        regs.write32(MTA + i * 4, 0);
    }

    let (Some(rx_ring), Some(rx_bufs), Some(tx_ring), Some(tx_bufs)) =
        (Dma::new(RX_COUNT * 16), Dma::new(RX_COUNT * BUF), Dma::new(TX_COUNT * 16), Dma::new(TX_COUNT * BUF))
    else {
        return E_NOMEM;
    };
    let mut rings = Rings { rx_ring, rx_bufs, rx_next: 0, tx_ring, tx_bufs, tx_next: 0 };
    rings.start(&regs);

    let link = regs.read32(STATUS) & 2 != 0;
    let nic: &'static Nic = Box::leak(Box::new(Nic {
        regs,
        rings: IrqLock::new(rings),
        mac,
        iface: AtomicI32::new(-1),
        has_irq: AtomicBool::new(false),
        is_82574: matches!(dev.device, 0x10D3 | 0x10F6),
        paused: AtomicBool::new(false),
    }));
    let ctx = nic as *const Nic as *mut c_void;

    let desc: &'static NetDesc = Box::leak(Box::new(NetDesc { ctx, mac, _pad: [0; 2], mtu: 1500, send }));
    let iface = arcdrv::net_register(dev.id, desc);
    if iface < 0 {
        nic.regs.write32(RCTL, 0);
        nic.regs.write32(TCTL, 0);
        return iface;
    }
    nic.iface.store(iface, Ordering::Relaxed);

    if arcdrv::irq_attach_any(dev, irq, ctx) {
        nic.has_irq.store(true, Ordering::Relaxed);
    }
    nic.unmask();
    // Interrupt routing can be wrong on some boards; poll as a safety net.
    arcdrv::spawn("e1000-rx", poller, ctx);
    arcdrv::power_hooks(dev.id, ctx, Some(suspend), resume);
    arcdrv::info!(
        "link {}, {}",
        if link { "up" } else { "down" },
        if nic.has_irq.load(Ordering::Relaxed) { "interrupts" } else { "polling" }
    );
    OK
}

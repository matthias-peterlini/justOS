//! Intel Ethernet driver for the chips with advanced descriptors:
//! 82575/82576/82580, I350/I354, I210/I211 and the 2.5 Gbit I225/I226.
//!
//! One receive and one transmit queue (queue 0 of each), 2 KiB buffers,
//! advanced descriptors (the only kind every one of these chips accepts),
//! interrupts by MSI or the legacy line with a polling thread as a
//! fallback. The PHY is powered up and left to negotiate the link.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use justdrv::{DeviceInfo, Dma, IrqLock, Mmio, NetDesc, Status, E_INVAL, E_IO, E_NODEV, E_NOMEM, OK, RES_MEMORY};
use core::ffi::c_void;
use core::sync::atomic::{fence, AtomicBool, AtomicI32, Ordering};

justdrv::driver! {
    name: "igb",
    version: "1.1.0",
    probe: probe,
}

const CTRL: usize = 0x0000;
const STATUS: usize = 0x0008;
const EERD: usize = 0x0014;
const CTRL_EXT: usize = 0x0018;
const MDIC: usize = 0x0020;
const ICR: usize = 0x1500;
const IMS: usize = 0x1508;
const IMC: usize = 0x150C;
const EIMS: usize = 0x1524;
const EIMC: usize = 0x1528;
const EIAC: usize = 0x152C;
const EICR: usize = 0x1580;
const IVAR0: usize = 0x1700;
const IVAR_MISC: usize = 0x1740;
const GPIE: usize = 0x1514;
const RCTL: usize = 0x0100;
const TCTL: usize = 0x0400;
const MTA: usize = 0x5200;
const RAL: usize = 0x5400;
const RAH: usize = 0x5404;
// Queue 0
const RDBAL: usize = 0xC000;
const RDBAH: usize = 0xC004;
const RDLEN: usize = 0xC008;
const SRRCTL: usize = 0xC00C;
const RDH: usize = 0xC010;
const RDT: usize = 0xC018;
const RXDCTL: usize = 0xC028;
const TDBAL: usize = 0xE000;
const TDBAH: usize = 0xE004;
const TDLEN: usize = 0xE008;
const TDH: usize = 0xE010;
const TDT: usize = 0xE018;
const TXDCTL: usize = 0xE028;

const CTRL_SLU: u32 = 1 << 6;
const CTRL_RST: u32 = 1 << 26;
const QUEUE_ENABLE: u32 = 1 << 25;

/// Receive: descriptor done, end of packet.
const RX_DD: u32 = 1 << 0;
const RX_EOP: u32 = 1 << 1;
/// Transmit command: end of packet, insert CRC, report status, data
/// descriptor (advanced).
const TX_CMD: u32 = 1 << 24 | 1 << 25 | 1 << 27 | 1 << 29 | 3 << 20;
const TX_DD: u32 = 1 << 0;

const RX_COUNT: usize = 256;
const TX_COUNT: usize = 64;
const BUF: usize = 2048;

struct Rings {
    rx_next: usize,
    tx_next: usize,
}

struct Nic {
    regs: Mmio,
    rx_ring: Dma,
    rx_bufs: Dma,
    tx_ring: Dma,
    tx_bufs: Dma,
    rings: IrqLock<Rings>,
    iface: AtomicI32,
    has_irq: AtomicBool,
    /// Interrupts come by MSI-X vector 0.
    msix: AtomicBool,
    mac: [u8; 6],
    /// Asleep: the card is left alone.
    paused: AtomicBool,
}

impl Nic {
    /// Gives receive descriptor `i` back to the card (the card overwrote
    /// its address with the status).
    fn give_rx(&self, i: usize) {
        self.rx_ring.write::<u64>(i * 16, self.rx_bufs.phys() + (i * BUF) as u64);
        self.rx_ring.write::<u64>(i * 16 + 8, 0);
    }

    /// After [`reset`]: address, filters, fresh rings, both units on
    /// (interrupts masked). Nothing else may use the rings meanwhile.
    fn configure(&self) {
        let regs = &self.regs;
        let mac = self.mac;
        regs.write32(RAL, u32::from_le_bytes([mac[0], mac[1], mac[2], mac[3]]));
        regs.write32(RAH, u16::from_le_bytes([mac[4], mac[5]]) as u32 | 1 << 31);
        for i in 0..128 {
            regs.write32(MTA + i * 4, 0);
        }
        for i in 0..RX_COUNT {
            self.give_rx(i);
        }
        self.tx_ring.as_mut_slice().fill(0);
        *self.rings.lock() = Rings { rx_next: 0, tx_next: 0 };

        // Receive: 2 KiB buffers, advanced one-buffer descriptors.
        regs.write32(RCTL, 0);
        regs.write32(RDBAL, self.rx_ring.phys() as u32);
        regs.write32(RDBAH, (self.rx_ring.phys() >> 32) as u32);
        regs.write32(RDLEN, (RX_COUNT * 16) as u32);
        regs.write32(SRRCTL, (BUF / 1024) as u32 | 1 << 25);
        regs.write32(RDH, 0);
        regs.write32(RDT, 0);
        regs.write32(RXDCTL, regs.read32(RXDCTL) | QUEUE_ENABLE);
        if !justdrv::wait_until(50, || regs.read32(RXDCTL) & QUEUE_ENABLE != 0) {
            justdrv::warn!("the receive queue does not start");
        }
        regs.write32(RDT, (RX_COUNT - 1) as u32);
        // Enable, broadcast, strip the CRC.
        regs.write32(RCTL, 1 << 1 | 1 << 15 | 1 << 26);

        regs.write32(TDBAL, self.tx_ring.phys() as u32);
        regs.write32(TDBAH, (self.tx_ring.phys() >> 32) as u32);
        regs.write32(TDLEN, (TX_COUNT * 16) as u32);
        regs.write32(TDH, 0);
        regs.write32(TDT, 0);
        regs.write32(TXDCTL, regs.read32(TXDCTL) | QUEUE_ENABLE);
        if !justdrv::wait_until(50, || regs.read32(TXDCTL) & QUEUE_ENABLE != 0) {
            justdrv::warn!("the transmit queue does not start");
        }
        // Enable, pad short frames, collision settings.
        regs.write32(TCTL, 1 << 1 | 1 << 3 | 0x0F << 4 | 0x3F << 12);
    }

    /// Lets the card interrupt again (when it has a way to).
    fn unmask(&self) {
        if !self.has_irq.load(Ordering::Relaxed) {
            return;
        }
        let regs = &self.regs;
        if self.msix.load(Ordering::Relaxed) {
            // MSI-X: queue 0 (both ways) and the other causes on vector 0,
            // cleared automatically.
            regs.write32(GPIE, 1 << 4 | 1 << 30 | 1 << 31 | 1 << 0);
            regs.write32(IVAR0, 0x8080);
            regs.write32(IVAR_MISC, 0x8000);
            regs.write32(EIAC, 1);
            regs.write32(EIMS, 1);
        }
        // Receive descriptor written back, receive timer, link change.
        regs.write32(IMS, 1 << 7 | 1 << 4 | 1 << 2 | 1 << 0);
    }

    fn receive(&self) {
        if self.paused.load(Ordering::Acquire) {
            return;
        }
        let iface = self.iface.load(Ordering::Relaxed);
        let mut r = self.rings.lock();
        for _ in 0..RX_COUNT {
            let i = r.rx_next;
            let status = self.rx_ring.read::<u32>(i * 16 + 8);
            if status & RX_DD == 0 {
                break;
            }
            fence(Ordering::SeqCst);
            let len = self.rx_ring.read::<u16>(i * 16 + 12) as usize;
            if status & RX_EOP != 0 && len <= BUF && iface >= 0 {
                justdrv::net_receive(iface as u32, &self.rx_bufs.as_slice()[i * BUF..i * BUF + len]);
            }
            self.give_rx(i);
            fence(Ordering::SeqCst);
            self.regs.write32(RDT, i as u32);
            r.rx_next = (i + 1) % RX_COUNT;
        }
    }

    fn send(&self, frame: &[u8]) -> Status {
        if frame.is_empty() || frame.len() > BUF {
            return E_INVAL;
        }
        if self.paused.load(Ordering::Acquire) {
            return E_IO;
        }
        // Wait for the slot without holding the lock (time stands still
        // while it is held).
        let start = justdrv::time_ms();
        let mut r = loop {
            let r = self.rings.lock();
            let at = r.tx_next * 16;
            // Free once done, or never used (address still zero).
            let used = self.tx_ring.read::<u64>(at) != 0;
            if !used || self.tx_ring.read::<u32>(at + 12) & TX_DD != 0 {
                break r;
            }
            drop(r);
            if justdrv::time_ms() - start > 100 {
                return E_IO;
            }
            justdrv::yield_now();
        };
        let i = r.tx_next;
        let at = i * 16;
        self.tx_bufs.as_mut_slice()[i * BUF..i * BUF + frame.len()].copy_from_slice(frame);
        self.tx_ring.write::<u64>(at, self.tx_bufs.phys() + (i * BUF) as u64);
        self.tx_ring.write::<u32>(at + 8, TX_CMD | frame.len() as u32);
        self.tx_ring.write::<u32>(at + 12, (frame.len() as u32) << 14);
        fence(Ordering::SeqCst);
        r.tx_next = (i + 1) % TX_COUNT;
        self.regs.write32(TDT, r.tx_next as u32);
        OK
    }
}

unsafe extern "C" fn send(ctx: *mut c_void, frame: *const u8, len: usize) -> Status {
    // SAFETY: ctx is the leaked Nic; the kernel passes a valid frame.
    let nic = unsafe { &*(ctx as *const Nic) };
    nic.send(unsafe { core::slice::from_raw_parts(frame, len) })
}

unsafe extern "C" fn irq(ctx: *mut c_void) {
    // SAFETY: the leaked Nic.
    let nic = unsafe { &*(ctx as *const Nic) };
    if nic.msix.load(Ordering::Relaxed) {
        // Clear the vector's cause and unmask it, then look at the queue.
        nic.regs.write32(EICR, u32::MAX);
        nic.regs.read32(ICR);
        if !nic.paused.load(Ordering::Acquire) {
            nic.regs.write32(EIMS, 1);
        }
        nic.receive();
        return;
    }
    let cause = nic.regs.read32(ICR); // reading clears it
    if cause == 0 || cause == u32::MAX {
        return;
    }
    nic.receive();
}

unsafe extern "C" fn poller(ctx: *mut c_void) {
    // SAFETY: the leaked Nic.
    let nic = unsafe { &*(ctx as *const Nic) };
    loop {
        nic.receive();
        justdrv::sleep_ms(if nic.has_irq.load(Ordering::Relaxed) { 50 } else { 5 });
    }
}

/// Before sleep: the card stops.
unsafe extern "C" fn suspend(ctx: *mut c_void) -> Status {
    // SAFETY: the leaked Nic.
    let nic = unsafe { &*(ctx as *const Nic) };
    nic.paused.store(true, Ordering::Release);
    let _r = nic.rings.lock(); // a send or receive under way ends
    let regs = &nic.regs;
    regs.write32(IMC, u32::MAX);
    regs.write32(EIMC, u32::MAX);
    regs.write32(RCTL, 0);
    regs.write32(TCTL, 0);
    OK
}

/// After sleep: the card set up again.
unsafe extern "C" fn resume(ctx: *mut c_void) -> Status {
    // SAFETY: the leaked Nic.
    let nic = unsafe { &*(ctx as *const Nic) };
    let st = reset(&nic.regs);
    if st == OK {
        nic.configure();
        nic.paused.store(false, Ordering::Release);
        nic.unmask();
    }
    st
}

/// Reads a PHY register (`None` if the PHY does not answer).
fn phy_read(regs: &Mmio, reg: u32) -> Option<u16> {
    regs.write32(MDIC, reg << 16 | 1 << 21 | 2 << 26);
    let mut v = 0;
    justdrv::wait_until(10, || {
        v = regs.read32(MDIC);
        v & (1 << 28) != 0
    })
    .then_some(())?;
    (v & (1 << 30) == 0).then_some(v as u16)
}

fn phy_write(regs: &Mmio, reg: u32, data: u16) {
    regs.write32(MDIC, data as u32 | reg << 16 | 1 << 21 | 1 << 26);
    justdrv::wait_until(10, || regs.read32(MDIC) & (1 << 28) != 0);
}

fn read_mac(regs: &Mmio) -> Option<[u8; 6]> {
    let (lo, hi) = (regs.read32(RAL), regs.read32(RAH));
    if hi & (1 << 31) != 0 && (lo != 0 || hi & 0xFFFF != 0) {
        let (l, h) = (lo.to_le_bytes(), hi.to_le_bytes());
        return Some([l[0], l[1], l[2], l[3], h[0], h[1]]);
    }
    // Not loaded into the address registers: read the NVM (82575/82576).
    let mut mac = [0u8; 6];
    for word in 0..3u32 {
        regs.write32(EERD, 1 | word << 2);
        let mut v = 0;
        if !justdrv::wait_until(10, || {
            v = regs.read32(EERD);
            v & 2 != 0
        }) {
            return None;
        }
        mac[word as usize * 2..word as usize * 2 + 2].copy_from_slice(&((v >> 16) as u16).to_le_bytes());
    }
    (mac != [0; 6] && mac != [0xFF; 6]).then_some(mac)
}

/// Resets the card, sets the link up and wakes the PHY (interrupts masked).
fn reset(regs: &Mmio) -> Status {
    regs.write32(IMC, u32::MAX);
    regs.write32(EIMC, u32::MAX);
    regs.write32(CTRL, regs.read32(CTRL) | CTRL_RST);
    justdrv::sleep_ms(20);
    if !justdrv::wait_until(200, || regs.read32(CTRL) & CTRL_RST == 0) {
        justdrv::warn!("the card does not come out of reset");
        return E_IO;
    }
    // The NVM is read again after a reset.
    justdrv::sleep_ms(20);
    regs.write32(IMC, u32::MAX);
    regs.write32(EIMC, u32::MAX);
    regs.write32(GPIE, 0); // interrupts by cause register, not per queue
    regs.write32(CTRL, regs.read32(CTRL) | CTRL_SLU);
    regs.write32(CTRL_EXT, regs.read32(CTRL_EXT) | 1 << 28); // driver loaded

    // Power up the PHY and restart auto-negotiation.
    if let Some(bmcr) = phy_read(regs, 0) {
        phy_write(regs, 0, (bmcr & !(1 << 11)) | 1 << 12 | 1 << 9);
    }
    OK
}

fn probe(dev: &DeviceInfo) -> Status {
    let bar = dev.resources[0];
    if dev.vendor != 0x8086 || bar.kind != RES_MEMORY || bar.base == 0 {
        return E_NODEV;
    }
    justdrv::Pci(dev.id).enable(false, true, true);
    let Some(regs) = Mmio::map(bar.base, 0x20000) else { return E_NOMEM };
    let st = reset(&regs);
    if st != OK {
        return st;
    }
    let mac = match read_mac(&regs) {
        Some(m) => m,
        None => {
            justdrv::warn!("no MAC address in the NVM");
            return E_IO;
        }
    };

    let (Some(rx_ring), Some(rx_bufs), Some(tx_ring), Some(tx_bufs)) =
        (Dma::new(RX_COUNT * 16), Dma::new(RX_COUNT * BUF), Dma::new(TX_COUNT * 16), Dma::new(TX_COUNT * BUF))
    else {
        return E_NOMEM;
    };
    let nic: &'static Nic = Box::leak(Box::new(Nic {
        regs,
        rx_ring,
        rx_bufs,
        tx_ring,
        tx_bufs,
        rings: IrqLock::new(Rings { rx_next: 0, tx_next: 0 }),
        iface: AtomicI32::new(-1),
        has_irq: AtomicBool::new(false),
        msix: AtomicBool::new(false),
        mac,
        paused: AtomicBool::new(false),
    }));
    nic.configure();

    let ctx = nic as *const Nic as *mut c_void;
    let desc: &'static NetDesc = Box::leak(Box::new(NetDesc { ctx, mac, _pad: [0; 2], mtu: 1500, send }));
    let iface = justdrv::net_register(dev.id, desc);
    if iface < 0 {
        regs.write32(RCTL, 0);
        regs.write32(TCTL, 0);
        return iface;
    }
    nic.iface.store(iface, Ordering::Relaxed);
    // The 82575 maps MSI-X vectors another way: it uses its line.
    let is_82575 = matches!(dev.device, 0x10A7 | 0x10A9 | 0x10D6);
    let message = !is_82575 && justdrv::irq_attach_msi(dev.id, irq, ctx) >= 0;
    let attached = message || (dev.irq != justdrv::IRQ_NONE && justdrv::irq_attach(dev.id, dev.irq, irq, ctx) == OK);
    if attached {
        nic.has_irq.store(true, Ordering::Relaxed);
        let msix = message && justdrv::Pci(dev.id).find_capability(0x11, 0).is_some();
        nic.msix.store(msix, Ordering::Relaxed);
        nic.unmask();
    }
    justdrv::spawn("igb-rx", poller, ctx);
    justdrv::power_hooks(dev.id, ctx, Some(suspend), resume);
    let status = regs.read32(STATUS);
    let speed = match (status >> 6) & 3 {
        0 => "10 Mbit",
        1 => "100 Mbit",
        _ => "1 Gbit or faster",
    };
    justdrv::info!(
        "link {}, {}",
        if status & 2 != 0 { speed } else { "down" },
        if nic.has_irq.load(Ordering::Relaxed) { "interrupts" } else { "polling" }
    );
    OK
}

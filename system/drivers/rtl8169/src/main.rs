//! Realtek RTL8169 / RTL8111 / RTL8168 / RTL810x and RTL8125 (2.5 Gbit)
//! Ethernet driver.
//!
//! Descriptor rings for receive and transmit (16-byte descriptors owned by
//! the card until it is done), 2 KiB buffers, interrupts by MSI or the
//! legacy line with a polling thread as a fallback. Every card found gets
//! its own state.
//!
//! The chips come in dozens of revisions that Linux tunes one by one; this
//! driver uses only the settings they share and leaves the PHY as the
//! firmware set it up. The RTL8125 keeps the descriptors and most registers
//! but moves the interrupt registers (32 bits wide, at 0x38 and 0x3C) and
//! the transmit poll (0x90).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use justdrv::{DeviceInfo, Dma, IrqLock, NetDesc, Regs, Status, E_INVAL, E_IO, E_NODEV, E_NOMEM, OK};
use core::ffi::c_void;
use core::sync::atomic::{fence, AtomicBool, AtomicI32, Ordering};

justdrv::driver! {
    name: "rtl8169",
    version: "1.1.0",
    probe: probe,
}

const IDR: u16 = 0x00;
const MAR: u16 = 0x08;
const TX_DESC: u16 = 0x20;
const CHIP_CMD: u16 = 0x37;
const TX_POLL: u16 = 0x38;
const INTR_MASK: u16 = 0x3C;
const INTR_STATUS: u16 = 0x3E;
const TX_CONFIG: u16 = 0x40;
const RX_CONFIG: u16 = 0x44;
const CFG9346: u16 = 0x50;
const PHY_STATUS: u16 = 0x6C;
const RX_MAX_SIZE: u16 = 0xDA;
const CPLUS_CMD: u16 = 0xE0;
const INTR_MITIGATE: u16 = 0xE2;
const RX_DESC: u16 = 0xE4;
const MAX_TX_SIZE: u16 = 0xEC;

const CMD_RESET: u8 = 1 << 4;
const CMD_RX: u8 = 1 << 3;
const CMD_TX: u8 = 1 << 2;
const POLL_NORMAL: u8 = 1 << 6;

const OWN: u32 = 1 << 31;
const EOR: u32 = 1 << 30;
const FS: u32 = 1 << 29;
const LS: u32 = 1 << 28;
/// Receive error summary.
const RES: u32 = 1 << 21;

/// Receive ok and error, transmit ok and error, no receive descriptor,
/// link change, FIFO overflow.
const INTERRUPTS: u16 = 0x007F;

// RTL8125
const INTR_MASK_8125: u16 = 0x38;
const INTR_STATUS_8125: u16 = 0x3C;
const TX_POLL_8125: u16 = 0x90;

const RX_COUNT: usize = 256;
const TX_COUNT: usize = 64;
const BUF: usize = 2048;
const MAX_FRAME: usize = 1518;

struct Rings {
    rx_next: usize,
    tx_next: usize,
}

struct Nic {
    regs: Regs,
    rx_ring: Dma,
    rx_bufs: Dma,
    tx_ring: Dma,
    tx_bufs: Dma,
    rings: IrqLock<Rings>,
    iface: AtomicI32,
    has_irq: AtomicBool,
    is_8125: bool,
    mac: [u8; 6],
    /// Asleep: the card is left alone.
    paused: AtomicBool,
}

impl Nic {
    /// Sets the card up, after [`reset`], to receive and send (interrupts
    /// masked): fresh rings, address, filters.
    fn configure(&self, r: &mut Rings) {
        let regs = &self.regs;
        self.tx_ring.as_mut_slice().fill(0);
        for i in 0..RX_COUNT {
            self.give_rx(i);
        }
        // The last transmit descriptor closes the ring.
        self.tx_ring.write::<u32>((TX_COUNT - 1) * 16, EOR);
        *r = Rings { rx_next: 0, tx_next: 0 };

        regs.write8(CFG9346, 0xC0); // unlock the configuration registers
        // The address again: some firmware leaves another one after sleep.
        let m = self.mac;
        regs.write32(IDR + 4, m[4] as u32 | (m[5] as u32) << 8);
        regs.write32(IDR, u32::from_le_bytes([m[0], m[1], m[2], m[3]]));
        // Checksums are left to the network stack; VLAN tags stay in the frame.
        let cplus = regs.read16(CPLUS_CMD) & !((1 << 5) | (1 << 6));
        regs.write16(CPLUS_CMD, cplus | (1 << 3)); // PCI multiple read/write
        if !self.is_8125 {
            regs.write16(INTR_MITIGATE, 0);
        }
        regs.write16(RX_MAX_SIZE, BUF as u16);
        regs.write8(MAX_TX_SIZE, 0x3B);
        regs.write32(TX_DESC, self.tx_ring.phys() as u32);
        regs.write32(TX_DESC + 4, (self.tx_ring.phys() >> 32) as u32);
        regs.write32(RX_DESC, self.rx_ring.phys() as u32);
        regs.write32(RX_DESC + 4, (self.rx_ring.phys() >> 32) as u32);
        // Older RTL8169s take the settings below only once both units run,
        // the others only before: write them on both sides.
        let configure = || {
            if self.is_8125 {
                // Default receive fetch, unlimited DMA bursts, own address,
                // broadcast and multicast; automatic transmit FIFO.
                regs.write32(RX_CONFIG, 8 << 27 | 7 << 8 | 0b1110);
                regs.write32(TX_CONFIG, 3 << 24 | 7 << 8 | 1 << 7);
                return;
            }
            // Own address, broadcast, multicast; unlimited DMA bursts; bits
            // 13-15 are "no FIFO threshold" on the RTL8169 and "multiple and
            // 128-byte interrupt" on the RTL8168, both wanted. Revision bits
            // above stay as the chip has them.
            regs.write32(RX_CONFIG, (regs.read32(RX_CONFIG) & 0xFFFF_1800) | (7 << 13) | (7 << 8) | 0b1110);
            // Standard inter-frame gap, unlimited DMA bursts.
            regs.write32(TX_CONFIG, (3 << 24) | (7 << 8));
        };
        configure();
        regs.write8(CHIP_CMD, CMD_RX | CMD_TX);
        configure();
        for i in 0..2 {
            regs.write32(MAR + i * 4, u32::MAX); // every multicast group
        }
        regs.write8(CFG9346, 0);
    }

    fn unmask(&self) {
        if self.has_irq.load(Ordering::Relaxed) {
            self.ack(u32::MAX);
            self.set_mask(INTERRUPTS as u32);
        }
    }

    fn interrupt_status(&self) -> u32 {
        if self.is_8125 {
            self.regs.read32(INTR_STATUS_8125)
        } else {
            self.regs.read16(INTR_STATUS) as u32
        }
    }

    /// Clears the given causes (write 1 to clear).
    fn ack(&self, cause: u32) {
        if self.is_8125 {
            self.regs.write32(INTR_STATUS_8125, cause);
        } else {
            self.regs.write16(INTR_STATUS, cause as u16);
        }
    }

    fn set_mask(&self, mask: u32) {
        if self.is_8125 {
            self.regs.write32(INTR_MASK_8125, mask);
        } else {
            self.regs.write16(INTR_MASK, mask as u16);
        }
    }

    /// Hands receive descriptor `i` back to the card.
    fn give_rx(&self, i: usize) {
        let at = i * 16;
        self.rx_ring.write::<u64>(at + 8, self.rx_bufs.phys() + (i * BUF) as u64);
        self.rx_ring.write::<u32>(at + 4, 0);
        fence(Ordering::SeqCst);
        let eor = if i == RX_COUNT - 1 { EOR } else { 0 };
        self.rx_ring.write::<u32>(at, OWN | eor | BUF as u32);
    }

    fn receive(&self) {
        if self.paused.load(Ordering::Acquire) {
            return;
        }
        let iface = self.iface.load(Ordering::Relaxed);
        let mut r = self.rings.lock();
        for _ in 0..RX_COUNT {
            let i = r.rx_next;
            let status = self.rx_ring.read::<u32>(i * 16);
            if status & OWN != 0 {
                break;
            }
            fence(Ordering::SeqCst);
            // The length includes the CRC.
            let len = (status & 0x3FFF) as usize;
            let whole = status & (FS | LS) == FS | LS;
            if status & RES == 0 && whole && (64..=MAX_FRAME).contains(&len) && iface >= 0 {
                justdrv::net_receive(iface as u32, &self.rx_bufs.as_slice()[i * BUF..i * BUF + len - 4]);
            }
            self.give_rx(i);
            r.rx_next = (i + 1) % RX_COUNT;
        }
    }

    fn send(&self, frame: &[u8]) -> Status {
        if frame.is_empty() || frame.len() > MAX_FRAME - 4 {
            return E_INVAL;
        }
        if self.paused.load(Ordering::Acquire) {
            return E_IO;
        }
        let start = justdrv::time_ms();
        let mut r = loop {
            let r = self.rings.lock();
            if self.tx_ring.read::<u32>(r.tx_next * 16) & OWN == 0 {
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
        // Pad short frames: not every revision does it by itself.
        let len = frame.len().max(60);
        let buf = &mut self.tx_bufs.as_mut_slice()[i * BUF..i * BUF + len];
        buf[..frame.len()].copy_from_slice(frame);
        buf[frame.len()..].fill(0);
        self.tx_ring.write::<u64>(at + 8, self.tx_bufs.phys() + (i * BUF) as u64);
        self.tx_ring.write::<u32>(at + 4, 0);
        fence(Ordering::SeqCst);
        let eor = if i == TX_COUNT - 1 { EOR } else { 0 };
        self.tx_ring.write::<u32>(at, OWN | eor | FS | LS | len as u32);
        r.tx_next = (i + 1) % TX_COUNT;
        fence(Ordering::SeqCst);
        if self.is_8125 {
            self.regs.write16(TX_POLL_8125, 1);
        } else {
            self.regs.write8(TX_POLL, POLL_NORMAL);
        }
        OK
    }

    fn link_up(&self) -> bool {
        self.regs.read8(PHY_STATUS) & (1 << 1) != 0
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
    let cause = nic.interrupt_status();
    if cause == 0 || cause == 0xFFFF || cause == u32::MAX {
        return; // not ours (shared line), or the card is gone
    }
    nic.ack(cause);
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
    nic.set_mask(0);
    nic.regs.write8(CHIP_CMD, 0);
    OK
}

/// After sleep: the card set up again.
unsafe extern "C" fn resume(ctx: *mut c_void) -> Status {
    // SAFETY: the leaked Nic.
    let nic = unsafe { &*(ctx as *const Nic) };
    let st = reset(&nic.regs, nic.is_8125);
    if st == OK {
        nic.configure(&mut nic.rings.lock());
        nic.paused.store(false, Ordering::Release);
        nic.unmask();
    }
    st
}

fn reset(regs: &Regs, is_8125: bool) -> Status {
    if is_8125 {
        regs.write32(INTR_MASK_8125, 0);
    } else {
        regs.write16(INTR_MASK, 0);
    }
    regs.write8(CHIP_CMD, CMD_RESET);
    if !justdrv::wait_until(100, || regs.read8(CHIP_CMD) & CMD_RESET == 0) {
        justdrv::warn!("the card does not come out of reset");
        return E_IO;
    }
    OK
}

fn probe(dev: &DeviceInfo) -> Status {
    let Some(regs) = Regs::find(dev, true, 256) else {
        return E_NODEV;
    };
    let is_8125 = matches!(dev.device, 0x8125 | 0x3000);
    let st = reset(&regs, is_8125);
    if st != OK {
        return st;
    }
    let mut mac = [0u8; 6];
    for (i, b) in mac.iter_mut().enumerate() {
        *b = regs.read8(IDR + i as u16);
    }

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
        is_8125,
        mac,
        paused: AtomicBool::new(false),
    }));
    nic.configure(&mut nic.rings.lock());

    let ctx = nic as *const Nic as *mut c_void;
    let desc: &'static NetDesc = Box::leak(Box::new(NetDesc { ctx, mac, _pad: [0; 2], mtu: 1500, send }));
    let iface = justdrv::net_register(dev.id, desc);
    if iface < 0 {
        regs.write8(CHIP_CMD, 0);
        return iface;
    }
    nic.iface.store(iface, Ordering::Relaxed);
    if justdrv::irq_attach_any(dev, irq, ctx) {
        nic.has_irq.store(true, Ordering::Relaxed);
        nic.unmask();
    }
    // Interrupt routing can be wrong on some boards; poll as a safety net.
    justdrv::spawn("rtl8169-rx", poller, ctx);
    justdrv::power_hooks(dev.id, ctx, Some(suspend), resume);
    justdrv::info!(
        "link {}, {}",
        if nic.link_up() { "up" } else { "down" },
        if nic.has_irq.load(Ordering::Relaxed) { "interrupts" } else { "polling" }
    );
    OK
}

//! Realtek RTL8139 Fast Ethernet driver.
//!
//! Frames arrive one after another in a single 32 KiB ring (the card
//! writes a 4-byte header before each); transmission uses the card's four
//! fixed slots in turn. Every card found gets its own state.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use arcdrv::{DeviceInfo, Dma, IrqLock, NetDesc, Regs, Status, E_INVAL, E_IO, E_NODEV, E_NOMEM, OK};
use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicI32, Ordering};

arcdrv::driver! {
    name: "rtl8139",
    version: "1.1.0",
    probe: probe,
}

const IDR: u16 = 0x00;
const MAR: u16 = 0x08;
const TSD: u16 = 0x10;
const TSAD: u16 = 0x20;
const RBSTART: u16 = 0x30;
const CR: u16 = 0x37;
const CAPR: u16 = 0x38;
const IMR: u16 = 0x3C;
const ISR: u16 = 0x3E;
const TCR: u16 = 0x40;
const RCR: u16 = 0x44;
const MPC: u16 = 0x4C;
const CONFIG1: u16 = 0x52;
const MSR: u16 = 0x58;

const CR_RESET: u8 = 1 << 4;
const CR_RX: u8 = 1 << 3;
const CR_TX: u8 = 1 << 2;
const CR_BUFE: u8 = 1 << 0;

const TSD_OWN: u32 = 1 << 13;
const TSD_ABORT: u32 = 1 << 30;

/// Receive ok, receive error, transmit ok, transmit error, ring overflow,
/// link change, FIFO overflow.
const INTERRUPTS: u16 = 0x007F;

/// 32 KiB ring (RBLEN = 2), plus the 16 bytes the card wants after it and
/// room for one frame written past the end (WRAP = 1).
const RING: usize = 32 * 1024;
const RING_ALLOC: usize = RING + 16 + 2048;
const TX_SLOTS: usize = 4;
const TX_BUF: usize = 2048;
/// Largest Ethernet frame, with its CRC.
const MAX_FRAME: usize = 1518;

struct State {
    rx_at: usize,
    tx_next: usize,
    tx_used: [bool; TX_SLOTS],
}

struct Nic {
    regs: Regs,
    rx: Dma,
    tx: Dma,
    state: IrqLock<State>,
    iface: AtomicI32,
    has_irq: AtomicBool,
    /// Asleep: the card is left alone.
    paused: AtomicBool,
}

impl Nic {
    /// Sets the card up, after [`reset`], to receive and send (interrupts
    /// masked).
    fn configure(&self, st: &mut State) {
        let regs = &self.regs;
        regs.write32(RBSTART, self.rx.phys() as u32);
        for i in 0..8 {
            regs.write8(MAR + i, 0xFF); // every multicast group
        }
        // The receive and transmit settings only hold with both units on.
        regs.write8(CR, CR_RX | CR_TX);
        self.set_rcr();
        // Standard inter-frame gap, 2 KiB DMA bursts.
        regs.write32(TCR, (3 << 24) | (7 << 8));
        regs.write16(CAPR, 0u16.wrapping_sub(16));
        regs.write32(MPC, 0);
        *st = State { rx_at: 0, tx_next: 0, tx_used: [false; TX_SLOTS] };
    }

    fn unmask(&self) {
        if self.has_irq.load(Ordering::Relaxed) {
            self.regs.write16(ISR, 0xFFFF);
            self.regs.write16(IMR, INTERRUPTS);
        }
    }

    fn receive(&self) {
        if self.paused.load(Ordering::Acquire) {
            return;
        }
        let iface = self.iface.load(Ordering::Relaxed);
        let mut st = self.state.lock();
        let ring = self.rx.as_slice();
        let mut errors = false;
        while self.regs.read8(CR) & CR_BUFE == 0 {
            let at = st.rx_at;
            let status = u16::from_le_bytes([ring[at], ring[at + 1]]);
            let len = u16::from_le_bytes([ring[at + 2], ring[at + 3]]) as usize;
            if status & 1 == 0 || !(64..=MAX_FRAME).contains(&len) {
                // A bad header: the ring can no longer be trusted.
                errors = true;
                break;
            }
            if iface >= 0 {
                // The length counts the CRC, which is not passed on.
                arcdrv::net_receive(iface as u32, &ring[at + 4..at + len]);
            }
            st.rx_at = (at + len + 4 + 3) & !3;
            if st.rx_at >= RING {
                st.rx_at -= RING;
            }
            // CAPR trails the read position by 16, as the card expects.
            self.regs.write16(CAPR, (st.rx_at as u16).wrapping_sub(16));
        }
        if errors {
            self.restart_rx(&mut st);
        }
    }

    fn restart_rx(&self, st: &mut State) {
        let cr = self.regs.read8(CR);
        self.regs.write8(CR, cr & !CR_RX);
        self.regs.write32(RBSTART, self.rx.phys() as u32);
        self.regs.write8(CR, cr | CR_RX);
        self.set_rcr();
        st.rx_at = 0;
        self.regs.write16(CAPR, 0u16.wrapping_sub(16));
    }

    fn set_rcr(&self) {
        // Own address, broadcast, multicast; 32 KiB ring with WRAP, no
        // FIFO threshold, unlimited DMA bursts.
        self.regs.write32(RCR, (1 << 1) | (1 << 2) | (1 << 3) | (1 << 7) | (2 << 11) | (7 << 8) | (7 << 13));
    }

    fn send(&self, frame: &[u8]) -> Status {
        if frame.is_empty() || frame.len() > MAX_FRAME - 4 {
            return E_INVAL;
        }
        if self.paused.load(Ordering::Acquire) {
            return E_IO;
        }
        let start = arcdrv::time_ms();
        let mut st = loop {
            let st = self.state.lock();
            let i = st.tx_next;
            let tsd = self.regs.read32(TSD + i as u16 * 4);
            if !st.tx_used[i] || tsd & (TSD_OWN | TSD_ABORT) != 0 {
                break st;
            }
            drop(st);
            if arcdrv::time_ms() - start > 100 {
                return E_IO;
            }
            arcdrv::yield_now();
        };
        let i = st.tx_next;
        // The card does not pad short frames itself.
        let len = frame.len().max(60);
        let buf = &mut self.tx.as_mut_slice()[i * TX_BUF..i * TX_BUF + len];
        buf[..frame.len()].copy_from_slice(frame);
        buf[frame.len()..].fill(0);
        self.regs.write32(TSAD + i as u16 * 4, (self.tx.phys() + (i * TX_BUF) as u64) as u32);
        // Writing the length clears OWN and starts the transfer.
        self.regs.write32(TSD + i as u16 * 4, len as u32);
        st.tx_used[i] = true;
        st.tx_next = (i + 1) % TX_SLOTS;
        OK
    }

    fn link_up(&self) -> bool {
        self.regs.read8(MSR) & (1 << 2) == 0
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
    let cause = nic.regs.read16(ISR);
    if cause == 0 || cause == 0xFFFF {
        return; // not ours (shared line), or the card is gone
    }
    nic.regs.write16(ISR, cause); // write 1 to clear
    nic.receive();
}

unsafe extern "C" fn poller(ctx: *mut c_void) {
    // SAFETY: the leaked Nic.
    let nic = unsafe { &*(ctx as *const Nic) };
    loop {
        nic.receive();
        arcdrv::sleep_ms(if nic.has_irq.load(Ordering::Relaxed) { 50 } else { 5 });
    }
}

/// Before sleep: the card stops.
unsafe extern "C" fn suspend(ctx: *mut c_void) -> Status {
    // SAFETY: the leaked Nic.
    let nic = unsafe { &*(ctx as *const Nic) };
    nic.paused.store(true, Ordering::Release);
    let _st = nic.state.lock(); // a send or receive under way ends
    nic.regs.write16(IMR, 0);
    nic.regs.write8(CR, 0);
    OK
}

/// After sleep: the card set up again (it reloads its address itself).
unsafe extern "C" fn resume(ctx: *mut c_void) -> Status {
    // SAFETY: the leaked Nic.
    let nic = unsafe { &*(ctx as *const Nic) };
    let st = reset(&nic.regs);
    if st == OK {
        nic.configure(&mut nic.state.lock());
        nic.paused.store(false, Ordering::Release);
        nic.unmask();
    }
    st
}

fn reset(regs: &Regs) -> Status {
    regs.write8(CONFIG1, 0); // wake up
    regs.write8(CR, CR_RESET);
    if !arcdrv::wait_until(100, || regs.read8(CR) & CR_RESET == 0) {
        arcdrv::warn!("the card does not come out of reset");
        return E_IO;
    }
    regs.write16(IMR, 0);
    OK
}

fn probe(dev: &DeviceInfo) -> Status {
    let Some(regs) = Regs::find(dev, false, 256) else {
        return E_NODEV;
    };
    let st = reset(&regs);
    if st != OK {
        return st;
    }

    let mut mac = [0u8; 6];
    for (i, b) in mac.iter_mut().enumerate() {
        *b = regs.read8(IDR + i as u16);
    }
    let (Some(rx), Some(tx)) = (Dma::new(RING_ALLOC), Dma::new(TX_SLOTS * TX_BUF)) else {
        return E_NOMEM;
    };

    let nic: &'static Nic = Box::leak(Box::new(Nic {
        regs,
        rx,
        tx,
        state: IrqLock::new(State { rx_at: 0, tx_next: 0, tx_used: [false; TX_SLOTS] }),
        iface: AtomicI32::new(-1),
        has_irq: AtomicBool::new(false),
        paused: AtomicBool::new(false),
    }));
    nic.configure(&mut nic.state.lock());

    let ctx = nic as *const Nic as *mut c_void;
    let desc: &'static NetDesc = Box::leak(Box::new(NetDesc { ctx, mac, _pad: [0; 2], mtu: 1500, send }));
    let iface = arcdrv::net_register(dev.id, desc);
    if iface < 0 {
        regs.write8(CR, 0);
        return iface;
    }
    nic.iface.store(iface, Ordering::Relaxed);
    if arcdrv::irq_attach_any(dev, irq, ctx) {
        nic.has_irq.store(true, Ordering::Relaxed);
        nic.unmask();
    }
    // Interrupt routing can be wrong on some boards; poll as a safety net.
    arcdrv::spawn("rtl8139-rx", poller, ctx);
    arcdrv::power_hooks(dev.id, ctx, Some(suspend), resume);
    arcdrv::info!(
        "link {}, {}",
        if nic.link_up() { "up" } else { "down" },
        if nic.has_irq.load(Ordering::Relaxed) { "interrupts" } else { "polling" }
    );
    OK
}

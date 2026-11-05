//! AMD PCnet Ethernet driver (Am79C970A and the rest of the PCnet-PCI
//! family; VirtualBox's and QEMU's default card on older setups).
//!
//! The card runs with 32-bit registers and descriptors (software style 2):
//! an initialisation block points it at a receive and a transmit ring of
//! 2 KiB buffers, each descriptor owned by the card until it is done.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use justdrv::{port, DeviceInfo, Dma, IrqLock, NetDesc, Status, E_INVAL, E_IO, E_NODEV, E_NOMEM, OK};
use core::ffi::c_void;
use core::sync::atomic::{fence, AtomicBool, AtomicI32, Ordering};

justdrv::driver! {
    name: "pcnet",
    version: "1.1.0",
    probe: probe,
}

// Registers in 32-bit (DWIO) mode.
const RDP: u16 = 0x10;
const RAP: u16 = 0x14;
const RESET: u16 = 0x18;
const BDP: u16 = 0x1C;
// The same in 16-bit mode, needed until the card is switched.
const RESET16: u16 = 0x14;

const CSR0_INIT: u32 = 1 << 0;
const CSR0_STRT: u32 = 1 << 1;
const CSR0_STOP: u32 = 1 << 2;
const CSR0_TDMD: u32 = 1 << 3;
const CSR0_IENA: u32 = 1 << 6;
const CSR0_INTR: u32 = 1 << 7;
const CSR0_IDON: u32 = 1 << 8;
/// Every interrupt cause (write 1 to clear): BABL, CERR, MISS, MERR, RINT,
/// TINT, IDON.
const CSR0_CAUSES: u32 = 0x7F00;

const OWN: u32 = 1 << 31;
const ERR: u32 = 1 << 30;
const STP: u32 = 1 << 25;
const ENP: u32 = 1 << 24;

const RX_LOG2: u32 = 7;
const TX_LOG2: u32 = 4;
const RX_COUNT: usize = 1 << RX_LOG2;
const TX_COUNT: usize = 1 << TX_LOG2;
const BUF: usize = 2048;
const MAX_FRAME: usize = 1518;

struct State {
    rx_next: usize,
    tx_next: usize,
}

struct Nic {
    io: u16,
    /// Initialisation block, then the receive and the transmit ring.
    rings: Dma,
    rx_bufs: Dma,
    tx_bufs: Dma,
    state: IrqLock<State>,
    iface: AtomicI32,
    has_irq: AtomicBool,
    mac: [u8; 6],
    /// Asleep: the card is left alone.
    paused: AtomicBool,
}

const INIT_BLOCK: usize = 0;
const RX_RING: usize = 32;
const TX_RING: usize = RX_RING + RX_COUNT * 16;

/// Resets the card (either width); it comes back in 16-bit mode.
fn reset(io: u16) {
    port::inl(io + RESET);
    port::inw(io + RESET16);
    justdrv::sleep_ms(2);
}

impl Nic {
    /// After [`reset`]: 32-bit registers and descriptors, fresh rings, the
    /// initialisation block taken. The card is left stopped.
    fn init(&self) -> Status {
        // A 32-bit write to RDP switches to 32-bit registers.
        port::outl(self.io + RDP, 0);
        self.set_csr(0, CSR0_STOP);
        // Software style 2: 32-bit descriptors and initialisation block.
        self.set_bcr(20, (self.bcr(20) & !0xFF) | 2);
        // Pick the cable automatically.
        self.set_bcr(2, self.bcr(2) | (1 << 1));

        let ib = &self.rings;
        ib.as_mut_slice().fill(0);
        for i in 0..RX_COUNT {
            let at = RX_RING + i * 16;
            ib.write::<u32>(at, (self.rx_bufs.phys() + (i * BUF) as u64) as u32);
            self.give_rx(i);
        }
        ib.write::<u16>(INIT_BLOCK, 0); // normal mode
        ib.write::<u8>(INIT_BLOCK + 2, (RX_LOG2 << 4) as u8);
        ib.write::<u8>(INIT_BLOCK + 3, (TX_LOG2 << 4) as u8);
        for (i, b) in self.mac.iter().enumerate() {
            ib.write::<u8>(INIT_BLOCK + 4 + i, *b);
        }
        ib.write::<u64>(INIT_BLOCK + 12, u64::MAX); // every multicast group
        ib.write::<u32>(INIT_BLOCK + 20, (ib.phys() + RX_RING as u64) as u32);
        ib.write::<u32>(INIT_BLOCK + 24, (ib.phys() + TX_RING as u64) as u32);

        let block = ib.phys() + INIT_BLOCK as u64;
        self.set_csr(1, block as u32 & 0xFFFF);
        self.set_csr(2, (block >> 16) as u32 & 0xFFFF);
        // No interrupt for "initialisation done"; pad short frames.
        self.set_csr(3, 1 << 10);
        self.set_csr(4, self.csr(4) | (1 << 11));
        self.set_csr(0, CSR0_INIT);
        if !justdrv::wait_until(100, || self.csr(0) & CSR0_IDON != 0) {
            justdrv::warn!("the card does not take its settings");
            return E_IO;
        }
        self.set_csr(0, CSR0_IDON);
        OK
    }

    fn start(&self) {
        let irq = self.has_irq.load(Ordering::Relaxed);
        self.set_csr(0, CSR0_STRT | if irq { CSR0_IENA } else { 0 });
    }

    fn csr(&self, n: u32) -> u32 {
        port::outl(self.io + RAP, n);
        port::inl(self.io + RDP)
    }

    fn set_csr(&self, n: u32, v: u32) {
        port::outl(self.io + RAP, n);
        port::outl(self.io + RDP, v);
    }

    fn bcr(&self, n: u32) -> u32 {
        port::outl(self.io + RAP, n);
        port::inl(self.io + BDP)
    }

    fn set_bcr(&self, n: u32, v: u32) {
        port::outl(self.io + RAP, n);
        port::outl(self.io + BDP, v);
    }

    /// Hands receive descriptor `i` back to the card.
    fn give_rx(&self, i: usize) {
        let at = RX_RING + i * 16;
        self.rings.write::<u32>(at + 8, 0);
        fence(Ordering::SeqCst);
        // Buffer size as a negative 12-bit count; bits 12-15 must be set.
        self.rings.write::<u32>(at + 4, OWN | 0xF000 | (BUF as u32).wrapping_neg() & 0xFFF);
    }

    fn receive(&self) {
        if self.paused.load(Ordering::Acquire) {
            return;
        }
        let iface = self.iface.load(Ordering::Relaxed);
        let mut st = self.state.lock();
        for _ in 0..RX_COUNT {
            let i = st.rx_next;
            let at = RX_RING + i * 16;
            let flags = self.rings.read::<u32>(at + 4);
            if flags & OWN != 0 {
                break;
            }
            fence(Ordering::SeqCst);
            // The count includes the CRC.
            let len = (self.rings.read::<u32>(at + 8) & 0xFFF) as usize;
            let whole = flags & (STP | ENP) == STP | ENP;
            if flags & ERR == 0 && whole && (64..=MAX_FRAME).contains(&len) && iface >= 0 {
                justdrv::net_receive(iface as u32, &self.rx_bufs.as_slice()[i * BUF..i * BUF + len - 4]);
            }
            self.give_rx(i);
            st.rx_next = (i + 1) % RX_COUNT;
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
        let mut st = loop {
            let st = self.state.lock();
            let at = TX_RING + st.tx_next * 16;
            if self.rings.read::<u32>(at + 4) & OWN == 0 {
                break st;
            }
            drop(st);
            if justdrv::time_ms() - start > 100 {
                return E_IO;
            }
            justdrv::yield_now();
        };
        let i = st.tx_next;
        let at = TX_RING + i * 16;
        self.tx_bufs.as_mut_slice()[i * BUF..i * BUF + frame.len()].copy_from_slice(frame);
        self.rings.write::<u32>(at, (self.tx_bufs.phys() + (i * BUF) as u64) as u32);
        self.rings.write::<u32>(at + 8, 0);
        fence(Ordering::SeqCst);
        self.rings.write::<u32>(at + 4, OWN | STP | ENP | 0xF000 | (frame.len() as u32).wrapping_neg() & 0xFFF);
        st.tx_next = (i + 1) % TX_COUNT;
        // Look at the ring now instead of at the next poll.
        self.set_csr(0, CSR0_TDMD | if self.has_irq.load(Ordering::Relaxed) { CSR0_IENA } else { 0 });
        OK
    }

    fn link_up(&self) -> bool {
        // BCR4 bit 15: link status (on the cards that have it).
        self.bcr(4) & (1 << 15) != 0
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
    if nic.paused.load(Ordering::Acquire) {
        return; // stopped, with interrupts off
    }
    let csr0 = {
        let _st = nic.state.lock(); // RAP is shared with send()
        let csr0 = nic.csr(0);
        if csr0 & CSR0_INTR == 0 {
            return; // not ours (shared line)
        }
        nic.set_csr(0, (csr0 & CSR0_CAUSES) | CSR0_IENA);
        csr0
    };
    if csr0 & (1 << 10) != 0 || csr0 & (1 << 12) != 0 {
        nic.receive();
    }
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
    let _st = nic.state.lock(); // a send or receive under way ends
    nic.set_csr(0, CSR0_STOP);
    OK
}

/// After sleep: the card set up again.
unsafe extern "C" fn resume(ctx: *mut c_void) -> Status {
    // SAFETY: the leaked Nic.
    let nic = unsafe { &*(ctx as *const Nic) };
    reset(nic.io);
    let st = nic.init();
    if st == OK {
        *nic.state.lock() = State { rx_next: 0, tx_next: 0 };
        nic.paused.store(false, Ordering::Release);
        nic.start();
    }
    st
}

fn probe(dev: &DeviceInfo) -> Status {
    let Some(io) = dev.io(0) else { return E_NODEV };
    justdrv::Pci(dev.id).enable(true, false, true);

    reset(io);
    // The address PROM can be read byte by byte only in 16-bit mode.
    let mut mac = [0u8; 6];
    for (i, b) in mac.iter_mut().enumerate() {
        *b = port::inb(io + i as u16);
    }
    let (Some(rings), Some(rx_bufs), Some(tx_bufs)) =
        (Dma::new(TX_RING + TX_COUNT * 16), Dma::new(RX_COUNT * BUF), Dma::new(TX_COUNT * BUF))
    else {
        return E_NOMEM;
    };
    let nic: &'static Nic = Box::leak(Box::new(Nic {
        io,
        rings,
        rx_bufs,
        tx_bufs,
        state: IrqLock::new(State { rx_next: 0, tx_next: 0 }),
        iface: AtomicI32::new(-1),
        has_irq: AtomicBool::new(false),
        mac,
        paused: AtomicBool::new(false),
    }));
    let st = nic.init();
    if st != OK {
        return st;
    }

    let ctx = nic as *const Nic as *mut c_void;
    let desc: &'static NetDesc = Box::leak(Box::new(NetDesc { ctx, mac, _pad: [0; 2], mtu: 1500, send }));
    let iface = justdrv::net_register(dev.id, desc);
    if iface < 0 {
        nic.set_csr(0, CSR0_STOP);
        return iface;
    }
    nic.iface.store(iface, Ordering::Relaxed);
    let has_irq = justdrv::irq_attach_any(dev, irq, ctx);
    nic.has_irq.store(has_irq, Ordering::Relaxed);
    nic.start();
    // Interrupt routing can be wrong on some boards; poll as a safety net.
    justdrv::spawn("pcnet-rx", poller, ctx);
    justdrv::power_hooks(dev.id, ctx, Some(suspend), resume);
    justdrv::info!("link {}, {}", if nic.link_up() { "up" } else { "down" }, if has_irq { "interrupts" } else { "polling" });
    OK
}

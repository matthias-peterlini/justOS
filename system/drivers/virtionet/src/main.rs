//! virtio network driver.
//!
//! Queue 0 receives into a ring of 2 KiB buffers that are handed back as
//! soon as their frame is passed on; queue 1 transmits from a set of slots
//! that are reused once the device is done with them. Works on the legacy
//! and the modern virtio interface.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use arcdrv::{DeviceInfo, Dma, IrqLock, NetDesc, Status, E_INVAL, E_IO, E_NODEV, E_NOMEM, OK};
use arcvirtio::{Queue, Virtio, DESC_WRITE};
use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicI32, Ordering};

arcdrv::driver! {
    name: "virtionet",
    version: "1.1.0",
    probe: probe,
}

/// The device has a MAC address in its configuration.
const F_MAC: u64 = 1 << 5;

const BUF: usize = 2048;
const MAX_RX: u16 = 128;
const MAX_TX: u16 = 64;

struct Tx {
    queue: Queue,
    count: u16,
    busy: [bool; MAX_TX as usize],
    next: u16,
}

struct Net {
    vio: Virtio,
    rx: IrqLock<Queue>,
    rx_count: u16,
    rx_bufs: Dma,
    tx: IrqLock<Tx>,
    tx_bufs: Dma,
    /// Size of the header in front of every frame (10 legacy, 12 modern).
    header: usize,
    iface: AtomicI32,
    has_irq: AtomicBool,
    /// The features agreed on (again after sleep).
    features: u64,
    /// Asleep: the queues are left alone.
    paused: AtomicBool,
}

impl Net {
    /// Every receive buffer offered to the device.
    fn fill_rx(&self, rx: &mut Queue) {
        for i in 0..self.rx_count {
            rx.desc(i, self.rx_bufs.phys() + (i as usize * BUF) as u64, BUF as u32, DESC_WRITE, 0);
            rx.publish(i);
        }
    }

    fn receive(&self) {
        if self.paused.load(Ordering::Acquire) {
            return;
        }
        let iface = self.iface.load(Ordering::Relaxed);
        let mut rx = self.rx.lock();
        let mut returned = false;
        while let Some((id, len)) = rx.take_used() {
            if id >= self.rx_count {
                continue;
            }
            let len = (len as usize).min(BUF);
            if iface >= 0 && len > self.header {
                let at = id as usize * BUF;
                arcdrv::net_receive(iface as u32, &self.rx_bufs.as_slice()[at + self.header..at + len]);
            }
            rx.publish(id);
            returned = true;
        }
        if returned {
            self.vio.notify(&rx);
        }
    }

    fn reclaim(tx: &mut Tx) {
        while let Some((id, _)) = tx.queue.take_used() {
            if let Some(b) = tx.busy.get_mut(id as usize) {
                *b = false;
            }
        }
    }

    fn send(&self, frame: &[u8]) -> Status {
        if frame.is_empty() || frame.len() + self.header > BUF {
            return E_INVAL;
        }
        if self.paused.load(Ordering::Acquire) {
            return E_IO;
        }
        let start = arcdrv::time_ms();
        let mut tx = loop {
            let mut tx = self.tx.lock();
            Self::reclaim(&mut tx);
            if !tx.busy[tx.next as usize] {
                break tx;
            }
            drop(tx);
            if arcdrv::time_ms() - start > 100 {
                return E_IO;
            }
            arcdrv::yield_now();
        };
        let slot = tx.next;
        tx.next = (slot + 1) % tx.count;
        tx.busy[slot as usize] = true;
        let at = slot as usize * BUF;
        let buf = &mut self.tx_bufs.as_mut_slice()[at..at + self.header + frame.len()];
        buf[..self.header].fill(0);
        buf[self.header..].copy_from_slice(frame);
        tx.queue.desc(slot, self.tx_bufs.phys() + at as u64, (self.header + frame.len()) as u32, 0, 0);
        tx.queue.publish(slot);
        self.vio.notify(&tx.queue);
        OK
    }
}

unsafe extern "C" fn send(ctx: *mut c_void, frame: *const u8, len: usize) -> Status {
    // SAFETY: ctx is the leaked Net; the kernel passes a valid frame.
    let net = unsafe { &*(ctx as *const Net) };
    net.send(unsafe { core::slice::from_raw_parts(frame, len) })
}

unsafe extern "C" fn irq(ctx: *mut c_void) {
    // SAFETY: the leaked Net.
    let net = unsafe { &*(ctx as *const Net) };
    if !net.vio.uses_msix() && net.vio.isr() == 0 {
        return; // not ours (shared line)
    }
    net.receive();
}

unsafe extern "C" fn poller(ctx: *mut c_void) {
    // SAFETY: the leaked Net.
    let net = unsafe { &*(ctx as *const Net) };
    loop {
        net.receive();
        // Interrupts can be routed wrongly on some machines: poll anyway.
        arcdrv::sleep_ms(if net.has_irq.load(Ordering::Relaxed) { 50 } else { 5 });
    }
}

/// Before sleep: the device stops.
unsafe extern "C" fn suspend(ctx: *mut c_void) -> Status {
    // SAFETY: the leaked Net.
    let net = unsafe { &*(ctx as *const Net) };
    net.paused.store(true, Ordering::Release);
    // Wait for a receive or send under way.
    drop(net.rx.lock());
    drop(net.tx.lock());
    net.vio.reset();
    OK
}

/// After sleep: the device set up again, the receive buffers offered anew.
unsafe extern "C" fn resume(ctx: *mut c_void) -> Status {
    // SAFETY: the leaked Net.
    let net = unsafe { &*(ctx as *const Net) };
    net.paused.store(true, Ordering::Release);
    if net.vio.negotiate(net.features).is_none() {
        return E_IO;
    }
    {
        let mut rx = net.rx.lock();
        net.vio.requeue(&mut rx);
        net.fill_rx(&mut rx);
    }
    {
        let mut tx = net.tx.lock();
        net.vio.requeue(&mut tx.queue);
        tx.busy = [false; MAX_TX as usize];
        tx.next = 0;
    }
    net.vio.reroute();
    net.vio.ready();
    net.vio.notify(&net.rx.lock());
    net.paused.store(false, Ordering::Release);
    OK
}

fn probe(dev: &DeviceInfo) -> Status {
    if dev.vendor != 0x1AF4 || !(dev.device == 0x1000 || dev.device == 0x1041) {
        return E_NODEV;
    }
    let Some(vio) = Virtio::open(dev) else {
        arcdrv::warn!("no usable virtio registers");
        return E_IO;
    };
    let Some(features) = vio.negotiate(F_MAC) else { return E_IO };
    let header = if vio.is_modern() { 12 } else { 10 };
    let (Some(rx), Some(tx)) = (vio.queue(0, MAX_RX), vio.queue(1, MAX_TX)) else {
        vio.set_status(arcvirtio::STATUS_FAILED);
        return E_IO;
    };
    let rx_count = rx.size.min(MAX_RX);
    let tx_count = tx.size.min(MAX_TX);
    let (Some(rx_bufs), Some(tx_bufs)) = (Dma::new(rx_count as usize * BUF), Dma::new(tx_count as usize * BUF)) else {
        return E_NOMEM;
    };
    let mut mac = [0u8; 6];
    if features & F_MAC != 0 {
        for (i, b) in mac.iter_mut().enumerate() {
            *b = vio.config8(i);
        }
    } else {
        // No address given: a locally administered one from the PCI slot.
        mac = [0x02, 0xA4, 0xC0, (dev.id >> 16) as u8, (dev.id >> 8) as u8, dev.id as u8];
    }

    let net: &'static Net = Box::leak(Box::new(Net {
        vio,
        rx: IrqLock::new(rx),
        rx_count,
        rx_bufs,
        tx: IrqLock::new(Tx { queue: tx, count: tx_count, busy: [false; MAX_TX as usize], next: 0 }),
        tx_bufs,
        header,
        iface: AtomicI32::new(-1),
        has_irq: AtomicBool::new(false),
        features,
        paused: AtomicBool::new(false),
    }));
    net.fill_rx(&mut net.rx.lock());
    let ctx = net as *const Net as *mut c_void;
    net.has_irq.store(net.vio.attach_irq(dev, irq, ctx), Ordering::Relaxed);
    net.vio.ready();
    net.vio.notify(&net.rx.lock());

    let desc: &'static NetDesc = Box::leak(Box::new(NetDesc { ctx, mac, _pad: [0; 2], mtu: 1500, send }));
    let iface = arcdrv::net_register(dev.id, desc);
    if iface < 0 {
        return iface;
    }
    net.iface.store(iface, Ordering::Relaxed);
    arcdrv::spawn("virtionet-rx", poller, ctx);
    arcdrv::power_hooks(dev.id, ctx, Some(suspend), resume);
    arcdrv::info!(
        "{} interface, {}",
        if net.vio.is_modern() { "modern" } else { "legacy" },
        if net.has_irq.load(Ordering::Relaxed) { "interrupts" } else { "polling" }
    );
    OK
}

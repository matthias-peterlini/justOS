//! virtio over PCI for arcOS drivers.
//!
//! Hides the two ways a virtio device shows itself on PCI:
//!
//! * **legacy** (virtio 0.9.5): registers in I/O BAR 0; every QEMU and
//!   VirtualBox device that is not "modern only" has them;
//! * **modern** (virtio 1.0+): register blocks in memory BARs, found
//!   through vendor capabilities; the only interface of "modern only"
//!   devices (cloud VMs, `disable-legacy=on`) and of the 0x104x device ids.
//!
//! A driver opens the device, negotiates features, sets up its queues,
//! attaches its interrupt and calls [`Virtio::ready`]. [`Queue`] is a split
//! virtqueue: the driver writes descriptors, publishes chains and collects
//! used buffers.

#![no_std]

use arcdrv::{port, DeviceInfo, Dma, IrqHandler, Mmio, Pci, IRQ_NONE, OK, RES_IO, RES_MEMORY};
use core::ffi::c_void;
use core::sync::atomic::{fence, AtomicBool, AtomicU16, Ordering};

pub const STATUS_ACK: u8 = 1;
pub const STATUS_DRIVER: u8 = 2;
pub const STATUS_DRIVER_OK: u8 = 4;
pub const STATUS_FEATURES_OK: u8 = 8;
pub const STATUS_FAILED: u8 = 0x80;

/// The device follows virtio 1.0 (required on the modern interface).
pub const F_VERSION_1: u64 = 1 << 32;

pub const DESC_NEXT: u16 = 1;
pub const DESC_WRITE: u16 = 2;

// Legacy registers (I/O BAR 0)
const L_DEVICE_FEATURES: u16 = 0x00;
const L_GUEST_FEATURES: u16 = 0x04;
const L_QUEUE_PFN: u16 = 0x08;
const L_QUEUE_SIZE: u16 = 0x0C;
const L_QUEUE_SELECT: u16 = 0x0E;
const L_QUEUE_NOTIFY: u16 = 0x10;
const L_STATUS: u16 = 0x12;
const L_ISR: u16 = 0x13;
const L_CONFIG: u16 = 0x14;

// Modern common configuration
const C_DEVICE_FEATURE_SELECT: usize = 0x00;
const C_DEVICE_FEATURE: usize = 0x04;
const C_DRIVER_FEATURE_SELECT: usize = 0x08;
const C_DRIVER_FEATURE: usize = 0x0C;
const C_MSIX_CONFIG: usize = 0x10;
const C_STATUS: usize = 0x14;
const C_QUEUE_SELECT: usize = 0x16;
const C_QUEUE_SIZE: usize = 0x18;
const C_QUEUE_MSIX_VECTOR: usize = 0x1A;
const C_QUEUE_ENABLE: usize = 0x1C;
const C_QUEUE_NOTIFY_OFF: usize = 0x1E;
const C_QUEUE_DESC: usize = 0x20;
const C_QUEUE_DRIVER: usize = 0x28;
const C_QUEUE_DEVICE: usize = 0x30;

const CAP_VENDOR: u8 = 0x09;
const CAP_MSIX: u8 = 0x11;
const CFG_COMMON: u8 = 1;
const CFG_NOTIFY: u8 = 2;
const CFG_ISR: u8 = 3;
const CFG_DEVICE: u8 = 4;

enum Regs {
    Legacy { io: u16 },
    Modern { common: Mmio, notify: Mmio, notify_mult: u32, isr: Mmio, device: Option<Mmio> },
}

/// An open virtio device.
pub struct Virtio {
    regs: Regs,
    pci: Pci,
    /// Queues set up so far (their MSI-X vectors are set on attach).
    queues: AtomicU16,
    msix: AtomicBool,
}

impl Virtio {
    /// Finds the device's registers: the modern interface if it has one,
    /// else the legacy I/O window.
    pub fn open(dev: &DeviceInfo) -> Option<Virtio> {
        let pci = Pci(dev.id);
        pci.enable(true, true, true);
        let (mut common, mut notify, mut isr, mut device) = (None, None, None, None);
        let mut notify_mult = 0;
        let mut at = 0;
        while let Some(cap) = pci.find_capability(CAP_VENDOR, at) {
            at = cap;
            let kind = pci.read8(cap + 3);
            let bar = pci.read8(cap + 4) as usize;
            let offset = pci.read32(cap + 8) as u64;
            let length = pci.read32(cap + 12) as usize;
            let Some(res) = dev.resources.get(bar).filter(|r| r.kind == RES_MEMORY && r.base != 0) else { continue };
            let map = || Mmio::map(res.base + offset, length.clamp(4, 0x10000));
            match kind {
                CFG_COMMON if common.is_none() => common = map(),
                CFG_NOTIFY if notify.is_none() => {
                    notify_mult = pci.read32(cap + 16);
                    notify = map();
                }
                CFG_ISR if isr.is_none() => isr = map(),
                CFG_DEVICE if device.is_none() => device = map(),
                _ => {}
            }
        }
        let regs = match (common, notify, isr) {
            (Some(common), Some(notify), Some(isr)) => Regs::Modern { common, notify, notify_mult, isr, device },
            _ if dev.resources[0].kind == RES_IO => Regs::Legacy { io: dev.resources[0].base as u16 },
            _ => return None,
        };
        Some(Virtio { regs, pci, queues: AtomicU16::new(0), msix: AtomicBool::new(false) })
    }

    pub fn is_modern(&self) -> bool {
        matches!(self.regs, Regs::Modern { .. })
    }

    pub fn status(&self) -> u8 {
        match &self.regs {
            Regs::Legacy { io } => port::inb(io + L_STATUS),
            Regs::Modern { common, .. } => common.read8(C_STATUS),
        }
    }

    pub fn set_status(&self, v: u8) {
        match &self.regs {
            Regs::Legacy { io } => port::outb(io + L_STATUS, v),
            Regs::Modern { common, .. } => common.write8(C_STATUS, v),
        }
    }

    pub fn reset(&self) {
        self.set_status(0);
        arcdrv::wait_until(100, || self.status() == 0);
    }

    /// Resets the device and agrees on features: returns the accepted
    /// subset of `wanted`, or None if the device refuses.
    pub fn negotiate(&self, wanted: u64) -> Option<u64> {
        self.reset();
        self.set_status(STATUS_ACK | STATUS_DRIVER);
        match &self.regs {
            Regs::Legacy { io } => {
                let offered = port::inl(io + L_DEVICE_FEATURES) as u64;
                let accepted = offered & wanted & 0xFFFF_FFFF;
                port::outl(io + L_GUEST_FEATURES, accepted as u32);
                Some(accepted)
            }
            Regs::Modern { common, .. } => {
                common.write32(C_DEVICE_FEATURE_SELECT, 0);
                let lo = common.read32(C_DEVICE_FEATURE) as u64;
                common.write32(C_DEVICE_FEATURE_SELECT, 1);
                let hi = common.read32(C_DEVICE_FEATURE) as u64;
                let offered = lo | hi << 32;
                if offered & F_VERSION_1 == 0 {
                    self.set_status(STATUS_FAILED);
                    return None;
                }
                let accepted = offered & (wanted | F_VERSION_1);
                common.write32(C_DRIVER_FEATURE_SELECT, 0);
                common.write32(C_DRIVER_FEATURE, accepted as u32);
                common.write32(C_DRIVER_FEATURE_SELECT, 1);
                common.write32(C_DRIVER_FEATURE, (accepted >> 32) as u32);
                self.set_status(STATUS_ACK | STATUS_DRIVER | STATUS_FEATURES_OK);
                if self.status() & STATUS_FEATURES_OK == 0 {
                    self.set_status(STATUS_FAILED);
                    return None;
                }
                Some(accepted)
            }
        }
    }

    /// Sets up queue `index` with at most `max` entries.
    pub fn queue(&self, index: u16, max: u16) -> Option<Queue> {
        let device_size = match &self.regs {
            Regs::Legacy { io } => {
                port::outw(io + L_QUEUE_SELECT, index);
                port::inw(io + L_QUEUE_SIZE)
            }
            Regs::Modern { common, .. } => {
                common.write16(C_QUEUE_SELECT, index);
                common.read16(C_QUEUE_SIZE)
            }
        };
        if device_size == 0 {
            return None;
        }
        // Legacy devices dictate the size; modern ones take a smaller one.
        let size = if self.is_modern() { device_size.min(max.max(1)) } else { device_size };
        if size > 4096 {
            return None;
        }
        // Descriptors, available ring, then the used ring on the next page
        // (the legacy layout; modern devices accept it as well).
        let avail = size as usize * 16;
        let used = (avail + 6 + 2 * size as usize).next_multiple_of(4096);
        let mem = Dma::new(used + 6 + 8 * size as usize)?;
        let mut q = Queue { index, size, mem, avail, used, notify_off: 0, avail_idx: 0, last_used: 0 };
        self.give(&mut q);
        self.queues.fetch_max(index + 1, Ordering::Relaxed);
        Some(q)
    }

    /// Hands a queue's memory to the device (the queue is selected).
    fn give(&self, q: &mut Queue) {
        let phys = q.mem.phys();
        match &self.regs {
            Regs::Legacy { io } => port::outl(io + L_QUEUE_PFN, (phys >> 12) as u32),
            Regs::Modern { common, .. } => {
                common.write16(C_QUEUE_SIZE, q.size);
                common.write64(C_QUEUE_DESC, phys);
                common.write64(C_QUEUE_DRIVER, phys + q.avail as u64);
                common.write64(C_QUEUE_DEVICE, phys + q.used as u64);
                common.write16(C_QUEUE_MSIX_VECTOR, 0xFFFF);
                q.notify_off = common.read16(C_QUEUE_NOTIFY_OFF);
                common.write16(C_QUEUE_ENABLE, 1);
            }
        }
    }

    /// After sleep, when the device has forgotten everything: a reset, the
    /// same `features` agreed again, and every queue handed back empty
    /// (the driver fills them again, then calls [`Virtio::ready`]).
    pub fn restore(&self, features: u64, queues: &mut [&mut Queue]) -> bool {
        if self.negotiate(features).is_none() {
            return false;
        }
        for q in queues.iter_mut() {
            self.requeue(q);
        }
        self.reroute();
        true
    }

    /// For [`Virtio::restore`] in steps (when the queues sit behind locks
    /// that must not be held while the device resets): after `negotiate`,
    /// hands one queue back empty. Never waits.
    pub fn requeue(&self, q: &mut Queue) {
        q.mem.as_mut_slice().fill(0);
        q.avail_idx = 0;
        q.last_used = 0;
        match &self.regs {
            Regs::Legacy { io } => port::outw(io + L_QUEUE_SELECT, q.index),
            Regs::Modern { common, .. } => common.write16(C_QUEUE_SELECT, q.index),
        }
        self.give(q);
    }

    /// The last step of [`Virtio::restore`]: MSI-X routed again.
    pub fn reroute(&self) {
        if self.uses_msix() {
            self.route_msix();
        }
    }

    /// Every MSI-X interrupt (configuration and queues) on the first vector.
    fn route_msix(&self) {
        if let Regs::Modern { common, .. } = &self.regs {
            common.write16(C_MSIX_CONFIG, 0);
            for q in 0..self.queues.load(Ordering::Relaxed) {
                common.write16(C_QUEUE_SELECT, q);
                common.write16(C_QUEUE_MSIX_VECTOR, 0);
            }
        }
    }

    /// Attaches `handler` to the device's interrupt: MSI-X or MSI on modern
    /// devices, the legacy line otherwise (legacy devices move their
    /// registers when MSI-X is on). Returns false if there is none.
    pub fn attach_irq(&self, dev: &DeviceInfo, handler: IrqHandler, ctx: *mut c_void) -> bool {
        if self.is_modern() && arcdrv::irq_attach_msi(dev.id, handler, ctx) >= 0 {
            let msix = self.pci.find_capability(CAP_MSIX, 0).is_some_and(|c| self.pci.read16(c + 2) & (1 << 15) != 0);
            if msix {
                self.route_msix();
                self.msix.store(true, Ordering::Relaxed);
            }
            return true;
        }
        dev.irq != IRQ_NONE && arcdrv::irq_attach(dev.id, dev.irq, handler, ctx) == OK
    }

    /// The driver is set up: the device may start.
    pub fn ready(&self) {
        let s = self.status();
        self.set_status(s | STATUS_DRIVER_OK);
    }

    pub fn notify(&self, q: &Queue) {
        fence(Ordering::SeqCst);
        match &self.regs {
            Regs::Legacy { io } => port::outw(io + L_QUEUE_NOTIFY, q.index),
            Regs::Modern { notify, notify_mult, .. } => notify.write16(q.notify_off as usize * *notify_mult as usize, q.index),
        }
    }

    /// Reads and acknowledges the interrupt status (bit 0: a queue was
    /// used, bit 1: the configuration changed). Always 0 with MSI-X.
    pub fn isr(&self) -> u8 {
        match &self.regs {
            Regs::Legacy { io } => port::inb(io + L_ISR),
            Regs::Modern { isr, .. } => isr.read8(0),
        }
    }

    /// Whether interrupts come by MSI-X (no ISR status to check).
    pub fn uses_msix(&self) -> bool {
        self.msix.load(Ordering::Relaxed)
    }

    pub fn config8(&self, off: usize) -> u8 {
        match &self.regs {
            Regs::Legacy { io } => port::inb(io + L_CONFIG + off as u16),
            Regs::Modern { device, .. } => device.as_ref().map(|d| d.read8(off)).unwrap_or(0),
        }
    }

    pub fn config16(&self, off: usize) -> u16 {
        match &self.regs {
            Regs::Legacy { io } => port::inw(io + L_CONFIG + off as u16),
            Regs::Modern { device, .. } => device.as_ref().map(|d| d.read16(off)).unwrap_or(0),
        }
    }

    pub fn config32(&self, off: usize) -> u32 {
        match &self.regs {
            Regs::Legacy { io } => port::inl(io + L_CONFIG + off as u16),
            Regs::Modern { device, .. } => device.as_ref().map(|d| d.read32(off)).unwrap_or(0),
        }
    }
}

/// A split virtqueue.
pub struct Queue {
    pub index: u16,
    pub size: u16,
    mem: Dma,
    avail: usize,
    used: usize,
    notify_off: u16,
    avail_idx: u16,
    last_used: u16,
}

impl Queue {
    /// Asks the device not to interrupt when it uses this queue: for queues
    /// the driver polls. Only a hint, so the line may still need acking.
    pub fn quiet(&self) {
        self.mem.write::<u16>(self.avail, 1);
    }

    /// Writes descriptor `i`.
    pub fn desc(&self, i: u16, addr: u64, len: u32, flags: u16, next: u16) {
        let at = i as usize * 16;
        self.mem.write::<u64>(at, addr);
        self.mem.write::<u32>(at + 8, len);
        self.mem.write::<u16>(at + 12, flags);
        self.mem.write::<u16>(at + 14, next);
    }

    /// Offers the chain starting at descriptor `head` to the device (call
    /// `Virtio::notify` afterwards).
    pub fn publish(&mut self, head: u16) {
        let slot = self.avail + 4 + (self.avail_idx % self.size) as usize * 2;
        self.mem.write::<u16>(slot, head);
        self.avail_idx = self.avail_idx.wrapping_add(1);
        fence(Ordering::SeqCst);
        self.mem.write::<u16>(self.avail + 2, self.avail_idx);
    }

    /// The next chain the device has finished with: (head, bytes written).
    pub fn take_used(&mut self) -> Option<(u16, u32)> {
        let idx = self.mem.read::<u16>(self.used + 2);
        if idx == self.last_used {
            return None;
        }
        fence(Ordering::SeqCst);
        let at = self.used + 4 + (self.last_used % self.size) as usize * 8;
        let id = self.mem.read::<u32>(at) as u16;
        let len = self.mem.read::<u32>(at + 4);
        self.last_used = self.last_used.wrapping_add(1);
        Some((id, len))
    }
}

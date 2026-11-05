//! virtio block driver.
//!
//! One virtqueue, one request at a time: header, data (bounce buffer) and
//! status descriptors chained together, completion polled on the used ring.
//! Works on the legacy and the modern virtio interface.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use justdrv::{BlockDesc, DeviceInfo, Dma, Mutex, Status, BLOCK_READONLY, E_INVAL, E_IO, E_NODEV, E_NOMEM, E_TIMEOUT, OK};
use justvirtio::{Queue, Virtio, DESC_NEXT, DESC_WRITE};
use core::ffi::c_void;

justdrv::driver! {
    name: "virtioblk",
    version: "1.1.0",
    probe: probe,
}

const F_RO: u64 = 1 << 5;
const F_FLUSH: u64 = 1 << 9;

const T_IN: u32 = 0;
const T_OUT: u32 = 1;
const T_FLUSH: u32 = 4;

const MAX_SECTORS: u32 = 128;

struct Device {
    queue: Queue,
    req: Dma,
    bounce: Dma,
    flush: bool,
}

struct Disk {
    vio: Virtio,
    dev: Mutex<Device>,
    sectors: u64,
    /// The features agreed on (again after sleep).
    features: u64,
}

/// Before sleep: everything written is flushed, then the device stops.
unsafe extern "C" fn suspend(ctx: *mut c_void) -> Status {
    // SAFETY: ctx is the leaked Disk.
    let disk = unsafe { &*(ctx as *const Disk) };
    let mut d = disk.dev.lock();
    if d.flush {
        let st = d.request(&disk.vio, T_FLUSH, 0, 0);
        if st != OK {
            return st;
        }
    }
    disk.vio.reset();
    OK
}

/// After sleep: the device set up again with its queue.
unsafe extern "C" fn resume(ctx: *mut c_void) -> Status {
    // SAFETY: ctx is the leaked Disk.
    let disk = unsafe { &*(ctx as *const Disk) };
    let mut d = disk.dev.lock();
    if !disk.vio.restore(disk.features, &mut [&mut d.queue]) {
        return E_IO;
    }
    d.queue.quiet();
    disk.vio.ready();
    OK
}

impl Device {
    fn request(&mut self, vio: &Virtio, kind: u32, sector: u64, bytes: usize) -> Status {
        // Header: type, reserved, sector. Status byte after it.
        self.req.write::<u32>(0, kind);
        self.req.write::<u32>(4, 0);
        self.req.write::<u64>(8, sector);
        self.req.write::<u8>(16, 0xFF);
        let header = self.req.phys();
        let q = &mut self.queue;
        if bytes > 0 {
            q.desc(0, header, 16, DESC_NEXT, 1);
            let data_flags = DESC_NEXT | if kind == T_IN { DESC_WRITE } else { 0 };
            q.desc(1, self.bounce.phys(), bytes as u32, data_flags, 2);
        } else {
            q.desc(0, header, 16, DESC_NEXT, 2);
        }
        q.desc(2, header + 16, 1, DESC_WRITE, 0);
        q.publish(0);
        vio.notify(&self.queue);
        let mut done = false;
        if !justdrv::wait_until(5000, || {
            done = self.queue.take_used().is_some();
            done
        }) {
            return E_TIMEOUT;
        }
        // Reading the ISR register acknowledges the interrupt line.
        vio.isr();
        if self.req.read::<u8>(16) == 0 {
            OK
        } else {
            E_IO
        }
    }
}

impl Disk {
    fn transfer(&self, lba: u64, count: u32, buf: *mut u8, write: bool) -> Status {
        if count == 0 || lba + count as u64 > self.sectors {
            return E_INVAL;
        }
        let mut d = self.dev.lock();
        let mut done = 0;
        while done < count {
            let n = (count - done).min(MAX_SECTORS);
            let bytes = n as usize * 512;
            // SAFETY: the kernel passes a buffer of `count` sectors.
            let chunk = unsafe { core::slice::from_raw_parts_mut(buf.add(done as usize * 512), bytes) };
            if write {
                d.bounce.as_mut_slice()[..bytes].copy_from_slice(chunk);
            }
            let st = d.request(&self.vio, if write { T_OUT } else { T_IN }, lba + done as u64, bytes);
            if st != OK {
                return st;
            }
            if !write {
                chunk.copy_from_slice(&d.bounce.as_slice()[..bytes]);
            }
            done += n;
        }
        OK
    }
}

/// Completions are polled; the handler only acknowledges a line that the
/// device raised anyway, which would otherwise stay asserted for every other
/// device sharing it.
unsafe extern "C" fn irq(ctx: *mut c_void) {
    // SAFETY: ctx is the leaked Disk.
    let disk = unsafe { &*(ctx as *const Disk) };
    if !disk.vio.uses_msix() {
        disk.vio.isr();
    }
}

unsafe extern "C" fn read(ctx: *mut c_void, lba: u64, count: u32, buf: *mut u8) -> Status {
    // SAFETY: ctx is the Disk registered with this descriptor.
    unsafe { &*(ctx as *const Disk) }.transfer(lba, count, buf, false)
}

unsafe extern "C" fn write(ctx: *mut c_void, lba: u64, count: u32, buf: *mut u8) -> Status {
    // SAFETY: as in read.
    unsafe { &*(ctx as *const Disk) }.transfer(lba, count, buf, true)
}

unsafe extern "C" fn flush(ctx: *mut c_void) -> Status {
    // SAFETY: as in read.
    let disk = unsafe { &*(ctx as *const Disk) };
    let mut d = disk.dev.lock();
    if !d.flush {
        return OK;
    }
    d.request(&disk.vio, T_FLUSH, 0, 0)
}

fn probe(dev: &DeviceInfo) -> Status {
    if dev.vendor != 0x1AF4 || !(dev.device == 0x1001 || dev.device == 0x1042) {
        return E_NODEV;
    }
    let Some(vio) = Virtio::open(dev) else {
        justdrv::warn!("no usable virtio registers");
        return E_IO;
    };
    let Some(accepted) = vio.negotiate(F_RO | F_FLUSH) else { return E_IO };
    let Some(queue) = vio.queue(0, 16) else {
        vio.set_status(justvirtio::STATUS_FAILED);
        return E_IO;
    };
    let (Some(req), Some(bounce)) = (Dma::new(64), Dma::new(MAX_SECTORS as usize * 512)) else {
        return E_NOMEM;
    };
    queue.quiet();

    let sectors = vio.config32(0) as u64 | (vio.config32(4) as u64) << 32;
    let device = Device { queue, req, bounce, flush: accepted & F_FLUSH != 0 };
    let disk: &'static Disk = Box::leak(Box::new(Disk { vio, dev: Mutex::new(device), sectors, features: accepted }));
    disk.vio.attach_irq(dev, irq, disk as *const Disk as *mut c_void);
    disk.vio.ready();
    let desc: &'static BlockDesc = Box::leak(Box::new(BlockDesc {
        ctx: disk as *const Disk as *mut c_void,
        sector_size: 512,
        flags: if accepted & F_RO != 0 { BLOCK_READONLY } else { 0 },
        sector_count: sectors,
        read,
        write,
        flush: Some(flush),
        model: justdrv::model_name("virtio disk"),
    }));
    let n = justdrv::block_register(dev.id, desc);
    if n < 0 {
        return n;
    }
    justdrv::power_hooks(dev.id, disk as *const Disk as *mut c_void, Some(suspend), resume);
    OK
}

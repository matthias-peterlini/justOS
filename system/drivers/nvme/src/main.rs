//! NVM Express driver.
//!
//! One admin queue pair and one I/O queue pair per controller, polled
//! completions, transfers through a 64 KiB bounce buffer described by a PRP
//! list. Every active namespace becomes a disk. Before sleep the controller
//! is flushed and shut down normally; after it, enabled again with the
//! same queues.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::string::String;
use justdrv::{BlockDesc, DeviceInfo, Dma, Mmio, Mutex, Status, E_INVAL, E_IO, E_NODEV, E_NOMEM, E_TIMEOUT, OK, RES_MEMORY};
use core::ffi::c_void;

justdrv::driver! {
    name: "nvme",
    version: "1.1.0",
    probe: probe,
}

const REG_CAP: usize = 0x00;
const REG_VS: usize = 0x08;
const REG_CC: usize = 0x14;
const REG_CSTS: usize = 0x1C;
const REG_AQA: usize = 0x24;
const REG_ASQ: usize = 0x28;
const REG_ACQ: usize = 0x30;

const QUEUE_DEPTH: u16 = 32;
const PAGE: usize = 4096;
const BOUNCE_PAGES: usize = 16; // 64 KiB per transfer

struct Queue {
    sq: Dma,
    cq: Dma,
    sq_tail: u16,
    cq_head: u16,
    phase: bool,
    id: u16,
    cid: u16,
}

struct Controller {
    regs: Mmio,
    stride: usize,
    /// How long the controller may take to become ready, in ms.
    timeout: u64,
    admin: Queue,
    io: Option<Queue>,
    bounce: Dma,
    prp_list: Dma,
}

struct Namespace {
    ctrl: &'static Mutex<Controller>,
    nsid: u32,
    sectors: u64,
    sector_size: u32,
}

#[derive(Default, Clone, Copy)]
struct Command {
    opcode: u8,
    nsid: u32,
    prp1: u64,
    prp2: u64,
    cdw10: u32,
    cdw11: u32,
    cdw12: u32,
}

impl Queue {
    fn new(id: u16) -> Option<Queue> {
        Some(Queue {
            sq: Dma::new(QUEUE_DEPTH as usize * 64)?,
            cq: Dma::new(QUEUE_DEPTH as usize * 16)?,
            sq_tail: 0,
            cq_head: 0,
            phase: true,
            id,
            cid: 0,
        })
    }

    /// Empty, as the controller sees a new queue.
    fn clear(&mut self) {
        self.sq.as_mut_slice().fill(0);
        self.cq.as_mut_slice().fill(0);
        self.sq_tail = 0;
        self.cq_head = 0;
        self.phase = true;
    }
}

impl Controller {
    /// Resets the controller and enables it with an empty admin queue.
    fn enable(&mut self) -> Status {
        let (regs, timeout) = (&self.regs, self.timeout);
        regs.write32(REG_CC, regs.read32(REG_CC) & !1);
        if !justdrv::wait_until(timeout, || regs.read32(REG_CSTS) & 1 == 0) {
            return E_TIMEOUT;
        }
        self.admin.clear();
        let depth = (QUEUE_DEPTH - 1) as u32;
        regs.write32(REG_AQA, depth << 16 | depth);
        regs.write64(REG_ASQ, self.admin.sq.phys());
        regs.write64(REG_ACQ, self.admin.cq.phys());
        regs.write32(REG_CC, 1 | 6 << 16 | 4 << 20); // enable, 64-byte SQ and 16-byte CQ entries, 4 KiB pages
        if !justdrv::wait_until(timeout, || regs.read32(REG_CSTS) & 3 != 0) || regs.read32(REG_CSTS) & 2 != 0 {
            justdrv::warn!("controller failed to start");
            return E_IO;
        }
        OK
    }

    /// Creates the I/O queue pair: completion queue 1, then submission
    /// queue 1.
    fn create_io(&mut self, mut io: Queue) -> Status {
        io.clear();
        let size = (QUEUE_DEPTH as u32 - 1) << 16 | 1;
        if let Err(e) = self.admin(Command { opcode: 0x05, prp1: io.cq.phys(), cdw10: size, cdw11: 1, ..Command::default() }) {
            return e;
        }
        if let Err(e) = self.admin(Command { opcode: 0x01, prp1: io.sq.phys(), cdw10: size, cdw11: 1 << 16 | 1, ..Command::default() }) {
            return e;
        }
        self.io = Some(io);
        OK
    }

    /// Submits a command and polls for its completion.
    fn run(regs: &Mmio, stride: usize, q: &mut Queue, c: Command) -> Result<u32, Status> {
        q.cid = q.cid.wrapping_add(1);
        let at = q.sq_tail as usize * 64;
        for i in 0..16 {
            q.sq.write::<u32>(at + i * 4, 0);
        }
        q.sq.write::<u32>(at, c.opcode as u32 | (q.cid as u32) << 16);
        q.sq.write::<u32>(at + 4, c.nsid);
        q.sq.write::<u64>(at + 24, c.prp1);
        q.sq.write::<u64>(at + 32, c.prp2);
        q.sq.write::<u32>(at + 40, c.cdw10);
        q.sq.write::<u32>(at + 44, c.cdw11);
        q.sq.write::<u32>(at + 48, c.cdw12);
        q.sq_tail = (q.sq_tail + 1) % QUEUE_DEPTH;
        regs.write32(0x1000 + 2 * q.id as usize * stride, q.sq_tail as u32);

        let entry = q.cq_head as usize * 16;
        let phase = q.phase;
        if !justdrv::wait_until(5000, || (q.cq.read::<u16>(entry + 14) & 1 != 0) == phase) {
            return Err(E_TIMEOUT);
        }
        let result = q.cq.read::<u32>(entry);
        let status = q.cq.read::<u16>(entry + 14) >> 1;
        q.cq_head = (q.cq_head + 1) % QUEUE_DEPTH;
        if q.cq_head == 0 {
            q.phase = !q.phase;
        }
        regs.write32(0x1000 + (2 * q.id as usize + 1) * stride, q.cq_head as u32);
        if status & 0x7FF != 0 {
            justdrv::warn!("command {:#x} failed with status {:#x}", c.opcode, status);
            return Err(E_IO);
        }
        Ok(result)
    }

    fn admin(&mut self, c: Command) -> Result<u32, Status> {
        Self::run(&self.regs, self.stride, &mut self.admin, c)
    }

    fn io(&mut self, c: Command) -> Result<u32, Status> {
        let q = self.io.as_mut().ok_or(E_IO)?;
        Self::run(&self.regs, self.stride, q, c)
    }

    /// PRP entries for `bytes` of the bounce buffer.
    fn prps(&self, bytes: usize) -> (u64, u64) {
        let first = self.bounce.phys();
        let pages = bytes.div_ceil(PAGE);
        match pages {
            0 | 1 => (first, 0),
            2 => (first, first + PAGE as u64),
            _ => (first, self.prp_list.phys()),
        }
    }
}

impl Namespace {
    fn transfer(&self, lba: u64, count: u32, buf: *mut u8, write: bool) -> Status {
        if count == 0 || lba + count as u64 > self.sectors {
            return E_INVAL;
        }
        let ss = self.sector_size as usize;
        let per = (BOUNCE_PAGES * PAGE / ss) as u32;
        let mut c = self.ctrl.lock();
        let mut done = 0;
        while done < count {
            let n = (count - done).min(per);
            let bytes = n as usize * ss;
            // SAFETY: the kernel passes a buffer of `count` sectors.
            let chunk = unsafe { core::slice::from_raw_parts_mut(buf.add(done as usize * ss), bytes) };
            if write {
                c.bounce.as_mut_slice()[..bytes].copy_from_slice(chunk);
            }
            let (prp1, prp2) = c.prps(bytes);
            let at = lba + done as u64;
            let cmd = Command {
                opcode: if write { 0x01 } else { 0x02 },
                nsid: self.nsid,
                prp1,
                prp2,
                cdw10: at as u32,
                cdw11: (at >> 32) as u32,
                cdw12: n - 1,
            };
            if let Err(e) = c.io(cmd) {
                return e;
            }
            if !write {
                chunk.copy_from_slice(&c.bounce.as_slice()[..bytes]);
            }
            done += n;
        }
        OK
    }
}

unsafe extern "C" fn read(ctx: *mut c_void, lba: u64, count: u32, buf: *mut u8) -> Status {
    // SAFETY: ctx is the Namespace registered with this descriptor.
    unsafe { &*(ctx as *const Namespace) }.transfer(lba, count, buf, false)
}

unsafe extern "C" fn write(ctx: *mut c_void, lba: u64, count: u32, buf: *mut u8) -> Status {
    // SAFETY: as in read.
    unsafe { &*(ctx as *const Namespace) }.transfer(lba, count, buf, true)
}

unsafe extern "C" fn flush(ctx: *mut c_void) -> Status {
    // SAFETY: as in read.
    let ns = unsafe { &*(ctx as *const Namespace) };
    match ns.ctrl.lock().io(Command { opcode: 0x00, nsid: ns.nsid, ..Command::default() }) {
        Ok(_) => OK,
        Err(e) => e,
    }
}

/// Before sleep: everything written goes to flash, then a normal shutdown
/// (the controller saves what it must).
unsafe extern "C" fn suspend(ctx: *mut c_void) -> Status {
    // SAFETY: ctx is the leaked controller.
    let mut c = unsafe { &*(ctx as *const Mutex<Controller>) }.lock();
    // A flush of every namespace; not every controller takes it, but the
    // shutdown below writes the cache out as well.
    let _ = c.io(Command { opcode: 0x00, nsid: u32::MAX, ..Command::default() });
    let regs = &c.regs;
    regs.write32(REG_CC, (regs.read32(REG_CC) & !(3 << 14)) | 1 << 14);
    if !justdrv::wait_until(c.timeout.max(5000), || (regs.read32(REG_CSTS) >> 2) & 3 == 2) {
        justdrv::warn!("controller did not finish its shutdown");
    }
    OK
}

/// After sleep: enabled again, with the I/O queues created anew.
unsafe extern "C" fn resume(ctx: *mut c_void) -> Status {
    // SAFETY: ctx is the leaked controller.
    let mut c = unsafe { &*(ctx as *const Mutex<Controller>) }.lock();
    let st = c.enable();
    if st != OK {
        return st;
    }
    match c.io.take() {
        Some(io) => c.create_io(io),
        None => OK,
    }
}

fn text(bytes: &[u8]) -> String {
    String::from(core::str::from_utf8(bytes).unwrap_or("").trim())
}

fn probe(dev: &DeviceInfo) -> Status {
    if dev.class != 0x01 || dev.subclass != 0x08 || dev.prog_if != 0x02 {
        return E_NODEV;
    }
    let bar = dev.resources[0];
    if bar.kind != RES_MEMORY || bar.base == 0 {
        return E_IO;
    }
    justdrv::Pci(dev.id).enable(false, true, true);
    let Some(regs) = Mmio::map(bar.base, (bar.size as usize).clamp(0x2000, 0x4000)) else { return E_NOMEM };
    let cap = regs.read64(REG_CAP);
    let stride = 4usize << ((cap >> 32) & 0xF);
    let timeout = ((cap >> 24) & 0xFF).max(1) * 500;
    let vs = regs.read32(REG_VS);

    // Reset, then bring the controller up with the admin queues.
    let (Some(admin), Some(bounce), Some(prp_list)) = (Queue::new(0), Dma::new(BOUNCE_PAGES * PAGE), Dma::new(PAGE)) else {
        return E_NOMEM;
    };
    for i in 1..BOUNCE_PAGES {
        prp_list.write::<u64>((i - 1) * 8, bounce.phys() + (i * PAGE) as u64);
    }
    let mut ctrl = Controller { regs, stride, timeout, admin, io: None, bounce, prp_list };
    let st = ctrl.enable();
    if st != OK {
        return st;
    }

    // Identify the controller.
    let prp = ctrl.bounce.phys();
    if let Err(e) = ctrl.admin(Command { opcode: 0x06, prp1: prp, cdw10: 1, ..Command::default() }) {
        return e;
    }
    let id = ctrl.bounce.as_slice();
    let model = text(&id[24..64]);
    let namespaces = u32::from_le_bytes(id[516..520].try_into().unwrap());
    justdrv::info!("{} (NVMe {}.{}), {} namespace(s)", model, vs >> 16, (vs >> 8) & 0xFF, namespaces);

    let Some(io) = Queue::new(1) else { return E_NOMEM };
    let st = ctrl.create_io(io);
    if st != OK {
        return st;
    }

    let ctrl: &'static Mutex<Controller> = Box::leak(Box::new(Mutex::new(ctrl)));
    justdrv::power_hooks(dev.id, ctrl as *const Mutex<Controller> as *mut c_void, Some(suspend), resume);
    for nsid in 1..=namespaces.min(16) {
        let mut c = ctrl.lock();
        let prp = c.bounce.phys();
        if c.admin(Command { opcode: 0x06, nsid, prp1: prp, cdw10: 0, ..Command::default() }).is_err() {
            continue;
        }
        let ns = c.bounce.as_slice();
        let sectors = u64::from_le_bytes(ns[0..8].try_into().unwrap());
        let format = (ns[26] & 0xF) as usize;
        let lbads = ns[128 + format * 4 + 2];
        drop(c);
        if sectors == 0 || !(9..=12).contains(&lbads) {
            continue; // inactive namespace or unsupported sector size
        }
        let ns: &'static Namespace = Box::leak(Box::new(Namespace { ctrl, nsid, sectors, sector_size: 1 << lbads }));
        let desc: &'static BlockDesc = Box::leak(Box::new(BlockDesc {
            ctx: ns as *const Namespace as *mut c_void,
            sector_size: 1 << lbads,
            flags: 0,
            sector_count: sectors,
            read,
            write,
            flush: Some(flush),
            model: justdrv::model_name(&model),
        }));
        justdrv::block_register(dev.id, desc);
    }
    OK
}

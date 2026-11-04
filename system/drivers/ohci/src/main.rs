//! OHCI (USB 1.1) host controller driver.
//!
//! The USB 1.1 controllers of AMD, NVIDIA, SiS and Apple chipsets: full-
//! and low-speed devices, usually next to an EHCI controller that hands
//! them its slower devices.
//!
//! The controller walks three lists of endpoint descriptors (EDs): control,
//! bulk, and the periodic list for interrupt endpoints, which every frame
//! visits. Each endpoint has its ED, its transfer descriptors (TDs) and a
//! data buffer in one DMA block; transfers on an endpoint run one at a time
//! and finish by watching the ED's queue.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use arcdrv::usb::*;
use arcdrv::{
    DeviceInfo, Dma, Event, IrqLock, Mmio, Mutex, Pci, Status, E_BUSY, E_GONE, E_INVAL, E_IO, E_NODEV, E_NOMEM, E_STALL, E_TIMEOUT, OK,
    RES_MEMORY,
};
use arcusb::{Attach, Handle, Host, HubInfo, PortStatus, Table};
use core::ffi::c_void;
use core::sync::atomic::{fence, AtomicBool, AtomicU16, AtomicU32, AtomicU8, Ordering};

arcdrv::driver! {
    name: "ohci",
    version: "1.1.0",
    probe: probe,
}

// Registers
const HC_CONTROL: usize = 0x04;
const HC_COMMAND: usize = 0x08;
const HC_INT_STATUS: usize = 0x0C;
const HC_INT_ENABLE: usize = 0x10;
const HC_INT_DISABLE: usize = 0x14;
const HC_HCCA: usize = 0x18;
const HC_CONTROL_HEAD: usize = 0x20;
const HC_BULK_HEAD: usize = 0x28;
const HC_FM_INTERVAL: usize = 0x34;
const HC_PERIODIC_START: usize = 0x40;
const HC_LS_THRESHOLD: usize = 0x44;
const HC_RH_DESC_A: usize = 0x48;
const HC_RH_STATUS: usize = 0x50;
const HC_RH_PORT: usize = 0x54;

// HcControl
const CTL_PLE: u32 = 1 << 2;
const CTL_CLE: u32 = 1 << 4;
const CTL_BLE: u32 = 1 << 5;
const CTL_HCFS: u32 = 3 << 6;
const CTL_OPERATIONAL: u32 = 2 << 6;
const CTL_RESUME: u32 = 1 << 6;
const CTL_IR: u32 = 1 << 8;
/// Control/bulk service ratio 4:1.
const CTL_CBSR: u32 = 3;

// HcCommandStatus
const CMD_HCR: u32 = 1;
const CMD_CLF: u32 = 1 << 1;
const CMD_BLF: u32 = 1 << 2;
const CMD_OCR: u32 = 1 << 3;

// Interrupts
const INT_WDH: u32 = 1 << 1;
const INT_UE: u32 = 1 << 4;
const INT_MIE: u32 = 1 << 31;

// Root hub port status (reads) and commands (writes)
const PORT_CCS: u32 = 1;
const PORT_PES: u32 = 1 << 1;
const PORT_PRS: u32 = 1 << 4;
const PORT_PPS: u32 = 1 << 8;
const PORT_LSDA: u32 = 1 << 9;
const PORT_PRSC: u32 = 1 << 20;
/// Change bits (write 1 to clear).
const PORT_CHANGES: u32 = 0x1F << 16;
/// HcRhStatus: switch power on for every port.
const RH_LPSC: u32 = 1 << 16;

// Endpoint descriptor (ED) fields
const ED_LOW_SPEED: u32 = 1 << 13;
const ED_SKIP: u32 = 1 << 14;
const ED_HALTED: u32 = 1;
const ED_CARRY: u32 = 1 << 1;

// Transfer descriptor (TD) fields
const TD_ROUNDING: u32 = 1 << 18;
const TD_SETUP: u32 = 0;
const TD_OUT: u32 = 1 << 19;
const TD_IN: u32 = 2 << 19;
const TD_NO_INT: u32 = 7 << 21;
const TD_DATA0: u32 = 2 << 24;
const TD_DATA1: u32 = 3 << 24;
const TD_NOT_ACCESSED: u32 = 0xF << 28;

// Completion codes
const CC_OK: u32 = 0;
const CC_STALL: u32 = 4;
const CC_UNDERRUN: u32 = 9;

/// Layout of a pipe's DMA block: ED, setup packet, TDs, then the data
/// buffer on its own page (a TD may cross one page boundary at most).
const SETUP_OFF: usize = 16;
const TD_OFF: usize = 32;
const BUF_OFF: usize = 4096;
const CONTROL_BUF: usize = 4096;
const BULK_BUF: usize = 16384;
/// Bytes one TD moves (one page; the buffer is page aligned).
const TD_BYTES: usize = 4096;

/// HCCA, then the anchor EDs of the three lists.
const ANCHOR_CONTROL: usize = 256;
const ANCHOR_BULK: usize = 272;
const ANCHOR_PERIODIC: usize = 288;

#[derive(Clone, Copy)]
struct Poll {
    report: UsbReport,
    ctx: *mut c_void,
}

// SAFETY: the context belongs to the driver that started the poll.
unsafe impl Send for Poll {}

struct Pipe {
    mem: Dma,
    kind: u8,
    /// Endpoint address with the direction bit (0 for control).
    endpoint: u8,
    mps: AtomicU16,
    buf_len: usize,
    busy: Mutex<()>,
    poll: IrqLock<Option<Poll>>,
    in_callback: AtomicBool,
    gone: AtomicBool,
}

impl Pipe {
    fn new(kind: u8, endpoint: u8, mps: u16, address: u8, low_speed: bool, buf_len: usize) -> Option<Arc<Pipe>> {
        let mem = Dma::new(BUF_OFF + buf_len)?;
        let p = Pipe {
            mem,
            kind,
            endpoint,
            mps: AtomicU16::new(mps.max(1)),
            buf_len,
            busy: Mutex::new(()),
            poll: IrqLock::new(None),
            in_callback: AtomicBool::new(false),
            gone: AtomicBool::new(false),
        };
        p.write_ed0(address, low_speed, false);
        // Empty queue: head = tail = TD 0.
        p.mem.write::<u32>(4, p.td_phys(0));
        p.mem.write::<u32>(8, p.td_phys(0));
        Some(Arc::new(p))
    }

    fn write_ed0(&self, address: u8, low_speed: bool, skip: bool) {
        let mps = self.mps.load(Ordering::Relaxed) as u32;
        let ls = if low_speed { ED_LOW_SPEED } else { 0 };
        let k = if skip { ED_SKIP } else { 0 };
        self.mem.write::<u32>(0, address as u32 & 0x7F | ((self.endpoint & 0xF) as u32) << 7 | ls | k | mps << 16);
    }

    fn ed(&self) -> u32 {
        self.mem.phys() as u32
    }

    fn td_phys(&self, i: usize) -> u32 {
        (self.mem.phys() + (TD_OFF + i * 16) as u64) as u32
    }

    fn buf_phys(&self) -> u32 {
        (self.mem.phys() + BUF_OFF as u64) as u32
    }

    fn buf(&self) -> &mut [u8] {
        &mut self.mem.as_mut_slice()[BUF_OFF..BUF_OFF + self.buf_len]
    }

    /// Writes TD `i`: control word, buffer [start, start + len), next TD.
    fn set_td(&self, i: usize, control: u32, start: u32, len: usize, next: u32) {
        let at = TD_OFF + i * 16;
        self.mem.write::<u32>(at, control | TD_NOT_ACCESSED);
        self.mem.write::<u32>(at + 4, if len == 0 { 0 } else { start });
        self.mem.write::<u32>(at + 8, next);
        self.mem.write::<u32>(at + 12, if len == 0 { 0 } else { start + len as u32 - 1 });
    }

    fn clear_td(&self, i: usize) {
        let at = TD_OFF + i * 16;
        for k in 0..4 {
            self.mem.write::<u32>(at + k * 4, 0);
        }
    }

    fn td_code(&self, i: usize) -> u32 {
        self.mem.read::<u32>(TD_OFF + i * 16) >> 28
    }

    /// Bytes TD `i` moved, given the length it was queued with.
    fn td_actual(&self, i: usize, start: u32, len: usize) -> usize {
        let cbp = self.mem.read::<u32>(TD_OFF + i * 16 + 4);
        if cbp == 0 {
            len
        } else {
            (cbp.saturating_sub(start) as usize).min(len)
        }
    }

    fn head(&self) -> u32 {
        self.mem.read::<u32>(8)
    }

    fn tail(&self) -> u32 {
        self.mem.read::<u32>(4)
    }

    /// Queues TDs 0..n (TD n is the empty tail).
    fn launch(&self, n: usize) {
        let carry = self.head() & ED_CARRY;
        fence(Ordering::SeqCst);
        self.mem.write::<u32>(4, self.td_phys(n));
        self.mem.write::<u32>(8, self.td_phys(0) | carry);
    }

    /// Empties the queue (dropping whatever is left in it) and clears a
    /// halt, keeping the data toggle unless `reset_toggle`.
    fn reset_queue(&self, reset_toggle: bool) {
        let carry = if reset_toggle { 0 } else { self.head() & ED_CARRY };
        self.mem.write::<u32>(4, self.td_phys(0));
        self.mem.write::<u32>(8, self.td_phys(0) | carry);
    }

    fn idle(&self) -> bool {
        self.head() & !0xF == self.tail()
    }

    fn halted(&self) -> bool {
        self.head() & ED_HALTED != 0
    }
}

struct Dev {
    address: AtomicU8,
    low_speed: bool,
    pipes: IrqLock<Vec<Arc<Pipe>>>,
}

impl Dev {
    fn pipe(&self, endpoint: u8) -> Option<Arc<Pipe>> {
        self.pipes.lock().iter().find(|p| p.endpoint == endpoint).cloned()
    }
}

struct Ohci {
    regs: Mmio,
    ports: u8,
    /// HCCA and the anchors of the lists.
    hcca: Dma,
    schedule: Mutex<Vec<Arc<Pipe>>>,
    devices: Table<Arc<Dev>>,
    next_handle: AtomicU32,
    polls: IrqLock<Vec<Arc<Pipe>>>,
    wake: Event,
    has_irq: AtomicBool,
    /// Frame interval the firmware left (bit times per frame).
    fi: u32,
    /// Stopped for sleep.
    asleep: AtomicBool,
}

impl Ohci {
    fn port_reg(&self, port: u8) -> usize {
        HC_RH_PORT + (port as usize - 1) * 4
    }

    fn dev(&self, h: Handle) -> Result<Arc<Dev>, Status> {
        self.devices.get(h).ok_or(E_GONE)
    }

    // ------------------------------------------------------------------
    // Lists

    fn anchor(kind: u8) -> usize {
        match kind {
            EP_CONTROL => ANCHOR_CONTROL,
            EP_BULK => ANCHOR_BULK,
            _ => ANCHOR_PERIODIC,
        }
    }

    /// Rewrites the links of one list back to front, so the controller
    /// always sees a valid list while it changes.
    fn relink(&self, list: &[Arc<Pipe>], kind: u8) {
        let mut next = 0;
        for p in list.iter().rev().filter(|p| p.kind == kind) {
            p.mem.write::<u32>(12, next);
            next = p.ed();
        }
        fence(Ordering::SeqCst);
        self.hcca.write::<u32>(Self::anchor(kind) + 12, next);
    }

    fn schedule_add(&self, pipe: &Arc<Pipe>) {
        let mut list = self.schedule.lock();
        list.push(pipe.clone());
        self.relink(&list, pipe.kind);
    }

    fn schedule_remove(&self, pipes: &[Arc<Pipe>]) {
        {
            let mut list = self.schedule.lock();
            list.retain(|p| !pipes.iter().any(|q| Arc::ptr_eq(p, q)));
            for kind in [EP_CONTROL, EP_BULK, EP_INTERRUPT] {
                self.relink(&list, kind);
            }
        }
        // The controller may still be on one of them in this frame.
        arcdrv::sleep_ms(2);
    }

    fn kick(&self, pipe: &Pipe) {
        let bit = match pipe.kind {
            EP_CONTROL => CMD_CLF,
            EP_BULK => CMD_BLF,
            _ => 0,
        };
        if bit != 0 {
            self.regs.write32(HC_COMMAND, bit);
        }
    }

    // ------------------------------------------------------------------
    // Transfers

    /// Waits until the queue is empty or halted. Returns whether it halted.
    fn wait_queue(&self, pipe: &Pipe, timeout_ms: u32) -> Result<bool, Status> {
        let deadline = arcdrv::time_ms() + timeout_ms as u64;
        loop {
            if pipe.halted() {
                return Ok(true);
            }
            if pipe.idle() {
                return Ok(false);
            }
            if pipe.gone.load(Ordering::Acquire) {
                return Err(E_GONE);
            }
            if arcdrv::time_ms() >= deadline {
                return Err(E_TIMEOUT);
            }
            arcdrv::sleep_ms(1);
        }
    }

    /// Takes the queue back after a transfer that did not finish cleanly:
    /// skip the ED, wait for the frame to end, empty it.
    fn abort(&self, pipe: &Pipe, dev: &Dev) {
        pipe.write_ed0(dev.address.load(Ordering::Relaxed), dev.low_speed, true);
        arcdrv::sleep_ms(2);
        pipe.reset_queue(false);
        pipe.write_ed0(dev.address.load(Ordering::Relaxed), dev.low_speed, false);
    }

    fn error_of(code: u32) -> Status {
        if code == CC_STALL {
            E_STALL
        } else {
            E_IO
        }
    }

    fn do_control(&self, dev: &Dev, setup: Setup, data: &mut [u8], timeout_ms: u32) -> Result<usize, Status> {
        let pipe = dev.pipe(0).ok_or(E_GONE)?;
        let _one = pipe.busy.lock();
        let len = (setup.length as usize).min(data.len());
        if len > pipe.buf_len {
            return Err(E_INVAL);
        }
        let input = setup.is_in();
        if !input {
            pipe.buf()[..len].copy_from_slice(&data[..len]);
        }
        pipe.write_ed0(dev.address.load(Ordering::Relaxed), dev.low_speed, false);
        pipe.mem.write::<u64>(SETUP_OFF, u64::from_le_bytes(setup.to_bytes()));
        let setup_phys = (pipe.mem.phys() + SETUP_OFF as u64) as u32;
        let mut n = 0;
        pipe.set_td(0, TD_SETUP | TD_DATA0 | TD_NO_INT, setup_phys, 8, pipe.td_phys(1));
        n += 1;
        if len > 0 {
            let dir = if input { TD_IN } else { TD_OUT };
            pipe.set_td(1, dir | TD_DATA1 | TD_ROUNDING | TD_NO_INT, pipe.buf_phys(), len, pipe.td_phys(2));
            n += 1;
        }
        let status_dir = if input && len > 0 { TD_OUT } else { TD_IN };
        pipe.set_td(n, status_dir | TD_DATA1, 0, 0, pipe.td_phys(n + 1));
        n += 1;
        pipe.clear_td(n);
        pipe.launch(n);
        self.kick(&pipe);
        match self.wait_queue(&pipe, timeout_ms) {
            Ok(false) => {}
            Ok(true) => {
                let code = (0..n).map(|i| pipe.td_code(i)).find(|&c| c != CC_OK && c < 0xE).unwrap_or(CC_STALL);
                self.abort(&pipe, dev);
                return Err(Self::error_of(code));
            }
            Err(e) => {
                self.abort(&pipe, dev);
                return Err(e);
            }
        }
        let got = if len > 0 { pipe.td_actual(1, pipe.buf_phys(), len) } else { 0 };
        if input {
            data[..got].copy_from_slice(&pipe.buf()[..got]);
        }
        Ok(got)
    }

    fn do_transfer(&self, dev: &Dev, endpoint: u8, data: &mut [u8], timeout_ms: u32) -> Result<usize, Status> {
        let pipe = dev.pipe(endpoint).ok_or(E_INVAL)?;
        if pipe.poll.lock().is_some() {
            return Err(E_BUSY);
        }
        let _one = pipe.busy.lock();
        let input = endpoint & 0x80 != 0;
        let dir = if input { TD_IN } else { TD_OUT };
        let mut done = 0;
        loop {
            let n = (data.len() - done).min(pipe.buf_len);
            if !input {
                pipe.buf()[..n].copy_from_slice(&data[done..done + n]);
            }
            // One TD per page; only the last may end short without an
            // error, so a short packet earlier stops the queue.
            let tds = n.div_ceil(TD_BYTES).max(1);
            for k in 0..tds {
                let off = k * TD_BYTES;
                let len = (n - off.min(n)).min(TD_BYTES);
                let last = k == tds - 1;
                let flags = dir | if last { TD_ROUNDING } else { TD_NO_INT };
                pipe.set_td(k, flags, pipe.buf_phys() + off as u32, len, pipe.td_phys(k + 1));
            }
            pipe.clear_td(tds);
            pipe.launch(tds);
            self.kick(&pipe);
            let halted = match self.wait_queue(&pipe, timeout_ms) {
                Ok(h) => h,
                Err(e) => {
                    self.abort(&pipe, dev);
                    return Err(e);
                }
            };
            let mut got = 0;
            for k in 0..tds {
                let off = k * TD_BYTES;
                let len = (n - off.min(n)).min(TD_BYTES);
                let code = pipe.td_code(k);
                if code >= 0xE {
                    break; // never reached
                }
                if code != CC_OK && code != CC_UNDERRUN {
                    self.abort(&pipe, dev);
                    return Err(Self::error_of(code));
                }
                let moved = pipe.td_actual(k, pipe.buf_phys() + off as u32, len);
                got += moved;
                if moved < len {
                    break;
                }
            }
            if halted {
                // A short packet before the last TD: the rest is skipped.
                self.abort(&pipe, dev);
            }
            if input {
                data[done..done + got].copy_from_slice(&pipe.buf()[..got]);
            }
            done += got;
            if got < n || done >= data.len() {
                return Ok(done);
            }
        }
    }

    // ------------------------------------------------------------------
    // Interrupt polling

    fn arm_poll(&self, pipe: &Pipe) {
        let mps = (pipe.mps.load(Ordering::Relaxed) as usize).min(pipe.buf_len);
        pipe.set_td(0, TD_IN | TD_ROUNDING, pipe.buf_phys(), mps, pipe.td_phys(1));
        pipe.clear_td(1);
        pipe.launch(1);
    }

    fn service_polls(&self) {
        let pipes: Vec<Arc<Pipe>> = self.polls.lock().clone();
        for pipe in pipes {
            if !pipe.idle() && !pipe.halted() {
                continue;
            }
            let poll = {
                let p = pipe.poll.lock();
                if p.is_some() {
                    pipe.in_callback.store(true, Ordering::Release);
                }
                *p
            };
            let Some(p) = poll else { continue };
            let code = pipe.td_code(0);
            if pipe.halted() || (code != CC_OK && code != CC_UNDERRUN) {
                arcdrv::debug!("endpoint {:#x}: polling stopped (code {code})", pipe.endpoint);
                *pipe.poll.lock() = None;
                pipe.in_callback.store(false, Ordering::Release);
                self.polls.lock().retain(|q| !Arc::ptr_eq(q, &pipe));
                continue;
            }
            let mps = (pipe.mps.load(Ordering::Relaxed) as usize).min(pipe.buf_len);
            let n = pipe.td_actual(0, pipe.buf_phys(), mps).min(64);
            let mut data = [0u8; 64];
            data[..n].copy_from_slice(&pipe.buf()[..n]);
            // SAFETY: the driver's callback with its own context.
            unsafe { (p.report)(p.ctx, data.as_ptr(), n as u32) };
            {
                let guard = pipe.poll.lock();
                if guard.is_some() {
                    self.arm_poll(&pipe);
                }
            }
            pipe.in_callback.store(false, Ordering::Release);
        }
    }
}

// ----------------------------------------------------------------------------
// The interface the USB core uses

impl Host for Ohci {
    fn ports(&self) -> u8 {
        self.ports
    }

    fn port_status(&self, port: u8) -> PortStatus {
        let v = self.regs.read32(self.port_reg(port));
        PortStatus {
            connected: v & PORT_CCS != 0,
            enabled: v & PORT_PES != 0,
            speed: if v & PORT_LSDA != 0 { SPEED_LOW } else { SPEED_FULL },
            changed: v & PORT_CHANGES != 0,
        }
    }

    fn port_clear_changes(&self, port: u8) {
        let r = self.port_reg(port);
        // Writes are commands: only the change bits are written back.
        self.regs.write32(r, self.regs.read32(r) & PORT_CHANGES);
    }

    fn port_reset(&self, port: u8) -> Result<u8, Status> {
        let r = self.port_reg(port);
        self.regs.write32(r, PORT_PRS);
        let ok = arcdrv::wait_until(200, || self.regs.read32(r) & PORT_PRSC != 0);
        self.regs.write32(r, PORT_PRSC);
        let v = self.regs.read32(r);
        if !ok || v & PORT_PES == 0 {
            return Err(E_IO);
        }
        arcdrv::sleep_ms(10); // reset recovery
        Ok(if v & PORT_LSDA != 0 { SPEED_LOW } else { SPEED_FULL })
    }

    fn add_device(&self, at: &Attach, mps0: u16) -> Result<Handle, Status> {
        let low = at.speed == SPEED_LOW;
        let pipe = Pipe::new(EP_CONTROL, 0, mps0, 0, low, CONTROL_BUF).ok_or(E_NOMEM)?;
        let h = self.next_handle.fetch_add(1, Ordering::Relaxed);
        let dev = Arc::new(Dev { address: AtomicU8::new(0), low_speed: low, pipes: IrqLock::new(alloc::vec![pipe.clone()]) });
        self.devices.insert(h, dev);
        self.schedule_add(&pipe);
        Ok(h)
    }

    fn set_address(&self, dev: Handle, address: u8) {
        if let Ok(d) = self.dev(dev) {
            d.address.store(address, Ordering::Relaxed);
        }
    }

    fn address(&self, dev: Handle) -> u8 {
        self.dev(dev).map(|d| d.address.load(Ordering::Relaxed)).unwrap_or(0)
    }

    fn set_mps0(&self, dev: Handle, mps0: u16) -> Result<(), Status> {
        let d = self.dev(dev)?;
        let pipe = d.pipe(0).ok_or(E_GONE)?;
        pipe.mps.store(mps0.clamp(8, 64), Ordering::Relaxed);
        pipe.write_ed0(d.address.load(Ordering::Relaxed), d.low_speed, false);
        Ok(())
    }

    fn configure(&self, dev: Handle, config: &[u8], _hub: Option<HubInfo>) -> Result<(), Status> {
        let d = self.dev(dev)?;
        let address = d.address.load(Ordering::Relaxed);
        for e in arcusb::config_endpoints(config) {
            if d.pipe(e.address).is_some() {
                continue;
            }
            let mps = e.packet_size().clamp(1, 1023);
            let buf = if e.kind() == EP_BULK { BULK_BUF } else { (mps as usize).min(TD_BYTES) };
            let pipe = Pipe::new(e.kind(), e.address, mps, address, d.low_speed, buf).ok_or(E_NOMEM)?;
            d.pipes.lock().push(pipe.clone());
            self.schedule_add(&pipe);
        }
        Ok(())
    }

    fn control(&self, dev: Handle, setup: Setup, data: &mut [u8], timeout_ms: u32) -> Result<usize, Status> {
        let d = self.dev(dev)?;
        self.do_control(&d, setup, data, timeout_ms)
    }

    fn transfer(&self, dev: Handle, endpoint: u8, data: &mut [u8], timeout_ms: u32) -> Result<usize, Status> {
        let d = self.dev(dev)?;
        self.do_transfer(&d, endpoint, data, timeout_ms)
    }

    fn poll_start(&self, dev: Handle, endpoint: u8, report: UsbReport, ctx: *mut c_void) -> Status {
        let Ok(d) = self.dev(dev) else { return E_GONE };
        let Some(pipe) = d.pipe(endpoint) else { return E_INVAL };
        if endpoint & 0x80 == 0 || pipe.kind != EP_INTERRUPT {
            return E_INVAL;
        }
        {
            let _one = pipe.busy.lock();
            let mut poll = pipe.poll.lock();
            if poll.is_some() {
                return E_BUSY;
            }
            *poll = Some(Poll { report, ctx });
            self.arm_poll(&pipe);
        }
        self.polls.lock().push(pipe);
        OK
    }

    fn poll_stop(&self, dev: Handle, endpoint: u8) {
        let Ok(d) = self.dev(dev) else { return };
        let Some(pipe) = d.pipe(endpoint) else { return };
        stop_poll(self, &d, &pipe);
    }

    fn reset_endpoint(&self, dev: Handle, endpoint: u8) -> Status {
        let Ok(d) = self.dev(dev) else { return E_GONE };
        let Some(pipe) = d.pipe(endpoint) else { return E_INVAL };
        let _one = pipe.busy.lock();
        pipe.write_ed0(d.address.load(Ordering::Relaxed), d.low_speed, true);
        arcdrv::sleep_ms(2);
        pipe.reset_queue(true);
        pipe.write_ed0(d.address.load(Ordering::Relaxed), d.low_speed, false);
        OK
    }

    fn remove_device(&self, dev: Handle) {
        let Some(d) = self.devices.remove(dev) else { return };
        let pipes: Vec<Arc<Pipe>> = d.pipes.lock().clone();
        for p in &pipes {
            p.gone.store(true, Ordering::Release);
            stop_poll(self, &d, p);
            p.write_ed0(0, d.low_speed, true);
        }
        self.schedule_remove(&pipes);
    }

    const SLEEPS: bool = true;

    fn stop(&self) {
        self.asleep.store(true, Ordering::Release);
        self.regs.write32(HC_INT_DISABLE, 0xFFFF_FFFF);
        self.regs.write32(HC_CONTROL, 0); // reset state: the bus stops
    }

    fn restart(&self) -> Status {
        take_ownership(&self.regs);
        let st = init(&self.regs, &self.hcca, self.fi);
        if st == OK {
            self.regs.write32(HC_INT_ENABLE, INT_MIE | INT_WDH | INT_UE);
            self.power_ports();
        }
        self.asleep.store(false, Ordering::Release);
        st
    }
}

impl Ohci {
    /// Powers the ports, globally and one by one (whichever the board has),
    /// and waits for the power to be good.
    fn power_ports(&self) {
        self.regs.write32(HC_RH_STATUS, RH_LPSC);
        for p in 1..=self.ports {
            self.regs.write32(self.port_reg(p), PORT_PPS);
        }
        arcdrv::sleep_ms(((self.regs.read32(HC_RH_DESC_A) >> 24) as u64 * 2).max(20));
    }
}

/// Resets the bus, then the controller, and makes it operational with
/// empty lists. It has 2 ms to go operational after the reset, so
/// everything is ready before.
fn init(regs: &Mmio, hcca: &Dma, fi: u32) -> Status {
    regs.write32(HC_INT_DISABLE, 0xFFFF_FFFF);
    regs.write32(HC_CONTROL, 0);
    arcdrv::sleep_ms(50);
    for anchor in [ANCHOR_CONTROL, ANCHOR_BULK, ANCHOR_PERIODIC] {
        hcca.write::<u32>(anchor, ED_SKIP);
    }
    let periodic = (hcca.phys() + ANCHOR_PERIODIC as u64) as u32;
    for i in 0..32 {
        hcca.write::<u32>(i * 4, periodic);
    }
    regs.write32(HC_COMMAND, CMD_HCR);
    if !arcdrv::wait_until(10, || regs.read32(HC_COMMAND) & CMD_HCR == 0) {
        arcdrv::warn!("controller reset timed out");
        return E_TIMEOUT;
    }
    regs.write32(HC_HCCA, hcca.phys() as u32);
    regs.write32(HC_CONTROL_HEAD, (hcca.phys() + ANCHOR_CONTROL as u64) as u32);
    regs.write32(HC_BULK_HEAD, (hcca.phys() + ANCHOR_BULK as u64) as u32);
    let fsmps = (fi - 210) * 6 / 7;
    regs.write32(HC_FM_INTERVAL, fi | fsmps << 16 | (!regs.read32(HC_FM_INTERVAL) & (1 << 31)));
    regs.write32(HC_PERIODIC_START, fi * 9 / 10);
    regs.write32(HC_LS_THRESHOLD, 0x628);
    regs.write32(HC_INT_STATUS, 0xFFFF_FFFF);
    regs.write32(HC_CONTROL, CTL_OPERATIONAL | CTL_CLE | CTL_BLE | CTL_PLE | CTL_CBSR);
    OK
}

/// Stops polling a pipe; never returns while its callback runs (the driver
/// may go away right after).
fn stop_poll(o: &Ohci, dev: &Dev, pipe: &Arc<Pipe>) {
    let stopped = pipe.poll.lock().take().is_some();
    while pipe.in_callback.load(Ordering::Acquire) {
        arcdrv::sleep_ms(1);
    }
    if stopped {
        o.polls.lock().retain(|q| !Arc::ptr_eq(q, pipe));
        o.abort(pipe, dev);
    }
}

// ----------------------------------------------------------------------------
// Bring-up

unsafe extern "C" fn irq(ctx: *mut c_void) {
    // SAFETY: the controller leaked in probe.
    let o = unsafe { &*(ctx as *const Ohci) };
    let sts = o.regs.read32(HC_INT_STATUS) & o.regs.read32(HC_INT_ENABLE) & !INT_MIE;
    if sts == 0 {
        return; // not ours (shared line)
    }
    o.regs.write32(HC_INT_STATUS, sts);
    o.wake.signal();
}

unsafe extern "C" fn poll_thread(ctx: *mut c_void) {
    // SAFETY: the controller leaked in probe.
    let o = unsafe { &*(ctx as *const Ohci) };
    let mut reported = false;
    loop {
        o.wake.wait(if o.has_irq.load(Ordering::Relaxed) { 20 } else { 4 });
        let sts = o.regs.read32(HC_INT_STATUS);
        // Let the controller report its next done queue.
        o.regs.write32(HC_INT_STATUS, sts & INT_WDH);
        if sts & INT_UE != 0 && !reported && !o.asleep.load(Ordering::Acquire) {
            arcdrv::error!("controller stopped (unrecoverable error)");
            reported = true;
        }
        o.service_polls();
    }
}

/// Takes the controller from the firmware (SMM or BIOS driver).
fn take_ownership(regs: &Mmio) {
    let ctl = regs.read32(HC_CONTROL);
    if ctl & CTL_IR != 0 {
        regs.write32(HC_COMMAND, CMD_OCR);
        if !arcdrv::wait_until(500, || regs.read32(HC_CONTROL) & CTL_IR == 0) {
            arcdrv::warn!("the firmware did not release the controller; taking it anyway");
            regs.write32(HC_CONTROL, regs.read32(HC_CONTROL) & !CTL_IR);
        }
    } else if ctl & CTL_HCFS != 0 && ctl & CTL_HCFS != CTL_OPERATIONAL {
        regs.write32(HC_CONTROL, (ctl & !CTL_HCFS) | CTL_RESUME);
        arcdrv::sleep_ms(20);
    }
}

fn probe(dev: &DeviceInfo) -> Status {
    if dev.class != 0x0C || dev.subclass != 0x03 || dev.prog_if != 0x10 {
        return E_NODEV;
    }
    let bar = dev.resources[0];
    if bar.kind != RES_MEMORY || bar.base == 0 {
        return E_IO;
    }
    Pci(dev.id).enable(false, true, true);
    let Some(regs) = Mmio::map(bar.base, 0x1000) else { return E_NOMEM };
    take_ownership(&regs);

    regs.write32(HC_INT_DISABLE, 0xFFFF_FFFF);
    let fi = match regs.read32(HC_FM_INTERVAL) & 0x3FFF {
        0 => 11999,
        v => v,
    };
    let Some(hcca) = Dma::new(4096) else { return E_NOMEM };
    let st = init(&regs, &hcca, fi);
    if st != OK {
        return st;
    }

    let ports = (regs.read32(HC_RH_DESC_A) & 0xFF).min(15) as u8;
    if ports == 0 {
        return E_NODEV;
    }

    let o: &'static Ohci = Box::leak(Box::new(Ohci {
        regs,
        ports,
        hcca,
        schedule: Mutex::new(Vec::new()),
        devices: Table::new(),
        next_handle: AtomicU32::new(1),
        polls: IrqLock::new(Vec::new()),
        wake: Event::new(),
        has_irq: AtomicBool::new(false),
        fi,
        asleep: AtomicBool::new(false),
    }));
    let ctx = o as *const Ohci as *mut c_void;
    o.has_irq.store(arcdrv::irq_attach_any(dev, irq, ctx), Ordering::Relaxed);
    arcdrv::spawn("ohci-poll", poll_thread, ctx);
    o.regs.write32(HC_INT_ENABLE, INT_MIE | INT_WDH | INT_UE);
    o.power_ports();
    arcdrv::info!("{} ports, {}", ports, if o.has_irq.load(Ordering::Relaxed) { "interrupts" } else { "polling" });
    arcusb::start(o, dev.id, "ohci-ports");
    OK
}

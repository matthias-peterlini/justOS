//! EHCI (USB 2.0) host controller driver.
//!
//! High-speed USB of PCs from 2002 to about 2015. Low- and full-speed
//! devices on a root port are handed to the companion UHCI or OHCI
//! controller next to it; behind a USB 2 hub they stay here and are reached
//! through the hub's transaction translator (split transactions).
//!
//! Control and bulk endpoints sit on the asynchronous ring, interrupt
//! endpoints on the periodic list (polled every frame). Each endpoint has a
//! queue head, its transfer descriptors (qTDs) and a data buffer in one DMA
//! block; transfers on an endpoint run one at a time.

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
use arcusb::{Attach, Bus, Handle, Host, HubInfo, PortStatus, Table};
use core::ffi::c_void;
use core::sync::atomic::{fence, AtomicBool, AtomicU16, AtomicU32, AtomicU8, Ordering};

arcdrv::driver! {
    name: "ehci",
    version: "1.1.0",
    probe: probe,
}

// Capability registers
const CAPLENGTH: usize = 0x00;
const HCSPARAMS: usize = 0x04;
const HCCPARAMS: usize = 0x08;

// Operational registers
const USBCMD: usize = 0x00;
const USBSTS: usize = 0x04;
const USBINTR: usize = 0x08;
const CTRLDSSEGMENT: usize = 0x10;
const PERIODICLISTBASE: usize = 0x14;
const ASYNCLISTADDR: usize = 0x18;
const CONFIGFLAG: usize = 0x40;
const PORTSC: usize = 0x44;

const CMD_RUN: u32 = 1;
const CMD_RESET: u32 = 1 << 1;
const CMD_PSE: u32 = 1 << 4;
const CMD_ASE: u32 = 1 << 5;
const CMD_IAAD: u32 = 1 << 6;
/// Interrupt threshold: 8 microframes (1 ms).
const CMD_ITC: u32 = 8 << 16;

const STS_PCD: u32 = 1 << 2;
const STS_HSE: u32 = 1 << 4;
const STS_IAA: u32 = 1 << 5;
const STS_HALTED: u32 = 1 << 12;
/// Interrupt, error, port change, frame rollover, system error, IAA.
const STS_EVENTS: u32 = 0x3F;
/// Interrupts: completion, error, port change, system error, IAA.
const INTR_ENABLE: u32 = 0x37;

// PORTSC
const PORT_CCS: u32 = 1;
const PORT_CSC: u32 = 1 << 1;
const PORT_PE: u32 = 1 << 2;
const PORT_PEC: u32 = 1 << 3;
const PORT_OCC: u32 = 1 << 5;
const PORT_PR: u32 = 1 << 8;
const PORT_LINE: u32 = 3 << 10;
const PORT_LINE_K: u32 = 1 << 10;
const PORT_PP: u32 = 1 << 12;
const PORT_OWNER: u32 = 1 << 13;
/// Write-1-to-clear change bits.
const PORT_CHANGES: u32 = PORT_CSC | PORT_PEC | PORT_OCC;

// Link pointers
const LINK_T: u32 = 1;
const LINK_QH: u32 = 1 << 1;

// qTD token
const QTD_ACTIVE: u32 = 1 << 7;
const QTD_HALTED: u32 = 1 << 6;
const QTD_PID_OUT: u32 = 0;
const QTD_PID_IN: u32 = 1 << 8;
const QTD_PID_SETUP: u32 = 2 << 8;
const QTD_CERR: u32 = 3 << 10;
const QTD_IOC: u32 = 1 << 15;
const QTD_TOGGLE: u32 = 1 << 31;

// Queue head
const QH_HEAD: u32 = 1 << 15;
const QH_DTC: u32 = 1 << 14;
const QH_CONTROL: u32 = 1 << 27;
const QH_MULT1: u32 = 1 << 30;

/// Layout of a pipe's DMA block: queue head, setup packet, qTDs, then the
/// data buffer from the second page.
const SETUP_OFF: usize = 96;
const QTD_OFF: usize = 128;
const QTD_SIZE: usize = 64;
const BUF_OFF: usize = 4096;
/// The qTD that a short packet sends the queue to: inactive, so it stops.
const STOP: usize = 0;
const CONTROL_BUF: usize = 4096;
const BULK_BUF: usize = 65536;
/// Bytes one qTD moves (its buffer starts on a page boundary).
const QTD_BYTES: usize = 16384;

const FRAMES: usize = 1024;

/// EHCI speed codes in a queue head.
fn eps(speed: u8) -> u32 {
    match speed {
        SPEED_FULL => 0,
        SPEED_LOW => 1,
        _ => 2,
    }
}

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
    fn new(kind: u8, endpoint: u8, mps: u16, buf_len: usize) -> Option<Arc<Pipe>> {
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
        p.mem.write::<u32>(0, LINK_T);
        p.clear_overlay(false);
        // The stop qTD: inactive, goes nowhere.
        p.mem.write::<u32>(QTD_OFF + STOP * QTD_SIZE, LINK_T);
        p.mem.write::<u32>(QTD_OFF + STOP * QTD_SIZE + 4, LINK_T);
        Some(Arc::new(p))
    }

    fn qh(&self) -> u32 {
        self.mem.phys() as u32
    }

    fn qtd_phys(&self, i: usize) -> u32 {
        (self.mem.phys() + (QTD_OFF + i * QTD_SIZE) as u64) as u32
    }

    fn buf_phys(&self) -> u32 {
        (self.mem.phys() + BUF_OFF as u64) as u32
    }

    fn buf(&self) -> &mut [u8] {
        &mut self.mem.as_mut_slice()[BUF_OFF..BUF_OFF + self.buf_len]
    }

    /// Endpoint characteristics and capabilities of the queue head.
    fn set_endpoint(&self, address: u8, speed: u8, tt: Option<(u8, u8)>) {
        let mps = self.mps.load(Ordering::Relaxed) as u32 & 0x7FF;
        let mut info1 = address as u32 & 0x7F | ((self.endpoint & 0xF) as u32) << 8 | eps(speed) << 12 | mps << 16;
        if self.kind == EP_CONTROL {
            info1 |= QH_DTC;
            if speed != SPEED_HIGH {
                info1 |= QH_CONTROL;
            }
        }
        let mut info2 = QH_MULT1;
        if let Some((hub, port)) = tt {
            info2 |= (hub as u32 & 0x7F) << 16 | (port as u32 & 0x7F) << 23;
        }
        if self.kind == EP_INTERRUPT {
            // Microframe 0 every frame; split completions in 2-4.
            info2 |= 0x01;
            if speed != SPEED_HIGH {
                info2 |= 0x1C << 8;
            }
        }
        self.mem.write::<u32>(4, info1);
        self.mem.write::<u32>(8, info2);
    }

    /// Empties the overlay: no qTD, not active, not halted.
    fn clear_overlay(&self, keep_toggle: bool) {
        let toggle = if keep_toggle { self.mem.read::<u32>(24) & QTD_TOGGLE } else { 0 };
        self.mem.write::<u32>(12, 0);
        self.mem.write::<u32>(16, LINK_T);
        self.mem.write::<u32>(20, LINK_T);
        for off in (28..68).step_by(4) {
            self.mem.write::<u32>(off, 0);
        }
        fence(Ordering::SeqCst);
        self.mem.write::<u32>(24, toggle);
    }

    /// Writes qTD `i` for `len` bytes at `start` (0: no data).
    fn set_qtd(&self, i: usize, next: u32, alt: u32, token: u32, start: u32, len: usize) {
        let at = QTD_OFF + i * QTD_SIZE;
        self.mem.write::<u32>(at, next);
        self.mem.write::<u32>(at + 4, alt);
        self.mem.write::<u32>(at + 12, start);
        let page = start & !0xFFF;
        for k in 1..5 {
            self.mem.write::<u32>(at + 12 + k * 4, if len == 0 { 0 } else { page + k as u32 * 4096 });
        }
        fence(Ordering::SeqCst);
        self.mem.write::<u32>(at + 8, token | (len as u32) << 16 | QTD_CERR);
    }

    fn token(&self, i: usize) -> u32 {
        self.mem.read::<u32>(QTD_OFF + i * QTD_SIZE + 8)
    }

    fn remaining(&self, i: usize) -> usize {
        (self.token(i) >> 16 & 0x7FFF) as usize
    }

    /// Hands qTD `first` to the controller (the overlay must be idle).
    fn start(&self, first: usize) {
        fence(Ordering::SeqCst);
        self.mem.write::<u32>(16, self.qtd_phys(first));
    }

    fn overlay_halted(&self) -> bool {
        self.mem.read::<u32>(24) & QTD_HALTED != 0
    }
}

struct Dev {
    address: AtomicU8,
    speed: u8,
    /// Transaction translator: (hub address, port), for low/full speed.
    tt: Option<(u8, u8)>,
    pipes: IrqLock<Vec<Arc<Pipe>>>,
}

impl Dev {
    fn pipe(&self, endpoint: u8) -> Option<Arc<Pipe>> {
        self.pipes.lock().iter().find(|p| p.endpoint == endpoint).cloned()
    }

    fn setup_pipe(&self, pipe: &Pipe) {
        pipe.set_endpoint(self.address.load(Ordering::Relaxed), self.speed, self.tt);
    }
}

struct Ehci {
    regs: Mmio,
    op: usize,
    ports: u8,
    /// Companion controllers that take low/full-speed devices.
    companions: bool,
    frames: Dma,
    /// Head of the asynchronous ring (never removed).
    anchor: Dma,
    schedule: Mutex<Vec<Arc<Pipe>>>,
    devices: Table<Arc<Dev>>,
    next_handle: AtomicU32,
    polls: IrqLock<Vec<Arc<Pipe>>>,
    wake: Event,
    iaa: AtomicBool,
    has_irq: AtomicBool,
    bus: IrqLock<Option<&'static Bus<Ehci>>>,
    /// Kernel id of the controller (for its PCI configuration).
    pci: u32,
    /// Stopped for sleep (a halted controller is not an error then).
    asleep: AtomicBool,
}

impl Ehci {
    fn op_read(&self, reg: usize) -> u32 {
        self.regs.read32(self.op + reg)
    }

    fn op_write(&self, reg: usize, v: u32) {
        self.regs.write32(self.op + reg, v)
    }

    fn portsc(&self, port: u8) -> u32 {
        self.op_read(PORTSC + (port as usize - 1) * 4)
    }

    fn set_portsc(&self, port: u8, v: u32) {
        self.op_write(PORTSC + (port as usize - 1) * 4, v)
    }

    fn dev(&self, h: Handle) -> Result<Arc<Dev>, Status> {
        self.devices.get(h).ok_or(E_GONE)
    }

    /// Gives a root port to the companion controller.
    fn release_port(&self, port: u8) {
        let v = self.portsc(port);
        self.set_portsc(port, (v & !PORT_CHANGES) | PORT_OWNER);
    }

    // ------------------------------------------------------------------
    // Schedules

    /// Rewrites both lists back to front, so the controller always sees
    /// valid lists while they change.
    fn relink(&self, list: &[Arc<Pipe>]) {
        let anchor = self.anchor.phys() as u32 | LINK_QH;
        let mut next = anchor;
        for p in list.iter().rev().filter(|p| p.kind != EP_INTERRUPT) {
            p.mem.write::<u32>(0, next);
            next = p.qh() | LINK_QH;
        }
        fence(Ordering::SeqCst);
        self.anchor.write::<u32>(0, next);

        let mut next = LINK_T;
        for p in list.iter().rev().filter(|p| p.kind == EP_INTERRUPT) {
            p.mem.write::<u32>(0, next);
            next = p.qh() | LINK_QH;
        }
        fence(Ordering::SeqCst);
        for i in 0..FRAMES {
            self.frames.write::<u32>(i * 4, next);
        }
    }

    fn schedule_add(&self, pipe: &Arc<Pipe>) {
        let mut list = self.schedule.lock();
        list.push(pipe.clone());
        self.relink(&list);
    }

    /// Takes queue heads out of the schedules and waits until the
    /// controller no longer looks at them.
    fn schedule_remove(&self, pipes: &[Arc<Pipe>]) {
        let mut list = self.schedule.lock();
        list.retain(|p| !pipes.iter().any(|q| Arc::ptr_eq(p, q)));
        self.relink(&list);
        self.quiesce();
    }

    /// Waits for the controller to let go of unlinked queue heads: the
    /// async advance doorbell, and a frame for the periodic list.
    fn quiesce(&self) {
        self.iaa.store(false, Ordering::Relaxed);
        self.op_write(USBSTS, STS_IAA);
        self.op_write(USBCMD, self.op_read(USBCMD) | CMD_IAAD);
        arcdrv::wait_until(50, || self.iaa.load(Ordering::Relaxed) || self.op_read(USBSTS) & STS_IAA != 0);
        self.op_write(USBSTS, STS_IAA);
        arcdrv::sleep_ms(2);
    }

    /// Takes a queue back after a transfer that did not finish: unlink it,
    /// empty it, link it again.
    fn reset_pipe(&self, pipe: &Arc<Pipe>, keep_toggle: bool) {
        let mut list = self.schedule.lock();
        let others: Vec<Arc<Pipe>> = list.iter().filter(|p| !Arc::ptr_eq(p, pipe)).cloned().collect();
        self.relink(&others);
        self.quiesce();
        pipe.clear_overlay(keep_toggle);
        list.retain(|p| !Arc::ptr_eq(p, pipe));
        if !pipe.gone.load(Ordering::Acquire) {
            list.push(pipe.clone());
        }
        self.relink(&list);
    }

    // ------------------------------------------------------------------
    // Transfers

    /// Waits until qTDs `from..=last` are done. Returns the index of an IN
    /// qTD that ended short (the queue left it for its alternate), if one did.
    fn wait_qtds(&self, pipe: &Pipe, from: usize, last: usize, deadline: u64) -> Result<Option<usize>, Status> {
        let mut i = from;
        loop {
            while i <= last {
                let t = pipe.token(i);
                if t & QTD_ACTIVE != 0 {
                    break;
                }
                if t & QTD_HALTED != 0 {
                    // Halted without other errors: the device stalled.
                    return Err(if t & 0x3C == 0 { E_STALL } else { E_IO });
                }
                i += 1;
                if t & 0x300 == QTD_PID_IN && t >> 16 & 0x7FFF != 0 {
                    return Ok(Some(i - 1));
                }
            }
            if i > last {
                return Ok(None);
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

    fn fail(&self, pipe: &Arc<Pipe>, e: Status, keep_toggle: bool) -> Status {
        if e == E_TIMEOUT || !pipe.overlay_halted() {
            self.reset_pipe(pipe, keep_toggle);
        } else {
            // A halted queue head is left alone by the controller.
            pipe.clear_overlay(keep_toggle);
        }
        e
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
        dev.setup_pipe(&pipe);
        pipe.mem.write::<u64>(SETUP_OFF, u64::from_le_bytes(setup.to_bytes()));
        let setup_phys = (pipe.mem.phys() + SETUP_OFF as u64) as u32;
        let status = if len > 0 { 3 } else { 2 };
        pipe.set_qtd(1, pipe.qtd_phys(2), LINK_T, QTD_ACTIVE | QTD_PID_SETUP, setup_phys, 8);
        if len > 0 {
            let pid = if input { QTD_PID_IN } else { QTD_PID_OUT };
            // A short data stage goes straight on to the status stage.
            pipe.set_qtd(2, pipe.qtd_phys(3), pipe.qtd_phys(3), QTD_ACTIVE | pid | QTD_TOGGLE, pipe.buf_phys(), len);
        }
        let status_pid = if input && len > 0 { QTD_PID_OUT } else { QTD_PID_IN };
        pipe.set_qtd(status, LINK_T, LINK_T, QTD_ACTIVE | status_pid | QTD_TOGGLE | QTD_IOC, 0, 0);
        pipe.start(1);
        let deadline = arcdrv::time_ms() + timeout_ms as u64;
        let mut result = self.wait_qtds(&pipe, 1, status, deadline);
        if let Ok(Some(i)) = result {
            if i < status {
                result = self.wait_qtds(&pipe, status, status, deadline);
            }
        }
        if let Err(e) = result {
            return Err(self.fail(&pipe, e, false));
        }
        let got = if len > 0 { len - pipe.remaining(2).min(len) } else { 0 };
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
        let pid = if input { QTD_PID_IN } else { QTD_PID_OUT };
        let stop = pipe.qtd_phys(STOP);
        let mut done = 0;
        loop {
            let n = (data.len() - done).min(pipe.buf_len);
            if !input {
                pipe.buf()[..n].copy_from_slice(&data[done..done + n]);
            }
            let count = n.div_ceil(QTD_BYTES).max(1);
            for k in 0..count {
                let off = k * QTD_BYTES;
                let len = (n - off.min(n)).min(QTD_BYTES);
                let last = k == count - 1;
                let next = if last { LINK_T } else { pipe.qtd_phys(k + 2) };
                let ioc = if last { QTD_IOC } else { 0 };
                pipe.set_qtd(k + 1, next, stop, QTD_ACTIVE | pid | ioc, pipe.buf_phys() + off as u32, len);
            }
            pipe.start(1);
            let deadline = arcdrv::time_ms() + timeout_ms as u64;
            if let Err(e) = self.wait_qtds(&pipe, 1, count, deadline) {
                return Err(self.fail(&pipe, e, e == E_TIMEOUT));
            }
            let mut got = 0;
            for k in 0..count {
                let t = pipe.token(k + 1);
                if t & QTD_ACTIVE != 0 {
                    break;
                }
                let len = (n - (k * QTD_BYTES).min(n)).min(QTD_BYTES);
                let moved = len - pipe.remaining(k + 1).min(len);
                got += moved;
                if moved < len {
                    break;
                }
            }
            if got < n {
                // The rest of the queue was skipped: make sure nothing of
                // it is left behind (the overlay sits on the stop qTD).
                for k in 0..count {
                    let t = pipe.token(k + 1);
                    pipe.mem.write::<u32>(QTD_OFF + (k + 1) * QTD_SIZE + 8, t & !QTD_ACTIVE);
                }
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
        pipe.set_qtd(1, LINK_T, LINK_T, QTD_ACTIVE | QTD_PID_IN | QTD_IOC, pipe.buf_phys(), mps);
        pipe.start(1);
    }

    fn service_polls(&self) {
        let pipes: Vec<Arc<Pipe>> = self.polls.lock().clone();
        for pipe in pipes {
            let t = pipe.token(1);
            if t & QTD_ACTIVE != 0 {
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
            if t & QTD_HALTED != 0 {
                arcdrv::debug!("endpoint {:#x}: polling stopped (status {:#x})", pipe.endpoint, t & 0xFF);
                *pipe.poll.lock() = None;
                pipe.in_callback.store(false, Ordering::Release);
                self.polls.lock().retain(|q| !Arc::ptr_eq(q, &pipe));
                pipe.clear_overlay(false);
                continue;
            }
            let mps = (pipe.mps.load(Ordering::Relaxed) as usize).min(pipe.buf_len);
            let n = (mps - pipe.remaining(1).min(mps)).min(64);
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

impl Host for Ehci {
    fn ports(&self) -> u8 {
        self.ports
    }

    fn port_status(&self, port: u8) -> PortStatus {
        let v = self.portsc(port);
        if v & PORT_OWNER != 0 {
            // The companion controller has it.
            return PortStatus::default();
        }
        PortStatus { connected: v & PORT_CCS != 0, enabled: v & PORT_PE != 0, speed: SPEED_HIGH, changed: v & PORT_CHANGES != 0 }
    }

    fn port_clear_changes(&self, port: u8) {
        let v = self.portsc(port);
        self.set_portsc(port, v);
    }

    fn port_reset(&self, port: u8) -> Result<u8, Status> {
        let v = self.portsc(port);
        if v & PORT_CCS == 0 {
            return Err(E_NODEV);
        }
        // A low-speed device shows before any reset: not ours.
        if v & PORT_LINE == PORT_LINE_K && self.companions {
            self.release_port(port);
            return Err(E_NODEV);
        }
        self.set_portsc(port, (v & !PORT_CHANGES & !PORT_PE) | PORT_PR);
        arcdrv::sleep_ms(50);
        self.set_portsc(port, self.portsc(port) & !PORT_CHANGES & !PORT_PR);
        arcdrv::wait_until(10, || self.portsc(port) & PORT_PR == 0);
        let v = self.portsc(port);
        if v & PORT_PE == 0 {
            // Not a high-speed device: a full-speed one, for the companion.
            if self.companions && v & PORT_CCS != 0 {
                self.release_port(port);
            }
            return Err(E_NODEV);
        }
        arcdrv::sleep_ms(10); // reset recovery
        Ok(SPEED_HIGH)
    }

    fn add_device(&self, at: &Attach, mps0: u16) -> Result<Handle, Status> {
        let tt = match at.tt {
            Some((hub, port, _)) => Some((self.address(hub), port)),
            None if at.speed != SPEED_HIGH => return Err(E_NODEV),
            None => None,
        };
        let pipe = Pipe::new(EP_CONTROL, 0, mps0, CONTROL_BUF).ok_or(E_NOMEM)?;
        let dev = Arc::new(Dev { address: AtomicU8::new(0), speed: at.speed, tt, pipes: IrqLock::new(alloc::vec![pipe.clone()]) });
        dev.setup_pipe(&pipe);
        let h = self.next_handle.fetch_add(1, Ordering::Relaxed);
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
        d.setup_pipe(&pipe);
        Ok(())
    }

    fn configure(&self, dev: Handle, config: &[u8], _hub: Option<HubInfo>) -> Result<(), Status> {
        let d = self.dev(dev)?;
        for e in arcusb::config_endpoints(config) {
            if d.pipe(e.address).is_some() {
                continue;
            }
            let mps = e.packet_size().max(1);
            let buf = if e.kind() == EP_BULK { BULK_BUF } else { (mps as usize).min(4096) };
            let pipe = Pipe::new(e.kind(), e.address, mps, buf).ok_or(E_NOMEM)?;
            d.setup_pipe(&pipe);
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
        stop_poll(self, &pipe);
    }

    fn reset_endpoint(&self, dev: Handle, endpoint: u8) -> Status {
        let Ok(d) = self.dev(dev) else { return E_GONE };
        let Some(pipe) = d.pipe(endpoint) else { return E_INVAL };
        let _one = pipe.busy.lock();
        self.reset_pipe(&pipe, false);
        OK
    }

    fn remove_device(&self, dev: Handle) {
        let Some(d) = self.devices.remove(dev) else { return };
        let pipes: Vec<Arc<Pipe>> = d.pipes.lock().clone();
        for p in &pipes {
            p.gone.store(true, Ordering::Release);
            if p.poll.lock().take().is_some() {
                self.polls.lock().retain(|q| !Arc::ptr_eq(q, p));
            }
            while p.in_callback.load(Ordering::Acquire) {
                arcdrv::sleep_ms(1);
            }
        }
        self.schedule_remove(&pipes);
    }

    const SLEEPS: bool = true;

    fn stop(&self) {
        self.asleep.store(true, Ordering::Release);
        self.op_write(USBINTR, 0);
        self.op_write(USBCMD, self.op_read(USBCMD) & !CMD_RUN);
        arcdrv::wait_until(20, || self.op_read(USBSTS) & STS_HALTED != 0);
    }

    fn restart(&self) -> Status {
        take_ownership(&Pci(self.pci), ((self.regs.read32(HCCPARAMS) >> 8) & 0xFF) as u16);
        let mut st = reset(&self.regs, self.op);
        if st == OK {
            init_lists(&self.frames, &self.anchor);
            st = self.start();
        }
        self.asleep.store(false, Ordering::Release);
        st
    }
}

impl Ehci {
    /// Gives the controller its lists, runs it, takes every port and
    /// powers them.
    fn start(&self) -> Status {
        self.op_write(CTRLDSSEGMENT, 0);
        self.op_write(PERIODICLISTBASE, self.frames.phys() as u32);
        self.op_write(ASYNCLISTADDR, self.anchor.phys() as u32);
        self.op_write(USBSTS, STS_EVENTS);
        self.op_write(USBINTR, INTR_ENABLE);
        self.op_write(USBCMD, CMD_RUN | CMD_ASE | CMD_PSE | CMD_ITC);
        if !arcdrv::wait_until(100, || self.op_read(USBSTS) & STS_HALTED == 0) {
            arcdrv::warn!("controller does not start");
            return E_IO;
        }
        // Route every port here; slower devices are handed back one by one.
        self.op_write(CONFIGFLAG, 1);
        arcdrv::sleep_ms(5);
        if self.regs.read32(HCSPARAMS) & (1 << 4) != 0 {
            for p in 1..=self.ports {
                let v = self.portsc(p);
                self.set_portsc(p, (v & !PORT_CHANGES) | PORT_PP);
            }
            arcdrv::sleep_ms(20);
        }
        OK
    }
}

/// Stops and resets the controller.
fn reset(regs: &Mmio, op: usize) -> Status {
    regs.write32(op + USBINTR, 0);
    regs.write32(op + USBCMD, regs.read32(op + USBCMD) & !CMD_RUN);
    arcdrv::wait_until(20, || regs.read32(op + USBSTS) & STS_HALTED != 0);
    regs.write32(op + USBCMD, CMD_RESET);
    if !arcdrv::wait_until(250, || regs.read32(op + USBCMD) & CMD_RESET == 0) {
        arcdrv::warn!("controller reset timed out");
        return E_TIMEOUT;
    }
    OK
}

/// An empty periodic list, and the asynchronous ring with only its anchor:
/// linked to itself, never active.
fn init_lists(frames: &Dma, anchor: &Dma) {
    for i in 0..FRAMES {
        frames.write::<u32>(i * 4, LINK_T);
    }
    anchor.write::<u32>(0, anchor.phys() as u32 | LINK_QH);
    anchor.write::<u32>(4, QH_HEAD);
    anchor.write::<u32>(8, QH_MULT1);
    anchor.write::<u32>(16, LINK_T);
    anchor.write::<u32>(20, LINK_T);
    anchor.write::<u32>(24, QTD_HALTED);
}

/// Stops polling a pipe; never returns while its callback runs (the driver
/// may go away right after).
fn stop_poll(e: &Ehci, pipe: &Arc<Pipe>) {
    let stopped = pipe.poll.lock().take().is_some();
    while pipe.in_callback.load(Ordering::Acquire) {
        arcdrv::sleep_ms(1);
    }
    if stopped {
        e.polls.lock().retain(|q| !Arc::ptr_eq(q, pipe));
        e.reset_pipe(pipe, true);
    }
}

// ----------------------------------------------------------------------------
// Bring-up

unsafe extern "C" fn irq(ctx: *mut c_void) {
    // SAFETY: the controller leaked in probe.
    let e = unsafe { &*(ctx as *const Ehci) };
    let sts = e.op_read(USBSTS) & STS_EVENTS;
    if sts == 0 {
        return; // not ours (shared line)
    }
    e.op_write(USBSTS, sts);
    if sts & STS_IAA != 0 {
        e.iaa.store(true, Ordering::Relaxed);
    }
    if sts & STS_PCD != 0 {
        if let Some(bus) = *e.bus.lock() {
            arcusb::port_changed(bus);
        }
    }
    e.wake.signal();
}

unsafe extern "C" fn poll_thread(ctx: *mut c_void) {
    // SAFETY: the controller leaked in probe.
    let e = unsafe { &*(ctx as *const Ehci) };
    let mut reported = false;
    loop {
        e.wake.wait(if e.has_irq.load(Ordering::Relaxed) { 20 } else { 4 });
        let sts = e.op_read(USBSTS);
        if !e.has_irq.load(Ordering::Relaxed) && sts & STS_PCD != 0 {
            e.op_write(USBSTS, STS_PCD);
            if let Some(bus) = *e.bus.lock() {
                arcusb::port_changed(bus);
            }
        }
        if sts & (STS_HSE | STS_HALTED) != 0 && !reported && !e.asleep.load(Ordering::Acquire) {
            arcdrv::error!("controller stopped (status {sts:#x})");
            reported = true;
        }
        e.service_polls();
    }
}

/// Takes the controller from the firmware's legacy USB support.
fn take_ownership(pci: &Pci, eecp: u16) {
    let mut at = eecp;
    for _ in 0..16 {
        if at < 0x40 {
            break;
        }
        let v = pci.read32(at);
        if v & 0xFF == 1 {
            if v & (1 << 16) != 0 {
                pci.write32(at, v | 1 << 24);
                if !arcdrv::wait_until(1000, || pci.read32(at) & (1 << 16) == 0) {
                    arcdrv::warn!("the firmware did not release the controller; taking it anyway");
                    pci.write32(at, (pci.read32(at) & !(1 << 16)) | 1 << 24);
                }
            } else {
                pci.write32(at, v | 1 << 24);
            }
            // No more system management interrupts; clear pending ones.
            pci.write32(at + 4, 0xE000_0000);
        }
        at = ((v >> 8) & 0xFF) as u16;
        if at == 0 {
            break;
        }
    }
}

fn probe(dev: &DeviceInfo) -> Status {
    if dev.class != 0x0C || dev.subclass != 0x03 || dev.prog_if != 0x20 {
        return E_NODEV;
    }
    let bar = dev.resources[0];
    if bar.kind != RES_MEMORY || bar.base == 0 {
        return E_IO;
    }
    let pci = Pci(dev.id);
    pci.enable(false, true, true);
    let Some(regs) = Mmio::map(bar.base, (bar.size as usize).clamp(0x100, 0x1000)) else { return E_NOMEM };
    let op = regs.read8(CAPLENGTH) as usize;
    let hcs = regs.read32(HCSPARAMS);
    let hcc = regs.read32(HCCPARAMS);
    let ports = (hcs & 0xF) as u8;
    let companions = (hcs >> 12) & 0xF != 0;
    take_ownership(&pci, ((hcc >> 8) & 0xFF) as u16);
    let st = reset(&regs, op);
    if st != OK {
        return st;
    }
    let (Some(frames), Some(anchor)) = (Dma::new(FRAMES * 4), Dma::new(128)) else { return E_NOMEM };
    init_lists(&frames, &anchor);

    let e: &'static Ehci = Box::leak(Box::new(Ehci {
        regs,
        op,
        ports,
        companions,
        frames,
        anchor,
        schedule: Mutex::new(Vec::new()),
        devices: Table::new(),
        next_handle: AtomicU32::new(1),
        polls: IrqLock::new(Vec::new()),
        wake: Event::new(),
        iaa: AtomicBool::new(false),
        has_irq: AtomicBool::new(false),
        bus: IrqLock::new(None),
        pci: dev.id,
        asleep: AtomicBool::new(false),
    }));
    let ctx = e as *const Ehci as *mut c_void;
    e.has_irq.store(arcdrv::irq_attach_any(dev, irq, ctx), Ordering::Relaxed);
    arcdrv::spawn("ehci-poll", poll_thread, ctx);
    let st = e.start();
    if st != OK {
        return st;
    }
    arcdrv::info!(
        "{} ports{}, {}",
        ports,
        if companions { ", with companion controllers" } else { "" },
        if e.has_irq.load(Ordering::Relaxed) { "interrupts" } else { "polling" }
    );
    let bus = arcusb::start(e, dev.id, "ehci-ports");
    *e.bus.lock() = Some(bus);
    OK
}

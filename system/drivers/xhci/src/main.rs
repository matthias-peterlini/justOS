//! xHCI (USB 3) host controller driver.
//!
//! Brings the controller up (taking it over from the firmware first), runs
//! its command and event rings and the transfer rings of every endpoint,
//! and implements `arcusb::Host`, so the USB core can enumerate devices and
//! hubs on its ports. Low-, full-, high- and SuperSpeed devices all work.
//!
//! Events are handled by one thread per controller, woken by the
//! controller's interrupt (MSI if available) and, as a safety net, every few
//! milliseconds.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use arcdrv::usb::*;
use arcdrv::{
    DeviceInfo, Dma, Event, IrqLock, Mmio, Mutex, Pci, Status, E_BUSY, E_GONE, E_INVAL, E_IO, E_NODEV, E_NOMEM, E_STALL, E_TIMEOUT, OK,
    RES_MEMORY,
};
use arcusb::{Attach, Bus, Handle, Host, HubInfo, PortStatus};
use core::ffi::c_void;
use core::sync::atomic::{fence, AtomicBool, Ordering};

arcdrv::driver! {
    name: "xhci",
    version: "1.1.0",
    probe: probe,
}

// Capability registers
const CAPLENGTH: usize = 0x00;
const HCSPARAMS1: usize = 0x04;
const HCSPARAMS2: usize = 0x08;
const HCCPARAMS1: usize = 0x10;
const DBOFF: usize = 0x14;
const RTSOFF: usize = 0x18;

// Operational registers
const USBCMD: usize = 0x00;
const USBSTS: usize = 0x04;
const CRCR: usize = 0x18;
const DCBAAP: usize = 0x30;
const CONFIG: usize = 0x38;
const PORTSC: usize = 0x400;

const CMD_RUN: u32 = 1;
const CMD_RESET: u32 = 1 << 1;
const CMD_INTE: u32 = 1 << 2;
const STS_HALTED: u32 = 1;
const STS_EINT: u32 = 1 << 3;
const STS_PCD: u32 = 1 << 4;
const STS_CNR: u32 = 1 << 11;

// PORTSC bits
const PORT_CCS: u32 = 1;
const PORT_PED: u32 = 1 << 1;
const PORT_PR: u32 = 1 << 4;
const PORT_PP: u32 = 1 << 9;
const PORT_PRC: u32 = 1 << 21;
const PORT_WPR: u32 = 1 << 31;
/// Status change bits (write 1 to clear).
const PORT_CHANGES: u32 = 0x7F << 17;
/// Bits that keep their value when written back (everything else is either
/// read-only or must be written as 0 to stay unchanged).
const PORT_PRESERVE: u32 = 0x0E00_C3E0;

// TRB types
const TRB_NORMAL: u32 = 1;
const TRB_SETUP: u32 = 2;
const TRB_DATA: u32 = 3;
const TRB_STATUS: u32 = 4;
const TRB_LINK: u32 = 6;
const TRB_ENABLE_SLOT: u32 = 9;
const TRB_DISABLE_SLOT: u32 = 10;
const TRB_ADDRESS_DEVICE: u32 = 11;
const TRB_CONFIGURE_EP: u32 = 12;
const TRB_EVALUATE_CTX: u32 = 13;
const TRB_RESET_EP: u32 = 14;
const TRB_STOP_EP: u32 = 15;
const TRB_SET_DEQUEUE: u32 = 16;
const TRB_TRANSFER_EVENT: u32 = 32;
const TRB_COMMAND_DONE: u32 = 33;
const TRB_PORT_CHANGE: u32 = 34;

// TRB control bits
const TRB_CYCLE: u32 = 1;
const TRB_TOGGLE: u32 = 1 << 1; // link TRB: toggle cycle
const TRB_ISP: u32 = 1 << 2;
const TRB_CHAIN: u32 = 1 << 4;
const TRB_IOC: u32 = 1 << 5;
const TRB_IDT: u32 = 1 << 6;
const TRB_DIR_IN: u32 = 1 << 16;

// Completion codes
const CC_SUCCESS: u8 = 1;
const CC_STALL: u8 = 6;
const CC_SHORT: u8 = 13;
const CC_STOPPED: u8 = 26;
const CC_STOPPED_SHORT: u8 = 28;

/// TRBs per ring segment (one page; the last is the link back).
const RING_TRBS: usize = 256;
const CONTROL_BUF: usize = 8192;
const BULK_BUF: usize = 65536;
const POLL_BUF: usize = 4096;

/// A ring of TRBs the driver produces (command ring, transfer rings).
struct Ring {
    mem: Dma,
    enq: usize,
    cycle: bool,
}

impl Ring {
    fn new() -> Option<Ring> {
        let mut ring = Ring { mem: Dma::new(RING_TRBS * 16)?, enq: 0, cycle: true };
        ring.clear();
        Some(ring)
    }

    /// Empty again, from the start.
    fn clear(&mut self) {
        self.mem.as_mut_slice().fill(0);
        // The last TRB links back to the start and flips the cycle bit.
        let at = (RING_TRBS - 1) * 16;
        self.mem.write::<u64>(at, self.mem.phys());
        self.mem.write::<u32>(at + 12, TRB_LINK << 10 | TRB_TOGGLE);
        self.enq = 0;
        self.cycle = true;
    }

    /// Writes a TRB (the cycle bit is added here) and returns its address.
    fn push(&mut self, trb: [u32; 4]) -> u64 {
        let at = self.enq * 16;
        let phys = self.mem.phys() + at as u64;
        self.mem.write::<u32>(at, trb[0]);
        self.mem.write::<u32>(at + 4, trb[1]);
        self.mem.write::<u32>(at + 8, trb[2]);
        fence(Ordering::SeqCst);
        self.mem.write::<u32>(at + 12, (trb[3] & !TRB_CYCLE) | self.cycle as u32);
        self.enq += 1;
        if self.enq == RING_TRBS - 1 {
            // Hand the link TRB over too; keep a chain going across it.
            let link = (RING_TRBS - 1) * 16;
            let chain = trb[3] & TRB_CHAIN;
            fence(Ordering::SeqCst);
            self.mem.write::<u32>(link + 12, TRB_LINK << 10 | TRB_TOGGLE | chain | self.cycle as u32);
            self.cycle = !self.cycle;
            self.enq = 0;
        }
        phys
    }

    /// The dequeue pointer for Set TR Dequeue: the next free TRB.
    fn next(&self) -> u64 {
        (self.mem.phys() + (self.enq * 16) as u64) | self.cycle as u64
    }
}

/// A completion the event thread handed to an endpoint: (TRB, code,
/// bytes not transferred).
type Completion = (u64, u8, u32);

/// An interrupt IN endpoint being polled for a driver.
#[derive(Clone, Copy)]
struct Poll {
    report: UsbReport,
    ctx: *mut c_void,
    trb: u64,
}

// SAFETY: the context belongs to the driver that started the poll.
unsafe impl Send for Poll {}

struct Endpoint {
    /// Device context index (1 = endpoint 0, then 2 x number + IN).
    dci: u8,
    kind: u8,
    max_packet: u16,
    ring: IrqLock<Ring>,
    buf: Dma,
    /// One transfer at a time.
    busy: Mutex<()>,
    done: Event,
    completions: IrqLock<VecDeque<Completion>>,
    poll: IrqLock<Option<Poll>>,
    /// The event thread is inside the poll callback.
    in_callback: AtomicBool,
}

impl Endpoint {
    fn new(dci: u8, kind: u8, max_packet: u16, buf: usize) -> Option<Arc<Endpoint>> {
        Some(Arc::new(Endpoint {
            dci,
            kind,
            max_packet,
            ring: IrqLock::new(Ring::new()?),
            buf: Dma::new(buf)?,
            busy: Mutex::new(()),
            done: Event::new(),
            completions: IrqLock::new(VecDeque::new()),
            poll: IrqLock::new(None),
            in_callback: AtomicBool::new(false),
        }))
    }
}

struct Slot {
    id: u8,
    speed: u8,
    output: Dma,
    input: Mutex<Dma>,
    eps: IrqLock<[Option<Arc<Endpoint>>; 32]>,
}

struct Xhci {
    regs: Mmio,
    op: usize,
    rt: usize,
    db: usize,
    /// Size of one context (32 or 64 bytes).
    csz: usize,
    ports: u8,
    /// USB major revision of every port (2 or 3), by port - 1.
    port_usb3: Vec<bool>,
    dcbaa: Dma,
    _scratch: Vec<Dma>,
    cmd_ring: IrqLock<Ring>,
    cmd_lock: Mutex<()>,
    cmd_done: Event,
    cmd_result: IrqLock<Option<(u64, u8, u8)>>,
    events: IrqLock<EventRing>,
    slots: IrqLock<Vec<Option<Arc<Slot>>>>,
    wake: Event,
    has_irq: AtomicBool,
    bus: IrqLock<Option<&'static Bus<Xhci>>>,
    max_slots: usize,
    /// The event ring segment table (one segment).
    erst: Dma,
    /// Kernel id of the controller (for its PCI configuration).
    pci: u32,
    /// An Intel controller whose ports start out wired to EHCI.
    switchable: bool,
}

struct EventRing {
    mem: Dma,
    deq: usize,
    cycle: bool,
}

fn speed_from_port(v: u32) -> u8 {
    match (v >> 10) & 0xF {
        1 => SPEED_FULL,
        2 => SPEED_LOW,
        3 => SPEED_HIGH,
        0 => 0,
        _ => SPEED_SUPER,
    }
}

/// The speed id an xHCI slot context uses.
fn slot_speed(speed: u8) -> u32 {
    match speed {
        SPEED_FULL => 1,
        SPEED_LOW => 2,
        SPEED_HIGH => 3,
        _ => 4,
    }
}

fn status_of(code: u8) -> Status {
    match code {
        CC_SUCCESS | CC_SHORT => OK,
        CC_STALL => E_STALL,
        _ => E_IO,
    }
}

impl Xhci {
    fn op_read(&self, reg: usize) -> u32 {
        self.regs.read32(self.op + reg)
    }

    fn op_write(&self, reg: usize, v: u32) {
        self.regs.write32(self.op + reg, v)
    }

    fn portsc(&self, port: u8) -> u32 {
        self.op_read(PORTSC + (port as usize - 1) * 0x10)
    }

    fn set_portsc(&self, port: u8, v: u32) {
        self.op_write(PORTSC + (port as usize - 1) * 0x10, v)
    }

    fn doorbell(&self, slot: u8, target: u8) {
        fence(Ordering::SeqCst);
        self.regs.write32(self.db + slot as usize * 4, target as u32);
    }

    fn slot(&self, id: Handle) -> Option<Arc<Slot>> {
        self.slots.lock().get(id as usize).cloned().flatten()
    }

    fn endpoint(&self, slot: &Slot, dci: u8) -> Option<Arc<Endpoint>> {
        slot.eps.lock().get(dci as usize).cloned().flatten()
    }

    // ------------------------------------------------------------------
    // Commands

    /// Runs a command; returns (completion code, slot id).
    fn command(&self, trb: [u32; 4]) -> Result<(u8, u8), Status> {
        let _one = self.cmd_lock.lock();
        *self.cmd_result.lock() = None;
        let phys = self.cmd_ring.lock().push(trb);
        self.doorbell(0, 0);
        let start = arcdrv::time_ms();
        loop {
            if let Some((p, code, slot)) = *self.cmd_result.lock() {
                if p == phys {
                    return Ok((code, slot));
                }
            }
            let waited = arcdrv::time_ms() - start;
            if waited >= 5000 {
                arcdrv::warn!("command {} timed out", trb[3] >> 10 & 0x3F);
                return Err(E_TIMEOUT);
            }
            self.cmd_done.wait(if self.has_irq.load(Ordering::Relaxed) { 50 } else { 2 });
        }
    }

    fn command_ok(&self, trb: [u32; 4]) -> Result<u8, Status> {
        match self.command(trb)? {
            (CC_SUCCESS, slot) => Ok(slot),
            (code, _) => {
                arcdrv::debug!("command {} failed with code {code}", trb[3] >> 10 & 0x3F);
                Err(E_IO)
            }
        }
    }

    // ------------------------------------------------------------------
    // Events

    fn process_events(&self) {
        let mut handled = false;
        loop {
            let trb = {
                let mut ev = self.events.lock();
                let at = ev.deq * 16;
                let control = ev.mem.read::<u32>(at + 12);
                if (control & 1 != 0) != ev.cycle {
                    break;
                }
                let trb = [ev.mem.read::<u32>(at), ev.mem.read::<u32>(at + 4), ev.mem.read::<u32>(at + 8), control];
                ev.deq += 1;
                if ev.deq == RING_TRBS {
                    ev.deq = 0;
                    ev.cycle = !ev.cycle;
                }
                trb
            };
            handled = true;
            self.event(trb);
        }
        if handled {
            let ev = self.events.lock();
            let deq = ev.mem.phys() + (ev.deq * 16) as u64;
            self.regs.write64(self.rt + 0x20 + 0x18, deq | 8);
        }
    }

    fn event(&self, trb: [u32; 4]) {
        let ptr = trb[0] as u64 | (trb[1] as u64) << 32;
        let code = (trb[2] >> 24) as u8;
        let slot = (trb[3] >> 24) as u8;
        match trb[3] >> 10 & 0x3F {
            TRB_COMMAND_DONE => {
                *self.cmd_result.lock() = Some((ptr, code, slot));
                self.cmd_done.signal();
            }
            TRB_PORT_CHANGE => {
                if let Some(bus) = *self.bus.lock() {
                    arcusb::port_changed(bus);
                }
            }
            TRB_TRANSFER_EVENT => {
                let dci = (trb[3] >> 16 & 0x1F) as u8;
                let residual = trb[2] & 0xFF_FFFF;
                let Some(s) = self.slot(slot as Handle) else { return };
                let Some(ep) = self.endpoint(&s, dci) else { return };
                let poll = {
                    let p = ep.poll.lock();
                    let mine = p.filter(|p| p.trb == ptr);
                    if mine.is_some() {
                        ep.in_callback.store(true, Ordering::Release);
                    }
                    mine
                };
                match poll {
                    Some(p) => {
                        self.poll_done(&s, &ep, p, code, residual);
                        ep.in_callback.store(false, Ordering::Release);
                    }
                    _ => {
                        ep.completions.lock().push_back((ptr, code, residual));
                        ep.done.signal();
                    }
                }
            }
            _ => {}
        }
    }

    /// A report arrived on a polled interrupt endpoint: hand it over and
    /// ask for the next one.
    fn poll_done(&self, slot: &Slot, ep: &Endpoint, p: Poll, code: u8, residual: u32) {
        if code != CC_SUCCESS && code != CC_SHORT {
            if code != CC_STOPPED && code != CC_STOPPED_SHORT {
                arcdrv::debug!("slot {} endpoint {}: polling stopped (code {code})", slot.id, ep.dci);
            }
            *ep.poll.lock() = None;
            return;
        }
        let len = (ep.max_packet as usize).min(POLL_BUF).saturating_sub(residual as usize);
        let mut data = [0u8; 64];
        let n = len.min(data.len());
        data[..n].copy_from_slice(&ep.buf.as_slice()[..n]);
        // SAFETY: the driver's callback with its own context.
        unsafe { (p.report)(p.ctx, data.as_ptr(), n as u32) };
        let mut poll = ep.poll.lock();
        if let Some(p) = poll.as_mut() {
            p.trb = self.queue_poll(slot, ep);
        }
    }

    fn queue_poll(&self, slot: &Slot, ep: &Endpoint) -> u64 {
        let len = (ep.max_packet as usize).min(POLL_BUF) as u32;
        let b = ep.buf.phys();
        let trb = ep.ring.lock().push([b as u32, (b >> 32) as u32, len, TRB_NORMAL << 10 | TRB_IOC | TRB_ISP]);
        self.doorbell(slot.id, ep.dci);
        trb
    }

    // ------------------------------------------------------------------
    // Transfers

    /// Waits for the completion of the TD made of `trbs`. Returns (code,
    /// the TRB it came from, residual) of the final event.
    fn wait_td(&self, slot: &Slot, ep: &Endpoint, trbs: &[u64], timeout_ms: u32, until_last: bool) -> Result<(u8, usize, u32), Status> {
        let start = arcdrv::time_ms();
        loop {
            while let Some((p, code, residual)) = ep.completions.lock().pop_front() {
                let Some(i) = trbs.iter().position(|&t| t == p) else { continue }; // stale
                let error = code != CC_SUCCESS && code != CC_SHORT;
                // Control transfers finish with the status stage, unless a
                // stage before it failed.
                if error || !until_last || i == trbs.len() - 1 {
                    return Ok((code, i, residual));
                }
            }
            if arcdrv::time_ms() - start >= timeout_ms as u64 {
                self.cancel(slot, ep);
                return Err(E_TIMEOUT);
            }
            ep.done.wait(if self.has_irq.load(Ordering::Relaxed) { 20 } else { 1 });
        }
    }

    /// Stops an endpoint and skips whatever is queued on it.
    fn cancel(&self, slot: &Slot, ep: &Endpoint) {
        let s = slot.id as u32;
        let _ = self.command([0, 0, 0, TRB_STOP_EP << 10 | (ep.dci as u32) << 16 | s << 24]);
        self.set_dequeue(slot, ep);
        ep.completions.lock().clear();
    }

    fn set_dequeue(&self, slot: &Slot, ep: &Endpoint) {
        let next = ep.ring.lock().next();
        let _ = self.command([next as u32, (next >> 32) as u32, 0, TRB_SET_DEQUEUE << 10 | (ep.dci as u32) << 16 | (slot.id as u32) << 24]);
    }

    /// After a stall or error the endpoint is halted: reset it and move
    /// past the failed transfer.
    fn recover(&self, slot: &Slot, ep: &Endpoint) {
        let _ = self.command([0, 0, 0, TRB_RESET_EP << 10 | (ep.dci as u32) << 16 | (slot.id as u32) << 24]);
        self.set_dequeue(slot, ep);
        ep.completions.lock().clear();
    }

    fn ep_state(&self, slot: &Slot, dci: u8) -> u32 {
        slot.output.read::<u32>(dci as usize * self.csz) & 7
    }

    fn do_control(&self, slot: &Slot, setup: Setup, data: &mut [u8], timeout_ms: u32) -> Result<usize, Status> {
        let ep = self.endpoint(slot, 1).ok_or(E_GONE)?;
        let len = (setup.length as usize).min(data.len());
        if len > CONTROL_BUF {
            return Err(E_INVAL);
        }
        let _one = ep.busy.lock();
        ep.completions.lock().clear();
        let input = setup.is_in();
        if !input && len > 0 {
            ep.buf.as_mut_slice()[..len].copy_from_slice(&data[..len]);
        }
        let b = setup.to_bytes();
        let trt = if len == 0 {
            0
        } else if input {
            3 << 16
        } else {
            2 << 16
        };
        let mut trbs: Vec<u64> = Vec::with_capacity(3);
        {
            let mut ring = ep.ring.lock();
            trbs.push(ring.push([
                u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                u32::from_le_bytes([b[4], b[5], b[6], b[7]]),
                8,
                TRB_SETUP << 10 | TRB_IDT | trt,
            ]));
            if len > 0 {
                let p = ep.buf.phys();
                trbs.push(ring.push([
                    p as u32,
                    (p >> 32) as u32,
                    len as u32,
                    TRB_DATA << 10 | TRB_ISP | if input { TRB_DIR_IN } else { 0 },
                ]));
            }
            let status_in = len == 0 || !input;
            trbs.push(ring.push([0, 0, 0, TRB_STATUS << 10 | TRB_IOC | if status_in { TRB_DIR_IN } else { 0 }]));
        }
        self.doorbell(slot.id, 1);
        let mut got = len;
        let start = arcdrv::time_ms();
        let mut remaining = timeout_ms;
        loop {
            let (code, i, residual) = self.wait_td(slot, &ep, &trbs, remaining, false)?;
            if code != CC_SUCCESS && code != CC_SHORT {
                self.recover(slot, &ep);
                return Err(status_of(code));
            }
            if len > 0 && i == 1 {
                got = len - (residual as usize).min(len);
            }
            if i == trbs.len() - 1 {
                break;
            }
            remaining = timeout_ms.saturating_sub((arcdrv::time_ms() - start) as u32).max(1);
        }
        if input && got > 0 {
            data[..got].copy_from_slice(&ep.buf.as_slice()[..got]);
        }
        Ok(got)
    }

    fn do_transfer(&self, slot: &Slot, endpoint: u8, data: &mut [u8], timeout_ms: u32) -> Result<usize, Status> {
        let dci = (endpoint & 0xF) * 2 + (endpoint >> 7);
        let ep = self.endpoint(slot, dci).ok_or(E_INVAL)?;
        if ep.poll.lock().is_some() {
            return Err(E_BUSY);
        }
        let input = endpoint & 0x80 != 0;
        let _one = ep.busy.lock();
        let chunk = ep.buf.len();
        let mut done = 0;
        loop {
            let n = (data.len() - done).min(chunk);
            if !input {
                ep.buf.as_mut_slice()[..n].copy_from_slice(&data[done..done + n]);
            }
            ep.completions.lock().clear();
            // One TRB per piece that stays inside a 64 KiB window.
            let mut trbs: Vec<u64> = Vec::new();
            let mut lens: Vec<usize> = Vec::new();
            {
                let mut ring = ep.ring.lock();
                let mut off = 0;
                loop {
                    let p = ep.buf.phys() + off as u64;
                    let room = 0x10000 - (p as usize & 0xFFFF);
                    let len = (n - off).min(room);
                    let last = off + len >= n;
                    let flags = TRB_NORMAL << 10 | TRB_ISP | if last { TRB_IOC } else { TRB_CHAIN };
                    trbs.push(ring.push([p as u32, (p >> 32) as u32, len as u32, flags]));
                    lens.push(len);
                    off += len;
                    if last {
                        break;
                    }
                }
            }
            self.doorbell(slot.id, dci);
            let (code, i, residual) = self.wait_td(slot, &ep, &trbs, timeout_ms, false)?;
            if code != CC_SUCCESS && code != CC_SHORT {
                self.recover(slot, &ep);
                return Err(status_of(code));
            }
            let got = lens[..i].iter().sum::<usize>() + lens[i] - (residual as usize).min(lens[i]);
            if input {
                data[done..done + got].copy_from_slice(&ep.buf.as_slice()[..got]);
            }
            done += got;
            if got < n || done >= data.len() {
                return Ok(done);
            }
        }
    }

    // ------------------------------------------------------------------
    // Contexts

    fn ctx_offset(&self, index: usize) -> usize {
        index * self.csz
    }

    /// Endpoint context `dci` inside an input context (after the control
    /// and slot contexts).
    fn input_ep(&self, dci: u8) -> usize {
        self.ctx_offset(1 + dci as usize)
    }

    // One argument per field of the endpoint context.
    #[allow(clippy::too_many_arguments)]
    fn write_ep_ctx(&self, input: &Dma, dci: u8, kind: u32, mps: u16, interval: u8, burst: u8, ring: u64, avg: u16, esit: u32) {
        let at = self.input_ep(dci);
        let cerr = if kind == 1 || kind == 5 { 0 } else { 3 };
        input.write::<u32>(at, (interval as u32) << 16 | (esit >> 16) << 24);
        input.write::<u32>(at + 4, cerr << 1 | kind << 3 | (burst as u32) << 8 | (mps as u32) << 16);
        input.write::<u64>(at + 8, ring | 1);
        input.write::<u32>(at + 16, avg as u32 | (esit & 0xFFFF) << 16);
    }

    fn clear_input(&self, input: &Dma) {
        input.as_mut_slice().fill(0);
    }
}

// ----------------------------------------------------------------------------
// The interface the USB core uses

impl Host for Xhci {
    fn ports(&self) -> u8 {
        self.ports
    }

    fn port_status(&self, port: u8) -> PortStatus {
        let v = self.portsc(port);
        PortStatus { connected: v & PORT_CCS != 0, enabled: v & PORT_PED != 0, speed: speed_from_port(v), changed: v & PORT_CHANGES != 0 }
    }

    fn port_clear_changes(&self, port: u8) {
        let v = self.portsc(port);
        self.set_portsc(port, (v & PORT_PRESERVE) | (v & PORT_CHANGES));
    }

    fn port_reset(&self, port: u8) -> Result<u8, Status> {
        let v = self.portsc(port);
        let usb3 = self.port_usb3.get(port as usize - 1).copied().unwrap_or(false);
        // USB 3 ports train the link and enable themselves.
        if usb3 && v & PORT_PED != 0 {
            return Ok(speed_from_port(v));
        }
        let reset = if usb3 { PORT_WPR } else { PORT_PR };
        self.set_portsc(port, (v & PORT_PRESERVE) | reset);
        let ok = arcdrv::wait_until(500, || self.portsc(port) & PORT_PRC != 0);
        let v = self.portsc(port);
        self.set_portsc(port, (v & PORT_PRESERVE) | (v & PORT_CHANGES));
        if !ok || v & PORT_PED == 0 {
            return Err(E_IO);
        }
        arcdrv::sleep_ms(10);
        Ok(speed_from_port(v))
    }

    fn assigns_addresses(&self) -> bool {
        true
    }

    fn add_device(&self, at: &Attach, mps0: u16) -> Result<Handle, Status> {
        let id = self.command_ok([0, 0, 0, TRB_ENABLE_SLOT << 10])?;
        let disable = |_: Status| {
            let _ = self.command([0, 0, 0, TRB_DISABLE_SLOT << 10 | (id as u32) << 24]);
        };
        let (Some(output), Some(input)) = (Dma::new(4096), Dma::new(4096)) else {
            disable(E_NOMEM);
            return Err(E_NOMEM);
        };
        let Some(ep0) = Endpoint::new(1, EP_CONTROL, mps0, CONTROL_BUF) else {
            disable(E_NOMEM);
            return Err(E_NOMEM);
        };
        // Input context: add the slot and endpoint 0.
        input.write::<u32>(4, 0b11);
        let slot_at = self.ctx_offset(1);
        input.write::<u32>(slot_at, at.route & 0xF_FFFF | slot_speed(at.speed) << 20 | 1 << 27);
        input.write::<u32>(slot_at + 4, (at.root_port as u32) << 16);
        if let Some((hub, port, multi)) = at.tt {
            input.write::<u32>(slot_at + 8, hub & 0xFF | (port as u32) << 8);
            if multi {
                input.write::<u32>(slot_at, input.read::<u32>(slot_at) | 1 << 25);
            }
        }
        let ring = ep0.ring.lock().mem.phys();
        self.write_ep_ctx(&input, 1, 4, mps0, 0, 0, ring, 8, 0);
        self.dcbaa.write::<u64>(id as usize * 8, output.phys());
        let slot = Arc::new(Slot { id, speed: at.speed, output, input: Mutex::new(input), eps: IrqLock::new([const { None }; 32]) });
        slot.eps.lock()[1] = Some(ep0);
        self.slots.lock()[id as usize] = Some(slot.clone());
        let phys = slot.input.lock().phys();
        if let Err(e) = self.command_ok([phys as u32, (phys >> 32) as u32, 0, TRB_ADDRESS_DEVICE << 10 | (id as u32) << 24]) {
            self.remove_device(id as Handle);
            return Err(e);
        }
        Ok(id as Handle)
    }

    fn address(&self, dev: Handle) -> u8 {
        self.slot(dev).map(|s| s.output.read::<u32>(12) as u8).unwrap_or(0)
    }

    fn set_mps0(&self, dev: Handle, mps0: u16) -> Result<(), Status> {
        let slot = self.slot(dev).ok_or(E_GONE)?;
        let ep0 = self.endpoint(&slot, 1).ok_or(E_GONE)?;
        if ep0.max_packet == mps0 {
            return Ok(());
        }
        let input = slot.input.lock();
        self.clear_input(&input);
        input.write::<u32>(4, 0b10);
        let ring = ep0.ring.lock().mem.phys();
        self.write_ep_ctx(&input, 1, 4, mps0, 0, 0, ring, 8, 0);
        // The dequeue pointer is not evaluated; only the packet size.
        let phys = input.phys();
        self.command_ok([phys as u32, (phys >> 32) as u32, 0, TRB_EVALUATE_CTX << 10 | (slot.id as u32) << 24])?;
        Ok(())
    }

    fn configure(&self, dev: Handle, config: &[u8], hub: Option<HubInfo>) -> Result<(), Status> {
        let slot = self.slot(dev).ok_or(E_GONE)?;
        let input = slot.input.lock();
        self.clear_input(&input);
        let slot_at = self.ctx_offset(1);
        // Start from the current slot context.
        for i in 0..4 {
            input.write::<u32>(slot_at + i * 4, slot.output.read::<u32>(i * 4));
        }
        input.write::<u32>(slot_at + 12, 0);
        let mut add = 1u32;
        let mut max_dci = 1u8;
        let mut new_eps: Vec<Arc<Endpoint>> = Vec::new();
        let mut last_ep: Option<(u8, EndpointDescriptor)> = None;
        let mut iface_ok = true;
        // Endpoints of alternate setting 0 of every interface, with the
        // SuperSpeed companion descriptor that may follow each.
        let mut pending: Vec<(EndpointDescriptor, u8)> = Vec::new();
        for d in descriptors(config) {
            match d {
                Descriptor::Interface(i) => {
                    if let Some((_, e)) = last_ep.take() {
                        pending.push((e, 0));
                    }
                    iface_ok = i.alternate == 0;
                }
                Descriptor::Endpoint(e) if iface_ok => {
                    if let Some((_, prev)) = last_ep.take() {
                        pending.push((prev, 0));
                    }
                    last_ep = Some((0, e));
                }
                Descriptor::Other(0x30, c) if iface_ok => {
                    // SuperSpeed endpoint companion: bMaxBurst.
                    if let Some((_, e)) = last_ep.take() {
                        pending.push((e, c.get(2).copied().unwrap_or(0)));
                    }
                }
                _ => {}
            }
        }
        if let Some((_, e)) = last_ep.take() {
            pending.push((e, 0));
        }
        for (e, burst) in pending {
            let kind = e.kind();
            if kind == EP_ISOCH || kind == EP_CONTROL {
                continue; // isochronous streams are not supported
            }
            let dci = e.number() * 2 + e.is_in() as u8;
            if !(2..=31).contains(&dci) || add & (1 << dci) != 0 {
                continue;
            }
            let xkind = match (kind, e.is_in()) {
                (EP_BULK, false) => 2,
                (EP_INTERRUPT, false) => 3,
                (EP_BULK, true) => 6,
                _ => 7,
            };
            let mps = e.packet_size().max(1);
            let bufsize = if kind == EP_BULK { BULK_BUF } else { POLL_BUF };
            let ep = Endpoint::new(dci, kind, mps, bufsize).ok_or(E_NOMEM)?;
            let ring = ep.ring.lock().mem.phys();
            let mult = if slot.speed == SPEED_HIGH { ((e.max_packet >> 11) & 3) as u32 } else { 0 };
            let (interval, avg, esit) = if kind == EP_INTERRUPT {
                (arcusb::interval_exponent(&e, slot.speed), mps, mps as u32 * (burst as u32 + 1) * (mult + 1))
            } else {
                (0, 3072, 0)
            };
            self.write_ep_ctx(
                &input,
                dci,
                xkind,
                mps,
                interval,
                if slot.speed == SPEED_HIGH { mult as u8 } else { burst },
                ring,
                avg,
                esit,
            );
            add |= 1 << dci;
            max_dci = max_dci.max(dci);
            new_eps.push(ep);
        }
        let mut dw0 = input.read::<u32>(slot_at) & !(0x1F << 27);
        dw0 |= (max_dci as u32) << 27;
        if let Some(h) = hub {
            dw0 |= 1 << 26;
            if h.multi_tt {
                dw0 |= 1 << 25;
            }
            input.write::<u32>(slot_at + 4, input.read::<u32>(slot_at + 4) & 0x00FF_FFFF | (h.ports as u32) << 24);
            input.write::<u32>(slot_at + 8, input.read::<u32>(slot_at + 8) & !(3 << 16) | (h.think_time as u32 & 3) << 16);
        }
        input.write::<u32>(slot_at, dw0);
        input.write::<u32>(0, 0);
        input.write::<u32>(4, add);
        let phys = input.phys();
        self.command_ok([phys as u32, (phys >> 32) as u32, 0, TRB_CONFIGURE_EP << 10 | (slot.id as u32) << 24])?;
        let mut eps = slot.eps.lock();
        for ep in new_eps {
            let dci = ep.dci as usize;
            eps[dci] = Some(ep);
        }
        Ok(())
    }

    fn control(&self, dev: Handle, setup: Setup, data: &mut [u8], timeout_ms: u32) -> Result<usize, Status> {
        let slot = self.slot(dev).ok_or(E_GONE)?;
        self.do_control(&slot, setup, data, timeout_ms)
    }

    fn transfer(&self, dev: Handle, endpoint: u8, data: &mut [u8], timeout_ms: u32) -> Result<usize, Status> {
        let slot = self.slot(dev).ok_or(E_GONE)?;
        self.do_transfer(&slot, endpoint, data, timeout_ms)
    }

    fn poll_start(&self, dev: Handle, endpoint: u8, report: UsbReport, ctx: *mut c_void) -> Status {
        let Some(slot) = self.slot(dev) else { return E_GONE };
        let dci = (endpoint & 0xF) * 2 + (endpoint >> 7);
        let Some(ep) = self.endpoint(&slot, dci) else { return E_INVAL };
        if endpoint & 0x80 == 0 || ep.kind != EP_INTERRUPT {
            return E_INVAL;
        }
        let mut poll = ep.poll.lock();
        if poll.is_some() {
            return E_BUSY;
        }
        *poll = Some(Poll { report, ctx, trb: 0 });
        let trb = self.queue_poll(&slot, &ep);
        if let Some(p) = poll.as_mut() {
            p.trb = trb;
        }
        OK
    }

    fn poll_stop(&self, dev: Handle, endpoint: u8) {
        let Some(slot) = self.slot(dev) else { return };
        let dci = (endpoint & 0xF) * 2 + (endpoint >> 7);
        let Some(ep) = self.endpoint(&slot, dci) else { return };
        let stopped = ep.poll.lock().take().is_some();
        // The driver may go away right after: never return while its
        // callback runs.
        while ep.in_callback.load(Ordering::Acquire) {
            arcdrv::sleep_ms(1);
        }
        if stopped {
            self.cancel(&slot, &ep);
        }
    }

    fn reset_endpoint(&self, dev: Handle, endpoint: u8) -> Status {
        let Some(slot) = self.slot(dev) else { return E_GONE };
        let dci = (endpoint & 0xF) * 2 + (endpoint >> 7);
        let Some(ep) = self.endpoint(&slot, dci) else { return E_INVAL };
        let _one = ep.busy.lock();
        match self.ep_state(&slot, dci) {
            2 => self.recover(&slot, &ep), // halted
            3 | 4 => self.set_dequeue(&slot, &ep),
            _ => {}
        }
        OK
    }

    fn remove_device(&self, dev: Handle) {
        let Some(slot) = self.slots.lock().get_mut(dev as usize).and_then(|s| s.take()) else { return };
        let _ = self.command([0, 0, 0, TRB_DISABLE_SLOT << 10 | (slot.id as u32) << 24]);
        self.dcbaa.write::<u64>(slot.id as usize * 8, 0);
        // The slot's memory goes when the last transfer using it returns.
    }

    const SLEEPS: bool = true;

    fn stop(&self) {
        self.op_write(USBCMD, self.op_read(USBCMD) & !(CMD_RUN | CMD_INTE));
        arcdrv::wait_until(100, || self.op_read(USBSTS) & STS_HALTED != 0);
    }

    fn restart(&self) -> Status {
        take_ownership(&self.regs, ((self.regs.read32(HCCPARAMS1) >> 16) as usize) * 4);
        if self.switchable {
            route_intel_ports(Pci(self.pci));
        }
        let st = reset(&self.regs, self.op);
        if st != OK {
            return st;
        }
        self.start()
    }
}

impl Xhci {
    /// Empty command and event rings given to the controller, which then
    /// runs; every port powered.
    fn start(&self) -> Status {
        let (regs, op, ir) = (&self.regs, self.op, self.rt + 0x20);
        let cmd = {
            let mut ring = self.cmd_ring.lock();
            ring.clear();
            ring.mem.phys()
        };
        let events = {
            let mut ev = self.events.lock();
            ev.mem.as_mut_slice().fill(0);
            ev.deq = 0;
            ev.cycle = true;
            ev.mem.phys()
        };
        *self.cmd_result.lock() = None;
        regs.write32(op + CONFIG, self.max_slots as u32);
        regs.write64(op + DCBAAP, self.dcbaa.phys());
        regs.write64(op + CRCR, cmd | 1);
        regs.write32(ir + 0x08, 1); // ERSTSZ
        regs.write64(ir + 0x18, events); // ERDP
        regs.write64(ir + 0x10, self.erst.phys()); // ERSTBA
        regs.write32(ir + 0x04, 160); // moderation: 40 µs
        regs.write32(ir, 0b11); // IMAN: clear pending, enable
        regs.write32(op + USBCMD, CMD_RUN | CMD_INTE);
        if !arcdrv::wait_until(1000, || regs.read32(op + USBSTS) & STS_HALTED == 0) {
            arcdrv::warn!("controller does not start");
            return E_IO;
        }
        // Power every port (a no-op where power is not switchable).
        for p in 1..=self.ports {
            let v = self.portsc(p);
            if v & PORT_PP == 0 {
                self.set_portsc(p, (v & PORT_PRESERVE) | PORT_PP);
            }
        }
        OK
    }
}

/// Stops and resets the controller.
fn reset(regs: &Mmio, op: usize) -> Status {
    arcdrv::wait_until(1000, || regs.read32(op + USBSTS) & STS_CNR == 0);
    regs.write32(op + USBCMD, regs.read32(op + USBCMD) & !CMD_RUN);
    if !arcdrv::wait_until(1000, || regs.read32(op + USBSTS) & STS_HALTED != 0) {
        arcdrv::warn!("controller does not halt");
        return E_IO;
    }
    regs.write32(op + USBCMD, CMD_RESET);
    arcdrv::sleep_ms(1);
    if !arcdrv::wait_until(1000, || regs.read32(op + USBCMD) & CMD_RESET == 0 && regs.read32(op + USBSTS) & STS_CNR == 0) {
        arcdrv::warn!("controller reset timed out");
        return E_TIMEOUT;
    }
    OK
}

// ----------------------------------------------------------------------------
// Bring-up

unsafe extern "C" fn irq(ctx: *mut c_void) {
    // SAFETY: the controller leaked in probe.
    let x = unsafe { &*(ctx as *const Xhci) };
    let sts = x.op_read(USBSTS);
    if sts & (STS_EINT | STS_PCD) == 0 {
        return; // not ours (shared line)
    }
    x.op_write(USBSTS, sts & (STS_EINT | STS_PCD));
    let iman = x.rt + 0x20;
    x.regs.write32(iman, x.regs.read32(iman) | 1);
    x.wake.signal();
}

unsafe extern "C" fn event_thread(ctx: *mut c_void) {
    // SAFETY: the controller leaked in probe.
    let x = unsafe { &*(ctx as *const Xhci) };
    loop {
        let timeout = if x.has_irq.load(Ordering::Relaxed) { 20 } else { 1 };
        x.wake.wait(timeout);
        x.process_events();
    }
}

/// Takes the controller away from the firmware's legacy USB emulation.
fn take_ownership(regs: &Mmio, xecp: usize) -> Vec<(u8, u8, u8)> {
    let mut protocols = Vec::new();
    let mut at = xecp;
    for _ in 0..64 {
        if at == 0 || at + 8 > 0x10000 {
            break;
        }
        let v = regs.read32(at);
        match v & 0xFF {
            1 => {
                if v & (1 << 16) != 0 {
                    regs.write32(at, v | (1 << 24));
                    if !arcdrv::wait_until(1000, || regs.read32(at) & (1 << 16) == 0) {
                        arcdrv::warn!("the firmware did not release the controller; taking it anyway");
                        regs.write32(at, regs.read32(at) & !(1 << 16));
                    }
                }
                // No more system management interrupts for USB events.
                let ctl = regs.read32(at + 4);
                regs.write32(at + 4, (ctl & 0xFFFF_1FEE) | 0xE000_0000);
            }
            2 => {
                // Supported protocol: (major revision, first port, count).
                let ports = regs.read32(at + 8);
                protocols.push(((v >> 24) as u8, ports as u8, (ports >> 8) as u8));
            }
            _ => {}
        }
        let next = ((v >> 8) & 0xFF) as usize;
        if next == 0 {
            break;
        }
        at += next * 4;
    }
    protocols
}

/// Intel chipsets of 2012-2014 start with every port wired to the EHCI
/// controllers (again after sleep).
fn switchable(dev: &DeviceInfo) -> bool {
    const SWITCHABLE: [u16; 5] = [0x1E31, 0x8C31, 0x9C31, 0x9CB1, 0x8CB1];
    dev.vendor == 0x8086 && SWITCHABLE.contains(&dev.device)
}

/// Hands the ports of a switchable controller to xHCI.
fn route_intel_ports(pci: Pci) {
    pci.write32(0xD8, pci.read32(0xDC)); // USB 3 ports: SuperSpeed on
    pci.write32(0xD0, pci.read32(0xD4)); // USB 2 ports: to xHCI
    arcdrv::debug!("USB ports switched over from EHCI");
}

fn probe(dev: &DeviceInfo) -> Status {
    if dev.class != 0x0C || dev.subclass != 0x03 || dev.prog_if != 0x30 {
        return E_NODEV;
    }
    let bar = dev.resources[0];
    if bar.kind != RES_MEMORY || bar.base == 0 {
        return E_IO;
    }
    Pci(dev.id).enable(false, true, true);
    let Some(regs) = Mmio::map(bar.base, (bar.size as usize).clamp(0x1000, 0x10000)) else { return E_NOMEM };

    let caplen = regs.read8(CAPLENGTH) as usize;
    let hcs1 = regs.read32(HCSPARAMS1);
    let hcs2 = regs.read32(HCSPARAMS2);
    let hcc1 = regs.read32(HCCPARAMS1);
    let max_slots = (hcs1 & 0xFF).min(255) as usize;
    let ports = (hcs1 >> 24) as u8;
    let csz = if hcc1 & (1 << 2) != 0 { 64 } else { 32 };
    let op = caplen;
    let rt = (regs.read32(RTSOFF) & !0x1F) as usize;
    let db = (regs.read32(DBOFF) & !0x3) as usize;
    let protocols = take_ownership(&regs, ((hcc1 >> 16) as usize) * 4);
    if switchable(dev) {
        route_intel_ports(Pci(dev.id));
    }
    let st = reset(&regs, op);
    if st != OK {
        return st;
    }

    // Device context array, scratchpad, command ring, event ring.
    let Some(dcbaa) = Dma::new((max_slots + 1) * 8) else { return E_NOMEM };
    let scratch_count = (((hcs2 >> 21) & 0x1F) << 5 | (hcs2 >> 27) & 0x1F) as usize;
    let mut scratch = Vec::new();
    if scratch_count > 0 {
        let Some(array) = Dma::new(scratch_count * 8) else { return E_NOMEM };
        for i in 0..scratch_count {
            let Some(page) = Dma::new(4096) else { return E_NOMEM };
            array.write::<u64>(i * 8, page.phys());
            scratch.push(page);
        }
        dcbaa.write::<u64>(0, array.phys());
        scratch.push(array);
    }
    let (Some(cmd_ring), Some(event_mem), Some(erst)) = (Ring::new(), Dma::new(RING_TRBS * 16), Dma::new(64)) else { return E_NOMEM };
    erst.write::<u64>(0, event_mem.phys());
    erst.write::<u32>(8, RING_TRBS as u32);

    let mut port_usb3 = alloc::vec![false; ports as usize];
    for &(major, first, count) in &protocols {
        for p in first..first.saturating_add(count) {
            if p >= 1 && p <= ports {
                port_usb3[p as usize - 1] = major >= 3;
            }
        }
    }

    let x: &'static Xhci = Box::leak(Box::new(Xhci {
        regs,
        op,
        rt,
        db,
        csz,
        ports,
        port_usb3,
        dcbaa,
        _scratch: scratch,
        cmd_ring: IrqLock::new(cmd_ring),
        cmd_lock: Mutex::new(()),
        cmd_done: Event::new(),
        cmd_result: IrqLock::new(None),
        events: IrqLock::new(EventRing { mem: event_mem, deq: 0, cycle: true }),
        slots: IrqLock::new(alloc::vec![None; max_slots + 1]),
        wake: Event::new(),
        has_irq: AtomicBool::new(false),
        bus: IrqLock::new(None),
        max_slots,
        erst,
        pci: dev.id,
        switchable: switchable(dev),
    }));
    let ctx = x as *const Xhci as *mut c_void;
    x.has_irq.store(arcdrv::irq_attach_any(dev, irq, ctx), Ordering::Relaxed);
    arcdrv::spawn("xhci-events", event_thread, ctx);
    let st = x.start();
    if st != OK {
        return st;
    }
    let usb3 = x.port_usb3.iter().filter(|&&u| u).count();
    arcdrv::info!(
        "{} ports ({} USB 3), {} slots, {}",
        ports,
        usb3,
        max_slots,
        if x.has_irq.load(Ordering::Relaxed) { "interrupts" } else { "polling" }
    );
    let bus = arcusb::start(x, dev.id, "xhci-ports");
    *x.bus.lock() = Some(bus);
    OK
}

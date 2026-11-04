//! UHCI (USB 1.1) host controller driver.
//!
//! The USB controllers of Intel and VIA chipsets up to about 2010: full- and
//! low-speed devices only (keyboards, mice, older sticks). On most of those
//! PCs they sit next to an EHCI controller, which hands them its slower
//! devices.
//!
//! The schedule is a single list that the controller walks every frame:
//! the interrupt queues first (so they are polled every millisecond), then
//! control, then bulk. Every endpoint has a queue head and its transfer
//! descriptors in one DMA block; transfers on an endpoint run one at a time
//! and finish by watching their descriptors.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use arcdrv::port::{inw, outb, outl, outw};
use arcdrv::usb::*;
use arcdrv::{
    DeviceInfo, Dma, Event, IrqLock, Mutex, Pci, Status, E_BUSY, E_GONE, E_INVAL, E_IO, E_NODEV, E_NOMEM, E_STALL, E_TIMEOUT, OK,
};
use arcusb::{Attach, Handle, Host, HubInfo, PortStatus, Table};
use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU8, Ordering};

arcdrv::driver! {
    name: "uhci",
    version: "1.1.0",
    probe: probe,
}

// Registers (I/O space)
const USBCMD: u16 = 0x00;
const USBSTS: u16 = 0x02;
const USBINTR: u16 = 0x04;
const FRNUM: u16 = 0x06;
const FRBASEADD: u16 = 0x08;
const SOFMOD: u16 = 0x0C;
const PORTSC: u16 = 0x10;

const CMD_RUN: u16 = 1;
const CMD_HCRESET: u16 = 1 << 1;
const CMD_GRESET: u16 = 1 << 2;
const CMD_CF: u16 = 1 << 6;
const CMD_MAXP: u16 = 1 << 7;
const STS_HALTED: u16 = 1 << 5;
/// Interrupt, error, resume, host system error, process error.
const STS_EVENTS: u16 = 0x1F;
const STS_FATAL: u16 = 0x18;
/// Interrupts: timeout/CRC, completion, short packet.
const INTR_ENABLE: u16 = 0b1101;

// PORTSC bits
const PORT_CCS: u16 = 1;
const PORT_CSC: u16 = 1 << 1;
const PORT_PED: u16 = 1 << 2;
const PORT_PEDC: u16 = 1 << 3;
const PORT_VALID: u16 = 1 << 7; // always reads 1
const PORT_LSDA: u16 = 1 << 8;
const PORT_PR: u16 = 1 << 9;
const PORT_SUSPEND: u16 = 1 << 12;
/// Write-1-to-clear change bits.
const PORT_CHANGES: u16 = PORT_CSC | PORT_PEDC;
/// Bits written back unchanged; the others are read-only or clear on 1.
const PORT_KEEP: u16 = PORT_PED | PORT_SUSPEND;

/// Legacy support register in PCI configuration space.
const USBLEGSUP: u16 = 0xC0;
/// Clears the legacy status bits and turns legacy emulation off.
const LEGSUP_OFF: u16 = 0x8F00;
/// Lets the controller raise PCI interrupts.
const LEGSUP_PIRQ: u16 = 0x2000;

// Link pointers
const LINK_T: u32 = 1;
const LINK_QH: u32 = 1 << 1;
const LINK_DEPTH: u32 = 1 << 2;

// Transfer descriptor status
const TD_ACTIVE: u32 = 1 << 23;
const TD_STALLED: u32 = 1 << 22;
/// Stalled, buffer error, babble, CRC/timeout, bit stuffing (not NAK).
const TD_ERRORS: u32 = 0x76 << 16;
const TD_IOC: u32 = 1 << 24;
const TD_LS: u32 = 1 << 26;
const TD_CERR: u32 = 3 << 27;
const TD_SPD: u32 = 1 << 29;

const PID_SETUP: u32 = 0x2D;
const PID_IN: u32 = 0x69;
const PID_OUT: u32 = 0xE1;

const CONTROL_BUF: usize = 4096;
const BULK_BUF: usize = 16384;
/// Control pipes are sized for the smallest packet (8 bytes), whatever
/// endpoint 0 turns out to use.
const CONTROL_TDS: usize = CONTROL_BUF / 8 + 2;

/// Size of the frame list; the anchor queue head follows it.
const FRAMES: usize = 1024;
const ANCHOR: usize = FRAMES * 4;

/// The token of a TD: packet id, address, endpoint, toggle, length.
fn token(pid: u32, address: u8, endpoint: u8, toggle: bool, len: usize) -> u32 {
    let max = if len == 0 { 0x7FF } else { (len as u32 - 1) & 0x7FF };
    pid | (address as u32) << 8 | ((endpoint & 0xF) as u32) << 15 | (toggle as u32) << 19 | max << 21
}

/// An interrupt IN endpoint being polled for a driver.
#[derive(Clone, Copy)]
struct Poll {
    report: UsbReport,
    ctx: *mut c_void,
}

// SAFETY: the context belongs to the driver that started the poll.
unsafe impl Send for Poll {}

/// One endpoint: its queue head (offset 0, with the setup packet of a
/// control transfer in its spare 8 bytes), its TDs (from offset 16) and a
/// data buffer, all in one DMA block.
struct Pipe {
    mem: Dma,
    kind: u8,
    /// Endpoint address with the direction bit (0 for control).
    endpoint: u8,
    mps: AtomicU16,
    tds: usize,
    buf_off: usize,
    buf_len: usize,
    toggle: AtomicBool,
    busy: Mutex<()>,
    poll: IrqLock<Option<Poll>>,
    in_callback: AtomicBool,
    gone: AtomicBool,
}

impl Pipe {
    fn new(kind: u8, endpoint: u8, mps: u16, tds: usize, buf_len: usize) -> Option<Arc<Pipe>> {
        let buf_off = 16 + tds * 16;
        let mem = Dma::new(buf_off + buf_len)?;
        mem.write::<u32>(0, LINK_T);
        mem.write::<u32>(4, LINK_T);
        Some(Arc::new(Pipe {
            mem,
            kind,
            endpoint,
            mps: AtomicU16::new(mps.max(1)),
            tds,
            buf_off,
            buf_len,
            toggle: AtomicBool::new(false),
            busy: Mutex::new(()),
            poll: IrqLock::new(None),
            in_callback: AtomicBool::new(false),
            gone: AtomicBool::new(false),
        }))
    }

    fn qh(&self) -> u32 {
        self.mem.phys() as u32
    }

    fn td_phys(&self, i: usize) -> u32 {
        (self.mem.phys() + 16 + i as u64 * 16) as u32
    }

    fn buf_phys(&self) -> u32 {
        (self.mem.phys() + self.buf_off as u64) as u32
    }

    fn buf(&self) -> &mut [u8] {
        &mut self.mem.as_mut_slice()[self.buf_off..self.buf_off + self.buf_len]
    }

    fn set_td(&self, i: usize, link: u32, status: u32, token: u32, buffer: u32) {
        let at = 16 + i * 16;
        self.mem.write::<u32>(at, link);
        self.mem.write::<u32>(at + 8, token);
        self.mem.write::<u32>(at + 12, buffer);
        core::sync::atomic::fence(Ordering::SeqCst);
        self.mem.write::<u32>(at + 4, status);
    }

    fn td_status(&self, i: usize) -> u32 {
        self.mem.read::<u32>(16 + i * 16 + 4)
    }

    fn td_token(&self, i: usize) -> u32 {
        self.mem.read::<u32>(16 + i * 16 + 8)
    }

    /// Points the queue at a TD (or LINK_T to stop it).
    fn start(&self, element: u32) {
        core::sync::atomic::fence(Ordering::SeqCst);
        self.mem.write::<u32>(4, element);
    }

    fn priority(&self) -> u8 {
        match self.kind {
            EP_INTERRUPT => 0,
            EP_CONTROL => 1,
            _ => 2,
        }
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

struct Uhci {
    io: u16,
    ports: u8,
    /// Frame list, then the anchor queue head every frame starts at.
    frames: Dma,
    /// Every queue in the schedule, in the order the controller walks them.
    schedule: Mutex<Vec<Arc<Pipe>>>,
    devices: Table<Arc<Dev>>,
    next_handle: AtomicU32,
    polls: IrqLock<Vec<Arc<Pipe>>>,
    wake: Event,
    has_irq: AtomicBool,
    /// Kernel id of the controller (for its PCI configuration).
    pci: u32,
    /// Stopped for sleep (a halted controller is not an error then).
    asleep: AtomicBool,
}

impl Uhci {
    fn port_reg(&self, port: u8) -> u16 {
        self.io + PORTSC + (port as u16 - 1) * 2
    }

    fn dev(&self, h: Handle) -> Result<Arc<Dev>, Status> {
        self.devices.get(h).ok_or(E_GONE)
    }

    // ------------------------------------------------------------------
    // Schedule

    /// Rewrites the links of the list back to front, so the controller
    /// always sees a valid list while it changes.
    fn relink(&self, list: &[Arc<Pipe>]) {
        let mut next = LINK_T;
        for p in list.iter().rev() {
            p.mem.write::<u32>(0, next);
            next = p.qh() | LINK_QH;
        }
        core::sync::atomic::fence(Ordering::SeqCst);
        self.frames.write::<u32>(ANCHOR, next);
    }

    fn schedule_add(&self, pipe: &Arc<Pipe>) {
        let mut list = self.schedule.lock();
        let at = list.iter().position(|p| p.priority() > pipe.priority()).unwrap_or(list.len());
        list.insert(at, pipe.clone());
        self.relink(&list);
    }

    fn schedule_remove(&self, pipes: &[Arc<Pipe>]) {
        {
            let mut list = self.schedule.lock();
            list.retain(|p| !pipes.iter().any(|q| Arc::ptr_eq(p, q)));
            self.relink(&list);
        }
        // The controller may still be inside one of them in this frame.
        arcdrv::sleep_ms(2);
    }

    // ------------------------------------------------------------------
    // Transfers

    /// Waits until TDs `from..to` are done. Returns the index of a TD that
    /// ended with a short packet (the queue stops there), if one did.
    fn wait_tds(&self, pipe: &Pipe, from: usize, to: usize, deadline: u64) -> Result<Option<usize>, Status> {
        let mut i = from;
        loop {
            while i < to {
                let st = pipe.td_status(i);
                if st & TD_ACTIVE != 0 {
                    break;
                }
                if st & TD_ERRORS != 0 {
                    return Err(if st & TD_STALLED != 0 { E_STALL } else { E_IO });
                }
                let tok = pipe.td_token(i);
                let actual = (st + 1) & 0x7FF;
                let max = ((tok >> 21) + 1) & 0x7FF;
                i += 1;
                if tok & 0xFF == PID_IN && actual < max {
                    return Ok(Some(i - 1));
                }
            }
            if i >= to {
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

    /// Stops the queue after a transfer that did not finish cleanly.
    fn halt(&self, pipe: &Pipe) {
        pipe.start(LINK_T);
        arcdrv::sleep_ms(2);
    }

    fn actual(pipe: &Pipe, i: usize) -> usize {
        ((pipe.td_status(i) + 1) & 0x7FF) as usize
    }

    fn do_control(&self, dev: &Dev, setup: Setup, data: &mut [u8], timeout_ms: u32) -> Result<usize, Status> {
        let pipe = dev.pipe(0).ok_or(E_GONE)?;
        let _one = pipe.busy.lock();
        let len = (setup.length as usize).min(data.len());
        let mps = pipe.mps.load(Ordering::Relaxed) as usize;
        let packets = len.div_ceil(mps);
        if len > pipe.buf_len || packets + 2 > pipe.tds {
            return Err(E_INVAL);
        }
        let input = setup.is_in();
        if !input {
            pipe.buf()[..len].copy_from_slice(&data[..len]);
        }
        let addr = dev.address.load(Ordering::Relaxed);
        let ls = if dev.low_speed { TD_LS } else { 0 };
        let base = TD_ACTIVE | TD_CERR | ls;
        // The setup packet lives in the queue head's spare bytes.
        let b = setup.to_bytes();
        pipe.mem.write::<u64>(8, u64::from_le_bytes(b));
        let status = packets + 1;
        let link = |i: usize| pipe.td_phys(i + 1) | LINK_DEPTH;
        pipe.set_td(0, link(0), base, token(PID_SETUP, addr, 0, false, 8), pipe.qh() + 8);
        let pid = if input { PID_IN } else { PID_OUT };
        for k in 0..packets {
            let n = (len - k * mps).min(mps);
            let spd = if input { TD_SPD } else { 0 };
            pipe.set_td(1 + k, link(1 + k), base | spd, token(pid, addr, 0, k % 2 == 0, n), pipe.buf_phys() + (k * mps) as u32);
        }
        let status_pid = if input && len > 0 { PID_OUT } else { PID_IN };
        pipe.set_td(status, LINK_T, base | TD_IOC, token(status_pid, addr, 0, true, 0), 0);
        pipe.start(pipe.td_phys(0));

        let deadline = arcdrv::time_ms() + timeout_ms as u64;
        let result = match self.wait_tds(&pipe, 0, status + 1, deadline) {
            // A short data stage: go on with the status stage.
            Ok(Some(i)) if i >= 1 && i < status => {
                pipe.start(pipe.td_phys(status));
                self.wait_tds(&pipe, status, status + 1, deadline).map(|_| i + 1)
            }
            Ok(_) => Ok(status),
            Err(e) => Err(e),
        };
        match result {
            Ok(end) => {
                pipe.start(LINK_T);
                let got: usize = (1..end.min(status)).map(|i| Self::actual(&pipe, i)).sum::<usize>().min(len);
                if input {
                    data[..got].copy_from_slice(&pipe.buf()[..got]);
                }
                Ok(got)
            }
            Err(e) => {
                self.halt(&pipe);
                Err(e)
            }
        }
    }

    fn do_transfer(&self, dev: &Dev, endpoint: u8, data: &mut [u8], timeout_ms: u32) -> Result<usize, Status> {
        let pipe = dev.pipe(endpoint).ok_or(E_INVAL)?;
        if pipe.poll.lock().is_some() {
            return Err(E_BUSY);
        }
        let _one = pipe.busy.lock();
        let input = endpoint & 0x80 != 0;
        let pid = if input { PID_IN } else { PID_OUT };
        let addr = dev.address.load(Ordering::Relaxed);
        let mps = pipe.mps.load(Ordering::Relaxed) as usize;
        let base = TD_ACTIVE | TD_CERR | if dev.low_speed { TD_LS } else { 0 } | if input { TD_SPD } else { 0 };
        let chunk = pipe.buf_len.min((pipe.tds) * mps);
        let mut done = 0;
        loop {
            let n = (data.len() - done).min(chunk);
            if !input {
                pipe.buf()[..n].copy_from_slice(&data[done..done + n]);
            }
            let packets = n.div_ceil(mps).max(1);
            let toggle = pipe.toggle.load(Ordering::Relaxed);
            for k in 0..packets {
                let len = (n - (k * mps).min(n)).min(mps);
                let last = k == packets - 1;
                let link = if last { LINK_T } else { pipe.td_phys(k + 1) | LINK_DEPTH };
                let ioc = if last { TD_IOC } else { 0 };
                pipe.set_td(
                    k,
                    link,
                    base | ioc,
                    token(pid, addr, endpoint, toggle ^ (k % 2 == 1), len),
                    pipe.buf_phys() + (k * mps) as u32,
                );
            }
            pipe.start(pipe.td_phys(0));
            let deadline = arcdrv::time_ms() + timeout_ms as u64;
            let (completed, result) = match self.wait_tds(&pipe, 0, packets, deadline) {
                Ok(Some(i)) => (i + 1, Ok(())),
                Ok(None) => (packets, Ok(())),
                Err(e) => ((0..packets).take_while(|&i| pipe.td_status(i) & (TD_ACTIVE | TD_ERRORS) == 0).count(), Err(e)),
            };
            pipe.toggle.store(toggle ^ (completed % 2 == 1), Ordering::Relaxed);
            if let Err(e) = result {
                self.halt(&pipe);
                return Err(e);
            }
            pipe.start(LINK_T);
            let got = (0..completed).map(|i| Self::actual(&pipe, i)).sum::<usize>().min(n);
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

    fn arm_poll(&self, pipe: &Pipe, addr: u8, low_speed: bool) {
        let mps = pipe.mps.load(Ordering::Relaxed) as usize;
        let ls = if low_speed { TD_LS } else { 0 };
        let toggle = pipe.toggle.load(Ordering::Relaxed);
        pipe.set_td(
            0,
            LINK_T,
            TD_ACTIVE | TD_CERR | TD_IOC | TD_SPD | ls,
            token(PID_IN, addr, pipe.endpoint, toggle, mps),
            pipe.buf_phys(),
        );
        pipe.start(pipe.td_phys(0));
    }

    fn service_polls(&self) {
        let pipes: Vec<Arc<Pipe>> = self.polls.lock().clone();
        for pipe in pipes {
            let st = pipe.td_status(0);
            if st & TD_ACTIVE != 0 {
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
            if st & TD_ERRORS != 0 {
                arcdrv::debug!("endpoint {:#x}: polling stopped (status {:#x})", pipe.endpoint, st >> 16 & 0xFF);
                *pipe.poll.lock() = None;
                pipe.in_callback.store(false, Ordering::Release);
                self.polls.lock().retain(|q| !Arc::ptr_eq(q, &pipe));
                continue;
            }
            let n = (((st + 1) & 0x7FF) as usize).min(pipe.buf_len).min(64);
            let mut data = [0u8; 64];
            data[..n].copy_from_slice(&pipe.buf()[..n]);
            pipe.toggle.fetch_xor(true, Ordering::Relaxed);
            // SAFETY: the driver's callback with its own context.
            unsafe { (p.report)(p.ctx, data.as_ptr(), n as u32) };
            {
                let guard = pipe.poll.lock();
                if guard.is_some() {
                    let tok = pipe.td_token(0);
                    self.arm_poll(&pipe, (tok >> 8 & 0x7F) as u8, st & TD_LS != 0);
                }
            }
            pipe.in_callback.store(false, Ordering::Release);
        }
    }
}

// ----------------------------------------------------------------------------
// The interface the USB core uses

impl Host for Uhci {
    fn ports(&self) -> u8 {
        self.ports
    }

    fn port_status(&self, port: u8) -> PortStatus {
        let v = inw(self.port_reg(port));
        PortStatus {
            connected: v & PORT_CCS != 0,
            enabled: v & PORT_PED != 0,
            speed: if v & PORT_LSDA != 0 { SPEED_LOW } else { SPEED_FULL },
            changed: v & PORT_CHANGES != 0,
        }
    }

    fn port_clear_changes(&self, port: u8) {
        let r = self.port_reg(port);
        let v = inw(r);
        outw(r, (v & PORT_KEEP) | (v & PORT_CHANGES));
    }

    fn port_reset(&self, port: u8) -> Result<u8, Status> {
        let r = self.port_reg(port);
        outw(r, (inw(r) & PORT_KEEP) | PORT_PR);
        arcdrv::sleep_ms(50);
        outw(r, inw(r) & PORT_KEEP & !PORT_PR);
        for _ in 0..10 {
            arcdrv::port::io_wait();
        }
        // The port does not enable itself after a reset.
        for _ in 0..10 {
            let v = inw(r);
            if v & PORT_CCS == 0 {
                return Err(E_NODEV);
            }
            if v & PORT_CHANGES != 0 {
                outw(r, (v & PORT_KEEP) | (v & PORT_CHANGES));
            }
            if v & PORT_PED != 0 {
                arcdrv::sleep_ms(10); // reset recovery
                return Ok(if v & PORT_LSDA != 0 { SPEED_LOW } else { SPEED_FULL });
            }
            outw(r, (v & PORT_KEEP) | PORT_PED);
            arcdrv::sleep_ms(10);
        }
        Err(E_IO)
    }

    fn add_device(&self, at: &Attach, mps0: u16) -> Result<Handle, Status> {
        let pipe = Pipe::new(EP_CONTROL, 0, mps0, CONTROL_TDS, CONTROL_BUF).ok_or(E_NOMEM)?;
        let h = self.next_handle.fetch_add(1, Ordering::Relaxed);
        let dev =
            Arc::new(Dev { address: AtomicU8::new(0), low_speed: at.speed == SPEED_LOW, pipes: IrqLock::new(alloc::vec![pipe.clone()]) });
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
        let pipe = self.dev(dev)?.pipe(0).ok_or(E_GONE)?;
        pipe.mps.store(mps0.clamp(8, 64), Ordering::Relaxed);
        Ok(())
    }

    fn configure(&self, dev: Handle, config: &[u8], _hub: Option<HubInfo>) -> Result<(), Status> {
        let d = self.dev(dev)?;
        for e in arcusb::config_endpoints(config) {
            if d.pipe(e.address).is_some() {
                continue;
            }
            let mps = e.packet_size().clamp(1, 64);
            let (tds, buf) = if e.kind() == EP_BULK { (BULK_BUF / mps as usize, BULK_BUF) } else { (1, mps as usize) };
            let pipe = Pipe::new(e.kind(), e.address, mps, tds, buf).ok_or(E_NOMEM)?;
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
            self.arm_poll(&pipe, d.address.load(Ordering::Relaxed), d.low_speed);
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
        pipe.start(LINK_T);
        pipe.toggle.store(false, Ordering::Relaxed);
        OK
    }

    fn remove_device(&self, dev: Handle) {
        let Some(d) = self.devices.remove(dev) else { return };
        let pipes: Vec<Arc<Pipe>> = d.pipes.lock().clone();
        for p in &pipes {
            p.gone.store(true, Ordering::Release);
            stop_poll(self, p);
        }
        self.schedule_remove(&pipes);
    }

    const SLEEPS: bool = true;

    fn stop(&self) {
        self.asleep.store(true, Ordering::Release);
        outw(self.io + USBINTR, 0);
        outw(self.io + USBCMD, 0);
        arcdrv::wait_until(10, || inw(self.io + USBSTS) & STS_HALTED != 0);
    }

    fn restart(&self) -> Status {
        let st = reset(self.io);
        if st != OK {
            return st;
        }
        init_frames(&self.frames);
        let st = self.start();
        self.asleep.store(false, Ordering::Release);
        st
    }
}

impl Uhci {
    /// Points the controller at its frame list and lets it run.
    fn start(&self) -> Status {
        let io = self.io;
        outl(io + FRBASEADD, self.frames.phys() as u32);
        outw(io + FRNUM, 0);
        outb(io + SOFMOD, 64);
        outw(io + USBSTS, 0x3F);
        outw(io + USBINTR, INTR_ENABLE);
        Pci(self.pci).write16(USBLEGSUP, LEGSUP_PIRQ);
        outw(io + USBCMD, CMD_RUN | CMD_CF | CMD_MAXP);
        if !arcdrv::wait_until(100, || inw(io + USBSTS) & STS_HALTED == 0) {
            arcdrv::warn!("controller does not start");
            return E_IO;
        }
        if inw(io + USBSTS) & STS_FATAL != 0 {
            arcdrv::warn!("controller reports an error");
        }
        OK
    }
}

/// Global reset, then a host controller reset.
fn reset(io: u16) -> Status {
    outw(io + USBINTR, 0);
    outw(io + USBCMD, 0);
    arcdrv::wait_until(10, || inw(io + USBSTS) & STS_HALTED != 0);
    outw(io + USBCMD, CMD_GRESET);
    arcdrv::sleep_ms(50);
    outw(io + USBCMD, 0);
    arcdrv::sleep_ms(10);
    outw(io + USBCMD, CMD_HCRESET);
    if !arcdrv::wait_until(50, || inw(io + USBCMD) & CMD_HCRESET == 0) {
        arcdrv::warn!("controller reset timed out");
        return E_TIMEOUT;
    }
    OK
}

/// Every frame starts at the anchor queue head, which links nowhere yet.
fn init_frames(frames: &Dma) {
    frames.write::<u32>(ANCHOR, LINK_T);
    frames.write::<u32>(ANCHOR + 4, LINK_T);
    let anchor = (frames.phys() + ANCHOR as u64) as u32 | LINK_QH;
    for i in 0..FRAMES {
        frames.write::<u32>(i * 4, anchor);
    }
}

/// Stops polling a pipe; never returns while its callback runs (the driver
/// may go away right after).
fn stop_poll(u: &Uhci, pipe: &Arc<Pipe>) {
    let stopped = pipe.poll.lock().take().is_some();
    while pipe.in_callback.load(Ordering::Acquire) {
        arcdrv::sleep_ms(1);
    }
    if stopped {
        u.polls.lock().retain(|q| !Arc::ptr_eq(q, pipe));
        u.halt(pipe);
    }
}

// ----------------------------------------------------------------------------
// Bring-up

unsafe extern "C" fn irq(ctx: *mut c_void) {
    // SAFETY: the controller leaked in probe.
    let u = unsafe { &*(ctx as *const Uhci) };
    let sts = inw(u.io + USBSTS);
    if sts & STS_EVENTS == 0 {
        return; // not ours (shared line)
    }
    outw(u.io + USBSTS, sts & STS_EVENTS);
    u.wake.signal();
}

unsafe extern "C" fn poll_thread(ctx: *mut c_void) {
    // SAFETY: the controller leaked in probe.
    let u = unsafe { &*(ctx as *const Uhci) };
    let mut reported = false;
    loop {
        u.wake.wait(if u.has_irq.load(Ordering::Relaxed) { 20 } else { 4 });
        if !u.has_irq.load(Ordering::Relaxed) {
            let sts = inw(u.io + USBSTS);
            outw(u.io + USBSTS, sts & STS_EVENTS);
        }
        let sts = inw(u.io + USBSTS);
        if sts & STS_HALTED != 0 && !reported && !u.asleep.load(Ordering::Acquire) {
            arcdrv::error!("controller stopped (status {sts:#x})");
            reported = true;
        }
        u.service_polls();
    }
}

/// Number of root ports: the registers that read like ports (bit 7 set).
fn count_ports(io: u16) -> u8 {
    let mut n = 0;
    while n < 8 {
        let v = inw(io + PORTSC + n as u16 * 2);
        if v & PORT_VALID == 0 || v == 0xFFFF {
            break;
        }
        n += 1;
    }
    if n > 7 {
        2
    } else {
        n
    }
}

fn probe(dev: &DeviceInfo) -> Status {
    if dev.class != 0x0C || dev.subclass != 0x03 || dev.prog_if != 0x00 {
        return E_NODEV;
    }
    let Some(io) = dev.io(0) else { return E_IO };
    let pci = Pci(dev.id);
    pci.enable(true, false, true);
    // Take the controller away from the firmware's keyboard emulation.
    pci.write16(USBLEGSUP, LEGSUP_OFF);

    let st = reset(io);
    if st != OK {
        return st;
    }
    let ports = count_ports(io);
    if ports == 0 {
        return E_NODEV;
    }

    let Some(frames) = Dma::new(ANCHOR + 16) else { return E_NOMEM };
    init_frames(&frames);

    let u: &'static Uhci = Box::leak(Box::new(Uhci {
        io,
        ports,
        frames,
        schedule: Mutex::new(Vec::new()),
        devices: Table::new(),
        next_handle: AtomicU32::new(1),
        polls: IrqLock::new(Vec::new()),
        wake: Event::new(),
        has_irq: AtomicBool::new(false),
        pci: dev.id,
        asleep: AtomicBool::new(false),
    }));
    let ctx = u as *const Uhci as *mut c_void;
    u.has_irq.store(arcdrv::irq_attach_any(dev, irq, ctx), Ordering::Relaxed);
    arcdrv::spawn("uhci-poll", poll_thread, ctx);

    let st = u.start();
    if st != OK {
        return st;
    }
    arcdrv::info!("{} ports, {}", ports, if u.has_irq.load(Ordering::Relaxed) { "interrupts" } else { "polling" });
    arcusb::start(u, dev.id, "uhci-ports");
    OK
}

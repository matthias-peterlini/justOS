//! The AML task: it owns the interpreter of the firmware's AML (justaml),
//! loads the DSDT and SSDTs, readies the devices, and then answers the
//! events of the chipset (GPEs, the embedded controller) and the requests
//! of the rest of the kernel. AML runs only here, on a big stack: methods
//! nest deeply.

use super::{ec, gpe, power};
use crate::arch::cpu::{inb, inl, inw, outb, outl, outw};
use crate::sync::IrqLock;
use crate::task::{self, Event, Tid};
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use justaml::{space, Error, Handler, Interpreter, NodeId, ObjectType, PciAddress, Value};
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

pub type Aml = Interpreter<Host>;

/// What the interpreter reaches the hardware through.
pub struct Host {
    /// Device memory mapped so far: physical page to virtual address.
    pages: BTreeMap<u64, u64>,
    /// Notify calls of the method that runs, handled after it.
    notes: Vec<(NodeId, u64)>,
    /// Problems reported (a few are shown, then only counted).
    problems: u32,
}

impl Host {
    fn new() -> Host {
        Host { pages: BTreeMap::new(), notes: Vec::new(), problems: 0 }
    }

    /// A virtual address for physical memory: RAM through the direct map,
    /// anything else (chipset registers) mapped uncached.
    fn virt(&mut self, addr: u64) -> Result<u64, Error> {
        if super::is_ram(addr) {
            return Ok(crate::mem::phys_to_virt(addr));
        }
        let page = addr & !0xFFF;
        let v = match self.pages.get(&page) {
            Some(&v) => v,
            None => {
                let v = crate::mem::region::map_device(page, 0x1000, crate::mem::paging::NO_CACHE).ok_or(Error::Hardware)?;
                self.pages.insert(page, v);
                v
            }
        };
        Ok(v + (addr & 0xFFF))
    }

    fn mem_read(&mut self, addr: u64, bits: u8) -> Result<u64, Error> {
        let n = bits as u64 / 8;
        if addr % n != 0 {
            // Unaligned: byte by byte.
            let mut v = 0;
            for i in (0..n).rev() {
                v = v << 8 | self.mem_read(addr + i, 8)?;
            }
            return Ok(v);
        }
        let p = self.virt(addr)?;
        // SAFETY: p maps `addr`, aligned to the access; AML asked for it.
        Ok(unsafe {
            match bits {
                8 => (p as *const u8).read_volatile() as u64,
                16 => (p as *const u16).read_volatile() as u64,
                32 => (p as *const u32).read_volatile() as u64,
                _ => (p as *const u64).read_volatile(),
            }
        })
    }

    fn mem_write(&mut self, addr: u64, bits: u8, v: u64) -> Result<(), Error> {
        let n = bits as u64 / 8;
        if addr % n != 0 {
            for i in 0..n {
                self.mem_write(addr + i, 8, v >> (8 * i))?;
            }
            return Ok(());
        }
        let p = self.virt(addr)?;
        // SAFETY: as in mem_read.
        unsafe {
            match bits {
                8 => (p as *mut u8).write_volatile(v as u8),
                16 => (p as *mut u16).write_volatile(v as u16),
                32 => (p as *mut u32).write_volatile(v as u32),
                _ => (p as *mut u64).write_volatile(v),
            }
        }
        Ok(())
    }
}

fn cmos_ports(reg: u64) -> (u16, u16) {
    if reg < 0x80 {
        (0x70, 0x71)
    } else {
        (0x72, 0x73)
    }
}

fn pci_loc(p: PciAddress) -> Result<u32, Error> {
    if p.segment != 0 {
        return Err(Error::NoHandler(space::PCI));
    }
    Ok((p.bus as u32) << 8 | (p.device as u32 & 0x1F) << 3 | (p.function as u32 & 7))
}

impl Handler for Host {
    fn read(&mut self, sp: u8, addr: u64, bits: u8) -> Result<u64, Error> {
        match sp {
            space::MEMORY => self.mem_read(addr, bits),
            space::IO => {
                let port = u16::try_from(addr).map_err(|_| Error::Range)?;
                Ok(match bits {
                    8 => inb(port) as u64,
                    16 => inw(port) as u64,
                    32 => inl(port) as u64,
                    _ => inl(port) as u64 | (inl(port.wrapping_add(4)) as u64) << 32,
                })
            }
            space::EMBEDDED_CONTROL | space::CMOS => {
                let mut v = 0;
                for i in (0..bits as u64 / 8).rev() {
                    let reg = addr + i;
                    let b = if sp == space::CMOS {
                        let (index, data) = cmos_ports(reg);
                        outb(index, reg as u8 & 0x7F);
                        inb(data)
                    } else {
                        ec::read(reg as u8).ok_or(Error::Hardware)?
                    };
                    v = v << 8 | b as u64;
                }
                Ok(v)
            }
            s => Err(Error::NoHandler(s)),
        }
    }

    fn write(&mut self, sp: u8, addr: u64, bits: u8, value: u64) -> Result<(), Error> {
        match sp {
            space::MEMORY => self.mem_write(addr, bits, value),
            space::IO => {
                let port = u16::try_from(addr).map_err(|_| Error::Range)?;
                match bits {
                    8 => outb(port, value as u8),
                    16 => outw(port, value as u16),
                    32 => outl(port, value as u32),
                    _ => {
                        outl(port, value as u32);
                        outl(port.wrapping_add(4), (value >> 32) as u32);
                    }
                }
                Ok(())
            }
            space::EMBEDDED_CONTROL | space::CMOS => {
                for i in 0..bits as u64 / 8 {
                    let reg = addr + i;
                    let b = (value >> (8 * i)) as u8;
                    if sp == space::CMOS {
                        let (index, data) = cmos_ports(reg);
                        outb(index, reg as u8 & 0x7F);
                        outb(data, b);
                    } else if !ec::write(reg as u8, b) {
                        return Err(Error::Hardware);
                    }
                }
                Ok(())
            }
            s => Err(Error::NoHandler(s)),
        }
    }

    fn read_pci(&mut self, p: PciAddress, offset: u16, bits: u8) -> Result<u64, Error> {
        let loc = pci_loc(p)?;
        Ok(match bits {
            64 => crate::dev::pci::read(loc, offset, 4) as u64 | (crate::dev::pci::read(loc, offset + 4, 4) as u64) << 32,
            b => crate::dev::pci::read(loc, offset, b / 8) as u64,
        })
    }

    fn write_pci(&mut self, p: PciAddress, offset: u16, bits: u8, value: u64) -> Result<(), Error> {
        let loc = pci_loc(p)?;
        match bits {
            64 => {
                crate::dev::pci::write(loc, offset, 4, value as u32);
                crate::dev::pci::write(loc, offset + 4, 4, (value >> 32) as u32);
            }
            b => crate::dev::pci::write(loc, offset, b / 8, value as u32),
        }
        Ok(())
    }

    fn sleep(&mut self, ms: u64) {
        task::sleep_ms(ms.max(1));
    }

    fn stall(&mut self, us: u64) {
        crate::time::spin_us(us);
    }

    fn timer(&mut self) -> u64 {
        crate::time::uptime_ms() * 10_000
    }

    fn notify(&mut self, node: NodeId, _path: &str, value: u64) {
        if self.notes.len() < 64 {
            self.notes.push((node, value));
        }
    }

    fn debug(&mut self, text: &str) {
        crate::debug!("acpi", "AML debug: {text}");
    }

    fn log(&mut self, text: &str) {
        self.problems += 1;
        if self.problems <= 12 {
            crate::debug!("acpi", "AML: {text}");
        }
    }

    fn fatal(&mut self, kind: u8, code: u32, arg: u64) {
        crate::warn!("acpi", "the firmware reports a fatal error: type {kind:#x}, code {code:#x}, argument {arg:#x}");
    }

    fn table(&mut self, signature: &str, oem: &str, oem_table: &str) -> Option<Vec<u8>> {
        let (phys, _) = self.table_address(signature, oem, oem_table)?;
        super::table(phys).map(|t| t.to_vec())
    }

    fn table_address(&mut self, signature: &str, oem: &str, oem_table: &str) -> Option<(u64, u64)> {
        for (sig, phys) in super::tables() {
            if sig != signature.as_bytes() {
                continue;
            }
            let Some(t) = super::table(phys) else { continue };
            let same = |field: &[u8], want: &str| want.is_empty() || field.iter().take_while(|&&c| c != 0).copied().eq(want.bytes());
            if same(&t[10..16], oem) && same(&t[16..24], oem_table) {
                return Some((phys, t.len() as u64));
            }
        }
        None
    }
}

type Job = Box<dyn FnOnce(&mut Aml) + Send>;

static JOBS: IrqLock<VecDeque<Job>> = IrqLock::new(VecDeque::new());
/// Wakes the AML task: a GPE fired or a job is waiting.
pub(super) static WAKE: Event = Event::new();
static READY: AtomicBool = AtomicBool::new(false);
static WORKER: AtomicU32 = AtomicU32::new(u32::MAX);
static SUMMARY: IrqLock<String> = IrqLock::new(String::new());

/// The interpreter is running and the devices are ready.
pub fn ready() -> bool {
    READY.load(Ordering::Acquire)
}

/// What the AML task found, for `acpi` and Info sistema.
pub fn summary() -> String {
    SUMMARY.lock().clone()
}

/// Runs `f` on the AML task and waits for its answer, at most `timeout_ms`.
/// None if the interpreter is not running, or from the AML task itself.
pub fn with<R: Send + 'static>(timeout_ms: u64, f: impl FnOnce(&mut Aml) -> R + Send + 'static) -> Option<R> {
    if !ready() || task::current_tid() == WORKER.load(Ordering::Relaxed) as Tid {
        return None;
    }
    let reply: Arc<(IrqLock<Option<R>>, Event)> = Arc::new((IrqLock::new(None), Event::new()));
    let r = reply.clone();
    JOBS.lock().push_back(Box::new(move |aml| {
        let v = f(aml);
        *r.0.lock() = Some(v);
        r.1.signal();
    }));
    WAKE.signal();
    if reply.1.wait(Some(timeout_ms)) {
        reply.0.lock().take()
    } else {
        None
    }
}

/// The devices under `path`, one per line, indented by depth, with their
/// ids and `_STA`. None if the interpreter is not running.
pub fn tree(path: &str) -> Option<Result<Vec<String>, String>> {
    let path = String::from(path);
    with(10_000, move |aml| {
        let root = aml.find(&path).ok_or_else(|| format!("{path}: not found"))?;
        let mut out = Vec::new();
        walk(aml, root, 0, &mut out);
        Ok(out)
    })
}

fn walk(aml: &mut Aml, n: NodeId, depth: usize, out: &mut Vec<String>) {
    for c in aml.children(n) {
        if out.len() >= 4000 {
            return;
        }
        let kind = aml.object_type(c);
        let what = match kind {
            ObjectType::Device => "",
            ObjectType::Processor => "processor",
            ObjectType::ThermalZone => "thermal zone",
            ObjectType::PowerResource => "power resource",
            ObjectType::Scope => "scope",
            _ => continue,
        };
        let mut line = format!("{:w$}{}", "", aml.name(c), w = depth * 2);
        if !what.is_empty() {
            line.push_str(&format!("  ({what})"));
        }
        let mut sta = 0x0F;
        if kind == ObjectType::Device {
            let ids = aml.ids(c);
            if !ids.is_empty() {
                line.push_str(&format!("  {}", ids.join(" ")));
            }
            if let Some(a) = aml.child(c, "_ADR").and_then(|_| aml.integer(c, "_ADR")) {
                line.push_str(&format!("  adr {a:#x}"));
            }
            sta = aml.status(c);
            if sta & 0x0F != 0x0F {
                line.push_str(&format!("  sta {sta:#x}"));
            }
        }
        out.push(line);
        if sta & 0x09 != 0 {
            walk(aml, c, depth + 1, out);
        }
    }
}

/// Evaluates an object (a method with integer arguments), for `acpi eval`.
pub fn eval(path: &str, args: Vec<u64>) -> Option<Result<String, String>> {
    let path = String::from(path);
    with(15_000, move |aml| {
        let args: Vec<Value> = args.into_iter().map(Value::Integer).collect();
        match aml.evaluate(&path, &args) {
            Ok(v) => Ok(format!("{v}")),
            Err(e) => Err(format!("{e}")),
        }
    })
}

/// Set by the wake-up job: the AML task enables its GPEs again and reads
/// batteries, lid and temperatures afresh.
static RESUMED: AtomicBool = AtomicBool::new(false);

/// Devices that may wake the computer: the buttons and the lid (as Linux
/// does by default).
const WAKE_IDS: [&str; 3] = ["PNP0C0C", "PNP0C0D", "PNP0C0E"];

/// Runs a method with one integer argument if the firmware has it.
fn call_if(aml: &mut Aml, path: &str, arg: u64) {
    if aml.find(path).is_some() {
        if let Err(e) = aml.evaluate(path, &[Value::Integer(arg)]) {
            crate::debug!("acpi", "{path}({arg}): {e}");
        }
    }
}

/// Arms (or disarms) the devices that may wake the computer from `state`
/// (`_DSW`, or the older `_PSW`). Their GPEs.
fn arm_wake(aml: &mut Aml, state: u8, on: bool) -> Vec<u16> {
    let mut gpes = Vec::new();
    for dev in aml.find_devices(&WAKE_IDS) {
        if aml.status(dev) & 1 == 0 {
            continue;
        }
        let Ok(Some(Value::Package(p))) = aml.evaluate_child(dev, "_PRW", &[]) else { continue };
        let (Some(Value::Integer(g)), Some(Value::Integer(deepest))) = (p.first(), p.get(1)) else { continue };
        if *deepest < state as u64 {
            continue;
        }
        let g = *g as u16;
        let r = if aml.child(dev, "_DSW").is_some() {
            let args = [Value::Integer(on as u64), Value::Integer(if on { state as u64 } else { 0 }), Value::Integer(0)];
            aml.evaluate_child(dev, "_DSW", &args).map(|_| ())
        } else if aml.child(dev, "_PSW").is_some() {
            aml.evaluate_child(dev, "_PSW", &[Value::Integer(on as u64)]).map(|_| ())
        } else {
            Ok(())
        };
        if let Err(e) = r {
            crate::debug!("acpi", "wake setup of {}: {e}", aml.path(dev));
        }
        if g < gpe::count() {
            gpes.push(g);
        }
    }
    gpes
}

/// Tells the firmware a sleep is coming (`_TTS`), before the devices stop.
pub(super) fn will_sleep(state: u8) {
    let _ = with(3000, move |aml| call_if(aml, "\\_TTS", state as u64));
}

/// What sleeping in a state takes, from the firmware.
pub(super) struct SleepPlan {
    /// SLP_TYPa/b of the state and of S0 (working).
    pub slp: (u8, u8),
    pub slp_s0: Option<(u8, u8)>,
    /// The GPEs that may wake the computer.
    pub wake_gpes: Vec<u16>,
}

/// Right before sleep in `state` (the devices stopped): the devices that
/// may wake the computer are armed and the firmware is told (`_PTS`,
/// `_SST`). None without AML or if the firmware has no such state.
pub(super) fn prepare_sleep(state: u8) -> Option<SleepPlan> {
    with(5000, move |aml| {
        let slp = aml.sleep_type(state)?;
        let slp_s0 = aml.sleep_type(0);
        let wake_gpes = arm_wake(aml, state, true);
        call_if(aml, "\\_PTS", state as u64);
        call_if(aml, "\\_SI._SST", 3);
        Some(SleepPlan { slp, slp_s0, wake_gpes })
    })
    .flatten()
}

/// After waking up: `_WAK`, the wake devices disarmed, the sleep light back
/// to working; then the AML task enables its GPEs again and reads
/// batteries, lid and temperatures afresh.
pub(super) fn finish_sleep(state: u8) {
    let _ = with(5000, move |aml| {
        call_if(aml, "\\_SI._SST", 2);
        call_if(aml, "\\_WAK", state as u64);
        arm_wake(aml, state, false);
        call_if(aml, "\\_SI._SST", 1);
        call_if(aml, "\\_TTS", 0);
        RESUMED.store(true, Ordering::Release);
    });
}

/// The embedded controller's ports and how many transfers timed out.
pub fn ec_info() -> Option<(u16, u16, u32)> {
    ec::present().then(|| {
        let (d, c) = ec::ports();
        (d, c, ec::timeouts())
    })
}

pub fn start() {
    match task::spawn_big("acpi", 1 << 20, worker) {
        Some(tid) => WORKER.store(tid, Ordering::Relaxed),
        None => crate::warn!("acpi", "cannot start the AML task"),
    }
}

/// What a GPE runs: the embedded controller, or a `\_GPE._Lxx`/`_Exx` method.
#[derive(Clone, Copy, PartialEq)]
enum GpeKind {
    Ec,
    Level(NodeId),
    Edge(NodeId),
}

struct Events {
    gpes: BTreeMap<u16, GpeKind>,
    /// Recent firings, to stop a GPE that fires without end.
    counts: BTreeMap<u16, (u64, u32)>,
    ec_node: Option<NodeId>,
}

fn load(aml: &mut Aml) -> (usize, usize) {
    let mut tables = 0;
    if let Some(dsdt) = super::dsdt().and_then(super::table) {
        match aml.load_table(dsdt) {
            Ok(()) => tables += 1,
            Err(e) => crate::warn!("acpi", "the DSDT does not load: {e}"),
        }
    }
    for (sig, phys) in super::tables() {
        if &sig != b"SSDT" {
            continue;
        }
        if let Some(t) = super::table(phys) {
            if aml.load_table(t).is_ok() {
                tables += 1;
            }
        }
    }
    (tables, aml.load_errors)
}

/// The ECDT, if the firmware has one: the embedded controller is usable
/// from the start, before the namespace is initialized. Its GPE and path.
fn ecdt() -> Option<(u16, String)> {
    let phys = super::tables().iter().find(|(s, _)| s == b"ECDT").map(|&(_, p)| p)?;
    let t = super::table(phys)?;
    // Both registers in I/O space (the GAS address space id).
    if t.len() < 66 || t[36] != 1 || t[48] != 1 {
        return None;
    }
    let command = u64::from_le_bytes(t[40..48].try_into().unwrap());
    let data = u64::from_le_bytes(t[52..60].try_into().unwrap());
    if command == 0 || data == 0 || command > 0xFFFF || data > 0xFFFF {
        return None;
    }
    ec::set_ports(data as u16, command as u16);
    Some((t[64] as u16, t[65..].iter().take_while(|&&c| c != 0).map(|&c| c as char).collect()))
}

/// The embedded controller: from the ECDT if there is one, else the
/// PNP0C09 device's `_CRS`; its GPE from `_GPE`.
fn setup_ec(aml: &mut Aml, ev: &mut Events, early: Option<(u16, String)>) {
    let mut ec_gpe = early.as_ref().map(|e| e.0);
    let node = early
        .and_then(|e| aml.find(&e.1))
        .or_else(|| aml.find_devices(&["PNP0C09"]).into_iter().find(|&n| aml.status(n) & 1 != 0));
    if let Some(n) = node {
        if !ec::present() {
            if let Ok(Some(Value::Buffer(crs))) = aml.evaluate_child(n, "_CRS", &[]) {
                let ports = justaml::resource::io_ports(&justaml::resource::parse(&crs));
                if ports.len() >= 2 {
                    ec::set_ports(ports[0], ports[1]);
                }
            }
        }
        if let Some(g) = aml.integer(n, "_GPE") {
            ec_gpe = Some(g as u16);
        }
    }
    if !ec::present() {
        return;
    }
    ev.ec_node = node;
    aml.region_ready(space::EMBEDDED_CONTROL);
    let (data, command) = ec::ports();
    crate::debug!("acpi", "embedded controller at ports {data:#x}/{command:#x}, GPE {ec_gpe:?}");
    if let Some(g) = ec_gpe.filter(|&g| g < gpe::count()) {
        ev.gpes.insert(g, GpeKind::Ec);
    }
}

/// GPEs with a method in `\_GPE`, except those only meant to wake the
/// computer (named by a device's `_PRW`), like ACPICA does.
fn setup_gpes(aml: &mut Aml, ev: &mut Events) {
    let count = gpe::count();
    let mut wake = Vec::new();
    for n in aml.nodes() {
        if aml.name(n) != "_PRW" {
            continue;
        }
        if let Ok(Value::Package(p)) = aml.evaluate_node(n, &[]) {
            if let Some(Value::Integer(g)) = p.first() {
                wake.push(*g as u16);
            }
        }
    }
    if let Some(scope) = aml.find("\\_GPE") {
        for m in aml.children(scope) {
            let name = aml.name(m);
            let b = name.as_bytes();
            if b.len() != 4 || b[0] != b'_' || !(b[1] == b'L' || b[1] == b'E') {
                continue;
            }
            let Ok(g) = u16::from_str_radix(&name[2..], 16) else { continue };
            if g >= count || wake.contains(&g) || ev.gpes.contains_key(&g) {
                continue;
            }
            ev.gpes.insert(g, if b[1] == b'L' { GpeKind::Level(m) } else { GpeKind::Edge(m) });
        }
    }
    for &g in ev.gpes.keys() {
        gpe::clear(g);
        gpe::enable(g);
    }
}

/// Runs the GPEs that fired, then enables them again.
fn dispatch_gpes(aml: &mut Aml, ev: &mut Events) {
    let now = crate::time::uptime_ms();
    for g in gpe::take_pending() {
        let Some(&kind) = ev.gpes.get(&g) else { continue };
        // More than 200 times in a second: something is stuck on.
        let c = ev.counts.entry(g).or_insert((now, 0));
        if now - c.0 > 1000 {
            *c = (now, 0);
        }
        c.1 += 1;
        if c.1 > 200 {
            crate::warn!("acpi", "GPE {g:#x} fires without end: turned off");
            ev.gpes.remove(&g);
            gpe::clear(g);
            continue;
        }
        match kind {
            GpeKind::Ec => {
                gpe::clear(g);
                // Every event the controller has queued.
                for _ in 0..32 {
                    if ec::status() & ec::SCI_EVT == 0 {
                        break;
                    }
                    let Some(q) = ec::query() else { break };
                    if let Some(node) = ev.ec_node {
                        let name = format!("_Q{q:02X}");
                        if aml.child(node, &name).is_some() {
                            if let Err(e) = aml.evaluate_child(node, &name, &[]) {
                                crate::debug!("acpi", "EC {name}: {e}");
                            }
                        }
                    }
                }
            }
            GpeKind::Edge(m) => {
                gpe::clear(g);
                if let Err(e) = aml.evaluate_node(m, &[]) {
                    crate::debug!("acpi", "GPE {g:#x}: {e}");
                }
            }
            GpeKind::Level(m) => {
                if let Err(e) = aml.evaluate_node(m, &[]) {
                    crate::debug!("acpi", "GPE {g:#x}: {e}");
                }
                gpe::clear(g);
            }
        }
        gpe::enable(g);
    }
}

/// What the firmware told with Notify.
fn handle_notes(aml: &mut Aml, devs: &mut power::Devices) {
    for _ in 0..8 {
        let notes = core::mem::take(&mut aml.handler.notes);
        if notes.is_empty() {
            return;
        }
        for (n, v) in notes {
            if power::is_battery(devs, n) {
                if v == 0x81 {
                    power::forget_info(devs, n);
                }
                power::refresh_batteries(aml, devs);
            } else if power::is_lid(devs, n) {
                let was_open = power::state().lid_open;
                power::refresh_lid(aml, devs);
                if was_open == Some(true) && power::state().lid_open == Some(false) {
                    super::sleep::request("lid closed");
                }
            } else if power::is_zone(devs, n) {
                power::refresh_thermal(aml, devs);
            } else {
                let ids = aml.ids(n);
                if ids.iter().any(|i| i == "PNP0C0C") && v == 0x80 {
                    super::request_power_off("power button pressed");
                } else if ids.iter().any(|i| i == "PNP0C0E") && v == 0x80 {
                    super::sleep::request("sleep button pressed");
                } else {
                    crate::debug!("acpi", "notify {} {v:#x} {ids:?}", aml.path(n));
                }
            }
        }
    }
}

fn worker() {
    let started = crate::time::uptime_ms();
    let mut aml = Interpreter::new(Host::new());
    let (tables, errors) = load(&mut aml);
    if tables == 0 {
        crate::warn!("acpi", "no AML to run");
        return;
    }
    let early = ecdt();
    let init = aml.initialize();
    aml.region_ready(space::CMOS);
    // The interrupts go through the 8259 PIC (_PIC 0), not the I/O APIC.
    if aml.find("\\_PIC").is_some() {
        let _ = aml.evaluate("\\_PIC", &[Value::Integer(0)]);
    }
    let mut ev = Events { gpes: BTreeMap::new(), counts: BTreeMap::new(), ec_node: None };
    setup_ec(&mut aml, &mut ev, early);
    setup_gpes(&mut aml, &mut ev);
    let mut devs = power::discover(&mut aml);
    let s = power::state();
    let text = format!(
        "{} names from {tables} tables ({errors} problems); {} devices, {} present, {} _INI; {} GPEs; EC {}; {} batteries, {} thermal zones",
        aml.node_count(),
        init.devices,
        init.present,
        init.ini,
        ev.gpes.len(),
        if ec::present() { "yes" } else { "no" },
        s.batteries.len(),
        s.thermal.len()
    );
    crate::info!("acpi", "AML ready in {} ms: {text}", crate::time::uptime_ms() - started);
    *SUMMARY.lock() = text;
    READY.store(true, Ordering::Release);
    loop {
        // Woken by GPEs and jobs; otherwise every few seconds for polling.
        WAKE.wait(Some(2000));
        dispatch_gpes(&mut aml, &mut ev);
        handle_notes(&mut aml, &mut devs);
        loop {
            let job = JOBS.lock().pop_front();
            let Some(job) = job else { break };
            job(&mut aml);
            handle_notes(&mut aml, &mut devs);
        }
        if RESUMED.swap(false, Ordering::AcqRel) {
            // Every GPE was turned off for sleep.
            for &g in ev.gpes.keys() {
                gpe::clear(g);
                gpe::enable(g);
            }
            ev.counts.clear();
            power::refresh_all(&mut aml, &mut devs);
            handle_notes(&mut aml, &mut devs);
        }
        power::poll(&mut aml, &mut devs, crate::time::uptime_ms());
        handle_notes(&mut aml, &mut devs);
    }
}

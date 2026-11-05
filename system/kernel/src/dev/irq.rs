//! Interrupts of devices: the 16 legacy lines (8259 PIC), shared between
//! kernel handlers and driver modules, and message-signalled interrupts
//! (MSI and MSI-X), one vector per device, delivered by the local APIC.

use crate::arch::{apic, pic};
use crate::sync::IrqLock;
use alloc::vec::Vec;
use justdk::IrqHandler;
use core::ffi::c_void;

#[derive(Clone, Copy)]
enum Handler {
    Kernel(fn()),
    Driver(IrqHandler, *mut c_void),
}

#[derive(Clone, Copy)]
struct Entry {
    owner: u32,
    handler: Handler,
}

// SAFETY: driver contexts are only used by their own handler.
unsafe impl Send for Entry {}

static LINES: IrqLock<[Vec<Entry>; 16]> = IrqLock::new([const { Vec::new() }; 16]);
static COUNTS: IrqLock<[u64; 16]> = IrqLock::new([0; 16]);

/// Owner id of handlers that belong to the kernel itself.
pub const KERNEL: u32 = 0;

pub fn attach_kernel(irq: u8, handler: fn()) {
    attach(irq, Entry { owner: KERNEL, handler: Handler::Kernel(handler) });
}

pub fn attach_driver(device: u32, irq: u8, handler: IrqHandler, ctx: *mut c_void) -> bool {
    if irq >= 16 || irq == 2 {
        return false;
    }
    attach(irq, Entry { owner: device, handler: Handler::Driver(handler, ctx) });
    true
}

fn attach(irq: u8, entry: Entry) {
    let mut lines = LINES.lock();
    lines[irq as usize].push(entry);
    pic::unmask(irq);
}

/// Vectors for message-signalled interrupts.
pub const MSI_FIRST: u8 = 48;
pub const MSI_LAST: u8 = 127;

static MSI: IrqLock<[Option<Entry>; (MSI_LAST - MSI_FIRST + 1) as usize]> = IrqLock::new([None; (MSI_LAST - MSI_FIRST + 1) as usize]);
static MSI_COUNTS: IrqLock<[u64; (MSI_LAST - MSI_FIRST + 1) as usize]> = IrqLock::new([0; (MSI_LAST - MSI_FIRST + 1) as usize]);

const CAP_MSI: u8 = 0x05;
const CAP_MSIX: u8 = 0x11;

/// Points a device's MSI-X (preferred) or MSI at a free vector that runs
/// `handler`. Returns the vector.
pub fn attach_msi(device: u32, handler: IrqHandler, ctx: *mut c_void) -> Result<u8, justdk::Status> {
    let info = super::info(device).ok_or(justdk::E_INVAL)?;
    if info.bus != justdk::BUS_PCI {
        return Err(justdk::E_UNSUPPORTED);
    }
    let loc = info.location;
    if super::pci::find_capability(loc, CAP_MSIX).is_none() && super::pci::find_capability(loc, CAP_MSI).is_none() {
        return Err(justdk::E_UNSUPPORTED);
    }
    apic::enable().ok_or(justdk::E_UNSUPPORTED)?;
    let vector = {
        let mut table = MSI.lock();
        let free = table.iter().position(|e| e.is_none()).ok_or(justdk::E_BUSY)?;
        table[free] = Some(Entry { owner: device, handler: Handler::Driver(handler, ctx) });
        MSI_COUNTS.lock()[free] = 0;
        MSI_FIRST + free as u8
    };
    if !program(&info, vector) {
        MSI.lock()[(vector - MSI_FIRST) as usize] = None;
        return Err(justdk::E_IO);
    }
    Ok(vector)
}

/// Points the device's MSI-X (preferred) or MSI at `vector` on the first
/// processor, and turns its legacy line off.
fn program(info: &justdk::DeviceInfo, vector: u8) -> bool {
    let loc = info.location;
    let Some(apic_id) = apic::enable() else { return false };
    let address = 0xFEE0_0000u32 | (apic_id & 0xFF) << 12;
    let ok = match (super::pci::find_capability(loc, CAP_MSIX), super::pci::find_capability(loc, CAP_MSI)) {
        (Some(cap), _) => program_msix(info, cap, address, vector as u32),
        (None, Some(cap)) => {
            program_msi(loc, cap, address, vector as u32);
            true
        }
        (None, None) => false,
    };
    if ok {
        // Message interrupts replace the legacy line.
        let cmd = super::pci::read(loc, 0x04, 2);
        super::pci::write(loc, 0x04, 2, cmd | (1 << 10));
    }
    ok
}

/// After sleep: the devices forgot where their message interrupts go (an
/// MSI-X table lives in the device); tells them again.
pub fn restore_msi() {
    let owners: Vec<(u32, u8)> =
        MSI.lock().iter().enumerate().filter_map(|(i, e)| e.map(|e| (e.owner, MSI_FIRST + i as u8))).collect();
    for (device, vector) in owners {
        match super::info(device) {
            Some(info) if program(&info, vector) => {}
            _ => crate::warn!("sleep", "device {device}: its message interrupt is lost"),
        }
    }
}

fn program_msi(loc: u32, cap: u16, address: u32, vector: u32) {
    use super::pci::{read, write};
    let control = read(loc, cap + 2, 2);
    let is64 = control & (1 << 7) != 0;
    write(loc, cap + 4, 4, address);
    if is64 {
        write(loc, cap + 8, 4, 0);
        write(loc, cap + 12, 2, vector);
    } else {
        write(loc, cap + 8, 2, vector);
    }
    // One message, enabled.
    write(loc, cap + 2, 2, (control & !(7 << 4)) | 1);
}

fn program_msix(info: &justdk::DeviceInfo, cap: u16, address: u32, vector: u32) -> bool {
    use super::pci::{read, write};
    let loc = info.location;
    let control = read(loc, cap + 2, 2);
    let table = read(loc, cap + 4, 4);
    let bar = info.resources[(table & 7) as usize];
    if bar.kind != justdk::RES_MEMORY || bar.base == 0 {
        return false;
    }
    let entries = (control & 0x7FF) as u64 + 1;
    let Some(t) = crate::mem::region::map_device(bar.base + (table & !7) as u64, entries * 16, crate::mem::paging::NO_CACHE) else {
        return false;
    };
    // Enabled with every vector masked, then entry 0 set and unmasked.
    write(loc, cap + 2, 2, control | (1 << 15) | (1 << 14));
    for i in 0..entries {
        // SAFETY: inside the mapped vector table.
        unsafe { core::ptr::write_volatile((t + i * 16 + 12) as *mut u32, 1) };
    }
    // SAFETY: entry 0 of the mapped table: address, upper address, data,
    // control.
    unsafe {
        core::ptr::write_volatile(t as *mut u32, address);
        core::ptr::write_volatile((t + 4) as *mut u32, 0);
        core::ptr::write_volatile((t + 8) as *mut u32, vector);
        core::ptr::write_volatile((t + 12) as *mut u32, 0);
    }
    write(loc, cap + 2, 2, (control | (1 << 15)) & !(1 << 14));
    true
}

/// Turns off a device's message interrupts (its driver is going away).
fn disable_msi(device: u32) {
    let Some(info) = super::info(device) else { return };
    if info.bus != justdk::BUS_PCI {
        return;
    }
    let loc = info.location;
    if let Some(cap) = super::pci::find_capability(loc, CAP_MSIX) {
        let c = super::pci::read(loc, cap + 2, 2);
        super::pci::write(loc, cap + 2, 2, c & !(1 << 15));
    }
    if let Some(cap) = super::pci::find_capability(loc, CAP_MSI) {
        let c = super::pci::read(loc, cap + 2, 2);
        super::pci::write(loc, cap + 2, 2, c & !1);
    }
}

/// Runs the handler of a message-signalled interrupt.
pub fn dispatch_msi(vector: u8) {
    MSI_COUNTS.lock()[(vector - MSI_FIRST) as usize] += 1;
    let entry = MSI.lock()[(vector - MSI_FIRST) as usize];
    if let Some(Entry { handler: Handler::Driver(f, ctx), .. }) = entry {
        // SAFETY: the driver registered this handler with this context.
        unsafe { f(ctx) };
    }
}

/// Removes the handlers of `device` (on `irq`, or on every line for `None`),
/// and its message interrupts.
pub fn detach(device: u32, irq: Option<u8>) {
    let mut had_msi = false;
    for e in MSI.lock().iter_mut() {
        if e.is_some_and(|e| e.owner == device) {
            *e = None;
            had_msi = true;
        }
    }
    if had_msi {
        disable_msi(device);
    }
    let mut lines = LINES.lock();
    for (line, list) in lines.iter_mut().enumerate() {
        if irq.is_some_and(|i| i as usize != line) {
            continue;
        }
        let before = list.len();
        list.retain(|e| e.owner != device);
        if list.is_empty() && before != 0 {
            pic::mask(line as u8);
        }
    }
}

/// Runs every handler of a line (called with interrupts disabled).
pub fn dispatch(irq: u8) {
    COUNTS.lock()[irq as usize] += 1;
    let handlers: Vec<Entry> = LINES.lock()[irq as usize].clone();
    for e in handlers {
        match e.handler {
            Handler::Kernel(f) => f(),
            // SAFETY: the driver registered this handler with this context.
            Handler::Driver(f, ctx) => unsafe { f(ctx) },
        }
    }
}

/// (line, interrupts so far, handler owners) for every line in use.
pub fn stats() -> Vec<(u8, u64, Vec<u32>)> {
    let lines = LINES.lock();
    let counts = COUNTS.lock();
    (0..16u8)
        .filter(|&i| counts[i as usize] != 0 || !lines[i as usize].is_empty())
        .map(|i| (i, counts[i as usize], lines[i as usize].iter().map(|e| e.owner).collect()))
        .collect()
}

/// (vector, interrupts so far, owner) for every message interrupt in use.
pub fn msi_stats() -> Vec<(u8, u64, u32)> {
    let table = MSI.lock();
    let counts = MSI_COUNTS.lock();
    table.iter().enumerate().filter_map(|(i, e)| e.map(|e| (MSI_FIRST + i as u8, counts[i], e.owner))).collect()
}

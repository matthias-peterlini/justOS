//! The local APIC: message-signalled interrupts (MSI), the other processors
//! (start-up and inter-processor interrupts) and their timers. Legacy
//! device lines and the first processor's timer stay on the 8259 PIC.

use crate::mem::{paging, region};
use crate::sync::IrqLock;
use core::sync::atomic::{AtomicU64, Ordering};

const MSR_APIC_BASE: u32 = 0x1B;
const REG_ID: usize = 0x20;
const REG_EOI: usize = 0xB0;
const REG_SVR: usize = 0xF0;
const REG_ICR_LOW: usize = 0x300;
const REG_ICR_HIGH: usize = 0x310;
const REG_TIMER: usize = 0x320;
const REG_LINT0: usize = 0x350;
const REG_LINT1: usize = 0x360;
const REG_TIMER_INIT: usize = 0x380;
const REG_TIMER_COUNT: usize = 0x390;
const REG_TIMER_DIV: usize = 0x3E0;

/// Vector the APIC uses for spurious interrupts (ignored, no EOI).
pub const VEC_SPURIOUS: u8 = 0xFF;

/// (virtual address of the registers, APIC id); 0 until enabled.
static APIC: IrqLock<(u64, u32)> = IrqLock::new((0, 0));
/// The register page, for the paths that must not take a lock (EOI, IPIs).
static BASE: AtomicU64 = AtomicU64::new(0);
/// Its physical address.
static PHYS: AtomicU64 = AtomicU64::new(0);

fn read(base: u64, reg: usize) -> u32 {
    // SAFETY: inside the mapped register page.
    unsafe { core::ptr::read_volatile((base as usize + reg) as *const u32) }
}

fn write(base: u64, reg: usize, v: u32) {
    // SAFETY: inside the mapped register page.
    unsafe { core::ptr::write_volatile((base as usize + reg) as *mut u32, v) }
}

/// Enables the local APIC (once) and returns its id, or `None` if the CPU
/// has none.
pub fn enable() -> Option<u32> {
    let mut a = APIC.lock();
    if a.0 != 0 {
        return Some(a.1);
    }
    if super::cpu::cpuid(1, 0).edx & (1 << 9) == 0 {
        return None;
    }
    let msr = super::cpu::rdmsr(MSR_APIC_BASE);
    let phys = msr & 0xF_FFFF_F000;
    if msr & (1 << 11) == 0 {
        // SAFETY: turns on the APIC at its current base.
        unsafe { super::cpu::wrmsr(MSR_APIC_BASE, msr | (1 << 11)) };
    }
    let base = region::map_device(phys, 4096, paging::NO_CACHE)?;
    PHYS.store(phys, Ordering::Relaxed);
    setup_first(base);
    let id = read(base, REG_ID) >> 24;
    *a = (base, id);
    BASE.store(base, Ordering::Release);
    Some(id)
}

/// The first processor's APIC: on, with the PIC's line through.
fn setup_first(base: u64) {
    let svr = read(base, REG_SVR);
    if svr & 0x100 == 0 {
        // Software-disabled until now: the PIC keeps working through LINT0
        // (virtual wire mode), NMIs through LINT1.
        write(base, REG_LINT0, 0x700);
        write(base, REG_LINT1, 0x400);
    }
    write(base, REG_SVR, svr | 0x100 | VEC_SPURIOUS as u32);
}

/// After sleep, on the first processor: its APIC is back to its reset state.
/// Turned on again at the same address if it was in use.
pub fn resume() {
    let base = BASE.load(Ordering::Acquire);
    if base == 0 {
        return;
    }
    let phys = PHYS.load(Ordering::Relaxed);
    let msr = super::cpu::rdmsr(MSR_APIC_BASE);
    if msr & 0xF_FFFF_F000 != phys || msr & (1 << 11) == 0 {
        // SAFETY: the APIC at the address the kernel mapped.
        unsafe { super::cpu::wrmsr(MSR_APIC_BASE, (msr & 0xFFF) | phys | (1 << 11)) };
    }
    setup_first(base);
}

/// Turns on the local APIC of another processor (the registers are at the
/// same address on every processor) and returns its id. The PIC's line
/// stays with the first processor: LINT0 is masked here.
pub fn enable_here() -> u32 {
    let base = BASE.load(Ordering::Acquire);
    let msr = super::cpu::rdmsr(MSR_APIC_BASE);
    if msr & (1 << 11) == 0 {
        // SAFETY: turns on this processor's APIC at the shared base.
        unsafe { super::cpu::wrmsr(MSR_APIC_BASE, msr | (1 << 11)) };
    }
    write(base, REG_LINT0, 1 << 16);
    write(base, REG_LINT1, 0x400);
    write(base, REG_SVR, read(base, REG_SVR) | 0x100 | VEC_SPURIOUS as u32);
    read(base, REG_ID) >> 24
}

/// Ends an APIC interrupt.
pub fn eoi() {
    let base = BASE.load(Ordering::Acquire);
    if base != 0 {
        write(base, REG_EOI, 0);
    }
}

fn send(dest: u32, low: u32) {
    let base = BASE.load(Ordering::Acquire);
    if base == 0 {
        return;
    }
    // Wait until the previous interrupt has been delivered.
    while read(base, REG_ICR_LOW) & (1 << 12) != 0 {
        core::hint::spin_loop();
    }
    write(base, REG_ICR_HIGH, dest << 24);
    write(base, REG_ICR_LOW, low);
}

/// INIT to a processor: it resets and waits for a start-up IPI.
pub fn send_init(dest: u32) {
    send(dest, 0x0000_4500); // INIT, level assert
}

/// Start-up IPI: the processor begins in real mode at `page * 4096`.
pub fn send_startup(dest: u32, page: u8) {
    send(dest, 0x0000_4600 | page as u32);
}

/// An interrupt with `vector` to the processor whose local APIC is `dest`.
pub fn send_to(dest: u32, vector: u8) {
    // Two writes: nothing else on this processor may send in between.
    let irq = crate::arch::cpu::irq_save();
    send(dest, (1 << 14) | vector as u32);
    crate::arch::cpu::irq_restore(irq);
}

/// An interrupt with `vector` to every processor but this one.
pub fn send_others(vector: u8) {
    send(0, (0b11 << 18) | (1 << 14) | vector as u32);
}

/// Timer ticks per millisecond, measured against the system tick (with
/// the divider set to 16). Called on the first processor with interrupts
/// on.
pub fn timer_calibrate() -> u32 {
    let base = BASE.load(Ordering::Acquire);
    write(base, REG_TIMER_DIV, 0x3);
    write(base, REG_TIMER, 1 << 16); // masked, one shot
    let start = crate::time::uptime_ms() + 1;
    while crate::time::uptime_ms() < start {
        core::hint::spin_loop();
    }
    write(base, REG_TIMER_INIT, u32::MAX);
    while crate::time::uptime_ms() < start + 20 {
        core::hint::spin_loop();
    }
    let used = u32::MAX - read(base, REG_TIMER_COUNT);
    write(base, REG_TIMER_INIT, 0);
    (used / 20).max(1)
}

/// Starts this processor's timer: `vector` every millisecond.
pub fn timer_start(vector: u8, per_ms: u32) {
    let base = BASE.load(Ordering::Acquire);
    write(base, REG_TIMER_DIV, 0x3);
    write(base, REG_TIMER, (1 << 17) | vector as u32); // periodic
    write(base, REG_TIMER_INIT, per_ms);
}

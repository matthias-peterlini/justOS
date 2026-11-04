//! Sleep (S3, suspend to RAM): the chipset's and the processor's part.
//!
//! While the machine sleeps RAM keeps its contents and everything else
//! loses power, the processor included. [`enter`] saves the registers the
//! calling code needs on its own stack (only the stack pointer goes in a
//! static) and writes SLP_EN. On wake-up the firmware jumps to the waking
//! vector of the FACS: the start-up code of the other processors (see
//! `arch::smp`), set up to reach `wake_entry` in long mode. That loads the
//! kernel's tables again and jumps back into `acpi_sleep_cpu`, which
//! returns as if SLP_EN had just been written. Interrupt controllers, PCI
//! and devices are restored by `crate::sleep`.

use super::{aml, enable_acpi_mode, gpe, le32, listen_pm1, pm1_events, pm1_regs, scan_sleep_type, write_sleep_type, Fadt, ACPI, RTC, WAK};
use crate::arch::cpu::{self, inw, outw};
use crate::arch::{gdt, idt, smp, syscall};
use crate::mem::{self, paging, region};
use alloc::vec::Vec;
use core::arch::global_asm;
use core::sync::atomic::{AtomicU64, AtomicU8, Ordering};

global_asm!(
    r#"
    .global acpi_sleep_cpu
acpi_sleep_cpu:
    push rbp
    push rbx
    push r12
    push r13
    push r14
    push r15
    pushfq
    mov [rdi], rsp
    mov rdi, rdx
    call rsi
    popfq
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbx
    pop rbp
    ret

    .global acpi_wake_tail
acpi_wake_tail:
    mov rsp, rdi
    mov eax, 1
    popfq
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbx
    pop rbp
    ret
"#
);

extern "C" {
    /// Saves the callee-saved registers and the flags on the stack, the
    /// stack pointer in `saved_rsp`, and returns what `enter(arg)` returns
    /// or, after waking up through [`acpi_wake_tail`], 1.
    fn acpi_sleep_cpu(saved_rsp: *mut u64, enter: extern "C" fn(u64) -> i64, arg: u64) -> i64;
    /// Returns from `acpi_sleep_cpu` with the stack it saved.
    fn acpi_wake_tail(rsp: u64) -> !;
}

static SAVED_RSP: AtomicU64 = AtomicU64::new(0);
static SAVED_CR3: AtomicU64 = AtomicU64::new(0);
static SAVED_CR4: AtomicU64 = AtomicU64::new(0);
/// The stack `wake_entry` runs on (allocated once).
static WAKE_STACK: AtomicU64 = AtomicU64::new(0);
const WAKE_STACK_PAGES: u64 = 4;

/// What sleeping takes on this machine.
pub struct Plan {
    fadt: Fadt,
    slp: (u16, u16),
    slp_s0: Option<(u16, u16)>,
    wake_gpes: Vec<u16>,
}

/// How a sleep went.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// The machine slept and woke up.
    Slept,
    /// The chipset reported a wake-up without the processor losing power
    /// (a wake event was already pending).
    WokeAtOnce,
    /// Nothing happened within a few seconds.
    Refused,
}

fn facs_ok(facs: u64) -> bool {
    facs != 0 && facs < crate::mem::frame::top().max(4 << 30) && super::bytes(facs, 4) == b"FACS" && le32(super::bytes(facs, 8), 4) >= 24
}

/// Whether the firmware offers S3, once known: 0 not asked yet, 1 yes,
/// 2 no (the AML interpreter has the last word once it runs).
static HAS_S3: AtomicU8 = AtomicU8::new(0);

/// Whether the machine can sleep: Err with the reason if not.
pub fn supported() -> Result<(), &'static str> {
    let f = ACPI.lock().fadt.ok_or("no ACPI tables")?;
    if f.pm1a_cnt == 0 || f.pm1a_cnt > 0xFFFF {
        return Err("the chipset has no power control registers (hardware-reduced ACPI)");
    }
    if !facs_ok(f.facs) {
        return Err("the firmware has no waking vector (FACS)");
    }
    let s3 = match HAS_S3.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ if aml::ready() => {
            let yes = aml::with(3000, |aml| aml.sleep_type(3)).flatten().is_some();
            HAS_S3.store(if yes { 1 } else { 2 }, Ordering::Relaxed);
            yes
        }
        _ => scan_sleep_type(f.dsdt, b"_S3_").is_some(),
    };
    if !s3 {
        return Err("the firmware does not offer sleep (no \\_S3)");
    }
    Ok(())
}

/// Tells the firmware a sleep is coming, before the devices stop.
pub fn will_sleep() {
    aml::will_sleep(3);
}

/// After the devices stopped: arms the wake devices, tells the firmware
/// (`_PTS`) and returns what entering S3 takes.
pub fn prepare() -> Result<Plan, &'static str> {
    let f = ACPI.lock().fadt.ok_or("no ACPI tables")?;
    let wide = |(a, b): (u8, u8)| (a as u16, b as u16);
    if let Some(p) = aml::prepare_sleep(3) {
        return Ok(Plan { fadt: f, slp: wide(p.slp), slp_s0: p.slp_s0.map(wide), wake_gpes: p.wake_gpes });
    }
    let slp = scan_sleep_type(f.dsdt, b"_S3_").ok_or("the firmware does not offer sleep (no \\_S3)")?;
    Ok(Plan { fadt: f, slp, slp_s0: scan_sleep_type(f.dsdt, b"_S0_"), wake_gpes: Vec::new() })
}

fn set_waking_vector(facs: u64, vector: u32) {
    if !facs_ok(facs) {
        return;
    }
    let len = le32(super::bytes(facs, 8), 4);
    let p = mem::phys_to_virt(facs);
    // SAFETY: the FACS, found and checked above; the firmware reads these
    // fields only while waking up. The 64-bit vector (version 1 and later)
    // stays 0, so the firmware uses the real-mode one.
    unsafe {
        ((p + 12) as *mut u32).write_volatile(vector);
        if len >= 32 {
            ((p + 24) as *mut u64).write_volatile(0);
        }
    }
}

struct Enter<'a> {
    plan: &'a Plan,
    alarm: bool,
}

/// Puts the machine to sleep. Called on the first processor with the
/// others stopped; returns with interrupts off, after waking up (or if the
/// machine did not sleep). `alarm`: the clock's alarm may wake it.
pub fn enter(plan: &Plan, alarm: bool) -> Outcome {
    let mut top = WAKE_STACK.load(Ordering::Relaxed);
    if top == 0 {
        let Some(stack) = region::alloc_pages(&region::STACKS, WAKE_STACK_PAGES, 1) else {
            cpu::disable_irq();
            return Outcome::Refused;
        };
        top = stack + WAKE_STACK_PAGES * mem::PAGE_SIZE;
        WAKE_STACK.store(top, Ordering::Relaxed);
    }
    let vector = smp::prepare_trampoline(wake_entry as *const () as u64, top, 0);
    cpu::disable_irq();
    SAVED_CR3.store(cpu::read_cr3(), Ordering::Relaxed);
    SAVED_CR4.store(cpu::read_cr4(), Ordering::Relaxed);
    set_waking_vector(plan.fadt.facs, vector as u32);
    let e = Enter { plan, alarm };
    // SAFETY: interrupts are off and no other processor runs; the save
    // area is a static; `e` outlives the call.
    let r = unsafe { acpi_sleep_cpu(SAVED_RSP.as_ptr(), enter_sleep_state, &e as *const Enter<'_> as u64) };
    set_waking_vector(plan.fadt.facs, 0);
    match r {
        1 => Outcome::Slept,
        0 => Outcome::WokeAtOnce,
        _ => Outcome::Refused,
    }
}

/// The last step, inside `acpi_sleep_cpu`: wake sources, caches written
/// back, SLP_EN. Returns only if the power stays on.
extern "C" fn enter_sleep_state(arg: u64) -> i64 {
    // SAFETY: `arg` is the Enter on the stack of `enter`.
    let e = unsafe { &*(arg as *const Enter<'_>) };
    let f = &e.plan.fadt;
    if f.pm1_evt_len >= 4 {
        let wake = pm1_events(f) | if e.alarm { RTC } else { 0 };
        for (status, enable) in pm1_regs(f) {
            outw(status, 0xFFFF);
            outw(enable, wake);
        }
    }
    gpe::arm(&e.plan.wake_gpes);
    // What is only in the caches would be lost.
    cpu::wbinvd();
    write_sleep_type(f, e.plan.slp, false);
    write_sleep_type(f, e.plan.slp, true);
    // The chipset takes a moment; some report a wake-up at once.
    for _ in 0..3000 {
        if f.pm1_evt_len >= 4 && pm1_regs(f).any(|(status, _)| inw(status) & WAK != 0) {
            return 0;
        }
        crate::time::spin_us(1000);
    }
    -1
}

/// Where the first processor arrives after waking up, in long mode on the
/// wake stack with the start-up page tables and interrupts off.
extern "C" fn wake_entry(_: u64) -> ! {
    // SAFETY: the address space and control registers the processor had
    // when it went to sleep; both map the kernel.
    unsafe {
        cpu::write_cr3(paging::kernel_pml4());
        cpu::write_cr4(SAVED_CR4.load(Ordering::Relaxed));
        cpu::write_cr3(SAVED_CR3.load(Ordering::Relaxed));
    }
    cpu::enable_sse();
    paging::init_pat();
    gdt::init_cpu(0);
    idt::load();
    syscall::init();
    smp::resume_bsp();
    // SAFETY: the stack saved by acpi_sleep_cpu, untouched while asleep.
    unsafe { acpi_wake_tail(SAVED_RSP.load(Ordering::Relaxed)) }
}

/// Right after waking up (PCI configuration restored, interrupts off): the
/// chipset back in ACPI mode, the fixed events listened to again, every GPE
/// off until the AML task enables its own.
pub fn after_wake(plan: &Plan) {
    let f = &plan.fadt;
    enable_acpi_mode(f);
    if let Some(s0) = plan.slp_s0 {
        write_sleep_type(f, s0, false);
    }
    gpe::disable_all();
    if super::sci_line().is_some() {
        listen_pm1(f);
    } else {
        for (status, _) in pm1_regs(f) {
            outw(status, 0xFFFF);
        }
    }
}

/// After the devices run again: `_WAK` and the firmware's events.
pub fn finish() {
    aml::finish_sleep(3);
}

/// Asks for sleep from an event (sleep button, lid): runs it on a thread
/// of its own.
pub fn request(reason: &'static str) {
    crate::sleep::request(reason);
}

//! More than one processor.
//!
//! The firmware starts only the first processor (the BSP). The others (APs)
//! are listed in ACPI's MADT; each one gets an INIT and two start-up IPIs
//! and begins in real mode at [`TRAMPOLINE`], where a small piece of code
//! switches straight to long mode with temporary page tables and jumps to
//! [`ap_entry`]. There the processor loads the kernel's tables, its own GDT
//! and TSS, starts its APIC timer and becomes one more place for the
//! scheduler to run threads.
//!
//! Each processor finds its own [`PerCpu`] through the GS base, which the
//! kernel keeps while it runs (user GS lives in `KERNEL_GS_BASE`, and the
//! entry paths `swapgs`). The `syscall` entry reads the kernel stack from it.
//!
//! When the kernel unmaps a page of its own half, the other processors may
//! still have it in their TLBs: [`shootdown`] asks them to flush with an IPI
//! and waits until they have. A processor that waits for a lock with
//! interrupts off answers such requests while it spins ([`service`]), so
//! two processors cannot wait for each other.
//!
//! Threads of user programs run on every processor. Each processor says
//! which address space it has loaded ([`load_cr3`]); a page of a program
//! that is unmapped or changed is flushed only where that program runs
//! ([`shootdown_user`]), and an address space is freed only once no
//! processor has it loaded ([`wait_unloaded`]). Switching address spaces
//! flushes the TLB, so a processor running something else holds nothing.

use super::{apic, cpu, gdt, idt, syscall};
use crate::mem::{self, paging, region};
use core::arch::global_asm;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

pub const MAX_CPUS: usize = 16;
/// The APIC timer of the other processors.
pub const VEC_TIMER: u8 = 0xF0;
/// "Flush your TLB."
pub const VEC_TLB: u8 = 0xF1;

/// Where the start-up code runs (below 1 MiB, never handed out by the
/// frame allocator), followed by its page tables.
const TRAMPOLINE: u64 = 0x8000;
const TMP_PML4: u64 = 0x9000;
const TMP_PDPT: u64 = 0xA000;
const TMP_PD: u64 = 0xB000;
const AP_STACK_PAGES: u64 = 16;

const MSR_GS_BASE: u32 = 0xC000_0101;
const MSR_EFER: u32 = 0xC000_0080;

/// One processor's own data. The first two fields are read by the
/// `syscall` entry at fixed offsets.
#[repr(C)]
pub struct PerCpu {
    /// Kernel stack of the running thread (`gs:[0]`).
    kernel_rsp: AtomicU64,
    /// The user stack pointer while `syscall` switches stacks (`gs:[8]`).
    user_rsp: AtomicU64,
    /// This processor's number, 0 for the first (`gs:[16]`).
    index: AtomicU64,
    apic_id: AtomicU32,
    /// The last TLB flush request this processor has done.
    tlb_seen: AtomicU64,
    online: AtomicBool,
    /// The top of its idle thread's stack (where it starts again after sleep).
    stack_top: AtomicU64,
    /// The address space loaded (see [`load_cr3`]).
    cr3: AtomicU64,
    /// Flushes asked of this processor alone ([`shootdown_user`]), and the
    /// count it had seen at its last flush.
    flush_asked: AtomicU64,
    flush_done: AtomicU64,
}

impl PerCpu {
    const fn new() -> PerCpu {
        PerCpu {
            kernel_rsp: AtomicU64::new(0),
            user_rsp: AtomicU64::new(0),
            index: AtomicU64::new(0),
            apic_id: AtomicU32::new(0),
            tlb_seen: AtomicU64::new(0),
            online: AtomicBool::new(false),
            stack_top: AtomicU64::new(0),
            cr3: AtomicU64::new(0),
            flush_asked: AtomicU64::new(0),
            flush_done: AtomicU64::new(0),
        }
    }
}

static CPUS: [PerCpu; MAX_CPUS] = [const { PerCpu::new() }; MAX_CPUS];
static ONLINE: AtomicUsize = AtomicUsize::new(1);
static TLB_GEN: AtomicU64 = AtomicU64::new(0);
/// How many processors were started (with the first).
static STARTED: AtomicUsize = AtomicUsize::new(1);
/// The other processors are asked to stop (before sleep): they leave their
/// threads to the first one and wait with interrupts off.
static PARK: AtomicBool = AtomicBool::new(false);
static PARKED: AtomicUsize = AtomicUsize::new(0);

global_asm!(
    r#"
    .section .text
    .balign 16
    .global ap_trampoline_start
    .global ap_trampoline_data
    .global ap_trampoline_end
    .code16
ap_trampoline_start:
    cli
    cld
    xorw %ax, %ax
    movw %ax, %ds
    movw %ax, %es
    movw %ax, %ss
    lgdtl 0x8000 + (ap_gdtr - ap_trampoline_start)
    movl %cr4, %eax
    orl $0x20, %eax
    movl %eax, %cr4
    movl 0x8000 + (ap_trampoline_data - ap_trampoline_start), %eax
    movl %eax, %cr3
    movl $0xC0000080, %ecx
    movl 0x8000 + (ap_trampoline_data - ap_trampoline_start) + 8, %eax
    xorl %edx, %edx
    wrmsr
    movl 0x8000 + (ap_trampoline_data - ap_trampoline_start) + 40, %eax
    movl %eax, %cr0
    ljmpl $0x08, $(0x8000 + (ap_long - ap_trampoline_start))
    .code64
ap_long:
    movw $0x10, %ax
    movw %ax, %ds
    movw %ax, %es
    movw %ax, %ss
    movq 0x8000 + (ap_trampoline_data - ap_trampoline_start) + 16, %rsp
    movq 0x8000 + (ap_trampoline_data - ap_trampoline_start) + 24, %rdi
    movq 0x8000 + (ap_trampoline_data - ap_trampoline_start) + 32, %rax
    pushq $0
    jmpq *%rax
    .balign 8
ap_gdt:
    .quad 0
    .quad 0x00AF9A000000FFFF
    .quad 0x00CF92000000FFFF
ap_gdtr:
    .word 23
    .long 0x8000 + (ap_gdt - ap_trampoline_start)
    .balign 8
ap_trampoline_data:
    .quad 0, 0, 0, 0, 0, 0
ap_trampoline_end:
"#,
    options(att_syntax)
);

extern "C" {
    static ap_trampoline_start: u8;
    static ap_trampoline_data: u8;
    static ap_trampoline_end: u8;
}

fn set_gs(index: usize) {
    // SAFETY: the kernel's GS base, read only through `this_cpu`.
    unsafe { cpu::wrmsr(MSR_GS_BASE, &CPUS[index] as *const PerCpu as u64) };
}

/// Makes the first processor processor 0. Right after the GDT is loaded.
pub fn init_bsp() {
    set_gs(0);
    CPUS[0].online.store(true, Ordering::Release);
}

/// The number of the processor running this code.
#[inline]
pub fn this_cpu() -> usize {
    let i: u64;
    // SAFETY: GS points at this processor's PerCpu since init_bsp/ap_entry.
    unsafe { core::arch::asm!("mov {}, gs:[16]", out(reg) i, options(nostack, readonly, preserves_flags)) };
    i as usize
}

/// How many processors are running.
pub fn count() -> usize {
    ONLINE.load(Ordering::Acquire)
}

/// The kernel stack `syscall` switches to on this processor.
pub fn set_kernel_rsp(top: u64) {
    CPUS[this_cpu()].kernel_rsp.store(top, Ordering::Relaxed);
}

fn flush_all() {
    // SAFETY: reloading CR3 with itself drops every (non-global) entry;
    // the kernel uses no global pages.
    unsafe { cpu::write_cr3(cpu::read_cr3()) };
}

/// Does the TLB flushes other processors asked for. Cheap when there is
/// nothing to do; called from spin loops and the flush IPI.
#[inline]
pub fn service() {
    if ONLINE.load(Ordering::Relaxed) <= 1 {
        return;
    }
    let want = TLB_GEN.load(Ordering::Acquire);
    let me = &CPUS[this_cpu()];
    let asked = me.flush_asked.load(Ordering::Acquire);
    if me.tlb_seen.load(Ordering::Relaxed) < want || me.flush_done.load(Ordering::Relaxed) < asked {
        flush_all();
        me.tlb_seen.fetch_max(want, Ordering::Release);
        me.flush_done.store(asked, Ordering::Release);
    }
}

/// Makes every processor forget the kernel mappings it has cached (after
/// unmapping kernel pages, before their frames are reused).
pub fn shootdown() {
    if ONLINE.load(Ordering::Acquire) <= 1 {
        return;
    }
    let gen = TLB_GEN.fetch_add(1, Ordering::AcqRel) + 1;
    let me = this_cpu();
    flush_all();
    CPUS[me].tlb_seen.fetch_max(gen, Ordering::AcqRel);
    apic::send_others(VEC_TLB);
    for (i, c) in CPUS.iter().enumerate() {
        if i == me || !c.online.load(Ordering::Acquire) {
            continue;
        }
        // A processor stuck with interrupts off for a very long time must
        // not hang this one forever. One that stops flushes everything
        // before it runs again.
        let mut spins = 0u64;
        while c.tlb_seen.load(Ordering::Acquire) < gen && c.online.load(Ordering::Acquire) && spins < 200_000_000 {
            service();
            core::hint::spin_loop();
            spins += 1;
        }
    }
}

/// Loads the address space `pml4` on this processor, after saying so: a
/// processor changing a page of it then sees that this one may hold the
/// page in its TLB. (Its change comes before it looks; this store comes
/// before the load, which flushes the TLB.)
///
/// # Safety
/// `pml4` must map the kernel half, as every address space does.
pub unsafe fn load_cr3(pml4: u64) {
    CPUS[this_cpu()].cr3.store(pml4, Ordering::SeqCst);
    core::sync::atomic::fence(Ordering::SeqCst);
    // SAFETY: the caller's promise.
    unsafe { cpu::write_cr3(pml4) };
}

/// Does another online processor have `pml4` loaded?
fn loaded_elsewhere(c: &PerCpu, me: usize, i: usize, pml4: u64) -> bool {
    i != me && c.online.load(Ordering::Acquire) && c.cr3.load(Ordering::SeqCst) == pml4
}

/// After a page of the user address space `pml4` was unmapped or changed
/// (and dropped from this processor's TLB), before its frame is used
/// again: the other processors running in that address space, and only
/// they, flush their TLBs. Nothing to do for a program running only here
/// (most of them).
pub fn shootdown_user(pml4: u64) {
    if ONLINE.load(Ordering::Acquire) <= 1 {
        return;
    }
    core::sync::atomic::fence(Ordering::SeqCst);
    let me = this_cpu();
    let mut asked = [0u64; MAX_CPUS];
    let mut sent = 0;
    for (i, c) in CPUS.iter().enumerate() {
        if loaded_elsewhere(c, me, i, pml4) {
            asked[i] = c.flush_asked.fetch_add(1, Ordering::AcqRel) + 1;
            apic::send_to(c.apic_id.load(Ordering::Relaxed), VEC_TLB);
            sent += 1;
        }
    }
    if sent == 0 {
        return;
    }
    let mut spins = 0u64;
    for (i, c) in CPUS.iter().enumerate() {
        // One that switched to another address space meanwhile is done too.
        let start = spins;
        while asked[i] != 0 && loaded_elsewhere(c, me, i, pml4) && c.flush_done.load(Ordering::Acquire) < asked[i] && spins - start < 200_000_000 {
            service();
            core::hint::spin_loop();
            spins += 1;
        }
    }
    USER_FLUSHES.fetch_add(1, Ordering::Relaxed);
    USER_IPIS.fetch_add(sent, Ordering::Relaxed);
    USER_SPINS.fetch_add(spins, Ordering::Relaxed);
}

/// What [`shootdown_user`] cost so far (the shell's `cpus`).
static USER_FLUSHES: AtomicU64 = AtomicU64::new(0);
static USER_IPIS: AtomicU64 = AtomicU64::new(0);
static USER_SPINS: AtomicU64 = AtomicU64::new(0);

/// Flushes of program pages that had to ask other processors, the IPIs
/// sent for them and the spins spent waiting for the answers.
pub fn user_flush_stats() -> (u64, u64, u64) {
    (USER_FLUSHES.load(Ordering::Relaxed), USER_IPIS.load(Ordering::Relaxed), USER_SPINS.load(Ordering::Relaxed))
}

/// Waits until no other processor has `pml4` loaded, before it is freed:
/// the other threads of a program that ended may still be switching away.
pub fn wait_unloaded(pml4: u64) {
    let me = this_cpu();
    for (i, c) in CPUS.iter().enumerate() {
        while loaded_elsewhere(c, me, i, pml4) {
            service();
            core::hint::spin_loop();
        }
    }
}

/// Copies the start-up code and its page tables below 1 MiB, and fills in
/// where it goes: `entry(arg)` in long mode on `stack` (16-byte aligned).
/// Returns the code's physical address.
pub(crate) fn prepare_trampoline(entry: u64, stack: u64, arg: u64) -> u64 {
    let data = install_trampoline() as *mut u64;
    // SAFETY: the data words of the copied trampoline.
    unsafe {
        data.write_volatile(TMP_PML4);
        // Long mode and no-execute as here; "long mode active" is the
        // processor's to set.
        data.add(1).write_volatile(cpu::rdmsr(MSR_EFER) & !(1 << 10));
        data.add(2).write_volatile(stack);
        data.add(3).write_volatile(arg);
        data.add(4).write_volatile(entry);
        // Paging, protection and caching as on this processor (a processor
        // just reset has its caches off).
        data.add(5).write_volatile((cpu::read_cr0() & 0x9FFF_FFFF) | 0x8000_0001);
    }
    TRAMPOLINE
}

/// Copies the start-up code and its page tables below 1 MiB.
fn install_trampoline() -> u64 {
    // The symbols bound the code in the kernel image.
    let (start, data, end) = (
        core::ptr::addr_of!(ap_trampoline_start) as u64,
        core::ptr::addr_of!(ap_trampoline_data) as u64,
        core::ptr::addr_of!(ap_trampoline_end) as u64,
    );
    let len = (end - start) as usize;
    // SAFETY: low memory is reserved for the kernel and reached through the
    // direct map; nothing else uses these pages.
    unsafe {
        core::ptr::copy_nonoverlapping(start as *const u8, mem::phys_to_virt(TRAMPOLINE) as *mut u8, len);
        let table = |p: u64| &mut *(mem::phys_to_virt(p) as *mut [u64; 512]);
        // Low 2 MiB mapped to itself (the code), the kernel half as usual.
        let pml4 = table(TMP_PML4);
        *pml4 = [0; 512];
        let kernel = table(paging::kernel_pml4());
        pml4[256..].copy_from_slice(&kernel[256..]);
        pml4[0] = TMP_PDPT | paging::PRESENT | paging::WRITE;
        let pdpt = table(TMP_PDPT);
        *pdpt = [0; 512];
        pdpt[0] = TMP_PD | paging::PRESENT | paging::WRITE;
        let pd = table(TMP_PD);
        *pd = [0; 512];
        pd[0] = paging::PRESENT | paging::WRITE | paging::HUGE;
    }
    mem::phys_to_virt(TRAMPOLINE + (data - start))
}

/// Starts the other processors listed by ACPI. Needs the scheduler, the
/// heap and the system tick.
pub fn start() {
    let ids = crate::acpi::processors();
    if ids.len() <= 1 {
        return;
    }
    let Some(bsp) = apic::enable() else { return };
    CPUS[0].apic_id.store(bsp, Ordering::Relaxed);
    let per_ms = apic::timer_calibrate();
    TIMER_PER_MS.store(per_ms, Ordering::Relaxed);
    let mut next = 1;
    for id in ids.into_iter().filter(|&id| id != bsp) {
        if next >= MAX_CPUS {
            break;
        }
        let Some(stack) = region::alloc_pages(&region::STACKS, AP_STACK_PAGES, 1) else { break };
        let top = stack + AP_STACK_PAGES * mem::PAGE_SIZE;
        STACK_OF_NEXT.store(stack, Ordering::Release);
        if boot(id, next, top, ap_entry as *const () as u64) {
            next += 1;
        } else {
            crate::warn!("smp", "processor with APIC id {id} did not start");
            region::free_pages(&region::STACKS, stack, AP_STACK_PAGES, 1);
        }
    }
    STARTED.store(next, Ordering::Release);
    crate::info!("smp", "{} processors running", count());
}

/// Starts processor `index` (APIC `id`) at `entry` on the stack `top`; true
/// once it is online.
fn boot(id: u32, index: usize, top: u64, entry: u64) -> bool {
    prepare_trampoline(entry, top, index as u64);
    apic::send_init(id);
    crate::task::sleep_ms(10);
    apic::send_startup(id, (TRAMPOLINE >> 12) as u8);
    let deadline = crate::time::uptime_ms() + 200;
    let mut second = false;
    while !CPUS[index].online.load(Ordering::Acquire) && crate::time::uptime_ms() < deadline {
        if !second && crate::time::uptime_ms() + 199 > deadline {
            // The specification asks for a second start-up IPI.
            apic::send_startup(id, (TRAMPOLINE >> 12) as u8);
            second = true;
        }
        crate::task::sleep_ms(1);
    }
    CPUS[index].online.load(Ordering::Acquire)
}

static TIMER_PER_MS: AtomicU32 = AtomicU32::new(0);
/// The stack of the processor being started, for its idle thread.
static STACK_OF_NEXT: AtomicU64 = AtomicU64::new(0);

/// The processor's own tables and registers, on a processor just started.
fn setup_here(index: usize) {
    // SAFETY: the kernel's tables map everything the kernel needs.
    unsafe { cpu::write_cr3(paging::kernel_pml4()) };
    cpu::enable_sse();
    paging::init_pat();
    gdt::init_cpu(index);
    idt::load();
    syscall::init();
    // After the GDT: loading the segment registers may clear the GS base.
    CPUS[index].index.store(index as u64, Ordering::Relaxed);
    set_gs(index);
    CPUS[index].cr3.store(paging::kernel_pml4(), Ordering::SeqCst);
    let id = apic::enable_here();
    CPUS[index].apic_id.store(id, Ordering::Relaxed);
}

/// Where a started processor arrives in Rust, on its own stack, with the
/// trampoline's page tables.
extern "C" fn ap_entry(index: u64) -> ! {
    let index = index as usize;
    setup_here(index);
    let stack = STACK_OF_NEXT.load(Ordering::Acquire);
    CPUS[index].stack_top.store(stack + AP_STACK_PAGES * mem::PAGE_SIZE, Ordering::Relaxed);
    crate::task::start_cpu(index, stack, AP_STACK_PAGES);
    go_online(index)
}

/// Where a processor arrives after sleep: it is still the scheduler's
/// processor `index`, running its idle thread from the top of its stack.
extern "C" fn ap_resume(index: u64) -> ! {
    let index = index as usize;
    setup_here(index);
    go_online(index)
}

fn go_online(index: usize) -> ! {
    CPUS[index].tlb_seen.store(TLB_GEN.load(Ordering::Acquire), Ordering::Release);
    flush_all();
    apic::timer_start(VEC_TIMER, TIMER_PER_MS.load(Ordering::Relaxed));
    CPUS[index].online.store(true, Ordering::Release);
    ONLINE.fetch_add(1, Ordering::AcqRel);
    // This is now the processor's idle thread.
    loop {
        if PARK.load(Ordering::Acquire) {
            park_here(index);
        }
        cpu::enable_and_halt();
    }
}

/// The other processors are being stopped: they run nothing new.
#[inline]
pub fn parking() -> bool {
    PARK.load(Ordering::Relaxed)
}

/// Stops the other processors: their threads move to the first one and
/// they wait with interrupts off. Called on a thread that may then only
/// run on the first processor; false (and nothing stopped) if a processor
/// did not stop within a second.
pub fn park() -> bool {
    let others = ONLINE.load(Ordering::Acquire).saturating_sub(1);
    PARKED.store(0, Ordering::Release);
    PARK.store(true, Ordering::Release);
    let deadline = crate::time::uptime_ms() + 1000;
    while PARKED.load(Ordering::Acquire) < others && crate::time::uptime_ms() < deadline {
        crate::task::sleep_ms(1);
    }
    // Threads that were running elsewhere come here at their next tick.
    while this_cpu() != 0 && crate::time::uptime_ms() < deadline {
        crate::task::sleep_ms(1);
    }
    if PARKED.load(Ordering::Acquire) < others || this_cpu() != 0 {
        unpark();
        return false;
    }
    true
}

/// Lets the stopped processors run again (sleep did not happen).
pub fn unpark() {
    PARK.store(false, Ordering::Release);
}

/// On a processor being stopped, in its idle thread.
fn park_here(index: usize) {
    cpu::disable_irq();
    let me = &CPUS[index];
    me.online.store(false, Ordering::Release);
    ONLINE.fetch_sub(1, Ordering::AcqRel);
    PARKED.fetch_add(1, Ordering::AcqRel);
    while PARK.load(Ordering::Acquire) {
        core::hint::spin_loop();
    }
    // Woken without sleeping: the kernel's mappings may have changed.
    me.tlb_seen.store(TLB_GEN.load(Ordering::Acquire), Ordering::Release);
    flush_all();
    me.online.store(true, Ordering::Release);
    ONLINE.fetch_add(1, Ordering::AcqRel);
    cpu::enable_irq();
}

/// After sleep, on the first processor (its tables already loaded): its
/// own data again.
pub fn resume_bsp() {
    set_gs(0);
}

/// After sleep: the other processors lost everything; they start again
/// where they stopped, as the idle threads they were.
pub fn resume() {
    let started = STARTED.load(Ordering::Acquire);
    ONLINE.store(1, Ordering::Release);
    for c in &CPUS[1..started] {
        c.online.store(false, Ordering::Release);
    }
    PARK.store(false, Ordering::Release);
    for (index, c) in CPUS.iter().enumerate().take(started).skip(1) {
        let id = c.apic_id.load(Ordering::Relaxed);
        if !boot(id, index, c.stack_top.load(Ordering::Relaxed), ap_resume as *const () as u64) {
            crate::warn!("sleep", "processor {index} did not start again");
        }
    }
}

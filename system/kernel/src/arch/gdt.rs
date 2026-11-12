//! Global descriptor table and task state segment.
//!
//! The user selectors are the ones Linux has, since Linux programs know
//! them by number (Wine, to run 32-bit Windows code inside a 64-bit
//! process): `0x23` 32-bit user code, `0x2b` user data, `0x33` 64-bit user
//! code, and three descriptors each thread sets for itself with
//! `set_thread_area` (`0x63`, `0x6b`, `0x73`). `0x08` is the kernel's code,
//! `0x10` its data, `0x40` the TSS.

use core::arch::asm;
use super::smp::MAX_CPUS;
use core::cell::UnsafeCell;

pub const KERNEL_CODE: u16 = 0x08;
pub const KERNEL_DATA: u16 = 0x10;
/// 32-bit code (compatibility mode), for programs that jump to it.
pub const USER_CODE32: u16 = 0x20 | 3;
pub const USER_DATA: u16 = 0x28 | 3;
pub const USER_CODE: u16 = 0x30 | 3;
pub const TSS_SELECTOR: u16 = 0x40;
/// The first of the descriptors a thread sets for itself, and how many.
pub const TLS_FIRST: usize = 12;
pub const TLS_COUNT: usize = 3;
const ENTRIES: usize = TLS_FIRST + TLS_COUNT;

/// What a thread has in the segment registers the kernel does not use, and
/// its own descriptors: kept while it does not run.
#[derive(Clone, Copy, Default)]
pub struct Segments {
    pub tls: [u64; TLS_COUNT],
    pub ds: u16,
    pub es: u16,
    pub fs: u16,
}

#[repr(C, packed)]
struct Tss {
    _reserved0: u32,
    rsp: [u64; 3],
    _reserved1: u64,
    ist: [u64; 7],
    _reserved2: u64,
    _reserved3: u16,
    iomap_base: u16,
}

#[repr(C, align(16))]
struct Stack([u8; 16384]);

struct Tables {
    gdt: [u64; ENTRIES],
    tss: Tss,
}

/// One processor's tables (each has its own TSS, so its own GDT).
struct Global(UnsafeCell<Tables>);
// SAFETY: each processor only touches its own entry: once while it starts
// and then from its scheduler with interrupts disabled.
unsafe impl Sync for Global {}

const EMPTY: Global = Global(UnsafeCell::new(Tables {
    gdt: [0; ENTRIES],
    tss: Tss { _reserved0: 0, rsp: [0; 3], _reserved1: 0, ist: [0; 7], _reserved2: 0, _reserved3: 0, iomap_base: 104 },
}));

static TABLES: [Global; MAX_CPUS] = [EMPTY; MAX_CPUS];

static mut DOUBLE_FAULT_STACKS: [Stack; MAX_CPUS] = [const { Stack([0; 16384]) }; MAX_CPUS];

/// Interrupt stack table slot used by the double-fault handler.
pub const IST_DOUBLE_FAULT: u8 = 1;

#[repr(C, packed)]
struct Pointer {
    limit: u16,
    base: u64,
}

/// The first processor's tables.
pub fn init() {
    init_cpu(0);
}

/// Loads processor `cpu`'s tables on the processor running this code. It
/// clears the GS base.
pub fn init_cpu(cpu: usize) {
    // SAFETY: only this processor uses its entry, and it is starting.
    let t = unsafe { &mut *TABLES[cpu].0.get() };
    t.gdt[1] = 0x00AF_9A00_0000_FFFF; // kernel code: present, ring 0, 64-bit
    t.gdt[2] = 0x00CF_9200_0000_FFFF; // kernel data
    t.gdt[4] = 0x00CF_FA00_0000_FFFF; // user code: ring 3, 32-bit
    t.gdt[5] = 0x00CF_F200_0000_FFFF; // user data: ring 3
    t.gdt[6] = 0x00AF_FA00_0000_FFFF; // user code: ring 3, 64-bit

    // SAFETY: only the address is taken.
    let stack_top = unsafe { core::ptr::addr_of!(DOUBLE_FAULT_STACKS[cpu]) } as u64 + 16384;
    t.tss.ist[IST_DOUBLE_FAULT as usize - 1] = stack_top;

    let base = core::ptr::addr_of!(t.tss) as u64;
    let limit = (core::mem::size_of::<Tss>() - 1) as u64;
    t.gdt[8] = limit & 0xFFFF
        | (base & 0xFF_FFFF) << 16
        | 0x89 << 40 // present, 64-bit available TSS
        | (limit >> 16 & 0xF) << 48
        | (base >> 24 & 0xFF) << 56;
    t.gdt[9] = base >> 32;

    let ptr = Pointer { limit: (core::mem::size_of_val(&t.gdt) - 1) as u16, base: t.gdt.as_ptr() as u64 };
    // SAFETY: the tables are static and valid; reloads every segment register.
    unsafe {
        asm!(
            "lgdt [{ptr}]",
            "push {code}",
            "lea {tmp}, [rip + 2f]",
            "push {tmp}",
            "retfq",
            "2:",
            "mov ds, {data:x}",
            "mov es, {data:x}",
            "mov ss, {data:x}",
            "xor {tmp:e}, {tmp:e}",
            "mov fs, {tmp:x}",
            "mov gs, {tmp:x}",
            "ltr {tss:x}",
            ptr = in(reg) &ptr,
            code = in(reg) KERNEL_CODE as u64,
            data = in(reg) KERNEL_DATA as u64,
            tss = in(reg) TSS_SELECTOR as u64,
            tmp = out(reg) _,
        );
    }
}

/// The running thread's segments. Call with interrupts disabled.
pub fn segments() -> Segments {
    // SAFETY: this processor's own table; interrupts are off.
    let t = unsafe { &*TABLES[super::smp::this_cpu()].0.get() };
    let mut s = Segments::default();
    s.tls.copy_from_slice(&t.gdt[TLS_FIRST..ENTRIES]);
    // SAFETY: reads three segment registers.
    unsafe {
        asm!("mov {0:x}, ds", "mov {1:x}, es", "mov {2:x}, fs", out(reg) s.ds, out(reg) s.es, out(reg) s.fs, options(nomem, nostack, preserves_flags));
    }
    s
}

/// Gives the processor a thread's segments. A selector that names nothing
/// the thread may use is loaded as null (the load would fault here, in the
/// kernel). Loading FS replaces the FS base: set it afterwards. Call with
/// interrupts disabled.
pub fn set_segments(s: &Segments) {
    // SAFETY: this processor's own table; interrupts are off.
    let t = unsafe { &mut *TABLES[super::smp::this_cpu()].0.get() };
    t.gdt[TLS_FIRST..ENTRIES].copy_from_slice(&s.tls);
    let usable = |sel: u16| -> u16 {
        let i = (sel >> 3) as usize;
        if sel == USER_DATA || (sel & 7 == 3 && (TLS_FIRST..ENTRIES).contains(&i) && s.tls[i - TLS_FIRST] != 0) {
            sel
        } else {
            0
        }
    };
    // SAFETY: each selector is null or names a present ring 3 data
    // segment; the kernel uses none of these registers.
    unsafe {
        asm!("mov ds, {0:x}", "mov es, {1:x}", "mov fs, {2:x}", in(reg) usable(s.ds), in(reg) usable(s.es), in(reg) usable(s.fs), options(nostack, preserves_flags));
    }
}

/// One of the running thread's own descriptors (0 clears it). Call with
/// interrupts disabled.
pub fn set_tls(index: usize, descriptor: u64) {
    // SAFETY: this processor's own table; interrupts are off.
    unsafe { (*TABLES[super::smp::this_cpu()].0.get()).gdt[TLS_FIRST + index] = descriptor };
}

/// Stack the CPU switches to when an interrupt arrives from user mode.
pub fn set_kernel_stack(top: u64) {
    // SAFETY: called with interrupts disabled; a plain store to this
    // processor's own TSS.
    unsafe { (*TABLES[super::smp::this_cpu()].0.get()).tss.rsp[0] = top };
    super::smp::set_kernel_rsp(top);
}

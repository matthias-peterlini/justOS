//! Interrupt descriptor table, entry stubs and the trap dispatcher.
//!
//! Every vector has a 16-byte stub that pushes a uniform [`TrapFrame`] and
//! calls [`trap_dispatch`]. Exceptions from the kernel are fatal; IRQs go to
//! [`crate::dev::irq`]; the timer drives the scheduler.

use super::{cpu, gdt, pic};
use core::arch::global_asm;
use core::cell::UnsafeCell;

/// Registers as saved on the kernel stack by the entry stubs.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TrapFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
    pub vector: u64,
    pub error: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

impl TrapFrame {
    pub fn is_user(&self) -> bool {
        self.cs & 3 == 3
    }
}

global_asm!(
    r#"
    .section .text
    .balign 16
    .global isr_stubs
isr_stubs:
    .set vec, 0
    .rept 256
    .balign 16
    .if (vec == 8) || (vec == 10) || (vec == 11) || (vec == 12) || (vec == 13) || (vec == 14) || (vec == 17) || (vec == 21) || (vec == 29) || (vec == 30)
    .else
    pushq $0
    .endif
    pushq $vec
    jmp isr_common
    .set vec, vec + 1
    .endr

    .global isr_common
isr_common:
    testb $3, 24(%rsp)
    jz 1f
    swapgs
1:
    pushq %rax
    pushq %rbx
    pushq %rcx
    pushq %rdx
    pushq %rsi
    pushq %rdi
    pushq %rbp
    pushq %r8
    pushq %r9
    pushq %r10
    pushq %r11
    pushq %r12
    pushq %r13
    pushq %r14
    pushq %r15
    movq %rsp, %rdi
    cld
    call trap_dispatch
    .global trap_return
trap_return:
    popq %r15
    popq %r14
    popq %r13
    popq %r12
    popq %r11
    popq %r10
    popq %r9
    popq %r8
    popq %rbp
    popq %rdi
    popq %rsi
    popq %rdx
    popq %rcx
    popq %rbx
    popq %rax
    testb $3, 24(%rsp)
    jz 2f
    swapgs
2:
    addq $16, %rsp
    iretq
"#,
    options(att_syntax)
);

extern "C" {
    static isr_stubs: u8;
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Gate {
    offset_low: u16,
    selector: u16,
    ist: u8,
    attributes: u8,
    offset_mid: u16,
    offset_high: u32,
    _reserved: u32,
}

const MISSING: Gate = Gate { offset_low: 0, selector: 0, ist: 0, attributes: 0, offset_mid: 0, offset_high: 0, _reserved: 0 };

struct Idt(UnsafeCell<[Gate; 256]>);
// SAFETY: only written during init.
unsafe impl Sync for Idt {}

static IDT: Idt = Idt(UnsafeCell::new([MISSING; 256]));

pub const VEC_IRQ_BASE: u8 = 32;
pub const VEC_SYSCALL: u8 = 0x80;

fn set_gate(vector: u8, handler: u64, ist: u8, user: bool) {
    // SAFETY: init-time write.
    let idt = unsafe { &mut *IDT.0.get() };
    idt[vector as usize] = Gate {
        offset_low: handler as u16,
        selector: gdt::KERNEL_CODE,
        ist,
        // present, interrupt gate (IF cleared on entry), DPL 0 or 3
        attributes: if user { 0xEE } else { 0x8E },
        offset_mid: (handler >> 16) as u16,
        offset_high: (handler >> 32) as u32,
        _reserved: 0,
    };
}

#[repr(C, packed)]
struct Pointer {
    limit: u16,
    base: u64,
}

pub fn init() {
    let base = core::ptr::addr_of!(isr_stubs) as u64;
    for v in 0..256u64 {
        let ist = if v == 8 { gdt::IST_DOUBLE_FAULT } else { 0 };
        set_gate(v as u8, base + v * 16, ist, v == VEC_SYSCALL as u64);
    }
    load();
}

/// Makes the processor running this code use the (shared) table.
pub fn load() {
    let ptr = Pointer { limit: (256 * 16 - 1) as u16, base: IDT.0.get() as u64 };
    // SAFETY: the IDT is static and fully initialised.
    unsafe { core::arch::asm!("lidt [{}]", in(reg) &ptr, options(nostack)) };
}

const EXCEPTIONS: [&str; 32] = [
    "divide error",
    "debug",
    "non-maskable interrupt",
    "breakpoint",
    "overflow",
    "bound range exceeded",
    "invalid opcode",
    "device not available",
    "double fault",
    "coprocessor segment overrun",
    "invalid TSS",
    "segment not present",
    "stack-segment fault",
    "general protection fault",
    "page fault",
    "reserved",
    "x87 floating-point error",
    "alignment check",
    "machine check",
    "SIMD floating-point error",
    "virtualization exception",
    "control protection exception",
    "reserved",
    "reserved",
    "reserved",
    "reserved",
    "reserved",
    "reserved",
    "hypervisor injection",
    "VMM communication",
    "security exception",
    "reserved",
];

#[no_mangle]
extern "C" fn trap_dispatch(frame: &mut TrapFrame) {
    let vector = frame.vector as u8;
    match vector {
        0..=31 => exception(frame),
        32..=47 => {
            let irq = vector - VEC_IRQ_BASE;
            if pic::is_spurious(irq) {
                return;
            }
            crate::dev::irq::dispatch(irq);
            pic::eoi(irq);
            if irq == 0 {
                crate::task::timer_tick();
                crate::user::check_kill(frame);
            }
        }
        VEC_SYSCALL => crate::task::syscall(frame),
        v if v as u64 == super::syscall::VEC_LINUX => crate::linux::syscall(frame),
        crate::dev::irq::MSI_FIRST..=crate::dev::irq::MSI_LAST => {
            crate::dev::irq::dispatch_msi(vector);
            super::apic::eoi();
        }
        super::smp::VEC_TIMER => {
            super::apic::eoi();
            crate::task::ap_tick();
            crate::user::check_kill(frame);
        }
        super::smp::VEC_TLB => {
            super::smp::service();
            super::apic::eoi();
        }
        _ => {}
    }
}

fn exception(frame: &mut TrapFrame) {
    let vector = frame.vector as usize;
    if frame.is_user() {
        crate::task::user_fault(frame, EXCEPTIONS[vector]);
        return;
    }
    let cr2 = cpu::read_cr2();
    // The kernel writing to a Linux program's memory (a read into its
    // buffer) where a page still maps a file's shared copy: the program
    // gets its own copy, as its own write would. No lock but the page
    // tables' is needed for that.
    if vector == 14 && frame.error & 3 == 3 && cr2 < crate::user::USER_TOP && crate::linux::mm::copy_on_write(cr2) {
        return;
    }
    crate::console::panic_screen(format_args!(
        "{} (vector {}, error {:#x})\n  rip {:#018x}  rsp {:#018x}  rflags {:#x}\n  rax {:#018x}  rbx {:#018x}  rcx {:#018x}\n  rdx {:#018x}  rsi {:#018x}  rdi {:#018x}\n  rbp {:#018x}  cr2 {:#018x}  thread {}",
        EXCEPTIONS[vector],
        vector,
        frame.error,
        frame.rip,
        frame.rsp,
        frame.rflags,
        frame.rax,
        frame.rbx,
        frame.rcx,
        frame.rdx,
        frame.rsi,
        frame.rdi,
        frame.rbp,
        cr2,
        crate::task::current_name_lossy(),
    ));
}

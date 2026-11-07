//! Thin wrappers over privileged x86-64 instructions.

use core::arch::asm;

#[inline]
pub fn inb(port: u16) -> u8 {
    let v: u8;
    // SAFETY: port I/O in ring 0 has no memory effects.
    unsafe { asm!("in al, dx", out("al") v, in("dx") port, options(nomem, nostack, preserves_flags)) };
    v
}

#[inline]
pub fn outb(port: u16, v: u8) {
    // SAFETY: see inb.
    unsafe { asm!("out dx, al", in("dx") port, in("al") v, options(nomem, nostack, preserves_flags)) };
}

#[inline]
pub fn inw(port: u16) -> u16 {
    let v: u16;
    // SAFETY: see inb.
    unsafe { asm!("in ax, dx", out("ax") v, in("dx") port, options(nomem, nostack, preserves_flags)) };
    v
}

#[inline]
pub fn outw(port: u16, v: u16) {
    // SAFETY: see inb.
    unsafe { asm!("out dx, ax", in("dx") port, in("ax") v, options(nomem, nostack, preserves_flags)) };
}

#[inline]
pub fn inl(port: u16) -> u32 {
    let v: u32;
    // SAFETY: see inb.
    unsafe { asm!("in eax, dx", out("eax") v, in("dx") port, options(nomem, nostack, preserves_flags)) };
    v
}

#[inline]
pub fn outl(port: u16, v: u32) {
    // SAFETY: see inb.
    unsafe { asm!("out dx, eax", in("dx") port, in("eax") v, options(nomem, nostack, preserves_flags)) };
}

#[inline]
pub fn io_wait() {
    outb(0x80, 0);
}

/// Disables interrupts and returns whether they were enabled.
#[inline]
pub fn irq_save() -> bool {
    let flags: u64;
    // SAFETY: reads RFLAGS and masks interrupts.
    unsafe { asm!("pushfq; pop {}; cli", out(reg) flags, options(nomem)) };
    flags & (1 << 9) != 0
}

#[inline]
pub fn irq_restore(enabled: bool) {
    if enabled {
        // SAFETY: re-enables interrupts that were enabled before irq_save.
        unsafe { asm!("sti", options(nomem, nostack)) };
    }
}

#[inline]
pub fn enable_irq() {
    // SAFETY: ring 0.
    unsafe { asm!("sti", options(nomem, nostack)) };
}

#[inline]
pub fn disable_irq() {
    // SAFETY: ring 0.
    unsafe { asm!("cli", options(nomem, nostack)) };
}

/// Waits for the next interrupt (interrupts must be enabled to wake up).
#[inline]
pub fn halt() {
    // SAFETY: ring 0.
    unsafe { asm!("hlt", options(nomem, nostack)) };
}

/// Atomically enables interrupts and halts: no interrupt can slip in between.
#[inline]
pub fn enable_and_halt() {
    // SAFETY: ring 0.
    unsafe { asm!("sti; hlt", options(nomem, nostack)) };
}

pub fn halt_forever() -> ! {
    loop {
        disable_irq();
        halt();
    }
}

#[inline]
pub fn read_cr2() -> u64 {
    let v: u64;
    // SAFETY: ring 0.
    unsafe { asm!("mov {}, cr2", out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

#[inline]
pub fn read_cr3() -> u64 {
    let v: u64;
    // SAFETY: ring 0.
    unsafe { asm!("mov {}, cr3", out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

/// # Safety
/// `pml4` must be a valid top-level page table that maps the running code.
#[inline]
pub unsafe fn write_cr3(pml4: u64) {
    // SAFETY: guaranteed by the caller.
    unsafe { asm!("mov cr3, {}", in(reg) pml4, options(nostack, preserves_flags)) };
}

pub fn read_cr0() -> u64 {
    let v: u64;
    // SAFETY: ring 0.
    unsafe { asm!("mov {}, cr0", out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

pub fn read_cr4() -> u64 {
    let v: u64;
    // SAFETY: ring 0.
    unsafe { asm!("mov {}, cr4", out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

/// # Safety
/// The value must keep the kernel's paging and protection working.
pub unsafe fn write_cr4(v: u64) {
    // SAFETY: guaranteed by the caller.
    unsafe { asm!("mov cr4, {}", in(reg) v, options(nostack, preserves_flags)) };
}

/// Writes every modified cache line back to memory and empties the caches
/// (before the processor loses its power).
pub fn wbinvd() {
    // SAFETY: ring 0; only costs time.
    unsafe { asm!("wbinvd", options(nostack, preserves_flags)) };
}

#[inline]
pub fn invlpg(addr: u64) {
    // SAFETY: only drops a TLB entry.
    unsafe { asm!("invlpg [{}]", in(reg) addr, options(nostack, preserves_flags)) };
}

#[inline]
pub fn rdmsr(msr: u32) -> u64 {
    let (lo, hi): (u32, u32);
    // SAFETY: ring 0; callers only use architectural MSRs.
    unsafe { asm!("rdmsr", in("ecx") msr, out("eax") lo, out("edx") hi, options(nomem, nostack, preserves_flags)) };
    (hi as u64) << 32 | lo as u64
}

/// # Safety
/// Writing an MSR can change how the CPU behaves in any way.
#[inline]
pub unsafe fn wrmsr(msr: u32, v: u64) {
    // SAFETY: guaranteed by the caller.
    unsafe { asm!("wrmsr", in("ecx") msr, in("eax") v as u32, in("edx") (v >> 32) as u32, options(nostack, preserves_flags)) };
}

#[derive(Clone, Copy, Debug)]
pub struct CpuidResult {
    pub eax: u32,
    pub ebx: u32,
    pub ecx: u32,
    pub edx: u32,
}

pub fn cpuid(leaf: u32, sub: u32) -> CpuidResult {
    let r = core::arch::x86_64::__cpuid_count(leaf, sub);
    CpuidResult { eax: r.eax, ebx: r.ebx, ecx: r.ecx, edx: r.edx }
}

static EMULATED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// The hypervisor's signature (CPUID 0x40000000), if there is one:
/// "TCGTCGTCGTCG" under QEMU's emulator, "KVMKVMKVM" under KVM.
pub fn hypervisor(out: &mut [u8; 12]) -> Option<&str> {
    if cpuid(1, 0).ecx & (1 << 31) == 0 {
        return None;
    }
    let r = cpuid(0x4000_0000, 0);
    out[0..4].copy_from_slice(&r.ebx.to_le_bytes());
    out[4..8].copy_from_slice(&r.ecx.to_le_bytes());
    out[8..12].copy_from_slice(&r.edx.to_le_bytes());
    let len = out.iter().position(|&b| b == 0).unwrap_or(12);
    core::str::from_utf8(&out[..len]).ok()
}

/// Notes whether the processor is emulated (QEMU's TCG): there `pause`
/// stops the virtual processor and takes QEMU's global lock, which every
/// other virtual processor needs for its devices and interrupts, so spin
/// loops leave it out ([`relax`]).
pub fn detect_emulation() {
    let mut sig = [0u8; 12];
    let tcg = hypervisor(&mut sig) == Some("TCGTCGTCGTCG");
    EMULATED.store(tcg, core::sync::atomic::Ordering::Relaxed);
}

pub fn emulated() -> bool {
    EMULATED.load(core::sync::atomic::Ordering::Relaxed)
}

/// One turn of a spin loop: `pause` on a real processor, nothing on an
/// emulated one (see [`detect_emulation`]).
#[inline]
pub fn relax() {
    if !emulated() {
        core::hint::spin_loop();
    }
}

/// The CPU brand string ("QEMU Virtual CPU", "Intel(R) Core(TM) ...").
pub fn brand(out: &mut [u8; 48]) -> &str {
    if cpuid(0x8000_0000, 0).eax < 0x8000_0004 {
        return "unknown CPU";
    }
    for i in 0..3 {
        let r = cpuid(0x8000_0002 + i, 0);
        for (j, v) in [r.eax, r.ebx, r.ecx, r.edx].iter().enumerate() {
            out[i as usize * 16 + j * 4..i as usize * 16 + j * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
    }
    let len = out.iter().position(|&b| b == 0).unwrap_or(48);
    core::str::from_utf8(&out[..len]).unwrap_or("unknown CPU").trim()
}

/// Enables the SSE unit so SSE/SSE2 instructions work. Programs built for a
/// standard x86-64 target (every Linux or Windows program, and much C code)
/// use them from the first instructions; without this they fault with an
/// invalid opcode. The kernel itself is built soft-float and needs nothing,
/// but turning SSE on is harmless for it.
pub fn enable_sse() {
    let (mut cr0, mut cr4): (u64, u64);
    // SAFETY: ring 0. Clears CR0.EM (no x87 emulation), sets CR0.MP, and sets
    // CR4.OSFXSR | CR4.OSXMMEXCPT so the CPU saves SSE state and reports SSE
    // exceptions as #XF. Also sets CR0.WP: the kernel too faults when it
    // writes to a read-only page, which is how a Linux program's page that
    // still maps a file's shared frame gets copied first.
    unsafe {
        asm!("mov {}, cr0", out(reg) cr0, options(nomem, nostack, preserves_flags));
        cr0 = (cr0 & !(1 << 2)) | (1 << 1) | (1 << 16);
        asm!("mov cr0, {}", in(reg) cr0, options(nomem, nostack, preserves_flags));
        asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack, preserves_flags));
        cr4 |= (1 << 9) | (1 << 10);
        asm!("mov cr4, {}", in(reg) cr4, options(nomem, nostack, preserves_flags));
    }
}

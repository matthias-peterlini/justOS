//! Raw system calls (see `justdk::syscall`).

pub use justdk::syscall::*;
use core::arch::asm;

#[inline]
pub fn call(n: u64, a: u64, b: u64, c: u64, d: u64, e: u64) -> i64 {
    let ret: i64;
    // SAFETY: int 0x80 enters the kernel, which validates every argument.
    unsafe {
        asm!("int 0x80", inlateout("rax") n as i64 => ret, in("rdi") a, in("rsi") b, in("rdx") c, in("r10") d, in("r8") e, options(nostack));
    }
    ret
}

/// Turns a raw result into `Ok(value)` or the error.
pub fn check(ret: i64) -> crate::Result<u64> {
    if ret < 0 {
        Err(Error::from_code(ret))
    } else {
        Ok(ret as u64)
    }
}

/// Reserves `bytes` of zeroed memory at a fixed address (`MAP_AT`).
pub fn map_at(addr: u64, bytes: u64) -> crate::Result<()> {
    check(call(MAP_AT, addr, bytes, 0, 0, 0)).map(|_| ())
}

/// Reserves `bytes` of zeroed memory at an address the kernel chooses (`MAP`).
pub fn map(bytes: u64) -> crate::Result<u64> {
    check(call(MAP, bytes, 0, 0, 0, 0))
}

/// Gives memory from [`map`]/[`map_at`] back to the kernel.
pub fn unmap(addr: u64, bytes: u64) -> crate::Result<()> {
    check(call(UNMAP, addr, bytes, 0, 0, 0)).map(|_| ())
}

/// Sets the base of the thread's FS or GS segment (`TLS_FS` / `TLS_GS`).
pub fn set_tls(which: u64, base: u64) -> crate::Result<()> {
    check(call(SET_TLS, which, base, 0, 0, 0)).map(|_| ())
}

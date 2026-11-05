//! Random numbers for keys and nonces: a ChaCha20 generator (justtls'
//! [`Drbg`]) seeded with SHA-256 of what entropy the machine offers — the
//! RDSEED and RDRAND instructions when the processor has them, the jitter
//! of the time-stamp counter, the clock — and stirred again by the timing
//! of network packets and every request.

use crate::arch::cpu;
use crate::sync::IrqLock;
use justtls::sha2::Sha256;
use justtls::Drbg;

static GEN: IrqLock<Option<Drbg>> = IrqLock::new(None);

fn tsc() -> u64 {
    // SAFETY: RDTSC only reads the counter.
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// RDSEED, else RDRAND, if the processor has either.
fn hardware() -> Option<u64> {
    let has_rdseed = cpu::cpuid(0, 0).eax >= 7 && cpu::cpuid(7, 0).ebx & (1 << 18) != 0;
    let has_rdrand = cpu::cpuid(1, 0).ecx & (1 << 30) != 0;
    for _ in 0..10 {
        let mut v: u64 = 0;
        // SAFETY: the instructions exist (checked above); they write `v`
        // and the carry flag only.
        let ok: u8 = unsafe {
            if has_rdseed {
                let ok: u8;
                core::arch::asm!("rdseed {v}", "setc {ok}", v = out(reg) v, ok = out(reg_byte) ok, options(nomem, nostack));
                ok
            } else if has_rdrand {
                let ok: u8;
                core::arch::asm!("rdrand {v}", "setc {ok}", v = out(reg) v, ok = out(reg_byte) ok, options(nomem, nostack));
                ok
            } else {
                return None;
            }
        };
        if ok == 1 {
            return Some(v);
        }
    }
    None
}

fn seed() -> Drbg {
    let mut h = Sha256::new();
    for _ in 0..32 {
        if let Some(v) = hardware() {
            h.update(&v.to_le_bytes());
        }
    }
    // Timing jitter: how long a little work takes varies with caches,
    // interrupts and the emulator.
    let mut x = 0u64;
    for i in 0..4096u64 {
        let t0 = tsc();
        for j in 0..(i % 13) {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(j);
        }
        let d = tsc().wrapping_sub(t0);
        h.update(&d.to_le_bytes()[..2]);
    }
    h.update(&x.to_le_bytes());
    h.update(&crate::time::now().to_le_bytes());
    h.update(&crate::time::uptime_ms().to_le_bytes());
    h.update(&tsc().to_le_bytes());
    Drbg::new(&h.finish())
}

/// Fills `out` with random bytes.
pub fn fill(out: &mut [u8]) {
    let mut g = GEN.lock();
    let d = g.get_or_insert_with(seed);
    let mut extra = [0u8; 16];
    extra[..8].copy_from_slice(&tsc().to_le_bytes());
    if let Some(v) = hardware() {
        extra[8..].copy_from_slice(&v.to_le_bytes());
    }
    d.reseed(&extra);
    d.fill(out);
}

pub fn u64() -> u64 {
    let mut b = [0u8; 8];
    fill(&mut b);
    u64::from_le_bytes(b)
}

/// Stirs in an event's timing (a packet arrived).
pub fn mix(event: u64) {
    if let Some(d) = GEN.lock().as_mut() {
        let t = tsc() ^ event.rotate_left(17);
        d.reseed(&t.to_le_bytes());
    }
}

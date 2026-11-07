//! seccomp: filters on the system calls of a thread, on which the
//! sandboxes of Firefox, Chromium and Android are built.
//!
//! A filter is a classic BPF program that looks at each system call the
//! thread makes (its number, the architecture, where it was made and the six
//! arguments) and says what happens to it: it runs, it fails with an error
//! of the filter's choice, it becomes a SIGSYS for the program's handler
//! (which may do the work some other way and set the result), or the
//! process ends. Filters only add up: a new one goes on top of those there,
//! and all of them are asked, the strictest answer winning. Threads and
//! processes the thread makes get its filters, execve keeps them, and
//! SECCOMP_FILTER_FLAG_TSYNC gives them to the other threads of the process.
//! Strict mode (read, write, exit and sigreturn only) is there too.
//!
//! Every program runs as root here, so no_new_privs is kept and reported
//! but not required. There is no tracer and no supervisor in user space
//! (user notifications): filters that hand a call to one make it fail with
//! ENOSYS, as on Linux when there is none.

use super::{err, signal, thread, user_bytes};
use crate::arch::idt::TrapFrame;
use crate::sync::IrqLock;
use crate::task::{self, Tid};
use crate::user;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;

/// In [`task::current_flags`]: the thread's calls go through [`allows`].
pub const FILTERED: u32 = 1;

const AUDIT_ARCH_X86_64: u32 = 0xc000_003e;

const PR_GET_SECCOMP: u64 = 21;
const PR_SET_SECCOMP: u64 = 22;
const PR_SET_NO_NEW_PRIVS: u64 = 38;
const PR_GET_NO_NEW_PRIVS: u64 = 39;

const SECCOMP_MODE_STRICT: u64 = 1;
const SECCOMP_MODE_FILTER: u64 = 2;

const SECCOMP_SET_MODE_STRICT: u64 = 0;
const SECCOMP_SET_MODE_FILTER: u64 = 1;
const SECCOMP_GET_ACTION_AVAIL: u64 = 2;

const FLAG_TSYNC: u64 = 1;
const FLAG_LOG: u64 = 2;
const FLAG_SPEC_ALLOW: u64 = 4;
const FLAG_TSYNC_ESRCH: u64 = 16;

const RET_KILL_PROCESS: u32 = 0x8000_0000;
const RET_KILL_THREAD: u32 = 0;
const RET_TRAP: u32 = 0x0003_0000;
const RET_ERRNO: u32 = 0x0005_0000;
const RET_USER_NOTIF: u32 = 0x7fc0_0000;
const RET_TRACE: u32 = 0x7ff0_0000;
const RET_LOG: u32 = 0x7ffc_0000;
const RET_ALLOW: u32 = 0x7fff_0000;
const ACTION: u32 = 0xffff_0000;

/// Instructions in one filter, and in all of a thread's together (each
/// filter counting 4 more), as Linux allows.
const MAX_INSNS: usize = 4096;
const MAX_PATH: usize = 32_768;

/// The size of what a filter looks at (`struct seccomp_data`).
const DATA_LEN: u32 = 64;

// The classic BPF instructions a seccomp filter may use.
const LD_IMM: u16 = 0x00;
const LDX_IMM: u16 = 0x01;
const ST: u16 = 0x02;
const STX: u16 = 0x03;
const LD_ABS: u16 = 0x20;
const LD_MEM: u16 = 0x60;
const LDX_MEM: u16 = 0x61;
const LD_LEN: u16 = 0x80;
const LDX_LEN: u16 = 0x81;
const RET_K: u16 = 0x06;
const RET_A: u16 = 0x16;
const TAX: u16 = 0x07;
const TXA: u16 = 0x87;
const ALU: u16 = 0x04;
const JMP: u16 = 0x05;
const SRC_X: u16 = 0x08;
const ADD: u16 = 0x00;
const SUB: u16 = 0x10;
const MUL: u16 = 0x20;
const DIV: u16 = 0x30;
const OR: u16 = 0x40;
const AND: u16 = 0x50;
const LSH: u16 = 0x60;
const RSH: u16 = 0x70;
const NEG: u16 = 0x80;
const XOR: u16 = 0xa0;
const JA: u16 = 0x00;
const JEQ: u16 = 0x10;
const JGT: u16 = 0x20;
const JGE: u16 = 0x30;
const JSET: u16 = 0x40;

#[derive(Clone, Copy)]
struct Insn {
    code: u16,
    jt: u8,
    jf: u8,
    k: u32,
}

/// A filter, on top of the ones that were there before it.
struct Filter {
    prog: Vec<Insn>,
    below: Option<Arc<Filter>>,
    /// Instructions in this one and the ones below, as Linux counts them.
    total: usize,
}

#[derive(Clone)]
enum Mode {
    Strict,
    Filtered(Arc<Filter>),
}

#[derive(Clone, Default)]
struct Sec {
    pid: u32,
    mode: Option<Mode>,
    no_new_privs: bool,
}

/// The threads with a filter, strict mode or no_new_privs.
static THREADS: IrqLock<BTreeMap<Tid, Sec>> = IrqLock::new(BTreeMap::new());

// ----------------------------------------------------------------------------
// Looking at a call

/// Before system call `nr` of a thread with filters: true if it may run.
/// Otherwise a filter has answered for it (an error in rax, or the SIGSYS
/// handler set to run), or the process ends here.
pub fn allows(frame: &mut TrapFrame, nr: u64) -> bool {
    let me = task::current_tid();
    let mode = THREADS.lock().get(&me).and_then(|s| s.mode.clone());
    let ret = match mode {
        None => return true,
        Some(Mode::Strict) if matches!(nr, 0 | 1 | 15 | 60) => return true, // read, write, rt_sigreturn, exit
        Some(Mode::Strict) => end(nr, "strict seccomp mode", 9),
        Some(Mode::Filtered(top)) => verdict(&top, &data(frame, nr)),
    };
    let value = ret & !ACTION;
    if ret & ACTION != RET_ALLOW && super::TRACE.load(Ordering::Relaxed) {
        crate::debug!("strace", "{}[{}] {nr} stopped by its seccomp filter: {ret:#x}", user::current_name(), me);
    }
    match ret & ACTION {
        RET_ALLOW | RET_LOG => true,
        RET_ERRNO => {
            frame.rax = (-i64::from(value.min(4095))) as u64;
            false
        }
        RET_TRAP => {
            if !signal::seccomp_trap(frame, nr, value, AUDIT_ARCH_X86_64) {
                end(nr, "its seccomp filter", signal::SIGSYS);
            }
            false
        }
        RET_TRACE | RET_USER_NOTIF => {
            frame.rax = (-err::ENOSYS) as u64;
            false
        }
        // KILL_PROCESS, KILL_THREAD (the whole process, here) and anything
        // unknown.
        _ => end(nr, "its seccomp filter", signal::SIGSYS),
    }
}

fn end(nr: u64, by: &str, sig: u32) -> ! {
    crate::println!("\x1b[91m{}: system call {nr} stopped by {by}\x1b[0m", user::current_name());
    user::terminate(-(128 + i64::from(sig)))
}

/// `struct seccomp_data` in 32-bit words: the call's number, the
/// architecture, where it was made (just past the `syscall` instruction) and
/// the six arguments, low half first.
fn data(frame: &TrapFrame, nr: u64) -> [u32; 16] {
    let mut d = [0; 16];
    d[0] = nr as u32;
    d[1] = AUDIT_ARCH_X86_64;
    let words = [frame.rip, frame.rdi, frame.rsi, frame.rdx, frame.r10, frame.r8, frame.r9];
    for (i, w) in words.iter().enumerate() {
        d[2 + 2 * i] = *w as u32;
        d[3 + 2 * i] = (*w >> 32) as u32;
    }
    d
}

/// What all the thread's filters say, newest first: the strictest answer
/// (the lowest action, as a signed number), the first of equals.
fn verdict(top: &Arc<Filter>, d: &[u32; 16]) -> u32 {
    let mut ret = RET_ALLOW;
    let mut f = Some(top);
    while let Some(cur) = f {
        let r = run(&cur.prog, d);
        if ((r & ACTION) as i32) < ((ret & ACTION) as i32) {
            ret = r;
        }
        f = cur.below.as_ref();
    }
    ret
}

/// Runs a checked program (see [`check`]) on `d`.
fn run(prog: &[Insn], d: &[u32; 16]) -> u32 {
    let (mut a, mut x) = (0u32, 0u32);
    let mut mem = [0u32; 16];
    let mut pc = 0;
    while let Some(i) = prog.get(pc) {
        pc += 1;
        let k = i.k;
        match i.code {
            LD_ABS => a = d[k as usize / 4],
            LD_IMM => a = k,
            LDX_IMM => x = k,
            LD_MEM => a = mem[k as usize],
            LDX_MEM => x = mem[k as usize],
            ST => mem[k as usize] = a,
            STX => mem[k as usize] = x,
            TAX => x = a,
            TXA => a = x,
            RET_K => return k,
            RET_A => return a,
            c if c == ALU | NEG => a = a.wrapping_neg(),
            c if c & 7 == ALU => {
                let v = if c & SRC_X != 0 { x } else { k };
                a = match c & 0xf0 {
                    ADD => a.wrapping_add(v),
                    SUB => a.wrapping_sub(v),
                    MUL => a.wrapping_mul(v),
                    // Dividing by an X of 0 ends the program with 0.
                    DIV if v == 0 => return 0,
                    DIV => a / v,
                    OR => a | v,
                    AND => a & v,
                    XOR => a ^ v,
                    LSH => a.wrapping_shl(v),
                    RSH => a.wrapping_shr(v),
                    _ => return RET_KILL_PROCESS,
                };
            }
            c if c == JMP | JA => pc += k as usize,
            c if c & 7 == JMP => {
                let v = if c & SRC_X != 0 { x } else { k };
                let yes = match c & 0xf0 {
                    JEQ => a == v,
                    JGT => a > v,
                    JGE => a >= v,
                    _ => a & v != 0, // JSET
                };
                pc += usize::from(if yes { i.jt } else { i.jf });
            }
            _ => return RET_KILL_PROCESS,
        }
    }
    RET_KILL_PROCESS
}

/// Whether a program is one Linux accepts as a seccomp filter: only the
/// instructions above, loads inside `seccomp_data` (whole aligned words),
/// scratch memory in range, no division by a constant 0 or shift by 32 or
/// more, jumps inside the program and a return at the end. The length
/// loads become constants.
fn check(prog: &mut [Insn]) -> bool {
    let n = prog.len();
    for (pc, i) in prog.iter_mut().enumerate() {
        let left = n - pc - 1;
        let ok = match i.code {
            LD_ABS => i.k < DATA_LEN && i.k.is_multiple_of(4),
            LD_LEN | LDX_LEN => {
                i.code = if i.code == LD_LEN { LD_IMM } else { LDX_IMM };
                i.k = DATA_LEN;
                true
            }
            LD_MEM | LDX_MEM | ST | STX => i.k < 16,
            RET_K | RET_A | LD_IMM | LDX_IMM | TAX | TXA => true,
            c if c == ALU | NEG => true,
            c if c & !(0xf0 | SRC_X) == ALU => match c & 0xf0 {
                ADD | SUB | MUL | OR | AND | XOR => true,
                DIV => c & SRC_X != 0 || i.k != 0,
                LSH | RSH => c & SRC_X != 0 || i.k < 32,
                _ => false,
            },
            c if c == JMP | JA => (i.k as usize) < left,
            c if c & !(0xf0 | SRC_X) == JMP => {
                matches!(c & 0xf0, JEQ | JGT | JGE | JSET) && usize::from(i.jt) < left && usize::from(i.jf) < left
            }
            _ => false,
        };
        if !ok {
            return false;
        }
    }
    prog.last().is_some_and(|i| matches!(i.code, RET_K | RET_A))
}

// ----------------------------------------------------------------------------
// Setting filters

/// `struct sock_fprog` at `ptr`: the instruction count and where they are.
fn read_prog(ptr: u64) -> Result<Vec<Insn>, i64> {
    let b = user_bytes(ptr, 16).map_err(|_| -err::EFAULT)?;
    let len = usize::from(u16::from_le_bytes([b[0], b[1]]));
    let at = u64::from_le_bytes(b[8..16].try_into().unwrap());
    if len == 0 || len > MAX_INSNS {
        return Err(-err::EINVAL);
    }
    let raw = user_bytes(at, len as u64 * 8).map_err(|_| -err::EFAULT)?;
    let mut prog: Vec<Insn> = raw
        .as_chunks::<8>()
        .0
        .iter()
        .map(|c| Insn { code: u16::from_le_bytes([c[0], c[1]]), jt: c[2], jf: c[3], k: u32::from_le_bytes([c[4], c[5], c[6], c[7]]) })
        .collect();
    if !check(&mut prog) {
        return Err(-err::EINVAL);
    }
    Ok(prog)
}

fn is_below(old: &Arc<Filter>, top: &Arc<Filter>) -> bool {
    let mut f = Some(top);
    while let Some(cur) = f {
        if Arc::ptr_eq(cur, old) {
            return true;
        }
        f = cur.below.as_ref();
    }
    false
}

fn set_strict() -> i64 {
    let me = task::current_tid();
    let pid = task::current_pid();
    {
        let mut threads = THREADS.lock();
        let s = threads.entry(me).or_insert_with(|| Sec { pid, ..Sec::default() });
        if matches!(s.mode, Some(Mode::Filtered(_))) {
            return -err::EINVAL;
        }
        s.mode = Some(Mode::Strict);
    }
    task::set_flags(me, FILTERED);
    0
}

/// A new filter for the calling thread (and with TSYNC for all the
/// process's threads, which must not have filters of their own the caller
/// lacks: the first that does is named in the result).
fn set_filter(flags: u64, ptr: u64) -> i64 {
    // NEW_LISTENER and WAIT_KILLABLE_RECV are for user notifications.
    if flags & !(FLAG_TSYNC | FLAG_LOG | FLAG_SPEC_ALLOW | FLAG_TSYNC_ESRCH) != 0 {
        return -err::EINVAL;
    }
    let prog = match read_prog(ptr) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let me = task::current_tid();
    let pid = task::current_pid();
    let others: Vec<Tid> = if flags & FLAG_TSYNC != 0 { user::threads().into_iter().filter(|&t| t != me).collect() } else { Vec::new() };
    let mut threads = THREADS.lock();
    let mine = threads.get(&me).cloned().unwrap_or_default();
    let below = match mine.mode {
        Some(Mode::Strict) => return -err::EINVAL,
        Some(Mode::Filtered(f)) => Some(f),
        None => None,
    };
    let total = prog.len() + 4 + below.as_ref().map_or(0, |f| f.total);
    if total > MAX_PATH {
        return -err::ENOMEM;
    }
    let top = Arc::new(Filter { prog, below, total });
    for &t in &others {
        let fits = match threads.get(&t).and_then(|s| s.mode.as_ref()) {
            None => true,
            Some(Mode::Strict) => false,
            Some(Mode::Filtered(f)) => is_below(f, &top),
        };
        if !fits {
            return if flags & FLAG_TSYNC_ESRCH != 0 { -err::ESRCH } else { i64::from(thread::linux_tid(t)) };
        }
    }
    for &t in others.iter().chain([&me]) {
        let s = threads.entry(t).or_insert_with(|| Sec { pid, ..Sec::default() });
        s.mode = Some(Mode::Filtered(top.clone()));
        s.no_new_privs |= mine.no_new_privs;
    }
    drop(threads);
    for &t in others.iter().chain([&me]) {
        task::set_flags(t, FILTERED);
    }
    let n = others.len() + 1;
    let what = if n == 1 { "thread" } else { "threads" };
    crate::debug!("linux", "{}: seccomp filter of {} instructions on {n} {what}", user::current_name(), top.prog.len());
    0
}

pub fn prctl(option: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> i64 {
    let me = task::current_tid();
    match option {
        // In strict mode prctl itself is not allowed: no need to say 1.
        PR_GET_SECCOMP => match THREADS.lock().get(&me).map(|s| s.mode.is_some()) {
            Some(true) => SECCOMP_MODE_FILTER as i64,
            _ => 0,
        },
        PR_SET_SECCOMP => match a2 {
            SECCOMP_MODE_STRICT => set_strict(),
            SECCOMP_MODE_FILTER => set_filter(0, a3),
            _ => -err::EINVAL,
        },
        PR_SET_NO_NEW_PRIVS if a2 == 1 && a3 | a4 | a5 == 0 => {
            let pid = task::current_pid();
            THREADS.lock().entry(me).or_insert_with(|| Sec { pid, ..Sec::default() }).no_new_privs = true;
            0
        }
        PR_GET_NO_NEW_PRIVS if a2 | a3 | a4 | a5 == 0 => i64::from(THREADS.lock().get(&me).is_some_and(|s| s.no_new_privs)),
        _ => -err::EINVAL,
    }
}

pub fn sys_seccomp(op: u64, flags: u64, args: u64) -> i64 {
    match op {
        SECCOMP_SET_MODE_STRICT if flags == 0 && args == 0 => set_strict(),
        SECCOMP_SET_MODE_FILTER => set_filter(flags, args),
        SECCOMP_GET_ACTION_AVAIL if flags == 0 => {
            let Ok(b) = user_bytes(args, 4) else { return -err::EFAULT };
            match u32::from_le_bytes([b[0], b[1], b[2], b[3]]) {
                RET_KILL_PROCESS | RET_KILL_THREAD | RET_TRAP | RET_ERRNO | RET_TRACE | RET_LOG | RET_ALLOW => 0,
                _ => -err::EOPNOTSUPP,
            }
        }
        _ => -err::EINVAL,
    }
}

// ----------------------------------------------------------------------------
// Threads and processes

/// A new thread, or a new process's first thread, made by the calling
/// thread: it gets the caller's filters and no_new_privs.
pub fn inherit(child: Tid, pid: u32) {
    let me = task::current_tid();
    let filtered = {
        let mut threads = THREADS.lock();
        let Some(mine) = threads.get(&me).cloned() else { return };
        let filtered = mine.mode.is_some();
        threads.insert(child, Sec { pid, ..mine });
        filtered
    };
    if filtered {
        task::set_flags(child, FILTERED);
    }
}

/// What /proc/<pid>/status says (0 nothing, 1 strict, 2 filters) of the
/// process's main thread, or of the calling thread for its own process.
pub fn mode_of(pid: u32) -> u32 {
    let tid = if pid == task::current_pid() { Some(task::current_tid()) } else { user::main_thread_of(pid) };
    let Some(tid) = tid else { return 0 };
    match THREADS.lock().get(&tid).and_then(|s| s.mode.as_ref()) {
        None => 0,
        Some(Mode::Strict) => SECCOMP_MODE_STRICT as u32,
        Some(Mode::Filtered(_)) => SECCOMP_MODE_FILTER as u32,
    }
}

pub fn thread_ended(tid: Tid) {
    THREADS.lock().remove(&tid);
}

pub fn forget(pid: u32) {
    THREADS.lock().retain(|_, s| s.pid != pid);
}

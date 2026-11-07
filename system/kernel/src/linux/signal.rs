//! Signals of Linux programs.
//!
//! A handler runs on the program's stack (or its alternate signal stack)
//! with the frame Linux builds there: the return address of the program's
//! restorer, a `ucontext` with the registers, the signal mask and the FPU
//! state, and the `siginfo`. `rt_sigreturn` puts the registers back from it,
//! with whatever changes the handler made.
//!
//! A signal reaches a thread when the thread returns to user mode, after a
//! system call or on a timer interrupt, and wakes it from a blocking call:
//! the call fails with EINTR, or starts again after the handler if the
//! handler asked for SA_RESTART. Faults (bad memory, invalid instructions,
//! division by zero) become SIGSEGV, SIGBUS, SIGILL, SIGFPE or SIGTRAP for
//! a program that handles them, a child's end sends SIGCHLD to its
//! parent, and a write to a pipe nobody reads sends SIGPIPE. Signals that nobody handles do what Linux does by default: end
//! the process, or nothing. There is no job control: SIGSTOP and SIGTSTP
//! do nothing.

use super::{err, try_state, user_bytes, with_state, with_state_of, LinuxState};
use crate::arch::{cpu, idt::TrapFrame};
use crate::task::{self, Tid};
use crate::user;
use alloc::collections::BTreeMap;
use core::sync::atomic::{AtomicUsize, Ordering};

const SIGHUP: u32 = 1;
const SIGINT: u32 = 2;
const SIGQUIT: u32 = 3;
const SIGILL: u32 = 4;
const SIGTRAP: u32 = 5;
const SIGABRT: u32 = 6;
const SIGBUS: u32 = 7;
const SIGFPE: u32 = 8;
const SIGKILL: u32 = 9;
const SIGSEGV: u32 = 11;
const SIGPIPE: u32 = 13;
const SIGTERM: u32 = 15;
const SIGCHLD: u32 = 17;
const SIGCONT: u32 = 18;
const SIGSTOP: u32 = 19;
const SIGTSTP: u32 = 20;
const SIGTTIN: u32 = 21;
const SIGTTOU: u32 = 22;
const SIGURG: u32 = 23;
const SIGWINCH: u32 = 28;

const SIG_DFL: u64 = 0;
const SIG_IGN: u64 = 1;
const SA_NOCLDWAIT: u64 = 2;
const SA_ONSTACK: u64 = 0x0800_0000;
const SA_RESTART: u64 = 0x1000_0000;
const SA_NODEFER: u64 = 0x4000_0000;
const SA_RESETHAND: u64 = 0x8000_0000;
const SS_ONSTACK: u32 = 1;
const SS_DISABLE: u32 = 2;
const MINSIGSTKSZ: u64 = 2048;

const SI_USER: i32 = 0;
const SI_KERNEL: i32 = 0x80;
const SI_TKILL: i32 = -6;
const CLD_EXITED: i32 = 1;
const CLD_KILLED: i32 = 2;

/// rt_sigsuspend, whose temporary mask goes back after the handler.
pub const SYS_RT_SIGSUSPEND: u64 = 130;

/// What a blocking call returns when a signal with a handler interrupts
/// it. The program never sees it: it becomes EINTR, or, with SA_RESTART,
/// the call starts again after the handler.
pub const ERESTART: i64 = 512;

const fn bit(sig: u32) -> u64 {
    1 << (sig - 1)
}

/// Signals that cannot be blocked or caught.
const UNBLOCKABLE: u64 = bit(SIGKILL) | bit(SIGSTOP);

/// sigaltstack: where a handler that asks for it (SA_ONSTACK) runs.
#[derive(Clone, Copy, Default)]
struct AltStack {
    sp: u64,
    flags: u32,
    size: u64,
}

impl AltStack {
    fn enabled(&self) -> bool {
        self.size != 0 && self.flags & SS_DISABLE == 0
    }

    fn contains(&self, sp: u64) -> bool {
        self.enabled() && sp > self.sp && sp <= self.sp + self.size
    }
}

/// A thread's signal setup.
#[derive(Clone, Copy, Default)]
pub struct ThreadSig {
    mask: u64,
    altstack: AltStack,
    /// The mask to put back after the handler that ends rt_sigsuspend.
    saved: Option<u64>,
}

/// A signal sent and not yet taken.
#[derive(Clone, Copy)]
pub struct Pending {
    sig: u32,
    /// For that thread only (tgkill), or for any thread of the process.
    tid: Option<Tid>,
    /// The start of its siginfo (the rest is 0).
    info: [u8; 32],
}

/// Signals waiting in all processes: while there are none, returning to
/// user mode does not look.
static QUEUED: AtomicUsize = AtomicUsize::new(0);

fn info(sig: u32, code: i32, pid: u32) -> [u8; 32] {
    let mut b = [0; 32];
    b[0..4].copy_from_slice(&sig.to_le_bytes());
    b[8..12].copy_from_slice(&code.to_le_bytes());
    b[16..20].copy_from_slice(&pid.to_le_bytes());
    b
}

/// A signal's sigaction, as the kernel keeps it.
#[derive(Clone, Copy)]
struct Act {
    handler: u64,
    flags: u64,
    restorer: u64,
    mask: u64,
}

fn act(st: &LinuxState, sig: u32) -> Act {
    let a = &st.sigactions[sig as usize];
    let w = |i: usize| u64::from_le_bytes(a[i * 8..i * 8 + 8].try_into().unwrap());
    Act { handler: w(0), flags: w(1), restorer: w(2), mask: w(3) }
}

#[derive(Clone, Copy, PartialEq)]
enum Action {
    Ignore,
    Terminate,
    Handle,
}

fn action(st: &LinuxState, sig: u32) -> Action {
    match sig {
        SIGKILL => return Action::Terminate,
        SIGSTOP => return Action::Ignore,
        _ => {}
    }
    match act(st, sig).handler {
        SIG_IGN => Action::Ignore,
        SIG_DFL if matches!(sig, SIGCHLD | SIGCONT | SIGURG | SIGWINCH | SIGTSTP | SIGTTIN | SIGTTOU) => Action::Ignore,
        SIG_DFL => Action::Terminate,
        _ => Action::Handle,
    }
}

fn mask_of(st: &LinuxState, tid: Tid) -> u64 {
    st.sigthreads.get(&tid).map_or(0, |t| t.mask)
}

fn thread(st: &mut LinuxState, tid: Tid) -> &mut ThreadSig {
    st.sigthreads.entry(tid).or_default()
}

/// Can thread `me` take `p` now?
fn takes(st: &LinuxState, me: Tid, p: &Pending) -> bool {
    p.tid.is_none_or(|t| t == me) && mask_of(st, me) & !UNBLOCKABLE & bit(p.sig) == 0
}

fn remove(st: &mut LinuxState, i: usize) -> Pending {
    QUEUED.fetch_sub(1, Ordering::Relaxed);
    st.pending.remove(i)
}

fn remove_where(st: &mut LinuxState, mut gone: impl FnMut(&Pending) -> bool) {
    let before = st.pending.len();
    st.pending.retain(|p| !gone(p));
    QUEUED.fetch_sub(before - st.pending.len(), Ordering::Relaxed);
}

fn queue(st: &mut LinuxState, sig: u32, tid: Option<Tid>, info: [u8; 32]) {
    let b = bit(sig);
    // An ignored signal is dropped, unless it is blocked: the program may
    // be about to take it with sigtimedwait.
    let blocked = match tid {
        Some(t) => mask_of(st, t) & b != 0,
        None => st.sigthreads.values().any(|t| t.mask & b != 0),
    };
    if action(st, sig) == Action::Ignore && !blocked {
        return;
    }
    // Standard signals do not queue up; real-time ones (32 and up) do.
    if sig < 32 && st.pending.iter().any(|p| p.sig == sig && p.tid == tid) || st.pending.len() >= 1024 {
        return;
    }
    st.pending.push(Pending { sig, tid, info });
    QUEUED.fetch_add(1, Ordering::Relaxed);
}

/// Sends `sig` to process `pid`, or to one of its threads. False if there
/// is no such process.
fn send(pid: u32, tid: Option<Tid>, sig: u32, info: [u8; 32]) -> bool {
    if !user::is_running(pid) {
        return false;
    }
    if sig == 0 {
        return true;
    }
    if sig == SIGKILL {
        user::kill_one(pid);
        return true;
    }
    if with_state_of(pid, |st| queue(st, sig, tid, info)).is_none() {
        // A JustOS program: only the signals that end a process mean
        // something to it.
        if matches!(sig, SIGHUP | SIGINT | SIGQUIT | SIGTERM) {
            user::kill_one(pid);
        }
        return true;
    }
    // A thread waiting for a child should see the signal.
    user::wake_waiters();
    true
}

/// Ctrl+C for a Linux program: one that handles SIGINT gets it, one that
/// ignores it carries on (true for both). False: it should stop.
pub fn interrupt(pid: u32) -> bool {
    with_state_of(pid, |st| match action(st, SIGINT) {
        Action::Terminate => false,
        _ => {
            queue(st, SIGINT, None, info(SIGINT, SI_KERNEL, 0));
            true
        }
    })
    .unwrap_or(false)
}

/// A child of `parent` ended with `code`: SIGCHLD for the parent. True if
/// the parent asked not to wait for its children (SIGCHLD ignored, or
/// SA_NOCLDWAIT): the child goes right away.
pub fn child_ended(parent: u32, child: u32, code: i64) -> bool {
    let killed = code < -128;
    let mut i = info(SIGCHLD, if killed { CLD_KILLED } else { CLD_EXITED }, child);
    let status = if killed { (-code - 128) as u32 } else { code as u32 & 0xff };
    i[24..28].copy_from_slice(&status.to_le_bytes());
    with_state_of(parent, |st| {
        let a = act(st, SIGCHLD);
        queue(st, SIGCHLD, None, i);
        a.handler == SIG_IGN || a.flags & SA_NOCLDWAIT != 0
    })
    .unwrap_or(false)
}

/// A write to a pipe or socket that nobody reads any more: SIGPIPE for
/// the writing thread (by default it ends the process; the write fails
/// with EPIPE).
pub fn broken_pipe() {
    let me = task::current_tid();
    let pid = task::current_pid();
    try_state(|st| queue(st, SIGPIPE, Some(me), info(SIGPIPE, SI_USER, pid)));
}

// ----------------------------------------------------------------------------
// Taking signals

enum Check {
    Clear,
    /// A handler is to run.
    Run,
    /// The process ends, with this code.
    End(i64),
}

/// What waits for the calling thread. Ignored signals in the way go.
fn check() -> Check {
    if user::kill_requested() {
        return Check::End(-130);
    }
    if QUEUED.load(Ordering::Relaxed) == 0 {
        return Check::Clear;
    }
    let me = task::current_tid();
    try_state(|st| loop {
        let Some(i) = st.pending.iter().position(|p| takes(st, me, p)) else { return Check::Clear };
        let sig = st.pending[i].sig;
        match action(st, sig) {
            Action::Ignore => {
                remove(st, i);
            }
            Action::Terminate => return Check::End(-(128 + sig as i64)),
            Action::Handle => return Check::Run,
        }
    })
    .unwrap_or(Check::Clear)
}

fn end(code: i64) -> ! {
    if code == -(128 + SIGABRT as i64) {
        crate::println!("\x1b[91m{}: aborted\x1b[0m", user::current_name());
    }
    user::terminate(code)
}

/// For blocking calls, while they wait: ends the process if it is being
/// stopped or a signal ends it (after `before_end`), and returns the error
/// the call should fail with when a handler is to run.
pub fn interrupted_then(before_end: impl FnOnce()) -> Option<i64> {
    match check() {
        Check::Clear => None,
        Check::Run => Some(-ERESTART),
        Check::End(code) => {
            before_end();
            end(code)
        }
    }
}

pub fn interrupted() -> Option<i64> {
    interrupted_then(|| {})
}

/// Whether a signal (or a stop) is waiting for the calling thread, without
/// acting on it.
pub fn pending_here() -> bool {
    !matches!(check(), Check::Clear)
}

/// Before a thread of a Linux program returns to user mode: takes the
/// first signal it can and runs its handler, or its default action.
/// `syscall` is the system call returning (its result in `rax`), None
/// after an interrupt or rt_sigreturn.
pub fn deliver(frame: &mut TrapFrame, syscall: Option<u64>) {
    let restarting = syscall.is_some() && frame.rax as i64 == -ERESTART;
    if QUEUED.load(Ordering::Relaxed) == 0 && !restarting && syscall != Some(SYS_RT_SIGSUSPEND) {
        return;
    }
    enum Step {
        Nothing,
        End(i64),
        Run(Pending, Act, u64, AltStack),
    }
    let me = task::current_tid();
    let step = try_state(|st| loop {
        let Some(i) = st.pending.iter().position(|p| takes(st, me, p)) else {
            // Nothing to run: a mask rt_sigsuspend set goes back.
            if let Some(t) = st.sigthreads.get_mut(&me) {
                if let Some(m) = t.saved.take() {
                    t.mask = m;
                }
            }
            return Step::Nothing;
        };
        let sig = st.pending[i].sig;
        match action(st, sig) {
            Action::Ignore => {
                remove(st, i);
            }
            Action::Terminate => return Step::End(-(128 + sig as i64)),
            Action::Handle => {
                let p = remove(st, i);
                let a = act(st, sig);
                let t = thread(st, me);
                let now = t.mask;
                let restore = t.saved.take().unwrap_or(now);
                let alt = t.altstack;
                // The handler runs with its signal and its sa_mask blocked.
                t.mask = (now | a.mask | if a.flags & SA_NODEFER != 0 { 0 } else { bit(sig) }) & !UNBLOCKABLE;
                if a.flags & SA_RESETHAND != 0 {
                    st.sigactions[sig as usize] = [0; 32];
                }
                return Step::Run(p, a, restore, alt);
            }
        }
    })
    .unwrap_or(Step::Nothing);
    // Back to the `syscall` instruction, with the call number in rax.
    let restart = |frame: &mut TrapFrame| {
        if let Some(n) = syscall {
            frame.rax = n;
            frame.rip -= 2;
        }
    };
    match step {
        Step::Nothing => {
            if restarting {
                restart(frame);
            }
        }
        Step::End(code) => end(code),
        Step::Run(p, a, restore, alt) => {
            if restarting {
                if a.flags & SA_RESTART != 0 {
                    restart(frame);
                } else {
                    frame.rax = (-err::EINTR) as u64;
                }
            }
            if setup_frame(frame, &p, &a, restore, alt, None).is_err() {
                crate::println!("\x1b[91m{}: no room on the stack for a signal\x1b[0m", user::current_name());
                user::terminate(-(128 + SIGSEGV as i64));
            }
        }
    }
}

/// A timer interrupt hit user mode: a signal waiting for this thread runs
/// now (a program in a loop that makes no system calls gets it too).
pub fn at_interrupt(frame: &mut TrapFrame) {
    if QUEUED.load(Ordering::Relaxed) == 0 {
        return;
    }
    cpu::enable_irq();
    deliver(frame, None);
    cpu::disable_irq();
}

/// A fault in user mode, for a Linux program that handles the signal it
/// becomes: true when its handler is set to run. False otherwise (no
/// handler, the signal blocked, not a Linux program): the program ends.
pub fn fault(frame: &mut TrapFrame, cr2: u64) -> bool {
    let (sig, code) = match frame.vector {
        0 => (SIGFPE, 1),       // FPE_INTDIV
        1 | 3 => (SIGTRAP, 1),  // TRAP_BRKPT
        6 => (SIGILL, 2),       // ILL_ILLOPN
        12 => (SIGBUS, SI_KERNEL),
        13 => (SIGSEGV, SI_KERNEL),
        14 => (SIGSEGV, if frame.error & 1 != 0 { 2 } else { 1 }), // SEGV_ACCERR, SEGV_MAPERR
        16 | 19 => (SIGFPE, 0),
        17 => (SIGBUS, 1), // BUS_ADRALN
        _ => return false,
    };
    let me = task::current_tid();
    let ready = try_state(|st| {
        if action(st, sig) != Action::Handle || mask_of(st, me) & bit(sig) != 0 {
            return None;
        }
        let a = act(st, sig);
        let t = thread(st, me);
        let restore = t.mask;
        let alt = t.altstack;
        t.mask = (restore | a.mask | if a.flags & SA_NODEFER != 0 { 0 } else { bit(sig) }) & !UNBLOCKABLE;
        if a.flags & SA_RESETHAND != 0 {
            st.sigactions[sig as usize] = [0; 32];
        }
        Some((a, restore, alt))
    });
    let Some(Some((a, restore, alt))) = ready else { return false };
    let mut i = info(sig, code, 0);
    let addr = if frame.vector == 14 { cr2 } else { frame.rip };
    i[16..24].copy_from_slice(&addr.to_le_bytes());
    let p = Pending { sig, tid: Some(me), info: i };
    let trap = (frame.vector, frame.error, cr2);
    setup_frame(frame, &p, &a, restore, alt, Some(trap)).is_ok()
}

// ----------------------------------------------------------------------------
// The signal frame

/// Offsets in the frame: the restorer's address, then the ucontext (and
/// in it the registers), then the siginfo. The FPU state goes above.
const UC: u64 = 8;
const MC: u64 = UC + 40;
const INFO: u64 = 312;
const FRAME: u64 = 440;
const FPU: u64 = 512;

/// Builds the frame of `p`'s handler and points the registers at it.
fn setup_frame(frame: &mut TrapFrame, p: &Pending, a: &Act, restore: u64, alt: AltStack, trap: Option<(u64, u64, u64)>) -> Result<(), i64> {
    let on_alt = alt.contains(frame.rsp);
    let top = if a.flags & SA_ONSTACK != 0 && alt.enabled() && !on_alt {
        alt.sp + alt.size
    } else {
        frame.rsp.wrapping_sub(128) // below the red zone
    };
    let fp = top.checked_sub(FPU).ok_or(-err::EFAULT)? & !63;
    // As if called: rsp + 8 is 16-byte aligned at the handler's entry.
    let base = (fp.checked_sub(FRAME + 8).ok_or(-err::EFAULT)? & !15) + 8;
    let len = fp + FPU - base;
    super::mm::populate(base, len);
    let b = user_bytes(base, len)?;
    b.fill(0);
    let put = |b: &mut [u8], at: u64, v: u64| b[at as usize..at as usize + 8].copy_from_slice(&v.to_le_bytes());
    put(b, 0, a.restorer);
    // uc_stack
    put(b, UC + 16, alt.sp);
    let ss_flags = if !alt.enabled() {
        SS_DISABLE
    } else if on_alt {
        SS_ONSTACK
    } else {
        0
    };
    b[(UC + 24) as usize..(UC + 28) as usize].copy_from_slice(&ss_flags.to_le_bytes());
    put(b, UC + 32, alt.size);
    // uc_mcontext, in the order of glibc's gregs
    let f = &*frame;
    let regs = [f.r8, f.r9, f.r10, f.r11, f.r12, f.r13, f.r14, f.r15, f.rdi, f.rsi, f.rbp, f.rbx, f.rdx, f.rax, f.rcx, f.rsp, f.rip, f.rflags];
    for (i, r) in regs.iter().enumerate() {
        put(b, MC + i as u64 * 8, *r);
    }
    b[(MC + 144) as usize..(MC + 146) as usize].copy_from_slice(&(f.cs as u16).to_le_bytes());
    b[(MC + 150) as usize..(MC + 152) as usize].copy_from_slice(&(f.ss as u16).to_le_bytes());
    if let Some((trapno, error, cr2)) = trap {
        put(b, MC + 152, error);
        put(b, MC + 160, trapno);
        put(b, MC + 176, cr2);
    }
    put(b, MC + 168, restore); // oldmask
    put(b, MC + 184, fp); // fpstate
    put(b, UC + 296, restore); // uc_sigmask
    b[INFO as usize..INFO as usize + 32].copy_from_slice(&p.info);
    // SAFETY: `fp` is 64-byte aligned and its 512 bytes are mapped user
    // memory (checked above). The FPU holds this thread's user state: the
    // kernel does not use it.
    unsafe { core::arch::asm!("fxsave64 [{}]", in(reg) fp, options(nostack, preserves_flags)) };
    frame.rsp = base;
    frame.rip = a.handler;
    frame.rdi = p.sig as u64;
    frame.rsi = base + INFO;
    frame.rdx = base + UC;
    frame.rax = 0;
    frame.rflags &= !(0x100 | 0x400 | 0x1_0000); // TF, DF, RF
    Ok(())
}

#[repr(C, align(16))]
struct FxArea([u8; 512]);

/// rt_sigreturn: the registers, mask and FPU state saved in the frame of
/// the handler that is returning (its restorer's `syscall` runs with the
/// stack pointer just past the return address, at the ucontext).
pub fn sys_rt_sigreturn(frame: &mut TrapFrame) {
    let Ok(b) = user_bytes(frame.rsp, 304) else { end(-(128 + SIGSEGV as i64)) };
    let r = |i: usize| u64::from_le_bytes(b[40 + i * 8..48 + i * 8].try_into().unwrap());
    let (rip, rsp) = (r(16), r(15));
    if rip >= user::USER_TOP || rsp >= user::USER_TOP {
        end(-(128 + SIGSEGV as i64));
    }
    frame.r8 = r(0);
    frame.r9 = r(1);
    frame.r10 = r(2);
    frame.r11 = r(3);
    frame.r12 = r(4);
    frame.r13 = r(5);
    frame.r14 = r(6);
    frame.r15 = r(7);
    frame.rdi = r(8);
    frame.rsi = r(9);
    frame.rbp = r(10);
    frame.rbx = r(11);
    frame.rdx = r(12);
    frame.rax = r(13);
    frame.rcx = r(14);
    frame.rsp = rsp;
    frame.rip = rip;
    // Only the flags a program may change (arithmetic, direction, trap).
    const USER_FLAGS: u64 = 0x5_0DD5;
    frame.rflags = (frame.rflags & !USER_FLAGS) | (r(17) & USER_FLAGS);
    let fp = r(23);
    let mask = u64::from_le_bytes(b[296..304].try_into().unwrap());
    let me = task::current_tid();
    try_state(|st| thread(st, me).mask = mask & !UNBLOCKABLE);
    if fp != 0 {
        if let Ok(src) = user_bytes(fp, FPU) {
            let mut area = FxArea([0; 512]);
            area.0.copy_from_slice(src);
            // Reserved MXCSR bits would make fxrstor fault in the kernel.
            let mxcsr = u32::from_le_bytes(area.0[24..28].try_into().unwrap()) & 0xffff;
            area.0[24..28].copy_from_slice(&mxcsr.to_le_bytes());
            // SAFETY: a 16-byte aligned fxsave image with a valid MXCSR.
            unsafe { core::arch::asm!("fxrstor64 [{}]", in(reg) &area as *const FxArea, options(nostack, preserves_flags)) };
        }
    }
}

// ----------------------------------------------------------------------------
// System calls

fn read_set(ptr: u64) -> Result<u64, i64> {
    let b = user_bytes(ptr, 8)?;
    Ok(u64::from_le_bytes(b[..8].try_into().unwrap()))
}

pub fn sys_rt_sigaction(sig: u64, new: u64, old: u64, size: u64) -> i64 {
    if sig == 0 || sig > 64 || size != 8 {
        return -err::EINVAL;
    }
    let s = sig as u32;
    if new != 0 && (s == SIGKILL || s == SIGSTOP) {
        return -err::EINVAL;
    }
    let prev = with_state(|st| st.sigactions[sig as usize]);
    if old != 0 {
        match user_bytes(old, 32) {
            Ok(b) => b.copy_from_slice(&prev),
            Err(e) => return e,
        }
    }
    if new != 0 {
        let new: [u8; 32] = match user_bytes(new, 32) {
            Ok(b) => b[..32].try_into().unwrap(),
            Err(e) => return e,
        };
        with_state(|st| {
            st.sigactions[sig as usize] = new;
            // A signal set to be ignored is dropped from the queue.
            if action(st, s) == Action::Ignore {
                remove_where(st, |p| p.sig == s);
            }
        });
    }
    0
}

pub fn sys_rt_sigprocmask(how: u64, set: u64, old: u64) -> i64 {
    let me = task::current_tid();
    let prev = with_state(|st| mask_of(st, me));
    if old != 0 {
        match user_bytes(old, 8) {
            Ok(b) => b.copy_from_slice(&prev.to_le_bytes()),
            Err(e) => return e,
        }
    }
    if set != 0 {
        let v = match read_set(set) {
            Ok(v) => v,
            Err(e) => return e,
        };
        let new = match how {
            0 => prev | v,
            1 => prev & !v,
            2 => v,
            _ => return -err::EINVAL,
        };
        // Signals it unblocks are delivered on the way out of this call.
        with_state(|st| thread(st, me).mask = new & !UNBLOCKABLE);
    }
    0
}

pub fn sys_rt_sigpending(set: u64) -> i64 {
    let me = task::current_tid();
    let bits = with_state(|st| st.pending.iter().filter(|p| p.tid.is_none_or(|t| t == me)).fold(0, |m, p| m | bit(p.sig)));
    match user_bytes(set, 8) {
        Ok(b) => {
            b.copy_from_slice(&bits.to_le_bytes());
            0
        }
        Err(e) => e,
    }
}

/// rt_sigsuspend: waits with a temporary mask until a handler is to run.
pub fn sys_rt_sigsuspend(set: u64) -> i64 {
    let new = match read_set(set) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let me = task::current_tid();
    with_state(|st| {
        let t = thread(st, me);
        t.saved = Some(t.mask);
        t.mask = new & !UNBLOCKABLE;
    });
    wait_for_handler()
}

pub fn sys_pause() -> i64 {
    wait_for_handler()
}

fn wait_for_handler() -> i64 {
    loop {
        if interrupted().is_some() {
            return -err::EINTR;
        }
        task::sleep_ms(10);
    }
}

/// rt_sigtimedwait: takes one of the signals in `set` (usually blocked)
/// without running a handler.
pub fn sys_rt_sigtimedwait(set: u64, info_ptr: u64, ts: u64) -> i64 {
    let want = match read_set(set) {
        Ok(v) => v & !UNBLOCKABLE,
        Err(e) => return e,
    };
    let deadline = if ts == 0 {
        None
    } else {
        let Ok(b) = user_bytes(ts, 16) else { return -err::EFAULT };
        let s = i64::from_le_bytes(b[0..8].try_into().unwrap()).max(0) as u64;
        let ns = i64::from_le_bytes(b[8..16].try_into().unwrap()).max(0) as u64;
        Some(crate::time::uptime_ms().saturating_add(s.saturating_mul(1000)).saturating_add(ns / 1_000_000))
    };
    let me = task::current_tid();
    loop {
        let got = try_state(|st| {
            let i = st.pending.iter().position(|p| p.tid.is_none_or(|t| t == me) && want & bit(p.sig) != 0)?;
            Some(remove(st, i))
        })
        .flatten();
        if let Some(p) = got {
            if info_ptr != 0 {
                if let Ok(b) = user_bytes(info_ptr, 128) {
                    b.fill(0);
                    b[..32].copy_from_slice(&p.info);
                }
            }
            return p.sig as i64;
        }
        if interrupted().is_some() {
            return -err::EINTR;
        }
        if deadline.is_some_and(|d| crate::time::uptime_ms() >= d) {
            return -err::EAGAIN;
        }
        task::sleep_ms(5);
    }
}

pub fn sys_sigaltstack(ss: u64, old: u64) -> i64 {
    let me = task::current_tid();
    let cur = with_state(|st| thread(st, me).altstack);
    if old != 0 {
        let Ok(b) = user_bytes(old, 24) else { return -err::EFAULT };
        b.fill(0);
        b[0..8].copy_from_slice(&cur.sp.to_le_bytes());
        let flags = if cur.enabled() { 0 } else { SS_DISABLE };
        b[8..12].copy_from_slice(&flags.to_le_bytes());
        b[16..24].copy_from_slice(&cur.size.to_le_bytes());
    }
    if ss != 0 {
        let Ok(b) = user_bytes(ss, 24) else { return -err::EFAULT };
        let sp = u64::from_le_bytes(b[0..8].try_into().unwrap());
        let flags = u32::from_le_bytes(b[8..12].try_into().unwrap());
        let size = u64::from_le_bytes(b[16..24].try_into().unwrap());
        let new = if flags & SS_DISABLE != 0 {
            AltStack { sp: 0, flags: SS_DISABLE, size: 0 }
        } else if size < MINSIGSTKSZ {
            return -err::ENOMEM;
        } else {
            AltStack { sp, flags: 0, size }
        };
        with_state(|st| thread(st, me).altstack = new);
    }
    0
}

pub fn sys_kill(pid: i64, sig: u64) -> i64 {
    if sig > 64 {
        return -err::EINVAL;
    }
    let me = task::current_pid();
    let target = match pid {
        0 => me,
        // Every process: not something a program here gets to do.
        -1 => return 0,
        // A process group: the one led by that process.
        p if p < 0 => p.unsigned_abs() as u32,
        p => p as u32,
    };
    if send(target, None, sig as u32, info(sig as u32, SI_USER, me)) {
        0
    } else {
        -err::ESRCH
    }
}

/// tgkill and tkill (`tgid` 0): a signal for one thread.
pub fn sys_tgkill(tgid: i64, ltid: i64, sig: u64) -> i64 {
    if sig > 64 || ltid <= 0 {
        return -err::EINVAL;
    }
    let Some((pid, tid)) = thread_of(ltid as u32) else { return -err::ESRCH };
    if tgid > 0 && tgid as u32 != pid {
        return -err::ESRCH;
    }
    if send(pid, Some(tid), sig as u32, info(sig as u32, SI_TKILL, task::current_pid())) {
        0
    } else {
        -err::ESRCH
    }
}

/// A Linux thread id's process and kernel thread (see `thread::linux_tid`).
fn thread_of(ltid: u32) -> Option<(u32, Tid)> {
    if ltid > 100_000 {
        let t = ltid - 100_000;
        Some((task::pid_of(t)?, t))
    } else {
        Some((ltid, user::main_thread_of(ltid)?))
    }
}

// ----------------------------------------------------------------------------
// Threads and processes

/// A new thread starts with its creator's mask and no alternate stack.
pub fn thread_started(tid: Tid) {
    let me = task::current_tid();
    with_state(|st| {
        let mask = mask_of(st, me);
        st.sigthreads.insert(tid, ThreadSig { mask, ..Default::default() });
    });
}

/// A thread ends: signals for it alone go with it.
pub fn thread_ended(st: &mut LinuxState, tid: Tid) {
    st.sigthreads.remove(&tid);
    remove_where(st, |p| p.tid == Some(tid));
}

/// A process's Linux state goes, with the signals it had not taken.
pub fn forgotten(st: &LinuxState) {
    QUEUED.fetch_sub(st.pending.len(), Ordering::Relaxed);
}

/// The signal setup of a fork child's thread `child`: the caller's mask
/// and alternate stack. Pending signals stay with the parent.
pub fn forked(st: &LinuxState, me: Tid, child: Tid) -> BTreeMap<Tid, ThreadSig> {
    let mut t = st.sigthreads.get(&me).copied().unwrap_or_default();
    t.saved = None;
    BTreeMap::from([(child, t)])
}

/// execve: the mask stays, the alternate stack goes.
pub fn exec(st: &mut LinuxState, me: Tid) {
    let mask = mask_of(st, me);
    st.sigthreads.clear();
    st.sigthreads.insert(me, ThreadSig { mask, ..Default::default() });
}

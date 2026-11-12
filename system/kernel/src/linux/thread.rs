//! Threads and processes of Linux programs: `clone` for threads sharing the
//! address space (as pthreads and the Go and Rust runtimes create them) and
//! for new processes (fork, vfork, posix_spawn), futexes, and thread exit
//! with its `clear_child_tid` wake-up. Signals are in [`super::signal`].

use super::{err, ns, user_bytes, with_state};
use crate::arch::cpu;
use crate::arch::idt::TrapFrame;
use crate::mem::paging;
use crate::sync::IrqLock;
use crate::task::{self, Tid};
use crate::user;
use alloc::vec::Vec;

const CLONE_VM: u64 = 0x100;
const CLONE_VFORK: u64 = 0x4000;
const CLONE_THREAD: u64 = 0x10000;
const CLONE_SETTLS: u64 = 0x80000;
const CLONE_PARENT_SETTID: u64 = 0x100000;
const CLONE_CHILD_CLEARTID: u64 = 0x200000;
const CLONE_CHILD_SETTID: u64 = 0x1000000;

/// Linux thread ids: the main thread's is the process id; the others get
/// their kernel id moved out of the range of process ids.
pub fn linux_tid(t: Tid) -> u32 {
    if user::main_thread() == Some(t) {
        task::current_pid()
    } else {
        t + 100_000
    }
}

pub fn sys_gettid() -> i64 {
    ns::local(linux_tid(task::current_tid())) as i64
}

pub fn sys_clone(frame: &TrapFrame, flags: u64, stack: u64, ptid: u64, ctid: u64, tls: u64) -> i64 {
    if flags & CLONE_THREAD == 0 {
        return clone_process(frame, flags, stack, ptid, ctid, tls);
    }
    // A thread lives in its process's namespaces.
    if flags & ns::CLONE_NAMESPACES != 0 {
        return -err::EINVAL;
    }
    if flags & CLONE_VM == 0 {
        return -err::EINVAL; // a thread shares the memory
    }
    let mut child = *frame;
    child.rax = 0;
    if stack != 0 {
        child.rsp = stack;
    }
    let fs = if flags & CLONE_SETTLS != 0 { tls } else { crate::arch::syscall::user_bases().0 };
    let tid = match user::spawn_thread(child, fs) {
        Ok(t) => t,
        Err(e) => return -super::errno(e),
    };
    super::signal::thread_started(tid);
    super::seccomp::inherit(tid, task::current_pid());
    ns::thread_started(tid + 100_000);
    // The id the process sees.
    let lt = ns::local(tid + 100_000);
    if flags & CLONE_PARENT_SETTID != 0 {
        super::mm::populate(ptid, 4);
        if let Ok(b) = user_bytes(ptid, 4) {
            b.copy_from_slice(&lt.to_le_bytes());
        }
    }
    if flags & CLONE_CHILD_SETTID != 0 {
        super::mm::populate(ctid, 4);
        if let Ok(b) = user_bytes(ctid, 4) {
            b.copy_from_slice(&lt.to_le_bytes());
        }
    }
    if flags & CLONE_CHILD_CLEARTID != 0 {
        with_state(|st| {
            st.clear_tid.insert(tid, ctid);
        });
    }
    // The child may now run.
    task::wake_new(tid);
    lt as i64
}

/// The signal a child sends its parent when it ends.
const SIGCHLD: u64 = 17;

pub fn sys_fork(frame: &TrapFrame) -> i64 {
    clone_process(frame, SIGCHLD, 0, 0, 0, 0)
}

pub fn sys_vfork(frame: &TrapFrame) -> i64 {
    clone_process(frame, CLONE_VM | CLONE_VFORK | SIGCHLD, 0, 0, 0, 0)
}

/// A new process. Without CLONE_VM (fork) it gets a copy of the memory;
/// with it (vfork, posix_spawn) it runs in the caller's memory, on its own
/// stack, and with CLONE_VFORK the caller waits until it execs or ends.
/// It can have namespaces of its own ([`ns`]), and share the caller's root
/// and current folder (CLONE_FS).
fn clone_process(frame: &TrapFrame, flags: u64, stack: u64, ptid: u64, ctid: u64, tls: u64) -> i64 {
    if let Err(e) = ns::check_clone(flags) {
        return e;
    }
    let share = flags & CLONE_VM != 0;
    let mut child = *frame;
    child.rax = 0;
    if stack != 0 {
        child.rsp = stack;
    }
    let fs = if flags & CLONE_SETTLS != 0 { tls } else { crate::arch::syscall::user_bases().0 };
    let (pid, tid) = match user::fork(child, fs, share, flags & ns::CLONE_FS != 0) {
        Ok(v) => v,
        Err(e) => return -super::errno(e),
    };
    super::fork_state(pid, tid);
    super::seccomp::inherit(tid, pid);
    ns::forked(pid, flags);
    // The child's main thread has the process's id as its thread id: as
    // the caller sees it, and as the child does (1 in a new PID namespace).
    let id = ns::local(pid).to_le_bytes();
    if flags & CLONE_PARENT_SETTID != 0 {
        super::mm::populate(ptid, 4);
        if let Ok(b) = user_bytes(ptid, 4) {
            b.copy_from_slice(&id);
        }
    }
    if flags & CLONE_CHILD_SETTID != 0 {
        let own = ns::local_in(pid, pid).to_le_bytes();
        if share {
            super::mm::populate(ctid, 4);
            if let Ok(b) = user_bytes(ctid, 4) {
                b.copy_from_slice(&own);
            }
        } else {
            user::write_in(pid, ctid, &own);
        }
    }
    if flags & CLONE_CHILD_CLEARTID != 0 {
        super::with_state_of(pid, |st| {
            st.clear_tid.insert(tid, ctid);
        });
    }
    task::wake_new(tid);
    if flags & CLONE_VFORK != 0 {
        user::wait_vfork(pid);
    }
    ns::local(pid) as i64
}

pub fn sys_set_tid_address(ptr: u64) -> i64 {
    let me = task::current_tid();
    with_state(|st| {
        st.clear_tid.insert(me, ptr);
    });
    ns::local(linux_tid(me)) as i64
}

/// The calling thread ends (exit); the process goes on unless it was the
/// last one.
pub fn sys_exit(code: i64) -> ! {
    let me = task::current_tid();
    let ctid = with_state(|st| {
        super::signal::thread_ended(st, me);
        st.clear_tid.remove(&me)
    });
    super::seccomp::thread_ended(me);
    if user::main_thread() != Some(me) {
        ns::thread_ended(me + 100_000);
    }
    if let Some(ctid) = ctid {
        if ctid != 0 {
            if let Ok(b) = user_bytes(ctid, 4) {
                b.copy_from_slice(&0u32.to_le_bytes());
            }
            futex_wake(futex_key(ctid, true), 1, u32::MAX);
        }
    }
    user::exit_thread(code)
}

// ----------------------------------------------------------------------------
// Futexes

struct Waiter {
    key: Key,
    tid: Tid,
    bitset: u32,
    woken: bool,
}

static WAITERS: IrqLock<Vec<Waiter>> = IrqLock::new(Vec::new());

const FUTEX_WAIT: u64 = 0;
const FUTEX_WAKE: u64 = 1;
const FUTEX_REQUEUE: u64 = 3;
const FUTEX_CMP_REQUEUE: u64 = 4;
const FUTEX_WAKE_OP: u64 = 5;
const FUTEX_WAIT_BITSET: u64 = 9;
const FUTEX_WAKE_BITSET: u64 = 10;
const FUTEX_PRIVATE: u64 = 128;
const FUTEX_CLOCK_REALTIME: u64 = 256;

/// What a futex word is known by. A private one (or any word of memory a
/// process has to itself): its address space and address. A shared one in
/// memory that processes share: the frame and the offset in it, which every
/// process mapping that memory sees the same.
type Key = (u64, u64);

fn futex_key(addr: u64, shared: bool) -> Key {
    let cr3 = cpu::read_cr3();
    if shared {
        super::mm::populate(addr, 4);
        let page = addr & !0xfff;
        if let Some(e) = paging::entry_in(cr3, page) {
            // Read-only shared frames are files' pages: a private
            // mapping's, unless the area is a shared one.
            if e & paging::SHARED != 0 && (e & paging::WRITE != 0 || super::mm::shared_at(addr)) {
                return (0, (e & paging::ADDR_MASK) | (addr & 0xfff));
            }
        }
    }
    (cr3, addr)
}

fn read_u32(addr: u64) -> Result<u32, i64> {
    super::mm::populate(addr, 4);
    let b = user_bytes(addr, 4)?;
    Ok(u32::from_le_bytes(b[..4].try_into().unwrap()))
}

fn futex_wake(key: Key, n: u32, bitset: u32) -> i64 {
    let mut count = 0;
    {
        let mut w = WAITERS.lock();
        for x in w.iter_mut() {
            if count >= n {
                break;
            }
            if !x.woken && x.key == key && x.bitset & bitset != 0 {
                x.woken = true;
                count += 1;
                // Under the lock: the waiter is still waiting (it leaves
                // the list under it), so the wake-up cannot reach it later
                // in some other wait.
                task::wake(x.tid);
            }
        }
    }
    count as i64
}

/// Waits while `*addr == val`; `deadline` is an uptime in ms.
fn futex_wait(addr: u64, shared: bool, val: u32, deadline: Option<u64>, bitset: u32) -> i64 {
    if bitset == 0 {
        return -err::EINVAL;
    }
    super::mm::populate(addr, 4);
    let key = futex_key(addr, shared);
    let me = task::current_tid();
    {
        // Compare and queue under the lock futex_wake takes: a thread that
        // changes the word and then wakes (on any processor) either changed
        // it before the compare or finds this one queued, so no wake-up is
        // lost.
        let mut w = WAITERS.lock();
        let cur = match user_bytes(addr, 4) {
            Ok(b) => u32::from_le_bytes(b[..4].try_into().unwrap()),
            Err(e) => return e,
        };
        if cur != val {
            return -err::EAGAIN;
        }
        w.push(Waiter { key, tid: me, bitset, woken: false });
    }
    let woken = |me: Tid| WAITERS.lock().iter().any(|x| x.tid == me && x.woken);
    let result = loop {
        // futex_wake marks the entry, then wakes the thread: cleared before
        // looking, a wake-up that comes before parking makes it return.
        task::clear_wake();
        if woken(me) {
            break 0;
        }
        let now = crate::time::uptime_ms();
        if deadline.is_some_and(|d| now >= d) {
            break -err::ETIMEDOUT;
        }
        if let Some(e) = super::signal::interrupted_then(|| WAITERS.lock().retain(|x| x.tid != me)) {
            break e;
        }
        // Signals wake the thread too; the nap is only a safety net.
        let nap = now + 500;
        task::park(Some(deadline.map_or(nap, |d| d.min(nap))));
    };
    WAITERS.lock().retain(|x| x.tid != me);
    result
}

fn timeout_deadline(ts: u64, absolute: bool, realtime: bool) -> Result<Option<u64>, i64> {
    if ts == 0 {
        return Ok(None);
    }
    let b = user_bytes(ts, 16)?;
    let s = i64::from_le_bytes(b[0..8].try_into().unwrap()).max(0) as u64;
    let ns = i64::from_le_bytes(b[8..16].try_into().unwrap()).max(0) as u64;
    let ms = s.saturating_mul(1000) + ns.div_ceil(1_000_000);
    let now = crate::time::uptime_ms();
    if !absolute {
        return Ok(Some(now.saturating_add(ms)));
    }
    // An absolute time on the clock the program used.
    let clock_now = if realtime { crate::time::boot_unix() * 1000 + now } else { now };
    Ok(Some(now + ms.saturating_sub(clock_now)))
}

pub fn sys_futex(addr: u64, op: u64, val: u64, ts: u64, addr2: u64, val3: u64) -> i64 {
    let realtime = op & FUTEX_CLOCK_REALTIME != 0;
    let shared = op & FUTEX_PRIVATE == 0;
    let key = |addr| futex_key(addr, shared);
    match op & !(FUTEX_PRIVATE | FUTEX_CLOCK_REALTIME) {
        FUTEX_WAIT => match timeout_deadline(ts, false, false) {
            Ok(d) => futex_wait(addr, shared, val as u32, d, u32::MAX),
            Err(e) => e,
        },
        FUTEX_WAIT_BITSET => match timeout_deadline(ts, true, realtime) {
            Ok(d) => futex_wait(addr, shared, val as u32, d, val3 as u32),
            Err(e) => e,
        },
        FUTEX_WAKE => futex_wake(key(addr), val as u32, u32::MAX),
        FUTEX_WAKE_BITSET => futex_wake(key(addr), val as u32, val3 as u32),
        op @ (FUTEX_REQUEUE | FUTEX_CMP_REQUEUE) => {
            if op == FUTEX_CMP_REQUEUE {
                match read_u32(addr) {
                    Ok(v) if v != val3 as u32 => return -err::EAGAIN,
                    Err(e) => return e,
                    _ => {}
                }
            }
            let (from, to) = (key(addr), key(addr2));
            let woken = futex_wake(from, val as u32, u32::MAX);
            // `ts` carries the number to move to addr2.
            let limit = ts as u32;
            let mut moved = 0;
            for x in WAITERS.lock().iter_mut() {
                if moved >= limit {
                    break;
                }
                if !x.woken && x.key == from {
                    x.key = to;
                    moved += 1;
                }
            }
            woken + moved as i64
        }
        FUTEX_WAKE_OP => {
            // op (4 bits, 8 = "oparg is a shift"), cmp, oparg, cmparg.
            let op4 = (val3 >> 28) & 15;
            let opcode = op4 & 7;
            let cmp = (val3 >> 24) & 15;
            let mut oparg = ((val3 >> 12) & 0xfff) as u32;
            let cmparg = (val3 & 0xfff) as u32;
            if op4 & 8 != 0 {
                oparg = 1 << (oparg & 31);
            }
            if addr2 % 4 != 0 {
                return -err::EINVAL;
            }
            super::mm::populate(addr2, 4);
            let old = {
                let _w = WAITERS.lock();
                let Ok(b) = user_bytes(addr2, 4) else { return -err::EFAULT };
                // Atomically: threads on other processors change the word
                // with atomic instructions meanwhile.
                // SAFETY: four mapped, aligned bytes of the program's memory.
                let word = unsafe { &*(b.as_ptr() as *const core::sync::atomic::AtomicU32) };
                let order = core::sync::atomic::Ordering::SeqCst;
                let r = word.try_update(order, order, |old| {
                    Some(match opcode & 7 {
                        0 => oparg,
                        1 => old.wrapping_add(oparg),
                        2 => old | oparg,
                        3 => old & !oparg,
                        _ => old ^ oparg,
                    })
                });
                r.unwrap_or_else(|old| old)
            };
            let mut n = futex_wake(key(addr), val as u32, u32::MAX);
            let hit = match cmp {
                0 => old == cmparg,
                1 => old != cmparg,
                2 => (old as i32) < cmparg as i32,
                3 => (old as i32) <= cmparg as i32,
                4 => (old as i32) > cmparg as i32,
                _ => (old as i32) >= cmparg as i32,
            };
            if hit {
                n += futex_wake(key(addr2), ts as u32, u32::MAX);
            }
            n
        }
        _ => -err::ENOSYS,
    }
}

/// set_thread_area: one of the calling thread's own segment descriptors
/// (three, as on Linux: selectors 0x63, 0x6b and 0x73), from a `user_desc`.
/// An entry number of -1 asks for a free one, and the number is written
/// back. Only 32-bit data segments are made, as Linux allows.
pub fn sys_set_thread_area(ptr: u64) -> i64 {
    use crate::arch::{cpu, gdt};
    let b = match user_bytes(ptr, 16) {
        Ok(b) => b,
        Err(e) => return e,
    };
    let word = |i: usize| u32::from_le_bytes(b[i * 4..i * 4 + 4].try_into().unwrap());
    let (entry, base, limit, flags) = (word(0) as i32, word(1) as u64, word(2) as u64, word(3));
    let (seg_32bit, contents, read_only) = (flags & 1 != 0, flags >> 1 & 3, flags & 8 != 0);
    let (in_pages, not_present, useable) = (flags & 0x10 != 0, flags & 0x20 != 0, flags & 0x40 != 0);
    // Linux's two ways to say "no segment".
    let empty = base == 0 && limit == 0 && (flags & 0x7f == 0 || flags & 0x7f == 0x28);
    if !empty && (!seg_32bit || contents > 1 || not_present) {
        return -err::EINVAL;
    }
    let descriptor = if empty {
        0
    } else {
        // Present, ring 3, data, already marked accessed.
        let access = 0xF1 | (!read_only as u64) << 1 | ((contents & 1) as u64) << 2;
        let size = (useable as u64) | 1 << 2 | (in_pages as u64) << 3;
        (limit & 0xFFFF) | (base & 0xFF_FFFF) << 16 | access << 40 | (limit >> 16 & 0xF) << 48 | size << 52 | (base >> 24 & 0xFF) << 56
    };
    let on = cpu::irq_save();
    let tls = gdt::segments().tls;
    let index = match entry {
        -1 => tls.iter().position(|&d| d == 0),
        e if (gdt::TLS_FIRST as i32..(gdt::TLS_FIRST + gdt::TLS_COUNT) as i32).contains(&e) => Some(e as usize - gdt::TLS_FIRST),
        _ => None,
    };
    if let Some(i) = index {
        gdt::set_tls(i, descriptor);
    }
    cpu::irq_restore(on);
    match index {
        Some(i) => {
            if entry == -1 {
                b[..4].copy_from_slice(&((gdt::TLS_FIRST + i) as u32).to_le_bytes());
            }
            0
        }
        None if entry == -1 => -err::ESRCH,
        None => -err::EINVAL,
    }
}

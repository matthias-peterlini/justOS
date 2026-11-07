//! Threads and processes of Linux programs: `clone` for threads sharing the
//! address space (as pthreads and the Go and Rust runtimes create them) and
//! for new processes (fork, vfork, posix_spawn), futexes, and thread exit
//! with its `clear_child_tid` wake-up. Signals are in [`super::signal`].

use super::{err, user_bytes, with_state};
use crate::arch::cpu;
use crate::arch::idt::TrapFrame;
use crate::sync::IrqLock;
use crate::task::{self, Tid, WaitQueue};
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
    linux_tid(task::current_tid()) as i64
}

pub fn sys_clone(frame: &TrapFrame, flags: u64, stack: u64, ptid: u64, ctid: u64, tls: u64) -> i64 {
    if flags & CLONE_THREAD == 0 {
        return clone_process(frame, flags, stack, ptid, ctid, tls);
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
    let lt = tid + 100_000;
    super::signal::thread_started(tid);
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
fn clone_process(frame: &TrapFrame, flags: u64, stack: u64, ptid: u64, ctid: u64, tls: u64) -> i64 {
    let share = flags & CLONE_VM != 0;
    let mut child = *frame;
    child.rax = 0;
    if stack != 0 {
        child.rsp = stack;
    }
    let fs = if flags & CLONE_SETTLS != 0 { tls } else { crate::arch::syscall::user_bases().0 };
    let (pid, tid) = match user::fork(child, fs, share) {
        Ok(v) => v,
        Err(e) => return -super::errno(e),
    };
    super::fork_state(pid, tid);
    // The child's main thread has the process's id as its thread id.
    let id = pid.to_le_bytes();
    if flags & CLONE_PARENT_SETTID != 0 {
        super::mm::populate(ptid, 4);
        if let Ok(b) = user_bytes(ptid, 4) {
            b.copy_from_slice(&id);
        }
    }
    if flags & CLONE_CHILD_SETTID != 0 {
        if share {
            super::mm::populate(ctid, 4);
            if let Ok(b) = user_bytes(ctid, 4) {
                b.copy_from_slice(&id);
            }
        } else {
            user::write_in(pid, ctid, &id);
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
    pid as i64
}

pub fn sys_set_tid_address(ptr: u64) -> i64 {
    let me = task::current_tid();
    with_state(|st| {
        st.clear_tid.insert(me, ptr);
    });
    linux_tid(me) as i64
}

/// The calling thread ends (exit); the process goes on unless it was the
/// last one.
pub fn sys_exit(code: i64) -> ! {
    let me = task::current_tid();
    let ctid = with_state(|st| {
        super::signal::thread_ended(st, me);
        st.clear_tid.remove(&me)
    });
    if let Some(ctid) = ctid {
        if ctid != 0 {
            if let Ok(b) = user_bytes(ctid, 4) {
                b.copy_from_slice(&0u32.to_le_bytes());
            }
            futex_wake(ctid, 1, u32::MAX);
        }
    }
    user::exit_thread(code)
}

// ----------------------------------------------------------------------------
// Futexes

struct Waiter {
    cr3: u64,
    addr: u64,
    tid: Tid,
    bitset: u32,
    woken: bool,
}

static WAITERS: IrqLock<Vec<Waiter>> = IrqLock::new(Vec::new());
static FUTEX_QUEUE: WaitQueue = WaitQueue::new();

const FUTEX_WAIT: u64 = 0;
const FUTEX_WAKE: u64 = 1;
const FUTEX_REQUEUE: u64 = 3;
const FUTEX_CMP_REQUEUE: u64 = 4;
const FUTEX_WAKE_OP: u64 = 5;
const FUTEX_WAIT_BITSET: u64 = 9;
const FUTEX_WAKE_BITSET: u64 = 10;
const FUTEX_PRIVATE: u64 = 128;
const FUTEX_CLOCK_REALTIME: u64 = 256;

fn read_u32(addr: u64) -> Result<u32, i64> {
    super::mm::populate(addr, 4);
    let b = user_bytes(addr, 4)?;
    Ok(u32::from_le_bytes(b[..4].try_into().unwrap()))
}

fn futex_wake(addr: u64, n: u32, bitset: u32) -> i64 {
    let cr3 = cpu::read_cr3();
    let mut count = 0;
    {
        let mut w = WAITERS.lock();
        for x in w.iter_mut() {
            if count >= n {
                break;
            }
            if !x.woken && x.cr3 == cr3 && x.addr == addr && x.bitset & bitset != 0 {
                x.woken = true;
                count += 1;
            }
        }
    }
    if count > 0 {
        FUTEX_QUEUE.notify_all();
    }
    count as i64
}

/// Waits while `*addr == val`; `deadline` is an uptime in ms.
fn futex_wait(addr: u64, val: u32, deadline: Option<u64>, bitset: u32) -> i64 {
    if bitset == 0 {
        return -err::EINVAL;
    }
    super::mm::populate(addr, 4);
    let cr3 = cpu::read_cr3();
    let me = task::current_tid();
    {
        // Compare and queue with interrupts off: the threads of programs all
        // run on the first processor, so none runs in between and no
        // wake-up is lost.
        let mut w = WAITERS.lock();
        let cur = match user_bytes(addr, 4) {
            Ok(b) => u32::from_le_bytes(b[..4].try_into().unwrap()),
            Err(e) => return e,
        };
        if cur != val {
            return -err::EAGAIN;
        }
        w.push(Waiter { cr3, addr, tid: me, bitset, woken: false });
    }
    let woken = |me: Tid| WAITERS.lock().iter().any(|x| x.tid == me && x.woken);
    let result = loop {
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
        let slice = deadline.map_or(100, |d| (d - now).clamp(1, 100));
        FUTEX_QUEUE.wait_for_timeout(slice, || woken(me).then_some(()));
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
    let clock_now = if realtime { crate::time::unix_now() * 1000 + now % 1000 } else { now };
    Ok(Some(now + ms.saturating_sub(clock_now)))
}

pub fn sys_futex(addr: u64, op: u64, val: u64, ts: u64, addr2: u64, val3: u64) -> i64 {
    let realtime = op & FUTEX_CLOCK_REALTIME != 0;
    match op & !(FUTEX_PRIVATE | FUTEX_CLOCK_REALTIME) {
        FUTEX_WAIT => match timeout_deadline(ts, false, false) {
            Ok(d) => futex_wait(addr, val as u32, d, u32::MAX),
            Err(e) => e,
        },
        FUTEX_WAIT_BITSET => match timeout_deadline(ts, true, realtime) {
            Ok(d) => futex_wait(addr, val as u32, d, val3 as u32),
            Err(e) => e,
        },
        FUTEX_WAKE => futex_wake(addr, val as u32, u32::MAX),
        FUTEX_WAKE_BITSET => futex_wake(addr, val as u32, val3 as u32),
        op @ (FUTEX_REQUEUE | FUTEX_CMP_REQUEUE) => {
            if op == FUTEX_CMP_REQUEUE {
                match read_u32(addr) {
                    Ok(v) if v != val3 as u32 => return -err::EAGAIN,
                    Err(e) => return e,
                    _ => {}
                }
            }
            let woken = futex_wake(addr, val as u32, u32::MAX);
            // `ts` carries the number to move to addr2.
            let limit = ts as u32;
            let cr3 = cpu::read_cr3();
            let mut moved = 0;
            for x in WAITERS.lock().iter_mut() {
                if moved >= limit {
                    break;
                }
                if !x.woken && x.cr3 == cr3 && x.addr == addr {
                    x.addr = addr2;
                    moved += 1;
                }
            }
            woken + moved as i64
        }
        FUTEX_WAKE_OP => {
            let opcode = (val3 >> 28) & 7;
            let cmp = (val3 >> 24) & 15;
            let mut oparg = ((val3 >> 12) & 0xfff) as u32;
            let cmparg = (val3 & 0xfff) as u32;
            if opcode & 8 != 0 {
                oparg = 1 << (oparg & 31);
            }
            super::mm::populate(addr2, 4);
            let old = {
                let _w = WAITERS.lock();
                let Ok(b) = user_bytes(addr2, 4) else { return -err::EFAULT };
                let old = u32::from_le_bytes(b[..4].try_into().unwrap());
                let new = match opcode & 7 {
                    0 => oparg,
                    1 => old.wrapping_add(oparg),
                    2 => old | oparg,
                    3 => old & !oparg,
                    _ => old ^ oparg,
                };
                b.copy_from_slice(&new.to_le_bytes());
                old
            };
            let mut n = futex_wake(addr, val as u32, u32::MAX);
            let hit = match cmp {
                0 => old == cmparg,
                1 => old != cmparg,
                2 => (old as i32) < cmparg as i32,
                3 => (old as i32) <= cmparg as i32,
                4 => (old as i32) > cmparg as i32,
                _ => (old as i32) >= cmparg as i32,
            };
            if hit {
                n += futex_wake(addr2, ts as u32, u32::MAX);
            }
            n
        }
        _ => -err::ENOSYS,
    }
}

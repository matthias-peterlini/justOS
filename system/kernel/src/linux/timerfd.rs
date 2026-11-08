//! timerfd: a timer read as a descriptor. Reading gives how many times it
//! went off since the last read (blocking until it does); poll and epoll
//! see it readable then, and wake at its deadline. Android's init and
//! systemd run their main loops on these.

use alloc::sync::Arc;

use super::fd::{self, Desc, Kind};
use super::{err, user_bytes};
use crate::sync::IrqLock;
use crate::task::WaitQueue;

const TFD_TIMER_ABSTIME: u32 = 1;
const TFD_TIMER_CANCEL_ON_SET: u32 = 2;
const NS: u64 = 1_000_000_000;

pub struct TimerFd {
    /// Set against the wall clock (CLOCK_REALTIME and its alarm), which an
    /// absolute time is given in.
    realtime: bool,
    state: IrqLock<Timer>,
    /// Told when the timer is set (a wait may need to wake sooner).
    pub wq: Arc<WaitQueue>,
}

#[derive(Default)]
struct Timer {
    /// When it goes off next (uptime, ns).
    next: Option<u64>,
    /// Every how long after that (0: once).
    interval: u64,
    /// How many times it went off and nobody read.
    ticks: u64,
}

impl Timer {
    /// Counts what went off up to `now`.
    fn update(&mut self, now: u64) {
        let Some(next) = self.next.filter(|&n| now >= n) else { return };
        match (now - next).checked_div(self.interval) {
            Some(late) => {
                self.ticks += late + 1;
                self.next = Some(next + (late + 1) * self.interval);
            }
            // Once.
            None => {
                self.ticks += 1;
                self.next = None;
            }
        }
    }
}

fn now() -> u64 {
    crate::time::uptime_ns()
}

pub fn sys_timerfd_create(clock: u64, flags: u32) -> i64 {
    // REALTIME, MONOTONIC, BOOTTIME and the two alarms.
    let realtime = match clock {
        0 | 8 => true,
        1 | 7 | 9 => false,
        _ => return -err::EINVAL,
    };
    if flags & !(fd::O_NONBLOCK | fd::O_CLOEXEC) != 0 {
        return -err::EINVAL;
    }
    let t = Arc::new(TimerFd { realtime, state: IrqLock::new(Timer::default()), wq: Arc::default() });
    fd::install(Desc::new(Kind::Timer(t), flags & fd::O_NONBLOCK), flags & fd::O_CLOEXEC != 0)
}

fn timer(fd: i32) -> Result<Arc<TimerFd>, i64> {
    match fd::get(fd) {
        Some(d) => match &d.kind {
            Kind::Timer(t) => Ok(t.clone()),
            _ => Err(-err::EINVAL),
        },
        None => Err(-err::EBADF),
    }
}

/// A timespec in user memory, in ns.
fn read_ts(b: &[u8]) -> Result<u64, i64> {
    let s = i64::from_le_bytes(b[0..8].try_into().unwrap());
    let ns = i64::from_le_bytes(b[8..16].try_into().unwrap());
    if s < 0 || !(0..NS as i64).contains(&ns) {
        return Err(-err::EINVAL);
    }
    Ok((s as u64).saturating_mul(NS).saturating_add(ns as u64))
}

fn write_ts(b: &mut [u8], ns: u64) {
    b[0..8].copy_from_slice(&(ns / NS).to_le_bytes());
    b[8..16].copy_from_slice(&(ns % NS).to_le_bytes());
}

/// An itimerspec of what is left and the interval.
fn current(t: &Timer, now: u64, out: u64) -> i64 {
    let Ok(b) = user_bytes(out, 32) else { return -err::EFAULT };
    write_ts(&mut b[0..16], t.interval);
    write_ts(&mut b[16..32], t.next.map_or(0, |n| n.saturating_sub(now)));
    0
}

pub fn sys_timerfd_settime(fd: i32, flags: u32, new: u64, old: u64) -> i64 {
    let t = match timer(fd) {
        Ok(t) => t,
        Err(e) => return e,
    };
    if flags & !(TFD_TIMER_ABSTIME | TFD_TIMER_CANCEL_ON_SET) != 0 {
        return -err::EINVAL;
    }
    let (interval, value) = match user_bytes(new, 32) {
        Ok(b) => match (read_ts(&b[0..16]), read_ts(&b[16..32])) {
            (Ok(i), Ok(v)) => (i, v),
            (Err(e), _) | (_, Err(e)) => return e,
        },
        Err(e) => return e,
    };
    let now = now();
    let next = if value == 0 {
        None
    } else if flags & TFD_TIMER_ABSTIME != 0 {
        // The wall clock is the boot time plus the uptime.
        let base = if t.realtime { crate::time::boot_unix() * NS } else { 0 };
        Some(value.saturating_sub(base))
    } else {
        Some(now.saturating_add(value))
    };
    {
        let mut s = t.state.lock();
        s.update(now);
        if old != 0 {
            let r = current(&s, now, old);
            if r != 0 {
                return r;
            }
        }
        *s = Timer { next, interval, ticks: 0 };
    }
    t.wq.notify_all();
    0
}

pub fn sys_timerfd_gettime(fd: i32, out: u64) -> i64 {
    let t = match timer(fd) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let now = now();
    let mut s = t.state.lock();
    s.update(now);
    current(&s, now, out)
}

impl TimerFd {
    pub fn ready(&self) -> bool {
        let mut s = self.state.lock();
        s.update(now());
        s.ticks > 0
    }

    /// When a wait should wake to look again (uptime, ms).
    pub fn deadline_ms(&self) -> Option<u64> {
        self.state.lock().next.map(|n| n.div_ceil(1_000_000))
    }

    pub fn read(&self, d: &Desc, buf: &mut [u8]) -> i64 {
        if buf.len() < 8 {
            return -err::EINVAL;
        }
        loop {
            let ticks = {
                let mut s = self.state.lock();
                s.update(now());
                core::mem::take(&mut s.ticks)
            };
            if ticks > 0 {
                buf[..8].copy_from_slice(&ticks.to_le_bytes());
                return 8;
            }
            if let Err(e) = fd::wait_until(d, || self.ready()) {
                return e;
            }
        }
    }
}

//! Waiting on many descriptors: `poll`, `select` and `epoll` (level
//! triggered; edge-triggered sets are served the same way, which programs
//! written for edge triggering handle since they read until EAGAIN, except
//! on descriptors that are always ready, like files: there the one edge is
//! when the watch is set, as on Linux, and they are reported once).
//!
//! Every source of readiness (network, pipes, eventfds) signals the one
//! queue [`crate::net::EVENTS`]; the console is looked at every few
//! milliseconds.

use super::fd::{self, Desc, Kind};
use super::{err, user_bytes};
use crate::net::EVENTS;
use crate::sync::IrqLock;
use alloc::collections::BTreeMap;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;

const POLLIN: u16 = 0x001;
const POLLPRI: u16 = 0x002;
const POLLOUT: u16 = 0x004;
const POLLERR: u16 = 0x008;
const POLLHUP: u16 = 0x010;
const POLLNVAL: u16 = 0x020;
const POLLRDHUP: u16 = 0x2000;

const EPOLLIN: u32 = 0x001;
const EPOLLOUT: u32 = 0x004;
const EPOLLERR: u32 = 0x008;
const EPOLLHUP: u32 = 0x010;
const EPOLLRDHUP: u32 = 0x2000;
const EPOLLONESHOT: u32 = 1 << 30;
const EPOLLET: u32 = 1 << 31;

/// Waits until `check` returns something, or the timeout (ms; negative:
/// forever) runs out.
fn wait<T>(timeout_ms: i64, mut check: impl FnMut() -> Option<T>) -> Result<Option<T>, i64> {
    let start = crate::time::uptime_ms();
    loop {
        if let Some(v) = check() {
            return Ok(Some(v));
        }
        let elapsed = (crate::time::uptime_ms() - start) as i64;
        if timeout_ms >= 0 && elapsed >= timeout_ms {
            return Ok(None);
        }
        // A signal handler to run: poll, select and epoll_wait fail with
        // EINTR, whatever SA_RESTART says.
        if super::signal::interrupted().is_some() {
            return Err(-err::EINTR);
        }
        let slice = if timeout_ms < 0 { 10 } else { (timeout_ms - elapsed).clamp(1, 10) as u64 };
        EVENTS.wait_for_timeout(slice, || None::<()>);
    }
}

fn ready_bits(d: &Desc, want: u16) -> u16 {
    let (r, w, h) = fd::readiness(d);
    let mut out = 0;
    if r && want & (POLLIN | POLLPRI) != 0 {
        out |= POLLIN;
    }
    if w && want & POLLOUT != 0 {
        out |= POLLOUT;
    }
    if h {
        out |= POLLHUP;
        if want & POLLRDHUP != 0 {
            out |= POLLRDHUP;
        }
    }
    out
}

pub fn sys_poll(ptr: u64, n: u64, timeout_ms: i64) -> i64 {
    if n > 4096 {
        return -err::EINVAL;
    }
    let fds = match user_bytes(ptr, n * 8) {
        Ok(b) => b,
        Err(e) => return e,
    };
    let entries: Vec<(i32, u16)> = fds.chunks(8).map(|c| (i32::from_le_bytes(c[0..4].try_into().unwrap()), u16::from_le_bytes([c[4], c[5]]))).collect();
    let descs: Vec<Option<Arc<Desc>>> = entries.iter().map(|&(f, _)| if f < 0 { None } else { fd::get(f) }).collect();
    let r = wait(timeout_ms, || {
        let mut count = 0;
        let mut out = Vec::with_capacity(entries.len());
        for (i, &(f, want)) in entries.iter().enumerate() {
            let bits = if f < 0 {
                0
            } else {
                match &descs[i] {
                    Some(d) => ready_bits(d, want),
                    None => POLLNVAL,
                }
            };
            if bits != 0 {
                count += 1;
            }
            out.push(bits);
        }
        (count > 0).then_some((count, out))
    });
    let r = match r {
        Ok(r) => r,
        Err(e) => return e,
    };
    let (count, bits) = r.unwrap_or((0, alloc::vec![0; entries.len()]));
    for (i, c) in fds.chunks_mut(8).enumerate() {
        c[6..8].copy_from_slice(&bits[i].to_le_bytes());
    }
    count
}

/// The timeout of ppoll / pselect (a timespec pointer, 0 = forever).
fn timespec_ms(ptr: u64) -> Result<i64, i64> {
    if ptr == 0 {
        return Ok(-1);
    }
    let b = user_bytes(ptr, 16)?;
    let s = i64::from_le_bytes(b[0..8].try_into().unwrap());
    let ns = i64::from_le_bytes(b[8..16].try_into().unwrap());
    Ok(s.saturating_mul(1000) + ns / 1_000_000 + if ns % 1_000_000 != 0 { 1 } else { 0 })
}

pub fn sys_ppoll(ptr: u64, n: u64, ts: u64) -> i64 {
    match timespec_ms(ts) {
        Ok(ms) => sys_poll(ptr, n, ms),
        Err(e) => e,
    }
}

/// select / pselect6: three bit sets of `n` descriptors.
pub fn sys_select(n: u64, rd: u64, wr: u64, ex: u64, timeout_ms: i64) -> i64 {
    let n = n.min(1024) as usize;
    let bytes = n.div_ceil(64) * 8;
    let read_set = |p: u64| -> Result<Vec<u8>, i64> { if p == 0 { Ok(alloc::vec![0; bytes]) } else { Ok(user_bytes(p, bytes as u64)?.to_vec()) } };
    let (r0, w0, e0) = match (read_set(rd), read_set(wr), read_set(ex)) {
        (Ok(a), Ok(b), Ok(c)) => (a, b, c),
        (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => return e,
    };
    let bit = |s: &[u8], i: usize| s[i / 8] & (1 << (i % 8)) != 0;
    let mut descs = BTreeMap::new();
    for i in 0..n {
        if bit(&r0, i) || bit(&w0, i) || bit(&e0, i) {
            match fd::get(i as i32) {
                Some(d) => {
                    descs.insert(i, d);
                }
                None => return -err::EBADF,
            }
        }
    }
    let r = wait(timeout_ms, || {
        let (mut r1, mut w1) = (alloc::vec![0u8; bytes], alloc::vec![0u8; bytes]);
        let mut count = 0;
        for (&i, d) in &descs {
            let (r, w, h) = fd::readiness(d);
            if bit(&r0, i) && (r || h) {
                r1[i / 8] |= 1 << (i % 8);
                count += 1;
            }
            if bit(&w0, i) && w {
                w1[i / 8] |= 1 << (i % 8);
                count += 1;
            }
        }
        (count > 0).then_some((count, r1, w1))
    });
    let r = match r {
        Ok(r) => r,
        Err(e) => return e,
    };
    let (count, r1, w1) = r.unwrap_or((0, alloc::vec![0; bytes], alloc::vec![0; bytes]));
    for (p, v) in [(rd, &r1), (wr, &w1), (ex, &alloc::vec![0u8; bytes])] {
        if p != 0 {
            if let Ok(b) = user_bytes(p, bytes as u64) {
                b.copy_from_slice(v);
            }
        }
    }
    count
}

pub fn sys_select_tv(n: u64, rd: u64, wr: u64, ex: u64, tv: u64) -> i64 {
    let ms = if tv == 0 {
        -1
    } else {
        match user_bytes(tv, 16) {
            Ok(b) => {
                let s = i64::from_le_bytes(b[0..8].try_into().unwrap());
                let us = i64::from_le_bytes(b[8..16].try_into().unwrap());
                s * 1000 + us / 1000
            }
            Err(e) => return e,
        }
    };
    sys_select(n, rd, wr, ex, ms)
}

pub fn sys_pselect6(n: u64, rd: u64, wr: u64, ex: u64, ts: u64) -> i64 {
    match timespec_ms(ts) {
        Ok(ms) => sys_select(n, rd, wr, ex, ms),
        Err(e) => e,
    }
}

// ----------------------------------------------------------------------------
// epoll

struct Watch {
    desc: Weak<Desc>,
    events: u32,
    data: u64,
    /// One-shot entries go quiet after reporting once, and so do
    /// edge-triggered ones on descriptors that are always ready.
    armed: bool,
}

pub struct Epoll {
    watches: IrqLock<BTreeMap<i32, Watch>>,
}

impl Epoll {
    /// Whether a wait would report something; it reports nothing itself,
    /// so one-shot entries stay armed for the wait that follows.
    pub fn any_ready(&self) -> bool {
        !self.collect(1, false).is_empty()
    }

    /// The events to report (at most `max`); `report` disarms the entries
    /// that go quiet once reported.
    fn collect(&self, max: usize, report: bool) -> Vec<(u32, u64)> {
        let list: Vec<(i32, Arc<Desc>, u32, u64)> = self
            .watches
            .lock()
            .iter()
            .filter(|(_, w)| w.armed)
            .filter_map(|(&fd, w)| Some((fd, w.desc.upgrade()?, w.events, w.data)))
            .collect();
        let mut out = Vec::new();
        for (fd, d, want, data) in list {
            if out.len() >= max {
                break;
            }
            let (r, w, h) = fd::readiness(&d);
            let mut ev = 0;
            if r && want & EPOLLIN != 0 {
                ev |= EPOLLIN;
            }
            if w && want & EPOLLOUT != 0 {
                ev |= EPOLLOUT;
            }
            if h {
                ev |= EPOLLHUP | if want & EPOLLRDHUP != 0 { EPOLLRDHUP } else { 0 };
            }
            if ev != 0 {
                if report && (want & EPOLLONESHOT != 0 || (want & EPOLLET != 0 && fd::always_ready(&d))) {
                    if let Some(w) = self.watches.lock().get_mut(&fd) {
                        w.armed = false;
                    }
                }
                out.push((ev, data));
            }
        }
        out
    }
}

pub fn sys_epoll_create1(flags: u32) -> i64 {
    let e = Epoll { watches: IrqLock::new(BTreeMap::new()) };
    fd::install(Desc::new(Kind::Epoll(e), 0), flags & fd::O_CLOEXEC != 0)
}

pub fn sys_epoll_ctl(epfd: i32, op: u32, fd_: i32, ev: u64) -> i64 {
    const ADD: u32 = 1;
    const DEL: u32 = 2;
    const MOD: u32 = 3;
    let Some(ep) = fd::get(epfd) else { return -err::EBADF };
    let Kind::Epoll(e) = &ep.kind else { return -err::EINVAL };
    let Some(target) = fd::get(fd_) else { return -err::EBADF };
    let (events, data) = if op == DEL {
        (0, 0)
    } else {
        match user_bytes(ev, 12) {
            Ok(b) => (u32::from_le_bytes(b[0..4].try_into().unwrap()), u64::from_le_bytes(b[4..12].try_into().unwrap())),
            Err(e) => return e,
        }
    };
    let mut w = e.watches.lock();
    match op {
        ADD => {
            if w.contains_key(&fd_) {
                return -err::EEXIST;
            }
            w.insert(fd_, Watch { desc: Arc::downgrade(&target), events, data, armed: true });
        }
        MOD => match w.get_mut(&fd_) {
            Some(x) => {
                x.events = events;
                x.data = data;
                x.armed = true;
            }
            None => return -err::ENOENT,
        },
        DEL => {
            if w.remove(&fd_).is_none() {
                return -err::ENOENT;
            }
        }
        _ => return -err::EINVAL,
    }
    drop(w);
    EVENTS.notify_all();
    0
}

pub fn sys_epoll_wait(epfd: i32, out: u64, max: i32, timeout_ms: i64) -> i64 {
    if max <= 0 {
        return -err::EINVAL;
    }
    let Some(ep) = fd::get(epfd) else { return -err::EBADF };
    let Kind::Epoll(e) = &ep.kind else { return -err::EINVAL };
    let max = (max as usize).min(1024);
    let got = match wait(timeout_ms, || {
        let v = e.collect(max, true);
        (!v.is_empty()).then_some(v)
    }) {
        Ok(v) => v.unwrap_or_default(),
        Err(x) => return x,
    };
    let buf = match user_bytes(out, (got.len() * 12) as u64) {
        Ok(b) => b,
        Err(e) => return e,
    };
    for (i, (ev, data)) in got.iter().enumerate() {
        buf[i * 12..i * 12 + 4].copy_from_slice(&ev.to_le_bytes());
        buf[i * 12 + 4..i * 12 + 12].copy_from_slice(&data.to_le_bytes());
    }
    got.len() as i64
}

pub fn sys_epoll_pwait2(epfd: i32, out: u64, max: i32, ts: u64) -> i64 {
    match timespec_ms(ts) {
        Ok(ms) => sys_epoll_wait(epfd, out, max, ms),
        Err(e) => e,
    }
}

#[allow(dead_code)]
const UNUSED: u16 = POLLERR;
#[allow(dead_code)]
const UNUSED2: u32 = EPOLLERR;

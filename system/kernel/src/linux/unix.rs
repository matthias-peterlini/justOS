//! Local sockets (AF_UNIX): how programs on one machine talk to each other.
//! Wayland clients reach the compositor through one, D-Bus runs over them,
//! and browsers pass their child processes descriptors (shared memory,
//! pipes) along them with SCM_RIGHTS.
//!
//! Each direction of a connection is a [`Queue`] of messages. A stream
//! socket reads them as one run of bytes (a message that carries
//! descriptors ends a read); datagram and seqpacket sockets read one
//! message at a time. Names live in the kernel only: a path name is not a
//! file on the volume, but `stat`, `access` and `unlink` know it.

use super::fd::{self, Desc, Kind, O_NONBLOCK, O_RDWR};
use super::{err, linux_path, user_bytes};
use crate::net::EVENTS;
use crate::sync::IrqLock;
use crate::task;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

pub const AF_UNIX: u64 = 1;
const SOCK_STREAM: u64 = 1;
const SOCK_DGRAM: u64 = 2;
const SOCK_SEQPACKET: u64 = 5;
const SOCK_NONBLOCK: u64 = 0o4000;
const SOCK_CLOEXEC: u64 = 0o2000000;

const MSG_PEEK: u64 = 2;
const MSG_TRUNC: u64 = 0x20;
const MSG_DONTWAIT: u64 = 0x40;
const MSG_WAITALL: u64 = 0x100;
const MSG_NOSIGNAL: u64 = 0x4000;
const MSG_CMSG_CLOEXEC: u64 = 0x4000_0000;
/// msg_flags: the control buffer was too small.
const MSG_CTRUNC: u32 = 8;
/// msg_flags: the message was longer than the buffer.
const MSG_TRUNC_FLAG: u32 = 0x20;

const SOL_SOCKET: u32 = 1;
const SCM_RIGHTS: u32 = 1;

/// What one direction holds before writers wait.
const CAP: usize = 256 * 1024;
/// The largest piece of a stream write kept as one message.
const CHUNK: usize = 64 * 1024;
/// Descriptors in one message, at most (SCM_MAX_FD).
const MAX_FDS: usize = 253;

#[derive(Clone, Copy, PartialEq)]
enum Ty {
    Stream,
    Dgram,
    Seqpacket,
}

impl Ty {
    fn parse(ty: u64) -> Option<Ty> {
        match ty & 0xf {
            SOCK_STREAM => Some(Ty::Stream),
            SOCK_DGRAM => Some(Ty::Dgram),
            SOCK_SEQPACKET => Some(Ty::Seqpacket),
            _ => None,
        }
    }

    fn number(self) -> u32 {
        match self {
            Ty::Stream => 1,
            Ty::Dgram => 2,
            Ty::Seqpacket => 5,
        }
    }
}

// ----------------------------------------------------------------------------
// Names

/// How the kernel tells names apart.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    /// The abstract namespace: `sun_path` starts with a 0 byte.
    Abstract(Vec<u8>),
    /// A place on a volume ("volume:/path").
    Path(String),
}

/// A socket's address.
#[derive(Clone)]
pub struct Name {
    key: Key,
    /// `sun_path` as the program gave it, without a trailing 0.
    raw: Vec<u8>,
}

/// Reads a `struct sockaddr_un`. None: no name (only the family).
fn read_name(addr: u64, len: u64) -> Result<Option<Name>, i64> {
    if !(2..=110).contains(&len) {
        return Err(-err::EINVAL);
    }
    let b = user_bytes(addr, len)?.to_vec();
    if u16::from_le_bytes([b[0], b[1]]) as u64 != AF_UNIX {
        return Err(-err::EINVAL);
    }
    let path = &b[2..];
    if path.is_empty() {
        return Ok(None);
    }
    if path[0] == 0 {
        return Ok(Some(Name { key: Key::Abstract(path[1..].to_vec()), raw: path.to_vec() }));
    }
    let end = path.iter().position(|&c| c == 0).unwrap_or(path.len());
    let text = core::str::from_utf8(&path[..end]).map_err(|_| -err::EINVAL)?;
    let p = linux_path(text)?;
    Ok(Some(Name { key: path_key(&p), raw: path[..end].to_vec() }))
}

fn path_key(p: &crate::fs::Path) -> Key {
    Key::Path(alloc::format!("{}:{}", p.volume.to_ascii_lowercase(), p.inner))
}

/// Writes a `struct sockaddr_un` where the program asked, with its length.
fn write_name(addr: u64, lenp: u64, name: Option<&Name>) -> i64 {
    if addr == 0 || lenp == 0 {
        return 0;
    }
    let cap = match user_bytes(lenp, 4) {
        Ok(b) => u32::from_le_bytes(b[..4].try_into().unwrap()) as usize,
        Err(e) => return e,
    };
    let mut sa = Vec::from((AF_UNIX as u16).to_le_bytes());
    if let Some(n) = name {
        sa.extend_from_slice(&n.raw);
        if let Key::Path(_) = n.key {
            sa.push(0);
        }
    }
    let k = cap.min(sa.len());
    match user_bytes(addr, k as u64) {
        Ok(b) => b.copy_from_slice(&sa[..k]),
        Err(e) => return e,
    }
    if let Ok(b) = user_bytes(lenp, 4) {
        b.copy_from_slice(&(sa.len() as u32).to_le_bytes());
    }
    0
}

enum Bound {
    /// Bound, not listening yet.
    Reserved,
    Listener(Weak<Listener>),
    Dgram(Weak<Queue>),
}

/// Bound names, with the socket (by id) that holds each.
static NAMES: IrqLock<BTreeMap<Key, (u64, Bound)>> = IrqLock::new(BTreeMap::new());
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn alive(b: &Bound) -> bool {
    match b {
        Bound::Reserved => true,
        Bound::Listener(w) => w.strong_count() > 0,
        Bound::Dgram(w) => w.strong_count() > 0,
    }
}

/// Takes `key` for socket `id` (or updates what it is bound as).
fn register(key: &Key, id: u64, what: Bound) -> Result<(), i64> {
    let mut names = NAMES.lock();
    if let Some((owner, b)) = names.get(key) {
        if *owner != id && alive(b) {
            return Err(-err::EADDRINUSE);
        }
    }
    names.insert(key.clone(), (id, what));
    Ok(())
}

fn unregister(key: &Key, id: u64) {
    let mut names = NAMES.lock();
    if names.get(key).is_some_and(|(owner, _)| *owner == id) {
        names.remove(key);
    }
}

/// The error for a name nobody listens on: a path that is not there is
/// ENOENT, as on Linux.
fn refused(name: &Name) -> i64 {
    match name.key {
        Key::Path(_) => -err::ENOENT,
        Key::Abstract(_) => -err::ECONNREFUSED,
    }
}

fn find_listener(name: &Name) -> Result<Arc<Listener>, i64> {
    match NAMES.lock().get(&name.key) {
        Some((_, Bound::Listener(w))) => w.upgrade().ok_or(-err::ECONNREFUSED),
        Some(_) => Err(-err::ECONNREFUSED),
        None => Err(refused(name)),
    }
}

fn find_dgram(name: &Name) -> Result<Weak<Queue>, i64> {
    match NAMES.lock().get(&name.key) {
        Some((_, Bound::Dgram(w))) if w.strong_count() > 0 => Ok(w.clone()),
        Some(_) => Err(-err::ECONNREFUSED),
        None => Err(refused(name)),
    }
}

/// Whether a socket is bound at this path (for `stat` and `access`).
pub fn is_bound(p: &crate::fs::Path) -> bool {
    NAMES.lock().get(&path_key(p)).is_some_and(|(_, b)| alive(b))
}

/// unlink of a bound path: the name goes (the socket stays, unreachable).
/// False if no socket is bound there.
pub fn unlink(p: &crate::fs::Path) -> bool {
    NAMES.lock().remove(&path_key(p)).is_some()
}

// ----------------------------------------------------------------------------
// Queues and connections

struct Msg {
    data: Vec<u8>,
    /// What a stream reader already took.
    off: usize,
    fds: Vec<Arc<Desc>>,
    /// Who sent it (a bound datagram socket).
    from: Option<Name>,
}

/// One direction of a connection, or a datagram socket's inbox.
pub struct Queue {
    msgs: IrqLock<VecDeque<Msg>>,
    /// Bytes waiting.
    bytes: AtomicUsize,
    /// No more writing: once the queue is empty, readers see the end.
    shut_write: AtomicBool,
    /// No reader any more: writers get EPIPE.
    shut_read: AtomicBool,
}

impl Queue {
    fn new() -> Arc<Queue> {
        Arc::new(Queue {
            msgs: IrqLock::new(VecDeque::new()),
            bytes: AtomicUsize::new(0),
            shut_write: AtomicBool::new(false),
            shut_read: AtomicBool::new(false),
        })
    }

    fn push(&self, m: Msg) {
        self.bytes.fetch_add(m.data.len(), Ordering::Relaxed);
        self.msgs.lock().push_back(m);
        EVENTS.notify_all();
    }

    fn waiting(&self) -> usize {
        self.bytes.load(Ordering::Relaxed)
    }

    fn is_empty(&self) -> bool {
        self.msgs.lock().is_empty()
    }

    /// Drops what is waiting (closing the descriptors in it).
    fn purge(&self) {
        let gone = core::mem::take(&mut *self.msgs.lock());
        self.bytes.store(0, Ordering::Relaxed);
        drop(gone);
    }
}

/// One end of a connection.
struct End {
    rx: Arc<Queue>,
    tx: Arc<Queue>,
    /// The process at the other end, for SO_PEERCRED.
    peer_pid: u32,
    peer_name: Option<Name>,
}

impl End {
    fn pair(a_pid: u32, a_name: Option<Name>, b_pid: u32, b_name: Option<Name>) -> (End, End) {
        let (x, y) = (Queue::new(), Queue::new());
        // a's end talks to b, and the other way round.
        let a = End { rx: x.clone(), tx: y.clone(), peer_pid: b_pid, peer_name: b_name };
        let b = End { rx: y, tx: x, peer_pid: a_pid, peer_name: a_name };
        (a, b)
    }
}

impl Drop for End {
    fn drop(&mut self) {
        self.tx.shut_write.store(true, Ordering::Relaxed);
        self.rx.shut_read.store(true, Ordering::Relaxed);
        self.rx.purge();
        EVENTS.notify_all();
    }
}

/// A listening socket's waiting connections.
struct Listener {
    backlog: IrqLock<VecDeque<End>>,
    max: AtomicUsize,
    pid: u32,
    name: Name,
    closed: AtomicBool,
}

impl Listener {
    fn close(&self) {
        self.closed.store(true, Ordering::Relaxed);
        let gone = core::mem::take(&mut *self.backlog.lock());
        drop(gone);
        EVENTS.notify_all();
    }
}

// ----------------------------------------------------------------------------
// The socket

enum St {
    /// A stream or seqpacket socket, not connected.
    Fresh,
    Listening(Arc<Listener>),
    Connected(End),
    /// A datagram socket: its inbox, and where send goes by default.
    Dgram { rx: Arc<Queue>, peer: Option<(Weak<Queue>, Name)> },
    Closed,
}

pub struct Unix {
    ty: Ty,
    id: u64,
    state: IrqLock<St>,
    name: IrqLock<Option<Name>>,
}

/// What a read took.
struct Got {
    n: usize,
    /// The whole message's length (datagrams), for MSG_TRUNC.
    full: usize,
    fds: Vec<Arc<Desc>>,
    from: Option<Name>,
}

fn new_socket(ty: Ty, st: St) -> Unix {
    Unix { ty, id: NEXT_ID.fetch_add(1, Ordering::Relaxed), state: IrqLock::new(st), name: IrqLock::new(None) }
}

fn install(s: Unix, flags: u64) -> i64 {
    let fl = O_RDWR | if flags & SOCK_NONBLOCK != 0 { O_NONBLOCK } else { 0 };
    fd::install(Desc::new(Kind::Unix(s), fl), flags & SOCK_CLOEXEC != 0)
}

pub fn socket(ty: u64) -> i64 {
    let Some(t) = Ty::parse(ty) else { return -err::EINVAL };
    let st = if t == Ty::Dgram { St::Dgram { rx: Queue::new(), peer: None } } else { St::Fresh };
    install(new_socket(t, st), ty)
}

pub fn socketpair(ty: u64, out: u64) -> i64 {
    let Some(t) = Ty::parse(ty) else { return -err::EINVAL };
    let pid = task::current_pid();
    let (a, b) = End::pair(pid, None, pid, None);
    let fa = install(new_socket(t, St::Connected(a)), ty);
    if fa < 0 {
        return fa;
    }
    let fb = install(new_socket(t, St::Connected(b)), ty);
    if fb < 0 {
        fd::sys_close(fa as i32);
        return fb;
    }
    match user_bytes(out, 8) {
        Ok(o) => {
            o[0..4].copy_from_slice(&(fa as i32).to_le_bytes());
            o[4..8].copy_from_slice(&(fb as i32).to_le_bytes());
            0
        }
        Err(e) => {
            fd::sys_close(fa as i32);
            fd::sys_close(fb as i32);
            e
        }
    }
}

fn nonblocking(d: &Desc, flags: u64) -> bool {
    d.nonblocking() || flags & MSG_DONTWAIT != 0
}

/// Waits a little for `ready`; the error a blocking call should stop with
/// (EAGAIN when non-blocking, or a signal).
fn pause(nb: bool, ready: impl Fn() -> bool) -> Result<(), i64> {
    if nb {
        return Err(-err::EAGAIN);
    }
    if let Some(e) = super::signal::interrupted() {
        return Err(e);
    }
    EVENTS.wait_for_timeout(50, || ready().then_some(()));
    Ok(())
}

impl Unix {
    pub fn close(&self) {
        let old = core::mem::replace(&mut *self.state.lock(), St::Closed);
        if let Some(n) = self.name.lock().as_ref() {
            unregister(&n.key, self.id);
        }
        match old {
            St::Listening(l) => l.close(),
            St::Dgram { rx, .. } => {
                rx.shut_read.store(true, Ordering::Relaxed);
                rx.purge();
            }
            _ => {}
        }
        EVENTS.notify_all();
    }

    pub fn bind(&self, addr: u64, len: u64) -> i64 {
        let name = match read_name(addr, len) {
            Ok(n) => n,
            Err(e) => return e,
        };
        if self.name.lock().is_some() {
            return -err::EINVAL;
        }
        // No name: one in the abstract namespace, as Linux's autobind.
        let name = name.unwrap_or_else(|| {
            let raw = alloc::format!("\0{:05x}", self.id & 0xfffff).into_bytes();
            Name { key: Key::Abstract(raw[1..].to_vec()), raw }
        });
        let what = match &*self.state.lock() {
            St::Fresh => Bound::Reserved,
            St::Dgram { rx, .. } => Bound::Dgram(Arc::downgrade(rx)),
            _ => return -err::EINVAL,
        };
        if let Err(e) = register(&name.key, self.id, what) {
            return e;
        }
        *self.name.lock() = Some(name);
        0
    }

    pub fn listen(&self, backlog: u64) -> i64 {
        if self.ty == Ty::Dgram {
            return -err::EOPNOTSUPP;
        }
        let max = (backlog as usize).clamp(1, 4096);
        let mut st = self.state.lock();
        match &*st {
            St::Listening(l) => {
                l.max.store(max, Ordering::Relaxed);
                return 0;
            }
            St::Fresh => {}
            _ => return -err::EINVAL,
        }
        let Some(name) = self.name.lock().clone() else { return -err::EINVAL };
        let l = Arc::new(Listener {
            backlog: IrqLock::new(VecDeque::new()),
            max: AtomicUsize::new(max),
            pid: task::current_pid(),
            name: name.clone(),
            closed: AtomicBool::new(false),
        });
        if let Err(e) = register(&name.key, self.id, Bound::Listener(Arc::downgrade(&l))) {
            return e;
        }
        *st = St::Listening(l);
        0
    }

    pub fn connect(&self, d: &Desc, addr: u64, len: u64) -> i64 {
        let name = match read_name(addr, len) {
            Ok(Some(n)) => n,
            Ok(None) => return -err::EINVAL,
            Err(e) => return e,
        };
        if self.ty == Ty::Dgram {
            let q = match find_dgram(&name) {
                Ok(q) => q,
                Err(e) => return e,
            };
            return match &mut *self.state.lock() {
                St::Dgram { peer, .. } => {
                    *peer = Some((q, name));
                    0
                }
                St::Connected(_) => -err::EISCONN,
                _ => -err::EINVAL,
            };
        }
        match &*self.state.lock() {
            St::Fresh => {}
            St::Connected(_) => return -err::EISCONN,
            _ => return -err::EINVAL,
        }
        let l = match find_listener(&name) {
            Ok(l) => l,
            Err(e) => return e,
        };
        let me = self.name.lock().clone();
        let (mine, theirs) = End::pair(task::current_pid(), me, l.pid, Some(l.name.clone()));
        let mut theirs = Some(theirs);
        loop {
            if l.closed.load(Ordering::Relaxed) {
                return -err::ECONNREFUSED;
            }
            {
                let mut q = l.backlog.lock();
                if q.len() < l.max.load(Ordering::Relaxed) {
                    q.push_back(theirs.take().unwrap());
                }
            }
            if theirs.is_none() {
                break;
            }
            let room = || l.closed.load(Ordering::Relaxed) || l.backlog.lock().len() < l.max.load(Ordering::Relaxed);
            if let Err(e) = pause(d.nonblocking(), room) {
                return e;
            }
        }
        EVENTS.notify_all();
        *self.state.lock() = St::Connected(mine);
        0
    }

    pub fn accept(&self, d: &Desc, addr: u64, lenp: u64, flags: u64) -> i64 {
        let l = match &*self.state.lock() {
            St::Listening(l) => l.clone(),
            _ => return -err::EINVAL,
        };
        let end = loop {
            if let Some(e) = l.backlog.lock().pop_front() {
                break e;
            }
            if let Err(e) = pause(d.nonblocking(), || !l.backlog.lock().is_empty()) {
                return e;
            }
        };
        EVENTS.notify_all();
        write_name(addr, lenp, end.peer_name.as_ref());
        let s = new_socket(self.ty, St::Connected(end));
        *s.name.lock() = Some(l.name.clone());
        install(s, flags)
    }

    /// Sends `data` (and `fds`, with the first byte), to `to` or the peer.
    fn send(&self, d: &Desc, data: &[u8], fds: Vec<Arc<Desc>>, to: Option<Name>, flags: u64) -> i64 {
        let nb = nonblocking(d, flags);
        // Where it goes: the connection, or a datagram socket's inbox.
        let target = match &*self.state.lock() {
            St::Connected(_) if to.is_some() && self.ty != Ty::Dgram => return -err::EISCONN,
            St::Connected(e) if to.is_none() => Ok(e.tx.clone()),
            St::Connected(_) | St::Dgram { .. } if to.is_some() => Err(None),
            St::Dgram { peer: Some((q, _)), .. } => Err(Some(q.clone())),
            _ => return -err::ENOTCONN,
        };
        let (tx, from) = match target {
            Ok(q) => (q, None),
            Err(peer) => {
                let peer = match (peer, &to) {
                    (Some(q), _) => q,
                    (None, Some(n)) => match find_dgram(n) {
                        Ok(q) => q,
                        Err(e) => return e,
                    },
                    (None, None) => return -err::ENOTCONN,
                };
                match peer.upgrade() {
                    Some(q) => (q, self.name.lock().clone()),
                    None => return -err::ECONNREFUSED,
                }
            }
        };
        let broken = |done: usize| -> i64 {
            if done > 0 {
                return done as i64;
            }
            if self.ty == Ty::Dgram {
                return -err::ECONNREFUSED;
            }
            if flags & MSG_NOSIGNAL == 0 {
                super::signal::broken_pipe();
            }
            -err::EPIPE
        };
        if self.ty != Ty::Stream {
            // One message, whole.
            if data.len() > CAP {
                return -err::EMSGSIZE;
            }
            let mut fds = Some(fds);
            loop {
                if tx.shut_read.load(Ordering::Relaxed) {
                    return broken(0);
                }
                if tx.waiting() + data.len() <= CAP || tx.is_empty() {
                    tx.push(Msg { data: data.to_vec(), off: 0, fds: fds.take().unwrap(), from: from.clone() });
                    return data.len() as i64;
                }
                let ready = || tx.shut_read.load(Ordering::Relaxed) || tx.waiting() + data.len() <= CAP;
                if let Err(e) = pause(nb, ready) {
                    return e;
                }
            }
        }
        let mut fds = Some(fds);
        let mut done = 0;
        while done < data.len() {
            if tx.shut_read.load(Ordering::Relaxed) {
                return broken(done);
            }
            let room = CAP.saturating_sub(tx.waiting());
            if room > 0 {
                let n = room.min(CHUNK).min(data.len() - done);
                tx.push(Msg { data: data[done..done + n].to_vec(), off: 0, fds: fds.take().unwrap_or_default(), from: None });
                done += n;
                continue;
            }
            let ready = || tx.shut_read.load(Ordering::Relaxed) || tx.waiting() < CAP;
            if let Err(e) = pause(nb, ready) {
                return if done > 0 { done as i64 } else { e };
            }
        }
        done as i64
    }

    fn inbox(&self) -> Result<Arc<Queue>, i64> {
        match &*self.state.lock() {
            St::Connected(e) => Ok(e.rx.clone()),
            St::Dgram { rx, .. } => Ok(rx.clone()),
            St::Closed => Err(-err::EBADF),
            St::Fresh | St::Listening(_) => Err(-err::EINVAL),
        }
    }

    /// Reads into `buf`, the program's memory: through a kernel buffer, so
    /// that no page fault comes while a queue is locked.
    fn recv(&self, d: &Desc, buf: &mut [u8], flags: u64) -> Result<Got, i64> {
        let mut tmp = alloc::vec![0u8; buf.len().min(1 << 20)];
        let g = self.take(d, &mut tmp, flags)?;
        buf[..g.n].copy_from_slice(&tmp[..g.n]);
        Ok(g)
    }

    /// Reads into a kernel buffer.
    fn take(&self, d: &Desc, buf: &mut [u8], flags: u64) -> Result<Got, i64> {
        let rx = self.inbox()?;
        let nb = nonblocking(d, flags);
        let peek = flags & MSG_PEEK != 0;
        let mut got = Got { n: 0, full: 0, fds: Vec::new(), from: None };
        if self.ty == Ty::Stream && buf.is_empty() {
            return Ok(got);
        }
        loop {
            {
                let mut q = rx.msgs.lock();
                if self.ty != Ty::Stream {
                    if let Some(m) = q.front() {
                        let k = m.data.len().min(buf.len());
                        buf[..k].copy_from_slice(&m.data[..k]);
                        got.n = k;
                        got.full = m.data.len();
                        got.from = m.from.clone();
                        if !peek {
                            let m = q.pop_front().unwrap();
                            rx.bytes.fetch_sub(m.data.len(), Ordering::Relaxed);
                            got.fds = m.fds;
                        }
                        drop(q);
                        EVENTS.notify_all();
                        return Ok(got);
                    }
                } else {
                    let mut i = 0;
                    while got.n < buf.len() && i < q.len() {
                        let m = &mut q[i];
                        let k = (m.data.len() - m.off).min(buf.len() - got.n);
                        buf[got.n..got.n + k].copy_from_slice(&m.data[m.off..m.off + k]);
                        got.n += k;
                        let has_fds = !m.fds.is_empty();
                        if peek {
                            i += 1;
                        } else {
                            m.off += k;
                            rx.bytes.fetch_sub(k, Ordering::Relaxed);
                            got.fds = core::mem::take(&mut m.fds);
                            if m.off == m.data.len() {
                                q.pop_front();
                            }
                        }
                        // The descriptors go with the bytes they came with.
                        if has_fds {
                            break;
                        }
                    }
                    let enough = got.n > 0 && (flags & MSG_WAITALL == 0 || got.n == buf.len() || !got.fds.is_empty() || peek);
                    drop(q);
                    if got.n > 0 {
                        EVENTS.notify_all();
                    }
                    if enough {
                        got.full = got.n;
                        return Ok(got);
                    }
                }
            }
            if rx.shut_write.load(Ordering::Relaxed) && rx.is_empty() {
                // The end: what was read so far, or 0.
                got.full = got.n;
                return Ok(got);
            }
            let ready = || !rx.is_empty() || rx.shut_write.load(Ordering::Relaxed);
            if let Err(e) = pause(nb, ready) {
                if got.n > 0 {
                    got.full = got.n;
                    return Ok(got);
                }
                return Err(e);
            }
        }
    }

    /// read(): the bytes only (descriptors sent along are closed).
    pub fn read(&self, d: &Desc, buf: &mut [u8]) -> i64 {
        match self.recv(d, buf, 0) {
            Ok(g) => g.n as i64,
            Err(e) => e,
        }
    }

    pub fn write(&self, d: &Desc, data: &[u8]) -> i64 {
        self.send(d, data, Vec::new(), None, 0)
    }

    pub fn shutdown(&self, how: u64) -> i64 {
        match &*self.state.lock() {
            St::Connected(e) => {
                if how == 0 || how == 2 {
                    e.rx.shut_read.store(true, Ordering::Relaxed);
                }
                if how == 1 || how == 2 {
                    e.tx.shut_write.store(true, Ordering::Relaxed);
                }
                EVENTS.notify_all();
                0
            }
            _ => -err::ENOTCONN,
        }
    }

    pub fn sockname(&self, addr: u64, lenp: u64) -> i64 {
        let n = self.name.lock().clone();
        write_name(addr, lenp, n.as_ref())
    }

    pub fn peername(&self, addr: u64, lenp: u64) -> i64 {
        let peer = match &*self.state.lock() {
            St::Connected(e) => e.peer_name.clone(),
            St::Dgram { peer: Some((_, n)), .. } => Some(n.clone()),
            _ => return -err::ENOTCONN,
        };
        write_name(addr, lenp, peer.as_ref())
    }

    /// (readable, writable, hang-up).
    pub fn readiness(&self) -> (bool, bool, bool) {
        match &*self.state.lock() {
            St::Connected(e) => {
                let ended = e.rx.shut_write.load(Ordering::Relaxed);
                let gone = e.tx.shut_read.load(Ordering::Relaxed);
                (!e.rx.is_empty() || ended, e.tx.waiting() < CAP || gone, ended && gone)
            }
            St::Listening(l) => (!l.backlog.lock().is_empty(), false, false),
            St::Dgram { rx, .. } => (!rx.is_empty(), true, false),
            St::Fresh | St::Closed => (false, false, true),
        }
    }

    /// FIONREAD: bytes waiting (a datagram socket: the next message's size).
    pub fn pending(&self) -> usize {
        let Ok(rx) = self.inbox() else { return 0 };
        if self.ty == Ty::Stream {
            return rx.waiting();
        }
        let q = rx.msgs.lock();
        q.front().map_or(0, |m| m.data.len())
    }

    /// getsockopt at SOL_SOCKET: the value, as the bytes to copy out.
    pub fn option(&self, name: u64) -> Option<Vec<u8>> {
        const SO_TYPE: u64 = 3;
        const SO_ERROR: u64 = 4;
        const SO_SNDBUF: u64 = 7;
        const SO_RCVBUF: u64 = 8;
        const SO_PASSCRED: u64 = 16;
        const SO_PEERCRED: u64 = 17;
        const SO_ACCEPTCONN: u64 = 30;
        const SO_PROTOCOL: u64 = 38;
        const SO_DOMAIN: u64 = 39;
        let int = |v: u32| Some(Vec::from(v.to_le_bytes()));
        match name {
            SO_TYPE => int(self.ty.number()),
            SO_ERROR | SO_PASSCRED | SO_PROTOCOL => int(0),
            SO_SNDBUF | SO_RCVBUF => int(CAP as u32),
            SO_DOMAIN => int(AF_UNIX as u32),
            SO_ACCEPTCONN => int(matches!(&*self.state.lock(), St::Listening(_)) as u32),
            SO_PEERCRED => {
                // struct ucred: pid, uid, gid (everything runs as root).
                let pid = match &*self.state.lock() {
                    St::Connected(e) => e.peer_pid,
                    _ => 0,
                };
                let mut v = Vec::from(pid.to_le_bytes());
                v.extend_from_slice(&[0; 8]);
                Some(v)
            }
            _ => None,
        }
    }
}

// ----------------------------------------------------------------------------
// The system calls that take a socket

pub fn sendto(s: &Unix, d: &Desc, data: &[u8], flags: u64, addr: u64, alen: u64) -> i64 {
    let to = if addr != 0 {
        match read_name(addr, alen) {
            Ok(n) => n,
            Err(e) => return e,
        }
    } else {
        None
    };
    s.send(d, data, Vec::new(), to, flags)
}

pub fn recvfrom(s: &Unix, d: &Desc, buf: &mut [u8], flags: u64, addr: u64, lenp: u64) -> i64 {
    match s.recv(d, buf, flags) {
        Ok(g) => {
            if s.ty != Ty::Stream {
                write_name(addr, lenp, g.from.as_ref());
            }
            if flags & MSG_TRUNC != 0 && s.ty != Ty::Stream {
                g.full as i64
            } else {
                g.n as i64
            }
        }
        Err(e) => e,
    }
}

/// The descriptors in a sendmsg control buffer (SCM_RIGHTS).
fn control_fds(ptr: u64, len: u64) -> Result<Vec<Arc<Desc>>, i64> {
    let mut fds = Vec::new();
    if ptr == 0 || len == 0 {
        return Ok(fds);
    }
    let c = user_bytes(ptr, len.min(64 * 1024))?.to_vec();
    let mut off = 0;
    while off + 16 <= c.len() {
        let clen = u64::from_le_bytes(c[off..off + 8].try_into().unwrap()) as usize;
        let level = u32::from_le_bytes(c[off + 8..off + 12].try_into().unwrap());
        let kind = u32::from_le_bytes(c[off + 12..off + 16].try_into().unwrap());
        if clen < 16 || off + clen > c.len() {
            return Err(-err::EINVAL);
        }
        if level == SOL_SOCKET && kind == SCM_RIGHTS {
            for f in c[off + 16..off + clen].as_chunks::<4>().0 {
                fds.push(fd::get(i32::from_le_bytes(*f)).ok_or(-err::EBADF)?);
            }
            if fds.len() > MAX_FDS {
                return Err(-err::EINVAL);
            }
        }
        // SCM_CREDENTIALS and the rest: nothing to do.
        off += (clen + 7) & !7;
    }
    Ok(fds)
}

/// sendmsg of `data` (already gathered), with the name and control buffer
/// in the msghdr at `msg`.
pub fn sendmsg(s: &Unix, d: &Desc, msg: u64, data: &[u8], flags: u64) -> i64 {
    let m = match user_bytes(msg, 56) {
        Ok(b) => b.to_vec(),
        Err(e) => return e,
    };
    let rd = |o: usize| u64::from_le_bytes(m[o..o + 8].try_into().unwrap());
    let (name, namelen, control, controllen) = (rd(0), rd(8) as u32 as u64, rd(32), rd(40));
    let fds = match control_fds(control, controllen) {
        Ok(f) => f,
        Err(e) => return e,
    };
    let to = if name != 0 && namelen != 0 {
        match read_name(name, namelen) {
            Ok(n) => n,
            Err(e) => return e,
        }
    } else {
        None
    };
    s.send(d, data, fds, to, flags)
}

/// What recvmsg got, for the caller to scatter: the length to return and
/// the bytes.
pub fn recvmsg(s: &Unix, d: &Desc, msg: u64, cap: usize, flags: u64) -> Result<(i64, Vec<u8>), i64> {
    let m = user_bytes(msg, 56)?.to_vec();
    let rd = |o: usize| u64::from_le_bytes(m[o..o + 8].try_into().unwrap());
    let (name, control, controllen) = (rd(0), rd(32), rd(40));
    let mut buf = alloc::vec![0u8; cap];
    let g = s.take(d, &mut buf, flags)?;
    buf.truncate(g.n);
    let mut mflags = 0u32;
    if s.ty != Ty::Stream && g.full > g.n {
        mflags |= MSG_TRUNC_FLAG;
    }
    if name != 0 {
        // msg_namelen is at offset 8.
        if s.ty != Ty::Stream {
            write_name(name, msg + 8, g.from.as_ref());
        } else if let Ok(b) = user_bytes(msg + 8, 4) {
            b.copy_from_slice(&0u32.to_le_bytes());
        }
    }
    // The descriptors that fit go in the control buffer; the others close.
    let mut used = 0u64;
    if !g.fds.is_empty() {
        let room = if control != 0 && controllen >= 16 { ((controllen - 16) / 4) as usize } else { 0 };
        let k = room.min(g.fds.len());
        if k < g.fds.len() {
            mflags |= MSG_CTRUNC;
        }
        if k > 0 {
            let cloexec = flags & MSG_CMSG_CLOEXEC != 0;
            let mut c = Vec::with_capacity(16 + 4 * k);
            c.extend_from_slice(&((16 + 4 * k) as u64).to_le_bytes());
            c.extend_from_slice(&SOL_SOCKET.to_le_bytes());
            c.extend_from_slice(&SCM_RIGHTS.to_le_bytes());
            for f in g.fds.iter().take(k) {
                let n = fd::install(f.clone(), cloexec);
                c.extend_from_slice(&(n as i32).to_le_bytes());
            }
            if let Ok(b) = user_bytes(control, c.len() as u64) {
                b.copy_from_slice(&c);
            }
            used = ((c.len() as u64 + 7) & !7).min(controllen);
        }
    }
    drop(g.fds);
    if let Ok(b) = user_bytes(msg + 40, 12) {
        b[0..8].copy_from_slice(&used.to_le_bytes());
        b[8..12].copy_from_slice(&mflags.to_le_bytes());
    }
    let n = if flags & MSG_TRUNC != 0 && s.ty != Ty::Stream { g.full } else { buf.len() };
    Ok((n as i64, buf))
}

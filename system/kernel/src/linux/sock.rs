//! BSD sockets for Linux programs: TCP and UDP over IPv4, on the kernel's
//! network stack. Blocking and non-blocking (connect returns EINPROGRESS,
//! then the socket turns writable), so event loops built on epoll work.

use super::fd::{self, Desc, Kind, O_NONBLOCK, O_RDWR};
use super::{err, user_bytes};
use crate::net::tcp::{self, Conn, Read};
use crate::net::{self, Ip};
use crate::sync::IrqLock;
use alloc::vec::Vec;

const AF_INET: u64 = 2;
const SOCK_STREAM: u64 = 1;
const SOCK_DGRAM: u64 = 2;
const SOCK_NONBLOCK: u64 = 0o4000;
const SOCK_CLOEXEC: u64 = 0o2000000;

const MSG_PEEK: u64 = 2;
const MSG_DONTWAIT: u64 = 0x40;

enum State {
    /// Not connected yet; maybe bound to a port.
    Fresh { port: u16 },
    Connecting(Conn),
    Connected(Conn),
    Listening(Conn, u16),
    Udp { port: u16, peer: Option<(Ip, u16)> },
    Closed,
}

pub struct Socket {
    stream: bool,
    state: IrqLock<State>,
    /// An error a non-blocking connect left, for SO_ERROR.
    error: IrqLock<i64>,
}

fn tcp_err(e: &str) -> i64 {
    match e {
        "connection refused" => -err::ECONNREFUSED,
        "connection timed out" | "timed out" => -err::ETIMEDOUT,
        "connection reset by peer" => -err::ECONNRESET,
        "the network is not configured" => -err::ENETUNREACH,
        "address in use" => -err::EADDRINUSE,
        "not connected" => -err::ENOTCONN,
        _ => -err::EPIPE,
    }
}

/// Reads a `struct sockaddr_in`.
fn read_addr(ptr: u64, len: u64) -> Result<(Ip, u16), i64> {
    if len < 8 {
        return Err(-err::EINVAL);
    }
    let b = user_bytes(ptr, 8)?;
    let family = u16::from_le_bytes([b[0], b[1]]) as u64;
    if family != AF_INET {
        return Err(-err::EAFNOSUPPORT);
    }
    Ok((Ip([b[4], b[5], b[6], b[7]]), u16::from_be_bytes([b[2], b[3]])))
}

/// Writes a `struct sockaddr_in` where the program asked (with its length).
fn write_addr(ptr: u64, lenp: u64, ip: Ip, port: u16) -> i64 {
    if ptr == 0 || lenp == 0 {
        return 0;
    }
    let cap = match user_bytes(lenp, 4) {
        Ok(b) => u32::from_le_bytes(b[..4].try_into().unwrap()) as usize,
        Err(e) => return e,
    };
    let mut sa = [0u8; 16];
    sa[0..2].copy_from_slice(&(AF_INET as u16).to_le_bytes());
    sa[2..4].copy_from_slice(&port.to_be_bytes());
    sa[4..8].copy_from_slice(&ip.0);
    let n = cap.min(16);
    match user_bytes(ptr, n as u64) {
        Ok(b) => b.copy_from_slice(&sa[..n]),
        Err(e) => return e,
    }
    if let Ok(b) = user_bytes(lenp, 4) {
        b.copy_from_slice(&16u32.to_le_bytes());
    }
    0
}

fn socket_of(fd: i32) -> Result<alloc::sync::Arc<Desc>, i64> {
    let d = fd::get(fd).ok_or(-err::EBADF)?;
    match d.kind {
        Kind::Socket(_) => Ok(d),
        _ => Err(-err::ENOTSOCK),
    }
}

fn sock(d: &Desc) -> &Socket {
    match &d.kind {
        Kind::Socket(s) => s,
        _ => unreachable!(),
    }
}

pub fn sys_socket(domain: u64, ty: u64, _proto: u64) -> i64 {
    if domain != AF_INET {
        return -err::EAFNOSUPPORT;
    }
    let base = ty & 0xf;
    let stream = match base {
        SOCK_STREAM => true,
        SOCK_DGRAM => false,
        _ => return -err::EINVAL,
    };
    let s = Socket { stream, state: IrqLock::new(State::Fresh { port: 0 }), error: IrqLock::new(0) };
    let flags = O_RDWR | if ty & SOCK_NONBLOCK != 0 { O_NONBLOCK } else { 0 };
    fd::install(Desc::new(Kind::Socket(s), flags), ty & SOCK_CLOEXEC != 0)
}

pub fn sys_socketpair(ptr: u64) -> i64 {
    // Two pipes would be needed for both directions; a single pipe pair
    // covers the common use (waking an event loop).
    fd::sys_pipe2(ptr, 0)
}

pub fn sys_connect(fd: i32, addr: u64, len: u64) -> i64 {
    let d = match socket_of(fd) {
        Ok(d) => d,
        Err(e) => return e,
    };
    let s = sock(&d);
    let (ip, port) = match read_addr(addr, len) {
        Ok(a) => a,
        Err(e) => return e,
    };
    if !s.stream {
        let mut st = s.state.lock();
        let my = match *st {
            State::Udp { port, .. } => port,
            State::Fresh { port } => {
                drop(st);
                let p = match net::udp_bind(port) {
                    Ok(p) => p,
                    Err(e) => return tcp_err(e),
                };
                st = s.state.lock();
                p
            }
            _ => return -err::EINVAL,
        };
        *st = State::Udp { port: my, peer: Some((ip, port)) };
        return 0;
    }
    let conn = {
        let st = s.state.lock();
        match *st {
            State::Connected(_) => return -err::EISCONN,
            State::Connecting(c) => Some(c),
            State::Fresh { .. } => None,
            _ => return -err::EINVAL,
        }
    };
    let conn = match conn {
        Some(c) => {
            if d.nonblocking() {
                return -err::EALREADY;
            }
            c
        }
        None => {
            let local = match *s.state.lock() {
                State::Fresh { port } => port,
                _ => 0,
            };
            match tcp::open(ip, port, local) {
                Ok(c) => {
                    *s.state.lock() = State::Connecting(c);
                    c
                }
                Err(e) => return tcp_err(e),
            }
        }
    };
    if d.nonblocking() {
        return -err::EINPROGRESS;
    }
    loop {
        match tcp::status(conn) {
            Some(Ok(())) => {
                *s.state.lock() = State::Connected(conn);
                return 0;
            }
            Some(Err(e)) => {
                tcp::close(conn);
                *s.state.lock() = State::Closed;
                return tcp_err(e);
            }
            None => {
                // The connection goes on; connect says EINTR.
                if super::signal::interrupted().is_some() {
                    return -err::EINTR;
                }
                net::EVENTS.wait_for_timeout(50, || tcp::status(conn).map(|_| ()));
            }
        }
    }
}

impl Socket {
    /// Turns a finished non-blocking connect into Connected (or an error).
    fn settle(&self) {
        let mut st = self.state.lock();
        if let State::Connecting(c) = *st {
            match tcp::status(c) {
                Some(Ok(())) => *st = State::Connected(c),
                Some(Err(e)) => {
                    *self.error.lock() = tcp_err(e);
                    tcp::close(c);
                    *st = State::Closed;
                }
                None => {}
            }
        }
    }

    pub fn readiness(&self) -> (bool, bool, bool) {
        self.settle();
        let st = self.state.lock();
        match *st {
            State::Connected(c) | State::Listening(c, _) => tcp::ready(c),
            State::Connecting(_) => (false, false, false),
            State::Udp { port, .. } => (net::udp_peek(port).is_some(), true, false),
            State::Fresh { .. } => (false, false, false),
            State::Closed => (true, true, true),
        }
    }

    pub fn pending(&self) -> usize {
        match *self.state.lock() {
            State::Connected(c) => tcp::pending(c),
            State::Udp { port, .. } => net::udp_peek(port).unwrap_or(0),
            _ => 0,
        }
    }

    pub fn close(&self) {
        let old = core::mem::replace(&mut *self.state.lock(), State::Closed);
        match old {
            State::Connected(c) | State::Connecting(c) | State::Listening(c, _) => tcp::close(c),
            State::Udp { port, .. } => net::udp_close(port),
            _ => {}
        }
    }

    pub fn send(&self, d: &Desc, data: &[u8], flags: u64) -> i64 {
        self.settle();
        let st = self.state.lock();
        match *st {
            State::Connected(c) => {
                drop(st);
                let nb = d.nonblocking() || flags & MSG_DONTWAIT != 0;
                let mut done = 0;
                loop {
                    match tcp::try_send(c, &data[done..]) {
                        Ok(n) => done += n,
                        Err(e) => return if done > 0 { done as i64 } else { tcp_err(e) },
                    }
                    if done == data.len() {
                        return done as i64;
                    }
                    if nb {
                        return if done > 0 { done as i64 } else { -err::EAGAIN };
                    }
                    if let Some(e) = super::signal::interrupted() {
                        return if done > 0 { done as i64 } else { e };
                    }
                    net::EVENTS.wait_for_timeout(50, || tcp::ready(c).1.then_some(()));
                }
            }
            State::Udp { port, peer: Some((ip, p)) } => {
                drop(st);
                match net::send_udp(ip, port, p, data) {
                    Ok(()) => data.len() as i64,
                    Err(_) => -err::ENETUNREACH,
                }
            }
            State::Udp { .. } => -err::EDESTADDRREQ,
            State::Connecting(_) => -err::EAGAIN,
            _ => -err::ENOTCONN,
        }
    }

    /// Receives into `buf`; also returns who sent it (UDP).
    pub fn recv(&self, d: &Desc, buf: &mut [u8], flags: u64) -> (i64, Option<(Ip, u16)>) {
        self.settle();
        let nb = d.nonblocking() || flags & MSG_DONTWAIT != 0;
        let st = self.state.lock();
        match *st {
            State::Connected(c) => {
                drop(st);
                loop {
                    if flags & MSG_PEEK != 0 && tcp::pending(c) > 0 {
                        return (tcp::peek(c, buf) as i64, None);
                    }
                    let r = if flags & MSG_PEEK != 0 { tcp::try_recv(c, &mut []) } else { tcp::try_recv(c, buf) };
                    match r {
                        Ok(Read::Data(n)) if n > 0 || buf.is_empty() && flags & MSG_PEEK == 0 => return (n as i64, None),
                        Ok(Read::Eof) => return (0, None),
                        Err(e) => return (tcp_err(e), None),
                        _ => {}
                    }
                    if nb {
                        return (-err::EAGAIN, None);
                    }
                    if let Some(e) = super::signal::interrupted() {
                        return (e, None);
                    }
                    net::EVENTS.wait_for_timeout(50, || tcp::ready(c).0.then_some(()));
                }
            }
            State::Udp { port, .. } => {
                drop(st);
                loop {
                    if let Some((ip, p, data)) = net::udp_try_recv(port) {
                        let n = data.len().min(buf.len());
                        buf[..n].copy_from_slice(&data[..n]);
                        return (n as i64, Some((ip, p)));
                    }
                    if nb {
                        return (-err::EAGAIN, None);
                    }
                    if let Some(e) = super::signal::interrupted() {
                        return (e, None);
                    }
                    net::EVENTS.wait_for_timeout(50, || net::udp_peek(port).map(|_| ()));
                }
            }
            State::Closed => (0, None),
            _ => (-err::ENOTCONN, None),
        }
    }
}

pub fn sys_bind(fd: i32, addr: u64, len: u64) -> i64 {
    let d = match socket_of(fd) {
        Ok(d) => d,
        Err(e) => return e,
    };
    let s = sock(&d);
    let (_, port) = match read_addr(addr, len) {
        Ok(a) => a,
        Err(e) => return e,
    };
    if s.stream {
        *s.state.lock() = State::Fresh { port };
        return 0;
    }
    match net::udp_bind(port) {
        Ok(p) => {
            *s.state.lock() = State::Udp { port: p, peer: None };
            0
        }
        Err(e) => tcp_err(e),
    }
}

pub fn sys_listen(fd: i32) -> i64 {
    let d = match socket_of(fd) {
        Ok(d) => d,
        Err(e) => return e,
    };
    let s = sock(&d);
    let port = match *s.state.lock() {
        State::Fresh { port } => port,
        State::Listening(..) => return 0,
        _ => return -err::EINVAL,
    };
    let port = if port == 0 { 40000 + (crate::time::uptime_ms() % 20000) as u16 } else { port };
    match tcp::listen(port) {
        Ok(c) => {
            *s.state.lock() = State::Listening(c, port);
            0
        }
        Err(e) => tcp_err(e),
    }
}

pub fn sys_accept4(fd: i32, addr: u64, lenp: u64, flags: u64) -> i64 {
    let d = match socket_of(fd) {
        Ok(d) => d,
        Err(e) => return e,
    };
    let s = sock(&d);
    let State::Listening(l, _) = *s.state.lock() else { return -err::EINVAL };
    let conn = loop {
        if let Some(c) = tcp::try_accept(l) {
            break c;
        }
        if d.nonblocking() {
            return -err::EAGAIN;
        }
        if let Some(e) = super::signal::interrupted() {
            return e;
        }
        net::EVENTS.wait_for_timeout(50, || tcp::ready(l).0.then_some(()));
    };
    if let Some((_, ip, port)) = tcp::endpoints(conn) {
        write_addr(addr, lenp, ip, port);
    }
    let ns = Socket { stream: true, state: IrqLock::new(State::Connected(conn)), error: IrqLock::new(0) };
    let fl = O_RDWR | if flags & SOCK_NONBLOCK != 0 { O_NONBLOCK } else { 0 };
    fd::install(Desc::new(Kind::Socket(ns), fl), flags & SOCK_CLOEXEC != 0)
}

pub fn sys_sendto(fd: i32, ptr: u64, len: u64, flags: u64, addr: u64, alen: u64) -> i64 {
    let d = match socket_of(fd) {
        Ok(d) => d,
        Err(e) => return e,
    };
    let s = sock(&d);
    let data = match user_bytes(ptr, len) {
        Ok(b) => b.to_vec(),
        Err(e) => return e,
    };
    if addr != 0 && !s.stream {
        let (ip, port) = match read_addr(addr, alen) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let my = {
            let st = s.state.lock();
            match *st {
                State::Udp { port, .. } => Some(port),
                _ => None,
            }
        };
        let my = match my {
            Some(p) => p,
            None => match net::udp_bind(0) {
                Ok(p) => {
                    *s.state.lock() = State::Udp { port: p, peer: None };
                    p
                }
                Err(e) => return tcp_err(e),
            },
        };
        return match net::send_udp(ip, my, port, &data) {
            Ok(()) => data.len() as i64,
            Err(_) => -err::ENETUNREACH,
        };
    }
    s.send(&d, &data, flags)
}

pub fn sys_recvfrom(fd: i32, ptr: u64, len: u64, flags: u64, addr: u64, lenp: u64) -> i64 {
    let d = match socket_of(fd) {
        Ok(d) => d,
        Err(e) => return e,
    };
    let buf = match user_bytes(ptr, len) {
        Ok(b) => b,
        Err(e) => return e,
    };
    let (n, from) = sock(&d).recv(&d, buf, flags);
    if let Some((ip, port)) = from {
        write_addr(addr, lenp, ip, port);
    }
    n
}

/// sendmsg/recvmsg: the data is in an iovec array.
pub fn sys_sendmsg(fd: i32, msg: u64, flags: u64) -> i64 {
    let m = match user_bytes(msg, 56) {
        Ok(b) => b.to_vec(),
        Err(e) => return e,
    };
    let rd = |o: usize| u64::from_le_bytes(m[o..o + 8].try_into().unwrap());
    let (name, namelen, iov, iovlen) = (rd(0), rd(8) as u32 as u64, rd(16), rd(24));
    let mut data = Vec::new();
    if let Err(e) = gather(iov, iovlen, |b| data.extend_from_slice(b)) {
        return e;
    }
    let d = match socket_of(fd) {
        Ok(d) => d,
        Err(e) => return e,
    };
    if name != 0 && !sock(&d).stream {
        let (ip, port) = match read_addr(name, namelen) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let my = match *sock(&d).state.lock() {
            State::Udp { port, .. } => Some(port),
            _ => None,
        };
        let my = match my {
            Some(p) => p,
            None => match net::udp_bind(0) {
                Ok(p) => {
                    *sock(&d).state.lock() = State::Udp { port: p, peer: None };
                    p
                }
                Err(e) => return tcp_err(e),
            },
        };
        return match net::send_udp(ip, my, port, &data) {
            Ok(()) => data.len() as i64,
            Err(_) => -err::ENETUNREACH,
        };
    }
    sock(&d).send(&d, &data, flags)
}

fn gather(iov: u64, count: u64, mut f: impl FnMut(&[u8])) -> Result<(), i64> {
    let v = user_bytes(iov, count.saturating_mul(16))?.to_vec();
    for c in v.chunks(16) {
        let base = u64::from_le_bytes(c[..8].try_into().unwrap());
        let len = u64::from_le_bytes(c[8..].try_into().unwrap());
        if len > 0 {
            f(user_bytes(base, len)?);
        }
    }
    Ok(())
}

pub fn sys_recvmsg(fd: i32, msg: u64, flags: u64) -> i64 {
    let m = match user_bytes(msg, 56) {
        Ok(b) => b.to_vec(),
        Err(e) => return e,
    };
    let rd = |o: usize| u64::from_le_bytes(m[o..o + 8].try_into().unwrap());
    let (name, iov, iovlen) = (rd(0), rd(16), rd(24));
    let d = match socket_of(fd) {
        Ok(d) => d,
        Err(e) => return e,
    };
    let total: u64 = match user_bytes(iov, iovlen.saturating_mul(16)) {
        Ok(v) => v.chunks(16).map(|c| u64::from_le_bytes(c[8..].try_into().unwrap())).sum(),
        Err(e) => return e,
    };
    let mut tmp = alloc::vec![0u8; total.min(1 << 20) as usize];
    let (n, from) = sock(&d).recv(&d, &mut tmp, flags);
    if n <= 0 {
        return n;
    }
    // Scatter into the buffers.
    let mut off = 0usize;
    let v = user_bytes(iov, iovlen * 16).map(|b| b.to_vec()).unwrap_or_default();
    for c in v.chunks(16) {
        if off >= n as usize {
            break;
        }
        let base = u64::from_le_bytes(c[..8].try_into().unwrap());
        let len = u64::from_le_bytes(c[8..].try_into().unwrap()) as usize;
        let k = len.min(n as usize - off);
        if let Ok(b) = user_bytes(base, k as u64) {
            b.copy_from_slice(&tmp[off..off + k]);
        }
        off += k;
    }
    if let (Some((ip, port)), true) = (from, name != 0) {
        // msg_namelen is at offset 8 (a u32).
        write_addr(name, msg + 8, ip, port);
    }
    // No control data, no flags.
    if let Ok(b) = user_bytes(msg + 40, 16) {
        b[0..8].copy_from_slice(&0u64.to_le_bytes()); // msg_controllen
        b[8..12].copy_from_slice(&0u32.to_le_bytes()); // msg_flags
    }
    n
}

pub fn sys_shutdown(fd: i32, how: u64) -> i64 {
    let d = match socket_of(fd) {
        Ok(d) => d,
        Err(e) => return e,
    };
    if how >= 1 {
        if let State::Connected(c) = *sock(&d).state.lock() {
            tcp::shutdown(c);
        }
    }
    0
}

pub fn sys_getsockname(fd: i32, addr: u64, lenp: u64) -> i64 {
    let d = match socket_of(fd) {
        Ok(d) => d,
        Err(e) => return e,
    };
    let me = net::local_ip().unwrap_or(Ip::ANY);
    let port = match *sock(&d).state.lock() {
        State::Connected(c) | State::Connecting(c) => tcp::endpoints(c).map(|e| e.0).unwrap_or(0),
        State::Listening(_, p) | State::Fresh { port: p } | State::Udp { port: p, .. } => p,
        State::Closed => 0,
    };
    write_addr(addr, lenp, me, port)
}

pub fn sys_getpeername(fd: i32, addr: u64, lenp: u64) -> i64 {
    let d = match socket_of(fd) {
        Ok(d) => d,
        Err(e) => return e,
    };
    let peer = match *sock(&d).state.lock() {
        State::Connected(c) => tcp::endpoints(c).map(|e| (e.1, e.2)),
        State::Udp { peer, .. } => peer,
        _ => None,
    };
    match peer {
        Some((ip, port)) => write_addr(addr, lenp, ip, port),
        None => -err::ENOTCONN,
    }
}

pub fn sys_getsockopt(fd: i32, level: u64, name: u64, val: u64, lenp: u64) -> i64 {
    const SOL_SOCKET: u64 = 1;
    const SO_TYPE: u64 = 3;
    const SO_ERROR: u64 = 4;
    let d = match socket_of(fd) {
        Ok(d) => d,
        Err(e) => return e,
    };
    let s = sock(&d);
    s.settle();
    let v: i32 = match (level, name) {
        (SOL_SOCKET, SO_ERROR) => {
            let e = core::mem::replace(&mut *s.error.lock(), 0);
            (-e) as i32
        }
        (SOL_SOCKET, SO_TYPE) => {
            if s.stream {
                1
            } else {
                2
            }
        }
        _ => 0,
    };
    if val != 0 {
        if let Ok(b) = user_bytes(val, 4) {
            b.copy_from_slice(&v.to_le_bytes());
        }
        if let Ok(b) = user_bytes(lenp, 4) {
            b.copy_from_slice(&4u32.to_le_bytes());
        }
    }
    0
}

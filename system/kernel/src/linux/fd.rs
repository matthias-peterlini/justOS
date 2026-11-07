//! File descriptors of Linux processes: files and folders of the justOS
//! volume, the console, pipes, eventfds, sockets ([`super::sock`],
//! [`super::unix`]) and epoll
//! sets ([`super::poll`]), plus the few virtual files programs expect
//! (`/dev/null`, `/dev/urandom`, `/etc/resolv.conf`, `/proc/self/...`).
//!
//! A descriptor is an `Arc<Desc>`: `dup` shares it, with its position and
//! flags, as on Linux. A memfd ([`super::shm`]) reads and writes like a
//! file.

use super::{err, errno, linux_path, show_path, user_bytes, with_state};
use crate::fs::{self, Path};
use crate::net::EVENTS;
use crate::sync::IrqLock;
use crate::task;
use crate::user;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

pub const O_WRONLY: u32 = 1;
pub const O_RDWR: u32 = 2;
pub const O_CREAT: u32 = 0o100;
pub const O_EXCL: u32 = 0o200;
pub const O_TRUNC: u32 = 0o1000;
pub const O_APPEND: u32 = 0o2000;
pub const O_NONBLOCK: u32 = 0o4000;
pub const O_DIRECTORY: u32 = 0o200000;
pub const O_CLOEXEC: u32 = 0o2000000;
pub const AT_FDCWD: i32 = -100;

pub struct Pipe {
    buf: IrqLock<VecDeque<u8>>,
    readers: AtomicU32,
    writers: AtomicU32,
}

const PIPE_SIZE: usize = 64 * 1024;

pub struct EventFd {
    count: AtomicU64,
    semaphore: bool,
}

pub enum Kind {
    File { path: Path, pos: IrqLock<u64>, append: bool },
    Dir { path: Path, names: IrqLock<Option<Vec<(String, bool)>>> },
    /// Fixed contents (generated at open).
    Virtual { data: Vec<u8>, pos: IrqLock<u64> },
    Null,
    Zero,
    Random,
    Console,
    PipeRead(Arc<Pipe>),
    PipeWrite(Arc<Pipe>),
    Event(Arc<EventFd>),
    /// Shared memory: memfd_create, or /dev/shm.
    Memfd { shm: Arc<super::shm::Shm>, pos: IrqLock<u64> },
    Socket(super::sock::Socket),
    Unix(super::unix::Unix),
    Epoll(super::poll::Epoll),
}

pub struct Desc {
    pub kind: Kind,
    /// O_NONBLOCK, O_APPEND and the access mode, as F_GETFL reports them.
    pub flags: AtomicU32,
}

impl Desc {
    pub fn new(kind: Kind, flags: u32) -> Arc<Desc> {
        match &kind {
            Kind::PipeRead(p) => {
                p.readers.fetch_add(1, Ordering::Relaxed);
            }
            Kind::PipeWrite(p) => {
                p.writers.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
        Arc::new(Desc { kind, flags: AtomicU32::new(flags) })
    }

    pub fn nonblocking(&self) -> bool {
        self.flags.load(Ordering::Relaxed) & O_NONBLOCK != 0
    }
}

impl Drop for Desc {
    fn drop(&mut self) {
        match &self.kind {
            Kind::PipeRead(p) => {
                p.readers.fetch_sub(1, Ordering::Relaxed);
                EVENTS.notify_all();
            }
            Kind::PipeWrite(p) => {
                p.writers.fetch_sub(1, Ordering::Relaxed);
                EVENTS.notify_all();
            }
            Kind::Socket(s) => s.close(),
            Kind::Unix(u) => u.close(),
            _ => {}
        }
    }
}

// ----------------------------------------------------------------------------
// The table

/// The descriptor `fd` of the calling process.
pub fn get(fd: i32) -> Option<Arc<Desc>> {
    with_state(|st| st.fds.get(&fd).map(|(d, _)| d.clone()))
}

/// Installs a descriptor at the lowest free number (at least `min`).
pub fn install(d: Arc<Desc>, cloexec: bool) -> i64 {
    install_from(d, cloexec, 0)
}

pub fn install_from(d: Arc<Desc>, cloexec: bool, min: i32) -> i64 {
    with_state(|st| {
        let mut fd = min.max(0);
        while st.fds.contains_key(&fd) {
            fd += 1;
        }
        if fd >= 4096 {
            return -err::EMFILE;
        }
        st.fds.insert(fd, (d, cloexec));
        fd as i64
    })
}

/// The justOS path behind a file descriptor (for `mmap` of a file).
pub fn file_path(fd: i32) -> Option<Path> {
    match &get(fd)?.kind {
        Kind::File { path, .. } => Some(path.clone()),
        _ => None,
    }
}

pub fn sys_close(fd: i32) -> i64 {
    // Dropped outside the state lock: closing a socket talks to the network.
    let d = with_state(|st| st.fds.remove(&fd));
    match d {
        Some(d) => {
            drop(d);
            0
        }
        None => -err::EBADF,
    }
}

/// close_range: closes the open descriptors from `first` to `last`, or
/// with CLOSE_RANGE_CLOEXEC only marks them close-on-exec (what programs
/// do before starting another one, instead of trying every number).
pub fn sys_close_range(first: u32, last: u32, flags: u32) -> i64 {
    const CLOSE_RANGE_UNSHARE: u32 = 2;
    const CLOSE_RANGE_CLOEXEC: u32 = 4;
    if first > last || flags & !(CLOSE_RANGE_UNSHARE | CLOSE_RANGE_CLOEXEC) != 0 {
        return -err::EINVAL;
    }
    let (lo, hi) = (first.min(i32::MAX as u32) as i32, last.min(i32::MAX as u32) as i32);
    let closed: Vec<(Arc<Desc>, bool)> = with_state(|st| {
        if flags & CLOSE_RANGE_CLOEXEC != 0 {
            for (_, (_, cloexec)) in st.fds.range_mut(lo..=hi) {
                *cloexec = true;
            }
            return Vec::new();
        }
        let fds: Vec<i32> = st.fds.range(lo..=hi).map(|(&fd, _)| fd).collect();
        fds.iter().filter_map(|fd| st.fds.remove(fd)).collect()
    });
    // Dropped outside the state lock, as in close.
    drop(closed);
    0
}

/// The open descriptor numbers of the calling process (/proc/self/fd).
pub fn open_numbers() -> Vec<i32> {
    with_state(|st| st.fds.keys().copied().collect())
}

pub fn sys_dup(fd: i32) -> i64 {
    match get(fd) {
        Some(d) => install(d, false),
        None => -err::EBADF,
    }
}

pub fn sys_dup3(old: i32, new: i32, flags: u32) -> i64 {
    let Some(d) = get(old) else { return -err::EBADF };
    if old == new {
        return if flags == u32::MAX { new as i64 } else { -err::EINVAL };
    }
    if !(0..4096).contains(&new) {
        return -err::EBADF;
    }
    // dup2 (flags u32::MAX) never marks the copy close-on-exec: programs
    // dup2 descriptors into place right before starting another program.
    let cloexec = flags != u32::MAX && flags & O_CLOEXEC != 0;
    let prev = with_state(|st| st.fds.insert(new, (d, cloexec)));
    drop(prev);
    new as i64
}

pub fn sys_pipe2(ptr: u64, flags: u32) -> i64 {
    let out = match user_bytes(ptr, 8) {
        Ok(b) => b,
        Err(e) => return e,
    };
    let p = Arc::new(Pipe { buf: IrqLock::new(VecDeque::new()), readers: AtomicU32::new(0), writers: AtomicU32::new(0) });
    let nb = flags & O_NONBLOCK;
    let cloexec = flags & O_CLOEXEC != 0;
    let r = install(Desc::new(Kind::PipeRead(p.clone()), nb), cloexec);
    let w = install(Desc::new(Kind::PipeWrite(p), O_WRONLY | nb), cloexec);
    out[0..4].copy_from_slice(&(r as i32).to_le_bytes());
    out[4..8].copy_from_slice(&(w as i32).to_le_bytes());
    0
}

pub fn sys_eventfd2(init: u32, flags: u32) -> i64 {
    const EFD_SEMAPHORE: u32 = 1;
    let e = Arc::new(EventFd { count: AtomicU64::new(init as u64), semaphore: flags & EFD_SEMAPHORE != 0 });
    install(Desc::new(Kind::Event(e), O_RDWR | (flags & O_NONBLOCK)), flags & O_CLOEXEC != 0)
}

// ----------------------------------------------------------------------------
// Opening

/// Contents of the virtual files, by Linux path.
fn virtual_file(text: &str) -> Option<Kind> {
    let data = |s: String| Some(Kind::Virtual { data: s.into_bytes(), pos: IrqLock::new(0) });
    match text {
        "/dev/null" => Some(Kind::Null),
        "/dev/zero" => Some(Kind::Zero),
        "/dev/urandom" | "/dev/random" => Some(Kind::Random),
        "/dev/tty" | "/dev/console" | "/dev/stdin" | "/dev/stdout" | "/dev/stderr" => Some(Kind::Console),
        "/etc/resolv.conf" => {
            let dns = crate::net::dns_server().map(|ip| alloc::format!("{ip}")).unwrap_or_else(|| String::from("10.0.2.3"));
            data(alloc::format!("nameserver {dns}\n"))
        }
        "/etc/hosts" => data(String::from("127.0.0.1 localhost\n::1 localhost\n")),
        "/etc/passwd" => data(String::from("root:x:0:0:root:/home:/bin/sh\n")),
        "/etc/group" => data(String::from("root:x:0:\n")),
        "/etc/localtime" => None,
        "/proc/self/maps" => {
            let mut s = String::new();
            for (a, b, prot, off, name) in super::mm::areas() {
                let r = |bit, c| if prot & bit != 0 { c } else { '-' };
                let line = alloc::format!("{a:x}-{b:x} {}{}{}p {off:08x} 00:00 0", r(1, 'r'), r(2, 'w'), r(4, 'x'));
                s.push_str(&alloc::format!("{line:<72} {name}\n"));
            }
            data(s)
        }
        // One file system: GIO (through libmount) reads these to know the
        // mounts it shows and watches.
        "/proc/mounts" | "/proc/self/mounts" | "/etc/mtab" => data(String::from("/dev/root / justfs rw,relatime 0 0\n")),
        "/proc/self/mountinfo" => data(String::from("1 1 0:1 / / rw,relatime shared:1 - justfs /dev/root rw\n")),
        "/proc/self/cmdline" => data(alloc::format!("{}\0", user::current_name())),
        "/proc/cpuinfo" => data(String::from("processor\t: 0\nmodel name\t: justOS virtual CPU\ncpu cores\t: 1\n\n")),
        "/proc/meminfo" => {
            let (free, total) = crate::mem::frame::stats();
            data(alloc::format!("MemTotal: {} kB\nMemFree: {} kB\nMemAvailable: {} kB\n", total / 1024, free / 1024, free / 1024))
        }
        "/proc/self/stat" => data(alloc::format!("{} (prog) R 1 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 0 0 0\n", task::current_pid())),
        _ => None,
    }
}

pub fn sys_openat(dirfd: i32, path_ptr: u64, flags: u32) -> i64 {
    let text = match user::user_cstr(path_ptr, 4096) {
        Ok(t) => t,
        Err(e) => return -errno(e),
    };
    if let Some(name) = super::shm::named(&text) {
        return super::shm::open_named(name, flags);
    }
    if dirfd != AT_FDCWD && !text.starts_with('/') {
        // Relative to an open folder.
        match get(dirfd).map(|d| match &d.kind {
            Kind::Dir { path, .. } => Some(path.clone()),
            _ => None,
        }) {
            Some(Some(dir)) => return open_path(&dir_join(&dir, &text), &text, flags),
            Some(None) => return -err::ENOTDIR,
            None => return -err::EBADF,
        }
    }
    let path = match linux_path(&text) {
        Ok(p) => p,
        Err(e) => return e,
    };
    open_path(&path, &text, flags)
}

fn dir_join(dir: &Path, rel: &str) -> Path {
    Path::parse(rel, dir).unwrap_or_else(|_| dir.clone())
}

fn open_path(path: &Path, text: &str, flags: u32) -> i64 {
    let cloexec = flags & O_CLOEXEC != 0;
    let keep = flags & (O_NONBLOCK | O_APPEND | 3);
    if let Some(kind) = virtual_file(text) {
        return install(Desc::new(kind, keep), cloexec);
    }
    if matches!(text.trim_end_matches('/'), "/proc/self/fd" | "/proc/thread-self/fd") {
        // The open descriptors, as a folder of numbers (programs read it to
        // close what a child must not inherit).
        let mut names = vec![(String::from("."), true), (String::from(".."), true)];
        names.extend(open_numbers().into_iter().map(|n| (alloc::format!("{n}"), false)));
        return install(Desc::new(Kind::Dir { path: path.clone(), names: IrqLock::new(Some(names)) }, keep), cloexec);
    }
    let writable = flags & (O_WRONLY | O_RDWR) != 0;
    match fs::stat(path) {
        Ok(st) if st.kind == justfs::Kind::Dir => {
            if writable {
                return -err::EISDIR;
            }
            return install(Desc::new(Kind::Dir { path: path.clone(), names: IrqLock::new(None) }, keep), cloexec);
        }
        Ok(_) => {
            if flags & O_CREAT != 0 && flags & O_EXCL != 0 {
                return -err::EEXIST;
            }
            if flags & O_TRUNC != 0 && writable {
                if let Err(e) = fs::write_file(path, &[]) {
                    return -errno(user::fs_error(e));
                }
            }
        }
        Err(fs::Error::NotFound) if flags & O_CREAT != 0 => {
            if let Err(e) = fs::write_file(path, &[]) {
                return -errno(user::fs_error(e));
            }
        }
        Err(e) => return -errno(user::fs_error(e)),
    }
    if flags & O_DIRECTORY != 0 {
        return -err::ENOTDIR;
    }
    let append = flags & O_APPEND != 0;
    install(Desc::new(Kind::File { path: path.clone(), pos: IrqLock::new(0), append }, keep), cloexec)
}

// ----------------------------------------------------------------------------
// Reading and writing

/// Blocks (unless non-blocking) until `ready` says so; checks for kill.
pub fn wait_until(d: &Desc, mut ready: impl FnMut() -> bool) -> Result<(), i64> {
    loop {
        if ready() {
            return Ok(());
        }
        if d.nonblocking() {
            return Err(-err::EAGAIN);
        }
        if let Some(e) = super::signal::interrupted() {
            return Err(e);
        }
        EVENTS.wait_for_timeout(20, || ready().then_some(()));
    }
}

pub fn read_desc(d: &Desc, buf: &mut [u8]) -> i64 {
    if buf.is_empty() {
        return 0;
    }
    match &d.kind {
        Kind::File { path, pos, .. } => {
            let at = *pos.lock();
            let mut tmp = vec![0u8; buf.len().min(1 << 20)];
            match fs::read_at(path, at, &mut tmp) {
                Ok(n) => {
                    buf[..n].copy_from_slice(&tmp[..n]);
                    *pos.lock() = at + n as u64;
                    n as i64
                }
                Err(e) => -errno(user::fs_error(e)),
            }
        }
        Kind::Virtual { data, pos } => {
            let mut p = pos.lock();
            let at = (*p as usize).min(data.len());
            let n = (data.len() - at).min(buf.len());
            buf[..n].copy_from_slice(&data[at..at + n]);
            *p += n as u64;
            n as i64
        }
        Kind::Dir { .. } => -err::EISDIR,
        Kind::Null => 0,
        Kind::Zero => {
            buf.fill(0);
            buf.len() as i64
        }
        Kind::Random => {
            crate::rand::fill(buf);
            buf.len() as i64
        }
        Kind::Console => super::tty::read(d, buf),
        Kind::PipeRead(p) => {
            let r = wait_until(d, || !p.buf.lock().is_empty() || p.writers.load(Ordering::Relaxed) == 0);
            if let Err(e) = r {
                return e;
            }
            let mut q = p.buf.lock();
            let n = q.len().min(buf.len());
            for (dst, src) in buf.iter_mut().zip(q.drain(..n)) {
                *dst = src;
            }
            drop(q);
            EVENTS.notify_all();
            n as i64
        }
        Kind::Event(e) => {
            if buf.len() < 8 {
                return -err::EINVAL;
            }
            if let Err(x) = wait_until(d, || e.count.load(Ordering::Relaxed) > 0) {
                return x;
            }
            let v = if e.semaphore {
                e.count.fetch_sub(1, Ordering::Relaxed);
                1
            } else {
                e.count.swap(0, Ordering::Relaxed)
            };
            buf[..8].copy_from_slice(&v.to_le_bytes());
            EVENTS.notify_all();
            8
        }
        Kind::Memfd { shm, pos } => {
            let at = *pos.lock();
            let n = shm.read_at(at, buf);
            *pos.lock() = at + n as u64;
            n as i64
        }
        Kind::Socket(s) => s.recv(d, buf, 0).0,
        Kind::Unix(u) => u.read(d, buf),
        Kind::PipeWrite(_) | Kind::Epoll(_) => -err::EBADF,
    }
}

pub fn write_desc(d: &Desc, data: &[u8]) -> i64 {
    match &d.kind {
        Kind::File { path, pos, append } => {
            let at = if *append { fs::stat(path).map(|s| s.size).unwrap_or(0) } else { *pos.lock() };
            super::mm::forget_file(path);
            match fs::write_at(path, at, data) {
                Ok(()) => {
                    *pos.lock() = at + data.len() as u64;
                    data.len() as i64
                }
                Err(e) => -errno(user::fs_error(e)),
            }
        }
        Kind::Console => {
            super::tty::write(data);
            data.len() as i64
        }
        Kind::Null | Kind::Zero | Kind::Random => data.len() as i64,
        Kind::PipeWrite(p) => {
            let mut done = 0;
            while done < data.len() {
                if p.readers.load(Ordering::Relaxed) == 0 {
                    if done > 0 {
                        return done as i64;
                    }
                    super::signal::broken_pipe();
                    return -err::EPIPE;
                }
                let r = wait_until(d, || p.buf.lock().len() < PIPE_SIZE || p.readers.load(Ordering::Relaxed) == 0);
                if let Err(e) = r {
                    return if done > 0 { done as i64 } else { e };
                }
                let mut q = p.buf.lock();
                let n = (PIPE_SIZE - q.len().min(PIPE_SIZE)).min(data.len() - done);
                q.extend(&data[done..done + n]);
                done += n;
                drop(q);
                EVENTS.notify_all();
            }
            done as i64
        }
        Kind::Event(e) => {
            if data.len() < 8 {
                return -err::EINVAL;
            }
            let v = u64::from_le_bytes(data[..8].try_into().unwrap());
            e.count.fetch_add(v, Ordering::Relaxed);
            EVENTS.notify_all();
            8
        }
        Kind::Memfd { shm, pos } => {
            let at = *pos.lock();
            match shm.write_at(at, data) {
                Ok(n) => {
                    *pos.lock() = at + n as u64;
                    n as i64
                }
                Err(e) => e,
            }
        }
        Kind::Socket(s) => s.send(d, data, 0),
        Kind::Unix(u) => u.write(d, data),
        _ => -err::EBADF,
    }
}

pub fn sys_read(fd: i32, ptr: u64, len: u64) -> i64 {
    let Some(d) = get(fd) else { return -err::EBADF };
    let buf = match user_bytes(ptr, len) {
        Ok(b) => b,
        Err(e) => return e,
    };
    read_desc(&d, buf)
}

pub fn sys_write(fd: i32, ptr: u64, len: u64) -> i64 {
    let Some(d) = get(fd) else { return -err::EBADF };
    let data = match user_bytes(ptr, len) {
        Ok(b) => b,
        Err(e) => return e,
    };
    write_desc(&d, data)
}

pub fn sys_pread(fd: i32, ptr: u64, len: u64, off: u64) -> i64 {
    let Some(d) = get(fd) else { return -err::EBADF };
    let buf = match user_bytes(ptr, len) {
        Ok(b) => b,
        Err(e) => return e,
    };
    match &d.kind {
        Kind::File { path, .. } => {
            let mut tmp = vec![0u8; buf.len().min(1 << 20)];
            match fs::read_at(path, off, &mut tmp) {
                Ok(n) => {
                    buf[..n].copy_from_slice(&tmp[..n]);
                    n as i64
                }
                Err(e) => -errno(user::fs_error(e)),
            }
        }
        Kind::Virtual { data, .. } => {
            let at = (off as usize).min(data.len());
            let n = (data.len() - at).min(buf.len());
            buf[..n].copy_from_slice(&data[at..at + n]);
            n as i64
        }
        Kind::Memfd { shm, .. } => shm.read_at(off, buf) as i64,
        _ => -err::ESPIPE,
    }
}

pub fn sys_pwrite(fd: i32, ptr: u64, len: u64, off: u64) -> i64 {
    let Some(d) = get(fd) else { return -err::EBADF };
    let data = match user_bytes(ptr, len) {
        Ok(b) => b,
        Err(e) => return e,
    };
    match &d.kind {
        Kind::File { path, .. } => {
            super::mm::forget_file(path);
            match fs::write_at(path, off, data) {
                Ok(()) => data.len() as i64,
                Err(e) => -errno(user::fs_error(e)),
            }
        }
        Kind::Memfd { shm, .. } => match shm.write_at(off, data) {
            Ok(n) => n as i64,
            Err(e) => e,
        },
        _ => -err::ESPIPE,
    }
}

pub fn sys_lseek(fd: i32, off: i64, whence: i32) -> i64 {
    let Some(d) = get(fd) else { return -err::EBADF };
    let (pos, size) = match &d.kind {
        Kind::File { path, pos, .. } => (pos, fs::stat(path).map(|s| s.size).unwrap_or(0) as i64),
        Kind::Virtual { data, pos } => (pos, data.len() as i64),
        Kind::Memfd { shm, pos } => (pos, shm.size() as i64),
        Kind::Dir { names, .. } => {
            if off == 0 && whence == 0 {
                *names.lock() = None; // rewinddir
                return 0;
            }
            return -err::EINVAL;
        }
        Kind::Null | Kind::Zero | Kind::Random => return 0,
        _ => return -err::ESPIPE,
    };
    let mut p = pos.lock();
    let base = match whence {
        0 => 0,
        1 => *p as i64,
        2 => size,
        _ => return -err::EINVAL,
    };
    let new = base + off;
    if new < 0 {
        return -err::EINVAL;
    }
    *p = new as u64;
    new
}

pub fn sys_ftruncate(fd: i32, len: u64) -> i64 {
    if let Some(d) = get(fd) {
        if let Kind::Memfd { shm, .. } = &d.kind {
            return match shm.truncate(len) {
                Ok(()) => 0,
                Err(e) => e,
            };
        }
    }
    let Some(path) = file_path(fd) else { return -err::EINVAL };
    truncate_path(&path, len)
}

pub fn truncate_path(path: &Path, len: u64) -> i64 {
    super::mm::forget_file(path);
    let mut data = match fs::read_file(path) {
        Ok(d) => d,
        Err(e) => return -errno(user::fs_error(e)),
    };
    data.resize(len as usize, 0);
    match fs::write_file(path, &data) {
        Ok(()) => 0,
        Err(e) => -errno(user::fs_error(e)),
    }
}

// ----------------------------------------------------------------------------
// stat

/// What `stat` reports: kind, size, time.
pub struct StatInfo {
    pub mode: u32,
    pub size: u64,
    pub time: u64,
    pub ino: u64,
}

pub fn stat_of_path(path: &Path) -> Result<StatInfo, i64> {
    stat_info(path, fs::stat(path))
}

/// Like [`stat_of_path`], but a link describes itself.
pub fn lstat_of_path(path: &Path) -> Result<StatInfo, i64> {
    stat_info(path, fs::lstat(path))
}

fn stat_info(path: &Path, st: Result<fs::Stat, justfs::Error>) -> Result<StatInfo, i64> {
    if matches!(st, Err(justfs::Error::NotFound)) && super::unix::is_bound(path) {
        return Ok(StatInfo { mode: 0o140777, size: 0, time: crate::time::unix_now(), ino: 4 });
    }
    let st = st.map_err(|e| -errno(user::fs_error(e)))?;
    // Names without an extension are programs (Linux ones are); the
    // shared libraries' names (libc.so.6) look like programs too.
    let mode = if st.kind == justfs::Kind::Dir {
        0o040755
    } else if st.kind == justfs::Kind::Link {
        0o120777
    } else if path.name().contains(".so") {
        0o100755
    } else if path.name().contains('.') {
        0o100644
    } else {
        0o100755
    };
    // The inode: the node, with the volume in the high bits (two paths to
    // one file, through links, give the same number, as the dynamic loader
    // expects when it checks whether a library is already loaded).
    let vol = path.volume.to_ascii_lowercase().bytes().fold(0u64, |h, b| h.wrapping_mul(31).wrapping_add(b as u64));
    let ino = (vol << 32) | st.node as u64;
    Ok(StatInfo { mode, size: st.size, time: st.modified + super::UNIX_2000, ino })
}

/// The stat of a /dev/shm path: the shared memory there, or ENOENT.
fn shm_stat(text: &str) -> Option<Result<StatInfo, i64>> {
    let name = super::shm::named(text)?;
    Some(match super::shm::named_size(name) {
        Some(size) => Ok(StatInfo { mode: 0o100600, size, time: crate::time::unix_now(), ino: 5 }),
        None => Err(-err::ENOENT),
    })
}

pub fn stat_of_desc(d: &Desc) -> Result<StatInfo, i64> {
    let simple = |mode: u32| Ok(StatInfo { mode, size: 0, time: crate::time::unix_now(), ino: 2 });
    match &d.kind {
        Kind::File { path, .. } | Kind::Dir { path, .. } => stat_of_path(path),
        Kind::Virtual { data, .. } => Ok(StatInfo { mode: 0o100444, size: data.len() as u64, time: crate::time::unix_now(), ino: 3 }),
        Kind::Memfd { shm, .. } => Ok(StatInfo { mode: 0o100777, size: shm.size(), time: crate::time::unix_now(), ino: Arc::as_ptr(shm) as u64 >> 4 }),
        Kind::Console => simple(0o020620),
        Kind::Null | Kind::Zero | Kind::Random => simple(0o020666),
        Kind::PipeRead(_) | Kind::PipeWrite(_) => simple(0o010600),
        Kind::Socket(_) | Kind::Unix(_) => simple(0o140777),
        Kind::Event(_) | Kind::Epoll(_) => simple(0o100600),
    }
}

pub fn write_stat(ptr: u64, s: &StatInfo) -> i64 {
    let buf = match user_bytes(ptr, 144) {
        Ok(b) => b,
        Err(e) => return e,
    };
    buf.fill(0);
    let put = |buf: &mut [u8], off: usize, v: u64| buf[off..off + 8].copy_from_slice(&v.to_le_bytes());
    put(buf, 0, 0x803); // st_dev
    put(buf, 8, s.ino);
    put(buf, 16, 1); // st_nlink
    buf[24..28].copy_from_slice(&s.mode.to_le_bytes());
    put(buf, 48, s.size);
    put(buf, 56, 4096); // st_blksize
    put(buf, 64, s.size.div_ceil(512));
    for off in [72, 88, 104] {
        put(buf, off, s.time);
    }
    0
}

fn path_arg(dirfd: i32, ptr: u64) -> Result<(Path, String), i64> {
    let text = user::user_cstr(ptr, 4096).map_err(|e| -errno(e))?;
    if dirfd != AT_FDCWD && !text.starts_with('/') && !text.is_empty() {
        if let Some(d) = get(dirfd) {
            if let Kind::Dir { path, .. } = &d.kind {
                return Ok((dir_join(path, &text), text));
            }
        }
    }
    Ok((linux_path(&text)?, text))
}

/// Don't follow a symbolic link at the end of the path.
pub const AT_SYMLINK_NOFOLLOW: u64 = 0x100;

pub fn sys_newfstatat(dirfd: i32, path_ptr: u64, buf: u64, flags: u64) -> i64 {
    const AT_EMPTY_PATH: u64 = 0x1000;
    let text = user::user_cstr(path_ptr, 4096).unwrap_or_default();
    if text.is_empty() && flags & AT_EMPTY_PATH != 0 {
        return sys_fstat(dirfd, buf);
    }
    if let Some(kind) = virtual_file(&text) {
        let d = Desc::new(kind, 0);
        return match stat_of_desc(&d) {
            Ok(s) => write_stat(buf, &s),
            Err(e) => e,
        };
    }
    if let Some(info) = shm_stat(&text) {
        return match info {
            Ok(s) => write_stat(buf, &s),
            Err(e) => e,
        };
    }
    let (path, _) = match path_arg(dirfd, path_ptr) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let info = if flags & AT_SYMLINK_NOFOLLOW != 0 { lstat_of_path(&path) } else { stat_of_path(&path) };
    match info {
        Ok(s) => write_stat(buf, &s),
        Err(e) => e,
    }
}

pub fn sys_fstat(fd: i32, buf: u64) -> i64 {
    let Some(d) = get(fd) else { return -err::EBADF };
    match stat_of_desc(&d) {
        Ok(s) => write_stat(buf, &s),
        Err(e) => e,
    }
}

pub fn sys_statx(dirfd: i32, path_ptr: u64, flags: u64, buf: u64) -> i64 {
    const AT_EMPTY_PATH: u64 = 0x1000;
    let text = user::user_cstr(path_ptr, 4096).unwrap_or_default();
    let info = if text.is_empty() && flags & AT_EMPTY_PATH != 0 {
        match get(dirfd) {
            Some(d) => stat_of_desc(&d),
            None => return -err::EBADF,
        }
    } else if let Some(kind) = virtual_file(&text) {
        stat_of_desc(&Desc::new(kind, 0))
    } else if let Some(info) = shm_stat(&text) {
        info
    } else {
        match path_arg(dirfd, path_ptr) {
            Ok((p, _)) if flags & AT_SYMLINK_NOFOLLOW != 0 => lstat_of_path(&p),
            Ok((p, _)) => stat_of_path(&p),
            Err(e) => return e,
        }
    };
    let s = match info {
        Ok(s) => s,
        Err(e) => return e,
    };
    let out = match user_bytes(buf, 256) {
        Ok(b) => b,
        Err(e) => return e,
    };
    out.fill(0);
    out[0..4].copy_from_slice(&0x7ffu32.to_le_bytes()); // stx_mask: the basic fields
    out[4..8].copy_from_slice(&4096u32.to_le_bytes()); // stx_blksize
    out[16..20].copy_from_slice(&1u32.to_le_bytes()); // stx_nlink
    out[28..30].copy_from_slice(&(s.mode as u16).to_le_bytes());
    out[32..40].copy_from_slice(&s.ino.to_le_bytes());
    out[40..48].copy_from_slice(&s.size.to_le_bytes());
    out[48..56].copy_from_slice(&s.size.div_ceil(512).to_le_bytes());
    for off in [64usize, 80, 96, 112] {
        out[off..off + 8].copy_from_slice(&s.time.to_le_bytes());
    }
    0
}

pub fn sys_faccessat(dirfd: i32, path_ptr: u64) -> i64 {
    let text = user::user_cstr(path_ptr, 4096).unwrap_or_default();
    if virtual_file(&text).is_some() {
        return 0;
    }
    if let Some(info) = shm_stat(&text) {
        return info.map_or_else(|e| e, |_| 0);
    }
    match path_arg(dirfd, path_ptr) {
        Ok((p, _)) if fs::exists(&p) || super::unix::is_bound(&p) => 0,
        Ok(_) => -err::ENOENT,
        Err(e) => e,
    }
}

// ----------------------------------------------------------------------------
// Folders

pub fn sys_getdents64(fd: i32, ptr: u64, cap: u64) -> i64 {
    let Some(d) = get(fd) else { return -err::EBADF };
    let Kind::Dir { path, names } = &d.kind else { return -err::ENOTDIR };
    if names.lock().is_none() {
        let list = match fs::list_dir(path) {
            Ok(l) => l,
            Err(e) => return -errno(user::fs_error(e)),
        };
        let mut v = vec![(String::from("."), true), (String::from(".."), true)];
        v.extend(list.into_iter().map(|(e, st)| (e.name, st.kind == justfs::Kind::Dir)));
        *names.lock() = Some(v);
    }
    let buf = match user_bytes(ptr, cap) {
        Ok(b) => b,
        Err(e) => return e,
    };
    let mut guard = names.lock();
    let list = guard.as_mut().unwrap();
    let mut off = 0usize;
    while let Some((name, dir)) = list.first() {
        let reclen = (19 + name.len() + 1 + 7) & !7;
        if off + reclen > buf.len() {
            if off == 0 {
                return -err::EINVAL;
            }
            break;
        }
        let e = &mut buf[off..off + reclen];
        e.fill(0);
        e[0..8].copy_from_slice(&((off as u64) + 2).to_le_bytes());
        e[8..16].copy_from_slice(&((off + reclen) as u64).to_le_bytes());
        e[16..18].copy_from_slice(&(reclen as u16).to_le_bytes());
        e[18] = if *dir { 4 } else { 8 }; // DT_DIR / DT_REG
        e[19..19 + name.len()].copy_from_slice(name.as_bytes());
        off += reclen;
        list.remove(0);
    }
    off as i64
}

pub fn sys_mkdirat(dirfd: i32, path_ptr: u64) -> i64 {
    match path_arg(dirfd, path_ptr) {
        Ok((p, _)) => match fs::make_dir(&p) {
            Ok(()) => 0,
            Err(e) => -errno(user::fs_error(e)),
        },
        Err(e) => e,
    }
}

pub fn sys_unlinkat(dirfd: i32, path_ptr: u64) -> i64 {
    let text = user::user_cstr(path_ptr, 4096).unwrap_or_default();
    if let Some(name) = super::shm::named(&text) {
        return super::shm::unlink_named(name);
    }
    match path_arg(dirfd, path_ptr) {
        // A local socket's name.
        Ok((p, _)) if super::unix::unlink(&p) => 0,
        Ok((p, _)) => {
            // Mappings of it keep what they map.
            super::mm::forget_file(&p);
            match fs::remove(&p) {
                Ok(()) => 0,
                Err(e) => -errno(user::fs_error(e)),
            }
        }
        Err(e) => e,
    }
}

pub fn sys_renameat(olddir: i32, old: u64, newdir: i32, new: u64) -> i64 {
    let (from, to) = match (path_arg(olddir, old), path_arg(newdir, new)) {
        (Ok((a, _)), Ok((b, _))) => (a, b),
        (Err(e), _) | (_, Err(e)) => return e,
    };
    // Mappings of either keep what they map.
    super::mm::forget_file(&from);
    super::mm::forget_file(&to);
    if fs::exists(&to) && !fs::is_dir(&to) {
        let _ = fs::remove(&to);
    }
    match fs::rename(&from, &to) {
        Ok(()) => 0,
        Err(e) => -errno(user::fs_error(e)),
    }
}

pub fn sys_readlinkat(dirfd: i32, path_ptr: u64, buf: u64, cap: u64) -> i64 {
    let text = user::user_cstr(path_ptr, 4096).unwrap_or_default();
    let target = match text.as_str() {
        "/proc/self/exe" => super::exe_path(),
        t if t.starts_with("/proc/self/fd/") => {
            let n: i32 = t[14..].parse().unwrap_or(-1);
            match get(n).map(|d| match &d.kind {
                Kind::File { path, .. } | Kind::Dir { path, .. } => show_path(path),
                Kind::Memfd { shm, .. } => shm.name.clone(),
                _ => String::from("anon_inode:[justos]"),
            }) {
                Some(s) => s,
                None => return -err::ENOENT,
            }
        }
        _ => match path_arg(dirfd, path_ptr) {
            Ok((p, _)) => match fs::read_link(&p) {
                Ok(t) => t,
                Err(justfs::Error::InvalidName) => return -err::EINVAL, // not a link
                Err(e) => return -errno(user::fs_error(e)),
            },
            Err(e) => return e,
        },
    };
    let out = match user_bytes(buf, cap) {
        Ok(b) => b,
        Err(e) => return e,
    };
    let n = target.len().min(out.len());
    out[..n].copy_from_slice(&target.as_bytes()[..n]);
    n as i64
}

/// symlink / symlinkat: a link at `path` pointing to `target`.
pub fn sys_symlinkat(target_ptr: u64, dirfd: i32, path_ptr: u64) -> i64 {
    let target = match user::user_cstr(target_ptr, 4096) {
        Ok(t) => t,
        Err(e) => return -errno(e),
    };
    let (path, _) = match path_arg(dirfd, path_ptr) {
        Ok(p) => p,
        Err(e) => return e,
    };
    match fs::symlink(&path, &target) {
        Ok(()) => 0,
        Err(e) => -errno(user::fs_error(e)),
    }
}

// ----------------------------------------------------------------------------
// fcntl and ioctl

pub fn sys_fcntl(fd: i32, cmd: u64, arg: u64) -> i64 {
    const F_DUPFD: u64 = 0;
    const F_GETFD: u64 = 1;
    const F_SETFD: u64 = 2;
    const F_GETFL: u64 = 3;
    const F_SETFL: u64 = 4;
    const F_SETLK: u64 = 6;
    const F_SETLKW: u64 = 7;
    const F_GETLK: u64 = 5;
    const F_DUPFD_CLOEXEC: u64 = 1030;
    const F_ADD_SEALS: u64 = 1033;
    const F_GET_SEALS: u64 = 1034;
    let Some(d) = get(fd) else { return -err::EBADF };
    match cmd {
        F_DUPFD => install_from(d, false, arg as i32),
        F_DUPFD_CLOEXEC => install_from(d, true, arg as i32),
        F_GETFD => with_state(|st| st.fds.get(&fd).map(|(_, c)| *c as i64).unwrap_or(0)),
        F_SETFD => {
            with_state(|st| {
                if let Some(e) = st.fds.get_mut(&fd) {
                    e.1 = arg & 1 != 0;
                }
            });
            0
        }
        F_GETFL => d.flags.load(Ordering::Relaxed) as i64,
        F_SETFL => {
            let keep = d.flags.load(Ordering::Relaxed) & 3;
            d.flags.store(keep | (arg as u32 & (O_NONBLOCK | O_APPEND)), Ordering::Relaxed);
            0
        }
        F_GETLK | F_SETLK | F_SETLKW => 0, // one process: no locks to fight over
        F_ADD_SEALS => match &d.kind {
            Kind::Memfd { shm, .. } => shm.add_seals(arg as u32),
            _ => -err::EINVAL,
        },
        F_GET_SEALS => match &d.kind {
            Kind::Memfd { shm, .. } => shm.seals() as i64,
            _ => -err::EINVAL,
        },
        _ => -err::EINVAL,
    }
}

pub fn sys_ioctl(fd: i32, req: u64, arg: u64) -> i64 {
    const FIONBIO: u64 = 0x5421;
    const FIONREAD: u64 = 0x541B;
    const FIOCLEX: u64 = 0x5451;
    let Some(d) = get(fd) else { return -err::EBADF };
    match req {
        FIONBIO => {
            let on = user_bytes(arg, 4).map(|b| b[0] != 0).unwrap_or(false);
            if on {
                d.flags.fetch_or(O_NONBLOCK, Ordering::Relaxed);
            } else {
                d.flags.fetch_and(!O_NONBLOCK, Ordering::Relaxed);
            }
            0
        }
        FIOCLEX => 0,
        FIONREAD => {
            let n = match &d.kind {
                Kind::PipeRead(p) => p.buf.lock().len() as u32,
                Kind::Socket(s) => s.pending() as u32,
                Kind::Unix(u) => u.pending() as u32,
                Kind::Console => super::tty::pending() as u32,
                _ => 0,
            };
            match user_bytes(arg, 4) {
                Ok(b) => {
                    b.copy_from_slice(&n.to_le_bytes());
                    0
                }
                Err(e) => e,
            }
        }
        _ => match &d.kind {
            Kind::Console => super::tty::ioctl(req, arg),
            _ => -err::ENOTTY,
        },
    }
}

// ----------------------------------------------------------------------------
// Readiness, for poll and epoll

/// (readable, writable, hang-up or error).
pub fn readiness(d: &Desc) -> (bool, bool, bool) {
    match &d.kind {
        Kind::PipeRead(p) => {
            let gone = p.writers.load(Ordering::Relaxed) == 0;
            (!p.buf.lock().is_empty() || gone, false, gone)
        }
        Kind::PipeWrite(p) => {
            let gone = p.readers.load(Ordering::Relaxed) == 0;
            (false, p.buf.lock().len() < PIPE_SIZE || gone, gone)
        }
        Kind::Event(e) => (e.count.load(Ordering::Relaxed) > 0, true, false),
        Kind::Socket(s) => s.readiness(),
        Kind::Unix(u) => u.readiness(),
        Kind::Console => (super::tty::pending() > 0, true, false),
        Kind::Epoll(e) => (e.any_ready(), false, false),
        _ => (true, true, false),
    }
}

/// Descriptors whose readiness never changes (files, generated files,
/// devices that never block): an edge-triggered watch sees one edge.
pub fn always_ready(d: &Desc) -> bool {
    matches!(d.kind, Kind::File { .. } | Kind::Dir { .. } | Kind::Virtual { .. } | Kind::Null | Kind::Zero | Kind::Random | Kind::Memfd { .. })
}

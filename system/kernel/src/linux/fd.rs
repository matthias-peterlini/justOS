//! File descriptors of Linux processes: files and folders of the justOS
//! volume, the console, pipes, eventfds, sockets ([`super::sock`],
//! [`super::unix`]), epoll sets ([`super::poll`]) and inotify instances
//! ([`super::inotify`]), plus the few virtual files programs expect
//! (`/dev/null`, `/dev/urandom`, `/etc/resolv.conf`, `/proc/self/...`).
//!
//! A descriptor is an `Arc<Desc>`: `dup` shares it, with its position and
//! flags, as on Linux. A memfd ([`super::shm`]) reads and writes like a
//! file.

use super::{err, errno, linux_path, show_path, user_bytes, with_state};
use crate::fs::{self, Path};
use crate::net::EVENTS;
use crate::sync::IrqLock;
use crate::task::{self, WaitQueue};
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
    /// Told whenever the pipe fills, empties or loses an end.
    wq: Arc<WaitQueue>,
}

const PIPE_SIZE: usize = 64 * 1024;

pub struct EventFd {
    count: AtomicU64,
    semaphore: bool,
    wq: Arc<WaitQueue>,
}

pub enum Kind {
    File { path: Path, pos: IrqLock<u64>, append: bool },
    Dir { path: Path, names: IrqLock<Option<Vec<(String, bool)>>> },
    /// Fixed contents (generated at open), and where writes go if it takes
    /// them (/proc/<pid>/uid_map and the like).
    Virtual { data: Vec<u8>, pos: IrqLock<u64>, sink: Option<super::ns::Sink> },
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
    Inotify(Arc<super::inotify::Inotify>),
    /// /dev/binder and its siblings: a binder process (Android's IPC).
    Binder(Arc<super::binder::Open>),
    /// /dev/ashmem: a region of Android's shared memory.
    Ashmem(Arc<super::ashmem::Ashmem>),
    /// timerfd_create.
    Timer(Arc<super::timerfd::TimerFd>),
    /// signalfd.
    Signal(Arc<super::signal::SignalFd>),
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
                p.wq.notify_all();
            }
            Kind::PipeWrite(p) => {
                p.writers.fetch_sub(1, Ordering::Relaxed);
                p.wq.notify_all();
            }
            Kind::Socket(s) => s.close(),
            Kind::Unix(u) => u.close(),
            // Watchers learn that a file written through it is complete.
            Kind::File { path, .. } if self.flags.load(Ordering::Relaxed) & (O_WRONLY | O_RDWR) != 0 => {
                fs::touched(path, fs::change::CLOSE_WRITE);
            }
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

/// fsync, fdatasync, syncfs: what was written reaches the disk (only files
/// have anything to write).
pub fn sys_fsync(fd: i32) -> i64 {
    let Some(d) = get(fd) else { return -err::EBADF };
    if let Kind::File { path, .. } = &d.kind {
        super::mm::sync_file(path);
        crate::fs::flush(path);
    }
    0
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
    let p = Arc::new(Pipe { buf: IrqLock::new(VecDeque::new()), readers: AtomicU32::new(0), writers: AtomicU32::new(0), wq: Arc::default() });
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
    let e = Arc::new(EventFd { count: AtomicU64::new(init as u64), semaphore: flags & EFD_SEMAPHORE != 0, wq: Arc::default() });
    install(Desc::new(Kind::Event(e), O_RDWR | (flags & O_NONBLOCK)), flags & O_CLOEXEC != 0)
}

// ----------------------------------------------------------------------------
// Opening

/// A process's own /proc files: /proc/self/…, /proc/thread-self/… and
/// /proc/<pid>/… of a running process (/proc/<pid>/task/<tid>/… are the
/// process's): which process, and the name in its folder.
/// The folder itself gives an empty name.
fn proc_of(text: &str) -> Option<(u32, &str)> {
    let rest = text.strip_prefix("/proc/")?;
    let (who, rest) = rest.split_once('/').unwrap_or((rest, ""));
    let pid = match who {
        "self" | "thread-self" => task::current_pid(),
        n => n.parse().ok().filter(|&p| user::is_running(p))?,
    };
    let rest = match rest.strip_prefix("task/") {
        Some(r) => r.split_once('/').map_or("", |(_, name)| name),
        None => rest,
    };
    Some((pid, rest))
}

/// The folders under /proc/<pid>/: the process's own and its fd, fdinfo,
/// ns and task folders, with what is in them.
fn virtual_dir(text: &str) -> Option<Vec<(String, bool)>> {
    let entries = |names: &[&str], dir: bool| names.iter().map(|n| (String::from(*n), dir)).collect::<Vec<_>>();
    let mut v = entries(&[".", ".."], true);
    if text.trim_end_matches('/') == "/proc" {
        // The processes the caller can see, and the links to itself.
        v.extend(entries(&["self", "thread-self", "cpuinfo", "meminfo", "mounts"], false));
        for p in user::processes() {
            let local = super::ns::local(p.pid);
            if local != 0 {
                v.push((alloc::format!("{local}"), true));
            }
        }
        return Some(v);
    }
    let (pid, name) = proc_of(text.trim_end_matches('/'))?;
    match name {
        "" => {
            v.extend(entries(&["fd", "fdinfo", "ns", "task"], true));
            v.extend(entries(&["cmdline", "comm", "exe", "gid_map", "maps", "mountinfo", "mounts", "setgroups", "stat", "status", "uid_map"], false));
        }
        // Only a process's own descriptors are listed.
        "fd" | "fdinfo" if pid == task::current_pid() => {
            v.extend(open_numbers().into_iter().map(|n| (alloc::format!("{n}"), false)));
        }
        "fd" | "fdinfo" => {}
        "ns" => v.extend(entries(&super::ns::NS_NAMES, false)),
        "task" => v.push((alloc::format!("{}", super::ns::local(pid)), true)),
        _ => return None,
    }
    Some(v)
}

/// What /proc/<pid>/ns/<name> points to.
fn ns_link(text: &str) -> Option<String> {
    let (pid, name) = proc_of(text)?;
    super::ns::link(pid, name.strip_prefix("ns/")?)
}

/// The stat of a /proc folder, namespace link or other virtual file, if
/// `text` names one.
fn virtual_stat(text: &str, follow: bool) -> Option<Result<StatInfo, i64>> {
    let now = crate::time::unix_now();
    if let Some(link) = ns_link(text) {
        // A namespace link leads to the namespace, whose number is its inode.
        let id = link.split_once('[').and_then(|(_, n)| n.trim_end_matches(']').parse().ok()).unwrap_or(0);
        let mode = if follow { 0o100444 } else { 0o120777 };
        return Some(Ok(StatInfo { mode, size: 0, time: now, ino: id, rdev: 0 }));
    }
    if virtual_dir(text).is_some() {
        return Some(Ok(StatInfo { mode: 0o040555, size: 0, time: now, ino: 4, rdev: 0 }));
    }
    if let Some(ctx) = super::binder::device(text) {
        // Without opening it (an open is a binder process).
        return Some(Ok(StatInfo { mode: 0o020666, size: 0, time: now, ino: 2 + BINDER_RDEV + ctx as u64, rdev: BINDER_RDEV + ctx as u64 }));
    }
    if super::ashmem::device(text) {
        // The same numbers as an open region's (libcutils compares them).
        let rdev = super::ashmem::RDEV;
        return Some(Ok(StatInfo { mode: 0o020666, size: 0, time: now, ino: 2 + rdev, rdev }));
    }
    if let Some(kind) = virtual_file(text) {
        return Some(stat_of_desc(&Desc::new(kind, 0)));
    }
    shm_stat(text)
}

/// Whether `text` is a folder, real or virtual (chdir and chroot ask).
pub fn is_dir(text: &str) -> bool {
    virtual_dir(text).is_some() || linux_path(text).is_ok_and(|p| fs::is_dir(&p))
}

/// The files in a process's /proc folder.
fn proc_file(pid: u32, name: &str) -> Option<String> {
    let (prog, parent, threads) = user::proc_info(pid)?;
    let (me, parent) = (super::ns::local(pid), super::ns::local(parent));
    // The name as Linux keeps it ("comm"): at most 15 bytes.
    let comm: String = prog.chars().take(15).collect();
    Some(match name {
        "maps" => {
            let mut s = String::new();
            for (a, b, prot, off, file) in super::mm::areas(pid) {
                let r = |bit, c| if prot & bit != 0 { c } else { '-' };
                let line = alloc::format!("{a:x}-{b:x} {}{}{}p {off:08x} 00:00 0", r(1, 'r'), r(2, 'w'), r(4, 'x'));
                s.push_str(&alloc::format!("{line:<72} {file}\n"));
            }
            s
        }
        // One file system: GIO (through libmount) reads these to know the
        // mounts it shows and watches.
        "mounts" => String::from("/dev/root / justfs rw,relatime 0 0\n"),
        "mountinfo" => String::from("1 1 0:1 / / rw,relatime shared:1 - justfs /dev/root rw\n"),
        "cmdline" => alloc::format!("{prog}\0"),
        "comm" => alloc::format!("{comm}\n"),
        "stat" => alloc::format!("{me} ({comm}) R {parent} {me} {me} 0 -1 0 0 0 0 0 0 0 0 0 20 0 {threads} 0 0 0 0\n"),
        "status" => {
            let (uid, gid) = (super::ns::uid(), super::ns::gid());
            let ids = super::ns::ids_of(pid).iter().map(|i| alloc::format!("\t{i}")).collect::<String>();
            alloc::format!(
                "Name:\t{comm}\nUmask:\t0022\nState:\tR (running)\nTgid:\t{me}\nNgid:\t0\nPid:\t{me}\nPPid:\t{parent}\nTracerPid:\t0\n\
                 Uid:\t{uid}\t{uid}\t{uid}\t{uid}\nGid:\t{gid}\t{gid}\t{gid}\t{gid}\nNStgid:{ids}\nNSpid:{ids}\n\
                 Threads:\t{threads}\nSeccomp:\t{}\n",
                super::seccomp::mode_of(pid)
            )
        }
        n if n.starts_with("fdinfo/") && pid == task::current_pid() => {
            let fd: i32 = n[7..].parse().ok()?;
            let d = get(fd)?;
            let pos = match &d.kind {
                Kind::File { pos, .. } | Kind::Virtual { pos, .. } | Kind::Memfd { pos, .. } => *pos.lock(),
                _ => 0,
            };
            alloc::format!("pos:\t{pos}\nflags:\t0{:o}\nmnt_id:\t1\nino:\t0\n", d.flags.load(Ordering::Relaxed))
        }
        _ => return None,
    })
}

/// Contents of the virtual files, by Linux path.
fn virtual_file(text: &str) -> Option<Kind> {
    let data = |s: String| Some(Kind::Virtual { data: s.into_bytes(), pos: IrqLock::new(0), sink: None });
    if let Some((pid, name)) = proc_of(text) {
        if let Some((now, sink)) = super::ns::id_file(pid, name) {
            return Some(Kind::Virtual { data: now.into_bytes(), pos: IrqLock::new(0), sink: Some(sink) });
        }
        return proc_file(pid, name).and_then(data);
    }
    match text {
        "/dev/null" => Some(Kind::Null),
        "/dev/zero" => Some(Kind::Zero),
        "/dev/urandom" | "/dev/random" => Some(Kind::Random),
        "/dev/tty" | "/dev/console" | "/dev/stdin" | "/dev/stdout" | "/dev/stderr" => Some(Kind::Console),
        _ if super::binder::device(text).is_some() => super::binder::device(text).map(|ctx| Kind::Binder(super::binder::open(ctx))),
        _ if super::ashmem::device(text) => Some(Kind::Ashmem(super::ashmem::open())),
        "/etc/resolv.conf" => {
            let dns = crate::net::dns_server().map(|ip| alloc::format!("{ip}")).unwrap_or_else(|| String::from("10.0.2.3"));
            data(alloc::format!("nameserver {dns}\n"))
        }
        "/etc/hosts" => data(String::from("127.0.0.1 localhost\n::1 localhost\n")),
        "/etc/passwd" => data(String::from("root:x:0:0:root:/home:/bin/sh\n")),
        "/etc/group" => data(String::from("root:x:0:\n")),
        "/etc/localtime" => None,
        "/proc/mounts" | "/etc/mtab" => proc_file(task::current_pid(), "mounts").and_then(data),
        "/proc/cpuinfo" => data(String::from("processor\t: 0\nmodel name\t: justOS virtual CPU\ncpu cores\t: 1\n\n")),
        "/proc/meminfo" => {
            let (free, total) = crate::mem::frame::stats();
            data(alloc::format!("MemTotal: {} kB\nMemFree: {} kB\nMemAvailable: {} kB\n", total / 1024, free / 1024, free / 1024))
        }
        _ => None,
    }
}

/// open / openat / creat: `mode` gives a file it creates its permissions
/// (less the umask).
pub fn sys_openat(dirfd: i32, path_ptr: u64, flags: u32, mode: u32) -> i64 {
    let text = match super::path_text_at(dirfd, path_ptr) {
        Ok(t) => t,
        Err(e) => return e,
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
            Some(Some(dir)) => {
                let path = dir_join(&dir, &text);
                return open_path(&path, &show_path(&path), flags, mode);
            }
            Some(None) => return -err::ENOTDIR,
            None => return -err::EBADF,
        }
    }
    let path = match linux_path(&text) {
        Ok(p) => p,
        Err(e) => return e,
    };
    open_path(&path, &text, flags, mode)
}

fn dir_join(dir: &Path, rel: &str) -> Path {
    Path::parse(&super::no_volume(rel), dir).unwrap_or_else(|_| dir.clone())
}

fn open_path(path: &Path, text: &str, flags: u32, mode: u32) -> i64 {
    let cloexec = flags & O_CLOEXEC != 0;
    let keep = flags & (O_NONBLOCK | O_APPEND | 3);
    if let Some(kind) = virtual_file(text) {
        return install(Desc::new(kind, keep), cloexec);
    }
    if let Some(names) = virtual_dir(text) {
        // The open descriptors are a folder of numbers (programs read it to
        // close what a child must not inherit).
        if flags & (O_WRONLY | O_RDWR) != 0 {
            return -err::EISDIR;
        }
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
                super::mm::forget_file(path);
                if let Err(e) = fs::write_file(path, &[]) {
                    return -errno(user::fs_error(e));
                }
                super::mm::file_truncated(path, 0);
            }
        }
        Err(fs::Error::NotFound) if flags & O_CREAT != 0 => {
            if let Err(e) = fs::write_file(path, &[]) {
                return -errno(user::fs_error(e));
            }
            let _ = fs::set_mode(path, super::masked(mode));
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
        let w = Waits::of(&[d]);
        w.wait(None, || ready().then_some(()));
    }
}

/// What wakes a thread that waits for descriptors to change: the queues
/// their sources tell, and for the rest, the network's queue or a look
/// every few milliseconds (the console).
#[derive(Default)]
pub struct Waits {
    queues: Vec<Arc<WaitQueue>>,
    events: bool,
    tick: bool,
    /// Signals (signalfd).
    signals: bool,
    /// When a timer goes off (uptime ms).
    deadline: Option<u64>,
}

/// How long a wait sleeps at most even when nothing tells it: a safety
/// net (signals wake their thread; every source tells its queue).
const NAP_MS: u64 = 200;

impl Waits {
    pub fn of(descs: &[&Desc]) -> Waits {
        let mut w = Waits::default();
        for d in descs {
            w.add(d, 0);
        }
        w
    }

    /// Adds what wakes a wait on `d` (`depth`: epoll sets inside epoll
    /// sets).
    pub fn add(&mut self, d: &Desc, depth: u32) {
        match &d.kind {
            Kind::PipeRead(p) | Kind::PipeWrite(p) => self.queue(&p.wq),
            Kind::Event(e) => self.queue(&e.wq),
            Kind::Inotify(i) => self.queue(&i.wq),
            Kind::Binder(b) => self.queue(&b.wq),
            Kind::Timer(t) => {
                self.queue(&t.wq);
                if let Some(at) = t.deadline_ms() {
                    self.deadline = Some(self.deadline.map_or(at, |d| d.min(at)));
                }
            }
            Kind::Signal(_) => self.signals = true,
            Kind::Unix(u) => u.waits(self),
            Kind::Epoll(e) if depth < 4 => e.waits(self, depth + 1),
            Kind::Epoll(_) | Kind::Socket(_) => self.events = true,
            Kind::Console => self.tick = true,
            _ => {}
        }
    }

    pub fn queue(&mut self, wq: &Arc<WaitQueue>) {
        if !self.queues.iter().any(|x| Arc::ptr_eq(x, wq)) {
            self.queues.push(wq.clone());
        }
    }

    /// Sleeps until a source changes, `until` (uptime ms) comes or a short
    /// while passes, unless `check` finds something first (see
    /// [`task::wait_any`]).
    pub fn wait<T>(&self, until: Option<u64>, check: impl FnOnce() -> Option<T>) -> Option<T> {
        let mut refs: Vec<&WaitQueue> = self.queues.iter().map(|q| &**q).collect();
        if self.events {
            refs.push(&EVENTS);
        }
        if self.signals {
            refs.push(&super::signal::SIGNALS);
        }
        let nap = crate::time::uptime_ms() + if self.tick { 10 } else { NAP_MS };
        let mut limit = until.map_or(nap, |u| u.min(nap));
        if let Some(d) = self.deadline {
            limit = limit.min(d);
        }
        task::wait_any(&refs, Some(limit), check)
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
                    super::mm::file_read(path, at, &mut tmp[..n]);
                    buf[..n].copy_from_slice(&tmp[..n]);
                    *pos.lock() = at + n as u64;
                    n as i64
                }
                Err(e) => -errno(user::fs_error(e)),
            }
        }
        Kind::Virtual { data, pos, .. } => {
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
            p.wq.notify_all();
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
            e.wq.notify_all();
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
        Kind::Inotify(i) => i.read(d, buf),
        // Binder is read and written through its ioctl.
        Kind::Binder(_) => -err::EINVAL,
        Kind::Ashmem(a) => a.read(buf),
        Kind::Timer(t) => t.read(d, buf),
        Kind::Signal(s) => s.read(d, buf),
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
                    super::mm::file_written(path, at, data);
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
                p.wq.notify_all();
            }
            done as i64
        }
        Kind::Event(e) => {
            if data.len() < 8 {
                return -err::EINVAL;
            }
            let v = u64::from_le_bytes(data[..8].try_into().unwrap());
            e.count.fetch_add(v, Ordering::Relaxed);
            e.wq.notify_all();
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
        Kind::Virtual { sink: Some(sink), pos, .. } => {
            let r = sink.write(data, *pos.lock());
            if r > 0 {
                *pos.lock() += r as u64;
            }
            r
        }
        Kind::Virtual { .. } => -err::EACCES,
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
                    super::mm::file_read(path, off, &mut tmp[..n]);
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
                Ok(()) => {
                    super::mm::file_written(path, off, data);
                    data.len() as i64
                }
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
        // The size only matters from the end (and costs a lookup).
        Kind::File { path, pos, .. } => (pos, if whence == 2 { fs::stat(path).map(|s| s.size).unwrap_or(0) as i64 } else { 0 }),
        Kind::Virtual { data, pos, .. } => (pos, data.len() as i64),
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
    match fs::truncate(path, len) {
        Ok(()) => {
            super::mm::file_truncated(path, len);
            0
        }
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
    /// The device a device file is (Linux's numbers: /dev/null is 1:3), as
    /// glibc's makedev encodes it.
    pub rdev: u64,
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
        return Ok(StatInfo { mode: 0o140777, size: 0, time: crate::time::unix_now(), ino: 4, rdev: 0 });
    }
    // A generated /proc folder (glibc's opendir checks with fstat that it
    // opened a folder).
    if matches!(st, Err(justfs::Error::NotFound)) && virtual_dir(&path.inner).is_some() {
        return Ok(StatInfo { mode: 0o040500, size: 0, time: crate::time::unix_now(), ino: 6, rdev: 0 });
    }
    let st = st.map_err(|e| -errno(user::fs_error(e)))?;
    // The permissions a program gave; without them, names without an
    // extension are programs (Linux ones are), and the shared libraries'
    // names (libc.so.6) look like programs too.
    let given = (st.mode & justfs::MODE_SET != 0).then_some((st.mode & 0o7777) as u32);
    let mode = if st.kind == justfs::Kind::Dir {
        0o040000 | given.unwrap_or(0o755)
    } else if st.kind == justfs::Kind::Link {
        0o120777
    } else if let Some(bits) = given {
        0o100000 | bits
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
    Ok(StatInfo { mode, size: st.size, time: st.modified + super::UNIX_2000, ino, rdev: 0 })
}

/// The device numbers of the binder devices (a misc device, major 10).
const BINDER_RDEV: u64 = 0xa00 + 60;

/// The stat of a /dev/shm path: the shared memory there, or ENOENT.
fn shm_stat(text: &str) -> Option<Result<StatInfo, i64>> {
    let name = super::shm::named(text)?;
    Some(match super::shm::named_size(name) {
        Some(size) => Ok(StatInfo { mode: 0o100600, size, time: crate::time::unix_now(), ino: 5, rdev: 0 }),
        None => Err(-err::ENOENT),
    })
}

pub fn stat_of_desc(d: &Desc) -> Result<StatInfo, i64> {
    let device = |mode: u32, rdev: u64| Ok(StatInfo { mode, size: 0, time: crate::time::unix_now(), ino: 2 + rdev, rdev });
    let simple = |mode: u32| device(mode, 0);
    match &d.kind {
        Kind::File { path, .. } | Kind::Dir { path, .. } => stat_of_path(path),
        Kind::Virtual { data, sink, .. } => {
            let mode = if sink.is_some() { 0o100644 } else { 0o100444 };
            Ok(StatInfo { mode, size: data.len() as u64, time: crate::time::unix_now(), ino: 3, rdev: 0 })
        }
        Kind::Memfd { shm, .. } => Ok(StatInfo { mode: 0o100777, size: shm.size(), time: crate::time::unix_now(), ino: Arc::as_ptr(shm) as u64 >> 4, rdev: 0 }),
        // A terminal (pts/0), and the memory devices with their numbers.
        Kind::Console => device(0o020620, 0x8800),
        Kind::Null => device(0o020666, 0x103),
        Kind::Zero => device(0o020666, 0x105),
        Kind::Random => device(0o020666, 0x109),
        Kind::PipeRead(_) | Kind::PipeWrite(_) => simple(0o010600),
        Kind::Socket(_) | Kind::Unix(_) => simple(0o140777),
        Kind::Event(_) | Kind::Epoll(_) | Kind::Inotify(_) | Kind::Timer(_) | Kind::Signal(_) => simple(0o100600),
        Kind::Binder(_) => device(0o020666, BINDER_RDEV),
        Kind::Ashmem(_) => device(0o020666, super::ashmem::RDEV),
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
    put(buf, 40, s.rdev);
    put(buf, 48, s.size);
    put(buf, 56, 4096); // st_blksize
    put(buf, 64, s.size.div_ceil(512));
    for off in [72, 88, 104] {
        put(buf, off, s.time);
    }
    0
}

fn path_arg(dirfd: i32, ptr: u64) -> Result<(Path, String), i64> {
    let text = super::path_text_at(dirfd, ptr)?;
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
    let text = super::path_text_at(dirfd, path_ptr).unwrap_or_default();
    if text.is_empty() && flags & AT_EMPTY_PATH != 0 {
        return sys_fstat(dirfd, buf);
    }
    if let Some(info) = virtual_stat(&text, flags & AT_SYMLINK_NOFOLLOW == 0) {
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
    let text = super::path_text_at(dirfd, path_ptr).unwrap_or_default();
    let info = if text.is_empty() && flags & AT_EMPTY_PATH != 0 {
        match get(dirfd) {
            Some(d) => stat_of_desc(&d),
            None => return -err::EBADF,
        }
    } else if let Some(info) = virtual_stat(&text, flags & AT_SYMLINK_NOFOLLOW == 0) {
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
    // stx_rdev and stx_dev, as major and minor numbers (st_dev is 8:3).
    let (major, minor) = ((s.rdev >> 8 & 0xfff) as u32, (s.rdev & 0xff | s.rdev >> 12 & !0xff) as u32);
    out[128..132].copy_from_slice(&major.to_le_bytes());
    out[132..136].copy_from_slice(&minor.to_le_bytes());
    out[136..140].copy_from_slice(&8u32.to_le_bytes());
    out[140..144].copy_from_slice(&3u32.to_le_bytes());
    0
}

pub fn sys_faccessat(dirfd: i32, path_ptr: u64) -> i64 {
    let text = super::path_text_at(dirfd, path_ptr).unwrap_or_default();
    if let Some(info) = virtual_stat(&text, true) {
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
        if let Some(v) = virtual_dir(&show_path(path)) {
            *names.lock() = Some(v);
        }
    }
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

pub fn sys_mkdirat(dirfd: i32, path_ptr: u64, mode: u32) -> i64 {
    let text = super::path_text_at(dirfd, path_ptr).unwrap_or_default();
    if virtual_stat(&text, false).is_some() {
        return -err::EEXIST;
    }
    match path_arg(dirfd, path_ptr) {
        // As on Linux: not over something that is there (a link too), and
        // only in a folder that is.
        Ok((p, _)) if fs::lstat(&p).is_ok() => -err::EEXIST,
        Ok((p, _)) if !fs::is_dir(&p.parent()) => -err::ENOENT,
        Ok((p, _)) => match fs::make_dir(&p) {
            Ok(()) => {
                let _ = fs::set_mode(&p, super::masked(mode));
                0
            }
            Err(e) => -errno(user::fs_error(e)),
        },
        Err(e) => e,
    }
}

/// chmod / fchmodat (`flags` AT_SYMLINK_NOFOLLOW is refused, as by Linux
/// for links) and fchmod: new permission bits.
pub fn sys_fchmodat(dirfd: i32, path_ptr: u64, mode: u32) -> i64 {
    let text = super::path_text_at(dirfd, path_ptr).unwrap_or_default();
    if virtual_stat(&text, true).is_some() {
        return -err::EPERM;
    }
    match path_arg(dirfd, path_ptr) {
        Ok((p, _)) => match fs::set_mode(&p, (mode & 0o7777) as u16) {
            Ok(()) => 0,
            Err(e) => -errno(user::fs_error(e)),
        },
        Err(e) => e,
    }
}

pub fn sys_fchmod(fd: i32, mode: u32) -> i64 {
    match get(fd).map(|d| match &d.kind {
        Kind::File { path, .. } | Kind::Dir { path, .. } => Some(path.clone()),
        _ => None,
    }) {
        Some(Some(path)) => match fs::set_mode(&path, (mode & 0o7777) as u16) {
            Ok(()) => 0,
            Err(e) => -errno(user::fs_error(e)),
        },
        // Pipes, sockets, memfds: nothing kept.
        Some(None) => 0,
        None => -err::EBADF,
    }
}

pub fn sys_unlinkat(dirfd: i32, path_ptr: u64) -> i64 {
    let text = super::path_text_at(dirfd, path_ptr).unwrap_or_default();
    if let Some(name) = super::shm::named(&text) {
        return super::shm::unlink_named(name);
    }
    match path_arg(dirfd, path_ptr) {
        // A local socket's name.
        Ok((p, _)) if super::unix::unlink(&p) => 0,
        Ok((p, _)) => {
            // Mappings of it keep what they map, and descriptors what they
            // read and write.
            super::mm::forget_file(&p);
            super::mm::file_gone(&p);
            super::keep_open(&p);
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
    super::mm::file_gone(&from);
    super::mm::file_gone(&to);
    if fs::exists(&to) && !fs::is_dir(&to) {
        let _ = fs::remove(&to);
    }
    match fs::rename(&from, &to) {
        Ok(()) => 0,
        Err(e) => -errno(user::fs_error(e)),
    }
}

pub fn sys_readlinkat(dirfd: i32, path_ptr: u64, buf: u64, cap: u64) -> i64 {
    let text = super::path_text_at(dirfd, path_ptr).unwrap_or_default();
    let me = task::current_pid();
    if let Some(link) = ns_link(&text) {
        return copy_link(&link, buf, cap);
    }
    // /proc/self and /proc/thread-self are links to the caller's folders.
    match text.trim_end_matches('/') {
        "/proc/self" => return copy_link(&alloc::format!("{}", super::ns::local(me)), buf, cap),
        "/proc/thread-self" => {
            let tid = super::ns::local(super::thread::linux_tid(task::current_tid()));
            return copy_link(&alloc::format!("{}/task/{tid}", super::ns::local(me)), buf, cap);
        }
        _ => {}
    }
    let target = match proc_of(&text) {
        Some((pid, "exe")) => match super::with_state_of(pid, |st| st.exe.clone()) {
            Some(exe) => exe,
            None => return -err::ENOENT,
        },
        Some((pid, fd)) if pid == me && fd.starts_with("fd/") => {
            let n: i32 = fd[3..].parse().unwrap_or(-1);
            match get(n).map(|d| match &d.kind {
                Kind::File { path, .. } | Kind::Dir { path, .. } => show_path(path),
                Kind::Memfd { shm, .. } => shm.name.clone(),
                Kind::Ashmem(_) => String::from("/dev/ashmem"),
                _ => String::from("anon_inode:[justos]"),
            }) {
                Some(s) => s,
                None => return -err::ENOENT,
            }
        }
        // Other virtual files and folders are not links.
        _ if virtual_stat(&text, false).is_some() => return -err::EINVAL,
        _ => match path_arg(dirfd, path_ptr) {
            Ok((p, _)) => match fs::read_link(&p) {
                Ok(t) => t,
                Err(justfs::Error::InvalidName) => return -err::EINVAL, // not a link
                Err(e) => return -errno(user::fs_error(e)),
            },
            Err(e) => return e,
        },
    };
    copy_link(&target, buf, cap)
}

/// readlink's answer: as much of the link as fits, without a NUL.
fn copy_link(target: &str, buf: u64, cap: u64) -> i64 {
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
                Kind::Inotify(i) => i.pending() as u32,
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
            Kind::Binder(b) => super::binder::ioctl(b, req, arg, d.nonblocking()),
            Kind::Ashmem(a) => super::ashmem::ioctl(a, req, arg),
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
        Kind::Inotify(i) => (i.ready(), false, false),
        Kind::Binder(b) => (super::binder::readable(b), false, false),
        Kind::Timer(t) => (t.ready(), false, false),
        Kind::Signal(s) => (s.ready(), false, false),
        _ => (true, true, false),
    }
}

/// Descriptors whose readiness never changes (files, generated files,
/// devices that never block): an edge-triggered watch sees one edge.
pub fn always_ready(d: &Desc) -> bool {
    matches!(d.kind, Kind::File { .. } | Kind::Dir { .. } | Kind::Virtual { .. } | Kind::Null | Kind::Zero | Kind::Random | Kind::Memfd { .. } | Kind::Ashmem(_))
}

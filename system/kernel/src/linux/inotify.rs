//! inotify: a program watches files and folders and reads what happens to
//! them (GLib's file monitors, Firefox, file managers). A watch names a
//! node of a volume, so it follows the file when it moves, as on Linux. The
//! file system describes each change ([`fs::Change`]) while anything is
//! watched; changes made by justOS programs show up too.
//!
//! Reported: created, changed (written or cut), closed after writing,
//! moved (from and to, tied by a cookie), deleted, and for the watched node
//! itself changed, deleted and moved. Opening and reading are not.

use super::fd::{self, Desc, Kind, O_CLOEXEC, O_NONBLOCK};
use super::{err, errno, linux_path};
use crate::fs::{self, change};
use crate::sync::IrqLock;
use crate::task::WaitQueue;
use crate::user;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use justfs::NodeId;

// The rest of the events (created, moved from) come from the file system
// as they are (see `fs::change`).
const IN_MODIFY: u32 = change::MODIFY;
const IN_ATTRIB: u32 = change::ATTRIB;
const IN_CLOSE_WRITE: u32 = change::CLOSE_WRITE;
const IN_MOVED_TO: u32 = change::MOVED_TO;
const IN_DELETE: u32 = change::DELETE;
const IN_DELETE_SELF: u32 = 0x400;
const IN_MOVE_SELF: u32 = 0x800;
const IN_Q_OVERFLOW: u32 = 0x4000;
const IN_IGNORED: u32 = 0x8000;
const IN_ISDIR: u32 = change::ISDIR;
const IN_ALL_EVENTS: u32 = 0xfff;
const IN_ONLYDIR: u32 = 0x0100_0000;
const IN_DONT_FOLLOW: u32 = 0x0200_0000;
const IN_MASK_CREATE: u32 = 0x1000_0000;
const IN_MASK_ADD: u32 = 0x2000_0000;
const IN_ONESHOT: u32 = 0x8000_0000;

/// Events kept unread, at most; then one IN_Q_OVERFLOW says some were lost.
const QUEUE_MAX: usize = 16384;

/// The size of `struct inotify_event` without its name.
const EVENT_HEAD: usize = 16;

/// One inotify instance (the object behind its descriptors).
pub struct Inotify {
    state: IrqLock<State>,
    /// Told when an event is queued.
    pub wq: Arc<WaitQueue>,
}

struct State {
    watches: BTreeMap<i32, Watch>,
    next: i32,
    queue: VecDeque<Event>,
}

struct Watch {
    volume: usize,
    node: NodeId,
    mask: u32,
}

struct Event {
    wd: i32,
    mask: u32,
    cookie: u32,
    name: String,
}

impl Event {
    /// Its name with the terminating zero, padded to a multiple of the head.
    fn name_len(&self) -> usize {
        if self.name.is_empty() {
            0
        } else {
            (self.name.len() + 1).next_multiple_of(EVENT_HEAD)
        }
    }
}

/// Every instance, to tell of changes.
static ALL: IrqLock<Vec<Weak<Inotify>>> = IrqLock::new(Vec::new());

impl Inotify {
    pub fn ready(&self) -> bool {
        !self.state.lock().queue.is_empty()
    }

    /// Bytes of events waiting (FIONREAD).
    pub fn pending(&self) -> usize {
        self.state.lock().queue.iter().map(|e| EVENT_HEAD + e.name_len()).sum()
    }

    /// Reads whole events into `buf`, waiting for one unless `d` does not
    /// block.
    pub fn read(&self, d: &Desc, buf: &mut [u8]) -> i64 {
        if let Err(e) = fd::wait_until(d, || self.ready()) {
            return e;
        }
        let mut st = self.state.lock();
        let mut at = 0;
        while let Some(e) = st.queue.front() {
            let len = EVENT_HEAD + e.name_len();
            if at + len > buf.len() {
                break;
            }
            let out = &mut buf[at..at + len];
            out[0..4].copy_from_slice(&e.wd.to_le_bytes());
            out[4..8].copy_from_slice(&e.mask.to_le_bytes());
            out[8..12].copy_from_slice(&e.cookie.to_le_bytes());
            out[12..16].copy_from_slice(&((len - EVENT_HEAD) as u32).to_le_bytes());
            out[EVENT_HEAD..].fill(0);
            out[EVENT_HEAD..EVENT_HEAD + e.name.len()].copy_from_slice(e.name.as_bytes());
            at += len;
            st.queue.pop_front();
        }
        // Not even the first event fits.
        if at == 0 {
            return -err::EINVAL;
        }
        at as i64
    }
}

impl State {
    /// Queues an event, unless it repeats the last unread one.
    fn push(&mut self, wd: i32, mask: u32, cookie: u32, name: &str) {
        if let Some(last) = self.queue.back() {
            if last.wd == wd && last.mask == mask && last.cookie == cookie && last.name == name {
                return;
            }
            if last.mask == IN_Q_OVERFLOW {
                return;
            }
        }
        if self.queue.len() >= QUEUE_MAX {
            self.queue.push_back(Event { wd: -1, mask: IN_Q_OVERFLOW, cookie: 0, name: String::new() });
            return;
        }
        self.queue.push_back(Event { wd, mask, cookie, name: String::from(name) });
    }

    /// Drops watch `wd`, telling the reader (IN_IGNORED).
    fn forget(&mut self, wd: i32) -> bool {
        if self.watches.remove(&wd).is_none() {
            return false;
        }
        fs::watching(false);
        self.push(wd, IN_IGNORED, 0, "");
        true
    }

    /// Queues what watchers of `c` see; returns whether anything was.
    fn tell(&mut self, c: &fs::Change<'_>) -> bool {
        let what = c.mask & !IN_ISDIR;
        let mut told = false;
        let mut done = Vec::new();
        let mut queued = Vec::new();
        for (&wd, w) in &self.watches {
            if w.volume != c.volume {
                continue;
            }
            if w.node == c.dir && w.mask & what != 0 {
                queued.push((wd, c.mask, c.cookie, true));
            }
            if w.node == c.node {
                let own = match what {
                    IN_MODIFY | IN_ATTRIB | IN_CLOSE_WRITE => what,
                    IN_DELETE => IN_DELETE_SELF,
                    IN_MOVED_TO => IN_MOVE_SELF,
                    _ => 0,
                };
                if w.mask & own != 0 {
                    queued.push((wd, own, 0, false));
                }
                // Gone: the watch goes too.
                if own == IN_DELETE_SELF {
                    done.push(wd);
                }
            }
        }
        for (wd, mask, cookie, named) in queued {
            self.push(wd, mask, cookie, if named { c.name } else { "" });
            told = true;
            if self.watches.get(&wd).is_some_and(|w| w.mask & IN_ONESHOT != 0) && !done.contains(&wd) {
                done.push(wd);
            }
        }
        for wd in done {
            told |= self.forget(wd);
        }
        told
    }
}

impl Drop for Inotify {
    fn drop(&mut self) {
        for _ in 0..self.state.lock().watches.len() {
            fs::watching(false);
        }
    }
}

/// Told by the file system of every change while anything is watched.
fn changed(c: &fs::Change<'_>) {
    let mut all = ALL.lock();
    all.retain(|w| w.strong_count() > 0);
    for i in all.iter().filter_map(Weak::upgrade) {
        if i.state.lock().tell(c) {
            i.wq.notify_all();
        }
    }
}

pub fn sys_inotify_init1(flags: u32) -> i64 {
    if flags & !(O_NONBLOCK | O_CLOEXEC) != 0 {
        return -err::EINVAL;
    }
    fs::set_watcher(changed);
    let i = Arc::new(Inotify { state: IrqLock::new(State { watches: BTreeMap::new(), next: 1, queue: VecDeque::new() }), wq: Arc::default() });
    ALL.lock().push(Arc::downgrade(&i));
    fd::install(Desc::new(Kind::Inotify(i), O_NONBLOCK & flags), flags & O_CLOEXEC != 0)
}

/// The instance behind descriptor `fd`.
fn instance(fd: i32) -> Result<Arc<Desc>, i64> {
    let d = fd::get(fd).ok_or(-err::EBADF)?;
    if matches!(d.kind, Kind::Inotify(_)) {
        Ok(d)
    } else {
        Err(-err::EINVAL)
    }
}

pub fn sys_inotify_add_watch(fd: i32, path_ptr: u64, mask: u32) -> i64 {
    let d = match instance(fd) {
        Ok(d) => d,
        Err(e) => return e,
    };
    let Kind::Inotify(i) = &d.kind else { return -err::EINVAL };
    if mask & IN_ALL_EVENTS == 0 || mask & IN_MASK_ADD != 0 && mask & IN_MASK_CREATE != 0 {
        return -err::EINVAL;
    }
    let text = match user::user_cstr(path_ptr, 4096) {
        Ok(t) => t,
        Err(e) => return -errno(e),
    };
    let path = match linux_path(&text) {
        Ok(p) => p,
        Err(e) => return -e,
    };
    let (volume, node, kind) = match fs::node_of(&path, mask & IN_DONT_FOLLOW == 0) {
        Ok(n) => n,
        Err(e) => return -errno(user::fs_error(e)),
    };
    if mask & IN_ONLYDIR != 0 && kind != justfs::Kind::Dir {
        return -err::ENOTDIR;
    }
    let mut st = i.state.lock();
    let wanted = mask & (IN_ALL_EVENTS | IN_ONESHOT);
    if let Some((&wd, w)) = st.watches.iter_mut().find(|(_, w)| w.volume == volume && w.node == node) {
        if mask & IN_MASK_CREATE != 0 {
            return -err::EEXIST;
        }
        w.mask = if mask & IN_MASK_ADD != 0 { w.mask | wanted } else { wanted };
        return wd as i64;
    }
    let wd = st.next;
    st.next += 1;
    st.watches.insert(wd, Watch { volume, node, mask: wanted });
    fs::watching(true);
    wd as i64
}

pub fn sys_inotify_rm_watch(fd: i32, wd: i32) -> i64 {
    let d = match instance(fd) {
        Ok(d) => d,
        Err(e) => return e,
    };
    let Kind::Inotify(i) = &d.kind else { return -err::EINVAL };
    if !i.state.lock().forget(wd) {
        return -err::EINVAL;
    }
    i.wq.notify_all();
    0
}

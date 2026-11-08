//! Namespaces: what a process sees of the system, split as Linux splits it.
//! clone and unshare give a process new ones, /proc/<pid>/ns/ names them,
//! and the sandboxes of Firefox, Chromium and bubblewrap are built on them.
//!
//! - User: user and group ids of its own, mapped onto the ids outside
//!   through /proc/<pid>/uid_map and gid_map, and a full set of
//!   capabilities that count inside. (Programs here run as root, uid 0.)
//! - PID: process and thread ids of its own, from 1 ([`local`], [`global`]).
//!   The first process is the namespace's init: when it ends, the others
//!   there are killed and no more can start there.
//! - Network: no way out. Internet sockets reach nothing (as in a new Linux
//!   namespace, whose loopback device is down), and abstract local socket
//!   names are its own.
//! - UTS: the host and domain names (sethostname, uname).
//! - Mount, IPC, cgroup, time: each a namespace of its own, with the same
//!   view as the one it came from. Nothing here changes those views (there
//!   is no mount, SysV IPC, cgroup control or clock offset), so they stay
//!   the same.
//!
//! Capabilities: every process has Linux's effective, permitted and
//! inheritable sets (capget, capset); execve gives root its full sets back.
//! They are checked where namespaces need them: new namespaces other than a
//! user one ask for CAP_SYS_ADMIN, chroot for CAP_SYS_CHROOT, and id maps
//! that name ids other than one's own for CAP_SETUID or CAP_SETGID.

use super::err;
use crate::sync::IrqLock;
use crate::task;
use crate::user;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

pub const CLONE_NEWTIME: u64 = 0x80;
pub const CLONE_VM: u64 = 0x100;
pub const CLONE_FS: u64 = 0x200;
pub const CLONE_FILES: u64 = 0x400;
pub const CLONE_SIGHAND: u64 = 0x800;
pub const CLONE_THREAD: u64 = 0x10000;
pub const CLONE_NEWNS: u64 = 0x2_0000;
pub const CLONE_SYSVSEM: u64 = 0x4_0000;
pub const CLONE_NEWCGROUP: u64 = 0x200_0000;
pub const CLONE_NEWUTS: u64 = 0x400_0000;
pub const CLONE_NEWIPC: u64 = 0x800_0000;
pub const CLONE_NEWUSER: u64 = 0x1000_0000;
pub const CLONE_NEWPID: u64 = 0x2000_0000;
pub const CLONE_NEWNET: u64 = 0x4000_0000;
/// The namespaces clone makes. (CLONE_NEWTIME's bit is part of the exit
/// signal there: only unshare takes it.)
pub const CLONE_NAMESPACES: u64 =
    CLONE_NEWNS | CLONE_NEWCGROUP | CLONE_NEWUTS | CLONE_NEWIPC | CLONE_NEWUSER | CLONE_NEWPID | CLONE_NEWNET;

const CAP_SETGID: u32 = 6;
const CAP_SETUID: u32 = 7;
const CAP_SETPCAP: u32 = 8;
pub const CAP_SYS_CHROOT: u32 = 18;
pub const CAP_SYS_ADMIN: u32 = 21;
/// Every capability Linux 6.1 has (up to CAP_CHECKPOINT_RESTORE, 40).
const ALL_CAPS: u64 = (1 << 41) - 1;

/// What an id without a mapping shows as (overflowuid, overflowgid).
const OVERFLOW_ID: u32 = 65534;
/// How deep user and PID namespaces nest.
const MAX_DEPTH: u32 = 32;
/// The most lines an id map takes.
const MAX_EXTENTS: usize = 340;
/// Ids of a PID namespace go up to this, then start again from 2.
const PID_MAX: u32 = 4_194_304;

const EUSERS: i64 = 87;

// Inode numbers of the first namespaces, as Linux gives them; new ones
// count on from NEXT_ID.
const INIT_IPC: u32 = 0xEFFF_FFFF;
const INIT_UTS: u32 = 0xEFFF_FFFE;
const INIT_USER: u32 = 0xEFFF_FFFD;
const INIT_PID: u32 = 0xEFFF_FFFC;
const INIT_CGROUP: u32 = 0xEFFF_FFFB;
const INIT_TIME: u32 = 0xEFFF_FFFA;
const INIT_NET: u32 = 0xF000_0000;
const INIT_MNT: u32 = 0xF000_0001;
static NEXT_ID: AtomicU32 = AtomicU32::new(0xF000_0100);

fn new_id() -> u32 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

// ----------------------------------------------------------------------------
// User namespaces

#[derive(Clone, Copy)]
struct Extent {
    inside: u32,
    outside: u32,
    count: u32,
}

#[derive(Default)]
struct Maps {
    uid: Option<Vec<Extent>>,
    gid: Option<Vec<Extent>>,
    setgroups_denied: bool,
}

pub struct UserNs {
    id: u32,
    parent: Option<Arc<UserNs>>,
    depth: u32,
    maps: IrqLock<Maps>,
}

impl UserNs {
    fn child(parent: &Arc<UserNs>) -> UserNs {
        let denied = parent.maps.lock().setgroups_denied;
        UserNs {
            id: new_id(),
            parent: Some(parent.clone()),
            depth: parent.depth + 1,
            maps: IrqLock::new(Maps { setgroups_denied: denied, ..Maps::default() }),
        }
    }

    /// `outer`, an id of the parent namespace, as seen inside.
    fn map_in(&self, gid: bool, outer: u32) -> Option<u32> {
        let m = self.maps.lock();
        let ext = if gid { m.gid.as_ref() } else { m.uid.as_ref() }?;
        ext.iter().find(|e| outer >= e.outside && outer - e.outside < e.count).map(|e| e.inside + (outer - e.outside))
    }

    /// An id of the first namespace as seen here.
    fn seen_from_global(&self, gid: bool, id: u32) -> Option<u32> {
        match &self.parent {
            None => Some(id),
            Some(p) => self.map_in(gid, p.seen_from_global(gid, id)?),
        }
    }

    /// Do ids `[first, first + count)` of this namespace have a mapping?
    fn has_ids(&self, gid: bool, first: u32, count: u32) -> bool {
        if self.parent.is_none() {
            return true;
        }
        let m = self.maps.lock();
        let Some(ext) = (if gid { m.gid.as_ref() } else { m.uid.as_ref() }) else { return false };
        ext.iter().any(|e| first >= e.inside && (first - e.inside) as u64 + count as u64 <= e.count as u64)
    }

    fn is_above(&self, other: &UserNs) -> bool {
        let mut n = other.parent.as_deref();
        while let Some(u) = n {
            if core::ptr::eq(u, self) {
                return true;
            }
            n = u.parent.as_deref();
        }
        false
    }

    fn map_text(&self, gid: bool) -> String {
        if self.parent.is_none() {
            return alloc::format!("{:>10} {:>10} {:>10}\n", 0, 0, u32::MAX);
        }
        let m = self.maps.lock();
        let mut s = String::new();
        for e in (if gid { m.gid.as_ref() } else { m.uid.as_ref() }).into_iter().flatten() {
            s.push_str(&alloc::format!("{:>10} {:>10} {:>10}\n", e.inside, e.outside, e.count));
        }
        s
    }
}

/// Reads an id map ("inside outside count" lines) as a program writes it.
fn parse_map(data: &[u8]) -> Result<Vec<Extent>, i64> {
    let text = core::str::from_utf8(data).map_err(|_| -err::EINVAL)?;
    let mut out: Vec<Extent> = Vec::new();
    for line in text.split('\n').filter(|l| !l.trim().is_empty()) {
        let mut f = line.split_ascii_whitespace().map(|w| w.parse::<u32>());
        let (Some(Ok(inside)), Some(Ok(outside)), Some(Ok(count)), None) = (f.next(), f.next(), f.next(), f.next()) else {
            return Err(-err::EINVAL);
        };
        let ends = |a: u32| (a as u64 + count as u64) <= u32::MAX as u64;
        if count == 0 || !ends(inside) || !ends(outside) || out.len() == MAX_EXTENTS {
            return Err(-err::EINVAL);
        }
        let overlap = |a: u32, b: u32, n: u32| a < b.saturating_add(n) && b < a.saturating_add(count);
        if out.iter().any(|e| overlap(inside, e.inside, e.count) || overlap(outside, e.outside, e.count)) {
            return Err(-err::EINVAL);
        }
        out.push(Extent { inside, outside, count });
    }
    if out.is_empty() {
        return Err(-err::EINVAL);
    }
    Ok(out)
}

/// The files of /proc/<pid>/ a program writes: the id maps and setgroups
/// of the process's user namespace.
pub enum Sink {
    UidMap(Arc<UserNs>),
    GidMap(Arc<UserNs>),
    SetGroups(Arc<UserNs>),
}

impl Sink {
    /// A write at `pos`: the whole map (or setgroups word) at once, as
    /// Linux takes it.
    pub fn write(&self, data: &[u8], pos: u64) -> i64 {
        if pos != 0 || data.len() >= 4096 {
            return -err::EINVAL;
        }
        let me = of(task::current_pid());
        match self {
            Sink::SetGroups(ns) => {
                let word = core::str::from_utf8(data).map(|s| s.trim_end_matches('\n'));
                let deny = match word {
                    Ok("deny") => true,
                    Ok("allow") => false,
                    _ => return -err::EINVAL,
                };
                // The first namespace allows it for good; a denial is for
                // good too, and comes before the group map.
                let mut m = ns.maps.lock();
                let fixed = ns.parent.is_none() || m.gid.is_some();
                if (deny && fixed) || (!deny && m.setgroups_denied) {
                    return -err::EPERM;
                }
                m.setgroups_denied = deny;
                data.len() as i64
            }
            Sink::UidMap(ns) | Sink::GidMap(ns) => {
                let gid = matches!(self, Sink::GidMap(_));
                let Some(parent) = ns.parent.as_ref() else { return -err::EPERM };
                // The writer is in the namespace or above it, with
                // CAP_SYS_ADMIN.
                if (!Arc::ptr_eq(&me.user, ns) && !me.user.is_above(ns)) || !me.caps.has(CAP_SYS_ADMIN) {
                    return -err::EPERM;
                }
                let map = match parse_map(data) {
                    Ok(m) => m,
                    Err(e) => return e,
                };
                if map.iter().any(|e| !parent.has_ids(gid, e.outside, e.count)) {
                    return -err::EPERM;
                }
                // One's own id needs no privilege (a group id only once
                // setgroups is denied); any other, CAP_SETUID or
                // CAP_SETGID above the namespace.
                let own = parent.seen_from_global(gid, 0);
                let denied = ns.maps.lock().setgroups_denied;
                let just_own = map.len() == 1 && map[0].count == 1 && Some(map[0].outside) == own && (!gid || denied);
                let cap = if gid { CAP_SETGID } else { CAP_SETUID };
                if !just_own && !(me.user.is_above(ns) && me.caps.has(cap)) {
                    return -err::EPERM;
                }
                let mut m = ns.maps.lock();
                let slot = if gid { &mut m.gid } else { &mut m.uid };
                if slot.is_some() {
                    return -err::EPERM;
                }
                *slot = Some(map);
                data.len() as i64
            }
        }
    }
}

// ----------------------------------------------------------------------------
// PID namespaces

#[derive(Default)]
struct PidIds {
    last: u32,
    to_local: BTreeMap<u32, u32>,
    to_global: BTreeMap<u32, u32>,
    /// Its init ended: no more processes here.
    ended: bool,
}

pub struct PidNs {
    id: u32,
    parent: Option<Arc<PidNs>>,
    depth: u32,
    ids: IrqLock<PidIds>,
}

impl PidNs {
    fn child(parent: &Arc<PidNs>) -> PidNs {
        PidNs { id: new_id(), parent: Some(parent.clone()), depth: parent.depth + 1, ids: IrqLock::new(PidIds::default()) }
    }

    fn is_first(&self) -> bool {
        self.parent.is_none()
    }

    fn local_of(&self, g: u32) -> u32 {
        if self.is_first() {
            return g;
        }
        self.ids.lock().to_local.get(&g).copied().unwrap_or(0)
    }

    fn global_of(&self, l: u32) -> Option<u32> {
        if self.is_first() {
            return Some(l);
        }
        self.ids.lock().to_global.get(&l).copied()
    }

    /// This namespace and the ones above it, but not the first.
    fn chain(self: &Arc<PidNs>) -> Vec<Arc<PidNs>> {
        let mut v = Vec::new();
        let mut n = Some(self.clone());
        while let Some(ns) = n {
            if ns.is_first() {
                break;
            }
            n = ns.parent.clone();
            v.push(ns);
        }
        v
    }

    fn ended(&self) -> bool {
        !self.is_first() && self.ids.lock().ended
    }

    /// Gives global id `g` an id here and in every namespace above.
    fn enter(self: &Arc<PidNs>, g: u32) -> Result<(), i64> {
        let chain = self.chain();
        if chain.iter().any(|n| n.ended()) {
            return Err(-err::ENOMEM);
        }
        for n in chain {
            let mut ids = n.ids.lock();
            let mut l = ids.last;
            loop {
                l = if l + 1 >= PID_MAX { 2 } else { l + 1 };
                if !ids.to_global.contains_key(&l) {
                    break;
                }
            }
            ids.last = l;
            ids.to_local.insert(g, l);
            ids.to_global.insert(l, g);
        }
        Ok(())
    }

    fn leave(self: &Arc<PidNs>, g: u32) {
        for n in self.chain() {
            let mut ids = n.ids.lock();
            if let Some(l) = ids.to_local.remove(&g) {
                ids.to_global.remove(&l);
            }
        }
    }
}

// ----------------------------------------------------------------------------
// UTS namespaces

pub struct UtsNs {
    id: u32,
    /// The host and domain names.
    names: IrqLock<(String, String)>,
}

// ----------------------------------------------------------------------------
// Processes

#[derive(Clone, Copy)]
pub struct Caps {
    eff: u64,
    perm: u64,
    inh: u64,
}

impl Caps {
    const ROOT: Caps = Caps { eff: ALL_CAPS, perm: ALL_CAPS, inh: 0 };

    fn has(&self, cap: u32) -> bool {
        self.eff & (1 << cap) != 0
    }
}

#[derive(Clone)]
struct Proc {
    user: Arc<UserNs>,
    pid: Arc<PidNs>,
    /// Where its children go (unshare(CLONE_NEWPID) changes only this).
    pid_children: Arc<PidNs>,
    uts: Arc<UtsNs>,
    net: u32,
    ipc: u32,
    mnt: u32,
    cgroup: u32,
    time: u32,
    time_children: u32,
    caps: Caps,
    /// Linux ids of its other threads, which have ids in its PID
    /// namespaces too.
    threads: Vec<u32>,
}

/// Processes with namespaces or capabilities of their own; the others have
/// the first namespaces and root's capabilities.
static PROCS: IrqLock<BTreeMap<u32, Proc>> = IrqLock::new(BTreeMap::new());
static FIRST: IrqLock<Option<Proc>> = IrqLock::new(None);
/// Processes that ended in a PID namespace other than the first: their ids
/// stay until they are waited for.
static ENDED: IrqLock<BTreeMap<u32, Arc<PidNs>>> = IrqLock::new(BTreeMap::new());
/// Set once a PID namespace other than the first exists: until then, ids
/// need no translation.
static PID_NAMESPACES: AtomicBool = AtomicBool::new(false);

fn first() -> Proc {
    FIRST
        .lock()
        .get_or_insert_with(|| {
            let user = Arc::new(UserNs { id: INIT_USER, parent: None, depth: 0, maps: IrqLock::new(Maps::default()) });
            let pid = Arc::new(PidNs { id: INIT_PID, parent: None, depth: 0, ids: IrqLock::new(PidIds::default()) });
            let uts = Arc::new(UtsNs { id: INIT_UTS, names: IrqLock::new((String::from("justos"), String::from("(none)"))) });
            Proc {
                user,
                pid: pid.clone(),
                pid_children: pid,
                uts,
                net: INIT_NET,
                ipc: INIT_IPC,
                mnt: INIT_MNT,
                cgroup: INIT_CGROUP,
                time: INIT_TIME,
                time_children: INIT_TIME,
                caps: Caps::ROOT,
                threads: Vec::new(),
            }
        })
        .clone()
}

fn of(pid: u32) -> Proc {
    let p = PROCS.lock().get(&pid).cloned();
    p.unwrap_or_else(first)
}

fn pid_ns(pid: u32) -> Option<Arc<PidNs>> {
    PROCS.lock().get(&pid).map(|p| p.pid.clone())
}

/// New namespaces for `p` as `ns` asks: for a new child (`child`), or for
/// the caller itself (unshare).
fn make(p: &mut Proc, ns: u64, child: bool) {
    if ns & CLONE_NEWUSER != 0 {
        p.user = Arc::new(UserNs::child(&p.user));
        p.caps = Caps::ROOT;
    }
    if ns & CLONE_NEWPID != 0 {
        let n = Arc::new(PidNs::child(&p.pid_children));
        PID_NAMESPACES.store(true, Ordering::Relaxed);
        if child {
            p.pid = n.clone();
        }
        p.pid_children = n;
    }
    if ns & CLONE_NEWUTS != 0 {
        let names = p.uts.names.lock().clone();
        p.uts = Arc::new(UtsNs { id: new_id(), names: IrqLock::new(names) });
    }
    if ns & CLONE_NEWNET != 0 {
        p.net = new_id();
    }
    if ns & CLONE_NEWIPC != 0 {
        p.ipc = new_id();
    }
    if ns & CLONE_NEWNS != 0 {
        p.mnt = new_id();
    }
    if ns & CLONE_NEWCGROUP != 0 {
        p.cgroup = new_id();
    }
    if ns & CLONE_NEWTIME != 0 {
        p.time_children = new_id();
    }
}

/// The namespaces in `ns`, for the log ("user, pid, net").
fn names_of(ns: u64) -> String {
    let all = [
        (CLONE_NEWUSER, "user"),
        (CLONE_NEWPID, "pid"),
        (CLONE_NEWNET, "net"),
        (CLONE_NEWUTS, "uts"),
        (CLONE_NEWIPC, "ipc"),
        (CLONE_NEWNS, "mnt"),
        (CLONE_NEWCGROUP, "cgroup"),
        (CLONE_NEWTIME, "time"),
    ];
    let v: Vec<&str> = all.iter().filter(|(bit, _)| ns & bit != 0).map(|(_, n)| *n).collect();
    v.join(", ")
}

/// What making namespaces `ns` asks of the caller `p`.
fn may_make(p: &Proc, ns: u64) -> Result<(), i64> {
    if ns & CLONE_NEWUSER != 0 {
        if p.user.depth >= MAX_DEPTH {
            return Err(-EUSERS);
        }
        // Not from inside a chroot.
        if user::root().is_some() {
            return Err(-err::EPERM);
        }
    } else if ns != 0 && !p.caps.has(CAP_SYS_ADMIN) {
        return Err(-err::EPERM);
    }
    if ns & CLONE_NEWPID != 0 && p.pid_children.depth >= MAX_DEPTH {
        return Err(-err::ENOSPC);
    }
    Ok(())
}

/// clone of a process: may it have the namespaces `flags` ask for?
pub fn check_clone(flags: u64) -> Result<(), i64> {
    let ns = flags & CLONE_NAMESPACES;
    if ns & (CLONE_NEWUSER | CLONE_NEWNS) != 0 && flags & CLONE_FS != 0 {
        return Err(-err::EINVAL);
    }
    let p = of(task::current_pid());
    may_make(&p, ns)?;
    if p.pid_children.ended() && ns & CLONE_NEWPID == 0 {
        return Err(-err::ENOMEM);
    }
    Ok(())
}

/// A child made by clone: its parent's namespaces and capabilities, or new
/// namespaces as `flags` ask, and its ids in its PID namespaces.
pub fn forked(child: u32, flags: u64) {
    let ns = flags & CLONE_NAMESPACES;
    let parent = PROCS.lock().get(&task::current_pid()).cloned();
    if parent.is_none() && ns == 0 {
        return;
    }
    let mut p = parent.unwrap_or_else(first);
    p.threads.clear();
    p.pid = p.pid_children.clone();
    p.time = p.time_children;
    make(&mut p, ns, true);
    let _ = p.pid.enter(child);
    PROCS.lock().insert(child, p);
    if ns != 0 {
        crate::debug!("linux", "{}: new process {child} in new {} namespaces", user::current_name(), names_of(ns));
    }
}

/// unshare(2): new namespaces (and fs information) for the caller.
pub fn sys_unshare(flags: u64) -> i64 {
    let known = CLONE_NAMESPACES | CLONE_NEWTIME | CLONE_FS | CLONE_FILES | CLONE_SYSVSEM | CLONE_THREAD | CLONE_SIGHAND | CLONE_VM;
    if flags & !known != 0 {
        return -err::EINVAL;
    }
    let me = task::current_pid();
    // What a process with other threads cannot leave behind.
    let single = user::threads().len() <= 1;
    if !single && flags & (CLONE_NEWUSER | CLONE_THREAD | CLONE_SIGHAND | CLONE_VM | CLONE_FILES) != 0 {
        return -err::EINVAL;
    }
    let ns = flags & (CLONE_NAMESPACES | CLONE_NEWTIME);
    let mut p = of(me);
    if let Err(e) = may_make(&p, ns) {
        return e;
    }
    if ns & CLONE_NEWPID != 0 && !Arc::ptr_eq(&p.pid, &p.pid_children) {
        return -err::EINVAL;
    }
    if flags & (CLONE_FS | CLONE_NEWUSER | CLONE_NEWNS) != 0 {
        user::unshare_fs();
    }
    if ns == 0 {
        return 0;
    }
    make(&mut p, ns, false);
    PROCS.lock().insert(me, p);
    crate::debug!("linux", "{}: unshare into new {} namespaces", user::current_name(), names_of(ns));
    0
}

/// A new thread of the calling process, Linux id `ltid`.
pub fn thread_started(ltid: u32) {
    if !PID_NAMESPACES.load(Ordering::Relaxed) {
        return;
    }
    let me = task::current_pid();
    let ns = {
        let mut procs = PROCS.lock();
        let Some(p) = procs.get_mut(&me).filter(|p| !p.pid.is_first()) else { return };
        p.threads.push(ltid);
        p.pid.clone()
    };
    let _ = ns.enter(ltid);
}

/// A thread of the calling process ended.
pub fn thread_ended(ltid: u32) {
    if !PID_NAMESPACES.load(Ordering::Relaxed) {
        return;
    }
    let me = task::current_pid();
    let ns = {
        let mut procs = PROCS.lock();
        let Some(p) = procs.get_mut(&me) else { return };
        p.threads.retain(|&t| t != ltid);
        p.pid.clone()
    };
    ns.leave(ltid);
}

/// A process ended. Its thread ids go now; its own id once it is waited
/// for ([`released`]). If it was the init of its PID namespace, the other
/// processes there are killed: returns them.
pub fn exited(pid: u32) -> Vec<u32> {
    let Some(p) = PROCS.lock().remove(&pid) else { return Vec::new() };
    if p.pid.is_first() {
        return Vec::new();
    }
    for &t in &p.threads {
        p.pid.leave(t);
    }
    let mut doomed = Vec::new();
    {
        let mut ids = p.pid.ids.lock();
        if ids.to_local.get(&pid) == Some(&1) {
            ids.ended = true;
            doomed.extend(ids.to_local.keys().copied().filter(|&g| g != pid && g < 100_000));
        }
    }
    ENDED.lock().insert(pid, p.pid);
    doomed
}

/// An ended process was waited for (or will never be): its ids go.
pub fn released(pid: u32) {
    if let Some(ns) = ENDED.lock().remove(&pid) {
        ns.leave(pid);
    }
}

// ----------------------------------------------------------------------------
// What processes see

/// Global id `g` (a process or thread) as the calling process sees it; 0
/// if it is outside the caller's PID namespace.
pub fn local(g: u32) -> u32 {
    local_in(task::current_pid(), g)
}

/// Global id `g` as process `viewer` sees it.
pub fn local_in(viewer: u32, g: u32) -> u32 {
    if !PID_NAMESPACES.load(Ordering::Relaxed) {
        return g;
    }
    pid_ns(viewer).map_or(g, |ns| ns.local_of(g))
}

/// The global id of what the calling process calls `l`.
pub fn global(l: u32) -> Option<u32> {
    if !PID_NAMESPACES.load(Ordering::Relaxed) {
        return Some(l);
    }
    match pid_ns(task::current_pid()) {
        Some(ns) => ns.global_of(l),
        None => Some(l),
    }
}

/// A process's ids from the first PID namespace down to its own (the
/// NSpid line of /proc/<pid>/status).
pub fn ids_of(pid: u32) -> Vec<u32> {
    let mut v = alloc::vec![pid];
    if let Some(ns) = pid_ns(pid) {
        let mut chain = ns.chain();
        chain.reverse();
        v.extend(chain.iter().map(|n| n.local_of(pid)));
    }
    v
}

/// The calling process's user and group id (root outside; inside a user
/// namespace, what root maps to there).
pub fn uid() -> u32 {
    id(false)
}

pub fn gid() -> u32 {
    id(true)
}

fn id(gid: bool) -> u32 {
    let p = PROCS.lock().get(&task::current_pid()).map(|p| p.user.clone());
    p.map_or(0, |u| u.seen_from_global(gid, 0).unwrap_or(OVERFLOW_ID))
}

/// Is the calling process allowed what `cap` covers?
pub fn capable(cap: u32) -> bool {
    of(task::current_pid()).caps.has(cap)
}

/// The network namespace of the calling process: the first one is 0.
pub fn net() -> u32 {
    let n = PROCS.lock().get(&task::current_pid()).map(|p| p.net);
    n.filter(|&n| n != INIT_NET).unwrap_or(0)
}

/// The host and domain names the calling process sees.
pub fn names() -> (String, String) {
    of(task::current_pid()).uts.names.lock().clone()
}

/// sethostname and setdomainname.
pub fn set_name(domain: bool, ptr: u64, len: u64) -> i64 {
    if len > 64 {
        return -err::EINVAL;
    }
    if !capable(CAP_SYS_ADMIN) {
        return -err::EPERM;
    }
    let name = match super::user_bytes(ptr, len) {
        Ok(b) => String::from_utf8_lossy(b).into_owned(),
        Err(e) => return e,
    };
    let p = of(task::current_pid());
    let mut names = p.uts.names.lock();
    if domain {
        names.1 = name;
    } else {
        names.0 = name;
    }
    0
}

/// After execve: root (uid 0 in its user namespace) has every capability;
/// any other user, none it may use.
pub fn exec() {
    let me = task::current_pid();
    let mut procs = PROCS.lock();
    if let Some(p) = procs.get_mut(&me) {
        let root = p.user.seen_from_global(false, 0) == Some(0);
        p.caps = if root { Caps::ROOT } else { Caps { eff: 0, perm: 0, inh: p.caps.inh } };
    }
}

// ----------------------------------------------------------------------------
// /proc/<pid>/ns and the id files

/// The names in /proc/<pid>/ns.
pub const NS_NAMES: [&str; 10] =
    ["cgroup", "ipc", "mnt", "net", "pid", "pid_for_children", "time", "time_for_children", "user", "uts"];

/// What /proc/<pid>/ns/<name> points to ("user:[4026531837]").
pub fn link(pid: u32, name: &str) -> Option<String> {
    let p = of(pid);
    let (kind, id) = match name {
        "cgroup" => ("cgroup", p.cgroup),
        "ipc" => ("ipc", p.ipc),
        "mnt" => ("mnt", p.mnt),
        "net" => ("net", p.net),
        "pid" => ("pid", p.pid.id),
        "pid_for_children" => ("pid", p.pid_children.id),
        "time" => ("time", p.time),
        "time_for_children" => ("time", p.time_children),
        "user" => ("user", p.user.id),
        "uts" => ("uts", p.uts.id),
        _ => return None,
    };
    Some(alloc::format!("{kind}:[{id}]"))
}

/// The contents of /proc/<pid>/uid_map, gid_map and setgroups, and where
/// writes to them go.
pub fn id_file(pid: u32, name: &str) -> Option<(String, Sink)> {
    let u = of(pid).user;
    Some(match name {
        "uid_map" => (u.map_text(false), Sink::UidMap(u)),
        "gid_map" => (u.map_text(true), Sink::GidMap(u)),
        "setgroups" => {
            let denied = u.maps.lock().setgroups_denied;
            (String::from(if denied { "deny\n" } else { "allow\n" }), Sink::SetGroups(u))
        }
        _ => return None,
    })
}

/// capget and capset take a header (version, pid) and one data block (a
/// 32-bit version 1) or two (versions 2 and 3, for 64 capabilities).
fn cap_header(hdr: u64) -> Result<(usize, i32), i64> {
    const V1: u32 = 0x1998_0330;
    const V2: u32 = 0x2007_1026;
    const V3: u32 = 0x2008_0522;
    let h = super::user_bytes(hdr, 8)?;
    let version = u32::from_le_bytes(h[0..4].try_into().unwrap());
    let pid = i32::from_le_bytes(h[4..8].try_into().unwrap());
    match version {
        V1 => Ok((1, pid)),
        V2 | V3 => Ok((2, pid)),
        _ => {
            h[0..4].copy_from_slice(&V3.to_le_bytes());
            Err(-err::EINVAL)
        }
    }
}

pub fn sys_capget(hdr: u64, data: u64) -> i64 {
    let (blocks, pid) = match cap_header(hdr) {
        Ok(v) => v,
        // Without data, a wrong version is how a program asks for the
        // right one.
        Err(e) => return if data == 0 { 0 } else { e },
    };
    if data == 0 {
        return 0;
    }
    if pid < 0 {
        return -err::EINVAL;
    }
    let target = if pid == 0 {
        task::current_pid()
    } else {
        match global(pid as u32).filter(|&g| user::is_running(g)) {
            Some(g) => g,
            None => return -err::ESRCH,
        }
    };
    let c = of(target).caps;
    let out = match super::user_bytes(data, 12 * blocks as u64) {
        Ok(b) => b,
        Err(e) => return e,
    };
    for i in 0..blocks {
        let word = |v: u64| ((v >> (32 * i)) as u32).to_le_bytes();
        out[i * 12..i * 12 + 4].copy_from_slice(&word(c.eff));
        out[i * 12 + 4..i * 12 + 8].copy_from_slice(&word(c.perm));
        out[i * 12 + 8..i * 12 + 12].copy_from_slice(&word(c.inh));
    }
    0
}

pub fn sys_capset(hdr: u64, data: u64) -> i64 {
    let (blocks, pid) = match cap_header(hdr) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let me = task::current_pid();
    if pid != 0 && global(pid as u32) != Some(me) {
        return -err::EPERM;
    }
    let raw = match super::user_bytes(data, 12 * blocks as u64) {
        Ok(b) => b.to_vec(),
        Err(e) => return e,
    };
    let mut new = Caps { eff: 0, perm: 0, inh: 0 };
    for i in 0..blocks {
        let word = |at: usize| (u32::from_le_bytes(raw[i * 12 + at..i * 12 + at + 4].try_into().unwrap()) as u64) << (32 * i);
        new.eff |= word(0);
        new.perm |= word(4);
        new.inh |= word(8);
    }
    let mut p = of(me);
    let old = p.caps;
    // Linux's rules: nothing new may be permitted, nothing effective that
    // is not permitted, and nothing inheritable that was neither
    // inheritable nor permitted (unless CAP_SETPCAP).
    let inh_ok = new.inh & !(old.inh | old.perm) == 0 || old.has(CAP_SETPCAP);
    if !inh_ok || new.perm & !old.perm != 0 || new.eff & !new.perm != 0 || (new.inh | new.perm) & !ALL_CAPS != 0 {
        return -err::EPERM;
    }
    p.caps = new;
    PROCS.lock().insert(me, p);
    0
}

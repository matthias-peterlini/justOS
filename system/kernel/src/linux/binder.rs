//! Binder, the IPC of Android: /dev/binder, /dev/hwbinder and
//! /dev/vndbinder, as the Linux driver offers them (protocol 8, 64-bit).
//!
//! Each open of a device is a binder process. Processes own nodes (objects
//! that live in them, named by a pointer and a cookie of their own) and
//! hold references to the nodes of others by handles; handle 0 is the
//! context manager (servicemanager). A transaction copies its data into a
//! buffer in the receiver's mapped area and turns the objects in it into
//! what the receiver can use: a node of its own, a handle of its own,
//! descriptors (installed in it when it reads), copies of the buffers that
//! pointers lead to. A two-way transaction waits for its reply on the
//! thread that sent it; a call back into a process that waits goes to the
//! waiting thread. The owner of a node hears when others start and stop
//! holding it (BR_INCREFS, BR_ACQUIRE and the releases), and holders that
//! ask hear when its process ends (BR_DEAD_BINDER).

use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::mem::take;

use super::fd::Desc;
use super::shm::Shm;
use super::{err, user_bytes};
use crate::sync::IrqLock;
use crate::task::{self, WaitQueue};

// ioctls
const WRITE_READ: u32 = 0xC030_6201;
const SET_IDLE_TIMEOUT: u32 = 0x4008_6203;
const SET_MAX_THREADS: u32 = 0x4004_6205;
const SET_IDLE_PRIORITY: u32 = 0x4004_6206;
const SET_CONTEXT_MGR: u32 = 0x4004_6207;
const THREAD_EXIT: u32 = 0x4004_6208;
const VERSION: u32 = 0xC004_6209;
const GET_NODE_DEBUG_INFO: u32 = 0xC018_620B;
const GET_NODE_INFO_FOR_REF: u32 = 0xC018_620C;
const SET_CONTEXT_MGR_EXT: u32 = 0x4018_620D;
const ENABLE_ONEWAY_SPAM_DETECTION: u32 = 0x4004_6210;
const GET_EXTENDED_ERROR: u32 = 0xC00C_6211;

const PROTOCOL_VERSION: u32 = 8;

// What processes write (BC_).
const BC_TRANSACTION: u32 = 0x4040_6300;
const BC_REPLY: u32 = 0x4040_6301;
const BC_FREE_BUFFER: u32 = 0x4008_6303;
const BC_INCREFS: u32 = 0x4004_6304;
const BC_ACQUIRE: u32 = 0x4004_6305;
const BC_RELEASE: u32 = 0x4004_6306;
const BC_DECREFS: u32 = 0x4004_6307;
const BC_INCREFS_DONE: u32 = 0x4010_6308;
const BC_ACQUIRE_DONE: u32 = 0x4010_6309;
const BC_REGISTER_LOOPER: u32 = 0x630B;
const BC_ENTER_LOOPER: u32 = 0x630C;
const BC_EXIT_LOOPER: u32 = 0x630D;
const BC_REQUEST_DEATH_NOTIFICATION: u32 = 0x400C_630E;
const BC_CLEAR_DEATH_NOTIFICATION: u32 = 0x400C_630F;
const BC_DEAD_BINDER_DONE: u32 = 0x4008_6310;
const BC_TRANSACTION_SG: u32 = 0x4048_6311;
const BC_REPLY_SG: u32 = 0x4048_6312;

// What they read (BR_).
const BR_TRANSACTION_SEC_CTX: u32 = 0x8048_7202;
const BR_TRANSACTION: u32 = 0x8040_7202;
const BR_REPLY: u32 = 0x8040_7203;
const BR_DEAD_REPLY: u32 = 0x7205;
const BR_TRANSACTION_COMPLETE: u32 = 0x7206;
const BR_INCREFS: u32 = 0x8010_7207;
const BR_ACQUIRE: u32 = 0x8010_7208;
const BR_RELEASE: u32 = 0x8010_7209;
const BR_DECREFS: u32 = 0x8010_720A;
const BR_NOOP: u32 = 0x720C;
const BR_SPAWN_LOOPER: u32 = 0x720D;
const BR_DEAD_BINDER: u32 = 0x8008_720F;
const BR_CLEAR_DEATH_NOTIFICATION_DONE: u32 = 0x8008_7210;
const BR_FAILED_REPLY: u32 = 0x7211;

// The objects a transaction carries.
const TYPE_BINDER: u32 = 0x7362_2a85;
const TYPE_WEAK_BINDER: u32 = 0x7762_2a85;
const TYPE_HANDLE: u32 = 0x7368_2a85;
const TYPE_WEAK_HANDLE: u32 = 0x7768_2a85;
const TYPE_FD: u32 = 0x6664_2a85;
const TYPE_FDA: u32 = 0x6664_6185;
const TYPE_PTR: u32 = 0x7074_2a85;

const TF_ONE_WAY: u32 = 1;
const FLAG_TXN_SECURITY_CTX: u32 = 0x1000;
const BUFFER_HAS_PARENT: u32 = 1;

// Thread states.
const REGISTERED: u32 = 1;
const ENTERED: u32 = 2;
const EXITED: u32 = 4;

/// The most an area holds (Linux's limit) and a transaction carries.
const AREA_MAX: u64 = 4 << 20;
/// Room a read needs for the largest thing it writes at once.
const READ_ROOM: usize = 80;
/// The security context transactions to a node that asks for one carry.
const SECCTX: &[u8] = b"u:r:kernel:s0\0";

/// The device a path names: 0 binder, 1 hwbinder, 2 vndbinder (each its
/// own world, with its own context manager).
pub fn device(path: &str) -> Option<usize> {
    match path {
        "/dev/binder" => Some(0),
        "/dev/hwbinder" => Some(1),
        "/dev/vndbinder" => Some(2),
        _ => None,
    }
}

// ----------------------------------------------------------------------------
// State

struct State {
    next: u64,
    procs: BTreeMap<u64, Proc>,
    nodes: BTreeMap<u64, Node>,
    txns: BTreeMap<u64, Txn>,
    /// The context manager's node, by device.
    managers: [Option<u64>; 3],
    /// Queues to tell, descriptors and memory to let go, once unlocked.
    wakes: Vec<Arc<WaitQueue>>,
    trash: Vec<Arc<Desc>>,
    areas: Vec<Arc<Shm>>,
}

static STATE: IrqLock<State> = IrqLock::new(State {
    next: 0,
    procs: BTreeMap::new(),
    nodes: BTreeMap::new(),
    txns: BTreeMap::new(),
    managers: [None; 3],
    wakes: Vec::new(),
    trash: Vec::new(),
    areas: Vec::new(),
});

/// Runs `f` on the state; wakes and drops what it left for after.
fn with<T>(f: impl FnOnce(&mut State) -> T) -> T {
    let (r, wakes, trash, areas) = {
        let mut s = STATE.lock();
        let r = f(&mut s);
        (r, take(&mut s.wakes), take(&mut s.trash), take(&mut s.areas))
    };
    for q in &wakes {
        q.notify_all();
    }
    drop(trash);
    drop(areas);
    r
}

struct Proc {
    ctx: usize,
    wq: Arc<WaitQueue>,
    area: Option<Area>,
    threads: BTreeMap<u32, Thread>,
    todo: VecDeque<Work>,
    /// Its nodes, by its pointer.
    nodes: BTreeMap<u64, u64>,
    refs: BTreeMap<u32, Ref>,
    /// Its handles, by node.
    handles: BTreeMap<u64, u32>,
    max_threads: u32,
    /// Threads asked for (BR_SPAWN_LOOPER) and not registered yet, and
    /// those registered.
    requested: u32,
    started: u32,
}

/// The mapped area where transactions to a process arrive.
struct Area {
    shm: Arc<Shm>,
    /// Where the process mapped it (0 until mmap is done).
    base: u64,
    size: u64,
    bufs: BTreeMap<u64, Buf>,
}

struct Buf {
    len: u64,
    /// The references the objects in it hold until it is freed.
    held: Vec<Held>,
    /// The node of a one-way transaction: the next one to it waits.
    async_node: Option<u64>,
    /// Given to the process (only then can it free it).
    delivered: bool,
}

enum Held {
    /// A count on a node of the area's process.
    Node(u64, bool),
    /// A count on a handle of the area's process.
    Ref(u32, bool),
}

struct Thread {
    todo: VecDeque<Work>,
    /// Transactions it sent and waits for, and those it received and is to
    /// answer, innermost last.
    stack: Vec<u64>,
    looper: u32,
    /// A read returns without waiting (a new thread's first call).
    need_return: bool,
    /// Asleep waiting for work of the process.
    waiting: bool,
}

impl Thread {
    fn new() -> Thread {
        Thread { todo: VecDeque::new(), stack: Vec::new(), looper: 0, need_return: true, waiting: false }
    }
}

enum Work {
    Txn(u64),
    Complete,
    Error(u32),
    Node(u64),
    Dead(u64),
    ClearDone(u64),
}

struct Node {
    /// None once its process is gone.
    owner: Option<u64>,
    ptr: u64,
    cookie: u64,
    /// References to it (any), and those of them that are strong.
    refs: u32,
    strong_refs: u32,
    /// Counts held by buffers and by what the owner was told but has not
    /// acknowledged yet.
    local_strong: u32,
    local_weak: u32,
    /// What the owner was told it holds.
    has_strong: bool,
    has_weak: bool,
    pending_strong: bool,
    pending_weak: bool,
    queued: bool,
    secctx: bool,
    /// A one-way transaction to it is being handled; the next ones wait.
    async_busy: bool,
    async_todo: VecDeque<u64>,
}

impl Node {
    fn new(owner: u64, ptr: u64, cookie: u64, flags: u32) -> Node {
        Node {
            owner: Some(owner),
            ptr,
            cookie,
            refs: 0,
            strong_refs: 0,
            local_strong: 0,
            local_weak: 0,
            has_strong: false,
            has_weak: false,
            pending_strong: false,
            pending_weak: false,
            queued: false,
            secctx: flags & FLAG_TXN_SECURITY_CTX != 0,
            async_busy: false,
            async_todo: VecDeque::new(),
        }
    }

    /// Whether it should be held strongly, and at all.
    fn wants(&self) -> (bool, bool) {
        let strong = self.strong_refs > 0 || self.local_strong > 0;
        (strong, strong || self.refs > 0 || self.local_weak > 0)
    }
}

struct Ref {
    node: u64,
    strong: u32,
    weak: u32,
    death: Option<u64>,
}

struct Txn {
    /// The sending process and thread of a two-way transaction.
    from: Option<(u64, u32)>,
    sender_pid: u32,
    sender_euid: u32,
    to: u64,
    /// The thread that received a two-way transaction.
    to_thread: Option<u32>,
    node: Option<u64>,
    reply: bool,
    code: u32,
    flags: u32,
    /// Its buffer in the receiver's area.
    buf: u64,
    data_size: u64,
    offsets_size: u64,
    secctx: Option<u64>,
    /// Descriptors to install when it is read, and where their numbers go
    /// in the area.
    fds: Vec<(u64, Arc<Desc>)>,
}

fn rd32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

fn rd64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at.checked_add(8)?)?.try_into().ok()?))
}

fn put32(b: &mut [u8], at: usize, v: u32) {
    if let Some(d) = b.get_mut(at..at + 4) {
        d.copy_from_slice(&v.to_le_bytes());
    }
}

fn put64(b: &mut [u8], at: usize, v: u64) {
    if let Some(d) = b.get_mut(at..at + 8) {
        d.copy_from_slice(&v.to_le_bytes());
    }
}

fn align8(n: u64) -> u64 {
    (n + 7) & !7
}

// ----------------------------------------------------------------------------
// Opening, mapping, closing

pub struct Open {
    id: u64,
    pub wq: Arc<WaitQueue>,
}

pub fn open(ctx: usize) -> Arc<Open> {
    let wq = Arc::new(WaitQueue::new());
    let id = with(|s| {
        let id = s.id();
        let proc = Proc {
            ctx,
            wq: wq.clone(),
            area: None,
            threads: BTreeMap::new(),
            todo: VecDeque::new(),
            nodes: BTreeMap::new(),
            refs: BTreeMap::new(),
            handles: BTreeMap::new(),
            max_threads: 0,
            requested: 0,
            started: 0,
        };
        s.procs.insert(id, proc);
        id
    });
    Arc::new(Open { id, wq })
}

impl Drop for Open {
    fn drop(&mut self) {
        let id = self.id;
        with(|s| s.release(id));
    }
}

/// The memory of the area a process maps (mmap of the device): read-only
/// for it, written by the driver.
pub fn area(open: &Open, len: u64, prot: u32) -> Result<Arc<Shm>, i64> {
    const PROT_WRITE: u32 = 2;
    if prot & PROT_WRITE != 0 {
        return Err(-err::EPERM);
    }
    let size = len.min(AREA_MAX);
    with(|s| {
        let p = s.procs.get_mut(&open.id).ok_or(-err::EINVAL)?;
        if p.area.is_some() {
            return Err(-err::EBUSY);
        }
        let shm = Shm::new(String::from("binder"), size, 0);
        p.area = Some(Area { shm: shm.clone(), base: 0, size, bufs: BTreeMap::new() });
        Ok(shm)
    })
}

/// Where the process mapped its area.
pub fn mapped(open: &Open, at: u64) {
    with(|s| {
        if let Some(a) = s.procs.get_mut(&open.id).and_then(|p| p.area.as_mut()) {
            a.base = at;
        }
    });
}

/// Whether the calling thread has something to read (poll and epoll).
pub fn readable(open: &Open) -> bool {
    let tid = task::current_tid();
    let s = STATE.lock();
    let Some(p) = s.procs.get(&open.id) else { return true };
    match p.threads.get(&tid) {
        Some(t) => t.need_return || !t.todo.is_empty() || (proc_work(t) && !p.todo.is_empty()),
        None => true,
    }
}

/// Whether a thread takes work of the whole process (it waits for nothing
/// and is one of the process's looper threads).
fn proc_work(t: &Thread) -> bool {
    t.stack.is_empty() && t.todo.is_empty() && t.looper & (REGISTERED | ENTERED) != 0
}

// ----------------------------------------------------------------------------
// ioctl

pub fn ioctl(open: &Open, req: u64, arg: u64, nonblock: bool) -> i64 {
    let tid = task::current_tid();
    let id = open.id;
    let r = match req as u32 {
        WRITE_READ => write_read(open, tid, arg, nonblock),
        SET_MAX_THREADS => match user_bytes(arg, 4) {
            Ok(b) => {
                let n = rd32(b, 0).unwrap_or(0);
                with(|s| {
                    if let Some(p) = s.procs.get_mut(&id) {
                        p.max_threads = n;
                    }
                });
                0
            }
            Err(e) => e,
        },
        SET_CONTEXT_MGR => with(|s| s.set_manager(id, 0, 0, 0)),
        SET_CONTEXT_MGR_EXT => match user_bytes(arg, 24) {
            Ok(b) => {
                let (flags, ptr, cookie) = (rd32(b, 4).unwrap_or(0), rd64(b, 8).unwrap_or(0), rd64(b, 16).unwrap_or(0));
                with(|s| s.set_manager(id, flags, ptr, cookie))
            }
            Err(e) => e,
        },
        THREAD_EXIT => {
            with(|s| s.thread_exit(id, tid));
            0
        }
        VERSION => match user_bytes(arg, 4) {
            Ok(b) => {
                put32(b, 0, PROTOCOL_VERSION);
                0
            }
            Err(e) => e,
        },
        SET_IDLE_TIMEOUT | SET_IDLE_PRIORITY | ENABLE_ONEWAY_SPAM_DETECTION => 0,
        GET_NODE_DEBUG_INFO => match user_bytes(arg, 24) {
            Ok(b) => {
                let after = rd64(b, 0).unwrap_or(0);
                let next = with(|s| s.node_after(id, after));
                b.fill(0);
                if let Some((ptr, cookie, strong, weak)) = next {
                    put64(b, 0, ptr);
                    put64(b, 8, cookie);
                    put32(b, 16, strong as u32);
                    put32(b, 20, weak as u32);
                }
                0
            }
            Err(e) => e,
        },
        GET_NODE_INFO_FOR_REF => match user_bytes(arg, 24) {
            Ok(b) => {
                let handle = rd32(b, 0).unwrap_or(0);
                match with(|s| s.node_info(id, handle)) {
                    Ok((strong, weak)) => {
                        b.fill(0);
                        put32(b, 0, handle);
                        put32(b, 4, strong);
                        put32(b, 8, weak);
                        0
                    }
                    Err(e) => e,
                }
            }
            Err(e) => e,
        },
        GET_EXTENDED_ERROR => match user_bytes(arg, 12) {
            Ok(b) => {
                b.fill(0);
                0
            }
            Err(e) => e,
        },
        _ => -err::EINVAL,
    };
    with(|s| {
        if let Some(t) = s.procs.get_mut(&id).and_then(|p| p.threads.get_mut(&tid)) {
            t.need_return = false;
        }
    });
    r
}

fn write_read(open: &Open, tid: u32, arg: u64, nonblock: bool) -> i64 {
    let mut bwr = [0u64; 6];
    match user_bytes(arg, 48) {
        Ok(b) => {
            for (i, v) in bwr.iter_mut().enumerate() {
                *v = rd64(b, i * 8).unwrap_or(0);
            }
        }
        Err(e) => return e,
    }
    let [write_size, write_consumed, write_buffer, read_size, read_consumed, read_buffer] = bwr;
    let mut ret = 0;
    if write_size > write_consumed {
        match user_bytes(write_buffer.wrapping_add(write_consumed), write_size - write_consumed) {
            Ok(b) => {
                let cmds = b.to_vec();
                let (used, r) = write(open, tid, &cmds);
                bwr[1] += used as u64;
                if let Err(e) = r {
                    ret = e;
                }
            }
            Err(e) => ret = e,
        }
    }
    if ret == 0 && read_size > read_consumed {
        match read(open, tid, read_buffer.wrapping_add(read_consumed), (read_size - read_consumed) as usize, read_consumed == 0, nonblock) {
            Ok(n) => bwr[4] += n as u64,
            Err(e) => ret = e,
        }
    }
    match user_bytes(arg, 48) {
        Ok(b) => {
            for (i, v) in bwr.iter().enumerate() {
                put64(b, i * 8, *v);
            }
        }
        Err(e) => return e,
    }
    ret
}

// ----------------------------------------------------------------------------
// Writing commands

fn write(open: &Open, tid: u32, cmds: &[u8]) -> (usize, Result<(), i64>) {
    let mut at = 0;
    while at + 4 <= cmds.len() {
        let cmd = rd32(cmds, at).unwrap_or(0);
        let size = ((cmd >> 16) & 0x3fff) as usize;
        let Some(arg) = cmds.get(at + 4..at + 4 + size) else { return (at, Err(-err::EFAULT)) };
        if let Err(e) = command(open, tid, cmd, arg) {
            return (at, Err(e));
        }
        at += 4 + size;
    }
    (at, Ok(()))
}

fn command(open: &Open, tid: u32, cmd: u32, arg: &[u8]) -> Result<(), i64> {
    let id = open.id;
    let a32 = rd32(arg, 0).unwrap_or(0);
    let a64 = rd64(arg, 0).unwrap_or(0);
    match cmd {
        BC_TRANSACTION | BC_REPLY | BC_TRANSACTION_SG | BC_REPLY_SG => {
            let reply = cmd == BC_REPLY || cmd == BC_REPLY_SG;
            let prepared = prepare(arg);
            let sender = (task::current_pid(), super::ns::uid());
            with(|s| s.transact(id, tid, prepared, reply, sender));
        }
        BC_FREE_BUFFER => with(|s| s.free_buffer(id, a64)),
        BC_INCREFS | BC_ACQUIRE | BC_RELEASE | BC_DECREFS => {
            let strong = cmd == BC_ACQUIRE || cmd == BC_RELEASE;
            let inc = cmd == BC_INCREFS || cmd == BC_ACQUIRE;
            with(|s| s.change_ref(id, tid, a32, strong, inc));
        }
        BC_INCREFS_DONE | BC_ACQUIRE_DONE => {
            let cookie = rd64(arg, 8).unwrap_or(0);
            with(|s| s.acknowledged(id, a64, cookie, cmd == BC_ACQUIRE_DONE));
        }
        BC_REGISTER_LOOPER | BC_ENTER_LOOPER | BC_EXIT_LOOPER => with(|s| {
            let Some(p) = s.procs.get_mut(&id) else { return };
            let t = p.threads.entry(tid).or_insert_with(Thread::new);
            match cmd {
                BC_REGISTER_LOOPER => {
                    t.looper |= REGISTERED;
                    if p.requested > 0 {
                        p.requested -= 1;
                        p.started += 1;
                    }
                }
                BC_ENTER_LOOPER => t.looper |= ENTERED,
                _ => t.looper |= EXITED,
            }
        }),
        BC_REQUEST_DEATH_NOTIFICATION | BC_CLEAR_DEATH_NOTIFICATION => {
            let cookie = rd64(arg, 4).unwrap_or(0);
            with(|s| s.death_notification(id, tid, a32, cookie, cmd == BC_REQUEST_DEATH_NOTIFICATION));
        }
        BC_DEAD_BINDER_DONE => {}
        _ => {
            crate::debug!("binder", "unknown command {cmd:#x}");
            return Err(-err::EINVAL);
        }
    }
    Ok(())
}

/// What a transaction carries, copied from the sender before the state is
/// locked: its data, the offsets of its objects, the buffers its pointers
/// lead to (by object) and the descriptors it passes.
struct Prepared {
    target: u64,
    code: u32,
    flags: u32,
    data: Vec<u8>,
    offsets: Vec<u64>,
    extra: BTreeMap<usize, Vec<u8>>,
    descs: BTreeMap<i32, Arc<Desc>>,
}

fn prepare(tr: &[u8]) -> Result<Prepared, u32> {
    const FAILED: u32 = BR_FAILED_REPLY;
    let get = |at| rd64(tr, at).ok_or(FAILED);
    let (target, code, flags) = (get(0)?, rd32(tr, 16).ok_or(FAILED)?, rd32(tr, 20).ok_or(FAILED)?);
    let (data_size, offsets_size, buffer, offsets_ptr) = (get(32)?, get(40)?, get(48)?, get(56)?);
    if data_size > AREA_MAX || offsets_size > AREA_MAX || offsets_size % 8 != 0 {
        return Err(FAILED);
    }
    let data = user_bytes(buffer, data_size).map_err(|_| FAILED)?.to_vec();
    let raw = user_bytes(offsets_ptr, offsets_size).map_err(|_| FAILED)?;
    let offsets: Vec<u64> = raw.as_chunks::<8>().0.iter().map(|c| u64::from_le_bytes(*c)).collect();
    let mut p = Prepared { target, code, flags, data, offsets, extra: BTreeMap::new(), descs: BTreeMap::new() };
    let mut extra_total = 0u64;
    for i in 0..p.offsets.len() {
        let off = p.offsets[i] as usize;
        let need = |n: usize| if off.is_multiple_of(4) && off.checked_add(n).is_some_and(|e| e <= p.data.len()) { Ok(()) } else { Err(FAILED) };
        need(8)?;
        match rd32(&p.data, off).ok_or(FAILED)? {
            TYPE_BINDER | TYPE_WEAK_BINDER | TYPE_HANDLE | TYPE_WEAK_HANDLE => need(24)?,
            TYPE_FD => {
                need(24)?;
                let fd = rd32(&p.data, off + 8).ok_or(FAILED)? as i32;
                fetch_fd(&mut p.descs, fd)?;
            }
            TYPE_PTR => {
                need(40)?;
                let (ptr, len) = (rd64(&p.data, off + 8).ok_or(FAILED)?, rd64(&p.data, off + 16).ok_or(FAILED)?);
                extra_total += align8(len);
                if extra_total > AREA_MAX {
                    return Err(FAILED);
                }
                let copy = user_bytes(ptr, len).map_err(|_| FAILED)?.to_vec();
                p.extra.insert(i, copy);
            }
            TYPE_FDA => {
                need(32)?;
                let num = rd64(&p.data, off + 8).ok_or(FAILED)? as usize;
                let (parent, poff) = (rd64(&p.data, off + 16).ok_or(FAILED)? as usize, rd64(&p.data, off + 24).ok_or(FAILED)? as usize);
                let fds: Vec<i32> = {
                    let buf = p.extra.get(&parent).filter(|_| parent < i).ok_or(FAILED)?;
                    (0..num).map(|k| rd32(buf, poff + 4 * k).map(|v| v as i32)).collect::<Option<_>>().ok_or(FAILED)?
                };
                for fd in fds {
                    fetch_fd(&mut p.descs, fd)?;
                }
            }
            _ => return Err(FAILED),
        }
    }
    Ok(p)
}

fn fetch_fd(descs: &mut BTreeMap<i32, Arc<Desc>>, fd: i32) -> Result<(), u32> {
    if let alloc::collections::btree_map::Entry::Vacant(e) = descs.entry(fd) {
        e.insert(super::fd::get(fd).ok_or(BR_FAILED_REPLY)?);
    }
    Ok(())
}

// ----------------------------------------------------------------------------
// Reading

fn read(open: &Open, tid: u32, ptr: u64, room: usize, first: bool, nonblock: bool) -> Result<usize, i64> {
    if room < 4 {
        return Ok(0);
    }
    let id = open.id;
    loop {
        if let Some(out) = with(|s| s.read_once(id, tid, room, first)) {
            let dst = user_bytes(ptr, out.len() as u64)?;
            dst.copy_from_slice(&out);
            return Ok(out.len());
        }
        if nonblock {
            return Err(-err::EAGAIN);
        }
        if let Some(e) = super::signal::interrupted() {
            return Err(e);
        }
        let until = crate::time::uptime_ms() + 1000;
        task::wait_any(&[&*open.wq], Some(until), || readable(open).then_some(()));
    }
}

impl State {
    fn id(&mut self) -> u64 {
        self.next += 1;
        self.next
    }

    /// Adds work for a thread of a process (or the process, when the thread
    /// is gone or not named) and wakes its threads.
    fn push(&mut self, proc: u64, thread: Option<u32>, w: Work) {
        let Some(p) = self.procs.get_mut(&proc) else { return };
        match thread.and_then(|t| p.threads.get_mut(&t)) {
            Some(t) => t.todo.push_back(w),
            None => p.todo.push_back(w),
        }
        if !self.wakes.iter().any(|q| Arc::ptr_eq(q, &p.wq)) {
            self.wakes.push(p.wq.clone());
        }
    }

    /// Work for the thread when it is a looper, else for its process.
    fn push_looper(&mut self, proc: u64, tid: u32, w: Work) {
        let looper = self.procs.get(&proc).and_then(|p| p.threads.get(&tid)).is_some_and(|t| t.looper & (REGISTERED | ENTERED) != 0);
        self.push(proc, looper.then_some(tid), w);
    }

    /// What one read gives back, or None to wait.
    fn read_once(&mut self, id: u64, tid: u32, room: usize, first: bool) -> Option<Vec<u8>> {
        let p = self.procs.get_mut(&id)?;
        let t = p.threads.entry(tid).or_insert_with(Thread::new);
        t.waiting = false;
        let for_proc = proc_work(t);
        if t.todo.is_empty() && !(for_proc && !p.todo.is_empty()) && !t.need_return {
            t.waiting = for_proc;
            return None;
        }
        let need_return = t.need_return;
        let mut out = Vec::new();
        if first {
            out.extend_from_slice(&BR_NOOP.to_le_bytes());
        }
        let start = out.len();
        let mut full = false;
        loop {
            if out.len() + READ_ROOM > room {
                full = true;
                break;
            }
            let Some(p) = self.procs.get_mut(&id) else { break };
            let Some(t) = p.threads.get_mut(&tid) else { break };
            let w = match t.todo.pop_front() {
                Some(w) => w,
                None if for_proc => match p.todo.pop_front() {
                    Some(w) => w,
                    None => break,
                },
                None => break,
            };
            match w {
                Work::Complete => out.extend_from_slice(&BR_TRANSACTION_COMPLETE.to_le_bytes()),
                Work::Error(code) => out.extend_from_slice(&code.to_le_bytes()),
                Work::Node(n) => self.node_work(n, &mut out),
                Work::Dead(cookie) => {
                    out.extend_from_slice(&BR_DEAD_BINDER.to_le_bytes());
                    out.extend_from_slice(&cookie.to_le_bytes());
                    break;
                }
                Work::ClearDone(cookie) => {
                    out.extend_from_slice(&BR_CLEAR_DEATH_NOTIFICATION_DONE.to_le_bytes());
                    out.extend_from_slice(&cookie.to_le_bytes());
                }
                Work::Txn(t) => {
                    if self.deliver(id, tid, t, &mut out) {
                        break;
                    }
                }
            }
        }
        let p = self.procs.get_mut(&id)?;
        if out.len() == start && !full && !need_return {
            // Someone else took the work.
            if let Some(t) = p.threads.get_mut(&tid) {
                t.waiting = for_proc;
            }
            return None;
        }
        // Ask for another looper thread when none is left waiting.
        let looper = p.threads.get(&tid).map_or(0, |t| t.looper);
        let idle = p.threads.values().any(|t| t.waiting);
        if first && p.requested == 0 && !idle && p.started < p.max_threads && looper & (REGISTERED | ENTERED) != 0 && looper & EXITED == 0 {
            p.requested += 1;
            out[..4].copy_from_slice(&BR_SPAWN_LOOPER.to_le_bytes());
        }
        Some(out)
    }

    /// Gives a transaction to the thread reading: installs its descriptors
    /// and writes BR_TRANSACTION or BR_REPLY. False if it is gone.
    fn deliver(&mut self, id: u64, tid: u32, txn: u64, out: &mut Vec<u8>) -> bool {
        let Some(t) = self.txns.get_mut(&txn) else { return false };
        let fds = take(&mut t.fds);
        let (code, flags, buf, data_size, offsets_size, reply, secctx) = (t.code, t.flags, t.buf, t.data_size, t.offsets_size, t.reply, t.secctx);
        let sender_pid = if t.from.is_some() { super::ns::local(t.sender_pid) } else { 0 };
        let sender_euid = t.sender_euid;
        let node = t.node.and_then(|n| self.nodes.get(&n));
        let (ptr, cookie) = node.map_or((0, 0), |n| (n.ptr, n.cookie));
        let wants_ctx = !reply && node.is_some_and(|n| n.secctx);
        let Some(area) = self.procs.get_mut(&id).and_then(|p| p.area.as_mut()) else { return false };
        for (at, d) in fds {
            let fd = super::fd::install(d, true);
            if fd >= 0 {
                let _ = area.shm.write_at(at, &(fd as u32).to_le_bytes());
            }
        }
        if let Some(b) = area.bufs.get_mut(&buf) {
            b.delivered = true;
        }
        let base = area.base;
        let mut tr = [0u8; 64];
        put64(&mut tr, 0, ptr);
        put64(&mut tr, 8, cookie);
        put32(&mut tr, 16, code);
        put32(&mut tr, 20, flags);
        put32(&mut tr, 24, sender_pid);
        put32(&mut tr, 28, sender_euid);
        put64(&mut tr, 32, data_size);
        put64(&mut tr, 40, offsets_size);
        put64(&mut tr, 48, base + buf);
        put64(&mut tr, 56, base + buf + align8(data_size));
        let cmd = if reply {
            BR_REPLY
        } else if wants_ctx {
            BR_TRANSACTION_SEC_CTX
        } else {
            BR_TRANSACTION
        };
        out.extend_from_slice(&cmd.to_le_bytes());
        out.extend_from_slice(&tr);
        if cmd == BR_TRANSACTION_SEC_CTX {
            out.extend_from_slice(&secctx.map_or(0, |c| base + c).to_le_bytes());
        }
        if reply || flags & TF_ONE_WAY != 0 {
            self.txns.remove(&txn);
        } else {
            if let Some(t) = self.txns.get_mut(&txn) {
                t.to_thread = Some(tid);
            }
            if let Some(t) = self.procs.get_mut(&id).and_then(|p| p.threads.get_mut(&tid)) {
                t.stack.push(txn);
            }
        }
        true
    }

    /// Tells the owner of a node what changed in how it is held.
    fn node_work(&mut self, id: u64, out: &mut Vec<u8>) {
        let Some(n) = self.nodes.get_mut(&id) else { return };
        n.queued = false;
        let Some(owner) = n.owner else { return };
        let (strong, weak) = n.wants();
        let mut put = |cmd: u32, n: &Node| {
            out.extend_from_slice(&cmd.to_le_bytes());
            out.extend_from_slice(&n.ptr.to_le_bytes());
            out.extend_from_slice(&n.cookie.to_le_bytes());
        };
        if weak && !n.has_weak {
            n.has_weak = true;
            n.pending_weak = true;
            n.local_weak += 1;
            put(BR_INCREFS, n);
        }
        if strong && !n.has_strong {
            n.has_strong = true;
            n.pending_strong = true;
            n.local_strong += 1;
            put(BR_ACQUIRE, n);
        }
        if !strong && n.has_strong {
            n.has_strong = false;
            put(BR_RELEASE, n);
        }
        if !weak && n.has_weak {
            n.has_weak = false;
            put(BR_DECREFS, n);
        }
        if !strong && !weak && !n.has_strong && !n.has_weak {
            let ptr = n.ptr;
            self.nodes.remove(&id);
            if let Some(p) = self.procs.get_mut(&owner) {
                if p.nodes.get(&ptr) == Some(&id) {
                    p.nodes.remove(&ptr);
                }
            }
        }
    }

    /// After a count of a node changed: tells its owner if it must hear,
    /// forgets it if nothing holds it. `here`: the thread at work, which
    /// hears first when it is the owner's.
    fn node_changed(&mut self, id: u64, here: Option<(u64, u32)>) {
        let Some(n) = self.nodes.get_mut(&id) else { return };
        let (strong, weak) = n.wants();
        let Some(owner) = n.owner else {
            if !weak {
                self.nodes.remove(&id);
            }
            return;
        };
        if n.queued {
            return;
        }
        if strong == n.has_strong && weak == n.has_weak {
            if !weak && !n.has_weak {
                let ptr = n.ptr;
                self.nodes.remove(&id);
                if let Some(p) = self.procs.get_mut(&owner) {
                    if p.nodes.get(&ptr) == Some(&id) {
                        p.nodes.remove(&ptr);
                    }
                }
            }
            return;
        }
        n.queued = true;
        let thread = here.filter(|h| h.0 == owner).map(|h| h.1);
        self.push(owner, thread, Work::Node(id));
    }

    // ------------------------------------------------------------------------
    // Nodes and references

    /// The node of a process's object `ptr`, made when first sent.
    fn node_for(&mut self, proc: u64, ptr: u64, cookie: u64, flags: u32) -> Result<u64, u32> {
        if let Some(&id) = self.procs.get(&proc).ok_or(BR_FAILED_REPLY)?.nodes.get(&ptr) {
            return match self.nodes.get(&id) {
                Some(n) if n.cookie == cookie => Ok(id),
                _ => Err(BR_FAILED_REPLY),
            };
        }
        let id = self.id();
        self.nodes.insert(id, Node::new(proc, ptr, cookie, flags));
        if let Some(p) = self.procs.get_mut(&proc) {
            p.nodes.insert(ptr, id);
        }
        Ok(id)
    }

    /// The handle of `proc` for `node`, made (with no counts) if it has none:
    /// 0 for the context manager, else the lowest free number.
    fn ref_for(&mut self, proc: u64, node: u64) -> Option<u32> {
        let p = self.procs.get_mut(&proc)?;
        if let Some(&h) = p.handles.get(&node) {
            return Some(h);
        }
        let h = if self.managers[p.ctx] == Some(node) && !p.refs.contains_key(&0) {
            0
        } else {
            let mut h = 1;
            for &k in p.refs.keys() {
                if k == h {
                    h += 1;
                } else if k > h {
                    break;
                }
            }
            h
        };
        p.refs.insert(h, Ref { node, strong: 0, weak: 0, death: None });
        p.handles.insert(node, h);
        if let Some(n) = self.nodes.get_mut(&node) {
            n.refs += 1;
        }
        Some(h)
    }

    fn ref_inc(&mut self, proc: u64, h: u32, strong: bool, here: Option<(u64, u32)>) -> bool {
        let Some(r) = self.procs.get_mut(&proc).and_then(|p| p.refs.get_mut(&h)) else { return false };
        let node = r.node;
        let first = if strong {
            r.strong += 1;
            r.strong == 1
        } else {
            r.weak += 1;
            false
        };
        if first {
            if let Some(n) = self.nodes.get_mut(&node) {
                n.strong_refs += 1;
            }
        }
        self.node_changed(node, here);
        true
    }

    fn ref_dec(&mut self, proc: u64, h: u32, strong: bool) -> bool {
        let Some(p) = self.procs.get_mut(&proc) else { return false };
        let Some(r) = p.refs.get_mut(&h) else { return false };
        let node = r.node;
        let mut last_strong = false;
        if strong {
            if r.strong == 0 {
                return false;
            }
            r.strong -= 1;
            last_strong = r.strong == 0;
        } else {
            if r.weak == 0 {
                return false;
            }
            r.weak -= 1;
        }
        let gone = r.strong == 0 && r.weak == 0;
        if gone {
            p.refs.remove(&h);
            p.handles.remove(&node);
        }
        if let Some(n) = self.nodes.get_mut(&node) {
            if last_strong {
                n.strong_refs = n.strong_refs.saturating_sub(1);
            }
            if gone {
                n.refs = n.refs.saturating_sub(1);
            }
        }
        self.node_changed(node, None);
        true
    }

    /// BC_INCREFS, BC_ACQUIRE, BC_RELEASE, BC_DECREFS. The first count on
    /// handle 0 makes the reference to the context manager.
    fn change_ref(&mut self, proc: u64, tid: u32, h: u32, strong: bool, inc: bool) {
        let Some(p) = self.procs.get(&proc) else { return };
        if !inc {
            if !self.ref_dec(proc, h, strong) {
                crate::debug!("binder", "release of handle {h} it does not hold");
            }
            return;
        }
        if !p.refs.contains_key(&h) {
            let manager = self.managers[p.ctx].filter(|m| self.nodes.get(m).is_some_and(|n| n.owner != Some(proc)));
            match manager {
                Some(m) if h == 0 => {
                    self.ref_for(proc, m);
                }
                _ => {
                    crate::debug!("binder", "count on unknown handle {h}");
                    return;
                }
            }
        }
        self.ref_inc(proc, h, strong, Some((proc, tid)));
    }

    /// BC_INCREFS_DONE, BC_ACQUIRE_DONE: the owner took what it was told.
    fn acknowledged(&mut self, proc: u64, ptr: u64, cookie: u64, strong: bool) {
        let Some(&id) = self.procs.get(&proc).and_then(|p| p.nodes.get(&ptr)) else { return };
        let Some(n) = self.nodes.get_mut(&id) else { return };
        if n.cookie != cookie {
            return;
        }
        if strong && n.pending_strong {
            n.pending_strong = false;
            n.local_strong -= 1;
        } else if !strong && n.pending_weak {
            n.pending_weak = false;
            n.local_weak -= 1;
        } else {
            return;
        }
        self.node_changed(id, None);
    }

    fn death_notification(&mut self, proc: u64, tid: u32, h: u32, cookie: u64, request: bool) {
        let Some(r) = self.procs.get_mut(&proc).and_then(|p| p.refs.get_mut(&h)) else { return };
        if request {
            if r.death.is_some() {
                return;
            }
            r.death = Some(cookie);
            let node = r.node;
            if self.nodes.get(&node).is_none_or(|n| n.owner.is_none()) {
                self.push_looper(proc, tid, Work::Dead(cookie));
            }
        } else {
            if r.death != Some(cookie) {
                return;
            }
            r.death = None;
            self.push_looper(proc, tid, Work::ClearDone(cookie));
        }
    }

    fn set_manager(&mut self, proc: u64, flags: u32, ptr: u64, cookie: u64) -> i64 {
        let Some(ctx) = self.procs.get(&proc).map(|p| p.ctx) else { return -err::EINVAL };
        if self.managers[ctx].is_some_and(|m| self.nodes.get(&m).is_some_and(|n| n.owner.is_some())) {
            return -err::EBUSY;
        }
        let Ok(id) = self.node_for(proc, ptr, cookie, flags) else { return -err::EINVAL };
        if let Some(n) = self.nodes.get_mut(&id) {
            // Held for as long as its process lives; it never hears of it.
            n.local_strong += 1;
            n.local_weak += 1;
            n.has_strong = true;
            n.has_weak = true;
            n.secctx = flags & FLAG_TXN_SECURITY_CTX != 0;
        }
        self.managers[ctx] = Some(id);
        0
    }

    fn node_after(&self, proc: u64, after: u64) -> Option<(u64, u64, bool, bool)> {
        let p = self.procs.get(&proc)?;
        let (_, &id) = p.nodes.range(after + 1..).next()?;
        let n = self.nodes.get(&id)?;
        Some((n.ptr, n.cookie, n.has_strong, n.has_weak))
    }

    /// How held the node behind a handle is (for the context manager: it
    /// tells lazy services whether they still have clients).
    fn node_info(&self, proc: u64, h: u32) -> Result<(u32, u32), i64> {
        let p = self.procs.get(&proc).ok_or(-err::EINVAL)?;
        let manager = self.managers[p.ctx].and_then(|m| self.nodes.get(&m)).and_then(|n| n.owner);
        if manager != Some(proc) {
            return Err(-err::EPERM);
        }
        let n = p.refs.get(&h).and_then(|r| self.nodes.get(&r.node)).ok_or(-err::EINVAL)?;
        Ok((n.strong_refs + n.local_strong, n.local_weak))
    }

    // ------------------------------------------------------------------------
    // Transactions

    /// BC_TRANSACTION and BC_REPLY.
    fn transact(&mut self, pid: u64, tid: u32, p: Result<Prepared, u32>, reply: bool, sender: (u32, u32)) {
        if !self.procs.contains_key(&pid) {
            return;
        }
        let me = (pid, tid);
        if let Some(p) = self.procs.get_mut(&pid) {
            p.threads.entry(tid).or_insert_with(Thread::new);
        }
        if reply {
            // The call this answers: the innermost one the thread received.
            let top = self.procs.get(&pid).and_then(|p| p.threads.get(&tid)).and_then(|t| t.stack.last().copied());
            let Some(call) = top.filter(|t| self.txns.get(t).is_some_and(|t| t.to_thread == Some(tid) && !t.reply)) else {
                crate::debug!("binder", "reply with no call to answer");
                self.push(pid, Some(tid), Work::Error(BR_FAILED_REPLY));
                return;
            };
            if let Some(t) = self.procs.get_mut(&pid).and_then(|p| p.threads.get_mut(&tid)) {
                t.stack.pop();
            }
            let from = self.txns.remove(&call).and_then(|t| t.from);
            let from = from.filter(|&(fp, ft)| self.procs.get(&fp).is_some_and(|p| p.threads.contains_key(&ft)));
            self.push(pid, Some(tid), Work::Complete);
            let Some((fp, ft)) = from else { return };
            if let Some(t) = self.procs.get_mut(&fp).and_then(|p| p.threads.get_mut(&ft)) {
                t.stack.retain(|&x| x != call);
            }
            let r = match p {
                Ok(p) => self.send(me, p, fp, Some(ft), None, true, sender).map(|_| ()),
                Err(code) => Err(code),
            };
            if let Err(code) = r {
                self.push(fp, Some(ft), Work::Error(code));
            }
            return;
        }
        let p = match p {
            Ok(p) => p,
            Err(code) => {
                self.push(pid, Some(tid), Work::Error(code));
                return;
            }
        };
        let r = self.target(pid, tid, &p).and_then(|(to, to_thread, node)| self.send(me, p, to, to_thread, Some(node), false, sender));
        match r {
            Ok(Some(txn)) => {
                if let Some(t) = self.procs.get_mut(&pid).and_then(|p| p.threads.get_mut(&tid)) {
                    t.stack.push(txn);
                }
                self.push(pid, Some(tid), Work::Complete);
            }
            Ok(None) => self.push(pid, Some(tid), Work::Complete),
            Err(code) => self.push(pid, Some(tid), Work::Error(code)),
        }
    }

    /// Where a transaction goes: the process owning the node behind the
    /// handle, and the thread when it is one waiting for this thread.
    fn target(&self, pid: u64, tid: u32, p: &Prepared) -> Result<(u64, Option<u32>, u64), u32> {
        let proc = self.procs.get(&pid).ok_or(BR_FAILED_REPLY)?;
        let h = p.target as u32;
        let node = match proc.refs.get(&h) {
            Some(r) => r.node,
            None if h == 0 => self.managers[proc.ctx].ok_or(BR_DEAD_REPLY)?,
            None => return Err(BR_FAILED_REPLY),
        };
        let to = self.nodes.get(&node).and_then(|n| n.owner).ok_or(BR_DEAD_REPLY)?;
        let mut to_thread = None;
        if p.flags & TF_ONE_WAY == 0 {
            if let Some(t) = proc.threads.get(&tid) {
                for x in t.stack.iter().rev() {
                    let Some(x) = self.txns.get(x) else { continue };
                    if x.to_thread == Some(tid) {
                        if let Some((_, ft)) = x.from.filter(|f| f.0 == to) {
                            to_thread = Some(ft);
                            break;
                        }
                    }
                }
            }
        }
        Ok((to, to_thread, node))
    }

    /// Copies a transaction into the receiver's area, turning its objects
    /// into the receiver's, and queues it. The transaction's id when it
    /// waits for a reply.
    #[allow(clippy::too_many_arguments)]
    fn send(&mut self, from: (u64, u32), mut p: Prepared, to: u64, to_thread: Option<u32>, node: Option<u64>, reply: bool, sender: (u32, u32)) -> Result<Option<u64>, u32> {
        let descs = take(&mut p.descs);
        let r = self.send_inner(from, &mut p, &descs, to, to_thread, node, reply, sender);
        self.trash.extend(descs.into_values());
        r
    }

    #[allow(clippy::too_many_arguments)]
    fn send_inner(&mut self, from: (u64, u32), p: &mut Prepared, descs: &BTreeMap<i32, Arc<Desc>>, to: u64, to_thread: Option<u32>, node: Option<u64>, reply: bool, sender: (u32, u32)) -> Result<Option<u64>, u32> {
        let oneway = !reply && p.flags & TF_ONE_WAY != 0;
        let secctx = !reply && node.and_then(|n| self.nodes.get(&n)).is_some_and(|n| n.secctx);
        let data_len = align8(p.data.len() as u64);
        let offs_len = p.offsets.len() as u64 * 8;
        let mut extra_at = BTreeMap::new();
        let mut at_end = data_len + offs_len;
        for (&i, v) in &p.extra {
            extra_at.insert(i, at_end);
            at_end += align8(v.len() as u64);
        }
        let ctx_at = at_end;
        let total = (ctx_at + if secctx { align8(SECCTX.len() as u64) } else { 0 }).max(8);
        let (base, at, shm) = {
            let area = self.procs.get_mut(&to).and_then(|p| p.area.as_mut()).filter(|a| a.base != 0).ok_or(BR_DEAD_REPLY)?;
            let at = area.alloc(total).ok_or(BR_FAILED_REPLY)?;
            area.bufs.insert(at, Buf { len: total, held: Vec::new(), async_node: None, delivered: false });
            (area.base, at, area.shm.clone())
        };
        let mut held = Vec::new();
        let mut fds = Vec::new();
        if let Err(code) = self.translate(from, to, p, descs, base + at, at, &extra_at, &mut held, &mut fds) {
            self.release_held(to, held);
            if let Some(a) = self.procs.get_mut(&to).and_then(|p| p.area.as_mut()) {
                a.bufs.remove(&at);
            }
            self.trash.extend(fds.into_iter().map(|(_, d)| d));
            return Err(code);
        }
        let offsets: Vec<u8> = p.offsets.iter().flat_map(|o| o.to_le_bytes()).collect();
        let _ = shm.write_at(at, &p.data);
        let _ = shm.write_at(at + data_len, &offsets);
        for (i, v) in &p.extra {
            let _ = shm.write_at(at + extra_at[i], v);
        }
        if secctx {
            let _ = shm.write_at(at + ctx_at, SECCTX);
        }
        if let Some(b) = self.procs.get_mut(&to).and_then(|p| p.area.as_mut()).and_then(|a| a.bufs.get_mut(&at)) {
            b.held = held;
            b.async_node = if oneway { node } else { None };
        }
        let id = self.id();
        let txn = Txn {
            from: (!reply && !oneway).then_some(from),
            sender_pid: sender.0,
            sender_euid: sender.1,
            to,
            to_thread: None,
            node,
            reply,
            code: p.code,
            flags: p.flags,
            buf: at,
            data_size: p.data.len() as u64,
            offsets_size: offs_len,
            secctx: secctx.then_some(at + ctx_at),
            fds,
        };
        self.txns.insert(id, txn);
        if oneway {
            let n = node.and_then(|n| self.nodes.get_mut(&n));
            match n {
                Some(n) if n.async_busy => n.async_todo.push_back(id),
                Some(n) => {
                    n.async_busy = true;
                    self.push(to, None, Work::Txn(id));
                }
                None => self.push(to, None, Work::Txn(id)),
            }
            return Ok(None);
        }
        self.push(to, to_thread, Work::Txn(id));
        Ok((!reply).then_some(id))
    }

    /// Turns the objects of a transaction into the receiver's.
    #[allow(clippy::too_many_arguments)]
    fn translate(
        &mut self,
        from: (u64, u32),
        to: u64,
        p: &mut Prepared,
        descs: &BTreeMap<i32, Arc<Desc>>,
        user_at: u64,
        at: u64,
        extra_at: &BTreeMap<usize, u64>,
        held: &mut Vec<Held>,
        fds: &mut Vec<(u64, Arc<Desc>)>,
    ) -> Result<(), u32> {
        const FAILED: u32 = BR_FAILED_REPLY;
        for i in 0..p.offsets.len() {
            let off = p.offsets[i] as usize;
            let ty = rd32(&p.data, off).ok_or(FAILED)?;
            match ty {
                TYPE_BINDER | TYPE_WEAK_BINDER => {
                    let strong = ty == TYPE_BINDER;
                    let (flags, ptr, cookie) = (rd32(&p.data, off + 4).ok_or(FAILED)?, rd64(&p.data, off + 8).ok_or(FAILED)?, rd64(&p.data, off + 16).ok_or(FAILED)?);
                    let node = self.node_for(from.0, ptr, cookie, flags)?;
                    let h = self.ref_for(to, node).ok_or(FAILED)?;
                    self.ref_inc(to, h, strong, Some(from));
                    held.push(Held::Ref(h, strong));
                    put32(&mut p.data, off, if strong { TYPE_HANDLE } else { TYPE_WEAK_HANDLE });
                    put64(&mut p.data, off + 8, h as u64);
                    put64(&mut p.data, off + 16, 0);
                }
                TYPE_HANDLE | TYPE_WEAK_HANDLE => {
                    let strong = ty == TYPE_HANDLE;
                    let h = rd32(&p.data, off + 8).ok_or(FAILED)?;
                    let node = self.procs.get(&from.0).and_then(|p| p.refs.get(&h)).map(|r| r.node).ok_or(FAILED)?;
                    let n = self.nodes.get_mut(&node).ok_or(FAILED)?;
                    if n.owner == Some(to) {
                        // Back home: the receiver's own object.
                        put32(&mut p.data, off, if strong { TYPE_BINDER } else { TYPE_WEAK_BINDER });
                        put64(&mut p.data, off + 8, n.ptr);
                        put64(&mut p.data, off + 16, n.cookie);
                        if strong {
                            n.local_strong += 1;
                        } else {
                            n.local_weak += 1;
                        }
                        held.push(Held::Node(node, strong));
                        self.node_changed(node, None);
                    } else {
                        let h2 = self.ref_for(to, node).ok_or(FAILED)?;
                        self.ref_inc(to, h2, strong, None);
                        held.push(Held::Ref(h2, strong));
                        put64(&mut p.data, off + 8, h2 as u64);
                        put64(&mut p.data, off + 16, 0);
                    }
                }
                TYPE_FD => {
                    let fd = rd32(&p.data, off + 8).ok_or(FAILED)? as i32;
                    fds.push((at + off as u64 + 8, descs.get(&fd).ok_or(FAILED)?.clone()));
                }
                TYPE_PTR => {
                    let flags = rd32(&p.data, off + 4).ok_or(FAILED)?;
                    let here = user_at + extra_at.get(&i).ok_or(FAILED)?;
                    put64(&mut p.data, off + 8, here);
                    if flags & BUFFER_HAS_PARENT != 0 {
                        let (parent, poff) = (rd64(&p.data, off + 24).ok_or(FAILED)? as usize, rd64(&p.data, off + 32).ok_or(FAILED)? as usize);
                        let buf = p.extra.get_mut(&parent).filter(|_| parent < i).ok_or(FAILED)?;
                        if poff.checked_add(8).is_none_or(|e| e > buf.len()) {
                            return Err(FAILED);
                        }
                        put64(buf, poff, here);
                    }
                }
                TYPE_FDA => {
                    let num = rd64(&p.data, off + 8).ok_or(FAILED)? as usize;
                    let (parent, poff) = (rd64(&p.data, off + 16).ok_or(FAILED)? as usize, rd64(&p.data, off + 24).ok_or(FAILED)? as usize);
                    let buf = p.extra.get(&parent).ok_or(FAILED)?;
                    let pat = at + extra_at.get(&parent).ok_or(FAILED)?;
                    for k in 0..num {
                        let fd = rd32(buf, poff + 4 * k).ok_or(FAILED)? as i32;
                        fds.push((pat + (poff + 4 * k) as u64, descs.get(&fd).ok_or(FAILED)?.clone()));
                    }
                }
                _ => return Err(FAILED),
            }
        }
        Ok(())
    }

    /// Lets go of what a buffer of `proc` held.
    fn release_held(&mut self, proc: u64, held: Vec<Held>) {
        for h in held {
            match h {
                Held::Node(n, strong) => {
                    if let Some(node) = self.nodes.get_mut(&n) {
                        if strong {
                            node.local_strong = node.local_strong.saturating_sub(1);
                        } else {
                            node.local_weak = node.local_weak.saturating_sub(1);
                        }
                    }
                    self.node_changed(n, None);
                }
                Held::Ref(h, strong) => {
                    self.ref_dec(proc, h, strong);
                }
            }
        }
    }

    /// BC_FREE_BUFFER: the process is done with a buffer it received.
    fn free_buffer(&mut self, proc: u64, ptr: u64) {
        let Some(area) = self.procs.get_mut(&proc).and_then(|p| p.area.as_mut()) else { return };
        let off = ptr.wrapping_sub(area.base);
        if !area.bufs.get(&off).is_some_and(|b| b.delivered) {
            crate::debug!("binder", "free of unknown buffer {ptr:#x}");
            return;
        }
        let Some(b) = area.bufs.remove(&off) else { return };
        self.release_held(proc, b.held);
        if let Some(n) = b.async_node {
            // The next one-way transaction to the node can go.
            let next = self.nodes.get_mut(&n).map(|node| {
                let next = node.async_todo.pop_front();
                node.async_busy = next.is_some();
                next
            });
            if let Some(Some(t)) = next {
                self.push(proc, None, Work::Txn(t));
            }
        }
    }

    /// A transaction that will never be answered: its sender hears
    /// BR_DEAD_REPLY.
    fn fail_call(&mut self, txn: u64) {
        let Some(t) = self.txns.remove(&txn) else { return };
        self.trash.extend(t.fds.into_iter().map(|(_, d)| d));
        if t.reply {
            return;
        }
        if let Some((fp, ft)) = t.from {
            if let Some(th) = self.procs.get_mut(&fp).and_then(|p| p.threads.get_mut(&ft)) {
                th.stack.retain(|&x| x != txn);
                self.push(fp, Some(ft), Work::Error(BR_DEAD_REPLY));
            }
        }
    }

    /// A thread's calls end with it: those it was to answer fail, those it
    /// waits for go nowhere.
    fn end_thread(&mut self, tid: u32, t: Thread) {
        for x in t.stack {
            match self.txns.get_mut(&x) {
                Some(txn) if txn.to_thread == Some(tid) && !txn.reply => self.fail_call(x),
                Some(txn) => txn.from = None,
                None => {}
            }
        }
        for w in t.todo {
            if let Work::Txn(x) = w {
                self.fail_call(x);
            }
        }
    }

    fn thread_exit(&mut self, proc: u64, tid: u32) {
        if let Some(t) = self.procs.get_mut(&proc).and_then(|p| p.threads.remove(&tid)) {
            self.end_thread(tid, t);
        }
    }

    /// The process closed the device (or ended): its calls fail, its nodes
    /// die (holders that asked hear), its references and buffers go.
    fn release(&mut self, id: u64) {
        let Some(mut p) = self.procs.remove(&id) else { return };
        for (tid, t) in take(&mut p.threads) {
            self.end_thread(tid, t);
        }
        for w in take(&mut p.todo) {
            if let Work::Txn(x) = w {
                self.fail_call(x);
            }
        }
        for m in self.managers.iter_mut() {
            if m.is_some_and(|n| self.nodes.get(&n).is_some_and(|n| n.owner == Some(id))) {
                *m = None;
            }
        }
        let nodes: Vec<u64> = p.nodes.values().copied().collect();
        for &n in &nodes {
            let waiting = match self.nodes.get_mut(&n) {
                Some(node) => {
                    node.owner = None;
                    node.queued = false;
                    take(&mut node.async_todo)
                }
                None => continue,
            };
            for x in waiting {
                if let Some(t) = self.txns.remove(&x) {
                    self.trash.extend(t.fds.into_iter().map(|(_, d)| d));
                }
            }
        }
        // Holders that asked hear of the death.
        let mut deaths = Vec::new();
        for (&pid, q) in &self.procs {
            for r in q.refs.values() {
                if let Some(cookie) = r.death.filter(|_| nodes.contains(&r.node)) {
                    deaths.push((pid, cookie));
                }
            }
        }
        crate::debug!("binder", "process {id} released: {} nodes, {} death notices", nodes.len(), deaths.len());
        for (pid, cookie) in deaths {
            self.push(pid, None, Work::Dead(cookie));
        }
        // Its references.
        for r in p.refs.values() {
            if let Some(n) = self.nodes.get_mut(&r.node) {
                n.refs = n.refs.saturating_sub(1);
                if r.strong > 0 {
                    n.strong_refs = n.strong_refs.saturating_sub(1);
                }
            }
        }
        let mut changed: Vec<u64> = p.refs.values().map(|r| r.node).collect();
        // Its buffers: the counts they held on its own nodes go with them.
        if let Some(area) = p.area.take() {
            for b in area.bufs.into_values() {
                for h in b.held {
                    if let Held::Node(n, strong) = h {
                        if let Some(node) = self.nodes.get_mut(&n) {
                            if strong {
                                node.local_strong = node.local_strong.saturating_sub(1);
                            } else {
                                node.local_weak = node.local_weak.saturating_sub(1);
                            }
                        }
                        changed.push(n);
                    }
                }
            }
            self.areas.push(area.shm);
        }
        changed.extend(nodes);
        for n in changed {
            self.node_changed(n, None);
        }
        // Transactions on their way to it.
        let gone: Vec<u64> = self.txns.iter().filter(|(_, t)| t.to == id).map(|(&x, _)| x).collect();
        for x in gone {
            self.fail_call(x);
        }
    }
}

impl Area {
    /// The offset of `len` free bytes (first fit).
    fn alloc(&self, len: u64) -> Option<u64> {
        let mut at = 0;
        for (&off, b) in &self.bufs {
            if off >= at + len {
                break;
            }
            at = off + b.len;
        }
        (at + len <= self.size).then_some(at)
    }
}

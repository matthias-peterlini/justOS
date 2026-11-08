//! Volumes and paths.
//!
//! justOS has no single root: every mounted file system is a **volume** with
//! a name, and a path names the volume first:
//!
//! ```text
//! sys:/system/drivers/ahci.jdp     the system volume
//! boot:/drivers/ps2kbd.jdp         the boot pack (read-only, in memory)
//! ram:/scratch.txt                 a RAM disk that starts empty at boot
//! ```
//!
//! Names are case-insensitive. Volumes mount by themselves when a disk with
//! a JustFS partition shows up: the first one labelled "System" becomes
//! `sys:`, the others use their label.

use crate::dev::block::{Disk, DiskBlocks};
use crate::sync::{IrqLock, Mutex};
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use justfs::{BlockDevice, DirEntry, Fs, Kind, MemDisk, NodeId};

pub use justfs::Error;

pub type Result<T> = core::result::Result<T, Error>;

struct KernelClock;

impl justfs::Clock for KernelClock {
    fn now(&self) -> u64 {
        crate::time::now()
    }
}

static CLOCK: KernelClock = KernelClock;

pub struct Volume {
    pub name: String,
    pub label: String,
    /// Disk the volume lives on ("memory" for RAM volumes).
    pub source: String,
    fs: Mutex<Fs<Box<dyn BlockDevice + Send>>>,
    /// Changed since it was last flushed (see [`changed`]).
    dirty: AtomicBool,
    /// Only used with `fs` locked.
    names: IrqLock<Names>,
    /// The disk under it (not for volumes in memory).
    disk: Option<Arc<Disk>>,
}

type VolFs = Fs<Box<dyn BlockDevice + Send>>;

/// Paths looked up before, so that most calls on a path do not walk its
/// folders again: JustFS reads the entries of each folder on the way, and
/// programs ask about the same paths over and over. A path found stays right
/// until a name goes away or moves; a path not found until a name is added.
/// Paths whose last link is followed ([`Follow::Yes`]) and paths whose last
/// link is not are kept apart.
#[derive(Default)]
struct Names {
    found: [BTreeMap<String, NodeId>; 2],
    missing: [BTreeSet<String>; 2],
}

/// Paths kept, at most, of each kind.
const NAMES_MAX: usize = 4096;

/// Whether a link at the end of a path is followed.
#[derive(Clone, Copy)]
enum Follow {
    Yes = 0,
    No = 1,
}

/// The node at `path` in `v`, whose file system `fs` is (locked): from the
/// names known, or found and then known. The names lock is not held while
/// the folders are read (the disk may be slow, and that lock keeps
/// interrupts off); the volume's lock keeps the names right meanwhile.
fn find(v: &Volume, fs: &mut VolFs, path: &str, follow: Follow) -> Result<NodeId> {
    if let Some(known) = v.names.lock().get(follow, path) {
        return known;
    }
    let found = match follow {
        Follow::Yes => fs.lookup(path),
        Follow::No => fs.lookup_link(path),
    };
    v.names.lock().put(follow, path, &found);
    found
}

/// The folder `path` is in, and its last name (like `Fs::lookup_parent`,
/// through the names known).
fn find_parent<'p>(v: &Volume, fs: &mut VolFs, path: &'p str) -> Result<(NodeId, &'p str)> {
    let path = path.trim_end_matches('/');
    let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
    justfs::valid_name(name)?;
    Ok((find(v, fs, dir, Follow::Yes)?, name))
}

impl Names {
    fn get(&self, follow: Follow, path: &str) -> Option<Result<NodeId>> {
        let k = follow as usize;
        if let Some(&id) = self.found[k].get(path) {
            return Some(Ok(id));
        }
        self.missing[k].contains(path).then_some(Err(Error::NotFound))
    }

    fn put(&mut self, follow: Follow, path: &str, found: &Result<NodeId>) {
        let k = follow as usize;
        match *found {
            Ok(id) => {
                if self.found[k].len() >= NAMES_MAX {
                    self.found[k].clear();
                }
                self.found[k].insert(String::from(path), id);
            }
            Err(Error::NotFound) => {
                if self.missing[k].len() >= NAMES_MAX {
                    self.missing[k].clear();
                }
                self.missing[k].insert(String::from(path));
            }
            Err(_) => {}
        }
    }

    /// A name was added.
    fn added(&mut self) {
        self.missing.iter_mut().for_each(BTreeSet::clear);
    }

    /// Node `id` lost its name. Paths through a removed link or folder may
    /// lead elsewhere now, so they all go then.
    fn removed(&mut self, id: NodeId, kind: Kind) {
        for found in &mut self.found {
            if kind == Kind::File {
                found.retain(|_, n| *n != id);
            } else {
                found.clear();
            }
        }
    }
}

impl Volume {
    /// Tells volumes apart in a [`Change`].
    fn key(&self) -> usize {
        self as *const Volume as usize
    }

    pub fn read_only(&self) -> bool {
        self.fs.lock().read_only()
    }

    /// (total, free) bytes.
    pub fn space(&self) -> (u64, u64) {
        self.fs.lock().space()
    }
}

static VOLUMES: IrqLock<Vec<Arc<Volume>>> = IrqLock::new(Vec::new());

pub fn volumes() -> Vec<Arc<Volume>> {
    VOLUMES.lock().clone()
}

pub fn volume(name: &str) -> Option<Arc<Volume>> {
    VOLUMES.lock().iter().find(|v| v.name.eq_ignore_ascii_case(name)).cloned()
}

fn mount(name: &str, source: &str, disk: Option<Arc<Disk>>, fs: Fs<Box<dyn BlockDevice + Send>>) -> Arc<Volume> {
    let label = fs.superblock().label.clone();
    let v = Arc::new(Volume {
        name: String::from(name),
        label,
        source: String::from(source),
        fs: Mutex::new(fs),
        dirty: AtomicBool::new(false),
        names: IrqLock::new(Names::default()),
        disk,
    });
    VOLUMES.lock().push(v.clone());
    crate::info!("fs", "mounted {}: ({}, {})", v.name, if v.label.is_empty() { "no label" } else { &v.label }, source);
    v
}

/// Picks a free volume name for a label.
fn name_for(label: &str, fallback: &str) -> String {
    let mut base: String = label
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if base == "system" && volume("sys").is_none() {
        return String::from("sys");
    }
    if base.is_empty() || base == "sys" || base == "boot" || base == "ram" {
        base = String::from(fallback);
    }
    let mut name = base.clone();
    let mut n = 2;
    while volume(&name).is_some() {
        name = alloc::format!("{base}{n}");
        n += 1;
    }
    name
}

/// Mounts the boot pack (a JustFS image the loader put in memory).
pub fn mount_boot_pack(phys: u64, size: u64) {
    if size == 0 {
        crate::warn!("fs", "the boot loader passed no boot pack");
        return;
    }
    // SAFETY: the frame allocator keeps the pack's memory reserved forever.
    let bytes = unsafe { core::slice::from_raw_parts(crate::mem::phys_to_virt(phys) as *const u8, size as usize) };
    let disk = MemDisk { data: bytes.to_vec(), read_only: true };
    match Fs::mount(Box::new(disk) as Box<dyn BlockDevice + Send>, &CLOCK) {
        Ok(fs) => {
            mount("boot", "memory", None, fs);
        }
        Err(e) => crate::error!("fs", "boot pack: {e}"),
    }
}

/// Mounts the system volume the boot loader read from a live CD as `sys:`.
/// It lives in memory: changes last until the computer is turned off.
pub fn mount_live_system(phys: u64, size: u64) {
    // SAFETY: the frame allocator keeps the volume's memory reserved.
    let bytes = unsafe { core::slice::from_raw_parts(crate::mem::phys_to_virt(phys) as *const u8, size as usize) };
    let disk = MemDisk { data: bytes.to_vec(), read_only: false };
    match Fs::mount(Box::new(disk) as Box<dyn BlockDevice + Send>, &CLOCK) {
        Ok(fs) => {
            mount("sys", "memory (live CD)", None, fs);
        }
        Err(e) => crate::error!("fs", "live system volume: {e}"),
    }
}

/// Creates the `ram:` volume.
pub fn mount_ram(bytes: usize) {
    let disk = MemDisk::new(bytes / justfs::BLOCK_SIZE);
    match Fs::format(Box::new(disk) as Box<dyn BlockDevice + Send>, "RAM", &CLOCK) {
        Ok(fs) => {
            mount("ram", "memory", None, fs);
        }
        Err(e) => crate::error!("fs", "RAM disk: {e}"),
    }
}

/// Mounts the disk if it holds JustFS (called for every new disk/partition).
pub fn offer(disk: &Arc<Disk>) {
    if disk.sector_size as usize > justfs::BLOCK_SIZE || is_mounted_on(&disk.name) {
        return;
    }
    let mut block0 = [0u8; justfs::BLOCK_SIZE];
    let mut dev = DiskBlocks::new(disk.clone());
    if dev.read_block(0, &mut block0).is_err() || !Fs::<MemDisk>::probe(&block0) {
        return;
    }
    match Fs::mount(Box::new(dev) as Box<dyn BlockDevice + Send>, &CLOCK) {
        Ok(fs) => {
            let name = name_for(&fs.superblock().label.clone(), &disk.name);
            mount(&name, &disk.name, Some(disk.clone()), fs);
        }
        Err(e) => crate::warn!("fs", "{}: JustFS volume not mounted: {e}", disk.name),
    }
}

pub fn is_mounted_on(disk: &str) -> bool {
    VOLUMES.lock().iter().any(|v| v.source.eq_ignore_ascii_case(disk))
}

/// Unmounts a volume (flushing it first).
pub fn unmount(name: &str) -> Result<()> {
    let v = volume(name).ok_or(Error::NotFound)?;
    v.fs.lock().sync()?;
    VOLUMES.lock().retain(|x| !Arc::ptr_eq(x, &v));
    Ok(())
}

/// The disk under a volume vanished: forget the volume (nothing can be
/// written back any more).
pub fn disk_gone(disk: &str) {
    let gone: Vec<Arc<Volume>> = VOLUMES.lock().iter().filter(|v| v.source.eq_ignore_ascii_case(disk)).cloned().collect();
    for v in gone {
        VOLUMES.lock().retain(|x| !Arc::ptr_eq(x, &v));
        crate::warn!("fs", "{}: removed while mounted; unsaved changes are lost", v.name);
    }
}

/// Formats a disk or partition with JustFS and mounts it.
pub fn format(disk: &Arc<Disk>, label: &str) -> Result<String> {
    if is_mounted_on(&disk.name) {
        return Err(Error::Exists);
    }
    let dev = DiskBlocks::new(disk.clone());
    let fs = Fs::format(Box::new(dev) as Box<dyn BlockDevice + Send>, label, &CLOCK)?;
    let name = name_for(label, &disk.name);
    mount(&name, &disk.name, Some(disk.clone()), fs);
    Ok(name)
}

/// Flushes every volume.
pub fn sync_all() {
    for v in volumes() {
        v.dirty.store(false, Ordering::SeqCst);
        if let Err(e) = v.fs.lock().sync() {
            crate::warn!("fs", "{}: {e}", v.name);
        }
    }
}

/// Notes a change. Its blocks are on the disk already; the superblock and
/// the disk's write cache are flushed within a second by the flusher
/// thread, or at once by [`flush`] (fsync) and [`sync_all`] (shutdown), not
/// after every write: a flush makes the disk write out its cache, and a
/// program like Firefox writes thousands of times.
fn changed(v: &Volume) -> Result<()> {
    static FLUSHER: AtomicBool = AtomicBool::new(false);
    v.dirty.store(true, Ordering::SeqCst);
    if !FLUSHER.swap(true, Ordering::SeqCst) {
        crate::task::spawn("fs-flush", || loop {
            crate::task::sleep_ms(1000);
            for v in volumes() {
                flush_volume(&v);
            }
        });
    }
    Ok(())
}

/// Writes out what changed on the volume and has its disk make it durable.
/// Only the writing holds the volume: a disk takes a while to empty its
/// cache, and programs like Firefox fsync many times a second.
fn flush_volume(v: &Volume) {
    if v.dirty.swap(false, Ordering::SeqCst) {
        let written = v.fs.lock().write_back();
        let flushed = match (&written, &v.disk) {
            (Ok(()), Some(disk)) if disk.flush() != justdk::OK => Err(Error::Io),
            _ => written,
        };
        if let Err(e) = flushed {
            crate::warn!("fs", "{}: {e}", v.name);
        }
    }
}

// ----------------------------------------------------------------------------
// Watching

/// What changed, in the bits of Linux's inotify (its only reader).
pub mod change {
    pub const MODIFY: u32 = 0x2;
    pub const ATTRIB: u32 = 0x4;
    pub const CLOSE_WRITE: u32 = 0x8;
    pub const MOVED_FROM: u32 = 0x40;
    pub const MOVED_TO: u32 = 0x80;
    pub const CREATE: u32 = 0x100;
    pub const DELETE: u32 = 0x200;
    /// Set with the others when the entry is a folder.
    pub const ISDIR: u32 = 0x4000_0000;
}

/// A change to the entry `name` (node `node`) of the folder `dir`.
pub struct Change<'a> {
    /// The volume ([`node_of`] gives the same number).
    pub volume: usize,
    pub dir: NodeId,
    pub name: &'a str,
    pub node: NodeId,
    pub mask: u32,
    /// Ties the two halves of a move together.
    pub cookie: u32,
}

/// Told of every change while anything is watched.
static WATCHER: IrqLock<Option<fn(&Change<'_>)>> = IrqLock::new(None);

/// How many things are watched: changes are described only while some are.
static WATCHED: AtomicUsize = AtomicUsize::new(0);

/// Sets who is told of changes.
pub fn set_watcher(f: fn(&Change<'_>)) {
    *WATCHER.lock() = Some(f);
}

/// One more thing is watched (`true`), or one fewer.
pub fn watching(more: bool) {
    if more {
        WATCHED.fetch_add(1, Ordering::Relaxed);
    } else {
        WATCHED.fetch_sub(1, Ordering::Relaxed);
    }
}

fn watched() -> bool {
    WATCHED.load(Ordering::Relaxed) != 0
}

fn tell(v: &Volume, dir: NodeId, name: &str, node: NodeId, mask: u32, cookie: u32) {
    let watcher = *WATCHER.lock();
    if let Some(f) = watcher {
        f(&Change { volume: v.key(), dir, name, node, mask, cookie });
    }
}

/// Tells of a change to the entry at `path` (node `id`), if anything is
/// watched.
fn tell_path(v: &Volume, fs: &mut VolFs, path: &str, id: NodeId, mask: u32) {
    if watched() {
        if let Ok((dir, name)) = find_parent(v, fs, path) {
            tell(v, dir, name, id, mask, 0);
        }
    }
}

/// The volume (as in [`Change`]), node and kind at `path`.
pub fn node_of(path: &Path, follow: bool) -> Result<(usize, NodeId, Kind)> {
    let v = vol(path)?;
    let mut fs = v.fs.lock();
    let id = find(&v, &mut fs, &path.inner, if follow { Follow::Yes } else { Follow::No })?;
    let kind = fs.node(id)?.kind;
    Ok((v.key(), id, kind))
}

/// Tells watchers of the file at `path` that `mask` happened to it (a
/// program closed it after writing, say).
pub fn touched(path: &Path, mask: u32) {
    if !watched() {
        return;
    }
    let Ok(v) = vol(path) else { return };
    let mut fs = v.fs.lock();
    if let Ok(id) = find(&v, &mut fs, &path.inner, Follow::Yes) {
        tell_path(&v, &mut fs, &path.inner, id, mask);
    }
}

/// Makes what was written to the volume of `path` reach the disk (fsync).
pub fn flush(path: &Path) {
    if let Ok(v) = vol(path) {
        flush_volume(&v);
    }
}

// ----------------------------------------------------------------------------
// Paths

/// A parsed absolute path: volume and the path inside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Path {
    pub volume: String,
    pub inner: String,
}

impl Path {
    /// Parses `vol:/a/b` or a path relative to `cwd` (`a/b`, `../x`, `/a`).
    pub fn parse(text: &str, cwd: &Path) -> Result<Path> {
        let text = text.trim();
        let (volume, rest, relative) = match text.find(':') {
            Some(i) if !text[..i].contains('/') => (String::from(&text[..i]), &text[i + 1..], false),
            _ => (cwd.volume.clone(), text, !text.starts_with('/')),
        };
        if volume.is_empty() {
            return Err(Error::InvalidName);
        }
        let mut parts: Vec<&str> = if relative { cwd.inner.split('/').filter(|p| !p.is_empty()).collect() } else { Vec::new() };
        for p in rest.split('/') {
            match p {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                p => parts.push(p),
            }
        }
        let mut inner = String::new();
        for p in parts {
            inner.push('/');
            inner.push_str(p);
        }
        if inner.is_empty() {
            inner.push('/');
        }
        Ok(Path { volume: volume.to_ascii_lowercase(), inner })
    }

    pub fn root(volume: &str) -> Path {
        Path { volume: String::from(volume), inner: String::from("/") }
    }

    pub fn join(&self, name: &str) -> Path {
        let mut inner = self.inner.clone();
        if !inner.ends_with('/') {
            inner.push('/');
        }
        inner.push_str(name);
        Path { volume: self.volume.clone(), inner }
    }

    /// The last component ("" for a volume root).
    pub fn name(&self) -> &str {
        self.inner.rsplit('/').next().unwrap_or("")
    }

    /// The folder this path is in (the root for a volume root).
    pub fn parent(&self) -> Path {
        let inner = match self.inner.trim_end_matches('/').rfind('/') {
            Some(0) | None => String::from("/"),
            Some(i) => String::from(&self.inner[..i]),
        };
        Path { volume: self.volume.clone(), inner }
    }

    /// True when `self` is `other` or something inside it.
    pub fn within(&self, other: &Path) -> bool {
        self.volume == other.volume
            && (self.inner == other.inner || other.inner == "/" || self.inner.strip_prefix(other.inner.as_str()).is_some_and(|r| r.starts_with('/')))
    }
}

impl core::fmt::Display for Path {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}:{}", self.volume, self.inner)
    }
}

fn vol(path: &Path) -> Result<Arc<Volume>> {
    volume(&path.volume).ok_or(Error::NotFound)
}

/// Parses an absolute path (`vol:/...`).
pub fn abs(text: &str) -> Result<Path> {
    Path::parse(text, &Path::root("sys"))
}

#[derive(Clone, Debug)]
pub struct Stat {
    pub kind: Kind,
    pub size: u64,
    pub modified: u64,
    /// The node on its volume (the same for every path that reaches it).
    pub node: u32,
    /// Marked hidden (the Files app leaves it out).
    pub hidden: bool,
    /// Unix permission bits with [`justfs::MODE_SET`], or 0 if never given.
    pub mode: u16,
}

pub fn stat(path: &Path) -> Result<Stat> {
    let v = vol(path)?;
    let mut fs = v.fs.lock();
    let id = find(&v, &mut fs, &path.inner, Follow::Yes)?;
    let n = fs.node(id)?;
    Ok(Stat { kind: n.kind, size: n.size, modified: n.modified, node: id, hidden: n.flags & justfs::FLAG_HIDDEN != 0, mode: n.mode })
}

/// Sets the Unix permission bits of what `path` names (through links).
pub fn set_mode(path: &Path, mode: u16) -> Result<()> {
    let v = vol(path)?;
    let mut fs = v.fs.lock();
    let id = find(&v, &mut fs, &path.inner, Follow::Yes)?;
    fs.set_mode(id, mode)?;
    tell_path(&v, &mut fs, &path.inner, id, change::ATTRIB);
    changed(&v)
}

/// Like [`stat`], but a symbolic link at the end of the path describes
/// itself (kind [`Kind::Link`]).
pub fn lstat(path: &Path) -> Result<Stat> {
    let v = vol(path)?;
    let mut fs = v.fs.lock();
    let id = find(&v, &mut fs, &path.inner, Follow::No)?;
    let n = fs.node(id)?;
    Ok(Stat { kind: n.kind, size: n.size, modified: n.modified, node: id, hidden: n.flags & justfs::FLAG_HIDDEN != 0, mode: n.mode })
}

/// Creates a symbolic link at `path` that points to `target`.
pub fn symlink(path: &Path, target: &str) -> Result<()> {
    let v = vol(path)?;
    let mut fs = v.fs.lock();
    let id = fs.symlink(&path.inner, target)?;
    v.names.lock().added();
    tell_path(&v, &mut fs, &path.inner, id, change::CREATE);
    changed(&v)
}

/// Where the symbolic link at `path` points.
pub fn read_link(path: &Path) -> Result<String> {
    let v = vol(path)?;
    let mut fs = v.fs.lock();
    let id = find(&v, &mut fs, &path.inner, Follow::No)?;
    fs.read_link(id)
}

pub fn exists(path: &Path) -> bool {
    stat(path).is_ok()
}

pub fn is_dir(path: &Path) -> bool {
    stat(path).is_ok_and(|s| s.kind == Kind::Dir)
}

pub fn read_file(path: &Path) -> Result<Vec<u8>> {
    let v = vol(path)?;
    let mut fs = v.fs.lock();
    let id = find(&v, &mut fs, &path.inner, Follow::Yes)?;
    fs.read_all(id)
}

pub fn write_file(path: &Path, data: &[u8]) -> Result<()> {
    let v = vol(path)?;
    let mut fs = v.fs.lock();
    let known = find(&v, &mut fs, &path.inner, Follow::Yes);
    match known {
        Ok(id) if !fs.node(id)?.is_dir() => {
            fs.write_all(id, data)?;
            tell_path(&v, &mut fs, &path.inner, id, change::MODIFY);
        }
        _ => {
            let id = fs.write_file(&path.inner, data)?;
            v.names.lock().added();
            tell_path(&v, &mut fs, &path.inner, id, change::CREATE);
            if !data.is_empty() {
                tell_path(&v, &mut fs, &path.inner, id, change::MODIFY);
            }
        }
    }
    changed(&v)
}

/// Reads up to `buf.len()` bytes at `offset`.
pub fn read_at(path: &Path, offset: u64, buf: &mut [u8]) -> Result<usize> {
    let v = vol(path)?;
    let mut fs = v.fs.lock();
    let id = find(&v, &mut fs, &path.inner, Follow::Yes)?;
    fs.read(id, offset, buf)
}

/// Writes at `offset`, creating the file if needed.
pub fn write_at(path: &Path, offset: u64, data: &[u8]) -> Result<()> {
    let v = vol(path)?;
    let mut fs = v.fs.lock();
    let known = find(&v, &mut fs, &path.inner, Follow::Yes);
    let id = match known {
        Ok(id) => id,
        Err(Error::NotFound) => {
            let (dir, name) = find_parent(&v, &mut fs, &path.inner)?;
            let id = fs.create(dir, name, Kind::File)?;
            v.names.lock().added();
            if watched() {
                tell(&v, dir, name, id, change::CREATE, 0);
            }
            id
        }
        Err(e) => return Err(e),
    };
    fs.write(id, offset, data)?;
    if !data.is_empty() {
        tell_path(&v, &mut fs, &path.inner, id, change::MODIFY);
    }
    changed(&v)
}

/// Sets a file's size: cuts it, or adds zeros.
pub fn truncate(path: &Path, len: u64) -> Result<()> {
    let v = vol(path)?;
    let mut fs = v.fs.lock();
    let id = find(&v, &mut fs, &path.inner, Follow::Yes)?;
    fs.truncate(id, len)?;
    tell_path(&v, &mut fs, &path.inner, id, change::MODIFY);
    changed(&v)
}

/// Appends to a file, creating it if needed.
pub fn append_file(path: &Path, data: &[u8]) -> Result<()> {
    let v = vol(path)?;
    let mut fs = v.fs.lock();
    let known = find(&v, &mut fs, &path.inner, Follow::Yes);
    let id = match known {
        Ok(id) => id,
        Err(Error::NotFound) => {
            let (dir, name) = find_parent(&v, &mut fs, &path.inner)?;
            let id = fs.create(dir, name, Kind::File)?;
            v.names.lock().added();
            if watched() {
                tell(&v, dir, name, id, change::CREATE, 0);
            }
            id
        }
        Err(e) => return Err(e),
    };
    let size = fs.node(id)?.size;
    fs.write(id, size, data)?;
    if !data.is_empty() {
        tell_path(&v, &mut fs, &path.inner, id, change::MODIFY);
    }
    changed(&v)
}

pub fn list_dir(path: &Path) -> Result<Vec<(DirEntry, Stat)>> {
    let v = vol(path)?;
    let mut fs = v.fs.lock();
    let id = find(&v, &mut fs, &path.inner, Follow::Yes)?;
    let mut out = Vec::new();
    for e in fs.read_dir(id)? {
        // A link is listed as what it points to (a dangling one as a link),
        // hidden if the link itself is.
        let hidden = fs.node(e.node)?.flags & justfs::FLAG_HIDDEN != 0;
        let mut node = e.node;
        if e.kind == Kind::Link {
            let full = alloc::format!("{}/{}", path.inner.trim_end_matches('/'), e.name);
            node = find(&v, &mut fs, &full, Follow::Yes).unwrap_or(e.node);
        }
        let n = fs.node(node)?;
        out.push((e, Stat { kind: n.kind, size: n.size, modified: n.modified, node, hidden, mode: n.mode }));
    }
    out.sort_by(|a, b| (a.1.kind != Kind::Dir).cmp(&(b.1.kind != Kind::Dir)).then(a.0.name.to_lowercase().cmp(&b.0.name.to_lowercase())));
    Ok(out)
}

pub fn make_dir(path: &Path) -> Result<()> {
    let v = vol(path)?;
    let mut fs = v.fs.lock();
    let known = find(&v, &mut fs, &path.inner, Follow::Yes);
    if known.is_ok_and(|id| fs.node(id).is_ok_and(|n| n.is_dir())) {
        return Ok(());
    }
    let id = fs.create_dirs(&path.inner)?;
    v.names.lock().added();
    tell_path(&v, &mut fs, &path.inner, id, change::CREATE | change::ISDIR);
    changed(&v)
}

pub fn remove(path: &Path) -> Result<()> {
    let v = vol(path)?;
    let mut fs = v.fs.lock();
    let (dir, name) = find_parent(&v, &mut fs, &path.inner)?;
    let id = fs.find(dir, name)?;
    let kind = fs.node(id)?.kind;
    fs.remove(dir, name)?;
    v.names.lock().removed(id, kind);
    if watched() {
        tell(&v, dir, name, id, change::DELETE | if kind == Kind::Dir { change::ISDIR } else { 0 }, 0);
    }
    changed(&v)
}

/// Removes a file or a folder with everything in it (a link to a folder
/// goes, the folder stays).
pub fn remove_all(path: &Path) -> Result<()> {
    if lstat(path).is_ok_and(|s| s.kind == Kind::Dir) {
        for (e, _) in list_dir(path)? {
            remove_all(&path.join(&e.name))?;
        }
    }
    remove(path)
}

/// Renames or moves within one volume.
pub fn rename(from: &Path, to: &Path) -> Result<()> {
    if !from.volume.eq_ignore_ascii_case(&to.volume) {
        return Err(Error::InvalidName);
    }
    let v = vol(from)?;
    let mut fs = v.fs.lock();
    let (dir, name) = find_parent(&v, &mut fs, &from.inner)?;
    let (new_dir, new_name) = find_parent(&v, &mut fs, &to.inner)?;
    let id = fs.find(dir, name)?;
    let kind = fs.node(id)?.kind;
    fs.rename(dir, name, new_dir, new_name)?;
    // The old name went away, a new one came.
    let mut names = v.names.lock();
    names.removed(id, kind);
    names.added();
    drop(names);
    if watched() {
        static COOKIE: AtomicU32 = AtomicU32::new(1);
        let cookie = COOKIE.fetch_add(1, Ordering::Relaxed);
        let dir_bit = if kind == Kind::Dir { change::ISDIR } else { 0 };
        tell(&v, dir, name, id, change::MOVED_FROM | dir_bit, cookie);
        tell(&v, new_dir, new_name, id, change::MOVED_TO | dir_bit, cookie);
    }
    changed(&v)
}

/// Copies a file (across volumes too).
pub fn copy_file(from: &Path, to: &Path) -> Result<()> {
    let data = read_file(from)?;
    write_file(to, &data)
}

/// Copies a file or a folder with everything in it (across volumes too).
pub fn copy_all(from: &Path, to: &Path) -> Result<()> {
    if !is_dir(from) {
        return copy_file(from, to);
    }
    make_dir(to)?;
    for (e, _) in list_dir(from)? {
        copy_all(&from.join(&e.name), &to.join(&e.name))?;
    }
    Ok(())
}

/// Runs the consistency check of a volume; returns the problems found.
pub fn check(name: &str) -> Result<usize> {
    let v = volume(name).ok_or(Error::NotFound)?;
    let mut fs = v.fs.lock();
    fs.check()
}

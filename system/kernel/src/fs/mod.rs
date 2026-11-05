//! Volumes and paths.
//!
//! JustOS has no single root: every mounted file system is a **volume** with
//! a name, and a path names the volume first:
//!
//! ```text
//! sys:/system/drivers/ahci.adp     the system volume
//! boot:/drivers/ps2kbd.adp         the boot pack (read-only, in memory)
//! ram:/scratch.txt                 a RAM disk that starts empty at boot
//! ```
//!
//! Names are case-insensitive. Volumes mount by themselves when a disk with
//! a JustFS partition shows up: the first one labelled "System" becomes
//! `sys:`, the others use their label.

use crate::dev::block::{Disk, DiskBlocks};
use crate::sync::{IrqLock, Mutex};
use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use justfs::{BlockDevice, DirEntry, Fs, Kind, MemDisk};

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
}

impl Volume {
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

fn mount(name: &str, source: &str, fs: Fs<Box<dyn BlockDevice + Send>>) -> Arc<Volume> {
    let label = fs.superblock().label.clone();
    let v = Arc::new(Volume { name: String::from(name), label, source: String::from(source), fs: Mutex::new(fs) });
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
            mount("boot", "memory", fs);
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
            mount("sys", "memory (live CD)", fs);
        }
        Err(e) => crate::error!("fs", "live system volume: {e}"),
    }
}

/// Creates the `ram:` volume.
pub fn mount_ram(bytes: usize) {
    let disk = MemDisk::new(bytes / justfs::BLOCK_SIZE);
    match Fs::format(Box::new(disk) as Box<dyn BlockDevice + Send>, "RAM", &CLOCK) {
        Ok(fs) => {
            mount("ram", "memory", fs);
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
    let mut dev = DiskBlocks { disk: disk.clone() };
    if dev.read_block(0, &mut block0).is_err() || !Fs::<MemDisk>::probe(&block0) {
        return;
    }
    match Fs::mount(Box::new(dev) as Box<dyn BlockDevice + Send>, &CLOCK) {
        Ok(fs) => {
            let name = name_for(&fs.superblock().label.clone(), &disk.name);
            mount(&name, &disk.name, fs);
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
    let dev = DiskBlocks { disk: disk.clone() };
    let fs = Fs::format(Box::new(dev) as Box<dyn BlockDevice + Send>, label, &CLOCK)?;
    let name = name_for(label, &disk.name);
    mount(&name, &disk.name, fs);
    Ok(name)
}

/// Flushes every volume.
pub fn sync_all() {
    for v in volumes() {
        if let Err(e) = v.fs.lock().sync() {
            crate::warn!("fs", "{}: {e}", v.name);
        }
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
}

pub fn stat(path: &Path) -> Result<Stat> {
    let v = vol(path)?;
    let mut fs = v.fs.lock();
    let id = fs.lookup(&path.inner)?;
    let n = fs.node(id)?;
    Ok(Stat { kind: n.kind, size: n.size, modified: n.modified })
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
    fs.read_file(&path.inner)
}

pub fn write_file(path: &Path, data: &[u8]) -> Result<()> {
    let v = vol(path)?;
    let mut fs = v.fs.lock();
    fs.write_file(&path.inner, data)?;
    fs.sync()
}

/// Reads up to `buf.len()` bytes at `offset`.
pub fn read_at(path: &Path, offset: u64, buf: &mut [u8]) -> Result<usize> {
    let v = vol(path)?;
    let mut fs = v.fs.lock();
    let id = fs.lookup(&path.inner)?;
    fs.read(id, offset, buf)
}

/// Writes at `offset`, creating the file if needed.
pub fn write_at(path: &Path, offset: u64, data: &[u8]) -> Result<()> {
    let v = vol(path)?;
    let mut fs = v.fs.lock();
    let id = match fs.lookup(&path.inner) {
        Ok(id) => id,
        Err(Error::NotFound) => {
            let (dir, name) = fs.lookup_parent(&path.inner)?;
            fs.create(dir, name, Kind::File)?
        }
        Err(e) => return Err(e),
    };
    fs.write(id, offset, data)?;
    fs.sync()
}

/// Appends to a file, creating it if needed.
pub fn append_file(path: &Path, data: &[u8]) -> Result<()> {
    let v = vol(path)?;
    let mut fs = v.fs.lock();
    let id = match fs.lookup(&path.inner) {
        Ok(id) => id,
        Err(Error::NotFound) => {
            let (dir, name) = fs.lookup_parent(&path.inner)?;
            fs.create(dir, name, Kind::File)?
        }
        Err(e) => return Err(e),
    };
    let size = fs.node(id)?.size;
    fs.write(id, size, data)?;
    fs.sync()
}

pub fn list_dir(path: &Path) -> Result<Vec<(DirEntry, Stat)>> {
    let v = vol(path)?;
    let mut fs = v.fs.lock();
    let id = fs.lookup(&path.inner)?;
    let mut out = Vec::new();
    for e in fs.read_dir(id)? {
        let n = fs.node(e.node)?;
        out.push((e, Stat { kind: n.kind, size: n.size, modified: n.modified }));
    }
    out.sort_by(|a, b| (a.0.kind != Kind::Dir).cmp(&(b.0.kind != Kind::Dir)).then(a.0.name.to_lowercase().cmp(&b.0.name.to_lowercase())));
    Ok(out)
}

pub fn make_dir(path: &Path) -> Result<()> {
    let v = vol(path)?;
    let mut fs = v.fs.lock();
    fs.create_dirs(&path.inner)?;
    fs.sync()
}

pub fn remove(path: &Path) -> Result<()> {
    let v = vol(path)?;
    let mut fs = v.fs.lock();
    let (dir, name) = fs.lookup_parent(&path.inner)?;
    fs.remove(dir, name)?;
    fs.sync()
}

/// Removes a file or a folder with everything in it.
pub fn remove_all(path: &Path) -> Result<()> {
    if is_dir(path) {
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
    let (dir, name) = fs.lookup_parent(&from.inner)?;
    let (new_dir, new_name) = fs.lookup_parent(&to.inner)?;
    fs.rename(dir, name, new_dir, new_name)?;
    fs.sync()
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

//! Memory of a Linux process: the program break and the areas `mmap`
//! reserves. Pages are given out when first touched (demand paging): a
//! reservation costs nothing until it is used, as runtimes such as Go's
//! expect when they set aside gigabytes of address space. Shared mappings
//! of shared memory ([`super::shm`]: memfd, /dev/shm, MAP_SHARED |
//! MAP_ANONYMOUS) map its frames, the same ones in every process.
//!
//! A mapped file is read from the disk only where it is touched, a few
//! pages at a time, into frames that every mapping of it shares
//! ([`FileData`]): read-only until a mapping writes to a page, which then
//! gets a copy of its own (copy on write). A browser maps a 160 MB library
//! in each of its processes and uses a part of it; reading it all at the
//! first `mmap` took a minute.
//!
//! A shared mapping that may write to a file maps the frames of a
//! [`SharedFile`], the same in every process; they go back to the file.

use super::shm::Shm;
use super::{err, with_state, LinuxState};
use crate::arch::cpu;
use crate::mem::{self, frame, paging, phys_to_virt, PAGE_SIZE};
use crate::user;
use crate::fs::Path;
use crate::sync::IrqLock;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use crate::user::USER_TOP;

pub const PROT_READ: u32 = 1;
pub const PROT_WRITE: u32 = 2;

const MAP_SHARED: u32 = 0x01;
const MAP_FIXED: u32 = 0x10;
const MAP_ANONYMOUS: u32 = 0x20;
const MAP_FIXED_NOREPLACE: u32 = 0x10_0000;

/// Where `mmap` looks for room when the program does not say.
pub const MMAP_BASE: u64 = 0x2000_0000_0000;
const MMAP_TOP: u64 = 0x6000_0000_0000;
/// The main thread's stack may grow down to this size.
pub const STACK_RESERVE: u64 = 8 * 1024 * 1024;

/// What fills an area's pages.
#[derive(Clone)]
pub enum Backing {
    /// Zeroes.
    Anon,
    /// A file (private: what the mapping writes stays its own).
    File(Arc<FileData>),
    /// Shared memory: the same frames for every mapping (true), or a
    /// private copy of each page made when it is first touched.
    Shm(Arc<Shm>, bool),
    /// A file mapped MAP_SHARED: its frames, the same for every mapping.
    SharedFile(Arc<SharedFile>),
}

/// A reserved area.
#[derive(Clone)]
pub struct Vma {
    pub end: u64,
    pub prot: u32,
    pub backing: Backing,
    /// Where the area's start is in the backing.
    pub off: u64,
}

impl Vma {
    pub fn anon(end: u64, prot: u32) -> Vma {
        Vma { end, prot, backing: Backing::Anon, off: 0 }
    }
}

/// Is `[start, end)` free of areas?
pub fn free_range(st: &LinuxState, start: u64, end: u64) -> bool {
    // The program's own segments and its heap are taken too.
    let heap_end = mem::align_up(st.brk_cur, PAGE_SIZE);
    let heap = heap_end > st.brk_start && st.brk_start < end && heap_end > start;
    !heap && !st.images.iter().any(|i| i.start < end && i.end > start) && !st.vmas.range(..end).any(|(&s, v)| s < end && v.end > start)
}

/// Removes `[start, end)` from the areas, splitting those that straddle it.
/// Returns the areas it touched: the caller keeps them until the pages are
/// unmapped, so that shared memory is not freed while still mapped.
fn cut(st: &mut LinuxState, start: u64, end: u64) -> Vec<Vma> {
    let hit: Vec<(u64, Vma)> = st.vmas.range(..end).filter(|(&s, v)| s < end && v.end > start).map(|(&s, v)| (s, v.clone())).collect();
    let mut gone = Vec::new();
    for (s, v) in hit {
        st.vmas.remove(&s);
        if s < start {
            let mut left = v.clone();
            left.end = start;
            st.vmas.insert(s, left);
        }
        if v.end > end {
            let mut right = v.clone();
            right.off = v.off + (end - s);
            st.vmas.insert(end, right);
        }
        gone.push(v);
    }
    gone
}

/// Frees the pages of a range that were handed out.
fn release_pages(start: u64, end: u64) {
    let cr3 = cpu::read_cr3();
    let mut freed = Vec::new();
    let mut any = false;
    let mut p = start;
    while p < end {
        if paging::user_accessible(cr3, p) {
            any = true;
            if let Some(phys) = paging::unmap_owned_in(cr3, p) {
                freed.push(phys);
            }
        }
        p += PAGE_SIZE;
    }
    // Threads on other processors may still reach them through their TLBs.
    if any {
        crate::arch::smp::shootdown_user(cr3);
    }
    for phys in freed {
        frame::free(phys);
    }
}

/// Finds `len` free bytes at or above `from`.
fn find_gap(st: &LinuxState, from: u64, len: u64) -> Option<u64> {
    let mut at = mem::align_up(from, PAGE_SIZE);
    loop {
        if at.checked_add(len)? > MMAP_TOP {
            return None;
        }
        // The furthest end of the areas in the way, if any.
        let blocker = st.vmas.range(..at + len).filter(|(_, v)| v.end > at).map(|(_, v)| v.end).max();
        match blocker {
            None => return Some(at),
            Some(e) => at = mem::align_up(e, PAGE_SIZE),
        }
    }
}

/// Files that are mapped somewhere: the dynamic loader maps each library
/// four or five times, and every process maps the same libc, so they share
/// one [`FileData`] (by path, size and change time) while any mapping of
/// it lives.
static MAPPED: IrqLock<Vec<Weak<FileData>>> = IrqLock::new(Vec::new());

/// How many pages a fault reads from a file at once: the page and the ones
/// after it (programs mostly go forward).
const READ_AHEAD: u64 = 32;

/// A mapped file: the pages read so far, each in a frame of its own that
/// the mappings map read-only (marked [`paging::SHARED`], so unmapping or
/// ending a process never frees it). They go with the last mapping.
pub struct FileData {
    path: Path,
    key: String,
    /// The file as the program named it (/proc/self/maps shows it).
    name: String,
    size: u64,
    modified: u64,
    /// Frames by page number.
    pages: IrqLock<BTreeMap<u64, u64>>,
}

impl FileData {
    /// The frame holding page `n` of the file (zeroes past its end), read
    /// along with the pages after it that are not here yet.
    fn frame(&self, n: u64) -> Option<u64> {
        if let Some(&f) = self.pages.lock().get(&n) {
            return Some(f);
        }
        let last = self.size.div_ceil(PAGE_SIZE);
        let count = if n >= last {
            1
        } else {
            let p = self.pages.lock();
            1 + (1..(last - n).min(READ_AHEAD)).take_while(|i| !p.contains_key(&(n + i))).count() as u64
        };
        // Read without holding a lock: the disk may take a while, and
        // another thread may read the same pages meanwhile (the first one
        // in keeps its frames).
        let mut buf = alloc::vec![0u8; (count * PAGE_SIZE) as usize];
        let mut done = 0;
        while n < last && done < buf.len() {
            match crate::fs::read_at(&self.path, n * PAGE_SIZE + done as u64, &mut buf[done..]) {
                Ok(0) | Err(_) => break,
                Ok(k) => done += k,
            }
        }
        let mut p = self.pages.lock();
        for i in 0..count {
            if p.contains_key(&(n + i)) {
                continue;
            }
            let Some(f) = frame::alloc() else { break };
            // SAFETY: a frame just allocated, reached through the direct map.
            unsafe { core::ptr::copy_nonoverlapping(buf.as_ptr().add((i * PAGE_SIZE) as usize), phys_to_virt(f) as *mut u8, PAGE_SIZE as usize) };
            p.insert(n + i, f);
        }
        p.get(&n).copied()
    }

    /// Reads every page not read yet (before the file changes: mappings
    /// made before keep what it was).
    fn load_all(&self) {
        let mut n = 0;
        while n < self.size.div_ceil(PAGE_SIZE) {
            if self.frame(n).is_none() {
                return;
            }
            n += 1;
        }
    }
}

impl Drop for FileData {
    fn drop(&mut self) {
        for &f in self.pages.lock().values() {
            frame::free(f);
        }
    }
}

/// Files mapped MAP_SHARED by a mapping that may write, see [`SharedFile`].
/// One stays here after its last mapping goes, until [`sweep_shared`]
/// writes it back.
static SHARED_FILES: IrqLock<Vec<Arc<SharedFile>>> = IrqLock::new(Vec::new());
/// How many there are: reads and writes of files only look when some are.
static SHARED_COUNT: AtomicUsize = AtomicUsize::new(0);

/// A file mapped MAP_SHARED with write access: its pages in frames that
/// every shared mapping of it maps (writable where the mapping may write),
/// so that processes see each other's stores, as Android's property areas
/// and SQLite's -shm files expect. While it is here, read() and write() of
/// the file go through its frames; they go back to the file at msync and
/// fsync, and when no mapping is left.
pub struct SharedFile {
    path: Path,
    key: String,
    name: String,
    /// Frames by page number.
    pages: IrqLock<BTreeMap<u64, u64>>,
    /// Bumped by every write() of the file: a page read from the disk
    /// meanwhile is read again.
    writes: AtomicU64,
    /// A mapping may write: the pages go back to the file.
    written: AtomicBool,
    /// The file was removed or renamed: nothing goes back.
    detached: AtomicBool,
}

impl SharedFile {
    /// The frame of page `n` (zeroes past the end of the file).
    fn frame(&self, n: u64) -> Option<u64> {
        loop {
            if let Some(&f) = self.pages.lock().get(&n) {
                return Some(f);
            }
            let gen = self.writes.load(Ordering::Acquire);
            let size = crate::fs::stat(&self.path).map(|s| s.size).unwrap_or(0);
            let want = size.saturating_sub(n * PAGE_SIZE).min(PAGE_SIZE) as usize;
            let mut buf = alloc::vec![0u8; PAGE_SIZE as usize];
            let mut done = 0;
            while done < want {
                match crate::fs::read_at(&self.path, n * PAGE_SIZE + done as u64, &mut buf[done..want]) {
                    Ok(0) | Err(_) => break,
                    Ok(k) => done += k,
                }
            }
            let f = frame::alloc()?;
            // SAFETY: a frame just allocated, reached through the direct map.
            unsafe { core::ptr::copy_nonoverlapping(buf.as_ptr(), phys_to_virt(f) as *mut u8, PAGE_SIZE as usize) };
            let mut p = self.pages.lock();
            // Another thread was first, or the file was written meanwhile
            // (then read it again).
            if p.contains_key(&n) || self.writes.load(Ordering::Acquire) != gen {
                let have = p.get(&n).copied();
                drop(p);
                frame::free(f);
                match have {
                    Some(have) => return Some(have),
                    None => continue,
                }
            }
            p.insert(n, f);
            return Some(f);
        }
    }

    /// Writes the pages back to the file (up to its size).
    fn write_back(&self) {
        if !self.written.load(Ordering::Relaxed) || self.detached.load(Ordering::Relaxed) {
            return;
        }
        let Ok(st) = crate::fs::stat(&self.path) else { return };
        let pages: Vec<(u64, u64)> = self.pages.lock().iter().map(|(&n, &f)| (n, f)).filter(|&(n, _)| n * PAGE_SIZE < st.size).collect();
        if pages.is_empty() {
            return;
        }
        forget_file(&self.path);
        // One write for each run of pages.
        let mut i = 0;
        while i < pages.len() {
            let first = pages[i].0;
            let mut buf = Vec::new();
            let mut next = first;
            while i < pages.len() && pages[i].0 == next && buf.len() < 1 << 20 {
                let (n, f) = pages[i];
                let len = (st.size - n * PAGE_SIZE).min(PAGE_SIZE) as usize;
                // SAFETY: the file's frame, through the direct map.
                buf.extend_from_slice(unsafe { core::slice::from_raw_parts(phys_to_virt(f) as *const u8, len) });
                i += 1;
                next += 1;
            }
            let _ = crate::fs::write_at(&self.path, first * PAGE_SIZE, &buf);
        }
    }

    /// Calls `f` with each frame there is in `[off, off + len)`: where the
    /// bytes are in the frame, how many, and where they are in the range.
    fn each(&self, off: u64, len: usize, mut f: impl FnMut(*mut u8, usize, usize)) {
        let end = off + len as u64;
        let p = self.pages.lock();
        for (&n, &frame) in p.range(off / PAGE_SIZE..end.div_ceil(PAGE_SIZE)) {
            let (a, b) = ((n * PAGE_SIZE).max(off), ((n + 1) * PAGE_SIZE).min(end));
            // SAFETY: the file's frame, through the direct map.
            let at = unsafe { (phys_to_virt(frame) as *mut u8).add((a - n * PAGE_SIZE) as usize) };
            f(at, (b - a) as usize, (a - off) as usize);
        }
    }
}

impl Drop for SharedFile {
    fn drop(&mut self) {
        for &f in self.pages.lock().values() {
            frame::free(f);
        }
    }
}

/// The shared file open as `fd`, for a MAP_SHARED mapping: the one already
/// here, or (for a mapping that may write) a new one. None: map the file
/// as private mappings do.
fn shared_file(fd: i32, write: bool) -> Result<Option<Arc<SharedFile>>, i64> {
    let path = super::fd::file_path(fd).ok_or(-err::EBADF)?;
    let key = map_key(&path);
    let mut all = SHARED_FILES.lock();
    if let Some(f) = all.iter().find(|f| f.key == key) {
        if write {
            f.written.store(true, Ordering::Relaxed);
        }
        return Ok(Some(f.clone()));
    }
    if !write {
        return Ok(None);
    }
    let name = super::show_path(&path);
    let f = Arc::new(SharedFile {
        path,
        key,
        name,
        pages: IrqLock::new(BTreeMap::new()),
        writes: AtomicU64::new(0),
        written: AtomicBool::new(true),
        detached: AtomicBool::new(false),
    });
    all.push(f.clone());
    SHARED_COUNT.fetch_add(1, Ordering::Relaxed);
    Ok(Some(f))
}

fn shared_of(path: &Path) -> Option<Arc<SharedFile>> {
    if SHARED_COUNT.load(Ordering::Relaxed) == 0 {
        return None;
    }
    let key = map_key(path);
    SHARED_FILES.lock().iter().find(|f| f.key == key).cloned()
}

/// `data` was written to the file at `off`: the shared pages get it too.
pub fn file_written(path: &Path, off: u64, data: &[u8]) {
    let Some(f) = shared_of(path) else { return };
    f.each(off, data.len(), |at, n, from| {
        // SAFETY: `at` is `n` bytes of the file's frame.
        unsafe { core::ptr::copy_nonoverlapping(data.as_ptr().add(from), at, n) };
    });
    f.writes.fetch_add(1, Ordering::Release);
}

/// `buf` was read from the file at `off`: what the shared pages hold is
/// newer.
pub fn file_read(path: &Path, off: u64, buf: &mut [u8]) {
    let Some(f) = shared_of(path) else { return };
    f.each(off, buf.len(), |at, n, from| {
        // SAFETY: `at` is `n` bytes of the file's frame.
        unsafe { core::ptr::copy_nonoverlapping(at as *const u8, buf.as_mut_ptr().add(from), n) };
    });
}

/// The file was cut to `len` bytes: what was past it reads as zeroes.
pub fn file_truncated(path: &Path, len: u64) {
    let Some(f) = shared_of(path) else { return };
    let end = f.pages.lock().keys().next_back().map_or(0, |n| (n + 1) * PAGE_SIZE);
    if end > len {
        f.each(len, (end - len) as usize, |at, n, _| {
            // SAFETY: `at` is `n` bytes of the file's frame.
            unsafe { core::ptr::write_bytes(at, 0, n) };
        });
    }
    f.writes.fetch_add(1, Ordering::Release);
}

/// The file was removed or renamed: its mappings keep their memory, new
/// mappings of the name map the new file.
pub fn file_gone(path: &Path) {
    let Some(f) = shared_of(path) else { return };
    f.detached.store(true, Ordering::Relaxed);
    let mut all = SHARED_FILES.lock();
    let before = all.len();
    all.retain(|g| !Arc::ptr_eq(g, &f));
    SHARED_COUNT.fetch_sub(before - all.len(), Ordering::Relaxed);
}

/// fsync: the shared pages of the file go back to it.
pub fn sync_file(path: &Path) {
    if let Some(f) = shared_of(path) {
        f.write_back();
    }
}

/// Writes back the shared files no mapping holds any more, and lets them
/// go (called where mappings end, outside any lock).
pub fn sweep_shared() {
    if SHARED_COUNT.load(Ordering::Relaxed) == 0 {
        return;
    }
    let idle: Vec<Arc<SharedFile>> = SHARED_FILES.lock().iter().filter(|f| Arc::strong_count(f) == 1).cloned().collect();
    if idle.is_empty() {
        return;
    }
    for f in &idle {
        f.write_back();
    }
    // Those still unused (only here and in `idle`) go; one mapped again
    // meanwhile stays.
    let mut all = SHARED_FILES.lock();
    let before = all.len();
    all.retain(|f| !(Arc::strong_count(f) == 2 && idle.iter().any(|g| Arc::ptr_eq(f, g))));
    SHARED_COUNT.fetch_sub(before - all.len(), Ordering::Relaxed);
    drop(all);
    drop(idle);
}

/// msync: the shared files mapped in the range go back to their files.
pub fn sys_msync(addr: u64, len: u64) -> i64 {
    if !addr.is_multiple_of(PAGE_SIZE) {
        return -err::EINVAL;
    }
    let end = addr.saturating_add(len);
    let files: Vec<Arc<SharedFile>> = with_state(|st| {
        st.vmas
            .range(..end)
            .filter(|&(&s, v)| s < end && v.end > addr)
            .filter_map(|(_, v)| match &v.backing {
                Backing::SharedFile(f) => Some(f.clone()),
                _ => None,
            })
            .collect()
    });
    for f in files {
        f.write_back();
    }
    0
}

/// Whether `addr` is in a shared mapping (of shared memory or of a file),
/// where a futex is the same one for every process that maps it.
pub fn shared_at(addr: u64) -> bool {
    super::try_state(|st| {
        st.vmas.range(..=addr).next_back().is_some_and(|(_, v)| addr < v.end && matches!(v.backing, Backing::Shm(_, true) | Backing::SharedFile(_)))
    })
    .unwrap_or(false)
}

/// An area the loader placed (see `LinuxState::images`).
#[derive(Clone)]
pub struct Image {
    pub start: u64,
    pub end: u64,
    pub prot: u32,
    pub off: u64,
    pub name: Arc<str>,
}

fn map_key(path: &Path) -> String {
    alloc::format!("{path}").to_ascii_lowercase()
}

/// The file open as `fd`, for a mapping (shared between mappings of it).
fn mapped_file(fd: i32) -> Result<Arc<FileData>, i64> {
    let path = super::fd::file_path(fd).ok_or(-err::EBADF)?;
    let st = crate::fs::stat(&path).map_err(|e| -super::errno(user::fs_error(e)))?;
    let key = map_key(&path);
    let mut cache = MAPPED.lock();
    cache.retain(|m| m.strong_count() > 0);
    if let Some(data) = cache.iter().filter_map(Weak::upgrade).find(|d| d.key == key && d.size == st.size && d.modified == st.modified) {
        return Ok(data);
    }
    let name = super::show_path(&path);
    let data = Arc::new(FileData { path, key, name, size: st.size, modified: st.modified, pages: IrqLock::new(BTreeMap::new()) });
    cache.push(Arc::downgrade(&data));
    Ok(data)
}

/// A file that is about to change, be replaced or go: the mappings of it
/// made so far keep it as it is now (as private mappings do), new ones
/// read the new one.
pub fn forget_file(path: &Path) {
    let key = map_key(path);
    let gone: Vec<Arc<FileData>> = {
        let mut cache = MAPPED.lock();
        let gone = cache.iter().filter_map(Weak::upgrade).filter(|d| d.key == key).collect();
        cache.retain(|m| m.upgrade().is_some_and(|d| d.key != key));
        gone
    };
    for d in gone {
        d.load_all();
    }
}

pub fn sys_mmap(addr: u64, len: u64, prot: u32, flags: u32, fd: i32, off: u64) -> i64 {
    if len == 0 || len > 1 << 40 || off % PAGE_SIZE != 0 {
        return -err::EINVAL;
    }
    let len = mem::align_up(len, PAGE_SIZE);
    let shared = flags & MAP_SHARED != 0;
    let (backing, off) = if flags & MAP_ANONYMOUS != 0 {
        (if shared { Backing::Shm(super::shm::anonymous(len), true) } else { Backing::Anon }, 0)
    } else {
        let Some(d) = super::fd::get(fd) else { return -err::EBADF };
        let memfd = match &d.kind {
            super::fd::Kind::Memfd { shm, .. } => Some(shm.clone()),
            super::fd::Kind::Binder(b) => {
                // The area where transactions arrive: the driver writes,
                // the process reads.
                let shm = match super::binder::area(b, len, prot) {
                    Ok(s) => s,
                    Err(e) => return e,
                };
                let at = map(addr, len, prot, flags, Backing::Shm(shm, true), 0);
                if at >= 0 {
                    super::binder::mapped(b, at as u64);
                }
                return at;
            }
            super::fd::Kind::Ashmem(a) => match a.map(len, prot) {
                Ok(shm) if shared => Some(shm),
                // A private mapping of a region is zeroes of its own.
                Ok(_) => return map(addr, len, prot, flags, Backing::Anon, 0),
                Err(e) => return e,
            },
            _ => None,
        };
        let file = if memfd.is_none() && shared {
            match shared_file(fd, prot & PROT_WRITE != 0) {
                Ok(f) => f,
                Err(e) => return e,
            }
        } else {
            None
        };
        match (memfd, file) {
            (Some(shm), _) => (Backing::Shm(shm, shared), off),
            (None, Some(file)) => (Backing::SharedFile(file), off),
            // A read-only shared mapping of a file that nothing maps to
            // write is as a private one (it cannot write).
            (None, None) => match mapped_file(fd) {
                Ok(d) => (Backing::File(d), off),
                Err(e) => return e,
            },
        }
    };
    map(addr, len, prot, flags, backing, off)
}

/// Reserves an area of `len` bytes (a multiple of the page size).
fn map(addr: u64, len: u64, prot: u32, flags: u32, backing: Backing, off: u64) -> i64 {
    let fixed = flags & (MAP_FIXED | MAP_FIXED_NOREPLACE) != 0;
    let mut old = Vec::new();
    let r = with_state(|st| {
        let at = if fixed {
            if addr % PAGE_SIZE != 0 || addr == 0 || addr + len > USER_TOP {
                return Err(-err::EINVAL);
            }
            if flags & MAP_FIXED_NOREPLACE != 0 && !free_range(st, addr, addr + len) {
                return Err(-err::EEXIST);
            }
            old = cut(st, addr, addr + len);
            addr
        } else if addr != 0 && addr % PAGE_SIZE == 0 && addr + len <= USER_TOP && free_range(st, addr, addr + len) {
            addr
        } else {
            let at = find_gap(st, st.mmap_next, len).or_else(|| find_gap(st, MMAP_BASE, len)).ok_or(-err::ENOMEM)?;
            st.mmap_next = at + len;
            at
        };
        st.vmas.insert(at, Vma { end: at + len, prot, backing, off });
        Ok(at)
    });
    match r {
        Ok(at) => {
            if fixed {
                // Whatever was mapped there before is gone.
                release_pages(at, at + len);
            }
            drop(old);
            sweep_shared();
            at as i64
        }
        Err(e) => e,
    }
}

pub fn sys_munmap(addr: u64, len: u64) -> i64 {
    if addr % PAGE_SIZE != 0 || len == 0 {
        return -err::EINVAL;
    }
    let end = addr.saturating_add(mem::align_up(len, PAGE_SIZE)).min(USER_TOP);
    let old = with_state(|st| cut(st, addr, end));
    release_pages(addr, end);
    drop(old);
    sweep_shared();
    0
}

pub fn sys_mprotect(addr: u64, len: u64, prot: u32) -> i64 {
    if addr % PAGE_SIZE != 0 {
        return -err::EINVAL;
    }
    let end = addr.saturating_add(mem::align_up(len.max(1), PAGE_SIZE));
    with_state(|st| {
        let parts: Vec<(u64, Vma)> = st.vmas.range(..end).filter(|(&s, v)| s < end && v.end > addr).map(|(&s, v)| (s, v.clone())).collect();
        for (s, v) in parts {
            if let (Backing::SharedFile(f), true) = (&v.backing, prot & PROT_WRITE != 0) {
                f.written.store(true, Ordering::Relaxed);
            }
            let (a, b) = (s.max(addr), v.end.min(end));
            cut(st, a, b);
            st.vmas.insert(a, Vma { end: b, prot, backing: v.backing.clone(), off: v.off + (a - s) });
        }
    });
    0
}

/// mremap: grows an area in place, or moves it: shared memory moves as it
/// is, other memory by copying the pages that were touched.
pub fn sys_mremap(old: u64, old_len: u64, new_len: u64, flags: u32) -> i64 {
    const MREMAP_MAYMOVE: u32 = 1;
    let old_len = mem::align_up(old_len, PAGE_SIZE);
    let new_len = mem::align_up(new_len, PAGE_SIZE);
    if new_len <= old_len {
        sys_munmap(old + new_len, old_len - new_len);
        return old as i64;
    }
    // Grow in place when the space after it is free.
    let grown = with_state(|st| {
        if free_range(st, old + old_len, old + new_len) {
            if let Some((_, v)) = st.vmas.range_mut(..old + old_len).next_back() {
                if v.end == old + old_len {
                    v.end = old + new_len;
                    return true;
                }
            }
        }
        false
    });
    if grown {
        return old as i64;
    }
    if flags & MREMAP_MAYMOVE == 0 {
        return -err::ENOMEM;
    }
    let area = with_state(|st| st.vmas.range(..=old).next_back().map(|(&s, v)| (v.prot, v.backing.clone(), v.off + (old - s))));
    let (prot, backing, off) = area.unwrap_or((PROT_READ | PROT_WRITE, Backing::Anon, 0));
    let shared = matches!(backing, Backing::Shm(_, true) | Backing::SharedFile(_));
    let new = map(0, new_len, prot, 0, backing, off);
    if new < 0 {
        return new;
    }
    // Copy only the pages that exist; the rest come from the backing.
    let cr3 = cpu::read_cr3();
    let mut p = 0;
    while p < old_len && !shared {
        if paging::user_accessible(cr3, old + p) {
            if fault(new as u64 + p, true) {
                // SAFETY: both pages are mapped user memory of this process.
                unsafe { core::ptr::copy_nonoverlapping((old + p) as *const u8, (new as u64 + p) as *mut u8, PAGE_SIZE as usize) };
            }
        }
        p += PAGE_SIZE;
    }
    sys_munmap(old, old_len);
    new
}

/// madvise: MADV_DONTNEED gives pages back; inside reserved areas they
/// come back as zeroes (or the file's bytes) when next touched. MADV_FREE
/// only allows that, for when memory is short: the pages stay (allocators
/// call it all the time, and giving pages back costs a TLB flush on every
/// processor running the program).
pub fn sys_madvise(addr: u64, len: u64, advice: u64) -> i64 {
    const MADV_DONTNEED: u64 = 4;
    if advice != MADV_DONTNEED {
        return 0;
    }
    if addr % PAGE_SIZE != 0 {
        return -err::EINVAL;
    }
    let end = addr.saturating_add(mem::align_up(len, PAGE_SIZE)).min(USER_TOP);
    let ranges: Vec<(u64, u64)> = with_state(|st| st.vmas.range(..end).filter(|(&s, v)| s < end && v.end > addr).map(|(&s, v)| (s.max(addr), v.end.min(end))).collect());
    for (a, b) in ranges {
        release_pages(a, b);
    }
    0
}

/// A page fault at `addr`: gives the page if an area allows it. Returns
/// whether the access may be retried.
pub fn fault(addr: u64, write: bool) -> bool {
    let page = addr & !(PAGE_SIZE - 1);
    let found = super::try_state(|st| {
        let (&s, v) = st.vmas.range(..=page).next_back()?;
        (page < v.end && v.prot != 0).then(|| (s, v.clone()))
    })
    .flatten();
    let Some((start, vma)) = found else { return false };
    let cr3 = cpu::read_cr3();
    if let Some(e) = paging::entry_in(cr3, page) {
        if let (Backing::SharedFile(_), true) = (&vma.backing, write && e & paging::WRITE == 0 && vma.prot & PROT_WRITE != 0) {
            // Mapped before mprotect let it write: the same frame, writable.
            return paging::replace_in(cr3, page, e, e & paging::ADDR_MASK, paging::USER | paging::SHARED | paging::WRITE) || paging::user_accessible(cr3, page);
        }
        if write && e & paging::WRITE == 0 {
            return copy_on_write(page);
        }
        return paging::user_accessible(cr3, page);
    }
    if paging::translate_in(cr3, page).is_some() {
        return paging::user_accessible(cr3, page);
    }
    let at = vma.off + (page - start);
    // A frame to map as it is (shared memory, or a file's page to read),
    // or what this mapping's own page starts as: a copy of a frame or
    // zeroes.
    let (shared, copy_of) = match &vma.backing {
        Backing::Anon => (None, None),
        Backing::Shm(shm, true) => match shm.frame(at / PAGE_SIZE) {
            Some(f) => (Some((f, paging::WRITE)), None),
            None => return false,
        },
        Backing::Shm(shm, false) => (None, shm.existing(at / PAGE_SIZE)),
        Backing::SharedFile(file) => match file.frame(at / PAGE_SIZE) {
            Some(f) => (Some((f, if vma.prot & PROT_WRITE != 0 { paging::WRITE } else { 0 })), None),
            None => return false,
        },
        Backing::File(file) => match file.frame(at / PAGE_SIZE) {
            Some(f) if write => (None, Some(f)),
            Some(f) => (Some((f, 0)), None),
            None => return false,
        },
    };
    if let Some((f, write)) = shared {
        return paging::map_vacant_in(cr3, page, f, paging::USER | paging::SHARED | write).is_some();
    }
    let Some(phys) = frame::alloc() else { return false };
    if let Some(from) = copy_of {
        // SAFETY: two frames reached through the direct map; the new one
        // is not mapped anywhere yet.
        unsafe { core::ptr::copy_nonoverlapping(phys_to_virt(from) as *const u8, phys_to_virt(phys) as *mut u8, PAGE_SIZE as usize) };
    }
    match paging::map_vacant_in(cr3, page, phys, paging::USER | paging::WRITE) {
        Some(true) => true,
        // Another thread made the page meanwhile: the access goes there.
        Some(false) => {
            frame::free(phys);
            true
        }
        None => {
            frame::free(phys);
            false
        }
    }
}

/// A write to a page that maps a file's shared frame (read-only): the
/// mapping gets a copy of its own, writable. Only the page tables are
/// looked at (a read-only shared user page is nothing else), so the kernel
/// can come here when it writes to a program's memory, whatever it holds.
pub fn copy_on_write(addr: u64) -> bool {
    let page = addr & !(PAGE_SIZE - 1);
    let cr3 = cpu::read_cr3();
    let Some(e) = paging::entry_in(cr3, page) else { return false };
    if e & paging::USER == 0 {
        return false;
    }
    if e & paging::WRITE != 0 {
        return true; // another thread was first
    }
    if e & paging::SHARED == 0 {
        return false;
    }
    let Some(phys) = frame::alloc() else { return false };
    // SAFETY: the file's frame and a new one, through the direct map.
    unsafe { core::ptr::copy_nonoverlapping(phys_to_virt(e & paging::ADDR_MASK) as *const u8, phys_to_virt(phys) as *mut u8, PAGE_SIZE as usize) };
    if paging::replace_in(cr3, page, e, phys, paging::USER | paging::WRITE) {
        // Other processors running this program may still read the
        // file's frame through their TLBs.
        crate::arch::smp::shootdown_user(cr3);
    } else {
        frame::free(phys);
    }
    true
}

/// Makes sure `[ptr, ptr + len)` is backed by pages (before the kernel
/// touches user memory on a program's behalf).
pub fn populate(ptr: u64, len: u64) {
    if len == 0 || ptr >= USER_TOP {
        return;
    }
    let cr3 = cpu::read_cr3();
    let end = ptr.saturating_add(len).min(USER_TOP);
    let mut p = ptr & !(PAGE_SIZE - 1);
    while p < end {
        // A file's page comes read-only; if the kernel writes there, it
        // makes the copy then.
        if !paging::user_accessible(cr3, p) {
            fault(p, false);
        }
        p += PAGE_SIZE;
    }
}

/// What is at `addr` in a Linux process: the file mapped there and the
/// offset in it (`libgtk-3.so.0+0x1234`), or the kind of memory.
pub fn describe(pid: u32, addr: u64) -> String {
    let image = super::with_state_of(pid, |st| st.images.iter().find(|i| i.start <= addr && addr < i.end).cloned()).flatten();
    if let Some(i) = image.filter(|i| !i.name.starts_with('[')) {
        return alloc::format!("{}+{:#x}", i.name.rsplit('/').next().unwrap_or(&i.name), addr - i.start + i.off);
    }
    let found = super::with_state_of(pid, |st| {
        let (&start, v) = st.vmas.range(..=addr).next_back()?;
        (addr < v.end).then(|| match &v.backing {
            Backing::File(f) => alloc::format!("{}+{:#x}", f.name.rsplit('/').next().unwrap_or(&f.name), addr - start + v.off),
            Backing::SharedFile(f) => alloc::format!("{}+{:#x}", f.name.rsplit('/').next().unwrap_or(&f.name), addr - start + v.off),
            _ => alloc::format!("{addr:#x} (memory, +{:#x})", addr - start),
        })
    })
    .flatten();
    found.unwrap_or_else(|| alloc::format!("{addr:#x} (not mapped)"))
}

/// The areas of process `pid`, for /proc/<pid>/maps, by address: start,
/// end, protection, offset in the file and its name (or [heap], [stack]).
pub fn areas(pid: u32) -> Vec<(u64, u64, u32, u64, String)> {
    let mut out = super::with_state_of(pid, |st| {
        let mut out: Vec<(u64, u64, u32, u64, String)> = st.images.iter().map(|i| (i.start, i.end, i.prot, i.off, String::from(&*i.name))).collect();
        let heap_end = mem::align_up(st.brk_cur, PAGE_SIZE);
        if heap_end > st.brk_start {
            out.push((st.brk_start, heap_end, PROT_READ | PROT_WRITE, 0, String::from("[heap]")));
        }
        for (&s, v) in &st.vmas {
            // The stack's reserve is in its [stack] line.
            if st.images.iter().any(|i| i.start <= s && v.end <= i.end) {
                continue;
            }
            let name = match &v.backing {
                Backing::File(f) => f.name.clone(),
                Backing::SharedFile(f) => f.name.clone(),
                _ => String::new(),
            };
            out.push((s, v.end, v.prot, v.off, name));
        }
        out
    })
    .unwrap_or_default();
    out.sort_by_key(|a| a.0);
    out
}

pub fn new_map() -> BTreeMap<u64, Vma> {
    BTreeMap::new()
}

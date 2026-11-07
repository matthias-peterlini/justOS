//! Memory of a Linux process: the program break and the areas `mmap`
//! reserves. Pages are given out when first touched (demand paging): a
//! reservation costs nothing until it is used, as runtimes such as Go's
//! expect when they set aside gigabytes of address space. Private file
//! mappings are filled from the file on first touch. Shared mappings of
//! shared memory ([`super::shm`]: memfd, /dev/shm, MAP_SHARED |
//! MAP_ANONYMOUS) map its frames, the same ones in every process.

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
    /// A private copy of a file's bytes.
    File(Arc<Vec<u8>>),
    /// Shared memory: the same frames for every mapping (true), or a
    /// private copy of each page made when it is first touched.
    Shm(Arc<Shm>, bool),
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
fn free_range(st: &LinuxState, start: u64, end: u64) -> bool {
    !st.vmas.range(..end).any(|(&s, v)| s < end && v.end > start)
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
    let mut p = start;
    while p < end {
        if paging::user_accessible(cr3, p) {
            if let Some(phys) = paging::unmap_owned_in(cr3, p) {
                frame::free(phys);
            }
        }
        p += PAGE_SIZE;
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

/// Files that are mapped somewhere, by path, size and change time: the
/// dynamic loader maps each library four or five times, and every process
/// maps the same libc, so they share one copy of its bytes while any
/// mapping of it lives.
static MAPPED: IrqLock<Vec<Mapped>> = IrqLock::new(Vec::new());

struct Mapped {
    key: String,
    size: u64,
    modified: u64,
    data: Weak<Vec<u8>>,
}

fn map_key(path: &Path) -> String {
    alloc::format!("{path}").to_ascii_lowercase()
}

/// Reads a whole file for a mapping (shared between mappings of it).
fn file_bytes(fd: i32) -> Result<Arc<Vec<u8>>, i64> {
    let path = super::fd::file_path(fd).ok_or(-err::EBADF)?;
    let st = crate::fs::stat(&path).map_err(|e| -super::errno(user::fs_error(e)))?;
    let key = map_key(&path);
    {
        let mut cache = MAPPED.lock();
        cache.retain(|m| m.data.strong_count() > 0);
        if let Some(data) = cache.iter().find(|m| m.key == key && m.size == st.size && m.modified == st.modified).and_then(|m| m.data.upgrade()) {
            return Ok(data);
        }
    }
    let data = crate::fs::read_file(&path).map(Arc::new).map_err(|e| -super::errno(user::fs_error(e)))?;
    MAPPED.lock().push(Mapped { key, size: st.size, modified: st.modified, data: Arc::downgrade(&data) });
    Ok(data)
}

/// Forgets the shared bytes of a file that is about to change (mappings
/// already made keep their copy, as private mappings do).
pub fn forget_file(path: &Path) {
    let key = map_key(path);
    MAPPED.lock().retain(|m| m.key != key);
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
        let memfd = match super::fd::get(fd) {
            Some(d) => match &d.kind {
                super::fd::Kind::Memfd { shm, .. } => Some(shm.clone()),
                _ => None,
            },
            None => return -err::EBADF,
        };
        match memfd {
            Some(shm) => (Backing::Shm(shm, shared), off),
            // A shared mapping of a file is a private copy here.
            None => match file_bytes(fd) {
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
    let shared = matches!(backing, Backing::Shm(_, true));
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

/// madvise: MADV_DONTNEED and MADV_FREE give pages back; inside reserved
/// areas they come back as zeroes (or the file's bytes) when next touched.
pub fn sys_madvise(addr: u64, len: u64, advice: u64) -> i64 {
    const MADV_DONTNEED: u64 = 4;
    const MADV_FREE: u64 = 8;
    if advice != MADV_DONTNEED && advice != MADV_FREE {
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
pub fn fault(addr: u64, _write: bool) -> bool {
    let page = addr & !(PAGE_SIZE - 1);
    let found = super::try_state(|st| {
        let (&s, v) = st.vmas.range(..=page).next_back()?;
        (page < v.end && v.prot != 0).then(|| (s, v.clone()))
    })
    .flatten();
    let Some((start, vma)) = found else { return false };
    let cr3 = cpu::read_cr3();
    if paging::translate_in(cr3, page).is_some() {
        return paging::user_accessible(cr3, page);
    }
    let at = vma.off + (page - start);
    if let Backing::Shm(shm, true) = &vma.backing {
        // The shared frame itself.
        let Some(phys) = shm.frame(at / PAGE_SIZE) else { return false };
        return paging::map_in(cr3, page, phys, paging::USER | paging::WRITE | paging::SHARED).is_some();
    }
    let Some(phys) = frame::alloc() else { return false };
    if paging::map_in(cr3, page, phys, paging::USER | paging::WRITE).is_none() {
        frame::free(phys);
        return false;
    }
    match &vma.backing {
        Backing::File(data) => {
            let at = at as usize;
            if at < data.len() {
                let n = (data.len() - at).min(PAGE_SIZE as usize);
                // SAFETY: the page was just mapped in this address space.
                unsafe { core::ptr::copy_nonoverlapping(data.as_ptr().add(at), page as *mut u8, n) };
            }
        }
        Backing::Shm(shm, false) => {
            if let Some(f) = shm.existing(at / PAGE_SIZE) {
                // SAFETY: the page was just mapped; the frame is the Shm's.
                unsafe { core::ptr::copy_nonoverlapping(phys_to_virt(f) as *const u8, page as *mut u8, PAGE_SIZE as usize) };
            }
        }
        _ => {}
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
        if !paging::user_accessible(cr3, p) {
            fault(p, true);
        }
        p += PAGE_SIZE;
    }
}

/// The areas, for /proc/self/maps.
pub fn areas() -> Vec<(u64, u64, u32)> {
    with_state(|st| st.vmas.iter().map(|(&s, v)| (s, v.end, v.prot)).collect())
}

pub fn new_map() -> BTreeMap<u64, Vma> {
    BTreeMap::new()
}

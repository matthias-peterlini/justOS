//! Shared memory: `memfd_create`, `/dev/shm` (what `shm_open` opens) and
//! the memory behind `MAP_SHARED | MAP_ANONYMOUS`.
//!
//! A [`Shm`] owns its frames. Every shared mapping of it maps those same
//! frames, marked [`paging::SHARED`]: unmapping them or ending a process
//! never frees them, and fork shares them instead of copying. They go with
//! the last descriptor or mapping that holds the `Shm`. This is how a
//! Wayland client hands the compositor its pixels, and how a browser's
//! processes share buffers.

use super::fd::{self, Desc, Kind, O_CREAT, O_EXCL, O_RDWR, O_TRUNC};
use super::err;
use crate::mem::{frame, paging, phys_to_virt, PAGE_SIZE};
use crate::sync::IrqLock;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

const MFD_CLOEXEC: u32 = 1;
const MFD_ALLOW_SEALING: u32 = 2;

pub const F_SEAL_SEAL: u32 = 1;
const F_SEAL_SHRINK: u32 = 2;
const F_SEAL_GROW: u32 = 4;
const F_SEAL_WRITE: u32 = 8;
const F_SEAL_FUTURE_WRITE: u32 = 0x10;

/// A piece of shared memory.
pub struct Shm {
    /// Frames by page number; a page never touched has none (zeroes).
    pages: IrqLock<BTreeMap<u64, u64>>,
    size: AtomicU64,
    /// What /proc/self/fd shows ("/memfd:name (deleted)").
    pub name: String,
    seals: AtomicU32,
}

impl Shm {
    pub fn new(name: String, size: u64, seals: u32) -> Arc<Shm> {
        Arc::new(Shm { pages: IrqLock::new(BTreeMap::new()), size: AtomicU64::new(size), name, seals: AtomicU32::new(seals) })
    }

    pub fn size(&self) -> u64 {
        self.size.load(Ordering::Relaxed)
    }

    /// The frame of page `n`, made (zeroed) when first wanted.
    pub fn frame(&self, n: u64) -> Option<u64> {
        let mut p = self.pages.lock();
        if let Some(&f) = p.get(&n) {
            return Some(f);
        }
        let f = frame::alloc()?;
        p.insert(n, f);
        Some(f)
    }

    /// The frame of page `n`, if it was ever touched.
    pub fn existing(&self, n: u64) -> Option<u64> {
        self.pages.lock().get(&n).copied()
    }

    /// Copies out what is at `off` (up to the size).
    pub fn read_at(&self, off: u64, buf: &mut [u8]) -> usize {
        let size = self.size();
        if off >= size {
            return 0;
        }
        let n = (buf.len() as u64).min(size - off) as usize;
        let mut done = 0;
        while done < n {
            let at = off + done as u64;
            let inpage = (at % PAGE_SIZE) as usize;
            let k = (PAGE_SIZE as usize - inpage).min(n - done);
            match self.existing(at / PAGE_SIZE) {
                // SAFETY: the frame is RAM this Shm owns, reachable through
                // the direct map.
                Some(f) => unsafe { core::ptr::copy_nonoverlapping((phys_to_virt(f) as *const u8).add(inpage), buf[done..].as_mut_ptr(), k) },
                None => buf[done..done + k].fill(0),
            }
            done += k;
        }
        n
    }

    /// Writes at `off`, growing the size if it goes past it.
    pub fn write_at(&self, off: u64, data: &[u8]) -> Result<usize, i64> {
        let seals = self.seals.load(Ordering::Relaxed);
        if seals & (F_SEAL_WRITE | F_SEAL_FUTURE_WRITE) != 0 {
            return Err(-err::EPERM);
        }
        let end = off.checked_add(data.len() as u64).ok_or(-err::EFBIG)?;
        if end > self.size() && seals & F_SEAL_GROW != 0 {
            return Err(-err::EPERM);
        }
        let mut done = 0;
        while done < data.len() {
            let at = off + done as u64;
            let inpage = (at % PAGE_SIZE) as usize;
            let k = (PAGE_SIZE as usize - inpage).min(data.len() - done);
            let f = self.frame(at / PAGE_SIZE).ok_or(-err::ENOMEM)?;
            // SAFETY: as in read_at.
            unsafe { core::ptr::copy_nonoverlapping(data[done..].as_ptr(), (phys_to_virt(f) as *mut u8).add(inpage), k) };
            done += k;
        }
        self.size.fetch_max(end, Ordering::Relaxed);
        Ok(done)
    }

    /// ftruncate. What is cut off reads as zeroes if the size grows again;
    /// its frames stay until the end (a mapping may still reach them).
    pub fn truncate(&self, len: u64) -> Result<(), i64> {
        let size = self.size();
        let seals = self.seals.load(Ordering::Relaxed);
        if len < size && seals & F_SEAL_SHRINK != 0 || len > size && seals & F_SEAL_GROW != 0 {
            return Err(-err::EPERM);
        }
        if len < size {
            let frames: alloc::vec::Vec<(u64, u64)> = self.pages.lock().range(len / PAGE_SIZE..).map(|(&n, &f)| (n, f)).collect();
            for (n, f) in frames {
                let from = len.saturating_sub(n * PAGE_SIZE).min(PAGE_SIZE) as usize;
                // SAFETY: as in read_at.
                unsafe { core::ptr::write_bytes((phys_to_virt(f) as *mut u8).add(from), 0, PAGE_SIZE as usize - from) };
            }
        }
        self.size.store(len, Ordering::Relaxed);
        Ok(())
    }

    pub fn seals(&self) -> u32 {
        self.seals.load(Ordering::Relaxed)
    }

    /// F_ADD_SEALS.
    pub fn add_seals(&self, s: u32) -> i64 {
        if self.seals() & F_SEAL_SEAL != 0 {
            return -err::EPERM;
        }
        self.seals.fetch_or(s & 0x1f, Ordering::Relaxed);
        0
    }
}

impl Drop for Shm {
    fn drop(&mut self) {
        for (_, f) in core::mem::take(&mut *self.pages.lock()) {
            frame::free(f);
        }
    }
}

// The frames are only ever mapped as shared pages.
const _: () = assert!(paging::SHARED & paging::ADDR_MASK == 0);

fn install(shm: Arc<Shm>, flags: u32, cloexec: bool) -> i64 {
    fd::install(Desc::new(Kind::Memfd { shm, pos: IrqLock::new(0) }, flags), cloexec)
}

pub fn sys_memfd_create(name_ptr: u64, flags: u32) -> i64 {
    let name = match crate::user::user_cstr(name_ptr, 250) {
        Ok(n) => n,
        Err(e) => return -super::errno(e),
    };
    if flags & !(MFD_CLOEXEC | MFD_ALLOW_SEALING | 4 | 8 | 0xfc00_0000) != 0 {
        return -err::EINVAL;
    }
    // Without MFD_ALLOW_SEALING, no seals can be added.
    let seals = if flags & MFD_ALLOW_SEALING != 0 { 0 } else { F_SEAL_SEAL };
    install(Shm::new(alloc::format!("/memfd:{name} (deleted)"), 0, seals), O_RDWR, flags & MFD_CLOEXEC != 0)
}

/// Memory for `MAP_SHARED | MAP_ANONYMOUS`.
pub fn anonymous(len: u64) -> Arc<Shm> {
    Shm::new(String::from("/dev/zero (deleted)"), len, F_SEAL_SEAL)
}

// ----------------------------------------------------------------------------
// /dev/shm: named shared memory (shm_open), kept until unlinked.

static NAMED: IrqLock<BTreeMap<String, Arc<Shm>>> = IrqLock::new(BTreeMap::new());

/// The name inside /dev/shm, for a path there.
pub fn named(text: &str) -> Option<&str> {
    let n = text.strip_prefix("/dev/shm/")?;
    (!n.is_empty() && !n.contains('/')).then_some(n)
}

pub fn open_named(name: &str, flags: u32) -> i64 {
    let shm = {
        let mut all = NAMED.lock();
        match all.get(name) {
            Some(_) if flags & O_CREAT != 0 && flags & O_EXCL != 0 => return -err::EEXIST,
            Some(s) => s.clone(),
            None if flags & O_CREAT != 0 => {
                let s = Shm::new(alloc::format!("/dev/shm/{name}"), 0, F_SEAL_SEAL);
                all.insert(String::from(name), s.clone());
                s
            }
            None => return -err::ENOENT,
        }
    };
    if flags & O_TRUNC != 0 {
        let _ = shm.truncate(0);
    }
    install(shm, flags & 3, flags & fd::O_CLOEXEC != 0)
}

/// unlink of /dev/shm/name.
pub fn unlink_named(name: &str) -> i64 {
    match NAMED.lock().remove(name) {
        Some(s) => {
            drop(s);
            0
        }
        None => -err::ENOENT,
    }
}

/// Whether /dev/shm/name exists (for stat and access).
pub fn named_size(name: &str) -> Option<u64> {
    NAMED.lock().get(name).map(|s| s.size())
}

// ----------------------------------------------------------------------------
// fallocate

pub fn sys_fallocate(fdn: i32, mode: u64, off: u64, len: u64) -> i64 {
    let Some(d) = fd::get(fdn) else { return -err::EBADF };
    match &d.kind {
        Kind::Memfd { shm, .. } => {
            if mode != 0 {
                return -err::EOPNOTSUPP;
            }
            let end = off.saturating_add(len);
            if end > shm.size() {
                if let Err(e) = shm.truncate(end) {
                    return e;
                }
            }
            0
        }
        // glibc's posix_fallocate writes the zeroes itself after this.
        Kind::File { .. } => -err::EOPNOTSUPP,
        _ => -err::ESPIPE,
    }
}

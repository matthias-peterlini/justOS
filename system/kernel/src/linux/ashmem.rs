//! /dev/ashmem: Android's shared memory. An open is a region with no size;
//! ASHMEM_SET_SIZE (and a name) come before the first mmap, which makes its
//! memory; the descriptor goes to other processes (over binder or a unix
//! socket), which map the same memory. Unpinning is accepted but pages are
//! never taken back.

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use super::shm::Shm;
use super::{err, user_bytes};
use crate::sync::IrqLock;

const SET_NAME: u32 = 0x4100_7701;
const GET_NAME: u32 = 0x8100_7702;
const SET_SIZE: u32 = 0x4008_7703;
const GET_SIZE: u32 = 0x7704;
const SET_PROT_MASK: u32 = 0x4008_7705;
const GET_PROT_MASK: u32 = 0x7706;
const PIN: u32 = 0x4008_7707;
const UNPIN: u32 = 0x4008_7708;
const GET_PIN_STATUS: u32 = 0x7709;
const PURGE_ALL_CACHES: u32 = 0x770a;
const GET_FILE_ID: u32 = 0x8008_770b;

const NAME_LEN: usize = 256;
/// The name of a region not given one.
const DEFAULT_NAME: &str = "dev/ashmem";
const PROT_ALL: u32 = 7;
/// What GET_PIN_STATUS says: the pages are pinned (they always stay).
const IS_PINNED: i64 = 1;

/// The device numbers of /dev/ashmem (a misc device, major 10; the binder
/// devices are 60 to 62).
pub const RDEV: u64 = 0xa00 + 63;

/// Whether a path names the device: /dev/ashmem, or /dev/ashmem followed
/// by the boot id (the name Android 10 and later open first).
pub fn device(path: &str) -> bool {
    path.strip_prefix("/dev/ashmem").is_some_and(|rest| rest.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-'))
}

pub struct Ashmem {
    name: IrqLock<String>,
    size: AtomicU64,
    prot_mask: AtomicU32,
    /// Its memory, made by the first mmap.
    shm: IrqLock<Option<Arc<Shm>>>,
    pos: IrqLock<u64>,
}

pub fn open() -> Arc<Ashmem> {
    Arc::new(Ashmem {
        name: IrqLock::new(String::new()),
        size: AtomicU64::new(0),
        prot_mask: AtomicU32::new(PROT_ALL),
        shm: IrqLock::new(None),
        pos: IrqLock::new(0),
    })
}

impl Ashmem {
    fn mapped(&self) -> bool {
        self.shm.lock().is_some()
    }

    /// The memory to map `len` bytes of with `prot`: made on the first map,
    /// at the size set (a private mapping gets zeroes of its own instead, as
    /// on Linux, but the region still has its memory from then on).
    pub fn map(&self, len: u64, prot: u32) -> Result<Arc<Shm>, i64> {
        let size = self.size.load(Ordering::Relaxed);
        if size == 0 {
            return Err(-err::EINVAL);
        }
        if len > crate::mem::align_up(size, crate::mem::PAGE_SIZE) {
            return Err(-err::EINVAL);
        }
        if prot & !self.prot_mask.load(Ordering::Relaxed) & PROT_ALL != 0 {
            return Err(-err::EPERM);
        }
        let name = self.name.lock().clone();
        let mut shm = self.shm.lock();
        let name = if name.is_empty() { String::from(DEFAULT_NAME) } else { format!("{DEFAULT_NAME}/{name}") };
        Ok(shm.get_or_insert_with(|| Shm::new(format!("/{name} (deleted)"), size, 0)).clone())
    }

    pub fn read(&self, buf: &mut [u8]) -> i64 {
        let Some(shm) = self.shm.lock().clone() else { return 0 };
        let mut pos = self.pos.lock();
        let n = shm.read_at(*pos, buf);
        *pos += n as u64;
        n as i64
    }

    pub fn size(&self) -> u64 {
        self.size.load(Ordering::Relaxed)
    }
}

pub fn ioctl(a: &Ashmem, req: u64, arg: u64) -> i64 {
    match req as u32 {
        SET_NAME => {
            if a.mapped() {
                return -err::EINVAL;
            }
            // Up to 255 bytes; a longer name is cut.
            let mut name = String::new();
            for i in 0..NAME_LEN as u64 - 1 {
                let Ok(b) = user_bytes(arg + i, 1) else { return -err::EFAULT };
                if b[0] == 0 {
                    break;
                }
                name.push(b[0] as char);
            }
            *a.name.lock() = name;
            0
        }
        GET_NAME => {
            let name = a.name.lock().clone();
            let name = if name.is_empty() { String::from(DEFAULT_NAME) } else { name };
            let n = name.len().min(NAME_LEN - 1);
            match user_bytes(arg, n as u64 + 1) {
                Ok(b) => {
                    b[..n].copy_from_slice(&name.as_bytes()[..n]);
                    b[n] = 0;
                    0
                }
                Err(e) => e,
            }
        }
        SET_SIZE => {
            if a.mapped() {
                return -err::EINVAL;
            }
            a.size.store(arg, Ordering::Relaxed);
            0
        }
        GET_SIZE => a.size() as i64,
        SET_PROT_MASK => {
            let prot = arg as u32 & PROT_ALL;
            let mask = a.prot_mask.load(Ordering::Relaxed);
            // Rights only go.
            if mask & prot != prot {
                return -err::EINVAL;
            }
            a.prot_mask.store(prot, Ordering::Relaxed);
            0
        }
        GET_PROT_MASK => a.prot_mask.load(Ordering::Relaxed) as i64,
        PIN | UNPIN => {
            if user_bytes(arg, 8).is_err() {
                return -err::EFAULT;
            }
            0
        }
        GET_PIN_STATUS => IS_PINNED,
        PURGE_ALL_CACHES => 0,
        GET_FILE_ID => match user_bytes(arg, 8) {
            Ok(b) => {
                b.copy_from_slice(&(a as *const Ashmem as u64 >> 4).to_le_bytes());
                0
            }
            Err(e) => e,
        },
        _ => -err::ENOTTY,
    }
}

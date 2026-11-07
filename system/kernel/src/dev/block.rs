//! Disks: the block devices drivers register, their partitions, and the
//! glue that lets file systems use them.
//!
//! Every registered disk is `disk<N>`; MBR and GPT partitions become
//! `disk<N>p<M>`. New disks and partitions are offered to the file system
//! layer right away, so volumes mount by themselves.

use crate::sync::{IrqLock, Mutex};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use justdk::{BlockDesc, Status, BLOCK_READONLY, BLOCK_REMOVABLE, E_INVAL, E_IO, OK};

pub struct Disk {
    pub name: String,
    pub device: u32,
    pub model: String,
    pub sector_size: u32,
    pub sectors: u64,
    pub readonly: bool,
    pub removable: bool,
    /// For partitions: the whole disk and the first sector.
    pub parent: Option<(Arc<Disk>, u64)>,
    /// Partition type ("JustFS", "FAT32", GUID...) for partitions.
    pub kind: String,
    desc: *const BlockDesc,
    io: Mutex<()>,
    gone: IrqLock<bool>,
}

// SAFETY: driver calls through `desc` are serialised by `io`.
unsafe impl Send for Disk {}
unsafe impl Sync for Disk {}

impl Disk {
    pub fn size_bytes(&self) -> u64 {
        self.sectors * self.sector_size as u64
    }

    fn whole(&self) -> (&Disk, u64) {
        match &self.parent {
            Some((d, start)) => (d, *start),
            None => (self, 0),
        }
    }

    /// Reads `buf.len() / sector_size` sectors at `lba`.
    pub fn read(&self, lba: u64, buf: &mut [u8]) -> Status {
        self.transfer(lba, buf.as_mut_ptr(), buf.len(), false)
    }

    pub fn write(&self, lba: u64, buf: &[u8]) -> Status {
        if self.readonly {
            return justdk::E_UNSUPPORTED;
        }
        self.transfer(lba, buf.as_ptr() as *mut u8, buf.len(), true)
    }

    fn transfer(&self, lba: u64, buf: *mut u8, len: usize, write: bool) -> Status {
        let ss = self.sector_size as usize;
        if !len.is_multiple_of(ss) {
            return E_INVAL;
        }
        let count = (len / ss) as u64;
        if lba + count > self.sectors {
            return E_INVAL;
        }
        let (disk, offset) = self.whole();
        if *disk.gone.lock() {
            return E_IO;
        }
        let _io = disk.lock_io();
        // SAFETY: the descriptor stays valid while the disk is registered;
        // `buf` holds `count` sectors.
        unsafe {
            let d = &*disk.desc;
            let mut done = 0u64;
            // Split big requests so drivers see bounded transfers.
            while done < count {
                let n = (count - done).min(128) as u32;
                let p = buf.add(done as usize * ss);
                let st = if write { (d.write)(d.ctx, offset + lba + done, n, p) } else { (d.read)(d.ctx, offset + lba + done, n, p) };
                if st != OK {
                    return st;
                }
                done += n as u64;
            }
        }
        OK
    }

    /// The disk's I/O lock, once the devices are up (they may be asleep).
    fn lock_io(&self) -> crate::sync::MutexGuard<'_, ()> {
        loop {
            let g = self.io.lock();
            if super::pm::awake() {
                return g;
            }
            drop(g);
            super::pm::wait_awake();
        }
    }

    /// Waits for the transfer under way, if any (before the machine sleeps).
    pub fn wait_idle(&self) {
        drop(self.whole().0.io.lock());
    }

    pub fn flush(&self) -> Status {
        let (disk, _) = self.whole();
        let _io = disk.lock_io();
        // SAFETY: as in transfer.
        unsafe {
            let d = &*disk.desc;
            match d.flush {
                Some(f) => f(d.ctx),
                None => OK,
            }
        }
    }
}

static DISKS: IrqLock<Vec<Arc<Disk>>> = IrqLock::new(Vec::new());
static NEXT: IrqLock<u32> = IrqLock::new(0);

pub fn list() -> Vec<Arc<Disk>> {
    DISKS.lock().clone()
}

pub fn find(name: &str) -> Option<Arc<Disk>> {
    DISKS.lock().iter().find(|d| d.name.eq_ignore_ascii_case(name)).cloned()
}

/// Called by drivers (through the ABI) for every disk they find.
pub fn register(device: u32, desc: *const BlockDesc) -> Status {
    // SAFETY: the driver passes a descriptor that outlives the disk.
    let d = unsafe { &*desc };
    if d.sector_size == 0 || d.sector_size > 4096 || !d.sector_size.is_power_of_two() || d.sector_count == 0 {
        return E_INVAL;
    }
    let n = {
        let mut next = NEXT.lock();
        let n = *next;
        *next += 1;
        n
    };
    let len = d.model.iter().position(|&b| b == 0).unwrap_or(d.model.len());
    let model = String::from(core::str::from_utf8(&d.model[..len]).unwrap_or("").trim());
    let disk = Arc::new(Disk {
        name: alloc::format!("disk{n}"),
        device,
        model,
        sector_size: d.sector_size,
        sectors: d.sector_count,
        readonly: d.flags & BLOCK_READONLY != 0,
        removable: d.flags & BLOCK_REMOVABLE != 0,
        parent: None,
        kind: String::new(),
        desc,
        io: Mutex::new(()),
        gone: IrqLock::new(false),
    });
    crate::info!(
        "disk",
        "{}: {} ({}, {} sectors of {} bytes)",
        disk.name,
        if disk.model.is_empty() { "disk" } else { &disk.model },
        crate::util::Size(disk.size_bytes()),
        disk.sectors,
        disk.sector_size
    );
    DISKS.lock().push(disk.clone());
    let parts = scan_partitions(&disk);
    crate::fs::offer(&disk);
    for p in parts {
        crate::fs::offer(&p);
    }
    n as Status
}

/// Removes the disks of a device (driver unloaded). Fails while one of them
/// holds a mounted volume.
pub fn unregister_device(device: u32) -> Result<(), &'static str> {
    let disks: Vec<Arc<Disk>> = DISKS.lock().iter().filter(|d| d.whole().0.device == device).cloned().collect();
    for d in &disks {
        if crate::fs::is_mounted_on(&d.name) {
            return Err("a volume on this disk is in use");
        }
    }
    for d in &disks {
        *d.gone.lock() = true;
    }
    DISKS.lock().retain(|d| d.whole().0.device != device);
    Ok(())
}

/// The hardware behind a device's disks is gone (unplugged): their volumes
/// are dropped without flushing and the disks removed.
pub fn device_gone(device: u32) {
    let disks: Vec<Arc<Disk>> = DISKS.lock().iter().filter(|d| d.whole().0.device == device).cloned().collect();
    if disks.is_empty() {
        return;
    }
    for d in &disks {
        *d.gone.lock() = true;
        crate::fs::disk_gone(&d.name);
    }
    DISKS.lock().retain(|d| d.whole().0.device != device);
    for d in disks.iter().filter(|d| d.parent.is_none()) {
        crate::info!("disk", "{}: removed", d.name);
    }
}

fn add_partition(disk: &Arc<Disk>, index: usize, start: u64, sectors: u64, kind: String) -> Option<Arc<Disk>> {
    if start == 0 || sectors == 0 || start + sectors > disk.sectors {
        return None;
    }
    let p = Arc::new(Disk {
        name: alloc::format!("{}p{}", disk.name, index),
        device: disk.device,
        model: disk.model.clone(),
        sector_size: disk.sector_size,
        sectors,
        readonly: disk.readonly,
        removable: disk.removable,
        parent: Some((disk.clone(), start)),
        kind,
        desc: disk.desc,
        io: Mutex::new(()),
        gone: IrqLock::new(false),
    });
    crate::debug!("disk", "{}: partition {} at sector {}, {}", p.name, p.kind, start, crate::util::Size(p.size_bytes()));
    DISKS.lock().push(p.clone());
    Some(p)
}

fn mbr_type_name(t: u8) -> &'static str {
    match t {
        0xAC => "JustFS",
        0x01 | 0x04 | 0x06 | 0x0E => "FAT16",
        0x0B | 0x0C => "FAT32",
        0x07 => "NTFS/exFAT",
        0x83 => "Linux",
        0x82 => "Linux swap",
        0xEF => "EFI system",
        0x05 | 0x0F => "extended",
        _ => "unknown",
    }
}

const GPT_JUSTFS: [u8; 16] = *b"justOS-JustFS-1\0";

fn guid_kind(g: &[u8]) -> String {
    match g {
        _ if g == GPT_JUSTFS => String::from("JustFS"),
        [0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, ..] => String::from("EFI system"),
        [0xA2, 0xA0, 0xD0, 0xEB, 0xE5, 0xB9, 0x33, 0x44, ..] => String::from("Microsoft data"),
        [0xAF, 0x3D, 0xC6, 0x0F, 0x83, 0x84, 0x72, 0x47, ..] => String::from("Linux"),
        _ => String::from("unknown"),
    }
}

fn scan_partitions(disk: &Arc<Disk>) -> Vec<Arc<Disk>> {
    let ss = disk.sector_size as usize;
    let mut out = Vec::new();
    let mut mbr = vec![0u8; ss.max(512)];
    if disk.read(0, &mut mbr[..ss]) != OK || mbr[510] != 0x55 || mbr[511] != 0xAA {
        return out;
    }
    let entries: Vec<(u8, u64, u64)> = (0..4)
        .map(|i| {
            let e = &mbr[0x1BE + i * 16..0x1CE + i * 16];
            (e[4], u32::from_le_bytes(e[8..12].try_into().unwrap()) as u64, u32::from_le_bytes(e[12..16].try_into().unwrap()) as u64)
        })
        .collect();
    if entries.iter().any(|e| e.0 == 0xEE) {
        return scan_gpt(disk);
    }
    for (i, &(t, start, len)) in entries.iter().enumerate() {
        if t != 0 && t != 0x05 && t != 0x0F {
            if let Some(p) = add_partition(disk, i + 1, start, len, String::from(mbr_type_name(t))) {
                out.push(p);
            }
        }
    }
    out
}

fn scan_gpt(disk: &Arc<Disk>) -> Vec<Arc<Disk>> {
    let ss = disk.sector_size as usize;
    let mut out = Vec::new();
    let mut hdr = vec![0u8; ss];
    if disk.read(1, &mut hdr) != OK || &hdr[..8] != b"EFI PART" {
        return out;
    }
    let le64 = |b: &[u8], o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
    let le32 = |b: &[u8], o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    let entries_lba = le64(&hdr, 72);
    let count = le32(&hdr, 80).min(256) as usize;
    let esize = le32(&hdr, 84) as usize;
    if !(128..=1024).contains(&esize) {
        return out;
    }
    let bytes = (count * esize).div_ceil(ss) * ss;
    let mut table = vec![0u8; bytes];
    if disk.read(entries_lba, &mut table) != OK {
        return out;
    }
    for i in 0..count {
        let e = &table[i * esize..(i + 1) * esize];
        if e[..16].iter().all(|&b| b == 0) {
            continue;
        }
        let first = le64(e, 32);
        let last = le64(e, 40);
        if last < first {
            continue;
        }
        if let Some(p) = add_partition(disk, i + 1, first, last - first + 1, guid_kind(&e[..16])) {
            out.push(p);
        }
    }
    out
}

/// JustFS on top of a disk or partition: 4 KiB blocks over its sectors,
/// with the blocks used last kept in memory and writes held back until a
/// flush.
pub struct DiskBlocks {
    pub disk: Arc<Disk>,
    cache: BlockCache,
}

impl DiskBlocks {
    pub fn new(disk: Arc<Disk>) -> DiskBlocks {
        DiskBlocks { disk, cache: BlockCache::new() }
    }
}

/// Blocks read or written last, so that walking a path or reading a file
/// again does not go to the disk each time: JustFS reads a folder's node and
/// all its entries for every name on a path. Written blocks stay here
/// (dirty) until the volume is flushed (by its flusher within a second, by
/// fsync, at shutdown) or until they make room for another block: a file
/// written a little at a time, and the nodes and bitmap blocks every write
/// changes, go to the disk once. Sets of [`WAYS`] blocks, the newest first;
/// a block can only be in the set its number picks.
struct BlockCache {
    sets: Vec<Vec<Entry>>,
}

struct Entry {
    block: u64,
    data: alloc::boxed::Box<justfs::Block>,
    /// Changed since it was read from or written to the disk.
    dirty: bool,
}

const WAYS: usize = 4;
/// Blocks a flush writes with one command, when they follow each other.
const FLUSH_RUN: usize = 16;
/// Blocks a miss reads with one command: the block and those after it that
/// are not here either (files are mostly runs of blocks, read forward).
const READ_AHEAD: u64 = 8;

impl BlockCache {
    /// A thirty-second of the memory, between 1 and 64 MiB.
    fn new() -> BlockCache {
        let (_, total) = crate::mem::frame::stats();
        let blocks = (total / 32 / justfs::BLOCK_SIZE as u64).clamp(256, 16384) as usize;
        BlockCache { sets: (0..blocks / WAYS).map(|_| Vec::new()).collect() }
    }

    fn set(&mut self, block: u64) -> &mut Vec<Entry> {
        let n = self.sets.len() as u64;
        &mut self.sets[(block % n) as usize]
    }

    fn get(&mut self, block: u64, buf: &mut justfs::Block) -> bool {
        let set = self.set(block);
        let Some(i) = set.iter().position(|e| e.block == block) else { return false };
        let hit = set.remove(i);
        buf.copy_from_slice(&hit.data[..]);
        set.insert(0, hit);
        true
    }

    fn contains(&mut self, block: u64) -> bool {
        self.set(block).iter().any(|e| e.block == block)
    }

    /// The dirty block that keeping `block` would push out, if any.
    fn dirty_victim(&mut self, block: u64) -> Option<&mut Entry> {
        let set = self.set(block);
        if set.len() < WAYS || set.iter().any(|e| e.block == block) {
            return None;
        }
        set.last_mut().filter(|e| e.dirty)
    }

    /// Keeps `data` as `block` (dirty: not on the disk yet). Whatever it
    /// pushes out must be clean ([`dirty_victim`](Self::dirty_victim)).
    fn put(&mut self, block: u64, data: &justfs::Block, dirty: bool) {
        let set = self.set(block);
        let mut entry = match set.iter().position(|e| e.block == block) {
            Some(i) => {
                let e = set.remove(i);
                Entry { dirty: e.dirty || dirty, ..e }
            }
            None if set.len() == WAYS => Entry { block, dirty, ..set.pop().unwrap() },
            None => Entry { block, data: alloc::boxed::Box::new([0u8; justfs::BLOCK_SIZE]), dirty },
        };
        entry.data.copy_from_slice(data);
        set.insert(0, entry);
    }

    /// Where the dirty blocks are (set, way), in block order.
    fn dirty(&self) -> Vec<(u64, usize, usize)> {
        let mut v: Vec<_> = self
            .sets
            .iter()
            .enumerate()
            .flat_map(|(s, set)| set.iter().enumerate().filter(|(_, e)| e.dirty).map(move |(w, e)| (e.block, s, w)))
            .collect();
        v.sort_unstable();
        v
    }
}

impl DiskBlocks {
    fn sectors_per_block(&self) -> u64 {
        justfs::BLOCK_SIZE as u64 / self.disk.sector_size as u64
    }

    /// Writes out the block that keeping `block` would push out, if it was
    /// not on the disk yet.
    fn make_room(&mut self, block: u64) -> justfs::Result<()> {
        let per = self.sectors_per_block();
        let disk = &self.disk;
        if let Some(e) = self.cache.dirty_victim(block) {
            if disk.write(e.block * per, &e.data[..]) != OK {
                return Err(justfs::Error::Io);
            }
            e.dirty = false;
        }
        Ok(())
    }

    /// Writes every dirty block, those that follow each other together.
    fn write_dirty(&mut self) -> justfs::Result<()> {
        let per = self.sectors_per_block();
        let dirty = self.cache.dirty();
        let mut buf = Vec::with_capacity(FLUSH_RUN * justfs::BLOCK_SIZE);
        let mut failed = false;
        let mut i = 0;
        while i < dirty.len() {
            let mut n = 1;
            while i + n < dirty.len() && n < FLUSH_RUN && dirty[i + n].0 == dirty[i].0 + n as u64 {
                n += 1;
            }
            buf.clear();
            for &(_, s, w) in &dirty[i..i + n] {
                buf.extend_from_slice(&self.cache.sets[s][w].data[..]);
            }
            if self.disk.write(dirty[i].0 * per, &buf) == OK {
                for &(_, s, w) in &dirty[i..i + n] {
                    self.cache.sets[s][w].dirty = false;
                }
            } else {
                failed = true;
            }
            i += n;
        }
        if failed {
            Err(justfs::Error::Io)
        } else {
            Ok(())
        }
    }
}

impl justfs::BlockDevice for DiskBlocks {
    fn read_block(&mut self, block: u64, buf: &mut justfs::Block) -> justfs::Result<()> {
        if self.cache.get(block, buf) {
            return Ok(());
        }
        self.make_room(block)?;
        let per = self.sectors_per_block();
        let max = READ_AHEAD.min(self.block_count().saturating_sub(block)).max(1);
        let mut n = 1;
        while n < max && !self.cache.contains(block + n) {
            n += 1;
        }
        let bs = justfs::BLOCK_SIZE;
        let mut run = alloc::vec![0u8; n as usize * bs];
        if self.disk.read(block * per, &mut run) != OK {
            return Err(justfs::Error::Io);
        }
        buf.copy_from_slice(&run[..bs]);
        self.cache.put(block, buf, false);
        for i in 1..n {
            if self.make_room(block + i).is_err() {
                break;
            }
            let data: &justfs::Block = run[i as usize * bs..(i as usize + 1) * bs].try_into().unwrap();
            self.cache.put(block + i, data, false);
        }
        Ok(())
    }

    fn write_block(&mut self, block: u64, buf: &justfs::Block) -> justfs::Result<()> {
        if self.disk.readonly {
            return Err(justfs::Error::Io);
        }
        self.make_room(block)?;
        self.cache.put(block, buf, true);
        Ok(())
    }

    fn block_count(&self) -> u64 {
        self.disk.size_bytes() / justfs::BLOCK_SIZE as u64
    }

    fn write_back(&mut self) -> justfs::Result<()> {
        self.write_dirty()
    }

    fn flush(&mut self) -> justfs::Result<()> {
        self.write_dirty()?;
        if self.disk.flush() == OK {
            Ok(())
        } else {
            Err(justfs::Error::Io)
        }
    }

    fn read_only(&self) -> bool {
        self.disk.readonly
    }
}

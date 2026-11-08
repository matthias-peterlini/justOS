//! JustFS — the native file system of justOS.
//!
//! A simple extent-based design with 4 KiB blocks:
//!
//! ```text
//! block 0                 superblock
//! bitmap_start ..         allocation bitmap, one bit per block
//! nodes_start ..          node table, 128-byte nodes (node 0 unused, 1 = root)
//! data_start ..           file and directory contents
//! ```
//!
//! A node (file, directory or symbolic link) maps its contents with up to 11
//! extents kept in the node plus 512 more in one indirect block. A
//! directory's contents are 64-byte entries; a link's contents are the path
//! it points to, followed when a path is looked up (an absolute target from
//! the volume's root, a relative one from the link's folder). Names are case-preserving and compared without regard to
//! ASCII case (`Drivers` and `drivers` are the same name); `/` separates path
//! components and names cannot contain `/`, `:` or control characters.
//!
//! The crate is `no_std` + `alloc`; the kernel and the host tools share it.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

pub const BLOCK_SIZE: usize = 4096;
pub const MAGIC: &[u8; 8] = b"JUSTFS\0\x01";
pub const VERSION: u32 = 1;
pub const NODE_SIZE: usize = 128;
pub const NODES_PER_BLOCK: usize = BLOCK_SIZE / NODE_SIZE;
pub const DIRECT_EXTENTS: usize = 11;
pub const INDIRECT_EXTENTS: usize = BLOCK_SIZE / 8;
pub const MAX_EXTENTS: usize = DIRECT_EXTENTS + INDIRECT_EXTENTS;
pub const DIRENT_SIZE: usize = 64;
/// The longest name, in bytes (as Linux's NAME_MAX; Windows programs, run
/// through Wine, use long ones).
pub const MAX_NAME: usize = 255;
/// Name bytes in an entry's slot. A longer name goes on in the slots right
/// after it, each marked [`CONT`] in place of a node and holding
/// [`CONT_NAME`] bytes more; short names take one slot, as they always did.
const HEAD_NAME: usize = 56;
const CONT_NAME: usize = DIRENT_SIZE - 4;
const CONT: NodeId = NodeId::MAX;

/// The slots an entry with a name of `len` bytes takes.
fn slots_for(len: usize) -> usize {
    1 + len.saturating_sub(HEAD_NAME).div_ceil(CONT_NAME)
}
pub const ROOT: NodeId = 1;

pub type NodeId = u32;
pub type Block = [u8; BLOCK_SIZE];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    NotFound,
    Exists,
    NotDir,
    IsDir,
    NotEmpty,
    NoSpace,
    NoNodes,
    NameTooLong,
    InvalidName,
    TooFragmented,
    ReadOnly,
    Corrupt,
    Io,
    /// A path went through too many symbolic links (or a loop of them).
    Loop,
}

impl Error {
    pub fn as_str(self) -> &'static str {
        match self {
            Error::NotFound => "not found",
            Error::Exists => "already exists",
            Error::NotDir => "not a folder",
            Error::IsDir => "is a folder",
            Error::NotEmpty => "folder not empty",
            Error::NoSpace => "disk full",
            Error::NoNodes => "no free nodes",
            Error::NameTooLong => "name too long",
            Error::InvalidName => "invalid name",
            Error::TooFragmented => "file too fragmented",
            Error::ReadOnly => "read-only volume",
            Error::Corrupt => "corrupt file system",
            Error::Io => "I/O error",
            Error::Loop => "too many links",
        }
    }
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

pub type Result<T> = core::result::Result<T, Error>;

/// Storage under a file system: 4 KiB blocks.
pub trait BlockDevice {
    fn read_block(&mut self, block: u64, buf: &mut Block) -> Result<()>;
    fn write_block(&mut self, block: u64, buf: &Block) -> Result<()>;
    fn block_count(&self) -> u64;
    /// Makes every block written so far durable.
    fn flush(&mut self) -> Result<()> {
        self.write_back()
    }
    /// Hands every block written so far to the device (out of any cache of
    /// the caller's), without waiting for the device to make it durable:
    /// [`BlockDevice::flush`] does both.
    fn write_back(&mut self) -> Result<()> {
        Ok(())
    }
    fn read_only(&self) -> bool {
        false
    }
}

impl<T: BlockDevice + ?Sized> BlockDevice for alloc::boxed::Box<T> {
    fn read_block(&mut self, block: u64, buf: &mut Block) -> Result<()> {
        (**self).read_block(block, buf)
    }
    fn write_block(&mut self, block: u64, buf: &Block) -> Result<()> {
        (**self).write_block(block, buf)
    }
    fn block_count(&self) -> u64 {
        (**self).block_count()
    }
    fn flush(&mut self) -> Result<()> {
        (**self).flush()
    }
    fn write_back(&mut self) -> Result<()> {
        (**self).write_back()
    }
    fn read_only(&self) -> bool {
        (**self).read_only()
    }
}

/// Seconds since 2000-01-01 00:00 UTC, supplied by the host or the kernel.
pub trait Clock: Sync {
    fn now(&self) -> u64;
}

/// A clock that is always 0 (for images built without a time source).
pub struct NoClock;

impl Clock for NoClock {
    fn now(&self) -> u64 {
        0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    Free = 0,
    File = 1,
    Dir = 2,
    /// A symbolic link: its contents are the path it points to.
    Link = 3,
}

impl Kind {
    fn from_u8(v: u8) -> Kind {
        match v {
            1 => Kind::File,
            2 => Kind::Dir,
            3 => Kind::Link,
            _ => Kind::Free,
        }
    }
}

/// `Node::flags`
pub const FLAG_SYSTEM: u8 = 1;
pub const FLAG_HIDDEN: u8 = 2;
pub const FLAG_READONLY: u8 = 4;

/// `Node::mode`: set on a node whose Unix permission bits (the low 12) were
/// given; a node without it has none (programs see the usual defaults).
pub const MODE_SET: u16 = 0x8000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Extent {
    pub start: u32,
    pub len: u32,
}

/// A node as stored on disk (the indirect extents are loaded on demand).
#[derive(Clone, Debug)]
pub struct Node {
    pub kind: Kind,
    pub flags: u8,
    /// Unix permission bits with [`MODE_SET`], or 0.
    pub mode: u16,
    pub parent: NodeId,
    pub size: u64,
    pub created: u64,
    pub modified: u64,
    pub extents: [Extent; DIRECT_EXTENTS],
    pub extent_count: u16,
    pub indirect: u32,
}

impl Node {
    fn empty(kind: Kind, parent: NodeId, now: u64) -> Node {
        Node {
            kind,
            flags: 0,
            mode: 0,
            parent,
            size: 0,
            created: now,
            modified: now,
            extents: [Extent::default(); DIRECT_EXTENTS],
            extent_count: 0,
            indirect: 0,
        }
    }

    fn decode(b: &[u8]) -> Node {
        let mut extents = [Extent::default(); DIRECT_EXTENTS];
        for (i, e) in extents.iter_mut().enumerate() {
            e.start = le32(b, 32 + i * 8);
            e.len = le32(b, 36 + i * 8);
        }
        Node {
            kind: Kind::from_u8(b[0]),
            flags: b[1],
            mode: le16(b, 124),
            extent_count: le16(b, 2),
            parent: le32(b, 4),
            size: le64(b, 8),
            created: le64(b, 16),
            modified: le64(b, 24),
            extents,
            indirect: le32(b, 120),
        }
    }

    fn encode(&self, b: &mut [u8]) {
        b[..NODE_SIZE].fill(0);
        b[0] = self.kind as u8;
        b[1] = self.flags;
        put16(b, 2, self.extent_count);
        put32(b, 4, self.parent);
        put64(b, 8, self.size);
        put64(b, 16, self.created);
        put64(b, 24, self.modified);
        for (i, e) in self.extents.iter().enumerate() {
            put32(b, 32 + i * 8, e.start);
            put32(b, 36 + i * 8, e.len);
        }
        put32(b, 120, self.indirect);
        put16(b, 124, self.mode);
    }

    pub fn is_dir(&self) -> bool {
        self.kind == Kind::Dir
    }

    pub fn is_link(&self) -> bool {
        self.kind == Kind::Link
    }
}

/// How many symbolic links one lookup may go through (as on Linux).
pub const MAX_LINKS: u32 = 40;

/// One entry of a directory listing.
#[derive(Clone, Debug)]
pub struct DirEntry {
    pub name: String,
    pub node: NodeId,
    pub kind: Kind,
}

#[derive(Clone, Debug)]
pub struct Superblock {
    pub block_count: u64,
    pub node_count: u32,
    pub bitmap_start: u32,
    pub bitmap_blocks: u32,
    pub nodes_start: u32,
    pub nodes_blocks: u32,
    pub data_start: u32,
    pub free_blocks: u64,
    pub free_nodes: u32,
    pub label: String,
    pub created: u64,
}

impl Superblock {
    fn decode(b: &Block) -> Result<Superblock> {
        if &b[..8] != MAGIC || le32(b, 8) != VERSION || le32(b, 12) as usize != BLOCK_SIZE {
            return Err(Error::Corrupt);
        }
        let label_len = b[64..96].iter().position(|&c| c == 0).unwrap_or(32);
        Ok(Superblock {
            block_count: le64(b, 16),
            node_count: le32(b, 24),
            bitmap_start: le32(b, 28),
            bitmap_blocks: le32(b, 32),
            nodes_start: le32(b, 36),
            nodes_blocks: le32(b, 40),
            data_start: le32(b, 44),
            free_blocks: le64(b, 48),
            free_nodes: le32(b, 56),
            label: String::from_utf8_lossy(&b[64..64 + label_len]).into_owned(),
            created: le64(b, 96),
        })
    }

    fn encode(&self, b: &mut Block) {
        b.fill(0);
        b[..8].copy_from_slice(MAGIC);
        put32(b, 8, VERSION);
        put32(b, 12, BLOCK_SIZE as u32);
        put64(b, 16, self.block_count);
        put32(b, 24, self.node_count);
        put32(b, 28, self.bitmap_start);
        put32(b, 32, self.bitmap_blocks);
        put32(b, 36, self.nodes_start);
        put32(b, 40, self.nodes_blocks);
        put32(b, 44, self.data_start);
        put64(b, 48, self.free_blocks);
        put32(b, 56, self.free_nodes);
        put32(b, 60, ROOT);
        let n = self.label.len().min(31);
        b[64..64 + n].copy_from_slice(&self.label.as_bytes()[..n]);
        put64(b, 96, self.created);
    }
}

/// Is `name` acceptable as a file or folder name?
pub fn valid_name(name: &str) -> Result<()> {
    if name.is_empty() || name == "." || name == ".." {
        return Err(Error::InvalidName);
    }
    if name.len() > MAX_NAME {
        return Err(Error::NameTooLong);
    }
    // A ':' is the volume's only before the path's first '/' ("sys:/a"),
    // so names may have one (as on Linux: Wine's drives are "c:" links).
    if name.chars().any(|c| c == '/' || c == '\\' || c.is_control()) {
        return Err(Error::InvalidName);
    }
    Ok(())
}

/// Case-insensitive (ASCII) name comparison used everywhere in JustFS.
pub fn same_name(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// A mounted JustFS volume.
pub struct Fs<D: BlockDevice> {
    dev: D,
    sb: Superblock,
    bitmap: Vec<u8>,
    alloc_hint: u64,
    /// No free node below this one (so a search for one starts here).
    node_hint: NodeId,
    clock: &'static dyn Clock,
}

impl<D: BlockDevice> Fs<D> {
    /// Creates an empty file system covering the whole device.
    pub fn format(mut dev: D, label: &str, clock: &'static dyn Clock) -> Result<Fs<D>> {
        let block_count = dev.block_count();
        if block_count < 64 {
            return Err(Error::NoSpace);
        }
        if dev.read_only() {
            return Err(Error::ReadOnly);
        }
        // One node per 16 KiB of disk, at least 256.
        let node_count = ((block_count / 4).clamp(256, u32::MAX as u64 / 2) as u32).next_multiple_of(NODES_PER_BLOCK as u32);
        let bitmap_blocks = block_count.div_ceil(BLOCK_SIZE as u64 * 8) as u32;
        let nodes_blocks = node_count / NODES_PER_BLOCK as u32;
        let bitmap_start = 1;
        let nodes_start = bitmap_start + bitmap_blocks;
        let data_start = nodes_start + nodes_blocks;
        if data_start as u64 + 8 > block_count {
            return Err(Error::NoSpace);
        }
        let now = clock.now();
        let sb = Superblock {
            block_count,
            node_count,
            bitmap_start,
            bitmap_blocks,
            nodes_start,
            nodes_blocks,
            data_start,
            free_blocks: block_count - data_start as u64,
            free_nodes: node_count - 2,
            label: String::from(label),
            created: now,
        };
        let zero = [0u8; BLOCK_SIZE];
        for b in nodes_start..data_start {
            dev.write_block(b as u64, &zero)?;
        }
        let mut bitmap = vec![0u8; bitmap_blocks as usize * BLOCK_SIZE];
        for b in 0..data_start as u64 {
            bitmap[(b / 8) as usize] |= 1 << (b % 8);
        }
        // Blocks past the end of the device are never free.
        for b in block_count..(bitmap.len() as u64 * 8) {
            bitmap[(b / 8) as usize] |= 1 << (b % 8);
        }
        let mut fs = Fs { dev, sb, bitmap, alloc_hint: data_start as u64, node_hint: 2, clock };
        for i in 0..bitmap_blocks as u64 {
            fs.write_bitmap_block(i)?;
        }
        let mut root = Node::empty(Kind::Dir, ROOT, now);
        root.flags = FLAG_SYSTEM;
        fs.write_node(ROOT, &root)?;
        fs.write_superblock()?;
        fs.dev.flush()?;
        Ok(fs)
    }

    /// Mounts an existing file system.
    pub fn mount(mut dev: D, clock: &'static dyn Clock) -> Result<Fs<D>> {
        let mut b = [0u8; BLOCK_SIZE];
        dev.read_block(0, &mut b)?;
        let sb = Superblock::decode(&b)?;
        if sb.block_count > dev.block_count() || sb.data_start as u64 >= sb.block_count {
            return Err(Error::Corrupt);
        }
        let mut bitmap = vec![0u8; sb.bitmap_blocks as usize * BLOCK_SIZE];
        for i in 0..sb.bitmap_blocks as usize {
            dev.read_block(sb.bitmap_start as u64 + i as u64, &mut b)?;
            bitmap[i * BLOCK_SIZE..(i + 1) * BLOCK_SIZE].copy_from_slice(&b);
        }
        let alloc_hint = sb.data_start as u64;
        let mut sb = sb;
        // The bitmap is the truth; the counter may be stale after a crash.
        let used: u64 = bitmap.iter().map(|b| b.count_ones() as u64).sum();
        sb.free_blocks = (bitmap.len() as u64 * 8).saturating_sub(used);
        Ok(Fs { dev, sb, bitmap, alloc_hint, node_hint: 2, clock })
    }

    /// Does the block hold a JustFS superblock?
    pub fn probe(block0: &Block) -> bool {
        Superblock::decode(block0).is_ok()
    }

    pub fn superblock(&self) -> &Superblock {
        &self.sb
    }

    pub fn device(&mut self) -> &mut D {
        &mut self.dev
    }

    pub fn into_device(self) -> D {
        self.dev
    }

    pub fn read_only(&self) -> bool {
        self.dev.read_only()
    }

    pub fn set_label(&mut self, label: &str) -> Result<()> {
        self.check_writable()?;
        self.sb.label = String::from(label);
        self.write_superblock()
    }

    /// Total and free space in bytes.
    pub fn space(&self) -> (u64, u64) {
        let total = (self.sb.block_count - self.sb.data_start as u64) * BLOCK_SIZE as u64;
        (total, self.sb.free_blocks * BLOCK_SIZE as u64)
    }

    pub fn sync(&mut self) -> Result<()> {
        if !self.read_only() {
            self.write_superblock()?;
        }
        self.dev.flush()
    }

    /// Like [`Fs::sync`], but only hands everything to the device
    /// ([`BlockDevice::write_back`]): the caller then has the device make it
    /// durable, without holding the file system meanwhile.
    pub fn write_back(&mut self) -> Result<()> {
        if !self.read_only() {
            self.write_superblock()?;
        }
        self.dev.write_back()
    }

    fn check_writable(&self) -> Result<()> {
        if self.read_only() {
            Err(Error::ReadOnly)
        } else {
            Ok(())
        }
    }

    // ------------------------------------------------------------------
    // Superblock, bitmap and nodes on disk

    fn write_superblock(&mut self) -> Result<()> {
        let mut b = [0u8; BLOCK_SIZE];
        self.sb.encode(&mut b);
        self.dev.write_block(0, &b)
    }

    fn write_bitmap_block(&mut self, index: u64) -> Result<()> {
        let mut b = [0u8; BLOCK_SIZE];
        let at = index as usize * BLOCK_SIZE;
        b.copy_from_slice(&self.bitmap[at..at + BLOCK_SIZE]);
        self.dev.write_block(self.sb.bitmap_start as u64 + index, &b)
    }

    fn is_used(&self, block: u64) -> bool {
        self.bitmap[(block / 8) as usize] & (1 << (block % 8)) != 0
    }

    fn set_used(&mut self, start: u64, len: u64, used: bool) -> Result<()> {
        let mut dirty_first = u64::MAX;
        let mut dirty_last = 0;
        for b in start..start + len {
            let byte = &mut self.bitmap[(b / 8) as usize];
            let bit = 1 << (b % 8);
            if used {
                debug_assert!(*byte & bit == 0);
                *byte |= bit;
            } else {
                debug_assert!(*byte & bit != 0);
                *byte &= !bit;
            }
            let blk = b / (BLOCK_SIZE as u64 * 8);
            dirty_first = dirty_first.min(blk);
            dirty_last = dirty_last.max(blk);
        }
        if used {
            self.sb.free_blocks -= len;
        } else {
            self.sb.free_blocks += len;
        }
        if dirty_first != u64::MAX {
            for blk in dirty_first..=dirty_last {
                self.write_bitmap_block(blk)?;
            }
        }
        Ok(())
    }

    /// Allocates up to `want` contiguous blocks, preferably at `near`.
    fn alloc_run(&mut self, near: Option<u64>, want: u64) -> Result<Extent> {
        if self.sb.free_blocks == 0 {
            return Err(Error::NoSpace);
        }
        let total = self.sb.block_count;
        let data = self.sb.data_start as u64;
        let want = want.clamp(1, u32::MAX as u64);
        // Right after the previous extent, if free.
        if let Some(n) = near {
            if n >= data && n < total && !self.is_used(n) {
                let mut len = 1;
                while len < want && n + len < total && !self.is_used(n + len) {
                    len += 1;
                }
                self.set_used(n, len, true)?;
                return Ok(Extent { start: n as u32, len: len as u32 });
            }
        }
        // First fit from the hint, wrapping once.
        let mut b = self.alloc_hint.clamp(data, total - 1);
        let mut scanned = 0;
        while scanned < total - data {
            if !self.is_used(b) {
                let mut len = 1;
                while len < want && b + len < total && !self.is_used(b + len) {
                    len += 1;
                }
                self.set_used(b, len, true)?;
                self.alloc_hint = b + len;
                return Ok(Extent { start: b as u32, len: len as u32 });
            }
            b += 1;
            scanned += 1;
            if b >= total {
                b = data;
            }
        }
        Err(Error::NoSpace)
    }

    fn node_location(&self, id: NodeId) -> Result<(u64, usize)> {
        if id == 0 || id >= self.sb.node_count {
            return Err(Error::Corrupt);
        }
        let block = self.sb.nodes_start as u64 + (id as usize / NODES_PER_BLOCK) as u64;
        Ok((block, (id as usize % NODES_PER_BLOCK) * NODE_SIZE))
    }

    pub fn node(&mut self, id: NodeId) -> Result<Node> {
        let (block, off) = self.node_location(id)?;
        let mut b = [0u8; BLOCK_SIZE];
        self.dev.read_block(block, &mut b)?;
        let node = Node::decode(&b[off..off + NODE_SIZE]);
        if node.kind == Kind::Free {
            return Err(Error::NotFound);
        }
        Ok(node)
    }

    fn write_node(&mut self, id: NodeId, node: &Node) -> Result<()> {
        let (block, off) = self.node_location(id)?;
        let mut b = [0u8; BLOCK_SIZE];
        self.dev.read_block(block, &mut b)?;
        node.encode(&mut b[off..off + NODE_SIZE]);
        self.dev.write_block(block, &b)
    }

    /// A free node, the lowest one (the caller writes it, or it stays free).
    fn alloc_node(&mut self) -> Result<NodeId> {
        let mut b = [0u8; BLOCK_SIZE];
        let first = self.node_hint.max(2) as usize;
        for blk in first / NODES_PER_BLOCK..self.sb.nodes_blocks as usize {
            self.dev.read_block(self.sb.nodes_start as u64 + blk as u64, &mut b)?;
            for slot in 0..NODES_PER_BLOCK {
                let id = blk * NODES_PER_BLOCK + slot;
                if id >= first && (id as u64) < self.sb.node_count as u64 && b[slot * NODE_SIZE] == Kind::Free as u8 {
                    self.sb.free_nodes = self.sb.free_nodes.saturating_sub(1);
                    self.node_hint = id as NodeId + 1;
                    return Ok(id as NodeId);
                }
            }
        }
        self.node_hint = self.sb.node_count;
        Err(Error::NoNodes)
    }

    /// Node `id` became free.
    fn freed_node(&mut self, id: NodeId) {
        self.sb.free_nodes += 1;
        self.node_hint = self.node_hint.min(id);
    }

    // ------------------------------------------------------------------
    // Extents

    fn load_extents(&mut self, node: &Node) -> Result<Vec<Extent>> {
        let n = node.extent_count as usize;
        let mut out = Vec::with_capacity(n);
        out.extend_from_slice(&node.extents[..n.min(DIRECT_EXTENTS)]);
        if n > DIRECT_EXTENTS {
            if node.indirect == 0 {
                return Err(Error::Corrupt);
            }
            let mut b = [0u8; BLOCK_SIZE];
            self.dev.read_block(node.indirect as u64, &mut b)?;
            for i in 0..n - DIRECT_EXTENTS {
                out.push(Extent { start: le32(&b, i * 8), len: le32(&b, i * 8 + 4) });
            }
        }
        Ok(out)
    }

    fn store_extents(&mut self, node: &mut Node, extents: &[Extent]) -> Result<()> {
        if extents.len() > MAX_EXTENTS {
            return Err(Error::TooFragmented);
        }
        node.extents = [Extent::default(); DIRECT_EXTENTS];
        let direct = extents.len().min(DIRECT_EXTENTS);
        node.extents[..direct].copy_from_slice(&extents[..direct]);
        node.extent_count = extents.len() as u16;
        if extents.len() > DIRECT_EXTENTS {
            if node.indirect == 0 {
                node.indirect = self.alloc_run(None, 1)?.start;
            }
            let mut b = [0u8; BLOCK_SIZE];
            for (i, e) in extents[DIRECT_EXTENTS..].iter().enumerate() {
                put32(&mut b, i * 8, e.start);
                put32(&mut b, i * 8 + 4, e.len);
            }
            self.dev.write_block(node.indirect as u64, &b)?;
        } else if node.indirect != 0 {
            self.set_used(node.indirect as u64, 1, false)?;
            node.indirect = 0;
        }
        Ok(())
    }

    fn blocks_of(extents: &[Extent]) -> u64 {
        extents.iter().map(|e| e.len as u64).sum()
    }

    /// Disk block holding file block `index`.
    fn map_block(extents: &[Extent], mut index: u64) -> Option<u64> {
        for e in extents {
            if index < e.len as u64 {
                return Some(e.start as u64 + index);
            }
            index -= e.len as u64;
        }
        None
    }

    /// Grows or shrinks the allocation to exactly `blocks` blocks.
    fn resize_blocks(&mut self, node: &mut Node, extents: &mut Vec<Extent>, blocks: u64) -> Result<()> {
        let mut have = Self::blocks_of(extents);
        while have < blocks {
            let near = extents.last().map(|e| e.start as u64 + e.len as u64);
            let run = match self.alloc_run(near, blocks - have) {
                Ok(run) => run,
                Err(e) => {
                    self.store_extents(node, extents)?;
                    return Err(e);
                }
            };
            have += run.len as u64;
            match extents.last_mut() {
                Some(last) if last.start as u64 + last.len as u64 == run.start as u64 => last.len += run.len,
                _ => {
                    if extents.len() == MAX_EXTENTS {
                        self.set_used(run.start as u64, run.len as u64, false)?;
                        self.store_extents(node, extents)?;
                        return Err(Error::TooFragmented);
                    }
                    extents.push(run);
                }
            }
        }
        while have > blocks {
            let last = extents.last_mut().ok_or(Error::Corrupt)?;
            let cut = (have - blocks).min(last.len as u64);
            last.len -= cut as u32;
            let freed = last.start as u64 + last.len as u64;
            if last.len == 0 {
                extents.pop();
            }
            self.set_used(freed, cut, false)?;
            have -= cut;
        }
        self.store_extents(node, extents)
    }

    // ------------------------------------------------------------------
    // File contents

    /// Reads from `offset`; returns the number of bytes read.
    pub fn read(&mut self, id: NodeId, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let node = self.node(id)?;
        self.read_node(&node, offset, buf)
    }

    fn read_node(&mut self, node: &Node, offset: u64, buf: &mut [u8]) -> Result<usize> {
        if offset >= node.size {
            return Ok(0);
        }
        let len = (buf.len() as u64).min(node.size - offset) as usize;
        let extents = self.load_extents(node)?;
        let mut done = 0;
        let mut b = [0u8; BLOCK_SIZE];
        while done < len {
            let pos = offset + done as u64;
            let in_block = (pos % BLOCK_SIZE as u64) as usize;
            let n = (BLOCK_SIZE - in_block).min(len - done);
            let disk = Self::map_block(&extents, pos / BLOCK_SIZE as u64).ok_or(Error::Corrupt)?;
            self.dev.read_block(disk, &mut b)?;
            buf[done..done + n].copy_from_slice(&b[in_block..in_block + n]);
            done += n;
        }
        Ok(len)
    }

    /// Reads a whole file.
    pub fn read_all(&mut self, id: NodeId) -> Result<Vec<u8>> {
        let node = self.node(id)?;
        if node.is_dir() {
            return Err(Error::IsDir);
        }
        let mut out = vec![0u8; node.size as usize];
        self.read_node(&node, 0, &mut out)?;
        Ok(out)
    }

    /// Writes at `offset`, growing the file as needed.
    pub fn write(&mut self, id: NodeId, offset: u64, data: &[u8]) -> Result<usize> {
        self.check_writable()?;
        let mut node = self.node(id)?;
        if node.is_dir() {
            return Err(Error::IsDir);
        }
        self.write_node_data(id, &mut node, offset, data)?;
        Ok(data.len())
    }

    fn write_node_data(&mut self, id: NodeId, node: &mut Node, offset: u64, data: &[u8]) -> Result<()> {
        let end = offset + data.len() as u64;
        let mut extents = self.load_extents(node)?;
        let need = end.div_ceil(BLOCK_SIZE as u64);
        if need > Self::blocks_of(&extents) {
            let old_size = node.size;
            self.resize_blocks(node, &mut extents, need)?;
            // Zero the gap between the old end and the write.
            if offset > old_size {
                self.zero_range(&extents, old_size, offset)?;
            }
        }
        let mut done = 0;
        let mut b = [0u8; BLOCK_SIZE];
        while done < data.len() {
            let pos = offset + done as u64;
            let in_block = (pos % BLOCK_SIZE as u64) as usize;
            let n = (BLOCK_SIZE - in_block).min(data.len() - done);
            let disk = Self::map_block(&extents, pos / BLOCK_SIZE as u64).ok_or(Error::Corrupt)?;
            if n != BLOCK_SIZE {
                if pos - in_block as u64 >= node.size {
                    b.fill(0);
                } else {
                    self.dev.read_block(disk, &mut b)?;
                }
            }
            b[in_block..in_block + n].copy_from_slice(&data[done..done + n]);
            self.dev.write_block(disk, &b)?;
            done += n;
        }
        node.size = node.size.max(end);
        node.modified = self.clock.now();
        self.write_node(id, node)
    }

    fn zero_range(&mut self, extents: &[Extent], from: u64, to: u64) -> Result<()> {
        let mut b = [0u8; BLOCK_SIZE];
        let mut pos = from;
        while pos < to {
            let in_block = (pos % BLOCK_SIZE as u64) as usize;
            let n = ((BLOCK_SIZE - in_block) as u64).min(to - pos) as usize;
            let disk = Self::map_block(extents, pos / BLOCK_SIZE as u64).ok_or(Error::Corrupt)?;
            if in_block == 0 && n == BLOCK_SIZE {
                b.fill(0);
            } else {
                self.dev.read_block(disk, &mut b)?;
                b[in_block..in_block + n].fill(0);
            }
            self.dev.write_block(disk, &b)?;
            pos += n as u64;
        }
        Ok(())
    }

    /// Sets the size of a file, freeing or zero-filling as needed.
    pub fn truncate(&mut self, id: NodeId, size: u64) -> Result<()> {
        self.check_writable()?;
        let mut node = self.node(id)?;
        if node.is_dir() {
            return Err(Error::IsDir);
        }
        let mut extents = self.load_extents(&node)?;
        let blocks = size.div_ceil(BLOCK_SIZE as u64);
        if size > node.size {
            self.resize_blocks(&mut node, &mut extents, blocks)?;
            self.zero_range(&extents, node.size, size)?;
        } else {
            self.resize_blocks(&mut node, &mut extents, blocks)?;
            // Clear the tail of the last block so a later grow reads zeros.
            if !size.is_multiple_of(BLOCK_SIZE as u64) {
                let end = blocks * BLOCK_SIZE as u64;
                self.zero_range(&extents, size, end)?;
            }
        }
        node.size = size;
        node.modified = self.clock.now();
        self.write_node(id, &node)
    }

    /// Replaces a file's contents.
    pub fn write_all(&mut self, id: NodeId, data: &[u8]) -> Result<()> {
        self.truncate(id, 0)?;
        let mut node = self.node(id)?;
        self.write_node_data(id, &mut node, 0, data)
    }

    // ------------------------------------------------------------------
    // Directories

    /// The directory `dir`'s node.
    fn dir_node(&mut self, dir: NodeId) -> Result<Node> {
        let node = self.node(dir)?;
        if !node.is_dir() {
            return Err(Error::NotDir);
        }
        Ok(node)
    }

    /// Goes through the entry slots of a directory, empty ones too, a block
    /// at a time, until `f` returns true for one; returns its index.
    fn scan_dir(&mut self, node: &Node, mut f: impl FnMut(&[u8; DIRENT_SIZE]) -> bool) -> Result<Option<usize>> {
        let extents = self.load_extents(node)?;
        let mut b = [0u8; BLOCK_SIZE];
        let mut pos = 0;
        while pos < node.size {
            let disk = Self::map_block(&extents, pos / BLOCK_SIZE as u64).ok_or(Error::Corrupt)?;
            self.dev.read_block(disk, &mut b)?;
            let n = (node.size - pos).min(BLOCK_SIZE as u64) as usize;
            if let Some(i) = b[..n].as_chunks::<DIRENT_SIZE>().0.iter().position(&mut f) {
                return Ok(Some(pos as usize / DIRENT_SIZE + i));
            }
            pos += BLOCK_SIZE as u64;
        }
        Ok(None)
    }

    /// Goes through a directory's entries, each name put together from the
    /// slots it takes, until `f` returns true for one; returns its first
    /// slot. Slots that go on a name no entry starts are passed over.
    fn scan_entries(&mut self, node: &Node, mut f: impl FnMut(NodeId, Kind, &[u8]) -> bool) -> Result<Option<usize>> {
        let mut name = [0u8; MAX_NAME];
        // The entry whose name is being put together: first slot, node,
        // kind, name length and bytes so far.
        let mut open: Option<(usize, NodeId, Kind, usize, usize)> = None;
        let mut slot = 0;
        let mut hit = None;
        self.scan_dir(node, |e| {
            let at = slot;
            slot += 1;
            let id = le32(e, 0);
            if id == CONT {
                let Some((first, id, kind, len, have)) = open.as_mut() else { return false };
                let n = (*len - *have).min(CONT_NAME);
                name[*have..*have + n].copy_from_slice(&e[4..4 + n]);
                *have += n;
                if *have < *len {
                    return false;
                }
                let (first, id, kind, len) = (*first, *id, *kind, *len);
                open = None;
                if f(id, kind, &name[..len]) {
                    hit = Some(first);
                    return true;
                }
                return false;
            }
            open = None;
            if id == 0 {
                return false;
            }
            let len = e[5] as usize;
            let n = len.min(HEAD_NAME);
            name[..n].copy_from_slice(&e[8..8 + n]);
            let kind = Kind::from_u8(e[4]);
            if n < len {
                open = Some((at, id, kind, len, n));
                return false;
            }
            if f(id, kind, &name[..len]) {
                hit = Some(at);
                return true;
            }
            false
        })?;
        Ok(hit)
    }

    /// Lists a directory.
    pub fn read_dir(&mut self, dir: NodeId) -> Result<Vec<DirEntry>> {
        let node = self.dir_node(dir)?;
        let mut out = Vec::new();
        self.scan_entries(&node, |id, kind, name| {
            out.push(DirEntry { name: String::from_utf8_lossy(name).into_owned(), node: id, kind });
            false
        })?;
        Ok(out)
    }

    /// Looks up one name in a directory.
    pub fn find(&mut self, dir: NodeId, name: &str) -> Result<NodeId> {
        self.find_entry(dir, name).map(|(id, _)| id)
    }

    /// Looks up one name in a directory; returns its node and kind. Names
    /// compare as [`same_name`] does, on the bytes in place.
    fn find_entry(&mut self, dir: NodeId, name: &str) -> Result<(NodeId, Kind)> {
        let node = self.dir_node(dir)?;
        let want = name.as_bytes();
        let mut found = (0, Kind::Free);
        self.scan_entries(&node, |id, kind, have| {
            let hit = have.eq_ignore_ascii_case(want);
            if hit {
                found = (id, kind);
            }
            hit
        })?
        .map(|_| found)
        .ok_or(Error::NotFound)
    }

    /// Whether a directory has no entries.
    fn dir_empty(&mut self, dir: NodeId) -> Result<bool> {
        let node = self.dir_node(dir)?;
        Ok(self.scan_dir(&node, |e| le32(e, 0) != 0)?.is_none())
    }

    /// Resolves a `/`-separated path from the root, following symbolic
    /// links. `""` and `"/"` are the root.
    pub fn lookup(&mut self, path: &str) -> Result<NodeId> {
        let mut budget = MAX_LINKS;
        self.walk(ROOT, path, true, &mut budget)
    }

    /// Like [`Fs::lookup`], but a link at the end of the path is returned
    /// itself rather than followed (links on the way are still followed).
    pub fn lookup_link(&mut self, path: &str) -> Result<NodeId> {
        let mut budget = MAX_LINKS;
        self.walk(ROOT, path, false, &mut budget)
    }

    /// Walks `path` from the folder `from` (from the root if the path starts
    /// with `/`), spending one of `budget` on each link it follows.
    fn walk(&mut self, from: NodeId, path: &str, follow_last: bool, budget: &mut u32) -> Result<NodeId> {
        let mut id = if path.starts_with('/') { ROOT } else { from };
        let mut parts = path.split('/').filter(|p| !p.is_empty() && *p != ".").peekable();
        while let Some(part) = parts.next() {
            if part == ".." {
                id = self.node(id)?.parent;
                continue;
            }
            let dir = id;
            let (child, kind) = self.find_entry(dir, part)?;
            id = child;
            if kind == Kind::Link && (parts.peek().is_some() || follow_last) {
                if *budget == 0 {
                    return Err(Error::Loop);
                }
                *budget -= 1;
                let target = self.read_link(child)?;
                id = self.walk(dir, &target, true, budget)?;
            }
        }
        Ok(id)
    }

    /// Where a symbolic link points.
    pub fn read_link(&mut self, id: NodeId) -> Result<String> {
        let node = self.node(id)?;
        if !node.is_link() {
            return Err(Error::InvalidName);
        }
        let mut raw = vec![0u8; node.size as usize];
        self.read_node(&node, 0, &mut raw)?;
        Ok(String::from_utf8_lossy(&raw).into_owned())
    }

    /// Creates a symbolic link at `path` pointing to `target`.
    pub fn symlink(&mut self, path: &str, target: &str) -> Result<NodeId> {
        if target.is_empty() || target.len() > 4096 {
            return Err(Error::InvalidName);
        }
        let (dir, name) = self.lookup_parent(path)?;
        let id = self.create(dir, name, Kind::Link)?;
        self.write_all(id, target.as_bytes())?;
        Ok(id)
    }

    /// Resolves the parent directory of a path and returns it with the last name.
    pub fn lookup_parent<'p>(&mut self, path: &'p str) -> Result<(NodeId, &'p str)> {
        let path = path.trim_end_matches('/');
        let (dir, name) = match path.rfind('/') {
            Some(i) => (&path[..i], &path[i + 1..]),
            None => ("", path),
        };
        valid_name(name)?;
        Ok((self.lookup(dir)?, name))
    }

    fn dir_insert(&mut self, dir: NodeId, name: &str, id: NodeId, kind: Kind) -> Result<()> {
        let mut node = self.node(dir)?;
        let name = name.as_bytes();
        if name.len() > MAX_NAME {
            return Err(Error::NameTooLong);
        }
        let need = slots_for(name.len());
        let mut entry = vec![0u8; need * DIRENT_SIZE];
        put32(&mut entry, 0, id);
        entry[4] = kind as u8;
        entry[5] = name.len() as u8;
        let head = name.len().min(HEAD_NAME);
        entry[8..8 + head].copy_from_slice(&name[..head]);
        for (i, part) in name[head..].chunks(CONT_NAME).enumerate() {
            let at = (i + 1) * DIRENT_SIZE;
            put32(&mut entry, at, CONT);
            entry[at + 4..at + 4 + part.len()].copy_from_slice(part);
        }
        // The first run of free slots long enough, else the end (with the
        // free slots there).
        let mut run = 0;
        let found = self.scan_dir(&node, |e| {
            run = if le32(e, 0) == 0 { run + 1 } else { 0 };
            run == need
        })?;
        let slot = match found {
            Some(last) => last + 1 - need,
            None => node.size as usize / DIRENT_SIZE - run,
        };
        self.write_node_data(dir, &mut node, (slot * DIRENT_SIZE) as u64, &entry)
    }

    fn dir_remove(&mut self, dir: NodeId, id: NodeId) -> Result<()> {
        let mut node = self.node(dir)?;
        let mut len = 0;
        let slot = self
            .scan_dir(&node, |e| {
                let hit = le32(e, 0) == id;
                if hit {
                    len = e[5] as usize;
                }
                hit
            })?
            .ok_or(Error::NotFound)?;
        let zero = vec![0u8; slots_for(len) * DIRENT_SIZE];
        self.write_node_data(dir, &mut node, (slot * DIRENT_SIZE) as u64, &zero)
    }

    /// Creates an empty file or directory in `dir`.
    pub fn create(&mut self, dir: NodeId, name: &str, kind: Kind) -> Result<NodeId> {
        self.check_writable()?;
        valid_name(name)?;
        if !self.node(dir)?.is_dir() {
            return Err(Error::NotDir);
        }
        if self.find(dir, name).is_ok() {
            return Err(Error::Exists);
        }
        let id = self.alloc_node()?;
        let node = Node::empty(kind, dir, self.clock.now());
        self.write_node(id, &node)?;
        if let Err(e) = self.dir_insert(dir, name, id, kind) {
            let mut free = node;
            free.kind = Kind::Free;
            self.write_node(id, &free)?;
            self.freed_node(id);
            return Err(e);
        }
        Ok(id)
    }

    /// Creates every missing directory of `path`, following links on the
    /// way as `mkdir -p` does (Wine reaches its C: drive through one).
    pub fn create_dirs(&mut self, path: &str) -> Result<NodeId> {
        let mut id = ROOT;
        let mut budget = MAX_LINKS;
        for part in path.split('/').filter(|p| !p.is_empty() && *p != ".") {
            if part == ".." {
                id = self.node(id)?.parent;
                continue;
            }
            id = match self.find_entry(id, part) {
                Ok((child, kind)) => {
                    let child = if kind == Kind::Link {
                        if budget == 0 {
                            return Err(Error::Loop);
                        }
                        budget -= 1;
                        let target = self.read_link(child)?;
                        self.walk(id, &target, true, &mut budget)?
                    } else {
                        child
                    };
                    if !self.node(child)?.is_dir() {
                        return Err(Error::NotDir);
                    }
                    child
                }
                Err(Error::NotFound) => self.create(id, part, Kind::Dir)?,
                Err(e) => return Err(e),
            };
        }
        Ok(id)
    }

    /// Removes a file or an empty directory.
    pub fn remove(&mut self, dir: NodeId, name: &str) -> Result<()> {
        self.check_writable()?;
        let id = self.find(dir, name)?;
        let mut node = self.node(id)?;
        if node.is_dir() && !self.dir_empty(id)? {
            return Err(Error::NotEmpty);
        }
        let mut extents = self.load_extents(&node)?;
        self.resize_blocks(&mut node, &mut extents, 0)?;
        self.dir_remove(dir, id)?;
        node.kind = Kind::Free;
        node.size = 0;
        self.write_node(id, &node)?;
        self.freed_node(id);
        Ok(())
    }

    /// Moves and/or renames an entry.
    pub fn rename(&mut self, dir: NodeId, name: &str, new_dir: NodeId, new_name: &str) -> Result<()> {
        self.check_writable()?;
        valid_name(new_name)?;
        let id = self.find(dir, name)?;
        if !self.node(new_dir)?.is_dir() {
            return Err(Error::NotDir);
        }
        if let Ok(existing) = self.find(new_dir, new_name) {
            if existing != id {
                return Err(Error::Exists);
            }
        }
        // A directory cannot move inside itself.
        let mut up = new_dir;
        while up != ROOT {
            if up == id {
                return Err(Error::InvalidName);
            }
            up = self.node(up)?.parent;
        }
        let mut node = self.node(id)?;
        self.dir_remove(dir, id)?;
        self.dir_insert(new_dir, new_name, id, node.kind)?;
        node.parent = new_dir;
        self.write_node(id, &node)
    }

    pub fn set_flags(&mut self, id: NodeId, flags: u8) -> Result<()> {
        self.check_writable()?;
        let mut node = self.node(id)?;
        node.flags = flags;
        self.write_node(id, &node)
    }

    /// Gives a node Unix permission bits (`mode & 0o7777`).
    pub fn set_mode(&mut self, id: NodeId, mode: u16) -> Result<()> {
        self.check_writable()?;
        let mut node = self.node(id)?;
        node.mode = MODE_SET | (mode & 0o7777);
        self.write_node(id, &node)
    }

    // ------------------------------------------------------------------
    // Path helpers

    /// Creates or replaces the file at `path` with `data`.
    pub fn write_file(&mut self, path: &str, data: &[u8]) -> Result<NodeId> {
        let (dir, name) = self.lookup_parent(path)?;
        let id = match self.find_entry(dir, name) {
            Ok((_, Kind::Link)) => self.lookup(path)?,
            Ok((id, _)) => id,
            Err(Error::NotFound) => self.create(dir, name, Kind::File)?,
            Err(e) => return Err(e),
        };
        if self.node(id)?.is_dir() {
            return Err(Error::IsDir);
        }
        self.write_all(id, data)?;
        Ok(id)
    }

    pub fn read_file(&mut self, path: &str) -> Result<Vec<u8>> {
        let id = self.lookup(path)?;
        self.read_all(id)
    }

    /// Checks the allocation bitmap against the nodes; returns the number of
    /// problems found (used by tests and `justfs check`).
    pub fn check(&mut self) -> Result<usize> {
        let mut seen = vec![0u8; self.bitmap.len()];
        let mut problems = 0;
        let mark = |seen: &mut Vec<u8>, b: u64| -> usize {
            let byte = &mut seen[(b / 8) as usize];
            let bit = 1 << (b % 8);
            let dup = (*byte & bit != 0) as usize;
            *byte |= bit;
            dup
        };
        for b in 0..self.sb.data_start as u64 {
            mark(&mut seen, b);
        }
        // The node table is read a block at a time, not a node at a time.
        let mut table = [0u8; BLOCK_SIZE];
        for id in 1..self.sb.node_count {
            if id == 1 || (id as usize).is_multiple_of(NODES_PER_BLOCK) {
                self.dev.read_block(self.node_location(id)?.0, &mut table)?;
            }
            let off = (id as usize % NODES_PER_BLOCK) * NODE_SIZE;
            let node = Node::decode(&table[off..off + NODE_SIZE]);
            if node.kind == Kind::Free {
                continue;
            }
            let extents = self.load_extents(&node)?;
            if node.indirect != 0 {
                problems += mark(&mut seen, node.indirect as u64);
            }
            for e in &extents {
                for b in e.start as u64..e.start as u64 + e.len as u64 {
                    problems += mark(&mut seen, b);
                }
            }
            if Self::blocks_of(&extents) != node.size.div_ceil(BLOCK_SIZE as u64) {
                problems += 1;
            }
        }
        for b in self.sb.block_count..(seen.len() as u64 * 8) {
            mark(&mut seen, b);
        }
        let mut used = 0u64;
        for (a, b) in seen.iter().zip(self.bitmap.iter()) {
            problems += (a ^ b).count_ones() as usize;
            used += b.count_ones() as u64;
        }
        let free = self.bitmap.len() as u64 * 8 - used;
        if free != self.sb.free_blocks {
            problems += 1;
        }
        Ok(problems)
    }
}

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}
fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn le64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}
fn put16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_le_bytes());
}
fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
fn put64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

/// A block device backed by a byte vector (boot packs, tests).
pub struct MemDisk {
    pub data: Vec<u8>,
    pub read_only: bool,
}

impl MemDisk {
    pub fn new(blocks: usize) -> MemDisk {
        MemDisk { data: vec![0; blocks * BLOCK_SIZE], read_only: false }
    }
}

impl BlockDevice for MemDisk {
    fn read_block(&mut self, block: u64, buf: &mut Block) -> Result<()> {
        let at = block as usize * BLOCK_SIZE;
        buf.copy_from_slice(self.data.get(at..at + BLOCK_SIZE).ok_or(Error::Io)?);
        Ok(())
    }
    fn write_block(&mut self, block: u64, buf: &Block) -> Result<()> {
        if self.read_only {
            return Err(Error::ReadOnly);
        }
        let at = block as usize * BLOCK_SIZE;
        self.data.get_mut(at..at + BLOCK_SIZE).ok_or(Error::Io)?.copy_from_slice(buf);
        Ok(())
    }
    fn block_count(&self) -> u64 {
        (self.data.len() / BLOCK_SIZE) as u64
    }
    fn read_only(&self) -> bool {
        self.read_only
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fs(blocks: usize) -> Fs<MemDisk> {
        Fs::format(MemDisk::new(blocks), "Test", &NoClock).unwrap()
    }

    #[test]
    fn symbolic_links() {
        let mut fs = fs(512);
        fs.create_dirs("usr/lib/x86_64-linux-gnu").unwrap();
        fs.write_file("usr/lib/x86_64-linux-gnu/libz.so.1.3", b"zlib").unwrap();
        fs.symlink("usr/lib/x86_64-linux-gnu/libz.so.1", "libz.so.1.3").unwrap();
        fs.symlink("lib", "usr/lib").unwrap();
        fs.create_dirs("usr/lib64").unwrap();
        fs.symlink("usr/lib64/libz.so", "../lib/x86_64-linux-gnu/libz.so.1").unwrap();
        fs.symlink("abs", "/usr/lib/x86_64-linux-gnu").unwrap();
        // Links in the middle and at the end of paths, relative and absolute.
        assert_eq!(fs.read_file("lib/x86_64-linux-gnu/libz.so.1").unwrap(), b"zlib");
        assert_eq!(fs.read_file("/usr/lib64/libz.so").unwrap(), b"zlib");
        assert_eq!(fs.read_file("abs/libz.so.1.3").unwrap(), b"zlib");
        let dir = fs.lookup("lib").unwrap();
        assert!(fs.node(dir).unwrap().is_dir());
        let link = fs.lookup_link("lib").unwrap();
        assert!(fs.node(link).unwrap().is_link());
        assert_eq!(fs.read_link(link).unwrap(), "usr/lib");
        // Writing through a link changes the target.
        fs.write_file("lib/x86_64-linux-gnu/libz.so.1", b"zlib 2").unwrap();
        assert_eq!(fs.read_file("usr/lib/x86_64-linux-gnu/libz.so.1.3").unwrap(), b"zlib 2");
        // Loops end with an error, dangling links say not found.
        fs.symlink("a", "b").unwrap();
        fs.symlink("b", "a").unwrap();
        assert_eq!(fs.lookup("a"), Err(Error::Loop));
        fs.symlink("gone", "nowhere").unwrap();
        assert_eq!(fs.lookup("gone"), Err(Error::NotFound));
        // Folders are made through links on the way (Wine's "c:").
        fs.create_dirs("prefix/drive_c").unwrap();
        fs.symlink("prefix/c:", "drive_c").unwrap();
        fs.create_dirs("prefix/c:/users/root").unwrap();
        let made = fs.lookup("prefix/drive_c/users/root").unwrap();
        assert!(fs.node(made).unwrap().is_dir());
        let link = fs.lookup_link("prefix/c:").unwrap();
        assert!(fs.node(link).unwrap().is_link());
        assert_eq!(fs.create_dirs("lib/x86_64-linux-gnu/libz.so.1/x"), Err(Error::NotDir));
        // Removing a link leaves the target.
        fs.remove(ROOT, "lib").unwrap();
        assert!(fs.lookup("usr/lib").is_ok());
        assert_eq!(fs.check().unwrap(), 0);
    }

    #[test]
    fn long_names() {
        let mut fs = fs(1024);
        let long = |i: usize| alloc::format!("amd64_microsoft.windows.common-controls_6595b64144ccf1df_6.0.{i}_none_deadbeef");
        fs.create_dirs("winsxs").unwrap();
        for i in 0..40 {
            fs.create_dirs(&alloc::format!("winsxs/{}", long(i))).unwrap();
        }
        let max = "x".repeat(MAX_NAME);
        fs.write_file(&alloc::format!("winsxs/{max}"), b"255").unwrap();
        assert_eq!(fs.write_file(&alloc::format!("winsxs/{max}y"), b""), Err(Error::NameTooLong));
        assert_eq!(fs.read_file(&alloc::format!("winsxs/{}", max.to_uppercase())).unwrap(), b"255");
        let dir = fs.lookup("winsxs").unwrap();
        let mut names: Vec<String> = fs.read_dir(dir).unwrap().into_iter().map(|e| e.name).collect();
        names.sort();
        assert_eq!(names.len(), 41);
        assert!(names.contains(&long(7)) && names.contains(&max));
        // Removing frees all the slots; a short name and a long one fit again.
        for i in (0..40).step_by(2) {
            fs.remove(dir, &long(i)).unwrap();
        }
        let size = fs.node(dir).unwrap().size;
        fs.create_dirs("winsxs/short").unwrap();
        fs.create_dirs(&alloc::format!("winsxs/{}", long(100))).unwrap();
        assert_eq!(fs.node(dir).unwrap().size, size);
        assert!(fs.lookup(&alloc::format!("winsxs/{}", long(3))).is_ok());
        assert_eq!(fs.lookup(&alloc::format!("winsxs/{}", long(4))), Err(Error::NotFound));
        fs.rename(dir, &long(3), dir, "renamed").unwrap();
        fs.rename(dir, "short", dir, &long(200)).unwrap();
        assert!(fs.lookup("winsxs/renamed").is_ok());
        assert!(fs.lookup(&alloc::format!("winsxs/{}", long(200))).is_ok());
        assert_eq!(fs.read_dir(dir).unwrap().len(), 23);
        assert_eq!(fs.check().unwrap(), 0);
    }

    #[test]
    fn files_and_folders() {
        let mut fs = fs(512);
        fs.create_dirs("System/Drivers").unwrap();
        fs.write_file("System/Drivers/a.jdp", b"hello").unwrap();
        assert_eq!(fs.read_file("system/DRIVERS/A.jdp").unwrap(), b"hello");
        let big: Vec<u8> = (0..50_000u32).map(|i| i as u8).collect();
        fs.write_file("big", &big).unwrap();
        assert_eq!(fs.read_file("big").unwrap(), big);
        let big_id = fs.lookup("big").unwrap();
        fs.truncate(big_id, 5000).unwrap();
        assert_eq!(fs.read_file("big").unwrap(), &big[..5000]);
        let id = fs.lookup("big").unwrap();
        fs.write(id, 9000, b"x").unwrap();
        let data = fs.read_file("big").unwrap();
        assert_eq!(data.len(), 9001);
        assert!(data[5000..9000].iter().all(|&b| b == 0));
        assert_eq!(fs.check().unwrap(), 0);
        let sys = fs.lookup("System").unwrap();
        assert_eq!(fs.remove(sys, "Drivers"), Err(Error::NotEmpty));
        let drv = fs.lookup("System/Drivers").unwrap();
        fs.remove(drv, "a.jdp").unwrap();
        fs.remove(sys, "drivers").unwrap();
        fs.remove(ROOT, "big").unwrap();
        assert_eq!(fs.check().unwrap(), 0);
        let (total, free) = fs.space();
        assert_eq!(total - free, 2 * BLOCK_SIZE as u64); // the folder blocks of the root and System
    }

    #[test]
    fn fragmentation_uses_indirect_extents() {
        let mut fs = fs(2048);
        // Interleave two files so neither gets contiguous blocks.
        let a = fs.create(ROOT, "a", Kind::File).unwrap();
        let b = fs.create(ROOT, "b", Kind::File).unwrap();
        for i in 0..40u64 {
            fs.write(a, i * 4096, &[1u8; 4096]).unwrap();
            fs.write(b, i * 4096, &[2u8; 4096]).unwrap();
        }
        assert!(fs.node(a).unwrap().extent_count as usize > DIRECT_EXTENTS);
        assert!(fs.read_all(a).unwrap().iter().all(|&x| x == 1));
        assert_eq!(fs.check().unwrap(), 0);
        fs.remove(ROOT, "a").unwrap();
        assert_eq!(fs.check().unwrap(), 0);
    }

    #[test]
    fn big_folders_and_node_reuse() {
        let mut fs = fs(1024);
        let dir = fs.create(ROOT, "many", Kind::Dir).unwrap();
        // 200 entries: more than three blocks of them.
        let ids: Vec<NodeId> = (0..200).map(|i| fs.create(dir, &format!("File{i}"), Kind::File).unwrap()).collect();
        assert!(fs.node(dir).unwrap().size > 3 * BLOCK_SIZE as u64);
        assert_eq!(fs.find(dir, "file199").unwrap(), ids[199]);
        assert_eq!(fs.find(dir, "FILE70").unwrap(), ids[70]);
        assert_eq!(fs.find(dir, "File2000"), Err(Error::NotFound));
        assert_eq!(fs.find(dir, "File1"), Ok(ids[1]));
        // A freed slot and a freed node are used again.
        fs.remove(dir, "File70").unwrap();
        let size = fs.node(dir).unwrap().size;
        let again = fs.create(dir, "again", Kind::File).unwrap();
        assert_eq!(again, ids[70]);
        assert_eq!(fs.node(dir).unwrap().size, size);
        assert_eq!(fs.read_dir(dir).unwrap().len(), 200);
        assert_eq!(fs.remove(ROOT, "many"), Err(Error::NotEmpty));
        for e in fs.read_dir(dir).unwrap() {
            fs.remove(dir, &e.name).unwrap();
        }
        fs.remove(ROOT, "many").unwrap();
        assert_eq!(fs.check().unwrap(), 0);
    }

    #[test]
    fn remount_and_rename() {
        let mut fs = fs(256);
        fs.create_dirs("x/y").unwrap();
        fs.write_file("x/f", b"data").unwrap();
        let x = fs.lookup("x").unwrap();
        let y = fs.lookup("x/y").unwrap();
        fs.rename(x, "f", y, "g").unwrap();
        assert_eq!(fs.rename(ROOT, "x", y, "x"), Err(Error::InvalidName));
        let disk = fs.into_device();
        let mut fs = Fs::mount(disk, &NoClock).unwrap();
        assert_eq!(fs.read_file("x/y/g").unwrap(), b"data");
        assert_eq!(fs.lookup("x/f"), Err(Error::NotFound));
        assert_eq!(fs.superblock().label, "Test");
    }
}

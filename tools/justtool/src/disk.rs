//! JustFS on a region of a host file (a disk image or one of its partitions).

use justfs::{Block, BlockDevice, Error, BLOCK_SIZE};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};

pub struct FileBlocks {
    pub file: File,
    pub offset: u64,
    pub blocks: u64,
}

impl BlockDevice for FileBlocks {
    fn read_block(&mut self, block: u64, buf: &mut Block) -> justfs::Result<()> {
        if block >= self.blocks {
            return Err(Error::Io);
        }
        self.file.seek(SeekFrom::Start(self.offset + block * BLOCK_SIZE as u64)).map_err(|_| Error::Io)?;
        self.file.read_exact(buf).map_err(|_| Error::Io)
    }

    fn write_block(&mut self, block: u64, buf: &Block) -> justfs::Result<()> {
        if block >= self.blocks {
            return Err(Error::Io);
        }
        self.file.seek(SeekFrom::Start(self.offset + block * BLOCK_SIZE as u64)).map_err(|_| Error::Io)?;
        self.file.write_all(buf).map_err(|_| Error::Io)
    }

    fn block_count(&self) -> u64 {
        self.blocks
    }

    fn flush(&mut self) -> justfs::Result<()> {
        self.file.flush().map_err(|_| Error::Io)
    }
}

/// Finds the JustFS volume in an image: the first MBR partition of type
/// 0xAC, or the whole file. Returns (byte offset, byte length).
pub fn locate(file: &mut File) -> std::io::Result<(u64, u64)> {
    let len = file.metadata()?.len();
    let mut mbr = [0u8; 512];
    file.seek(SeekFrom::Start(0))?;
    if file.read_exact(&mut mbr).is_ok() && mbr[510] == 0x55 && mbr[511] == 0xAA {
        for i in 0..4 {
            let e = &mbr[0x1BE + i * 16..0x1CE + i * 16];
            if e[4] == 0xAC {
                let start = u32::from_le_bytes(e[8..12].try_into().unwrap()) as u64 * 512;
                let size = u32::from_le_bytes(e[12..16].try_into().unwrap()) as u64 * 512;
                return Ok((start, size.min(len.saturating_sub(start))));
            }
        }
    }
    Ok((0, len))
}

pub struct HostClock;

impl justfs::Clock for HostClock {
    fn now(&self) -> u64 {
        // Seconds since 2000-01-01 (946684800 in Unix time).
        let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        unix.saturating_sub(946_684_800)
    }
}

pub static CLOCK: HostClock = HostClock;

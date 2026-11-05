//! A FAT16 volume for the EFI system partition: the UEFI firmware reads the
//! loader, the kernel and the boot pack from it. Written once, in one go:
//! every file and folder gets contiguous clusters, and the data area starts
//! on a 2048-byte boundary, so on a CD the BIOS loader can read files
//! straight from their sectors.

use crate::Result;

const SECTOR: usize = 512;
const SECTORS_PER_CLUSTER: usize = 4;
const CLUSTER: usize = SECTOR * SECTORS_PER_CLUSTER;
const RESERVED_SECTORS: usize = 4;
const FATS: usize = 2;
const ROOT_ENTRIES: usize = 512;
const ATTR_LABEL: u8 = 0x08;
const ATTR_DIR: u8 = 0x10;
const ATTR_ARCHIVE: u8 = 0x20;
/// 2026-01-01, 00:00 in FAT's packed format.
const DATE: u16 = ((2026 - 1980) << 9) | (1 << 5) | 1;
const LABEL: &[u8; 11] = b"JUSTOS BOOT";

#[derive(Default)]
struct Dir {
    dirs: Vec<(String, Dir)>,
    files: Vec<(String, Vec<u8>)>,
}

impl Dir {
    fn add(&mut self, path: &str, data: Vec<u8>) -> Result<()> {
        let (first, rest) = match path.split_once('/') {
            Some((a, b)) => (a, Some(b)),
            None => (path, None),
        };
        short_name(first)?;
        match rest {
            None => self.files.push((first.to_string(), data)),
            Some(rest) => {
                let i = match self.dirs.iter().position(|(n, _)| n == first) {
                    Some(i) => i,
                    None => {
                        self.dirs.push((first.to_string(), Dir::default()));
                        self.dirs.len() - 1
                    }
                };
                self.dirs[i].1.add(rest, data)?;
            }
        }
        Ok(())
    }

    /// Directory entries this folder needs: "." and "..", or the volume
    /// label in the root.
    fn entries(&self, root: bool) -> usize {
        self.dirs.len() + self.files.len() + if root { 1 } else { 2 }
    }
}

/// The 8.3 name as stored on disk: "BOOTX64.EFI" → "BOOTX64 EFI".
fn short_name(name: &str) -> Result<[u8; 11]> {
    let (base, ext) = name.split_once('.').unwrap_or((name, ""));
    let ok = |s: &str, max| !s.is_empty() && s.len() <= max && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if !ok(base, 8) || !(ext.is_empty() || ok(ext, 3)) {
        return Err(format!("{name}: not a valid 8.3 name for the boot partition"));
    }
    let mut out = [b' '; 11];
    for (i, b) in base.bytes().enumerate() {
        out[i] = b.to_ascii_uppercase();
    }
    for (i, b) in ext.bytes().enumerate() {
        out[8 + i] = b.to_ascii_uppercase();
    }
    Ok(out)
}

fn dir_entry(name: [u8; 11], attr: u8, cluster: u16, size: u32) -> [u8; 32] {
    let mut e = [0u8; 32];
    e[..11].copy_from_slice(&name);
    e[11] = attr;
    e[16..18].copy_from_slice(&DATE.to_le_bytes()); // created
    e[18..20].copy_from_slice(&DATE.to_le_bytes()); // accessed
    e[24..26].copy_from_slice(&DATE.to_le_bytes()); // written
    e[26..28].copy_from_slice(&cluster.to_le_bytes());
    e[28..32].copy_from_slice(&size.to_le_bytes());
    e
}

struct Writer {
    image: Vec<u8>,
    fat: Vec<u16>,
    next: usize,
    data_start: usize,
    offsets: Offsets,
}

impl Writer {
    /// Reserves `bytes` worth of contiguous clusters; returns the first.
    fn alloc(&mut self, bytes: usize) -> Result<u16> {
        let count = bytes.div_ceil(CLUSTER).max(1);
        if self.next + count > self.fat.len() {
            return Err("the boot partition is full".into());
        }
        let first = self.next;
        for c in first..first + count {
            self.fat[c] = if c + 1 == first + count { 0xFFFF } else { (c + 1) as u16 };
        }
        self.next += count;
        Ok(first as u16)
    }

    fn put(&mut self, cluster: u16, data: &[u8]) {
        let at = self.data_start + (cluster as usize - 2) * CLUSTER;
        self.image[at..at + data.len()].copy_from_slice(data);
    }

    /// Writes the contents of `dir` (whose own cluster is `own`, 0 for the
    /// root) and returns its entry table.
    fn write_dir(&mut self, dir: &Dir, own: u16, parent: u16, path: &str) -> Result<Vec<u8>> {
        let mut table = Vec::new();
        if own == 0 {
            table.extend_from_slice(&dir_entry(*LABEL, ATTR_LABEL, 0, 0));
        } else {
            table.extend_from_slice(&dir_entry(*b".          ", ATTR_DIR, own, 0));
            table.extend_from_slice(&dir_entry(*b"..         ", ATTR_DIR, parent, 0));
        }
        for (name, data) in &dir.files {
            let cluster = if data.is_empty() { 0 } else { self.alloc(data.len())? };
            if cluster != 0 {
                self.put(cluster, data);
                let at = self.data_start + (cluster as usize - 2) * CLUSTER;
                self.offsets.push((format!("{path}{name}"), at as u64));
            }
            table.extend_from_slice(&dir_entry(short_name(name)?, ATTR_ARCHIVE, cluster, data.len() as u32));
        }
        for (name, sub) in &dir.dirs {
            let cluster = self.alloc(sub.entries(false) * 32)?;
            let contents = self.write_dir(sub, cluster, own, &format!("{path}{name}/"))?;
            self.put(cluster, &contents);
            table.extend_from_slice(&dir_entry(short_name(name)?, ATTR_DIR, cluster, 0));
        }
        Ok(table)
    }
}

/// Where each file's data starts in the image, by path.
pub type Offsets = Vec<(String, u64)>;

/// Builds a FAT16 volume of `size` bytes holding `files` (paths with '/').
/// `hidden` is the partition's first sector on the disk. Also returns
/// where each file's data starts in the image.
pub fn build(size: u64, hidden: u64, files: Vec<(&str, Vec<u8>)>) -> Result<(Vec<u8>, Offsets)> {
    let total = (size as usize) / SECTOR;
    let root_sectors = ROOT_ENTRIES * 32 / SECTOR;
    let clusters_estimate = (total - RESERVED_SECTORS - root_sectors) / SECTORS_PER_CLUSTER;
    let fat_sectors = ((clusters_estimate + 2) * 2).div_ceil(SECTOR);
    // Extra reserved sectors put the data area on a cluster boundary.
    let unaligned = RESERVED_SECTORS + FATS * fat_sectors + root_sectors;
    let reserved = RESERVED_SECTORS + unaligned.next_multiple_of(SECTORS_PER_CLUSTER) - unaligned;
    let data_start = reserved + FATS * fat_sectors + root_sectors;
    let clusters = (total - data_start) / SECTORS_PER_CLUSTER;
    if !(4085..65525).contains(&clusters) {
        return Err(format!("a boot partition of {} MiB cannot be FAT16", size >> 20));
    }

    let mut root = Dir::default();
    for (path, data) in files {
        root.add(path, data)?;
    }
    if root.entries(true) > ROOT_ENTRIES {
        return Err("too many files in the boot partition".into());
    }

    let mut w = Writer {
        image: vec![0; total * SECTOR],
        fat: vec![0; clusters + 2],
        next: 2,
        data_start: data_start * SECTOR,
        offsets: Vec::new(),
    };
    w.fat[0] = 0xFFF8;
    w.fat[1] = 0xFFFF;
    let root_table = w.write_dir(&root, 0, 0, "")?;
    let root_at = (reserved + FATS * fat_sectors) * SECTOR;
    w.image[root_at..root_at + root_table.len()].copy_from_slice(&root_table);

    // Boot sector with the BIOS parameter block.
    let b = &mut w.image[..SECTOR];
    b[0..3].copy_from_slice(&[0xEB, 0x3C, 0x90]);
    b[3..11].copy_from_slice(b"JUSTOS  ");
    b[11..13].copy_from_slice(&(SECTOR as u16).to_le_bytes());
    b[13] = SECTORS_PER_CLUSTER as u8;
    b[14..16].copy_from_slice(&(reserved as u16).to_le_bytes());
    b[16] = FATS as u8;
    b[17..19].copy_from_slice(&(ROOT_ENTRIES as u16).to_le_bytes());
    if total < 0x10000 {
        b[19..21].copy_from_slice(&(total as u16).to_le_bytes());
    } else {
        b[32..36].copy_from_slice(&(total as u32).to_le_bytes());
    }
    b[21] = 0xF8; // fixed disk
    b[22..24].copy_from_slice(&(fat_sectors as u16).to_le_bytes());
    b[24..26].copy_from_slice(&32u16.to_le_bytes()); // sectors per track
    b[26..28].copy_from_slice(&64u16.to_le_bytes()); // heads
    b[28..32].copy_from_slice(&(hidden as u32).to_le_bytes());
    b[36] = 0x80;
    b[38] = 0x29;
    b[39..43].copy_from_slice(&0x4152_4331u32.to_le_bytes());
    b[43..54].copy_from_slice(LABEL);
    b[54..62].copy_from_slice(b"FAT16   ");
    b[510] = 0x55;
    b[511] = 0xAA;

    let fat_bytes: Vec<u8> = w.fat.iter().flat_map(|e| e.to_le_bytes()).collect();
    for i in 0..FATS {
        let at = (reserved + i * fat_sectors) * SECTOR;
        w.image[at..at + fat_bytes.len()].copy_from_slice(&fat_bytes);
    }
    Ok((w.image, w.offsets))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(&short_name("BOOTX64.EFI").unwrap(), b"BOOTX64 EFI");
        assert_eq!(&short_name("efi").unwrap(), b"EFI        ");
        assert!(short_name("toolongname.txt").is_err());
        assert!(short_name("a.text").is_err());
    }

    #[test]
    fn layout() {
        let (img, offsets) =
            build(16 << 20, 2048, vec![("EFI/BOOT/BOOTX64.EFI", vec![1; 5000]), ("JUSTOS/KERNEL.BIN", vec![2; 10])]).unwrap();
        assert_eq!(img.len(), 16 << 20);
        assert_eq!(&img[54..59], b"FAT16");
        let fat_sectors = u16::from_le_bytes([img[22], img[23]]) as usize;
        let reserved = u16::from_le_bytes([img[14], img[15]]) as usize;
        let root = (reserved + FATS * fat_sectors) * SECTOR;
        // Files start on 2048-byte boundaries, where their data is.
        for (name, at) in &offsets {
            assert_eq!(at % 2048, 0, "{name}");
        }
        let kernel = offsets.iter().find(|(n, _)| n == "JUSTOS/KERNEL.BIN").unwrap().1 as usize;
        assert_eq!(&img[kernel..kernel + 10], &[2; 10]);
        // Root: the folders, in the order they were first named.
        assert_eq!(&img[root..root + 11], LABEL);
        assert_eq!(&img[root + 32..root + 43], b"EFI        ");
        assert_eq!(img[root + 43], ATTR_DIR);
        assert_eq!(&img[root + 64..root + 75], b"JUSTOS     ");
    }
}

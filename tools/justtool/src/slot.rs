//! System slots for A/B updates.
//!
//! A slot holds one version of the system: the kernel and its boot pack
//! (which carries every driver that comes with the system). A disk has two,
//! `JUSTOS/SLOTA.BIN` and `JUSTOS/SLOTB.BIN` in the EFI system partition, each
//! bigger than the system so that a newer one fits; `JUSTOS/BOOT.CTL` says
//! which one starts. The files are contiguous: the BIOS loader and the
//! kernel reach them by sector, the UEFI loader as files.
//!
//! ```text
//! slot image (what goes at the start of a slot; a system update is one):
//!   sector 0   header
//!     0x00 "ARCSLOT1"        0x08 kernel sectors (from sector 1)
//!     0x0C kernel memory size (with .bss)    0x10 kernel entry (u64)
//!     0x18 boot pack: first sector           0x1C boot pack sectors
//!     0x20 kernel bytes      0x24 boot pack bytes
//!     0x28 sectors in all (header included)
//!     0x30 SHA-256 of the sectors after the header
//!     0x50 version, 32 bytes, NUL-padded     0x70 build time (Unix, u64)
//!     0x1FC checksum: the 128 little-endian dwords add up to 0
//!   sector 1…  the kernel, then the boot pack, each padded to a sector
//!
//! BOOT.CTL (one sector):
//!   0x00 "ARCCTL01"
//!   0x08 slot A, 0x0C slot B: first sector on the disk
//!   0x10 sectors per slot
//!   0x14 the slot that starts (0 = A, 1 = B)
//!   0x15 starts left for it while it is not known to work
//!   0x16 slots known to work (bit 0 = A, bit 1 = B)
//!   0x18 sequence number, one more on every change
//!   0x1FC checksum, as in the header
//! ```
//!
//! The loaders start the active slot; one not yet known to work uses up a
//! start each time, and with none left they start the other. The kernel
//! marks its slot as working once the desktop is up.

use justtls::sha2::Sha256;

pub const SECTOR: usize = 512;
pub const SLOT_MAGIC: &[u8; 8] = b"ARCSLOT1";
pub const CTL_MAGIC: &[u8; 8] = b"ARCCTL01";

fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn get32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

/// Sets the checksum dword so the sector's dwords add up to zero.
pub fn seal(sector: &mut [u8]) {
    put32(sector, 0x1FC, 0);
    let sum = (0..SECTOR / 4).fold(0u32, |s, i| s.wrapping_add(get32(sector, i * 4)));
    put32(sector, 0x1FC, sum.wrapping_neg());
}

pub fn sealed(sector: &[u8]) -> bool {
    sector.len() >= SECTOR && (0..SECTOR / 4).fold(0u32, |s, i| s.wrapping_add(get32(sector, i * 4))) == 0
}

/// What a slot image says about itself.
pub struct Header {
    pub kernel_sectors: u32,
    pub pack_sector: u32,
    pub pack_sectors: u32,
    pub total_sectors: u32,
    pub version: String,
}

pub fn parse(image: &[u8]) -> Option<Header> {
    let h = image.get(..SECTOR)?;
    if &h[..8] != SLOT_MAGIC || !sealed(h) {
        return None;
    }
    let v = &h[0x50..0x70];
    let len = v.iter().position(|&b| b == 0).unwrap_or(v.len());
    Some(Header {
        kernel_sectors: get32(h, 0x08),
        pack_sector: get32(h, 0x18),
        pack_sectors: get32(h, 0x1C),
        total_sectors: get32(h, 0x28),
        version: String::from_utf8_lossy(&v[..len]).into_owned(),
    })
}

/// A slot image: the header, the kernel (flat, from its link address) and
/// the boot pack.
pub fn build(flat: &[u8], memsz: u64, entry: u64, pack: &[u8], version: &str, built: u64) -> Vec<u8> {
    let kernel_sectors = flat.len().div_ceil(SECTOR);
    let pack_sector = 1 + kernel_sectors;
    let pack_sectors = pack.len().div_ceil(SECTOR);
    let total = pack_sector + pack_sectors;
    let mut out = vec![0u8; total * SECTOR];
    out[SECTOR..SECTOR + flat.len()].copy_from_slice(flat);
    out[pack_sector * SECTOR..pack_sector * SECTOR + pack.len()].copy_from_slice(pack);
    let digest = Sha256::digest(&out[SECTOR..]);
    let h = &mut out[..SECTOR];
    h[..8].copy_from_slice(SLOT_MAGIC);
    put32(h, 0x08, kernel_sectors as u32);
    put32(h, 0x0C, memsz as u32);
    h[0x10..0x18].copy_from_slice(&entry.to_le_bytes());
    put32(h, 0x18, pack_sector as u32);
    put32(h, 0x1C, pack_sectors as u32);
    put32(h, 0x20, flat.len() as u32);
    put32(h, 0x24, pack.len() as u32);
    put32(h, 0x28, total as u32);
    h[0x30..0x50].copy_from_slice(&digest);
    let v = version.as_bytes();
    h[0x50..0x50 + v.len().min(31)].copy_from_slice(&v[..v.len().min(31)]);
    h[0x70..0x78].copy_from_slice(&built.to_le_bytes());
    seal(h);
    out
}

/// A fresh BOOT.CTL: slot A starts, both slots known to work (a new disk
/// has the same system in both).
pub fn control(slot_a: u64, slot_b: u64, slot_sectors: u64) -> Vec<u8> {
    let mut c = vec![0u8; SECTOR];
    c[..8].copy_from_slice(CTL_MAGIC);
    put32(&mut c, 0x08, slot_a as u32);
    put32(&mut c, 0x0C, slot_b as u32);
    put32(&mut c, 0x10, slot_sectors as u32);
    c[0x14] = 0;
    c[0x15] = 0;
    c[0x16] = 0b11;
    put32(&mut c, 0x18, 1);
    seal(&mut c);
    c
}

/// Seconds since 1970 for the header: SOURCE_DATE_EPOCH when set (a
/// reproducible build), else now.
pub fn build_time() -> u64 {
    std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or_else(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header() {
        let img = build(&[1; 1000], 5000, 0xFFFF_FFFF_8010_0000, &[2; 600], "1.2.3", 7);
        assert_eq!(img.len(), (1 + 2 + 2) * SECTOR);
        let h = parse(&img).unwrap();
        assert_eq!((h.kernel_sectors, h.pack_sector, h.pack_sectors, h.total_sectors), (2, 3, 2, 5));
        assert_eq!(h.version, "1.2.3");
        assert_eq!(&img[SECTOR..SECTOR + 1000], &[1; 1000][..]);
        assert_eq!(img[3 * SECTOR], 2);
        assert_eq!(&img[0x30..0x50], &Sha256::digest(&img[SECTOR..])[..]);
        let mut bad = img.clone();
        bad[0x08] ^= 1;
        assert!(parse(&bad).is_none());
    }

    #[test]
    fn control_sector() {
        let c = control(4096, 53248, 49152);
        assert!(sealed(&c));
        assert_eq!(&c[..8], CTL_MAGIC);
        assert_eq!(get32(&c, 0x0C), 53248);
        assert_eq!(c[0x16], 3);
    }
}

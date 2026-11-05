//! A bootable CD image: ISO 9660 with an El Torito boot catalog.
//!
//! The file system is the smallest valid one (an empty root folder); what
//! matters is the boot catalog, which points the BIOS at a boot sector
//! ("no emulation": it is loaded and run as it is) and UEFI firmware at a
//! FAT image holding the EFI loader.
//!
//! ```text
//! sector 0-15   system area (unused)
//! 16            primary volume descriptor
//! 17            boot record (El Torito)
//! 18            volume descriptor set terminator
//! 19            boot catalog
//! 20, 21        path tables (little and big endian)
//! 22            root folder
//! 23 ...        whatever the caller reserves
//! ```

pub const SECTOR: usize = 2048;
const PVD: usize = 16;
const BOOT_RECORD: usize = 17;
const TERMINATOR: usize = 18;
const CATALOG: usize = 19;
const PATH_L: usize = 20;
const PATH_M: usize = 21;
const ROOT: usize = 22;
const FIRST_FREE: u64 = 23;

pub struct Iso {
    image: Vec<u8>,
    volume_id: String,
    bios: Option<(u64, u16)>,
    efi: Option<(u64, u64)>,
}

/// Both-endian fields, as ISO 9660 stores numbers.
fn both16(v: u16) -> [u8; 4] {
    let (l, b) = (v.to_le_bytes(), v.to_be_bytes());
    [l[0], l[1], b[0], b[1]]
}

fn both32(v: u32) -> [u8; 8] {
    let (l, b) = (v.to_le_bytes(), v.to_be_bytes());
    [l[0], l[1], l[2], l[3], b[0], b[1], b[2], b[3]]
}

/// A directory record for the root folder ('.' is name 0, '..' name 1).
fn root_record(name: u8) -> [u8; 34] {
    let mut r = [0u8; 34];
    r[0] = 34;
    r[2..10].copy_from_slice(&both32(ROOT as u32));
    r[10..18].copy_from_slice(&both32(SECTOR as u32));
    // 2026-01-01 00:00:00 UTC: years since 1900, month, day, h, m, s, zone.
    r[18..25].copy_from_slice(&[126, 1, 1, 0, 0, 0, 0]);
    r[25] = 2; // a folder
    r[28..32].copy_from_slice(&both16(1));
    r[32] = 1;
    r[33] = name;
    r
}

fn sector(image: &mut [u8], n: usize) -> &mut [u8] {
    &mut image[n * SECTOR..(n + 1) * SECTOR]
}

/// "a-characters" padded with spaces.
fn text(field: &mut [u8], s: &str) {
    field.fill(b' ');
    let n = s.len().min(field.len());
    field[..n].copy_from_slice(&s.as_bytes()[..n]);
}

impl Iso {
    pub fn new(volume_id: &str) -> Iso {
        Iso { image: vec![0; FIRST_FREE as usize * SECTOR], volume_id: volume_id.to_string(), bios: None, efi: None }
    }

    /// Room for `bytes`; returns its first sector.
    pub fn reserve(&mut self, bytes: u64) -> u64 {
        let at = (self.image.len() / SECTOR) as u64;
        let sectors = bytes.div_ceil(SECTOR as u64).max(1);
        self.image.resize(self.image.len() + sectors as usize * SECTOR, 0);
        at
    }

    pub fn put(&mut self, sector: u64, data: &[u8]) {
        let at = sector as usize * SECTOR;
        self.image[at..at + data.len()].copy_from_slice(data);
    }

    /// BIOS boot: `count` 512-byte sectors from `sector`, loaded at 0000:7C00.
    pub fn boot_bios(&mut self, sector: u64, count: u16) {
        self.bios = Some((sector, count));
    }

    /// UEFI boot: the FAT image of `bytes` at `sector`.
    pub fn boot_efi(&mut self, sector: u64, bytes: u64) {
        self.efi = Some((sector, bytes));
    }

    pub fn finish(mut self) -> Vec<u8> {
        let total = (self.image.len() / SECTOR) as u32;

        let p = sector(&mut self.image, PVD);
        p[0] = 1;
        p[1..6].copy_from_slice(b"CD001");
        p[6] = 1;
        text(&mut p[8..40], "JUSTOS");
        text(&mut p[40..72], &self.volume_id);
        p[80..88].copy_from_slice(&both32(total));
        p[120..124].copy_from_slice(&both16(1)); // volume set size
        p[124..128].copy_from_slice(&both16(1)); // volume sequence number
        p[128..132].copy_from_slice(&both16(SECTOR as u16));
        p[132..140].copy_from_slice(&both32(10)); // path table size
        p[140..144].copy_from_slice(&(PATH_L as u32).to_le_bytes());
        p[148..152].copy_from_slice(&(PATH_M as u32).to_be_bytes());
        p[156..190].copy_from_slice(&root_record(0));
        for (from, to) in [(190, 318), (318, 446), (446, 574), (574, 702)] {
            text(&mut p[from..to], ""); // volume set, publisher, preparer, application
        }
        text(&mut p[318..446], "JUSTOS");
        for (from, to) in [(702, 739), (739, 776), (776, 813)] {
            text(&mut p[from..to], ""); // copyright, abstract, bibliography files
        }
        // Creation, modification, expiration, effective dates.
        for at in [813, 830, 847, 864] {
            let date: &[u8; 16] = if at == 847 || at == 864 { b"0000000000000000" } else { b"2026010100000000" };
            p[at..at + 16].copy_from_slice(date);
            p[at + 16] = 0;
        }
        p[881] = 1; // file structure version

        let b = sector(&mut self.image, BOOT_RECORD);
        b[0] = 0;
        b[1..6].copy_from_slice(b"CD001");
        b[6] = 1;
        b[7..30].copy_from_slice(b"EL TORITO SPECIFICATION");
        b[0x47..0x4B].copy_from_slice(&(CATALOG as u32).to_le_bytes());

        let t = sector(&mut self.image, TERMINATOR);
        t[0] = 255;
        t[1..6].copy_from_slice(b"CD001");
        t[6] = 1;

        // Path tables: the root folder only.
        for (n, big) in [(PATH_L, false), (PATH_M, true)] {
            let s = sector(&mut self.image, n);
            s[0] = 1;
            let loc = if big { (ROOT as u32).to_be_bytes() } else { (ROOT as u32).to_le_bytes() };
            s[2..6].copy_from_slice(&loc);
            s[6..8].copy_from_slice(&if big { 1u16.to_be_bytes() } else { 1u16.to_le_bytes() });
        }
        let r = sector(&mut self.image, ROOT);
        r[0..34].copy_from_slice(&root_record(0));
        r[34..68].copy_from_slice(&root_record(1));

        let catalog = self.catalog();
        sector(&mut self.image, CATALOG)[..catalog.len()].copy_from_slice(&catalog);
        self.image
    }

    fn catalog(&self) -> Vec<u8> {
        let mut c = vec![0u8; 32];
        // Validation entry: x86, a checksum that makes the words sum to 0.
        c[0] = 1;
        c[4..10].copy_from_slice(b"JUSTOS");
        c[30] = 0x55;
        c[31] = 0xAA;
        let sum = c.chunks(2).fold(0u16, |s, w| s.wrapping_add(u16::from_le_bytes([w[0], w[1]])));
        c[28..30].copy_from_slice(&sum.wrapping_neg().to_le_bytes());

        let entry = |sector: u64, count: u16| {
            let mut e = [0u8; 32];
            e[0] = 0x88; // bootable, no emulation, loaded at the default segment
            e[6..8].copy_from_slice(&count.to_le_bytes());
            e[8..12].copy_from_slice(&(sector as u32).to_le_bytes());
            e
        };
        // Default entry: BIOS (or nothing bootable).
        match self.bios {
            Some((sector, count)) => c.extend_from_slice(&entry(sector, count)),
            None => c.extend_from_slice(&[0; 32]),
        }
        if let Some((sector, bytes)) = self.efi {
            // A final section header for platform 0xEF (UEFI), one entry.
            let mut h = [0u8; 32];
            h[0] = 0x91;
            h[1] = 0xEF;
            h[2..4].copy_from_slice(&1u16.to_le_bytes());
            c.extend_from_slice(&h);
            // The count is in 512-byte units and only 16 bits wide; firmware
            // reads the FAT image's own size for larger ones.
            c.extend_from_slice(&entry(sector, (bytes / 512).min(0xFFFF) as u16));
        }
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout() {
        let mut iso = Iso::new("TEST");
        let boot = iso.reserve(512);
        let efi = iso.reserve(5000);
        iso.put(boot, &[0xEB; 512]);
        iso.boot_bios(boot, 4);
        iso.boot_efi(efi, 5000);
        let img = iso.finish();
        assert_eq!(img.len(), (FIRST_FREE as usize + 1 + 3) * SECTOR);
        assert_eq!(&img[PVD * SECTOR + 1..PVD * SECTOR + 6], b"CD001");
        assert_eq!(u32::from_le_bytes(img[PVD * SECTOR + 80..PVD * SECTOR + 84].try_into().unwrap()) as usize, img.len() / SECTOR);
        let cat = &img[CATALOG * SECTOR..CATALOG * SECTOR + 128];
        let sum = cat[..32].chunks(2).fold(0u16, |s, w| s.wrapping_add(u16::from_le_bytes([w[0], w[1]])));
        assert_eq!(sum, 0);
        assert_eq!((cat[32], u32::from_le_bytes(cat[40..44].try_into().unwrap())), (0x88, boot as u32));
        assert_eq!((cat[64], cat[65]), (0x91, 0xEF));
        assert_eq!(u32::from_le_bytes(cat[104..108].try_into().unwrap()), efi as u32);
        assert_eq!(img[boot as usize * SECTOR], 0xEB);
    }
}

//! `.adp` driver packages (the JustOS driver package format).
//!
//! ```text
//! offset  size  field
//!      0     8  magic "ARCDRVPK"
//!      8     4  format version (1)
//!     12     4  driver ABI version the module was built for
//!     16     4  manifest offset      20  4  manifest length
//!     24     4  module offset        28  4  module length
//!     32     4  CRC-32 of every byte after the header
//!     36     4  reserved (0)
//!     40        manifest (UTF-8), module (ELF64 position-independent image)
//! ```
//! All integers are little-endian.

use crate::crc32;
use crate::manifest::Manifest;

pub const MAGIC: &[u8; 8] = b"ARCDRVPK";
pub const FORMAT_VERSION: u32 = 1;
pub const HEADER_SIZE: usize = 40;

/// A validated view over the bytes of a driver package.
#[derive(Clone, Copy)]
pub struct Package<'a> {
    pub abi_version: u32,
    pub manifest: Manifest<'a>,
    pub module: &'a [u8],
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

impl<'a> Package<'a> {
    /// Checks the header, the CRC and the manifest.
    pub fn parse(bytes: &'a [u8]) -> Result<Package<'a>, &'static str> {
        if bytes.len() < HEADER_SIZE || &bytes[..8] != MAGIC {
            return Err("not a driver package");
        }
        if le32(bytes, 8) != FORMAT_VERSION {
            return Err("unsupported package format");
        }
        let abi_version = le32(bytes, 12);
        let range = |off: usize, len: usize| -> Result<&'a [u8], &'static str> {
            let end = off.checked_add(len).ok_or("corrupt package")?;
            if off < HEADER_SIZE || end > bytes.len() {
                return Err("corrupt package");
            }
            Ok(&bytes[off..end])
        };
        let manifest = range(le32(bytes, 16) as usize, le32(bytes, 20) as usize)?;
        let module = range(le32(bytes, 24) as usize, le32(bytes, 28) as usize)?;
        if crc32::crc32(&bytes[HEADER_SIZE..]) != le32(bytes, 32) {
            return Err("package checksum mismatch");
        }
        let text = core::str::from_utf8(manifest).map_err(|_| "manifest is not UTF-8")?;
        let manifest = Manifest::parse(text)?;
        if module.len() < 4 || &module[..4] != b"\x7fELF" {
            return Err("package module is not an ELF image");
        }
        Ok(Package { abi_version, manifest, module })
    }
}

/// Size of the package built by [`write`].
pub fn packed_size(manifest: &str, module: &[u8]) -> usize {
    HEADER_SIZE + manifest.len() + module.len()
}

/// Builds a package into `out`, which must be exactly [`packed_size`] bytes.
pub fn write(out: &mut [u8], abi_version: u32, manifest: &str, module: &[u8]) {
    assert_eq!(out.len(), packed_size(manifest, module));
    let m_off = HEADER_SIZE;
    let i_off = m_off + manifest.len();
    out[..8].copy_from_slice(MAGIC);
    let fields = [FORMAT_VERSION, abi_version, m_off as u32, manifest.len() as u32, i_off as u32, module.len() as u32];
    for (i, v) in fields.iter().enumerate() {
        out[8 + i * 4..12 + i * 4].copy_from_slice(&v.to_le_bytes());
    }
    out[36..40].copy_from_slice(&0u32.to_le_bytes());
    out[m_off..i_off].copy_from_slice(manifest.as_bytes());
    out[i_off..].copy_from_slice(module);
    let crc = crc32::crc32(&out[HEADER_SIZE..]);
    out[32..36].copy_from_slice(&crc.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let manifest = "name = test\nversion = 1.0.0\n";
        let module = b"\x7fELFdata";
        let mut buf = [0u8; 128];
        let n = packed_size(manifest, module);
        write(&mut buf[..n], 1, manifest, module);
        let p = Package::parse(&buf[..n]).unwrap();
        assert_eq!(p.manifest.name(), "test");
        assert_eq!(p.module, module);
        buf[n - 1] ^= 1;
        assert!(Package::parse(&buf[..n]).is_err());
    }
}

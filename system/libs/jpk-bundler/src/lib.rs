//! # jpk-bundler
//!
//! Standalone JustScript executables: a runtime stub (a Windows `.exe` or a
//! Linux ELF program) with a `.jpk` package appended.
//!
//! ```text
//! [ runtime stub (PE / ELF) ][ .jpk payload ][ trailer (24 bytes) ]
//!
//! trailer: u64 payload length | u32 payload CRC-32 | u32 version (1) | "JPKBNDL\0"
//! ```
//!
//! Loaders ignore data after the image (the PE "overlay"), so the stub runs
//! unchanged: it reads its own file, finds the trailer at the end and runs
//! the package in front of it. `signtool` appends its certificate table
//! after the trailer, so in a signed `.exe` the trailer is right before
//! that table (up to 7 bytes of padding in between).
//!
//! `justc build -t windows` makes the executables on justOS; the stub is
//! `tools/just-stub`.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

use alloc::vec::Vec;
use core::fmt;
use justc_core::crc32::crc32;

/// The last 8 bytes of a bundle.
pub const MAGIC: &[u8; 8] = b"JPKBNDL\0";
/// The trailer's version field.
pub const VERSION: u32 = 1;
pub const TRAILER_LEN: usize = 24;

/// The kind of program a stub is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// A 64-bit Windows program (PE32+, x86-64).
    Windows,
    /// A 64-bit ELF program (Linux).
    Elf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The stub is not a 64-bit Windows or ELF program.
    NotAProgram,
    /// The stub already carries a package.
    AlreadyBundled,
    /// No trailer: a program without a package.
    NoPackage,
    /// A trailer whose length does not fit in the file.
    Damaged,
    /// The package does not match its CRC-32.
    BadChecksum,
    /// A trailer of a newer bundler.
    UnknownVersion(u32),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotAProgram => f.write_str("not a 64-bit Windows or ELF program"),
            Error::AlreadyBundled => f.write_str("the runtime already carries a package"),
            Error::NoPackage => f.write_str("no JustScript package in this program"),
            Error::Damaged => f.write_str("the package at the end of the program is damaged"),
            Error::BadChecksum => f.write_str("the package does not match its checksum"),
            Error::UnknownVersion(v) => write!(f, "package bundle version {v} is newer than this runtime"),
        }
    }
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at.checked_add(2)?)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

/// The PE optional header of a 64-bit x86 Windows program.
fn pe_optional_header(b: &[u8]) -> Option<usize> {
    if b.get(..2)? != b"MZ" {
        return None;
    }
    let pe = u32_at(b, 0x3C)? as usize;
    if b.get(pe..pe.checked_add(4)?)? != b"PE\0\0" || u16_at(b, pe + 4)? != 0x8664 {
        return None;
    }
    let optional = pe + 24;
    // PE32+.
    (u16_at(b, optional)? == 0x20B).then_some(optional)
}

/// What kind of program `stub` is, if one this crate can bundle.
pub fn format(stub: &[u8]) -> Option<Format> {
    if pe_optional_header(stub).is_some() {
        Some(Format::Windows)
    } else if stub.len() > 0x40 && stub[..4] == *b"\x7FELF" && stub[4] == 2 {
        Some(Format::Elf)
    } else {
        None
    }
}

/// The certificate table of a signed `.exe`: its file offset and length.
fn certificates(b: &[u8]) -> Option<(usize, usize)> {
    let optional = pe_optional_header(b)?;
    // Data directory 4 (security) holds a file offset, not an address.
    let entries = u32_at(b, optional + 108)?;
    if entries <= 4 {
        return None;
    }
    let offset = u32_at(b, optional + 112 + 4 * 8)? as usize;
    let len = u32_at(b, optional + 112 + 4 * 8 + 4)? as usize;
    (offset != 0 && len != 0 && offset.checked_add(len)? <= b.len()).then_some((offset, len))
}

/// A trailer ending at `end`: the package's offset, length and CRC-32.
fn trailer(b: &[u8], end: usize) -> Option<Result<(usize, usize, u32), Error>> {
    let start = end.checked_sub(TRAILER_LEN)?;
    let t = b.get(start..end)?;
    if t[16..] != *MAGIC {
        return None;
    }
    let version = u32_at(t, 12)?;
    if version != VERSION {
        return Some(Err(Error::UnknownVersion(version)));
    }
    let len = u64::from_le_bytes(t[..8].try_into().ok()?);
    let Some(offset) = usize::try_from(len).ok().and_then(|len| start.checked_sub(len)) else {
        return Some(Err(Error::Damaged));
    };
    Some(Ok((offset, start - offset, u32_at(t, 8)?)))
}

/// The package a bundled program carries.
pub fn payload(program: &[u8]) -> Result<&[u8], Error> {
    let mut ends = Vec::from([program.len()]);
    if let Some((table, _)) = certificates(program) {
        // Signed after bundling: the trailer ends before the certificates,
        // which start on an 8-byte boundary.
        ends.extend((0..8).filter_map(|pad| table.checked_sub(pad)));
    }
    for end in ends {
        if let Some(found) = trailer(program, end) {
            let (offset, len, crc) = found?;
            let package = &program[offset..offset + len];
            return if crc32(package) == crc { Ok(package) } else { Err(Error::BadChecksum) };
        }
    }
    Err(Error::NoPackage)
}

/// A standalone program: `stub` with the package `jpk` appended.
pub fn bundle(stub: &[u8], jpk: &[u8]) -> Result<Vec<u8>, Error> {
    if format(stub).is_none() {
        return Err(Error::NotAProgram);
    }
    if payload(stub) != Err(Error::NoPackage) {
        return Err(Error::AlreadyBundled);
    }
    let mut out = Vec::with_capacity(stub.len() + jpk.len() + TRAILER_LEN);
    out.extend_from_slice(stub);
    out.extend_from_slice(jpk);
    out.extend_from_slice(&(jpk.len() as u64).to_le_bytes());
    out.extend_from_slice(&crc32(jpk).to_le_bytes());
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(MAGIC);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;

    /// A minimal PE32+ x86-64 header, `security` as its certificate table.
    fn pe(len: usize, security: Option<(u32, u32)>) -> Vec<u8> {
        let mut b = vec![0u8; len];
        b[..2].copy_from_slice(b"MZ");
        b[0x3C..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        b[0x80..0x84].copy_from_slice(b"PE\0\0");
        b[0x84..0x86].copy_from_slice(&0x8664u16.to_le_bytes());
        let opt = 0x80 + 24;
        b[opt..opt + 2].copy_from_slice(&0x20Bu16.to_le_bytes());
        b[opt + 108..opt + 112].copy_from_slice(&16u32.to_le_bytes());
        if let Some((offset, size)) = security {
            b[opt + 144..opt + 148].copy_from_slice(&offset.to_le_bytes());
            b[opt + 148..opt + 152].copy_from_slice(&size.to_le_bytes());
        }
        b
    }

    #[test]
    fn round_trip() {
        let stub = pe(1024, None);
        let exe = bundle(&stub, b"JPK\0 a package").unwrap();
        assert_eq!(&exe[..1024], &stub[..]);
        assert_eq!(&exe[exe.len() - 8..], MAGIC);
        assert_eq!(payload(&exe), Ok(&b"JPK\0 a package"[..]));
        assert_eq!(format(&exe), Some(Format::Windows));
        assert_eq!(bundle(&exe, b"again"), Err(Error::AlreadyBundled));
        assert_eq!(payload(&stub), Err(Error::NoPackage));
    }

    #[test]
    fn elf_stub() {
        let mut stub = vec![0u8; 256];
        stub[..5].copy_from_slice(b"\x7FELF\x02");
        let program = bundle(&stub, b"pkg").unwrap();
        assert_eq!(payload(&program), Ok(&b"pkg"[..]));
    }

    #[test]
    fn not_a_program() {
        assert_eq!(bundle(b"#!/bin/sh\necho hi\n", b"pkg"), Err(Error::NotAProgram));
        let mut pe32 = pe(512, None);
        pe32[0x80 + 24] = 0x0B; // PE32 (0x10B): 32-bit
        pe32[0x80 + 25] = 0x01;
        assert_eq!(format(&pe32), None);
    }

    #[test]
    fn damage_is_found() {
        let mut exe = bundle(&pe(512, None), b"a package").unwrap();
        exe[515] ^= 1;
        assert_eq!(payload(&exe), Err(Error::BadChecksum));
        let n = exe.len();
        exe[n - 24..n - 16].copy_from_slice(&u64::MAX.to_le_bytes());
        assert_eq!(payload(&exe), Err(Error::Damaged));
        exe[n - 12..n - 8].copy_from_slice(&2u32.to_le_bytes());
        assert_eq!(payload(&exe), Err(Error::UnknownVersion(2)));
    }

    #[test]
    fn signed_after_bundling() {
        let exe = bundle(&pe(512, None), b"signed package").unwrap();
        // signtool: pad to 8 bytes, append the certificate table and point
        // the security directory at it.
        let mut signed = exe.clone();
        while signed.len() % 8 != 0 {
            signed.push(0);
        }
        let table = signed.len() as u32;
        signed.extend_from_slice(&[0xAB; 40]);
        let opt = 0x80 + 24;
        signed[opt + 144..opt + 148].copy_from_slice(&table.to_le_bytes());
        signed[opt + 148..opt + 152].copy_from_slice(&40u32.to_le_bytes());
        assert_eq!(payload(&signed), Ok(&b"signed package"[..]));
    }
}

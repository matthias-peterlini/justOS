//! The `.jpk` package format: the binary container executed by JustVM.
//!
//! ```text
//! offset  size  field
//! 0       4     magic  "JPK\0"  (0x4A 0x50 0x4B 0x00)
//! 4       2     format version (u16 LE)
//! 6       2     flags (bit 0: debug info present)
//! 8       4     section count N (u32 LE)
//! 12      4     CRC-32 of bytes 4..12 followed by the section table
//! 16      24*N  section table
//! ...           section payloads
//! ```
//!
//! Each section table entry:
//!
//! ```text
//! 0   4   tag        e.g. "MANI", "CPOL", "CODE", "ASST"
//! 4   4   flags      bit 0: payload is DEFLATE-compressed
//! 8   4   offset     from start of file
//! 12  4   stored length (bytes in file)
//! 16  4   raw length (bytes after decompression)
//! 20  4   CRC-32 of the raw (decompressed) payload
//! ```
//!
//! Readers bounds-check every offset, cap decompression at the declared raw
//! length and verify all checksums, so a malformed or hostile file is
//! rejected with an error instead of panicking.

use crate::bytecode::{put_str, ByteReader, DecodeError, Program};
use crate::crc32::crc32;
use crate::prelude::*;
use core::fmt;

pub const MAGIC: [u8; 4] = *b"JPK\0";
pub const FORMAT_VERSION: u16 = 1;
pub const FLAG_DEBUG_INFO: u16 = 1;

const HEADER_LEN: usize = 16;
const ENTRY_LEN: usize = 24;
const SECTION_DEFLATE: u32 = 1;
/// Upper bound for any single decompressed section (256 MiB).
const MAX_SECTION_LEN: u32 = 256 << 20;

pub const TAG_MANIFEST: [u8; 4] = *b"MANI";
pub const TAG_CONSTANTS: [u8; 4] = *b"CPOL";
pub const TAG_CODE: [u8; 4] = *b"CODE";
pub const TAG_ASSETS: [u8; 4] = *b"ASST";
pub const TAG_TYPES: [u8; 4] = *b"TYPE";

/// Prefix of the application ids of JustScript apps on the justOS desktop.
pub const APP_ID_PREFIX: &str = "dev.justos.";

/// The application id of the app `name`: `dev.justos.` followed by the name
/// in lowercase, with every run of other characters turned into `-`
/// (`"My App"` becomes `dev.justos.my-app`). It is the Wayland app id / X11
/// `WM_CLASS` of the window, the name of the `.desktop` file and of the icon,
/// and the `app_icon` of notifications, so the desktop can match them.
pub fn app_id(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let mut out = out.trim_end_matches('-').to_owned();
    if !out.starts_with(|c: char| c.is_ascii_lowercase()) {
        out.insert_str(0, "app-");
    }
    format!("{APP_ID_PREFIX}{}", out.trim_end_matches('-'))
}

/// Check an explicit application id: at least two dot-separated elements
/// of ASCII letters, digits, `-` and `_`, none empty or starting with a digit.
pub fn check_app_id(id: &str) -> Result<(), String> {
    let elements: Vec<&str> = id.split('.').collect();
    let valid = id.len() <= 255
        && elements.len() >= 2
        && elements.iter().all(|e| {
            e.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                && e.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        });
    if valid {
        Ok(())
    } else {
        Err(format!("invalid application id {id:?}: expected a reverse domain name such as \"{APP_ID_PREFIX}notes\""))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageError {
    NotAJpk,
    UnsupportedVersion(u16),
    Corrupt(String),
    MissingSection(&'static str),
    Decode(DecodeError),
}

impl fmt::Display for PackageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PackageError::NotAJpk => write!(f, "not an JustScript package (bad magic bytes)"),
            PackageError::UnsupportedVersion(v) => {
                write!(f, "unsupported .jpk format version {v} (this runtime supports {FORMAT_VERSION})")
            }
            PackageError::Corrupt(why) => write!(f, "corrupt .jpk package: {why}"),
            PackageError::MissingSection(tag) => write!(f, "corrupt .jpk package: missing `{tag}` section"),
            PackageError::Decode(e) => write!(f, "{e}"),
        }
    }
}

impl core::error::Error for PackageError {}

impl From<DecodeError> for PackageError {
    fn from(e: DecodeError) -> Self {
        PackageError::Decode(e)
    }
}

/// Ordered `key = value` metadata.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Manifest {
    entries: Vec<(String, String)>,
}

impl Manifest {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        let (key, value) = (key.into(), value.into());
        match self.entries.iter_mut().find(|(k, _)| *k == key) {
            Some(entry) => entry.1 = value,
            None => self.entries.push((key, value)),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());
        for (k, v) in &self.entries {
            put_str(&mut out, k);
            put_str(&mut out, v);
        }
        out
    }

    fn decode(data: &[u8]) -> Result<Self, DecodeError> {
        let mut r = ByteReader::new(data);
        let n = r.count(8)?;
        let entries = (0..n).map(|_| Ok((r.string()?, r.string()?))).collect::<Result<_, DecodeError>>()?;
        Ok(Manifest { entries })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Jpk {
    pub manifest: Manifest,
    pub program: Program,
    pub assets: Vec<Asset>,
}

impl Jpk {
    pub fn new(program: Program) -> Self {
        Jpk { manifest: Manifest::default(), program, assets: Vec::new() }
    }

    /// Serialize the package. Sections are DEFLATE-compressed when that
    /// actually makes them smaller.
    pub fn to_bytes(&self, compress: bool) -> Vec<u8> {
        let mut sections: Vec<([u8; 4], Vec<u8>)> = vec![
            (TAG_MANIFEST, self.manifest.encode()),
            (TAG_CONSTANTS, self.program.encode_constants()),
            (TAG_CODE, self.program.encode_code()),
        ];
        if !self.program.types.is_empty() {
            sections.push((TAG_TYPES, self.program.encode_types()));
        }
        if !self.assets.is_empty() {
            let mut out = Vec::new();
            out.extend_from_slice(&(self.assets.len() as u32).to_le_bytes());
            for a in &self.assets {
                put_str(&mut out, &a.name);
                out.extend_from_slice(&(a.data.len() as u32).to_le_bytes());
                out.extend_from_slice(&a.data);
            }
            sections.push((TAG_ASSETS, out));
        }

        let has_debug = self.program.functions.iter().any(|f| !f.lines.is_empty());
        let mut table = Vec::with_capacity(sections.len() * ENTRY_LEN);
        let mut payload = Vec::new();
        let data_start = HEADER_LEN + sections.len() * ENTRY_LEN;
        for (tag, raw) in &sections {
            let packed = compress.then(|| crate::deflate::deflate(raw)).filter(|p| p.len() < raw.len());
            let (flags, stored) = match &packed {
                Some(p) => (SECTION_DEFLATE, p.as_slice()),
                None => (0, raw.as_slice()),
            };
            table.extend_from_slice(tag);
            table.extend_from_slice(&flags.to_le_bytes());
            table.extend_from_slice(&((data_start + payload.len()) as u32).to_le_bytes());
            table.extend_from_slice(&(stored.len() as u32).to_le_bytes());
            table.extend_from_slice(&(raw.len() as u32).to_le_bytes());
            table.extend_from_slice(&crc32(raw).to_le_bytes());
            payload.extend_from_slice(stored);
        }

        let mut out = Vec::with_capacity(data_start + payload.len());
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        out.extend_from_slice(&(if has_debug { FLAG_DEBUG_INFO } else { 0 }).to_le_bytes());
        out.extend_from_slice(&(sections.len() as u32).to_le_bytes());
        let header_crc = header_checksum(&out[4..12], &table);
        out.extend_from_slice(&header_crc.to_le_bytes());
        out.extend_from_slice(&table);
        out.extend_from_slice(&payload);
        out
    }

    /// Parse, decompress, checksum and verify a package.
    pub fn from_bytes(data: &[u8]) -> Result<Self, PackageError> {
        if data.len() < HEADER_LEN || data[..4] != MAGIC {
            return Err(PackageError::NotAJpk);
        }
        let u16_at = |o: usize| u16::from_le_bytes([data[o], data[o + 1]]);
        let u32_at = |o: usize| u32::from_le_bytes(data[o..o + 4].try_into().unwrap());
        let version = u16_at(4);
        if version != FORMAT_VERSION {
            return Err(PackageError::UnsupportedVersion(version));
        }
        let count = u32_at(8) as usize;
        let table_end = count
            .checked_mul(ENTRY_LEN)
            .and_then(|n| n.checked_add(HEADER_LEN))
            .filter(|&e| e <= data.len())
            .ok_or_else(|| PackageError::Corrupt("section table exceeds file size".into()))?;
        let table = &data[HEADER_LEN..table_end];
        if header_checksum(&data[4..12], table) != u32_at(12) {
            return Err(PackageError::Corrupt("section table checksum mismatch".into()));
        }

        let mut manifest = None;
        let mut constants = None;
        let mut code = None;
        let mut types = Vec::new();
        let mut assets = Vec::new();
        for entry in table.as_chunks::<ENTRY_LEN>().0 {
            let tag: [u8; 4] = entry[..4].try_into().unwrap();
            let field = |i: usize| u32::from_le_bytes(entry[4 + i * 4..8 + i * 4].try_into().unwrap());
            let (flags, offset, stored_len, raw_len, crc) = (field(0), field(1) as usize, field(2) as usize, field(3), field(4));
            let tag_name = String::from_utf8_lossy(&tag).into_owned();
            let stored = offset
                .checked_add(stored_len)
                .and_then(|end| data.get(offset..end))
                .ok_or_else(|| PackageError::Corrupt(format!("section `{tag_name}` out of bounds")))?;
            if raw_len > MAX_SECTION_LEN {
                return Err(PackageError::Corrupt(format!("section `{tag_name}` is too large")));
            }
            let raw = if flags & SECTION_DEFLATE != 0 {
                crate::deflate::inflate(stored, raw_len as usize)
                    .map_err(|_| PackageError::Corrupt(format!("section `{tag_name}` failed to decompress")))?
            } else {
                stored.to_vec()
            };
            if raw.len() != raw_len as usize || crc32(&raw) != crc {
                return Err(PackageError::Corrupt(format!("section `{tag_name}` checksum mismatch")));
            }
            match tag {
                TAG_MANIFEST => manifest = Some(Manifest::decode(&raw)?),
                TAG_CONSTANTS => constants = Some(raw),
                TAG_CODE => code = Some(raw),
                TAG_TYPES => types = raw,
                TAG_ASSETS => assets = decode_assets(&raw)?,
                _ => {} // Unknown sections are skipped for forward compatibility.
            }
        }
        let constants = constants.ok_or(PackageError::MissingSection("CPOL"))?;
        let code = code.ok_or(PackageError::MissingSection("CODE"))?;
        let program = Program::decode(&constants, &code, &types)?;
        Ok(Jpk { manifest: manifest.unwrap_or_default(), program, assets })
    }
}

fn header_checksum(header_fields: &[u8], table: &[u8]) -> u32 {
    let mut buf = Vec::with_capacity(header_fields.len() + table.len());
    buf.extend_from_slice(header_fields);
    buf.extend_from_slice(table);
    crc32(&buf)
}

fn decode_assets(data: &[u8]) -> Result<Vec<Asset>, DecodeError> {
    let mut r = ByteReader::new(data);
    let n = r.count(8)?;
    (0..n)
        .map(|_| {
            let name = r.string()?;
            let len = r.u32()? as usize;
            Ok(Asset { name, data: r.bytes(len)?.to_vec() })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{compile_source, CompileOptions};

    fn sample() -> Jpk {
        let src = "let greeting = \"hello\"\nfn shout(s)\n    return s + \"!\"\nprint(shout(greeting))\n";
        let out = compile_source("sample.jt", src, CompileOptions::DEBUG).unwrap();
        let mut pkg = Jpk::new(out.program);
        pkg.manifest.set("name", "sample");
        pkg.manifest.set("version", "1.2.3");
        pkg.assets.push(Asset { name: "logo.txt".into(), data: b"JPK".repeat(100) });
        pkg
    }

    #[test]
    fn roundtrip_compressed_and_plain() {
        let pkg = sample();
        for compress in [true, false] {
            let bytes = pkg.to_bytes(compress);
            assert_eq!(&bytes[..4], b"JPK\0");
            let back = Jpk::from_bytes(&bytes).unwrap();
            assert_eq!(back, pkg);
            assert_eq!(back.manifest.get("version"), Some("1.2.3"));
        }
        assert!(pkg.to_bytes(true).len() < pkg.to_bytes(false).len());
    }

    #[test]
    fn app_ids() {
        assert_eq!(app_id("aurora"), "dev.justos.aurora");
        assert_eq!(app_id("My App!"), "dev.justos.my-app");
        assert_eq!(app_id("  Note_Book 2 "), "dev.justos.note-book-2");
        assert_eq!(app_id("2048"), "dev.justos.app-2048");
        assert_eq!(app_id(""), "dev.justos.app");
        for id in ["dev.justos.notes", "dev.justos.my-app", "com.example.App_2"] {
            assert!(check_app_id(id).is_ok(), "{id}");
            assert!(check_app_id(&app_id(id)).is_ok());
        }
        for id in ["notes", "org..notes", "dev.justos.2048", "dev.justos.a b", "dev.justos."] {
            assert!(check_app_id(id).is_err(), "{id}");
        }
    }

    #[test]
    fn rejects_corruption_without_panicking() {
        let bytes = sample().to_bytes(true);
        assert_eq!(Jpk::from_bytes(b"MZ\0\0garbage-garbage"), Err(PackageError::NotAJpk));
        for cut in 0..bytes.len() {
            assert!(Jpk::from_bytes(&bytes[..cut]).is_err(), "truncated at {cut} accepted");
        }
        for i in 4..bytes.len() {
            let mut evil = bytes.clone();
            evil[i] ^= 0x5a;
            // Must never panic; flipping a byte must be detected.
            assert!(Jpk::from_bytes(&evil).is_err(), "flip at {i} accepted");
        }
    }
}

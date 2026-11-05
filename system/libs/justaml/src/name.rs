//! Names in the ACPI namespace: four-character segments and the paths made
//! of them (ACPI 6.5, 20.2.2).

use crate::Error;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

/// One name segment, four characters padded with `_` (`_SB_`, `PCI0`).
pub type Seg = [u8; 4];

/// A name as written in AML or in text: from the root (`\`), or some
/// levels up from the current scope (`^`), then the segments.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct NameString {
    pub root: bool,
    pub up: u8,
    pub segs: Vec<Seg>,
}

/// Can a name string start with this byte?
pub(crate) fn is_lead(b: u8) -> bool {
    matches!(b, b'\\' | b'^' | 0x2E | 0x2F | b'_' | b'A'..=b'Z')
}

fn seg_char(b: u8) -> bool {
    matches!(b, b'_' | b'A'..=b'Z' | b'0'..=b'9')
}

/// A segment as text, without the `_` padding at its end (`_SB_` → `_SB`).
pub fn seg_str(s: &Seg) -> &str {
    let mut len = 4;
    while len > 1 && s[len - 1] == b'_' {
        len -= 1;
    }
    core::str::from_utf8(&s[..len]).unwrap_or("????")
}

impl NameString {
    /// Reads a NameString from AML.
    pub(crate) fn parse(c: &[u8], pos: &mut usize) -> Result<NameString, Error> {
        let mut n = NameString::default();
        if c.get(*pos) == Some(&b'\\') {
            n.root = true;
            *pos += 1;
        } else {
            while c.get(*pos) == Some(&b'^') {
                n.up = n.up.saturating_add(1);
                *pos += 1;
            }
        }
        let count = match *c.get(*pos).ok_or(Error::Truncated)? {
            0x00 => {
                *pos += 1;
                0
            }
            0x2E => {
                *pos += 1;
                2
            }
            0x2F => {
                *pos += 1;
                let k = *c.get(*pos).ok_or(Error::Truncated)? as usize;
                *pos += 1;
                k
            }
            _ => 1,
        };
        for _ in 0..count {
            let s: Seg = c.get(*pos..*pos + 4).ok_or(Error::Truncated)?.try_into().unwrap();
            if !(s[0] == b'_' || s[0].is_ascii_uppercase()) || !s.iter().all(|&b| seg_char(b)) {
                return Err(Error::BadName(*pos));
            }
            *pos += 4;
            n.segs.push(s);
        }
        Ok(n)
    }

    /// Reads a path written as text: `\_SB.PCI0._STA`, `^^FOO`, `_STA`.
    /// Segments shorter than four characters are padded with `_`.
    pub fn parse_str(s: &str) -> Option<NameString> {
        let mut n = NameString::default();
        let mut rest = s;
        if let Some(r) = rest.strip_prefix('\\') {
            n.root = true;
            rest = r;
        }
        while let Some(r) = rest.strip_prefix('^') {
            n.up += 1;
            rest = r;
        }
        if rest.is_empty() {
            return Some(n);
        }
        for part in rest.split('.') {
            let b = part.as_bytes();
            if b.is_empty() || b.len() > 4 || b[0].is_ascii_digit() || !b.iter().all(|&c| seg_char(c)) {
                return None;
            }
            let mut seg = [b'_'; 4];
            seg[..b.len()].copy_from_slice(b);
            n.segs.push(seg);
        }
        Some(n)
    }

    /// A name of one segment.
    pub fn single(seg: Seg) -> NameString {
        NameString { root: false, up: 0, segs: vec![seg] }
    }

    /// A plain one-segment name, which is also looked for in the scopes
    /// around the current one (ACPI 6.5, 5.3: namespace search rules).
    pub(crate) fn searchable(&self) -> bool {
        !self.root && self.up == 0 && self.segs.len() == 1
    }
}

impl fmt::Display for NameString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.root {
            f.write_str("\\")?;
        }
        for _ in 0..self.up {
            f.write_str("^")?;
        }
        for (i, s) in self.segs.iter().enumerate() {
            if i > 0 {
                f.write_str(".")?;
            }
            f.write_str(seg_str(s))?;
        }
        Ok(())
    }
}

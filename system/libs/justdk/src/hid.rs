//! HID report descriptors: which fields an input device's reports carry,
//! where they sit and what they mean.
//!
//! Only the input reports are described, and only what the justOS input
//! layer uses: keys (keyboard page), pointer axes (X, Y, wheel), buttons.

use alloc::vec::Vec;

pub const PAGE_DESKTOP: u16 = 0x01;
pub const PAGE_KEYBOARD: u16 = 0x07;
pub const PAGE_BUTTON: u16 = 0x09;
pub const PAGE_CONSUMER: u16 = 0x0C;

pub const USAGE_X: u16 = 0x30;
pub const USAGE_Y: u16 = 0x31;
pub const USAGE_WHEEL: u16 = 0x38;

/// One `Input` main item: `count` values of `size` bits each.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Field {
    /// 0 when the device uses no report ids.
    pub report_id: u8,
    /// Bit offset in the report (after the id byte, if any).
    pub offset: u32,
    pub size: u8,
    pub count: u16,
    pub page: u16,
    /// Explicit usages (low 16 bits; a page in the high bits overrides
    /// `page`), or empty with `usage_min..=usage_max`.
    pub usages: Vec<u32>,
    pub usage_min: u32,
    pub usage_max: u32,
    pub logical_min: i32,
    pub logical_max: i32,
    /// Variable (one usage per value) rather than array (values are
    /// indices into the usages).
    pub variable: bool,
    pub relative: bool,
    pub constant: bool,
}

impl Field {
    /// The (page, usage) of the `i`-th value of a variable field, or of the
    /// array index `i` of an array field.
    pub fn usage(&self, i: u32) -> (u16, u16) {
        let raw = if !self.usages.is_empty() {
            *self.usages.get(i as usize).or(self.usages.last()).unwrap()
        } else {
            self.usage_min.saturating_add(i).min(self.usage_max.max(self.usage_min))
        };
        let page = if raw > 0xFFFF { (raw >> 16) as u16 } else { self.page };
        (page, raw as u16)
    }

    /// Reads value `i` from a report (without the id byte). Signed when
    /// the logical minimum is negative.
    pub fn value(&self, report: &[u8], i: u32) -> Option<i32> {
        let bit = self.offset + i * self.size as u32;
        let raw = bits(report, bit, self.size as u32)?;
        if self.logical_min < 0 && self.size < 32 && raw & (1 << (self.size - 1)) != 0 {
            Some((raw | !((1u32 << self.size) - 1)) as i32)
        } else {
            Some(raw as i32)
        }
    }
}

fn bits(data: &[u8], start: u32, len: u32) -> Option<u32> {
    if len == 0 || len > 32 || (start + len).div_ceil(8) as usize > data.len() {
        return None;
    }
    let mut v: u64 = 0;
    let first = (start / 8) as usize;
    let last = ((start + len - 1) / 8) as usize;
    for (n, i) in (first..=last).enumerate() {
        v |= (data[i] as u64) << (8 * n);
    }
    v >>= start % 8;
    Some((v & ((1u64 << len) - 1)) as u32)
}

#[derive(Clone, Copy, Default)]
struct Globals {
    page: u16,
    logical_min: i32,
    logical_max: i32,
    size: u8,
    count: u16,
    report_id: u8,
}

/// The input fields of a report descriptor.
pub fn parse(desc: &[u8]) -> Vec<Field> {
    let mut fields = Vec::new();
    let mut g = Globals::default();
    let mut stack: Vec<Globals> = Vec::new();
    let mut usages: Vec<u32> = Vec::new();
    let (mut umin, mut umax) = (0u32, 0u32);
    // Bit offset reached in every report id.
    let mut offsets: Vec<(u8, u32)> = Vec::new();
    let mut i = 0;
    while i < desc.len() {
        let b = desc[i];
        if b == 0xFE {
            // Long item: size in the next byte.
            let len = desc.get(i + 1).copied().unwrap_or(0) as usize;
            i += 3 + len;
            continue;
        }
        let size = match b & 3 {
            3 => 4,
            n => n as usize,
        };
        if i + 1 + size > desc.len() {
            break;
        }
        let data = &desc[i + 1..i + 1 + size];
        let unsigned = data.iter().rev().fold(0u32, |a, &x| a << 8 | x as u32);
        let signed = match size {
            1 => data[0] as i8 as i32,
            2 => i16::from_le_bytes([data[0], data[1]]) as i32,
            4 => unsigned as i32,
            _ => 0,
        };
        let kind = (b >> 2) & 3;
        let tag = b >> 4;
        match (kind, tag) {
            // Main items
            (0, 8) => {
                let at = match offsets.iter_mut().find(|(id, _)| *id == g.report_id) {
                    Some((_, off)) => off,
                    None => {
                        offsets.push((g.report_id, 0));
                        &mut offsets.last_mut().unwrap().1
                    }
                };
                let field = Field {
                    report_id: g.report_id,
                    offset: *at,
                    size: g.size,
                    count: g.count,
                    page: g.page,
                    usages: core::mem::take(&mut usages),
                    usage_min: umin,
                    usage_max: umax,
                    logical_min: g.logical_min,
                    logical_max: g.logical_max,
                    variable: unsigned & 2 != 0,
                    relative: unsigned & 4 != 0,
                    constant: unsigned & 1 != 0,
                };
                *at += g.size as u32 * g.count as u32;
                if !field.constant && g.size > 0 && g.size <= 32 {
                    fields.push(field);
                }
                umin = 0;
                umax = 0;
            }
            (0, _) => {
                // Output, feature, collections: local state ends here.
                usages.clear();
                umin = 0;
                umax = 0;
            }
            // Global items
            (1, 0) => g.page = unsigned as u16,
            (1, 1) => g.logical_min = signed,
            (1, 2) => {
                // A maximum that only fits unsigned (0..255 in one byte).
                g.logical_max = if signed < g.logical_min { unsigned as i32 } else { signed }
            }
            (1, 7) => g.size = unsigned.min(255) as u8,
            (1, 8) => g.report_id = unsigned as u8,
            (1, 9) => g.count = unsigned.min(4096) as u16,
            (1, 10) => stack.push(g),
            (1, 11) => g = stack.pop().unwrap_or(g),
            // Local items
            (2, 0) => usages.push(if size == 4 { unsigned } else { unsigned & 0xFFFF }),
            (2, 1) => umin = unsigned,
            (2, 2) => umax = unsigned,
            _ => {}
        }
        i += 1 + size;
    }
    fields
}

/// Whether the reports start with a report id byte.
pub fn uses_report_ids(fields: &[Field]) -> bool {
    fields.iter().any(|f| f.report_id != 0)
}

/// What one report says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Keyboard usages held down (page 7), modifiers included.
    pub keys: Vec<u8>,
    pub has_keys: bool,
    /// Relative motion.
    pub dx: i32,
    pub dy: i32,
    pub wheel: i32,
    /// Absolute position scaled to 0..=`scale`, if the device reports one.
    pub abs_x: Option<i32>,
    pub abs_y: Option<i32>,
    /// Buttons 1..=8 as bits 0..=7.
    pub buttons: u8,
    pub has_pointer: bool,
}

/// Decodes a report (with its id byte, if the device uses ids).
/// Absolute axes are scaled to `0..=scale`.
pub fn decode(fields: &[Field], report: &[u8], scale: i32) -> Report {
    let mut out = Report::default();
    let (id, data) = if uses_report_ids(fields) {
        match report.split_first() {
            Some((&id, rest)) => (id, rest),
            None => return out,
        }
    } else {
        (0, report)
    };
    for f in fields.iter().filter(|f| f.report_id == id) {
        for i in 0..f.count as u32 {
            let Some(v) = f.value(data, i) else { break };
            if f.variable {
                let (page, usage) = f.usage(i);
                match (page, usage) {
                    (PAGE_KEYBOARD, u) => {
                        out.has_keys = true;
                        if v != 0 && u > 3 && u <= 0xFF {
                            out.keys.push(u as u8);
                        }
                    }
                    (PAGE_BUTTON, u) if (1..=8).contains(&u) => {
                        out.has_pointer = true;
                        if v != 0 {
                            out.buttons |= 1 << (u - 1);
                        }
                    }
                    (PAGE_DESKTOP, USAGE_X | USAGE_Y | USAGE_WHEEL) => {
                        out.has_pointer = true;
                        if f.relative {
                            match usage {
                                USAGE_X => out.dx += v,
                                USAGE_Y => out.dy += v,
                                _ => out.wheel += v,
                            }
                        } else {
                            let range = (f.logical_max as i64 - f.logical_min as i64).max(1);
                            let scaled = ((v as i64 - f.logical_min as i64).clamp(0, range) * scale as i64 / range) as i32;
                            match usage {
                                USAGE_X => out.abs_x = Some(scaled),
                                USAGE_Y => out.abs_y = Some(scaled),
                                _ => out.wheel += v,
                            }
                        }
                    }
                    _ => {}
                }
            } else {
                // Array: each value selects a usage.
                if v < f.logical_min || v > f.logical_max {
                    continue;
                }
                let (page, usage) = f.usage((v - f.logical_min) as u32);
                if page == PAGE_KEYBOARD {
                    out.has_keys = true;
                    // 1-3: error codes (too many keys held).
                    if usage > 3 && usage <= 0xFF {
                        out.keys.push(usage as u8);
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // The descriptors QEMU's usb-kbd, usb-mouse and usb-tablet report.
    const KEYBOARD: &[u8] = &[
        0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0x75, 0x01, 0x95, 0x08, 0x05, 0x07, 0x19, 0xe0, 0x29, 0xe7, 0x15, 0x00, 0x25, 0x01, 0x81, 0x02,
        0x95, 0x01, 0x75, 0x08, 0x81, 0x01, 0x95, 0x05, 0x75, 0x01, 0x05, 0x08, 0x19, 0x01, 0x29, 0x05, 0x91, 0x02, 0x95, 0x01, 0x75, 0x03,
        0x91, 0x01, 0x95, 0x06, 0x75, 0x08, 0x15, 0x00, 0x25, 0xff, 0x05, 0x07, 0x19, 0x00, 0x29, 0xff, 0x81, 0x00, 0xc0,
    ];
    const MOUSE: &[u8] = &[
        0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x09, 0x01, 0xa1, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29, 0x03, 0x15, 0x00, 0x25, 0x01, 0x95, 0x03,
        0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x05, 0x81, 0x01, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x09, 0x38, 0x15, 0x81, 0x25, 0x7f,
        0x75, 0x08, 0x95, 0x03, 0x81, 0x06, 0xc0, 0xc0,
    ];
    const TABLET: &[u8] = &[
        0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x09, 0x01, 0xa1, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29, 0x03, 0x15, 0x00, 0x25, 0x01, 0x95, 0x03,
        0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x05, 0x81, 0x01, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x00, 0x26, 0xff, 0x7f, 0x35,
        0x00, 0x46, 0xff, 0x7f, 0x75, 0x10, 0x95, 0x02, 0x81, 0x02, 0x05, 0x01, 0x09, 0x38, 0x15, 0x81, 0x25, 0x7f, 0x35, 0x00, 0x45, 0x00,
        0x75, 0x08, 0x95, 0x01, 0x81, 0x06, 0xc0, 0xc0,
    ];

    #[test]
    fn keyboard() {
        let f = parse(KEYBOARD);
        // Shift held, 'a' and 'b' pressed.
        let r = decode(&f, &[0x02, 0, 0x04, 0x05, 0, 0, 0, 0], 0x7FFF);
        assert!(r.has_keys);
        assert_eq!(r.keys, alloc::vec![0xE1, 0x04, 0x05]);
        let r = decode(&f, &[0, 0, 0, 0, 0, 0, 0, 0], 0x7FFF);
        assert!(r.keys.is_empty());
        // Too many keys: ErrorRollOver everywhere is ignored.
        let r = decode(&f, &[0, 0, 1, 1, 1, 1, 1, 1], 0x7FFF);
        assert!(r.keys.is_empty());
    }

    #[test]
    fn mouse() {
        let f = parse(MOUSE);
        let r = decode(&f, &[0x05, 0xFE, 0x03, 0x01], 0x7FFF);
        assert!(r.has_pointer && !r.has_keys);
        assert_eq!((r.buttons, r.dx, r.dy, r.wheel), (0x05, -2, 3, 1));
    }

    #[test]
    fn tablet() {
        let f = parse(TABLET);
        let r = decode(&f, &[0x01, 0xFF, 0x3F, 0xFF, 0x7F, 0x00], 0x7FFF);
        assert_eq!(r.buttons, 1);
        assert_eq!(r.abs_x, Some(0x3FFF));
        assert_eq!(r.abs_y, Some(0x7FFF));
    }

    #[test]
    fn report_ids() {
        // Two reports: id 1 a button, id 2 an X axis.
        let d = [
            0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x85, 0x01, 0x05, 0x09, 0x19, 0x01, 0x29, 0x01, 0x15, 0x00, 0x25, 0x01, 0x75, 0x08, 0x95,
            0x01, 0x81, 0x02, 0x85, 0x02, 0x05, 0x01, 0x09, 0x30, 0x15, 0x81, 0x25, 0x7f, 0x75, 0x08, 0x95, 0x01, 0x81, 0x06, 0xc0,
        ];
        let f = parse(&d);
        assert!(uses_report_ids(&f));
        assert_eq!(decode(&f, &[1, 1], 100).buttons, 1);
        assert_eq!(decode(&f, &[2, 0xFB], 100).dx, -5);
        assert_eq!(decode(&f, &[2, 0xFB], 100).buttons, 0);
    }
}

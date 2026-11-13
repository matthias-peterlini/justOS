//! Pictures: PNG, JPEG (baseline and progressive) and GIF into canvases.
//! The desktop draws the icons of Linux programs (PNG files named by their
//! .desktop files) with them.

#![no_std]

extern crate alloc;

mod gif;
mod jpeg;
mod png;

use justgfx::Canvas;

/// The largest picture we decode, in pixels (larger ones would take too
/// much memory for what a window shows).
pub const MAX_PIXELS: usize = 24 << 20;

/// Decodes a picture by its signature.
pub fn decode(data: &[u8]) -> Option<Canvas> {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        png::decode(data)
    } else if data.starts_with(&[0xFF, 0xD8]) {
        jpeg::decode(data)
    } else if data.starts_with(b"GIF8") {
        gif::decode(data)
    } else {
        None
    }
}

/// The size of a picture without decoding it, when its header says.
pub fn size(data: &[u8]) -> Option<(u32, u32)> {
    if data.starts_with(b"\x89PNG") && data.len() >= 24 {
        let be = |o: usize| u32::from_be_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]]);
        return Some((be(16), be(20)));
    }
    if data.starts_with(b"GIF8") && data.len() >= 10 {
        return Some((u16::from_le_bytes([data[6], data[7]]) as u32, u16::from_le_bytes([data[8], data[9]]) as u32));
    }
    if data.starts_with(&[0xFF, 0xD8]) {
        return jpeg::size(data);
    }
    None
}

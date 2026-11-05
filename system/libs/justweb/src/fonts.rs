//! The faces pages are drawn with: an atlas of sizes in a regular, a bold
//! and a monospaced style (`sys:/system/webfonts.bin`, made by
//! `tools/assets/gen_fonts.py --web`), and the choice of the nearest face
//! for a computed style.

use crate::Round;
use justgfx::{Canvas, Fonts};
use alloc::vec::Vec;

/// A face of the atlas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Face(pub u16);

pub struct WebFonts {
    pub fonts: Fonts,
    /// (style, size, face): style 0 regular, 1 bold, 2 monospaced.
    table: Vec<(u8, i32, usize)>,
}

impl WebFonts {
    pub fn new(fonts: Fonts) -> WebFonts {
        let table = (0..fonts.face_count())
            .map(|i| {
                let tag = fonts.face_size(i);
                (((tag >> 12) & 0xF) as u8, tag & 0xFFF, i)
            })
            .collect();
        WebFonts { fonts, table }
    }

    /// The face nearest to a size and style.
    pub fn face(&self, size: f32, bold: bool, mono: bool) -> Face {
        let want = if mono {
            2
        } else if bold {
            1
        } else {
            0
        };
        let size = size.rnd() as i32;
        let pick = |style: u8| self.table.iter().filter(|t| t.0 == style).min_by_key(|t| (t.1 - size).abs()).map(|t| t.2);
        let i = pick(want).or_else(|| pick(0)).unwrap_or(0);
        Face(i as u16)
    }

    pub fn width(&self, f: Face, s: &str) -> i32 {
        self.fonts.face_width(f.0 as usize, s)
    }

    pub fn advance(&self, f: Face, c: char) -> i32 {
        self.fonts.face_advance(f.0 as usize, c)
    }

    /// (line height, ascent).
    pub fn metrics(&self, f: Face) -> (i32, i32) {
        self.fonts.face_metrics(f.0 as usize)
    }

    pub fn draw(&self, c: &mut Canvas, f: Face, x: i32, y: i32, s: &str, color: u32) -> i32 {
        self.fonts.face_draw(c, f.0 as usize, x, y, s, color)
    }
}

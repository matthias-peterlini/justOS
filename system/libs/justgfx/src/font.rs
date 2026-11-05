//! Text, drawn from an anti-aliased glyph atlas (`fonts.bin`, rendered at
//! build time by `tools/assets/gen_fonts.py`).
//!
//! The atlas: "ARCF", a version, the number of faces, then for each face its
//! line height, ascent and glyph table (code point, size, offset, advance,
//! where its coverage mask starts); the masks follow, one byte per pixel.
//!
//! On a canvas with two pixels per unit, text is drawn from sharper faces
//! rendered at twice the size (`fonts@2x.bin`, the same faces in the same
//! order, or for the faces of web pages the face of twice the size in the
//! same atlas). Where each letter goes still comes from the normal faces,
//! so text measures the same at any scale.

use crate::gfx::Canvas;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

/// The fonts in the atlas, in its order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Font {
    Body = 0,
    Strong = 1,
    Small = 2,
    Heading = 3,
    Title = 4,
    Clock = 5,
    Mono = 6,
    Display = 7,
    /// Small bold capitals, spaced out ("OGGI", "SPAZI").
    Caps = 8,
}

#[derive(Clone, Copy)]
struct Glyph {
    code: u32,
    w: u8,
    h: u8,
    x: i8,
    y: i8,
    advance: u8,
    data: u32,
}

struct Face {
    size: u16,
    line: u16,
    ascent: i16,
    glyphs: Vec<Glyph>,
}

/// A parsed atlas.
pub struct Fonts {
    bytes: &'static [u8],
    faces: Vec<Face>,
    data: usize,
    sharp: Sharp,
}

/// Where the faces for two pixels per unit are.
enum Sharp {
    None,
    /// Another atlas, face for face.
    Atlas(Box<Fonts>),
    /// In this one: the face of twice the size and the same style.
    Doubled,
}

impl Fonts {
    /// Reads an atlas; `None` if it is not one.
    pub fn parse(bytes: &'static [u8]) -> Option<Fonts> {
        let u16_at = |o: usize| bytes.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
        let u32_at = |o: usize| bytes.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
        if bytes.get(..4)? != b"ARCF" {
            return None;
        }
        let count = u16_at(6)? as usize;
        let mut at = 8;
        let mut faces = Vec::new();
        for _ in 0..count {
            let size = u16_at(at)?;
            let line = u16_at(at + 2)?;
            let ascent = u16_at(at + 4)? as i16;
            let n = u16_at(at + 6)? as usize;
            at += 8;
            let mut glyphs = Vec::with_capacity(n);
            for _ in 0..n {
                let b = bytes.get(at..at + 14)?;
                glyphs.push(Glyph {
                    code: u32_at(at)?,
                    w: b[4],
                    h: b[5],
                    x: b[6] as i8,
                    y: b[7] as i8,
                    advance: b[8],
                    data: u32_at(at + 10)?,
                });
                at += 14;
            }
            glyphs.sort_by_key(|g| g.code);
            faces.push(Face { size, line, ascent, glyphs });
        }
        Some(Fonts { bytes, faces, data: at, sharp: Sharp::None })
    }

    /// Text on canvases with two pixels per unit comes from `atlas`, which
    /// holds the same faces rendered at twice the size.
    pub fn set_sharp(&mut self, atlas: Fonts) {
        self.sharp = Sharp::Atlas(Box::new(atlas));
    }

    /// Text on canvases with two pixels per unit comes from the face of
    /// twice the size in this atlas (the faces of web pages, whose size
    /// field is the size with the style in its high bits).
    pub fn double_sizes(&mut self) {
        self.sharp = Sharp::Doubled;
    }

    /// Are there faces for two pixels per unit?
    pub fn has_sharp(&self) -> bool {
        !matches!(self.sharp, Sharp::None)
    }

    /// The face that draws face number `face` on a canvas of `scale`.
    fn sharp_face(&self, face: usize, scale: i32) -> Option<(&Fonts, &Face)> {
        if scale != 2 {
            return None;
        }
        match &self.sharp {
            Sharp::None => None,
            Sharp::Atlas(a) => a.faces.get(face).map(|f| (&**a, f)),
            Sharp::Doubled => {
                let tag = self.faces.get(face)?.size;
                let want = (tag & 0xF000) | ((tag & 0x0FFF) * 2);
                self.faces.iter().find(|f| f.size == want).map(|f| (self, f))
            }
        }
    }

    fn index(&self, font: Font) -> usize {
        (font as usize).min(self.faces.len() - 1)
    }

    fn face(&self, font: Font) -> &Face {
        &self.faces[self.index(font)]
    }

    /// How many faces the atlas holds (an atlas of web faces is addressed
    /// by number rather than by [`Font`]).
    pub fn face_count(&self) -> usize {
        self.faces.len()
    }

    /// The pixel size a face was rendered at.
    pub fn face_size(&self, face: usize) -> i32 {
        self.faces.get(face).map_or(0, |f| f.size as i32)
    }

    /// Line height and ascent of a face by number.
    pub fn face_metrics(&self, face: usize) -> (i32, i32) {
        self.faces.get(face).map_or((0, 0), |f| (f.line as i32, f.ascent as i32))
    }

    /// Width of one character in a face by number.
    pub fn face_advance(&self, face: usize, c: char) -> i32 {
        self.faces.get(face).and_then(|f| Self::find(f, c)).map_or(0, |g| g.advance as i32)
    }

    /// Width of `s` in a face by number.
    pub fn face_width(&self, face: usize, s: &str) -> i32 {
        let Some(f) = self.faces.get(face) else { return 0 };
        s.chars().map(|c| Self::find(f, c).map_or(0, |g| g.advance as i32)).sum()
    }

    /// Draws `s` in a face by number, its top-left corner at (`x`, `y`).
    pub fn face_draw(&self, c: &mut Canvas, face: usize, x: i32, y: i32, s: &str, color: u32) -> i32 {
        if face >= self.faces.len() {
            return 0;
        }
        self.draw_face(c, face, x, y, s, color)
    }

    fn draw_face(&self, c: &mut Canvas, index: usize, x: i32, y: i32, s: &str, color: u32) -> i32 {
        let face = &self.faces[index];
        let base = y + face.ascent as i32;
        let sharp = self.sharp_face(index, c.scale());
        let mut pen = x;
        for ch in s.chars() {
            let Some(g) = Self::find(face, ch) else { continue };
            self.glyph(c, sharp, ch, &g, (pen, base), color);
            pen += g.advance as i32;
        }
        pen - x
    }

    /// Draws one letter with its origin at `at` (in units): from the sharp
    /// face if there is one, else `g`.
    fn glyph(&self, c: &mut Canvas, sharp: Option<(&Fonts, &Face)>, ch: char, g: &Glyph, at: (i32, i32), color: u32) {
        let (pen, base) = at;
        if let Some((atlas, hi)) = sharp {
            if let Some(h) = Self::find(hi, ch) {
                if h.w > 0 {
                    if let Some(mask) = atlas.mask(&h) {
                        let k = c.scale();
                        c.draw_mask_px(mask, h.w as i32, h.h as i32, pen * k + h.x as i32, base * k + h.y as i32, color);
                    }
                }
                return;
            }
        }
        if g.w > 0 {
            if let Some(mask) = self.mask(g) {
                c.draw_mask(mask, g.w as i32, g.h as i32, pen + g.x as i32, base + g.y as i32, color);
            }
        }
    }

    fn find(face: &Face, c: char) -> Option<Glyph> {
        let c = match c {
            '\u{a0}' => ' ',
            c => c,
        };
        let get = |c: char| face.glyphs.binary_search_by_key(&(c as u32), |g| g.code).ok().map(|i| face.glyphs[i]);
        get(c).or_else(|| {
            // A missing letter: try without its accent, else a question mark.
            get(match c {
                'á' | 'â' | 'ä' => 'a',
                'ê' | 'ë' => 'e',
                'í' | 'î' | 'ï' => 'i',
                'ó' | 'ô' | 'ö' => 'o',
                'ú' | 'û' | 'ü' => 'u',
                _ => '?',
            })
        })
    }

    fn mask(&self, g: &Glyph) -> Option<&'static [u8]> {
        let start = self.data + g.data as usize;
        self.bytes.get(start..start + g.w as usize * g.h as usize)
    }

    /// Width of `s` in pixels.
    pub fn width(&self, font: Font, s: &str) -> i32 {
        let face = self.face(font);
        s.chars().map(|c| Self::find(face, c).map_or(0, |g| g.advance as i32)).sum()
    }

    /// Width of one character.
    pub fn advance(&self, font: Font, c: char) -> i32 {
        Self::find(self.face(font), c).map_or(0, |g| g.advance as i32)
    }

    /// Line height and ascent (distance from the top of a line to its
    /// baseline).
    pub fn metrics(&self, font: Font) -> (i32, i32) {
        let face = self.face(font);
        (face.line as i32, face.ascent as i32)
    }

    /// Draws `s` with its top-left corner at (`x`, `y`). Returns the width.
    pub fn draw(&self, c: &mut Canvas, font: Font, x: i32, y: i32, s: &str, color: u32) -> i32 {
        self.draw_face(c, self.index(font), x, y, s, color)
    }

    /// Draws `s` centred horizontally on `cx`.
    pub fn draw_centered(&self, c: &mut Canvas, font: Font, cx: i32, y: i32, s: &str, color: u32) {
        let w = self.width(font, s);
        self.draw(c, font, cx - w / 2, y, s, color);
    }

    /// `s` shortened with "…" to fit `max` pixels.
    pub fn fit(&self, font: Font, s: &str, max: i32) -> String {
        if self.width(font, s) <= max {
            return s.into();
        }
        let ell = self.width(font, "…");
        let mut out = String::new();
        let mut w = 0;
        for ch in s.chars() {
            let a = self.advance(font, ch);
            if w + a + ell > max {
                break;
            }
            w += a;
            out.push(ch);
        }
        out.push('…');
        out
    }

    /// Splits `s` into lines that fit `max` pixels, breaking at spaces
    /// (and inside words longer than a line).
    pub fn wrap(&self, font: Font, s: &str, max: i32) -> Vec<String> {
        let mut lines = Vec::new();
        for para in s.split('\n') {
            let mut line = String::new();
            let mut w = 0;
            for word in para.split(' ') {
                let ww = self.width(font, word);
                let space = if line.is_empty() { 0 } else { self.advance(font, ' ') };
                if w + space + ww <= max {
                    if !line.is_empty() {
                        line.push(' ');
                    }
                    line.push_str(word);
                    w += space + ww;
                    continue;
                }
                if !line.is_empty() {
                    lines.push(core::mem::take(&mut line));
                    w = 0;
                }
                for ch in word.chars() {
                    let a = self.advance(font, ch);
                    if w + a > max && !line.is_empty() {
                        lines.push(core::mem::take(&mut line));
                        w = 0;
                    }
                    line.push(ch);
                    w += a;
                }
            }
            lines.push(line);
        }
        lines
    }

    /// Draws one glyph of the terminal font in a cell; false if the font
    /// lacks it.
    pub fn draw_cell_glyph(&self, c: &mut Canvas, x: i32, y: i32, ch: char, color: u32) -> bool {
        let index = self.index(Font::Mono);
        let face = &self.faces[index];
        let Ok(i) = face.glyphs.binary_search_by_key(&(ch as u32), |g| g.code) else { return false };
        let g = face.glyphs[i];
        let sharp = self.sharp_face(index, c.scale());
        self.glyph(c, sharp, ch, &g, (x, y + face.ascent as i32), color);
        true
    }
}

//! Text, drawn from the anti-aliased glyph atlas in `assets/fonts.bin`
//! (rendered at build time by `tools/assets/gen_fonts.py`), through
//! [`arcgfx::Fonts`].

use super::gfx::Canvas;
use alloc::string::String;
use alloc::vec::Vec;
pub use arcgfx::Font;
use arcgfx::Fonts;

static ATLAS: &[u8] = include_bytes!("../../assets/fonts.bin");
/// The same faces at twice the size, for two pixels per unit (gzipped:
/// unpacked only on such a screen).
static SHARP: &[u8] = include_bytes!("../../assets/fonts@2x.bin.gz");

static FACES: crate::sync::IrqLock<Option<Fonts>> = crate::sync::IrqLock::new(None);

fn with<T>(f: impl FnOnce(&Fonts) -> T) -> T {
    let mut guard = FACES.lock();
    let fonts = guard.get_or_insert_with(|| Fonts::parse(ATLAS).expect("bad font atlas"));
    f(fonts)
}

/// Unpacks the sharp faces (once): from now on text on canvases with two
/// pixels per unit uses them.
pub fn load_sharp() {
    if with(|f| f.has_sharp()) {
        return;
    }
    // Unpacked outside the lock: it takes a few milliseconds.
    let Some(atlas) = arczip::gunzip(SHARP).and_then(|raw| Fonts::parse(alloc::boxed::Box::leak(raw.into_boxed_slice()))) else {
        crate::warn!("gui", "bad sharp font atlas");
        return;
    };
    let mut guard = FACES.lock();
    let fonts = guard.get_or_insert_with(|| Fonts::parse(ATLAS).expect("bad font atlas"));
    if !fonts.has_sharp() {
        fonts.set_sharp(atlas);
    }
}

/// Frees the atlas lock (for the fatal error screen).
///
/// # Safety
/// Only when the machine is stopping and nothing else runs.
pub unsafe fn force_unlock() {
    // SAFETY: guaranteed by the caller.
    unsafe { FACES.force_unlock() };
}

/// Runs `f` with the parsed atlas (for many measurements at once).
pub fn fonts<T>(f: impl FnOnce(&Fonts) -> T) -> T {
    with(f)
}

/// Width of `s` in pixels.
pub fn width(font: Font, s: &str) -> i32 {
    with(|f| f.width(font, s))
}

/// Line height and ascent (distance from the top of a line to its baseline).
pub fn metrics(font: Font) -> (i32, i32) {
    with(|f| f.metrics(font))
}

/// Draws `s` with its top-left corner at (`x`, `y`). Returns the width.
pub fn draw(c: &mut Canvas, font: Font, x: i32, y: i32, s: &str, color: u32) -> i32 {
    with(|f| f.draw(c, font, x, y, s, color))
}

/// Draws `s` centred horizontally on `cx`.
pub fn draw_centered(c: &mut Canvas, font: Font, cx: i32, y: i32, s: &str, color: u32) {
    with(|f| f.draw_centered(c, font, cx, y, s, color))
}

/// Draws `s` so that it ends at `right`.
pub fn draw_right(c: &mut Canvas, font: Font, right: i32, y: i32, s: &str, color: u32) {
    with(|f| {
        let w = f.width(font, s);
        f.draw(c, font, right - w, y, s, color);
    })
}

/// `s` shortened with "…" to fit `max` pixels.
pub fn fit(font: Font, s: &str, max: i32) -> String {
    with(|f| f.fit(font, s, max))
}

/// `s` broken into lines of at most `max` pixels.
pub fn wrap(font: Font, s: &str, max: i32) -> Vec<String> {
    with(|f| f.wrap(font, s, max))
}

/// Width of one character.
pub fn advance(font: Font, c: char) -> i32 {
    with(|f| f.advance(font, c))
}

/// Draws one glyph of the terminal font in a cell; false if the font lacks it.
pub fn draw_cell_glyph(c: &mut Canvas, x: i32, y: i32, ch: char, color: u32) -> bool {
    with(|f| f.draw_cell_glyph(c, x, y, ch, color))
}

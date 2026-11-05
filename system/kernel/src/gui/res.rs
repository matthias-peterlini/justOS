//! Pictures: the icon atlas and the wallpapers in `sys:/system/gui`.
//!
//! Icons are stored once at 128 px; each size the interface asks for is
//! scaled on first use and kept, with the screen's pixels per unit (an icon
//! of 52 units is 104 pixels on a high-density screen). Wallpapers are
//! scaled to the screen's pixels too.

use super::gfx::{Canvas, Rect};
use super::qoi;
use crate::fs;
use crate::sync::IrqLock;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

const DIR: &str = "sys:/system/gui";
const CELL: i32 = 128;

struct Icons {
    atlas: Canvas,
    names: Vec<String>,
    /// By icon, size and scale.
    cache: BTreeMap<(usize, i32, i32), Canvas>,
    /// Icons of installed programs ("@sys:/apps/x.qoi"), by name, size and
    /// scale.
    files: BTreeMap<(String, i32, i32), Option<Canvas>>,
}

static ICONS: IrqLock<Option<Icons>> = IrqLock::new(None);

fn read(path: &str) -> Option<Vec<u8>> {
    fs::abs(path).and_then(|p| fs::read_file(&p)).ok()
}

/// Loads the icons; `progress` gets 0..=100.
pub fn load(progress: impl Fn(u32)) {
    progress(0);
    let atlas = read(&alloc::format!("{DIR}/icons.qoi")).and_then(|d| qoi::decode(&d));
    progress(60);
    let names = read(&alloc::format!("{DIR}/icons.idx"))
        .map(|b| String::from_utf8_lossy(&b).lines().map(String::from).collect())
        .unwrap_or_default();
    match atlas {
        Some(atlas) => *ICONS.lock() = Some(Icons { atlas, names, cache: BTreeMap::new(), files: BTreeMap::new() }),
        None => crate::warn!("gui", "no icons in {DIR}"),
    }
    // The common sizes now, so the first frames are quick.
    for name in ["files", "terminal", "settings", "info", "apps", "trash", "text-editor", "store"] {
        with_icon(name, 52, |_| {});
    }
    progress(100);
}

/// Runs `f` with icon `name` at `size` units (if there is such an icon), at
/// the screen's scale.
pub fn with_icon<T>(name: &str, size: i32, f: impl FnOnce(&Canvas) -> T) -> Option<T> {
    with_icon_at(name, size, super::scale(), f)
}

/// [`with_icon`] at `k` pixels per unit (for the canvas it is drawn on).
pub fn with_icon_at<T>(name: &str, size: i32, k: i32, f: impl FnOnce(&Canvas) -> T) -> Option<T> {
    if let Some(path) = name.strip_prefix('@') {
        let key = (String::from(path), size, k);
        let known = ICONS.lock().as_ref()?.files.contains_key(&key);
        if !known {
            // Read outside the lock: the file system may sleep.
            let img = read(path).and_then(|d| qoi::decode(&d)).map(|img| if img.w == size && k == 1 { img } else { img.scaled_at(size, size, k) });
            ICONS.lock().as_mut()?.files.insert(key.clone(), img);
        }
        let guard = ICONS.lock();
        return guard.as_ref()?.files.get(&key)?.as_ref().map(f);
    }
    let mut guard = ICONS.lock();
    let icons = guard.as_mut()?;
    let id = icons.names.iter().position(|n| n == name)?;
    if !icons.cache.contains_key(&(id, size, k)) {
        let cols = icons.atlas.w / CELL;
        let cell = Rect::new((id as i32 % cols) * CELL, (id as i32 / cols) * CELL, CELL, CELL);
        let img = icons.atlas.crop(cell);
        let img = if size == CELL && k == 1 { img } else { img.scaled_at(size, size, k) };
        icons.cache.insert((id, size, k), img);
    }
    Some(f(&icons.cache[&(id, size, k)]))
}

/// Draws an icon; a plain rounded square if it is missing.
pub fn draw_icon(c: &mut Canvas, name: &str, size: i32, x: i32, y: i32) {
    draw_icon_faded(c, name, size, x, y, 255);
}

pub fn draw_icon_faded(c: &mut Canvas, name: &str, size: i32, x: i32, y: i32, opacity: u32) {
    if with_icon_at(name, size, c.scale(), |img| c.draw_image(img, x, y, opacity)).is_none() {
        c.fill_round(Rect::new(x + size / 10, y + size / 10, size * 8 / 10, size * 8 / 10), size / 5, 0xC0A0_A8B8);
    }
}

/// Draws a glyph icon (white in the atlas) in `color`: its alpha is the mask.
pub fn draw_icon_tinted(c: &mut Canvas, name: &str, size: i32, x: i32, y: i32, color: u32) {
    with_icon_at(name, size, c.scale(), |img| c.draw_tinted(img, x, y, color));
}

/// A wallpaper scaled to cover `w` x `h` units (at the screen's scale), and
/// a blurred copy of it for the frosted-glass panels. A gradient if the file
/// is missing.
pub fn wallpaper(name: &str, w: i32, h: i32) -> (Canvas, Canvas) {
    let k = super::scale();
    let img = read(&alloc::format!("{DIR}/wallpapers/{name}.qoi")).and_then(|d| qoi::decode(&d));
    let sharp = match img {
        Some(img) => {
            // Cover: scale so both sides fill the screen, then crop the middle.
            let (sw, sh) = if (img.w as i64 * h as i64) > (img.h as i64 * w as i64) {
                ((img.w as i64 * h as i64 / img.h as i64) as i32, h)
            } else {
                (w, (img.h as i64 * w as i64 / img.w as i64) as i32)
            };
            let scaled = img.scaled_at(sw, sh, k);
            scaled.crop(Rect::new((sw - w) / 2, (sh - h) / 2, w, h))
        }
        None => {
            let mut c = Canvas::new_at(w, h, 0xFF2A_2038, k);
            c.fill_round_grad(c.bounds(), 0, 0xFF3B_2E5A, 0xFFE0_8F6E);
            c
        }
    };
    let mut sharp = sharp;
    for p in sharp.px.iter_mut() {
        *p |= 0xFF00_0000;
    }
    // The blur is cheaper on a quarter-size copy (one pixel per unit),
    // scaled back up.
    let mut soft = sharp.scaled_at(w / 4, h / 4, 1);
    soft.blur(6);
    let soft = soft.scaled_at(w, h, k);
    (sharp, soft)
}

/// A small preview of a wallpaper (for the settings and the setup), `w` x `h`
/// units at the screen's scale: from the ready-made one in `thumbs/` if there
/// is one, else from the full picture.
pub fn wallpaper_thumb(name: &str, w: i32, h: i32) -> Canvas {
    let img = read(&alloc::format!("{DIR}/wallpapers/thumbs/{name}.qoi"))
        .or_else(|| read(&alloc::format!("{DIR}/wallpapers/{name}.qoi")))
        .and_then(|d| qoi::decode(&d));
    match img {
        Some(img) => img.scaled_at(w, h, super::scale()),
        None => Canvas::new_at(w, h, 0xFF55_4466, super::scale()),
    }
}

/// An icon for a downloaded program (QOI, 128 pixels): a squircle in its
/// colour with its initials, and a terminal mark for text programs.
pub fn make_icon(name: &str, color: u32, platform: crate::store::Platform) -> Vec<u8> {
    use super::text::{self, Font};
    let mut c = Canvas::new_layer(CELL, CELL);
    let r = Rect::new(6, 6, CELL - 12, CELL - 12);
    let darker = super::gfx::mix(color, 0xFF10_0C10, 110);
    c.fill_round_grad(r, 30, color, darker);
    c.glow_round(r, 30, r.x + 24, r.y + 18, 90, 0xFFFF_FFFF, 60);
    let initials: String = {
        let words: Vec<&str> = name.split(|ch: char| !ch.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
        let mut s: String = words.iter().take(2).filter_map(|w| w.chars().next()).collect();
        if words.len() == 1 {
            s = words[0].chars().take(2).collect();
        }
        let mut cs = s.chars();
        match cs.next() {
            Some(f) => f.to_uppercase().chain(cs).collect(),
            None => String::from("?"),
        }
    };
    let (lh, _) = text::metrics(Font::Title);
    text::draw_centered(&mut c, Font::Title, CELL / 2, CELL / 2 - lh / 2 - 6, &initials, 0xFFFF_FFFF);
    // A badge: ">_" for Linux, a window for Windows.
    let badge = Rect::new(CELL - 46, CELL - 40, 34, 24);
    c.fill_round(badge, 8, 0x8C00_0000);
    let label = match platform {
        crate::store::Platform::Windows => "win",
        _ => ">_",
    };
    let (sh, _) = text::metrics(Font::Small);
    text::draw_centered(&mut c, Font::Small, badge.x + badge.w / 2, badge.y + (badge.h - sh) / 2, label, 0xFFFF_FFFF);
    super::qoi::encode(&c)
}

/// The photos of the Dolomites (Unsplash, see CREDITS.txt next to them)
/// first, then the painted ones. The first is the default.
pub const WALLPAPERS: [(&str, &str); 8] = [
    ("cadini", "Cadini"),
    ("braies", "Braies"),
    ("seceda", "Seceda"),
    ("siusi", "Alpe di Siusi"),
    ("brenta", "Brenta"),
    ("dolomiti", "Tramonto"),
    ("alba", "Alba"),
    ("notte", "Notte"),
];

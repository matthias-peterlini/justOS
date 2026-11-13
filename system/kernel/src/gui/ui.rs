//! Pieces the screens are built from: the theme's colours, panels, tiles,
//! soft-pixel symbols, buttons, text fields, and the loop that turns the
//! mouse and keyboard into events.

use super::cursor::Cursor;
use super::gfx::{Canvas, Rect};
use super::text::{self, Font};
use super::Display;
use crate::dev::input::{self, KeyEvent};
use core::sync::atomic::{AtomicBool, Ordering};

// ----------------------------------------------------------------------------
// The theme
//
// Surfaces are opaque: panels (windows, menus, cards) with a gradient from
// top to bottom, a thin line all round and a lighter line along the top;
// inside them darker (or, in the light theme, white) wells for content. The
// colours come from the theme the user chose, read when drawing: the
// functions below ([`text`], [`well`], [`line`]...) are cheap.

/// Dark or light surfaces.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Theme {
    Dark,
    Light,
}

impl Theme {
    /// The name in the settings file.
    pub fn name(self) -> &'static str {
        match self {
            Theme::Dark => "dark",
            Theme::Light => "light",
        }
    }

    pub fn from_name(s: &str) -> Theme {
        if s == "light" {
            Theme::Light
        } else {
            Theme::Dark
        }
    }
}

/// The colours of a theme (0xAARRGGBB).
pub struct Palette {
    /// The gradient of a panel, top to bottom.
    pub panel_top: u32,
    pub panel_bottom: u32,
    /// Inner panes, fields, cards.
    pub well: u32,
    pub text: u32,
    pub text_dim: u32,
    pub text_faint: u32,
    /// The thin line round panels, wells and buttons; separators.
    pub line: u32,
    /// The lighter line along the top of a panel.
    pub sheen: u32,
    /// The selected row or item, and the one under the pointer.
    pub sel: u32,
    pub hover: u32,
    /// A faint pane drawn on a well (rows, cards inside a window).
    pub pane: u32,
    /// An empty cell of a slider; the track of a switch that is off.
    pub cell: u32,
    /// The gradient of a grey tile (window buttons), and its symbol.
    pub knob_top: u32,
    pub knob_bottom: u32,
    pub knob_glyph: u32,
    /// How strong shadows are, in percent of the dark theme's.
    pub shadow: u32,
}

pub const DARK: Palette = Palette {
    panel_top: 0xFF23_2326,
    panel_bottom: 0xFF19_191B,
    well: 0xFF0F_0F10,
    text: 0xFFF5_F5F7,
    text_dim: 0xFFA1_A1A6,
    text_faint: 0x99A1_A1A6,
    line: 0x1AFF_FFFF,
    sheen: 0x24FF_FFFF,
    sel: 0x1AFF_FFFF,
    hover: 0x0FFF_FFFF,
    pane: 0x0AFF_FFFF,
    cell: 0x1FFF_FFFF,
    knob_top: 0xFF4A_4A4F,
    knob_bottom: 0xFF2A_2A2D,
    knob_glyph: 0xFFF2_EFE6,
    shadow: 100,
};

pub const LIGHT: Palette = Palette {
    panel_top: 0xFFFA_FAFC,
    panel_bottom: 0xFFEC_ECF0,
    well: 0xFFFF_FFFF,
    text: 0xFF1D_1D1F,
    text_dim: 0xFF6E_6E73,
    text_faint: 0x996E_6E73,
    line: 0x1400_0000,
    sheen: 0xFFFF_FFFF,
    sel: 0x0F00_0000,
    hover: 0x0800_0000,
    pane: 0x0500_0000,
    cell: 0x1A00_0000,
    knob_top: 0xFFFF_FFFF,
    knob_bottom: 0xFFD9_D9DF,
    knob_glyph: 0xFF2B_2B2E,
    shadow: 70,
};

/// The light theme is in use (kept here so that drawing never has to ask
/// the settings).
static LIGHT_THEME: AtomicBool = AtomicBool::new(false);

/// Draws with `t` from now on (the settings call it when they load or
/// change).
pub fn set_theme(t: Theme) {
    LIGHT_THEME.store(t == Theme::Light, Ordering::Relaxed);
}

pub fn theme() -> Theme {
    if LIGHT_THEME.load(Ordering::Relaxed) {
        Theme::Light
    } else {
        Theme::Dark
    }
}

/// The colours of the theme in use.
#[inline]
pub fn palette() -> &'static Palette {
    if LIGHT_THEME.load(Ordering::Relaxed) {
        &LIGHT
    } else {
        &DARK
    }
}

#[inline]
pub fn panel_top() -> u32 {
    palette().panel_top
}

#[inline]
pub fn panel_bottom() -> u32 {
    palette().panel_bottom
}

#[inline]
pub fn well() -> u32 {
    palette().well
}

#[inline]
pub fn text() -> u32 {
    palette().text
}

#[inline]
pub fn text_dim() -> u32 {
    palette().text_dim
}

#[inline]
pub fn text_faint() -> u32 {
    palette().text_faint
}

#[inline]
pub fn line() -> u32 {
    palette().line
}

#[inline]
pub fn sheen() -> u32 {
    palette().sheen
}

#[inline]
pub fn sel() -> u32 {
    palette().sel
}

#[inline]
pub fn hover() -> u32 {
    palette().hover
}

#[inline]
pub fn pane() -> u32 {
    palette().pane
}

#[inline]
pub fn cell() -> u32 {
    palette().cell
}

/// The grey tile's gradient, top and bottom.
#[inline]
pub fn knob_grad() -> (u32, u32) {
    let p = palette();
    (p.knob_top, p.knob_bottom)
}

/// The colour of a symbol on a grey tile.
#[inline]
pub fn knob_glyph() -> u32 {
    palette().knob_glyph
}

/// A shadow's `strength` (0..=255) in the theme in use.
#[inline]
pub fn shadow_strength(strength: u32) -> u32 {
    strength * palette().shadow / 100
}

// Fixed colours, the same in both themes.

/// The justOS orange (#ff5a1f): the text cursor, progress, focus.
pub const ACCENT: u32 = 0xFFFF_5A1F;
/// The orange of things that call for attention, the same justOS orange.
pub const ORANGE: u32 = 0xFFFF_5A1F;
/// The orange gradient of primary buttons, switches that are on and the
/// close button, top and bottom.
pub const ORANGE_GRAD: (u32, u32) = (0xFFFF_8A4C, 0xFFF0_4A10);
pub const RED: u32 = 0xFFFF_5A4E;
pub const GREEN: u32 = 0xFF34_C46A;
/// The glow in the corner of a card.
pub const GLOW: u32 = 0xFF8A_3416;
/// The light line along the top of a tile (whatever its colour).
pub const TILE_SHEEN: u32 = 0x47FF_FFFF;

// Text and glass straight on the wallpaper (the desktop clock, the
// launcher, the overview, the lock screen), where the theme does not reach:
// white, or ink where the picture is light.
pub const WHITE: u32 = 0xFFFF_FFFF;
pub const WHITE_DIM: u32 = 0xFFC9_C6CE;
pub const WHITE_FAINT: u32 = 0xFF94_919A;
pub const INK: u32 = 0xFF1B_1A1F;
pub const INK_DIM: u32 = 0xFF4A_4850;
/// Dark glass over the wallpaper (the overview's labels).
pub const GLASS_STRONG: u32 = 0xE02C_2B31;
/// Light glass over the wallpaper (where a window will snap).
pub const GLASS_LIGHT: u32 = 0x66DA_D6E2;
/// The light edge of glass over the wallpaper.
pub const GLASS_EDGE: u32 = 0x29FF_FFFF;

// ----------------------------------------------------------------------------
// Surfaces

/// The soft shadow under a surface, `size` units wide, at `strength`
/// (0..=255, weaker in the light theme), with a thin darker one right under
/// its edge.
pub fn drop_shadow(c: &mut Canvas, r: Rect, rad: i32, size: i32, strength: u32) {
    c.shadow(r, rad, size, shadow_strength(strength));
    c.shadow(r, rad, 3, shadow_strength(strength * 2 / 3));
}

/// A panel of the theme (window frames, menus, cards over the wallpaper):
/// opaque, with the panel gradient, a thin line all round and the sheen
/// along the top. No shadow: see [`drop_shadow`].
pub fn panel(c: &mut Canvas, r: Rect, rad: i32) {
    c.fill_round_grad(r, rad, panel_top(), panel_bottom());
    c.stroke_round(r, rad, 1, line());
    c.sheen_round(r, rad, 1, sheen());
}

/// A well inside a panel (a window's content, fields, cards): the well
/// colour with a thin line all round.
pub fn well_pane(c: &mut Canvas, r: Rect, rad: i32) {
    c.fill_round(r, rad, well());
    c.stroke_round(r, rad, 1, line());
}

/// A tile: a rounded rectangle with a vertical gradient (`grad`: top,
/// bottom), the light line along its top and a small shadow under it.
/// Square tiles take a corner of about a quarter of their side.
pub fn tile(c: &mut Canvas, r: Rect, rad: i32, grad: (u32, u32)) {
    c.shadow(r, rad, 2, shadow_strength(70));
    c.fill_round_grad(r, rad, grad.0, grad.1);
    c.sheen_round(r, rad, 1, TILE_SHEEN);
}

/// Lighter, for the thing under the pointer: `amount` (0..=255) of the way
/// to white.
pub fn lighten(c: u32, amount: u32) -> u32 {
    (super::gfx::mix(c, 0xFFFF_FFFF, amount) & 0xFF_FFFF) | (c & 0xFF00_0000)
}

/// [`lighten`] for both ends of a gradient.
pub fn lighten_grad(g: (u32, u32), amount: u32) -> (u32, u32) {
    (lighten(g.0, amount), lighten(g.1, amount))
}

/// Was a frosted-glass panel; now a [`panel`] of the theme with a shadow
/// (the blurred wallpaper and the tint are no longer used).
pub fn glass(c: &mut Canvas, _blur: &Canvas, r: Rect, rad: i32, _tint: u32) {
    drop_shadow(c, r, rad, 14, 70);
    panel(c, r, rad);
}

/// Was light frosted glass (the dock, widgets); now a [`panel`] of the
/// theme, with a shadow if asked.
pub fn light_glass(c: &mut Canvas, _blur: &Canvas, r: Rect, rad: i32, shadow: bool) {
    if shadow {
        drop_shadow(c, r, rad, 16, 55);
    }
    panel(c, r, rad);
}

// ----------------------------------------------------------------------------
// Soft pixels
//
// Symbols made of grid squares, as on justapps.it: corners that stick out
// are rounded by half a cell, inside corners filled in by 0.38 of a cell.
// A symbol is a few rows of text: '#' (or any other byte) fills a cell, '.'
// or ' ' leaves it empty. Cell sizes are in thousandths of a unit, so a
// symbol can have cells of 2.5 or 3.4 units and still look the same at any
// scale.

/// Minimise: a chevron pointing down.
pub const GLYPH_MIN: [&str; 5] = [".....", "#...#", ".#.#.", "..#..", "....."];
/// Full screen: two opposite corners.
pub const GLYPH_FULL: [&str; 5] = ["###..", "#....", "#...#", "....#", "..###"];
/// Close: a cross.
pub const GLYPH_CLOSE: [&str; 5] = ["#...#", ".#.#.", "..#..", ".#.#.", "#...#"];

/// The size of a soft-pixel symbol with cells `cell` thousandths of a unit
/// wide, in thousandths of a unit.
pub fn soft_size(rows: &[&str], cell: i32) -> (i32, i32) {
    let cols = rows.iter().map(|r| r.len()).max().unwrap_or(0) as i32;
    (cols * cell, rows.len() as i32 * cell)
}

/// Draws a soft-pixel symbol with its top-left corner at (`x`, `y`)
/// thousandths of a unit, each filled cell in `color(byte)`.
pub fn soft_pixels_at(c: &mut Canvas, rows: &[&str], x: i32, y: i32, cell: i32, color: &dyn Fn(u8) -> u32) {
    let cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if cols == 0 {
        return;
    }
    let mut keys = alloc::vec![0u8; cols * rows.len()];
    for (j, row) in rows.iter().enumerate() {
        for (i, &b) in row.as_bytes().iter().enumerate() {
            keys[j * cols + i] = if b == b'.' || b == b' ' { 0 } else { b };
        }
    }
    c.soft_grid(&keys, cols as i32, x, y, cell, color);
}

/// Draws a soft-pixel symbol in one colour, centred in `r`, cells `cell`
/// thousandths of a unit wide.
pub fn soft_pixels_in(c: &mut Canvas, rows: &[&str], r: Rect, cell: i32, color: u32) {
    soft_pixels_keyed(c, rows, r, cell, &|_| color);
}

/// Draws a soft-pixel symbol centred in `r`, each filled cell in
/// `color(byte)` ('o' for the orange dot of a picture, say).
pub fn soft_pixels_keyed(c: &mut Canvas, rows: &[&str], r: Rect, cell: i32, color: &dyn Fn(u8) -> u32) {
    let (w, h) = soft_size(rows, cell);
    soft_pixels_at(c, rows, r.x * 1000 + (r.w * 1000 - w) / 2, r.y * 1000 + (r.h * 1000 - h) / 2, cell, color);
}

/// Does text on the blurred wallpaper inside `r` read better dark? (The
/// glass is light over a bright sky and dark over the woods.)
pub fn wants_ink(blur: &Canvas, r: Rect) -> bool {
    // The light tint adds about a quarter of white.
    luminance(blur, r) * 3 / 4 + 0x40 > 128
}

/// The mean brightness (0..=255) of `blur` inside `r`, from a few samples.
pub fn luminance(blur: &Canvas, r: Rect) -> u32 {
    let mut sum = 0u64;
    let mut n = 0u64;
    let step_x = (r.w / 12).max(1);
    let step_y = (r.h / 4).max(1);
    let mut y = r.y + step_y / 2;
    while y < r.bottom() {
        let mut x = r.x + step_x / 2;
        while x < r.right() {
            let p = blur.get(x, y);
            let (rr, gg, bb) = ((p >> 16) & 0xFF, (p >> 8) & 0xFF, p & 0xFF);
            sum += (rr * 299 + gg * 587 + bb * 114) as u64 / 1000;
            n += 1;
            x += step_x;
        }
        y += step_y;
    }
    if n == 0 {
        255
    } else {
        (sum / n) as u32
    }
}

/// A glyph icon (white in the atlas) drawn in another colour.
pub fn glyph(c: &mut Canvas, name: &str, size: i32, x: i32, y: i32, color: u32) {
    super::res::draw_icon_tinted(c, name, size, x, y, color);
}

/// The window buttons: three tiles, minimise and full screen grey, close
/// orange, each with its soft-pixel symbol (`hover`: the one under the
/// pointer, a little lighter).
pub fn window_knobs(c: &mut Canvas, buttons: &[Rect; 3], hover: Option<usize>) {
    for (i, (b, glyph)) in buttons.iter().zip([&GLYPH_MIN, &GLYPH_FULL, &GLYPH_CLOSE]).enumerate() {
        let (grad, fg) = if i == 2 { (ORANGE_GRAD, WHITE) } else { (knob_grad(), knob_glyph()) };
        let grad = if hover == Some(i) { lighten_grad(grad, 40) } else { grad };
        tile(c, *b, 8, grad);
        // Five cells of 3/26 of the tile: 3 units on a 26-unit button.
        soft_pixels_in(c, glyph, *b, b.w * 3000 / 26, fg);
    }
}

/// The area a panel's shadow reaches.
pub fn with_shadow(r: Rect) -> Rect {
    r.grow(24).offset(0, 7)
}

/// A label in small spaced capitals ("OGGI", "CERCA NEL WEB").
pub fn caps(c: &mut Canvas, x: i32, y: i32, label: &str, color: u32) -> i32 {
    let mut cx = x;
    for ch in label.chars().flat_map(|ch| ch.to_uppercase()) {
        let mut buf = [0u8; 4];
        cx += text::draw(c, Font::Caps, cx, y, ch.encode_utf8(&mut buf), color) + 2;
    }
    cx - x
}

/// Width of [`caps`] text.
pub fn caps_width(label: &str) -> i32 {
    label.chars().flat_map(|ch| ch.to_uppercase()).map(|ch| text::advance(Font::Caps, ch) + 2).sum()
}

/// The corner of a button `h` units high.
fn button_rad(h: i32) -> i32 {
    14.min(h / 2)
}

/// A button: a well with a thin line; `primary` is orange with white text.
pub fn button(c: &mut Canvas, r: Rect, label: &str, primary: bool, hover: bool) {
    let rad = button_rad(r.h);
    let fg = if primary {
        let grad = if hover { lighten_grad(ORANGE_GRAD, 30) } else { ORANGE_GRAD };
        c.fill_round_grad(r, rad, grad.0, grad.1);
        c.sheen_round(r, rad, 1, TILE_SHEEN);
        WHITE
    } else {
        well_pane(c, r, rad);
        if hover {
            c.fill_round(r, rad, sel());
        }
        text()
    };
    let (lh, _) = text::metrics(Font::Strong);
    text::draw_centered(c, Font::Strong, r.x + r.w / 2, r.y + (r.h - lh) / 2, label, fg);
}

/// A button in a colour of its own: `bg` at the bottom, lighter at the
/// top, with the light line along its top.
pub fn button_color(c: &mut Canvas, r: Rect, label: &str, bg: u32, fg: u32) {
    let rad = button_rad(r.h);
    c.fill_round_grad(r, rad, lighten(bg, 40), bg);
    c.sheen_round(r, rad, 1, (TILE_SHEEN & 0xFF_FFFF) | (((TILE_SHEEN >> 24) * (bg >> 24) / 255) << 24));
    let (lh, _) = text::metrics(Font::Strong);
    text::draw_centered(c, Font::Strong, r.x + r.w / 2, r.y + (r.h - lh) / 2, label, fg);
}

/// A round button with a glyph from the icon set, in the theme's text
/// colour.
pub fn icon_button(c: &mut Canvas, r: Rect, glyph: &str, hover: bool) {
    let rad = button_rad(r.w.min(r.h));
    well_pane(c, r, rad);
    if hover {
        c.fill_round(r, rad, sel());
    }
    let s = r.w * 5 / 9;
    self::glyph(c, glyph, s, r.x + (r.w - s) / 2, r.y + (r.h - s) / 2, text());
}

/// An on/off switch: a rounded rectangle 44 x 26 in the middle of `r`,
/// orange when on, with a white knob.
pub fn toggle(c: &mut Canvas, r: Rect, on: bool) {
    let (w, h) = (r.w.min(44), r.h.min(26));
    let t = Rect::new(r.x + (r.w - w) / 2, r.y + (r.h - h) / 2, w, h);
    if on {
        c.fill_round_grad(t, 9, ORANGE_GRAD.0, ORANGE_GRAD.1);
        c.sheen_round(t, 9, 1, TILE_SHEEN);
    } else {
        c.fill_round(t, 9, cell());
    }
    let d = h - 6;
    let x = if on { t.right() - 3 - d } else { t.x + 3 };
    let knob = Rect::new(x, t.y + 3, d, d);
    c.shadow(knob, 6, 2, shadow_strength(90));
    c.fill_round_grad(knob, 6, 0xFFFF_FFFF, 0xFFE4_E4EA);
}

/// A row of 16 rounded cells in `r` that fill up from the left: `value`
/// in 0..=1000; filled cells orange (`accent`) or in the text colour (for
/// brightness, say).
pub fn cell_bar(c: &mut Canvas, r: Rect, value: u32, accent: bool) {
    const N: i32 = 16;
    const GAP: i32 = 3;
    let h = r.h.min(14);
    let y = r.y + (r.h - h) / 2;
    let filled = ((value.min(1000) * N as u32 + 500) / 1000) as i32;
    for i in 0..N {
        let x0 = r.x + i * (r.w + GAP) / N;
        let x1 = r.x + (i + 1) * (r.w + GAP) / N - GAP;
        let cell_r = Rect::new(x0, y, (x1 - x0).max(1), h);
        if i >= filled {
            c.fill_round(cell_r, 4, cell());
        } else if accent {
            c.fill_round_grad(cell_r, 4, ORANGE_GRAD.0, ORANGE_GRAD.1);
        } else {
            c.fill_round(cell_r, 4, text());
        }
    }
}

/// A horizontal slider; `value` in 0..=1000: a [`cell_bar`] in orange.
pub fn slider(c: &mut Canvas, r: Rect, value: u32) {
    cell_bar(c, r, value, true);
}

/// The slider value (0..=1000) for a pointer at `x`.
pub fn slider_value(r: Rect, x: i32) -> u32 {
    ((x - r.x).clamp(0, r.w.max(1)) as i64 * 1000 / r.w.max(1) as i64) as u32
}

/// The parts of a segmented control.
pub fn segments(r: Rect, n: usize) -> alloc::vec::Vec<Rect> {
    let n = n.max(1) as i32;
    let w = (r.w - 4) / n;
    (0..n).map(|i| Rect::new(r.x + 2 + i * w, r.y + 2, w, r.h - 4)).collect()
}

/// Tabs on a track: `labels`, the one at `sel` on a raised grey tile.
pub fn segmented(c: &mut Canvas, r: Rect, labels: &[&str], sel: usize) {
    // "Mese / Settimana / Giorno": a grey track, the choice raised on it.
    let rad = 12.min(r.h / 2);
    c.fill_round(r, rad, cell());
    let (lh, _) = text::metrics(Font::Strong);
    for (i, (s, label)) in segments(r, labels.len()).iter().zip(labels).enumerate() {
        if i == sel {
            tile(c, *s, (rad - 2).max(0), knob_grad());
        }
        let color = if i == sel { text() } else { text_dim() };
        text::draw_centered(c, Font::Strong, s.x + s.w / 2, s.y + (s.h - lh) / 2, label, color);
    }
}

/// The edge of a field: orange while typing into it.
fn field_edge(focused: bool) -> u32 {
    if focused {
        0xB3FF_5A1F
    } else {
        line()
    }
}

/// A search field with its magnifier.
pub fn search_field(c: &mut Canvas, r: Rect, value: &str, placeholder: &str, focused: bool) {
    let rad = (r.h / 2).min(12);
    c.fill_round(r, rad, well());
    c.stroke_round(r, rad, 1, field_edge(focused));
    glyph(c, "g-search", 14, r.x + 12, r.y + (r.h - 14) / 2, text_dim());
    let (lh, _) = text::metrics(Font::Body);
    let ty = r.y + (r.h - lh) / 2;
    if value.is_empty() {
        text::draw(c, Font::Body, r.x + 34, ty, placeholder, text_faint());
        if focused {
            c.fill(Rect::new(r.x + 34, ty + 2, 2, lh - 4), ACCENT);
        }
    } else {
        let shown = text::fit(Font::Body, value, r.w - 50);
        let w = text::draw(c, Font::Body, r.x + 34, ty, &shown, text());
        if focused {
            c.fill(Rect::new(r.x + 35 + w, ty + 2, 2, lh - 4), ACCENT);
        }
    }
}

/// A one-line text field.
pub fn text_field(c: &mut Canvas, r: Rect, value: &str, placeholder: &str, focused: bool) {
    c.fill_round(r, 14, well());
    c.stroke_round(r, 14, 1, field_edge(focused));
    let (lh, _) = text::metrics(Font::Heading);
    let ty = r.y + (r.h - lh) / 2;
    let shown = text::fit(Font::Heading, value, r.w - 36);
    let w = if value.is_empty() {
        text::draw(c, Font::Heading, r.x + 16, ty, placeholder, text_faint());
        0
    } else {
        text::draw(c, Font::Heading, r.x + 16, ty, &shown, text())
    };
    if focused {
        c.fill(Rect::new(r.x + 17 + w, ty + 2, 2, lh - 4), ACCENT);
    }
}

/// Applies a key to a text value; returns whether it changed.
pub fn edit_text(value: &mut alloc::string::String, ev: &KeyEvent, max: usize) -> bool {
    use crate::dev::input::Key;
    match ev.key {
        Key::Backspace => value.pop().is_some(),
        Key::Char(ch) if !ev.ctrl && !ch.is_control() && value.chars().count() < max => {
            value.push(ch);
            true
        }
        _ => false,
    }
}

/// What the event loop hands to a screen.
#[derive(Clone, Copy, Debug)]
pub enum Event {
    /// The left button went down (`double`: the second click of a pair).
    Press {
        x: i32,
        y: i32,
        double: bool,
    },
    /// The right button went down.
    Menu {
        x: i32,
        y: i32,
    },
    /// The left button went up.
    Release,
    /// The pointer moved (with the left button `held` or not).
    Move {
        x: i32,
        y: i32,
        held: bool,
    },
    /// The wheel turned (positive: scroll down).
    Wheel {
        lines: i32,
    },
    Key(KeyEvent),
    /// About ten times a second, for clocks and blinking.
    Tick,
    /// As often as the screen can be drawn, while the view animates.
    Frame,
}

/// A screen the loop drives: it handles events and draws damaged areas.
pub trait View {
    /// Returns true to leave the loop.
    fn event(&mut self, l: &mut Looper, ev: Event) -> bool;
    /// Draws everything inside `r` (the canvas is clipped to it).
    fn draw(&mut self, c: &mut Canvas, r: Rect);
    /// The screen changed size.
    fn resized(&mut self, _w: i32, _h: i32) {}
    /// Is something moving? The loop then sends [`Event::Frame`] for every
    /// frame instead of resting between events.
    fn animating(&self) -> bool {
        false
    }
}

// ----------------------------------------------------------------------------
// Animation

/// A transition that runs for a fixed time, whatever the frame rate: each
/// frame asks how far along it is.
#[derive(Clone, Copy, Debug)]
pub struct Anim {
    start: u64,
    ms: u64,
}

impl Anim {
    pub fn new(ms: u64) -> Anim {
        Anim { start: crate::time::uptime_ms(), ms: ms.max(1) }
    }

    /// Progress, 0..=1000.
    pub fn t(&self) -> u32 {
        let el = crate::time::uptime_ms().saturating_sub(self.start);
        (el.min(self.ms) * 1000 / self.ms) as u32
    }

    pub fn done(&self) -> bool {
        self.t() >= 1000
    }

    /// Eased progress (fast start, gentle stop), 0..=1000.
    pub fn out(&self) -> u32 {
        ease_out(self.t())
    }

    pub fn in_out(&self) -> u32 {
        ease_in_out(self.t())
    }

    /// Milliseconds since it started.
    pub fn elapsed(&self) -> u64 {
        crate::time::uptime_ms().saturating_sub(self.start)
    }
}

/// Cubic ease-out: 1 - (1 - t)^3, in 1/1000.
pub fn ease_out(t: u32) -> u32 {
    let u = 1000 - t.min(1000) as u64;
    (1000 - u * u * u / 1_000_000) as u32
}

/// Cubic ease-in-out, in 1/1000.
pub fn ease_in_out(t: u32) -> u32 {
    let t = t.min(1000) as u64;
    if t < 500 {
        (4 * t * t * t / 1_000_000) as u32
    } else {
        let u = 1000 - t;
        (1000 - 4 * u * u * u / 1_000_000) as u32
    }
}

/// `a` to `b` at `t` (0..=1000).
pub fn lerp(a: i32, b: i32, t: u32) -> i32 {
    a + ((b - a) as i64 * t as i64 / 1000) as i32
}

pub fn lerp_rect(a: Rect, b: Rect, t: u32) -> Rect {
    Rect::new(lerp(a.x, b.x, t), lerp(a.y, b.y, t), lerp(a.w, b.w, t), lerp(a.h, b.h, t))
}

/// `r` scaled by `permille` around its centre.
pub fn scale_rect(r: Rect, permille: i32) -> Rect {
    let w = (r.w as i64 * permille as i64 / 1000) as i32;
    let h = (r.h as i64 * permille as i64 / 1000) as i32;
    Rect::new(r.x + (r.w - w) / 2, r.y + (r.h - h) / 2, w, h)
}

/// A value that glides towards a target (hover highlights, the dock's
/// magnification): call [`step`](Self::step) every frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct Glide {
    pub value: i32,
    pub target: i32,
    last: u64,
}

impl Glide {
    pub fn new(v: i32) -> Glide {
        Glide { value: v, target: v, last: 0 }
    }

    /// Moves a share of the way to the target (about 90% in 150 ms);
    /// returns whether it is still moving.
    pub fn step(&mut self) -> bool {
        let now = crate::time::uptime_ms();
        let dt = if self.last == 0 { 16 } else { now.saturating_sub(self.last).min(100) } as i64;
        self.last = now;
        if self.value == self.target {
            self.last = 0;
            return false;
        }
        let diff = (self.target - self.value) as i64;
        let mut d = diff * (dt * 15).min(1000) / 1000;
        if d == 0 {
            d = diff.signum();
        }
        self.value += d as i32;
        if (self.target - self.value).abs() <= 2 {
            self.value = self.target;
        }
        true
    }

    pub fn moving(&self) -> bool {
        self.value != self.target
    }
}

// ----------------------------------------------------------------------------
// Clipboard

static CLIPBOARD: crate::sync::IrqLock<alloc::string::String> = crate::sync::IrqLock::new(alloc::string::String::new());

pub fn copy(text: &str) {
    *CLIPBOARD.lock() = alloc::string::String::from(text);
}

pub fn paste() -> alloc::string::String {
    CLIPBOARD.lock().clone()
}

/// Mouse, keyboard and screen updates for one interactive screen.
pub struct Looper {
    pub cursor: Cursor,
    dirty: Rect,
    last_click: (u64, i32, i32),
    last_tick: u64,
}

impl Looper {
    pub fn new(d: &Display) -> Looper {
        let (w, h) = (d.width(), d.height());
        input::set_capture(true);
        input::take_clicks();
        Looper { cursor: Cursor::new(w, h, d.scale()), dirty: Rect::EMPTY, last_click: (0, -100, -100), last_tick: 0 }
    }

    /// Marks part of the screen to be drawn again.
    pub fn damage(&mut self, r: Rect) {
        self.dirty = self.dirty.union(&r);
    }

    pub fn damage_all(&mut self, d: &Display) {
        self.damage(d.scene.bounds());
    }

    /// Waits for the next events and passes each to `handle`; redraws the
    /// damaged area with `draw` after each batch. Returns when `handle`
    /// returns true.
    pub fn run(&mut self, d: &mut Display, view: &mut dyn View) {
        loop {
            if super::take_redraw() {
                d.follow_framebuffer();
                self.damage_all(d);
            }
            if let Some(mode) = super::take_mode_request() {
                self.cursor.hide(d);
                match d.set_mode(mode) {
                    Ok(()) => {
                        self.cursor.set_scale(d.scale());
                        view.resized(d.width(), d.height());
                        self.damage_all(d);
                    }
                    Err(e) => crate::warn!("gui", "{}x{}: {e}", mode.width, mode.height),
                }
            }
            if super::take_scale_request() {
                self.cursor.hide(d);
                if d.apply_scale() {
                    self.cursor.set_scale(d.scale());
                    view.resized(d.width(), d.height());
                }
                self.damage_all(d);
            }
            let mut events: alloc::vec::Vec<Event> = alloc::vec::Vec::new();
            let (w, h) = (d.width(), d.height());
            let moved = self.cursor.poll(w, h);
            let (x, y) = (self.cursor.x, self.cursor.y);
            let b = self.cursor.buttons;
            if moved {
                events.push(Event::Move { x, y, held: b & 1 != 0 });
            }
            // Clicks in the order they happened, even if quicker than a frame.
            for (button, down, rx, ry) in input::take_clicks() {
                let (cx, cy) = self.cursor.click_at(rx, ry, w, h);
                match (button, down) {
                    (0, true) => {
                        let now = crate::time::uptime_ms();
                        let (t, px, py) = self.last_click;
                        let double = now - t < 450 && (px - cx).abs() < 6 && (py - cy).abs() < 6;
                        self.last_click = if double { (0, -100, -100) } else { (now, cx, cy) };
                        events.push(Event::Press { x: cx, y: cy, double });
                    }
                    (0, false) => events.push(Event::Release),
                    (1, true) => events.push(Event::Menu { x: cx, y: cy }),
                    _ => {}
                }
            }
            if self.cursor.scrolled != 0 {
                events.push(Event::Wheel { lines: self.cursor.scrolled });
            }
            while let Some(k) = input::gui_key() {
                events.push(Event::Key(k));
            }
            let now = crate::time::uptime_ms();
            if now - self.last_tick >= 100 {
                self.last_tick = now;
                events.push(Event::Tick);
            }
            let animating = view.animating();
            if animating {
                events.push(Event::Frame);
            }
            for ev in events {
                if view.event(self, ev) {
                    self.flush(d, view);
                    return;
                }
            }
            if moved {
                self.cursor.hide(d);
            }
            self.flush(d, view);
            if moved {
                self.cursor.show(d);
            }
            crate::task::sleep_ms(if animating { 2 } else { 8 });
        }
    }

    fn flush(&mut self, d: &mut Display, view: &mut dyn View) {
        let r = self.dirty.intersect(&d.scene.bounds());
        self.dirty = Rect::EMPTY;
        if r.is_empty() {
            return;
        }
        d.scene.set_clip(r);
        view.draw(&mut d.scene, r);
        d.scene.reset_clip();
        d.present(r);
        if r.overlaps(&self.cursor.rect()) {
            self.cursor.show(d);
        }
    }
}

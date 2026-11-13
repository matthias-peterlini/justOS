//! The look of the system's apps, so programs fit in: the colours of the
//! theme the user chose (dark or light, in Impostazioni) and a few controls
//! drawn as the system draws its own.
//!
//! The theme's colours are functions ([`text`], [`well`], [`line`]...): the
//! library asks the system which theme is in use when the first window opens
//! and again with each event ([`refresh_theme`]), so the next frame after the
//! user switched is drawn in the new colours. Colours that are the same in
//! both themes (the justOS orange) are constants.

use core::sync::atomic::{AtomicU8, Ordering};
use justdk::syscall as sys;
use justgfx::{Canvas, Font, Fonts, Rect};

/// Dark or light surfaces.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Theme {
    Dark,
    Light,
}

/// The colours of a theme (0xAARRGGBB), the same as the system's.
pub struct Palette {
    /// The gradient of a panel (a card over the content), top to bottom.
    pub panel_top: u32,
    pub panel_bottom: u32,
    /// The window's content, fields, buttons.
    pub well: u32,
    pub text: u32,
    pub text_dim: u32,
    pub text_faint: u32,
    /// The thin line round panels, wells and buttons; separators.
    pub line: u32,
    /// The lighter line along the top of a panel.
    pub sheen: u32,
    /// The selected item, and the one under the pointer.
    pub sel: u32,
    pub hover: u32,
    /// A faint pane on the content (rows, cards).
    pub pane: u32,
    /// An empty cell, a track (of tabs, of a bar).
    pub cell: u32,
    /// The gradient of a grey tile (the chosen tab), and a symbol on it.
    pub knob_top: u32,
    pub knob_bottom: u32,
    pub knob_glyph: u32,
    /// How strong shadows are, in percent of the dark theme's.
    pub shadow: u32,
}

pub const DARK: Palette = Palette {
    panel_top: 0xF723_2326,
    panel_bottom: 0xF719_191B,
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
    panel_top: 0xF7FA_FAFC,
    panel_bottom: 0xF7EC_ECF0,
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

/// The theme in use: 0 not asked yet, 1 dark, 2 light.
static THEME: AtomicU8 = AtomicU8::new(0);

/// Asks the system which theme is in use (an older system, which cannot
/// tell, is dark); returns whether it changed.
pub fn refresh_theme() -> bool {
    let light = justrt::sys::call(sys::THEME, 0, 0, 0, 0, 0) == sys::THEME_LIGHT as i64;
    let v = if light { 2 } else { 1 };
    THEME.swap(v, Ordering::Relaxed) != v
}

pub fn theme() -> Theme {
    if THEME.load(Ordering::Relaxed) == 0 {
        refresh_theme();
    }
    if THEME.load(Ordering::Relaxed) == 2 {
        Theme::Light
    } else {
        Theme::Dark
    }
}

/// The colours of the theme in use.
pub fn palette() -> &'static Palette {
    match theme() {
        Theme::Light => &LIGHT,
        Theme::Dark => &DARK,
    }
}

macro_rules! tokens {
    ($($name:ident),*) => {
        $(
            #[doc = concat!("The theme's `", stringify!($name), "` colour (see [`Palette`]).")]
            #[inline]
            pub fn $name() -> u32 {
                palette().$name
            }
        )*
    };
}

tokens!(panel_top, panel_bottom, well, text, text_dim, text_faint, line, sheen, sel, hover, pane, cell, knob_glyph);

/// The grey tile's gradient, top and bottom.
pub fn knob_grad() -> (u32, u32) {
    (palette().knob_top, palette().knob_bottom)
}

/// A shadow's `strength` (0..=255) in the theme in use.
pub fn shadow_strength(strength: u32) -> u32 {
    strength * palette().shadow / 100
}

// The same in both themes.

/// The justOS orange (#ff5a1f): the text cursor, focus, things chosen.
pub const ACCENT: u32 = 0xFFFF_5A1F;
pub const ORANGE: u32 = 0xFFFF_5A1F;
/// The orange gradient of primary buttons, top and bottom.
pub const ORANGE_GRAD: (u32, u32) = (0xFFFF_8A4C, 0xFFF0_4A10);
pub const RED: u32 = 0xFFFF_5A4E;
pub const GREEN: u32 = 0xFF34_C46A;
pub const YELLOW: u32 = 0xFFFF_D60A;
pub const WHITE: u32 = 0xFFFF_FFFF;
/// The light line along the top of a tile, whatever its colour.
pub const TILE_SHEEN: u32 = 0x47FF_FFFF;

/// Lighter, for the thing under the pointer: `amount` (0..=255) of the way
/// to white.
pub fn lighten(c: u32, amount: u32) -> u32 {
    (justgfx::gfx::mix(c, 0xFFFF_FFFF, amount) & 0xFF_FFFF) | (c & 0xFF00_0000)
}

// Surfaces.

/// A panel of the theme (a card over the content): the panel gradient, a
/// thin line all round, the sheen along the top and a soft shadow.
pub fn panel(c: &mut Canvas, r: Rect, rad: i32) {
    c.shadow(r, rad, 18, shadow_strength(80));
    c.fill_round_grad(r, rad, panel_top(), panel_bottom());
    c.stroke_round(r, rad, 1, line());
    c.sheen_round(r, rad, 1, sheen());
}

/// A well (a field, a pane): the well colour with a thin line all round.
pub fn well_pane(c: &mut Canvas, r: Rect, rad: i32) {
    c.fill_round(r, rad, well());
    c.stroke_round(r, rad, 1, line());
}

/// A tile: a rounded rectangle with a vertical gradient (`grad`: top,
/// bottom), the light line along its top and a small shadow.
pub fn tile(c: &mut Canvas, r: Rect, rad: i32, grad: (u32, u32)) {
    c.shadow(r, rad, 2, shadow_strength(70));
    c.fill_round_grad(r, rad, grad.0, grad.1);
    c.sheen_round(r, rad, 1, TILE_SHEEN);
}

// Controls.

fn button_rad(h: i32) -> i32 {
    14.min(h / 2)
}

fn label(c: &mut Canvas, f: &Fonts, r: Rect, s: &str, fg: u32) {
    let (lh, _) = f.metrics(Font::Strong);
    f.draw_centered(c, Font::Strong, r.x + r.w / 2, r.y + (r.h - lh) / 2, s, fg);
}

/// A button: a well with a thin line; `primary` is orange with white text.
pub fn button(c: &mut Canvas, f: &Fonts, r: Rect, s: &str, primary: bool, hover: bool) {
    let rad = button_rad(r.h);
    let fg = if primary {
        let amount = if hover { 30 } else { 0 };
        c.fill_round_grad(r, rad, lighten(ORANGE_GRAD.0, amount), lighten(ORANGE_GRAD.1, amount));
        c.sheen_round(r, rad, 1, TILE_SHEEN);
        WHITE
    } else {
        well_pane(c, r, rad);
        if hover {
            c.fill_round(r, rad, sel());
        }
        text()
    };
    label(c, f, r, s, fg);
}

/// A button in a colour of its own: `bg` at the bottom, lighter at the top,
/// with the light line along its top.
pub fn button_color(c: &mut Canvas, f: &Fonts, r: Rect, s: &str, bg: u32, fg: u32) {
    let rad = button_rad(r.h);
    c.fill_round_grad(r, rad, lighten(bg, 40), bg);
    c.sheen_round(r, rad, 1, (TILE_SHEEN & 0xFF_FFFF) | (((TILE_SHEEN >> 24) * (bg >> 24) / 255) << 24));
    label(c, f, r, s, fg);
}

/// Tabs on a grey track, the one at `sel` on a raised tile.
pub fn segmented(c: &mut Canvas, f: &Fonts, r: Rect, labels: &[&str], sel: usize) {
    let rad = 12.min(r.h / 2);
    c.fill_round(r, rad, cell());
    for (i, (s, l)) in segments(r, labels.len()).iter().zip(labels).enumerate() {
        if i == sel {
            tile(c, *s, (rad - 2).max(0), knob_grad());
        }
        label(c, f, *s, l, if i == sel { text() } else { text_dim() });
    }
}

/// The parts of a [`segmented`] control.
pub fn segments(r: Rect, n: usize) -> alloc::vec::Vec<Rect> {
    let n = n.max(1) as i32;
    let w = (r.w - 4) / n;
    (0..n).map(|i| Rect::new(r.x + 2 + i * w, r.y + 2, w, r.h - 4)).collect()
}

/// A one-line text field (`focused`: typing into it, with an orange edge
/// and the cursor).
pub fn text_field(c: &mut Canvas, f: &Fonts, r: Rect, value: &str, placeholder: &str, focused: bool) {
    let rad = button_rad(r.h);
    c.fill_round(r, rad, well());
    c.stroke_round(r, rad, 1, if focused { 0xB3FF_5A1F } else { line() });
    let (lh, _) = f.metrics(Font::Body);
    let ty = r.y + (r.h - lh) / 2;
    let w = if value.is_empty() {
        f.draw(c, Font::Body, r.x + 14, ty, placeholder, text_faint());
        0
    } else {
        f.draw(c, Font::Body, r.x + 14, ty, &f.fit(Font::Body, value, r.w - 30), text())
    };
    if focused {
        c.fill(Rect::new(r.x + 15 + w, ty + 2, 2, lh - 4), ACCENT);
    }
}

/// A card over the dimmed content, for messages ("Hai vinto!"): returns the
/// card's rectangle.
pub fn overlay(c: &mut Canvas, area: Rect, w: i32, h: i32) -> Rect {
    c.fill(area, shadow_strength(0x80) << 24);
    let r = Rect::new(area.x + (area.w - w) / 2, area.y + (area.h - h) / 2, w, h);
    panel(c, r, 18);
    r
}

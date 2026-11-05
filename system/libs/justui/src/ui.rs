//! The look of the system's apps: colours and a few controls, so programs
//! fit in.

use justgfx::{Canvas, Font, Fonts, Rect};

pub const TEXT: u32 = 0xFFF5_F2F8;
pub const TEXT_DIM: u32 = 0xB3F5_F2F8;
pub const TEXT_FAINT: u32 = 0x80F5_F2F8;
pub const ACCENT: u32 = 0xFF0A_84FF;
pub const ORANGE: u32 = 0xFFFF_9F0A;
pub const RED: u32 = 0xFFFF_453A;
pub const GREEN: u32 = 0xFF30_D158;
pub const YELLOW: u32 = 0xFFFF_D60A;
/// A panel inside the window.
pub const PANEL: u32 = 0x1FFF_FFFF;
pub const PANEL_HOVER: u32 = 0x2EFF_FFFF;
pub const SEPARATOR: u32 = 0x24FF_FFFF;

/// A pill-shaped button; `primary` is white with dark text.
pub fn button(c: &mut Canvas, f: &Fonts, r: Rect, label: &str, primary: bool, hover: bool) {
    let (bg, fg) = if primary {
        (if hover { 0xFFFF_FFFF } else { 0xF2FF_FFFF }, 0xFF1E_1626)
    } else {
        (if hover { 0x40FF_FFFF } else { 0x26FF_FFFF }, TEXT)
    };
    c.fill_round(r, r.h / 2, bg);
    let (lh, _) = f.metrics(Font::Strong);
    f.draw_centered(c, Font::Strong, r.x + r.w / 2, r.y + (r.h - lh) / 2, label, fg);
}

/// A button in a colour of its own.
pub fn button_color(c: &mut Canvas, f: &Fonts, r: Rect, label: &str, bg: u32, fg: u32) {
    c.fill_round(r, r.h / 2, bg);
    let (lh, _) = f.metrics(Font::Strong);
    f.draw_centered(c, Font::Strong, r.x + r.w / 2, r.y + (r.h - lh) / 2, label, fg);
}

/// Tabs in a pill.
pub fn segmented(c: &mut Canvas, f: &Fonts, r: Rect, labels: &[&str], sel: usize) {
    c.fill_round(r, r.h / 2, 0x26FF_FFFF);
    let (lh, _) = f.metrics(Font::Strong);
    for (i, s) in segments(r, labels.len()).iter().enumerate() {
        if i == sel {
            c.fill_round(*s, s.h / 2, 0x40FF_FFFF);
        }
        f.draw_centered(c, Font::Strong, s.x + s.w / 2, s.y + (s.h - lh) / 2, labels[i], if i == sel { TEXT } else { TEXT_DIM });
    }
}

pub fn segments(r: Rect, n: usize) -> alloc::vec::Vec<Rect> {
    let n = n.max(1) as i32;
    let w = (r.w - 4) / n;
    (0..n).map(|i| Rect::new(r.x + 2 + i * w, r.y + 2, w, r.h - 4)).collect()
}

/// A dark card with a dimmed backdrop, for messages over the content
/// ("Hai vinto!"): returns the card's rectangle.
pub fn overlay(c: &mut Canvas, area: Rect, w: i32, h: i32) -> Rect {
    c.fill(area, 0x8010_0A16);
    let r = Rect::new(area.x + (area.w - w) / 2, area.y + (area.h - h) / 2, w, h);
    c.shadow(r, 20, 16, 90);
    c.fill_round(r, 20, 0xF62A_2234);
    c.stroke_round(r, 20, 1, 0x2EFF_FFFF);
    r
}

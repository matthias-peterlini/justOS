//! The mouse pointer: drawn straight on the framebuffer over the back
//! buffer, so moving it costs a few hundred pixels and no redraw.
//!
//! Its position is in units, like everything the interface draws; on a
//! screen with two pixels per unit the arrow is drawn with twice the detail.

use super::gfx::{blend, Canvas, Rect};
use super::Display;
use crate::dev::input;

const W: i32 = 16;
const H: i32 = 24;

pub struct Cursor {
    pub x: i32,
    pub y: i32,
    pub buttons: u8,
    img: Canvas,
    drawn: Option<Rect>,
    raw: Option<(i32, i32)>,
    wheel: Option<i32>,
    /// Wheel movement since the last poll (positive: towards the user).
    pub scrolled: i32,
}

/// Is (`x`, `y`) inside the polygon? Even-odd rule.
fn inside(poly: &[(i32, i32)], x: i32, y: i32) -> bool {
    let mut odd = false;
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        let (xi, yi) = poly[i];
        let (xj, yj) = poly[j];
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            odd = !odd;
        }
        j = i;
    }
    odd
}

/// The arrow at `scale` pixels per unit, anti-aliased with 4x4 samples per
/// pixel: white with a dark outline.
fn arrow(scale: i32) -> Canvas {
    let shape: [(i32, i32); 7] = [(1, 1), (1, 18), (5, 14), (8, 21), (11, 20), (8, 13), (13, 13)];
    // Quarter pixels per unit.
    let q = 4 * scale;
    let p = |pts: &[(i32, i32)], grow: i32| -> alloc::vec::Vec<(i32, i32)> {
        // Grow the shape around its middle by `grow` quarter units.
        let (cx, cy) = (6 * q, 11 * q);
        pts.iter()
            .map(|&(x, y)| {
                let (x, y) = (x * q, y * q);
                (x + (x - cx).signum() * grow * scale, y + (y - cy).signum() * grow * scale)
            })
            .collect()
    };
    let outer = p(&shape, 5);
    let inner = p(&shape, 0);
    let mut c = Canvas::new_at(W, H, 0, scale);
    let stride = c.pixel_width();
    for y in 0..c.pixel_height() {
        for x in 0..stride {
            let (mut o, mut i) = (0u32, 0u32);
            for sy in 0..4 {
                for sx in 0..4 {
                    let (px, py) = (x * 4 + sx, y * 4 + sy);
                    o += inside(&outer, px, py) as u32;
                    i += inside(&inner, px, py) as u32;
                }
            }
            if o == 0 {
                continue;
            }
            // Dark edge, white inside.
            let a = o * 255 / 16;
            let white = i * 255 / 16;
            let v = 0x20 + (white * (0xFF - 0x20) / 255);
            c.px[(y * stride + x) as usize] = (a << 24) | (v << 16) | (v << 8) | v;
        }
    }
    c
}

impl Cursor {
    pub fn new(w: i32, h: i32, scale: i32) -> Cursor {
        Cursor { x: w / 2, y: h / 2, buttons: 0, img: arrow(scale), drawn: None, raw: None, wheel: None, scrolled: 0 }
    }

    /// The screen has another scale: the arrow is drawn for it and keeps
    /// its place on the screen.
    pub fn set_scale(&mut self, scale: i32) {
        let old = self.img.scale();
        if old == scale {
            return;
        }
        self.x = self.x * old / scale;
        self.y = self.y * old / scale;
        self.img = arrow(scale);
        self.drawn = None;
    }

    /// Reads the pointing device. Returns whether the position changed.
    pub fn poll(&mut self, w: i32, h: i32) -> bool {
        let Some(p) = input::pointer() else { return false };
        self.buttons = p.buttons;
        self.scrolled = self.wheel.map_or(0, |w| w - p.wheel);
        self.wheel = Some(p.wheel);
        let (old_x, old_y) = (self.x, self.y);
        if p.absolute {
            // Tablets report screen pixels.
            let k = self.img.scale();
            self.x = p.x / k;
            self.y = p.y / k;
        } else {
            if let Some((rx, ry)) = self.raw {
                // Drivers report motion in screen directions (down is
                // positive), whatever the device counts.
                self.x += p.x - rx;
                self.y += p.y - ry;
            }
            self.raw = Some((p.x, p.y));
        }
        self.x = self.x.clamp(0, w - 1);
        self.y = self.y.clamp(0, h - 1);
        (self.x, self.y) != (old_x, old_y)
    }

    /// Where a click reported at device position (`x`, `y`) happened on
    /// the screen.
    pub fn click_at(&self, x: i32, y: i32, w: i32, h: i32) -> (i32, i32) {
        match input::pointer() {
            Some(p) if p.absolute => {
                let k = self.img.scale();
                ((x / k).clamp(0, w - 1), (y / k).clamp(0, h - 1))
            }
            _ => (self.x, self.y),
        }
    }

    /// Puts the screen under the pointer back as the back buffer has it.
    pub fn hide(&mut self, d: &Display) {
        if let Some(r) = self.drawn.take() {
            d.present(r);
        }
    }

    pub fn show(&mut self, d: &Display) {
        let r = Rect::new(self.x, self.y, W, H).intersect(&d.scene.bounds());
        let k = d.scene.scale();
        if k != self.img.scale() {
            self.img = arrow(k);
        }
        let (ox, oy) = (self.x * k, self.y * k);
        for y in r.y * k..r.bottom() * k {
            for x in r.x * k..r.right() * k {
                let p = self.img.pixel(x - ox, y - oy);
                let a = p >> 24;
                if a != 0 {
                    d.put_screen(x, y, blend(d.scene.pixel(x, y), p, a));
                }
            }
        }
        crate::dev::display::flush(r.x * k, r.y * k, r.w * k, r.h * k);
        self.drawn = Some(r);
    }

    pub fn rect(&self) -> Rect {
        Rect::new(self.x, self.y, W, H)
    }
}

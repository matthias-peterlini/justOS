//! The boot splash: the arc wordmark and a progress bar on the dark ticly
//! background with its rust glow. No text.

use super::gfx::{Canvas, Rect};

pub const BG_TOP: u32 = 0xFF15_1012;
pub const BG_BOTTOM: u32 = 0xFF0B_0809;

static LOGO: &[u8] = include_bytes!("../../assets/logo.bin");

/// Width of the arc wordmark drawn `h` units tall.
pub fn logo_width(h: i32) -> i32 {
    let (lw, lh) = (u16::from_le_bytes([LOGO[0], LOGO[1]]) as i32, u16::from_le_bytes([LOGO[2], LOGO[3]]) as i32);
    lw * h / lh
}

/// The arc wordmark ("ARC", from `tools/assets/gen_logo.py`), `h` units
/// tall and centred on (`cx`, `cy`). Scaled with a box filter straight to
/// the canvas's pixels, so it stays sharp at 2x, without allocating (the
/// fatal error screen uses it too).
pub fn draw_logo(c: &mut Canvas, cx: i32, cy: i32, h: i32, color: u32) {
    let (lw, lh) = (u16::from_le_bytes([LOGO[0], LOGO[1]]) as i32, u16::from_le_bytes([LOGO[2], LOGO[3]]) as i32);
    let mask = &LOGO[4..];
    let h = h.clamp(4, lh);
    let w = logo_width(h);
    let s = c.scale();
    let (x0, y0) = ((cx - w / 2) * s, (cy - h / 2) * s);
    let (w, h) = (w * s, h * s);
    let a = color >> 24;
    let clip = c.pixel_clip();
    for ty in 0..h {
        let (sy0, sy1) = (ty * lh / h, ((ty + 1) * lh / h).max(ty * lh / h + 1));
        for tx in 0..w {
            let (x, y) = (x0 + tx, y0 + ty);
            if !clip.contains(x, y) {
                continue;
            }
            let (sx0, sx1) = (tx * lw / w, ((tx + 1) * lw / w).max(tx * lw / w + 1));
            let mut sum = 0u32;
            for sy in sy0..sy1 {
                let row = &mask[(sy * lw) as usize..];
                for sx in sx0..sx1 {
                    sum += row[sx as usize] as u32;
                }
            }
            let cov = sum / ((sy1 - sy0) * (sx1 - sx0)) as u32;
            if cov > 0 {
                c.blend_pixel(x, y, ((a * cov / 255) << 24) | (color & 0xFF_FFFF));
            }
        }
    }
}

/// The splash with the bar at `percent`; the first call draws everything,
/// later ones only the bar.
pub fn show(percent: u32) {
    super::with_display(|d| {
        let (w, h) = (d.width(), d.height());
        let cx = w / 2;
        let logo_y = h / 2 - 50;
        let bar = Rect::new(cx - 110, h / 2 + 70, 220, 6);
        let first = d.scene.get(0, 0) & 0xFF_FFFF == 0;
        let c = &mut d.scene;
        c.reset_clip();
        if first || percent == 0 {
            c.fill_round_grad(c.bounds(), 0, BG_TOP, BG_BOTTOM);
            c.glow(cx, logo_y - 40, w.min(h) * 3 / 5, 0xFF78_2E12, 150);
            draw_logo(c, cx, logo_y, 64, 0xFFFF_FFFF);
        }
        // The bar: a faint track and a white fill.
        let behind = Rect::new(bar.x - 2, bar.y - 2, bar.w + 4, bar.h + 4);
        if !(first || percent == 0) {
            c.fill_round(bar, 3, 0xFF1A_1416);
        }
        c.fill_round(bar, 3, 0x30FF_FFFF);
        let filled = bar.w * percent.min(100) as i32 / 100;
        if filled > 0 {
            c.fill_round(Rect::new(bar.x, bar.y, filled.max(6), bar.h), 3, 0xFFFF_FFFF);
        }
        if first || percent == 0 {
            d.present(d.scene.bounds());
        } else {
            d.present(behind);
        }
    });
}

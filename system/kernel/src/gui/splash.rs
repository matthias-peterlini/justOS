//! The boot splash: the JustOS wordmark and a progress bar on a dark
//! background with a warm glow. No text.
//!
//! The wordmark is the one on justos.dev: "JUSTOS" in a pixel font seven
//! pixels tall, then an orange cursor block. Each of its pixels is drawn as
//! a square of whole screen pixels, so it stays crisp at any scale.

use super::gfx::{Canvas, Rect};

pub const BG_TOP: u32 = 0xFF17_1615;
pub const BG_BOTTOM: u32 = 0xFF0D_0C0C;
/// The cursor block's orange (#ff5a1f).
pub const CURSOR: u32 = 0xFFFF_5A1F;

/// "JUSTOS", one string per row: `#` is a pixel. The cursor follows after a
/// column of space.
const WORD: [&[u8; 36]; 7] = [
    b"#####.#...#..####.#####..###...####.",
    b"...#..#...#.#.......#...#...#.#.....",
    b"...#..#...#.#.......#...#...#.#.....",
    b"...#..#...#..###....#...#...#..###..",
    b"...#..#...#.....#...#...#...#.....#.",
    b"#..#..#...#.....#...#...#...#.....#.",
    b".##....###..####....#....###..####..",
];
/// Rows and columns of the wordmark, cursor included.
const ROWS: i32 = 7;
const COLS: i32 = 40;
/// The mark ("J" and the cursor, for small places like the dock): the J's
/// five columns, a space, a narrower cursor.
const MARK_COLS: i32 = 9;

/// The size of one of the wordmark's pixels, in screen pixels, for a mark
/// `h` units tall.
fn cell(c: &Canvas, h: i32) -> i32 {
    (h * c.scale() / ROWS).max(1)
}

/// Draws a grid of `cols` x 7 pixels centred on (`cx`, `cy`), each `h / 7`
/// units square; `at(col, row)` gives a pixel's colour, if any.
fn draw_grid(c: &mut Canvas, cx: i32, cy: i32, h: i32, cols: i32, at: impl Fn(i32, i32) -> Option<u32>) {
    let s = c.scale();
    let p = cell(c, h);
    let (x0, y0) = (cx * s - cols * p / 2, cy * s - ROWS * p / 2);
    for row in 0..ROWS {
        // Runs of pixels of the same colour become one rectangle.
        let mut col = 0;
        while col < cols {
            let Some(color) = at(col, row) else {
                col += 1;
                continue;
            };
            let start = col;
            while col < cols && at(col, row) == Some(color) {
                col += 1;
            }
            c.fill_pixels(Rect::new(x0 + start * p, y0 + row * p, (col - start) * p, p), color);
        }
    }
}

/// The alpha of `color` applied to the cursor's orange.
fn cursor_in(color: u32) -> u32 {
    let a = (color >> 24) * (CURSOR >> 24) / 255;
    (a << 24) | (CURSOR & 0xFF_FFFF)
}

/// The JustOS wordmark, `h` units tall and centred on (`cx`, `cy`): the
/// letters in `color`, the cursor in orange (as transparent as `color`).
pub fn draw_logo(c: &mut Canvas, cx: i32, cy: i32, h: i32, color: u32) {
    let cursor = cursor_in(color);
    draw_grid(c, cx, cy, h, COLS, |col, row| {
        if col >= 36 {
            Some(cursor)
        } else {
            (WORD[row as usize][col as usize] == b'#').then_some(color)
        }
    });
}

/// The small mark: the "J" in `color` and the orange cursor.
pub fn draw_mark(c: &mut Canvas, cx: i32, cy: i32, h: i32, color: u32) {
    let cursor = cursor_in(color);
    draw_grid(c, cx, cy, h, MARK_COLS, |col, row| match col {
        0..=4 => (WORD[row as usize][col as usize] == b'#').then_some(color),
        6.. => Some(cursor),
        _ => None,
    });
}

/// The splash with the bar at `percent`; the first call draws everything,
/// later ones only the bar.
pub fn show(percent: u32) {
    super::with_display(|d| {
        let (w, h) = (d.width(), d.height());
        let cx = w / 2;
        let logo_y = h / 2 - 40;
        let bar = Rect::new(cx - 110, h / 2 + 50, 220, 6);
        let first = d.scene.get(0, 0) & 0xFF_FFFF == 0;
        let all = first || percent == 0;
        let behind = Rect::new(bar.x - 2, bar.y - 2, bar.w + 4, bar.h + 4);
        let c = &mut d.scene;
        // Everything the first time; later only the background behind the bar.
        c.reset_clip();
        if !all {
            c.set_clip(behind);
        }
        c.fill_round_grad(c.bounds(), 0, BG_TOP, BG_BOTTOM);
        c.glow(cx, h, w.min(h) * 4 / 5, 0xFF8A_3014, 120);
        if all {
            draw_logo(c, cx, logo_y, 42, 0xFFF2_EFE6);
        }
        c.reset_clip();
        // The bar: a faint track and a cream fill.
        c.fill_round(bar, 3, 0x30FF_FFFF);
        let filled = bar.w * percent.min(100) as i32 / 100;
        if filled > 0 {
            c.fill_round(Rect::new(bar.x, bar.y, filled.max(6), bar.h), 3, 0xFFF2_EFE6);
        }
        if all {
            d.present(d.scene.bounds());
        } else {
            d.present(behind);
        }
    });
}

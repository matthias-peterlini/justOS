//! Terminale: the system shell, in a window. It shows the console's cells
//! (the shell and the programs it runs write there) and passes the keys
//! typed into it on to them.

use crate::console;
use crate::dev::input::{self, KeyEvent};
use crate::gui::gfx::{Canvas, Rect};
use crate::gui::text;
use crate::gui::wm::{Action, App};
use alloc::string::String;
use alloc::vec::Vec;

const CW: i32 = 8;
const CH: i32 = 17;
const PAD: i32 = 14;

/// A cell the console never has: rows not drawn yet.
const UNSEEN: console::Cell = console::Cell { glyph: 0, fg: u8::MAX, bg: u8::MAX };

pub struct Terminal {
    cells: Vec<console::Cell>,
    seen: u64,
    blink: bool,
    ticks: u32,
    /// The cells as they are on the screen, columns and rows: only the rows
    /// that changed are drawn again.
    shown: Vec<console::Cell>,
    shown_size: (usize, usize),
    /// Where a caret is on the screen (column, row).
    shown_carets: Vec<(usize, usize)>,
    focused: bool,
}

impl Terminal {
    pub fn new() -> Terminal {
        Terminal {
            cells: Vec::new(),
            seen: u64::MAX,
            blink: true,
            ticks: 0,
            shown: Vec::new(),
            shown_size: (0, 0),
            shown_carets: Vec::new(),
            focused: false,
        }
    }

    /// Where the cells start.
    fn origin(area: Rect) -> (i32, i32) {
        let panel = area.grow(-6);
        (panel.x + PAD, panel.y + PAD)
    }
}

impl App for Terminal {
    fn title(&self) -> String {
        String::from("Terminale")
    }

    fn icon(&self) -> &str {
        "terminal"
    }

    fn kind(&self) -> &'static str {
        "terminal"
    }

    fn size(&self) -> (i32, i32) {
        (console::HEADLESS_COLS as i32 * CW + 2 * PAD + 12, console::HEADLESS_ROWS as i32 * CH + 2 * PAD + 12)
    }

    fn draw(&mut self, c: &mut Canvas, area: Rect, _blur: &Canvas, focused: bool) {
        let panel = area.grow(-6);
        c.fill_round(panel, 16, 0xF214_1019);
        let (cols, rows, ccol, crow, cursor) = console::snapshot(&mut self.cells);
        self.seen = console::generation();
        self.focused = focused;
        if self.shown_size != (cols, rows) {
            self.shown_size = (cols, rows);
            self.shown = alloc::vec![UNSEEN; cols * rows];
            self.shown_carets.clear();
        }
        let caret = cursor && focused && self.blink && ccol < cols && crow < rows;
        let (x0, y0) = Self::origin(area);
        let default_bg = console::DEFAULT_BG;
        let clip = c.clip();
        for row in 0..rows {
            let y = y0 + row as i32 * CH;
            if y > clip.bottom() || y + CH < clip.y {
                continue;
            }
            let line = Rect::new(x0, y, cols as i32 * CW, CH);
            if clip.intersect(&line) == line {
                // The whole row is drawn now.
                self.shown[row * cols..(row + 1) * cols].copy_from_slice(&self.cells[row * cols..(row + 1) * cols]);
                self.shown_carets.retain(|&(_, r)| r != row);
                if caret && crow == row {
                    self.shown_carets.push((ccol, row));
                }
            }
            for col in 0..cols {
                let cell = self.cells[row * cols + col];
                let x = x0 + col as i32 * CW;
                if cell.bg != default_bg {
                    c.fill(Rect::new(x, y, CW, CH), console::palette(cell.bg) | 0xFF00_0000);
                }
                if cell.glyph == b' ' {
                    continue;
                }
                let fg = console::palette(cell.fg) | 0xFF00_0000;
                let ch = console::cell_char(cell.glyph);
                if !text::draw_cell_glyph(c, x, y, ch, fg) {
                    // Box drawing and other symbols: the BIOS font.
                    let bits = console::bios_glyph(cell.glyph);
                    let mut mask = [0u8; 128];
                    for (r, b) in bits.iter().enumerate() {
                        for i in 0..8 {
                            mask[r * 8 + i] = if b & (0x80 >> i) != 0 { 255 } else { 0 };
                        }
                    }
                    c.draw_mask(&mask, 8, 16, x, y, fg);
                }
            }
        }
        if caret {
            c.fill(Rect::new(x0 + ccol as i32 * CW, y0 + crow as i32 * CH + CH - 3, CW, 2), 0xFFC9_A7FF);
        }
    }

    fn key(&mut self, ev: KeyEvent) -> Action {
        input::inject(ev);
        self.blink = true;
        Action::None
    }

    /// Files dropped on the terminal: their paths are typed at the prompt
    /// (quoted when they have spaces), for the command being written.
    fn drop_files(&mut self, paths: &[String], _x: i32, _y: i32, _area: Rect) -> Option<Action> {
        let line = crate::gui::dnd::as_words(paths);
        for ch in line.chars().chain(core::iter::once(' ')) {
            input::inject(KeyEvent { key: input::Key::Char(ch), ctrl: false, alt: false, shift: false });
        }
        self.blink = true;
        Some(Action::Redraw)
    }

    fn tick(&mut self) -> bool {
        self.ticks += 1;
        let blink = self.ticks.is_multiple_of(6);
        if blink {
            self.blink = !self.blink;
        }
        blink || console::generation() != self.seen
    }

    /// The rows whose cells or caret differ from the screen.
    fn changed(&mut self, area: Rect) -> Option<Rect> {
        let (cols, rows, ccol, crow, cursor) = console::snapshot(&mut self.cells);
        if self.shown_size != (cols, rows) {
            return None;
        }
        let caret = (cursor && self.focused && self.blink && ccol < cols && crow < rows).then_some((ccol, crow));
        let (mut lo, mut hi) = (usize::MAX, 0);
        let mut mark = |r: usize| {
            lo = lo.min(r);
            hi = hi.max(r);
        };
        for r in 0..rows {
            if self.cells[r * cols..(r + 1) * cols] != self.shown[r * cols..(r + 1) * cols] {
                mark(r);
            }
        }
        for &(c, r) in &self.shown_carets {
            if Some((c, r)) != caret {
                mark(r);
            }
        }
        if let Some((c, r)) = caret {
            if !self.shown_carets.contains(&(c, r)) {
                mark(r);
            }
        }
        if lo > hi {
            return Some(Rect::EMPTY);
        }
        let (x0, y0) = Self::origin(area);
        // A little more: a glyph may reach out of its cell.
        Some(Rect::new(x0, y0 + lo as i32 * CH, cols as i32 * CW, (hi - lo + 1) as i32 * CH).grow(2))
    }
}

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

pub struct Terminal {
    cells: Vec<console::Cell>,
    seen: u64,
    blink: bool,
    ticks: u32,
}

impl Terminal {
    pub fn new() -> Terminal {
        Terminal { cells: Vec::new(), seen: u64::MAX, blink: true, ticks: 0 }
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
        let (x0, y0) = (panel.x + PAD, panel.y + PAD);
        let default_bg = console::DEFAULT_BG;
        for row in 0..rows {
            let y = y0 + row as i32 * CH;
            if y > c.clip().bottom() || y + CH < c.clip().y {
                continue;
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
        if cursor && focused && self.blink && ccol < cols && crow < rows {
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
}

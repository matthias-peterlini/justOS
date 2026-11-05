//! Sudoku: fill the grid so that every row, column and box has the digits
//! 1 to 9 once. Every puzzle is new and has exactly one solution.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::vec::Vec;
use justui::{ui, Event, Font, Key, Rect, Rng, Window};

justrt::main!(main);

const CELL: i32 = 52;
const GRID: i32 = CELL * 9;
const X0: i32 = 20;
const Y0: i32 = 70;
const W: i32 = GRID + 40 + 190;
const H: i32 = GRID + 90;
const LEVELS: [(&str, usize); 3] = [("Facile", 38), ("Medio", 31), ("Difficile", 26)];

type Board = [u8; 81];

fn can_place(b: &Board, i: usize, d: u8) -> bool {
    let (r, c) = (i / 9, i % 9);
    for k in 0..9 {
        if b[r * 9 + k] == d || b[k * 9 + c] == d {
            return false;
        }
    }
    let (br, bc) = (r / 3 * 3, c / 3 * 3);
    for rr in br..br + 3 {
        for cc in bc..bc + 3 {
            if b[rr * 9 + cc] == d {
                return false;
            }
        }
    }
    true
}

/// Counts solutions, stopping at `limit`.
fn count(b: &mut Board, limit: u32) -> u32 {
    // The empty cell with the fewest choices first.
    let mut best: Option<(usize, u32)> = None;
    for i in 0..81 {
        if b[i] == 0 {
            let n = (1..=9).filter(|&d| can_place(b, i, d)).count() as u32;
            if n == 0 {
                return 0;
            }
            if best.is_none_or(|(_, m)| n < m) {
                best = Some((i, n));
            }
        }
    }
    let Some((i, _)) = best else { return 1 };
    let mut total = 0;
    for d in 1..=9 {
        if can_place(b, i, d) {
            b[i] = d;
            total += count(b, limit - total);
            b[i] = 0;
            if total >= limit {
                break;
            }
        }
    }
    total
}

fn fill(b: &mut Board, rng: &mut Rng) -> bool {
    let Some(i) = (0..81).find(|&i| b[i] == 0) else { return true };
    let mut digits: Vec<u8> = (1..=9).collect();
    for k in (1..9).rev() {
        let j = rng.below(k as u64 + 1) as usize;
        digits.swap(k, j);
    }
    for d in digits {
        if can_place(b, i, d) {
            b[i] = d;
            if fill(b, rng) {
                return true;
            }
            b[i] = 0;
        }
    }
    false
}

/// A puzzle with `clues` digits shown and a single solution.
fn generate(clues: usize, rng: &mut Rng) -> (Board, Board) {
    let mut solution = [0u8; 81];
    fill(&mut solution, rng);
    let mut puzzle = solution;
    let mut order: Vec<usize> = (0..81).collect();
    for k in (1..81).rev() {
        let j = rng.below(k as u64 + 1) as usize;
        order.swap(k, j);
    }
    let mut shown = 81;
    for i in order {
        if shown <= clues {
            break;
        }
        let keep = puzzle[i];
        puzzle[i] = 0;
        let mut test = puzzle;
        if count(&mut test, 2) != 1 {
            puzzle[i] = keep;
        } else {
            shown -= 1;
        }
    }
    (puzzle, solution)
}

struct Game {
    level: usize,
    given: Board,
    board: Board,
    solution: Board,
    /// Pencil marks: bit d for digit d.
    notes: [u16; 81],
    selected: Option<usize>,
    pencil: bool,
    mistakes: u32,
    started: u64,
    done: Option<u64>,
    rng: Rng,
}

impl Game {
    fn new(level: usize, mut rng: Rng) -> Game {
        let (puzzle, solution) = generate(LEVELS[level].1, &mut rng);
        Game {
            level,
            given: puzzle,
            board: puzzle,
            solution,
            notes: [0; 81],
            selected: None,
            pencil: false,
            mistakes: 0,
            started: justui::now_ms(),
            done: None,
            rng,
        }
    }

    fn put(&mut self, d: u8) {
        let Some(i) = self.selected else { return };
        if self.given[i] != 0 || self.done.is_some() {
            return;
        }
        if self.pencil && d != 0 {
            self.notes[i] ^= 1 << d;
            return;
        }
        self.board[i] = d;
        self.notes[i] = 0;
        if d != 0 && d != self.solution[i] {
            self.mistakes += 1;
        }
        if d != 0 {
            // A placed digit clears that pencil mark around it.
            let (r, c) = (i / 9, i % 9);
            for k in 0..81 {
                let (rr, cc) = (k / 9, k % 9);
                if rr == r || cc == c || (rr / 3 == r / 3 && cc / 3 == c / 3) {
                    self.notes[k] &= !(1 << d);
                }
            }
        }
        if self.board == self.solution {
            self.done = Some(justui::now_ms());
        }
    }

    fn conflict(&self, i: usize) -> bool {
        let d = self.board[i];
        if d == 0 {
            return false;
        }
        let mut b = self.board;
        b[i] = 0;
        !can_place(&b, i, d)
    }

    fn side(&self) -> Rect {
        Rect::new(X0 + GRID + 20, Y0, 170, GRID)
    }

    fn pad(&self, d: usize) -> Rect {
        let s = self.side();
        let k = d - 1;
        Rect::new(s.x + (k % 3) as i32 * 58, s.y + 120 + (k / 3) as i32 * 58, 52, 52)
    }

    fn buttons(&self) -> [Rect; 3] {
        let s = self.side();
        [Rect::new(s.x, s.y + 310, 170, 36), Rect::new(s.x, s.y + 354, 170, 36), Rect::new(s.x, s.y + 398, 170, 36)]
    }

    fn levels(&self) -> Rect {
        Rect::new(W - 20 - 300, 16, 300, 32)
    }

    fn draw(&self, win: &mut Window) {
        win.clear();
        win.text(Font::Heading, X0, 18, "Sudoku", ui::TEXT);
        let f = win.fonts;
        let labels: Vec<&str> = LEVELS.iter().map(|l| l.0).collect();
        ui::segmented(&mut win.canvas, f, self.levels(), &labels, self.level);
        let grid = Rect::new(X0, Y0, GRID, GRID);
        win.canvas.fill_round(grid.grow(4), 12, 0xF0F7_F7FA);
        let sel = self.selected;
        let sel_digit = sel.map(|i| self.board[i]).unwrap_or(0);
        for i in 0..81 {
            let (r, c) = ((i / 9) as i32, (i % 9) as i32);
            let cell = Rect::new(X0 + c * CELL, Y0 + r * CELL, CELL, CELL);
            let related = sel.is_some_and(|s| {
                let (sr, sc) = ((s / 9) as i32, (s % 9) as i32);
                sr == r || sc == c || (sr / 3 == r / 3 && sc / 3 == c / 3)
            });
            if sel == Some(i) {
                win.canvas.fill(cell, 0xFFBB_D8FF);
            } else if sel_digit != 0 && self.board[i] == sel_digit {
                win.canvas.fill(cell, 0xFFCF_E2FF);
            } else if related {
                win.canvas.fill(cell, 0xFFE6_EEF9);
            }
            let d = self.board[i];
            if d != 0 {
                let color = if self.given[i] != 0 {
                    0xFF1C_1C1E
                } else if self.conflict(i) || d != self.solution[i] {
                    0xFFE5_3935
                } else {
                    0xFF1E_5FD8
                };
                let (lh, _) = win.fonts.metrics(Font::Title);
                win.text_centered(Font::Title, cell.x + CELL / 2, cell.y + (CELL - lh) / 2, &format!("{d}"), color);
            } else if self.notes[i] != 0 {
                for n in 1..=9u8 {
                    if self.notes[i] & (1 << n) != 0 {
                        let k = (n - 1) as i32;
                        win.text_centered(Font::Small, cell.x + 9 + (k % 3) * 17, cell.y + 2 + (k / 3) * 16, &format!("{n}"), 0xFF6B_7280);
                    }
                }
            }
        }
        for k in 0..=9 {
            let w = if k % 3 == 0 { 3 } else { 1 };
            let col = if k % 3 == 0 { 0xFF2C_2C2E } else { 0xFFB8_BCC4 };
            win.canvas.fill(Rect::new(X0 + k * CELL - w / 2, Y0, w, GRID), col);
            win.canvas.fill(Rect::new(X0, Y0 + k * CELL - w / 2, GRID, w), col);
        }
        // The side: time, mistakes, digits, buttons.
        let s = self.side();
        let secs = (self.done.unwrap_or(justui::now_ms()) - self.started) / 1000;
        win.canvas.fill_round(Rect::new(s.x, s.y, s.w, 100), 14, ui::PANEL);
        win.text(Font::Small, s.x + 14, s.y + 12, "TEMPO", ui::TEXT_FAINT);
        win.text(Font::Heading, s.x + 14, s.y + 28, &format!("{}:{:02}", secs / 60, secs % 60), ui::TEXT);
        win.text(Font::Small, s.x + 14, s.y + 66, &format!("Errori: {}", self.mistakes), ui::TEXT_DIM);
        for d in 1..=9 {
            let r = self.pad(d);
            let left = 9 - self.board.iter().filter(|&&x| x as usize == d).count();
            win.canvas.fill_round(r, 12, if left == 0 { 0x14FF_FFFF } else { 0x33FF_FFFF });
            let (lh, _) = win.fonts.metrics(Font::Title);
            win.text_centered(Font::Title, r.x + r.w / 2, r.y + (r.h - lh) / 2, &format!("{d}"), if left == 0 { ui::TEXT_FAINT } else { ui::TEXT });
        }
        let [erase, pencil, new] = self.buttons();
        ui::button(&mut win.canvas, f, erase, "Cancella", false, false);
        ui::button_color(&mut win.canvas, f, pencil, if self.pencil { "Matita: sì" } else { "Matita: no" }, if self.pencil { ui::ACCENT } else { 0x26FF_FFFF }, ui::TEXT);
        ui::button(&mut win.canvas, f, new, "Nuova partita", true, false);
        if let Some(t) = self.done {
            let area = Rect::new(0, 0, W, H);
            let card = ui::overlay(&mut win.canvas, area, 300, 140);
            win.text_centered(Font::Title, card.x + 150, card.y + 20, "Risolto!", ui::GREEN);
            let secs = (t - self.started) / 1000;
            win.text_centered(Font::Body, card.x + 150, card.y + 66, &format!("In {}:{:02} con {} errori", secs / 60, secs % 60, self.mistakes), ui::TEXT_DIM);
            win.text_centered(Font::Small, card.x + 150, card.y + 102, "Clic per una nuova partita", ui::TEXT_FAINT);
        }
    }
}

fn main(_args: &str) -> i32 {
    let Ok(mut win) = Window::new("Sudoku", W, H) else { return 1 };
    let mut g = Game::new(0, Rng::new());
    loop {
        g.draw(&mut win);
        win.present();
        let ev = if g.done.is_none() { win.event(1000 - (justui::now_ms() - g.started) % 1000) } else { Some(win.wait()) };
        let Some(ev) = ev else { continue };
        match ev {
            Event::Close => return 0,
            Event::Press { x, y, .. } => {
                if g.done.is_some() {
                    let rng = core::mem::take(&mut g.rng);
                    g = Game::new(g.level, rng);
                    continue;
                }
                let lr = g.levels();
                if lr.contains(x, y) {
                    if let Some(l) = ui::segments(lr, LEVELS.len()).iter().position(|s| s.contains(x, y)) {
                        let rng = core::mem::take(&mut g.rng);
                        g = Game::new(l, rng);
                    }
                    continue;
                }
                if Rect::new(X0, Y0, GRID, GRID).contains(x, y) {
                    let (c, r) = ((x - X0) / CELL, (y - Y0) / CELL);
                    g.selected = Some((r * 9 + c) as usize);
                    continue;
                }
                if let Some(d) = (1..=9).find(|&d| g.pad(d).contains(x, y)) {
                    g.put(d as u8);
                    continue;
                }
                let [erase, pencil, new] = g.buttons();
                if erase.contains(x, y) {
                    g.put(0);
                } else if pencil.contains(x, y) {
                    g.pencil = !g.pencil;
                } else if new.contains(x, y) {
                    let rng = core::mem::take(&mut g.rng);
                    g = Game::new(g.level, rng);
                }
            }
            Event::Key(k) => {
                let sel = g.selected.unwrap_or(40) as i32;
                match k.key {
                    Key::Char(d @ '1'..='9') => g.put(d as u8 - b'0'),
                    Key::Char('0') | Key::Backspace | Key::Delete => g.put(0),
                    Key::Char('p') => g.pencil = !g.pencil,
                    Key::Up => g.selected = Some(((sel + 72) % 81) as usize),
                    Key::Down => g.selected = Some(((sel + 9) % 81) as usize),
                    Key::Left => g.selected = Some((sel / 9 * 9 + (sel % 9 + 8) % 9) as usize),
                    Key::Right => g.selected = Some((sel / 9 * 9 + (sel % 9 + 1) % 9) as usize),
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

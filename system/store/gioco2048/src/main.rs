//! 2048: slide the tiles with the arrows; two equal tiles that meet become
//! one. Reach 2048. The best score is kept in the app's own folder.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::vec::Vec;
use arcui::{ui, Event, Font, Key, Rect, Rng, Window};

arcrt::main!(main);

const N: usize = 4;
const CELL: i32 = 96;
const GAP: i32 = 12;
const BOARD: i32 = N as i32 * CELL + (N as i32 + 1) * GAP;
const W: i32 = BOARD + 48;
const H: i32 = BOARD + 150;
const SLIDE_MS: u64 = 110;

/// Where the best score is kept.
fn best_file() -> alloc::string::String {
    arcrt::app::data_file("best")
}

/// A tile moving from one cell to another (for the animation).
#[derive(Clone, Copy)]
struct Move {
    value: u32,
    from: (usize, usize),
    to: (usize, usize),
}

struct Game {
    grid: [[u32; N]; N],
    score: u32,
    best: u32,
    rng: Rng,
    moves: Vec<Move>,
    /// When the last slide started, and the cells that just merged or
    /// appeared (they pop).
    since: u64,
    popped: Vec<(usize, usize)>,
    won: bool,
    keep_going: bool,
    over: bool,
    hover_new: bool,
}

fn color(v: u32) -> (u32, u32) {
    match v {
        2 => (0xFFEE_E4DA, 0xFF77_6E65),
        4 => (0xFFED_E0C8, 0xFF77_6E65),
        8 => (0xFFF2_B179, 0xFFFF_FFFF),
        16 => (0xFFF5_9563, 0xFFFF_FFFF),
        32 => (0xFFF6_7C5F, 0xFFFF_FFFF),
        64 => (0xFFF6_5E3B, 0xFFFF_FFFF),
        128 => (0xFFED_CF72, 0xFFFF_FFFF),
        256 => (0xFFED_CC61, 0xFFFF_FFFF),
        512 => (0xFFED_C850, 0xFFFF_FFFF),
        1024 => (0xFFED_C53F, 0xFFFF_FFFF),
        2048 => (0xFFED_C22E, 0xFFFF_FFFF),
        _ => (0xFF3C_3A32, 0xFFFF_FFFF),
    }
}

fn cell_rect(r: usize, c: usize) -> Rect {
    let (x0, y0) = ((W - BOARD) / 2, 120);
    Rect::new(x0 + GAP + c as i32 * (CELL + GAP), y0 + GAP + r as i32 * (CELL + GAP), CELL, CELL)
}

fn new_button() -> Rect {
    Rect::new(W - 24 - 188, 68, 188, 36)
}

impl Game {
    fn new(best: u32) -> Game {
        let mut g = Game {
            grid: [[0; N]; N],
            score: 0,
            best,
            rng: Rng::new(),
            moves: Vec::new(),
            since: 0,
            popped: Vec::new(),
            won: false,
            keep_going: false,
            over: false,
            hover_new: false,
        };
        g.spawn();
        g.spawn();
        g.since = arcui::now_ms();
        g
    }

    fn spawn(&mut self) {
        let free: Vec<(usize, usize)> = (0..N * N).map(|i| (i / N, i % N)).filter(|&(r, c)| self.grid[r][c] == 0).collect();
        if free.is_empty() {
            return;
        }
        let (r, c) = free[self.rng.below(free.len() as u64) as usize];
        self.grid[r][c] = if self.rng.below(10) == 0 { 4 } else { 2 };
        self.popped.push((r, c));
    }

    /// Slides towards (dr, dc); returns whether anything moved.
    fn slide(&mut self, dr: i32, dc: i32) -> bool {
        let mut moved = false;
        let mut moves = Vec::new();
        let mut merged = [[false; N]; N];
        let mut popped = Vec::new();
        // Walk from the side the tiles move towards.
        let order: Vec<usize> = if dr > 0 || dc > 0 { (0..N).rev().collect() } else { (0..N).collect() };
        for &a in &order {
            for b in 0..N {
                let (r, c) = if dr != 0 { (a, b) } else { (b, a) };
                let v = self.grid[r][c];
                if v == 0 {
                    continue;
                }
                let (mut nr, mut nc) = (r as i32, c as i32);
                loop {
                    let (tr, tc) = (nr + dr, nc + dc);
                    if !(0..N as i32).contains(&tr) || !(0..N as i32).contains(&tc) {
                        break;
                    }
                    let t = self.grid[tr as usize][tc as usize];
                    if t == 0 {
                        nr = tr;
                        nc = tc;
                    } else {
                        if t == v && !merged[tr as usize][tc as usize] {
                            nr = tr;
                            nc = tc;
                        }
                        break;
                    }
                }
                let (nr, nc) = (nr as usize, nc as usize);
                if (nr, nc) != (r, c) {
                    moved = true;
                    moves.push(Move { value: v, from: (r, c), to: (nr, nc) });
                    self.grid[r][c] = 0;
                    if self.grid[nr][nc] == v {
                        self.grid[nr][nc] = v * 2;
                        merged[nr][nc] = true;
                        self.score += v * 2;
                        popped.push((nr, nc));
                        if v * 2 == 2048 && !self.keep_going {
                            self.won = true;
                        }
                    } else {
                        self.grid[nr][nc] = v;
                    }
                } else {
                    moves.push(Move { value: v, from: (r, c), to: (r, c) });
                }
            }
        }
        if moved {
            self.moves = moves;
            self.popped = popped;
            self.spawn();
            self.since = arcui::now_ms();
            if self.score > self.best {
                self.best = self.score;
                let _ = arcrt::fs::write(&best_file(), format!("{}", self.best).as_bytes());
            }
            self.over = !self.can_move();
        }
        moved
    }

    fn can_move(&self) -> bool {
        for r in 0..N {
            for c in 0..N {
                let v = self.grid[r][c];
                if v == 0 || (c + 1 < N && self.grid[r][c + 1] == v) || (r + 1 < N && self.grid[r + 1][c] == v) {
                    return true;
                }
            }
        }
        false
    }

    fn animating(&self) -> bool {
        arcui::now_ms() - self.since < SLIDE_MS * 2 + 40
    }

    fn draw(&self, win: &mut Window) {
        win.clear();
        win.text(Font::Display, 24, 18, "2048", 0xFFED_C22E);
        for (k, (label, v)) in [("PUNTI", self.score), ("RECORD", self.best)].iter().enumerate() {
            let r = Rect::new(W - 24 - 90 - (1 - k as i32) * 98, 14, 90, 44);
            win.canvas.fill_round(r, 10, ui::PANEL);
            win.text_centered(Font::Small, r.x + r.w / 2, r.y + 3, label, ui::TEXT_DIM);
            win.text_centered(Font::Strong, r.x + r.w / 2, r.y + 20, &format!("{v}"), ui::TEXT);
        }
        win.text(Font::Body, 26, 74, "Unisci le tessere fino a 2048", ui::TEXT_DIM);
        let f = win.fonts;
        ui::button(&mut win.canvas, f, new_button(), "Nuova partita", false, self.hover_new);
        let board = Rect::new((W - BOARD) / 2, 120, BOARD, BOARD);
        win.canvas.fill_round(board, 14, 0x40_00_00_00);
        for r in 0..N {
            for c in 0..N {
                win.canvas.fill_round(cell_rect(r, c), 8, 0x26FF_FFFF);
            }
        }
        let el = arcui::now_ms() - self.since;
        let sliding = el < SLIDE_MS && !self.moves.is_empty();
        if sliding {
            // Tiles on their way: drawn between the two cells.
            let t = (el * 1000 / SLIDE_MS) as i32;
            for m in &self.moves {
                let (a, b) = (cell_rect(m.from.0, m.from.1), cell_rect(m.to.0, m.to.1));
                let r = Rect::new(a.x + (b.x - a.x) * t / 1000, a.y + (b.y - a.y) * t / 1000, CELL, CELL);
                self.tile(win, r, m.value);
            }
            return;
        }
        for r in 0..N {
            for c in 0..N {
                let v = self.grid[r][c];
                if v == 0 {
                    continue;
                }
                let mut rect = cell_rect(r, c);
                if self.popped.contains(&(r, c)) {
                    // A short pop: bigger, then back.
                    let p = el.saturating_sub(SLIDE_MS).min(SLIDE_MS) as i32 * 1000 / SLIDE_MS as i32;
                    let s = if p < 500 { 1000 + p * 24 / 100 } else { 1120 - (p - 500) * 24 / 100 };
                    let w = CELL * s / 1000;
                    rect = Rect::new(rect.x + (CELL - w) / 2, rect.y + (CELL - w) / 2, w, w);
                }
                self.tile(win, rect, v);
            }
        }
        if self.won && !self.keep_going {
            let area = Rect::new(0, 0, W, H);
            let card = ui::overlay(&mut win.canvas, area, 300, 150);
            win.text_centered(Font::Title, card.x + 150, card.y + 22, "Hai vinto!", 0xFFED_C22E);
            win.text_centered(Font::Body, card.x + 150, card.y + 68, "Continua per un punteggio più alto", ui::TEXT_DIM);
            win.text_centered(Font::Small, card.x + 150, card.y + 104, "Premi un tasto freccia", ui::TEXT_FAINT);
        } else if self.over {
            let area = Rect::new(0, 0, W, H);
            let card = ui::overlay(&mut win.canvas, area, 300, 150);
            win.text_centered(Font::Title, card.x + 150, card.y + 22, "Fine partita", ui::TEXT);
            win.text_centered(Font::Body, card.x + 150, card.y + 68, &format!("{} punti", self.score), ui::TEXT_DIM);
            win.text_centered(Font::Small, card.x + 150, card.y + 104, "Invio per ricominciare", ui::TEXT_FAINT);
        }
    }

    fn tile(&self, win: &mut Window, r: Rect, v: u32) {
        let (bg, fg) = color(v);
        win.canvas.fill_round(r, 8, bg);
        let label = format!("{v}");
        let font = if v < 100 {
            Font::Display
        } else if v < 1000 {
            Font::Clock
        } else {
            Font::Title
        };
        let (lh, _) = win.fonts.metrics(font);
        win.text_centered(font, r.x + r.w / 2, r.y + (r.h - lh) / 2, &label, fg);
    }
}

fn main(_args: &str) -> i32 {
    let best = arcrt::fs::read_to_string(&best_file()).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
    let Ok(mut win) = Window::new("2048", W, H) else { return 1 };
    let mut game = Game::new(best);
    loop {
        game.draw(&mut win);
        win.present();
        let ev = if game.animating() { win.event(16) } else { Some(win.wait()) };
        let Some(ev) = ev else { continue };
        match ev {
            Event::Close => return 0,
            Event::Key(k) => {
                if game.won && !game.keep_going {
                    game.keep_going = true;
                    game.won = false;
                }
                match k.key {
                    Key::Up | Key::Char('w') => {
                        game.slide(-1, 0);
                    }
                    Key::Down | Key::Char('s') => {
                        game.slide(1, 0);
                    }
                    Key::Left | Key::Char('a') => {
                        game.slide(0, -1);
                    }
                    Key::Right | Key::Char('d') => {
                        game.slide(0, 1);
                    }
                    Key::Enter if game.over => game = Game::new(game.best),
                    Key::Char('n') => game = Game::new(game.best),
                    _ => {}
                }
            }
            Event::Press { x, y, .. } => {
                if new_button().contains(x, y) || game.over {
                    game = Game::new(game.best);
                }
            }
            Event::Move { x, y, .. } => game.hover_new = new_button().contains(x, y),
            Event::Leave => game.hover_new = false,
            _ => {}
        }
    }
}

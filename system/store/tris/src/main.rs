//! Tris: noughts and crosses against the computer, which never loses
//! (minimax) in "Difficile" and sometimes slips in "Facile".

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use justui::{ui, Event, Font, Key, Rect, Rng, Window};

justrt::main!(main);

const W: i32 = 420;
const H: i32 = 560;
const CELL: i32 = 112;
const GRID_X: i32 = (W - 3 * CELL) / 2;
const GRID_Y: i32 = 150;
const LINES: [[usize; 3]; 8] = [[0, 1, 2], [3, 4, 5], [6, 7, 8], [0, 3, 6], [1, 4, 7], [2, 5, 8], [0, 4, 8], [2, 4, 6]];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Cell {
    Empty,
    X,
    O,
}

struct Game {
    board: [Cell; 9],
    /// The player (X) moves first in even games, the computer in odd ones.
    turn_player: bool,
    games: u32,
    wins: (u32, u32, u32),
    hard: bool,
    over: Option<(Cell, Option<[usize; 3]>)>,
    rng: Rng,
    hover: Option<usize>,
    /// When the computer may move (a short pause looks natural).
    think_until: u64,
    /// When each cell was filled (marks grow in).
    placed: [u64; 9],
}

fn winner(b: &[Cell; 9]) -> Option<(Cell, [usize; 3])> {
    LINES.iter().find(|l| b[l[0]] != Cell::Empty && b[l[0]] == b[l[1]] && b[l[1]] == b[l[2]]).map(|l| (b[l[0]], *l))
}

/// Score of the board for O to move (`me`): +10 O wins, -10 X wins.
fn minimax(b: &mut [Cell; 9], me: bool, depth: i32) -> i32 {
    if let Some((w, _)) = winner(b) {
        return if w == Cell::O { 10 - depth } else { depth - 10 };
    }
    if b.iter().all(|&c| c != Cell::Empty) {
        return 0;
    }
    let mut best = if me { i32::MIN } else { i32::MAX };
    for i in 0..9 {
        if b[i] == Cell::Empty {
            b[i] = if me { Cell::O } else { Cell::X };
            let s = minimax(b, !me, depth + 1);
            b[i] = Cell::Empty;
            best = if me { best.max(s) } else { best.min(s) };
        }
    }
    best
}

fn cell_rect(i: usize) -> Rect {
    Rect::new(GRID_X + (i % 3) as i32 * CELL, GRID_Y + (i / 3) as i32 * CELL, CELL, CELL)
}

fn level_rect() -> Rect {
    Rect::new(W / 2 - 110, 98, 220, 32)
}

fn again_rect() -> Rect {
    Rect::new(W / 2 - 80, GRID_Y + 3 * CELL + 30, 160, 38)
}

impl Game {
    fn new() -> Game {
        Game {
            board: [Cell::Empty; 9],
            turn_player: true,
            games: 0,
            wins: (0, 0, 0),
            hard: false,
            over: None,
            rng: Rng::new(),
            hover: None,
            think_until: 0,
            placed: [0; 9],
        }
    }

    fn restart(&mut self) {
        self.board = [Cell::Empty; 9];
        self.over = None;
        self.games += 1;
        self.turn_player = self.games % 2 == 0;
        self.think_until = justui::now_ms() + 400;
    }

    fn play(&mut self, i: usize, c: Cell) {
        self.board[i] = c;
        self.placed[i] = justui::now_ms();
        if let Some((w, line)) = winner(&self.board) {
            self.over = Some((w, Some(line)));
            if w == Cell::X {
                self.wins.0 += 1;
            } else {
                self.wins.2 += 1;
            }
        } else if self.board.iter().all(|&c| c != Cell::Empty) {
            self.over = Some((Cell::Empty, None));
            self.wins.1 += 1;
        }
        self.turn_player = c == Cell::O;
        self.think_until = justui::now_ms() + 350;
    }

    fn computer(&mut self) {
        let free: alloc::vec::Vec<usize> = (0..9).filter(|&i| self.board[i] == Cell::Empty).collect();
        if free.is_empty() {
            return;
        }
        // In the easy level, one move in three is random.
        let i = if !self.hard && self.rng.below(3) == 0 {
            free[self.rng.below(free.len() as u64) as usize]
        } else {
            let mut b = self.board;
            let mut best = (i32::MIN, free[0]);
            for &i in &free {
                b[i] = Cell::O;
                let s = minimax(&mut b, false, 0) * 16 + self.rng.below(8) as i32;
                b[i] = Cell::Empty;
                if s > best.0 {
                    best = (s, i);
                }
            }
            best.1
        };
        self.play(i, Cell::O);
    }

    fn draw(&self, win: &mut Window) {
        win.clear();
        win.text_centered(Font::Title, W / 2, 16, "Tris", ui::text());
        let score = format!("Tu {}  ·  Pari {}  ·  justOS {}", self.wins.0, self.wins.1, self.wins.2);
        win.text_centered(Font::Body, W / 2, 62, &score, ui::text_dim());
        let f = win.fonts;
        ui::segmented(&mut win.canvas, f, level_rect(), &["Facile", "Difficile"], self.hard as usize);
        let now = justui::now_ms();
        for k in 1..3 {
            let x = GRID_X + k * CELL;
            win.canvas.fill_round(Rect::new(x - 2, GRID_Y + 8, 4, 3 * CELL - 16), 2, ui::text_faint());
            let y = GRID_Y + k * CELL;
            win.canvas.fill_round(Rect::new(GRID_X + 8, y - 2, 3 * CELL - 16, 4), 2, ui::text_faint());
        }
        for i in 0..9 {
            let r = cell_rect(i);
            let (cx, cy) = (r.x + r.w / 2, r.y + r.h / 2);
            let grow = ((now - self.placed[i]).min(160) * 1000 / 160) as i32;
            let s = 32 * grow / 1000;
            match self.board[i] {
                Cell::X => {
                    win.canvas.line(cx - s, cy - s, cx + s, cy + s, 7, 0xFF5A_C8FA);
                    win.canvas.line(cx - s, cy + s, cx + s, cy - s, 7, 0xFF5A_C8FA);
                }
                Cell::O => {
                    win.canvas.arc(cx, cy, s + 4, 8, 1000, ui::ORANGE);
                }
                Cell::Empty => {
                    if self.hover == Some(i) && self.turn_player && self.over.is_none() {
                        win.canvas.line(cx - 26, cy - 26, cx + 26, cy + 26, 5, 0x335A_C8FA);
                        win.canvas.line(cx - 26, cy + 26, cx + 26, cy - 26, 5, 0x335A_C8FA);
                    }
                }
            }
        }
        match self.over {
            Some((w, line)) => {
                if let Some(l) = line {
                    let (a, b) = (cell_rect(l[0]), cell_rect(l[2]));
                    win.canvas.line(a.x + a.w / 2, a.y + a.h / 2, b.x + b.w / 2, b.y + b.h / 2, 6, ui::text());
                }
                let msg = match w {
                    Cell::X => "Hai vinto!",
                    Cell::O => "Vince justOS",
                    Cell::Empty => "Pareggio",
                };
                win.text_centered(Font::Heading, W / 2, GRID_Y + 3 * CELL - 4, msg, ui::text());
                ui::button(&mut win.canvas, f, again_rect(), "Ancora", true, false);
            }
            None => {
                let msg = if self.turn_player { "Tocca a te (X)" } else { "justOS ci pensa…" };
                win.text_centered(Font::Body, W / 2, GRID_Y + 3 * CELL + 14, msg, ui::text_faint());
            }
        }
    }
}

fn main(_args: &str) -> i32 {
    let Ok(mut win) = Window::new("Tris", W, H) else { return 1 };
    let mut g = Game::new();
    loop {
        g.draw(&mut win);
        win.present();
        let now = justui::now_ms();
        let growing = (0..9).any(|i| g.board[i] != Cell::Empty && now - g.placed[i] < 200);
        let thinking = !g.turn_player && g.over.is_none();
        let ev = if thinking {
            if now >= g.think_until {
                g.computer();
                continue;
            }
            win.event(g.think_until - now)
        } else if growing {
            win.event(16)
        } else {
            Some(win.wait())
        };
        let Some(ev) = ev else { continue };
        match ev {
            Event::Close => return 0,
            Event::Press { x, y, .. } => {
                let lr = level_rect();
                if lr.contains(x, y) {
                    g.hard = x >= lr.x + lr.w / 2;
                    continue;
                }
                if g.over.is_some() {
                    if again_rect().contains(x, y) {
                        g.restart();
                    }
                    continue;
                }
                if g.turn_player {
                    if let Some(i) = (0..9).find(|&i| cell_rect(i).contains(x, y)) {
                        if g.board[i] == Cell::Empty {
                            g.play(i, Cell::X);
                        }
                    }
                }
            }
            Event::Move { x, y, .. } => g.hover = (0..9).find(|&i| cell_rect(i).contains(x, y)),
            Event::Key(k) if k.key == Key::Enter && g.over.is_some() => g.restart(),
            Event::Key(k) => {
                if let Key::Char(d @ '1'..='9') = k.key {
                    let i = d as usize - '1' as usize;
                    if g.turn_player && g.over.is_none() && g.board[i] == Cell::Empty {
                        g.play(i, Cell::X);
                    }
                }
            }
            _ => {}
        }
    }
}

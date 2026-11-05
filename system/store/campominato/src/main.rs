//! Campo minato: uncover every cell without a mine. A number says how many
//! mines touch the cell; the right button (or a long look and F) plants a
//! flag. Three levels.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::vec;
use alloc::vec::Vec;
use justui::{ui, Event, Font, Key, Rect, Rng, Window};

justrt::main!(main);

const LEVELS: [(&str, usize, usize, usize); 3] = [("Facile", 9, 9, 10), ("Medio", 16, 16, 40), ("Difficile", 24, 16, 76)];
const CELL: i32 = 30;
const TOP: i32 = 64;
const PAD: i32 = 16;

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Ready,
    Playing,
    Won,
    Lost,
}

struct Game {
    level: usize,
    w: usize,
    h: usize,
    mines: usize,
    mine: Vec<bool>,
    open: Vec<bool>,
    flag: Vec<bool>,
    state: State,
    started: u64,
    ended: u64,
    rng: Rng,
    hover: Option<usize>,
    /// The mine that went off.
    boom: Option<usize>,
}

fn size(level: usize) -> (i32, i32) {
    let (_, w, h, _) = LEVELS[level];
    ((w as i32 * CELL + 2 * PAD).max(MIN_W), h as i32 * CELL + TOP + PAD)
}

/// Wide enough for the counters and the levels.
const MIN_W: i32 = 480;

impl Game {
    fn new(level: usize) -> Game {
        let (_, w, h, mines) = LEVELS[level];
        Game {
            level,
            w,
            h,
            mines,
            mine: vec![false; w * h],
            open: vec![false; w * h],
            flag: vec![false; w * h],
            state: State::Ready,
            started: 0,
            ended: 0,
            rng: Rng::new(),
            hover: None,
            boom: None,
        }
    }

    fn neighbours(&self, i: usize) -> impl Iterator<Item = usize> + '_ {
        let (x, y) = ((i % self.w) as i32, (i / self.w) as i32);
        (-1..=1).flat_map(move |dy| (-1..=1).map(move |dx| (x + dx, y + dy))).filter_map(move |(nx, ny)| {
            if (nx, ny) != (x, y) && nx >= 0 && ny >= 0 && (nx as usize) < self.w && (ny as usize) < self.h {
                Some(ny as usize * self.w + nx as usize)
            } else {
                None
            }
        })
    }

    fn count(&self, i: usize) -> usize {
        self.neighbours(i).filter(|&n| self.mine[n]).count()
    }

    /// Mines go in after the first click, never on it or next to it.
    fn place(&mut self, first: usize) {
        let safe: Vec<usize> = core::iter::once(first).chain(self.neighbours(first)).collect();
        let mut left = self.mines;
        while left > 0 {
            let i = self.rng.below((self.w * self.h) as u64) as usize;
            if !self.mine[i] && !safe.contains(&i) {
                self.mine[i] = true;
                left -= 1;
            }
        }
    }

    fn reveal(&mut self, i: usize) {
        if self.state == State::Ready {
            self.place(i);
            self.state = State::Playing;
            self.started = justui::now_ms();
        }
        if self.state != State::Playing || self.flag[i] {
            return;
        }
        if self.open[i] {
            // Clicking an open number with its flags all placed opens the rest.
            let flags = self.neighbours(i).filter(|&n| self.flag[n]).count();
            if flags == self.count(i) && flags > 0 {
                let ns: Vec<usize> = self.neighbours(i).filter(|&n| !self.open[n] && !self.flag[n]).collect();
                for n in ns {
                    self.open_one(n);
                }
            }
        } else {
            self.open_one(i);
        }
        self.check_won();
    }

    fn open_one(&mut self, i: usize) {
        if self.state != State::Playing {
            return;
        }
        if self.mine[i] {
            self.open[i] = true;
            self.boom = Some(i);
            self.state = State::Lost;
            self.ended = justui::now_ms();
            return;
        }
        // Flood the empty area.
        let mut stack = vec![i];
        while let Some(j) = stack.pop() {
            if self.open[j] || self.flag[j] {
                continue;
            }
            self.open[j] = true;
            if self.count(j) == 0 {
                stack.extend(self.neighbours(j).filter(|&n| !self.open[n]));
            }
        }
    }

    fn check_won(&mut self) {
        if self.state == State::Playing && (0..self.w * self.h).all(|i| self.mine[i] || self.open[i]) {
            self.state = State::Won;
            self.ended = justui::now_ms();
            for i in 0..self.w * self.h {
                if self.mine[i] {
                    self.flag[i] = true;
                }
            }
        }
    }

    fn toggle_flag(&mut self, i: usize) {
        if matches!(self.state, State::Ready | State::Playing) && !self.open[i] {
            self.flag[i] = !self.flag[i];
        }
    }

    /// Where the grid starts (it is centred in the window).
    fn grid_x(&self) -> i32 {
        (size(self.level).0 - self.w as i32 * CELL) / 2
    }

    fn cell_at(&self, x: i32, y: i32) -> Option<usize> {
        let (cx, cy) = ((x - self.grid_x()).div_euclid(CELL), (y - TOP).div_euclid(CELL));
        (cx >= 0 && cy >= 0 && (cx as usize) < self.w && (cy as usize) < self.h).then(|| cy as usize * self.w + cx as usize)
    }

    fn level_btns(&self, win_w: i32) -> Rect {
        Rect::new(win_w - 16 - 250, 15, 250, 32)
    }

    fn seconds(&self) -> u64 {
        match self.state {
            State::Ready => 0,
            State::Playing => (justui::now_ms() - self.started) / 1000,
            _ => (self.ended - self.started) / 1000,
        }
    }

    fn draw(&self, win: &mut Window) {
        win.clear();
        let ww = win.width();
        let flags = self.flag.iter().filter(|&&f| f).count();
        let left = self.mines as i64 - flags as i64;
        win.canvas.fill_round(Rect::new(PAD, 14, 92, 34), 10, ui::PANEL);
        win.text(Font::Strong, PAD + 12, 21, &format!("Mine {left}"), ui::TEXT);
        win.canvas.fill_round(Rect::new(PAD + 102, 14, 80, 34), 10, ui::PANEL);
        let s = self.seconds();
        win.text(Font::Strong, PAD + 114, 21, &format!("{}:{:02}", s / 60, s % 60), ui::TEXT);
        let f = win.fonts;
        let labels: Vec<&str> = LEVELS.iter().map(|l| l.0).collect();
        ui::segmented(&mut win.canvas, f, self.level_btns(ww), &labels, self.level);
        let colors = [0, 0xFF5A_A9FF, 0xFF5D_E28A, 0xFFFF_6961, 0xFFBF_8CFF, 0xFFFF_B340, 0xFF64_D2FF, 0xFFF5_F2F8, 0xFFAE_AEB2];
        for i in 0..self.w * self.h {
            let (x, y) = (self.grid_x() + (i % self.w) as i32 * CELL, TOP + (i / self.w) as i32 * CELL);
            let r = Rect::new(x + 1, y + 1, CELL - 2, CELL - 2);
            let show_mine = self.mine[i] && (self.state == State::Lost);
            if self.open[i] || show_mine {
                let bg = if self.boom == Some(i) { 0xFFFF_453A } else { 0x1AFF_FFFF };
                win.canvas.fill_round(r, 5, bg);
                if self.mine[i] {
                    let (cx, cy) = (r.x + r.w / 2, r.y + r.h / 2);
                    win.canvas.fill_circle(cx, cy, 8, 0xFF1C_1C1E);
                    for k in 0..4 {
                        let (dx, dy) = [(1, 0), (0, 1), (1, 1), (1, -1)][k];
                        win.canvas.line(cx - dx * 11, cy - dy * 11, cx + dx * 11, cy + dy * 11, 2, 0xFF1C_1C1E);
                    }
                    win.canvas.fill_circle(cx - 3, cy - 3, 2, 0xFFFF_FFFF);
                } else {
                    let n = self.count(i);
                    if n > 0 {
                        win.text_centered(Font::Strong, r.x + r.w / 2, r.y + 5, &format!("{n}"), colors[n]);
                    }
                }
            } else {
                let hover = self.hover == Some(i) && matches!(self.state, State::Ready | State::Playing);
                win.canvas.fill_round(r, 5, if hover { 0x66FF_FFFF } else { 0x40FF_FFFF });
                if self.flag[i] {
                    let (cx, cy) = (r.x + r.w / 2, r.y + r.h / 2);
                    win.canvas.fill(Rect::new(cx - 1, cy - 9, 2, 18), 0xFFFF_FFFF);
                    win.canvas.triangle([(cx + 1, cy - 9), (cx + 10, cy - 4), (cx + 1, cy + 1)], 0xFFFF_453A);
                    if self.state == State::Lost && !self.mine[i] {
                        win.canvas.line(r.x + 4, r.y + 4, r.right() - 5, r.bottom() - 5, 2, 0xFFFF_453A);
                    }
                }
            }
        }
        if matches!(self.state, State::Won | State::Lost) && justui::now_ms() - self.ended > 600 {
            let area = Rect::new(0, 0, ww, win.height());
            let card = ui::overlay(&mut win.canvas, area, 280, 140);
            let (title, color) = if self.state == State::Won { ("Hai vinto!", ui::GREEN) } else { ("Boom!", ui::RED) };
            win.text_centered(Font::Title, card.x + 140, card.y + 22, title, color);
            let line = if self.state == State::Won { format!("In {} secondi", self.seconds()) } else { format!("Mancavano {} celle", self.remaining()) };
            win.text_centered(Font::Body, card.x + 140, card.y + 66, &line, ui::TEXT_DIM);
            win.text_centered(Font::Small, card.x + 140, card.y + 100, "Clic per un'altra partita", ui::TEXT_FAINT);
        }
    }

    fn remaining(&self) -> usize {
        (0..self.w * self.h).filter(|&i| !self.mine[i] && !self.open[i]).count()
    }
}

fn main(_args: &str) -> i32 {
    let mut level = 0;
    let (w, h) = size(level);
    let Ok(mut win) = Window::new("Campo minato", w, h) else { return 1 };
    let mut game = Game::new(level);
    loop {
        game.draw(&mut win);
        win.present();
        // While playing, the clock moves every second.
        let ev = match game.state {
            State::Playing => win.event(250),
            State::Won | State::Lost if justui::now_ms() - game.ended < 700 => win.event(100),
            _ => Some(win.wait()),
        };
        let Some(ev) = ev else { continue };
        match ev {
            Event::Close => return 0,
            Event::Press { x, y, double } => {
                let ww = win.width();
                let lb = game.level_btns(ww);
                if lb.contains(x, y) {
                    let segs = ui::segments(lb, LEVELS.len());
                    if let Some(l) = segs.iter().position(|s| s.contains(x, y)) {
                        level = l;
                        game = Game::new(level);
                        let (nw, nh) = size(level);
                        // A new window of the right size for the level.
                        drop(win);
                        let Ok(w2) = Window::new("Campo minato", nw, nh) else { return 1 };
                        win = w2;
                    }
                    continue;
                }
                if matches!(game.state, State::Won | State::Lost) {
                    if justui::now_ms() - game.ended > 600 {
                        game = Game::new(level);
                    }
                    continue;
                }
                let _ = double;
                if let Some(i) = game.cell_at(x, y) {
                    game.reveal(i);
                }
            }
            Event::Menu { x, y } => {
                if let Some(i) = game.cell_at(x, y) {
                    game.toggle_flag(i);
                }
            }
            Event::Move { x, y, .. } => game.hover = game.cell_at(x, y),
            Event::Leave => game.hover = None,
            Event::Key(k) => match k.key {
                Key::Char('f') => {
                    if let Some(i) = game.hover {
                        game.toggle_flag(i);
                    }
                }
                Key::Char('n') | Key::Enter => game = Game::new(level),
                _ => {}
            },
            _ => {}
        }
    }
}

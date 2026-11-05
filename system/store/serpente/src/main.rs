//! Serpente: eat the apples, grow, don't bite yourself. The arrows steer,
//! the space bar pauses; it gets faster as you grow.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::format;
use justui::{ui, Event, Font, Key, Rect, Rng, Window};

justrt::main!(main);

const COLS: i32 = 26;
const ROWS: i32 = 18;
const CELL: i32 = 24;
const TOP: i32 = 56;
const PAD: i32 = 14;
const W: i32 = COLS * CELL + 2 * PAD;
const H: i32 = ROWS * CELL + TOP + PAD;

/// Where the best score is kept (the app's own folder).
fn best_file() -> alloc::string::String {
    justrt::app::data_file("best")
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Ready,
    Running,
    Paused,
    Over,
}

struct Game {
    body: VecDeque<(i32, i32)>,
    dir: (i32, i32),
    /// Turns typed but not yet made (two quick ones in a row work).
    queued: VecDeque<(i32, i32)>,
    apple: (i32, i32),
    score: u32,
    best: u32,
    state: State,
    rng: Rng,
    next_step: u64,
    /// When the last apple was eaten (it flashes).
    ate: u64,
}

impl Game {
    fn new(best: u32) -> Game {
        let mut body = VecDeque::new();
        for i in 0..4 {
            body.push_back((8 - i, ROWS / 2));
        }
        let mut g = Game {
            body,
            dir: (1, 0),
            queued: VecDeque::new(),
            apple: (0, 0),
            score: 0,
            best,
            state: State::Ready,
            rng: Rng::new(),
            next_step: 0,
            ate: 0,
        };
        g.place_apple();
        g
    }

    fn place_apple(&mut self) {
        loop {
            let p = (self.rng.below(COLS as u64) as i32, self.rng.below(ROWS as u64) as i32);
            if !self.body.contains(&p) {
                self.apple = p;
                return;
            }
        }
    }

    fn interval(&self) -> u64 {
        (140u64).saturating_sub(self.score as u64 * 3).max(60)
    }

    fn step(&mut self) {
        if let Some(d) = self.queued.pop_front() {
            self.dir = d;
        }
        let (hx, hy) = self.body[0];
        let head = (hx + self.dir.0, hy + self.dir.1);
        let tail = *self.body.back().unwrap_or(&head);
        let hits_self = self.body.iter().any(|&p| p == head) && head != tail;
        if head.0 < 0 || head.1 < 0 || head.0 >= COLS || head.1 >= ROWS || hits_self {
            self.state = State::Over;
            if self.score > self.best {
                self.best = self.score;
                let _ = justrt::fs::write(&best_file(), format!("{}", self.best).as_bytes());
            }
            return;
        }
        self.body.push_front(head);
        if head == self.apple {
            self.score += 1;
            self.ate = justui::now_ms();
            self.place_apple();
        } else {
            self.body.pop_back();
        }
    }

    fn turn(&mut self, d: (i32, i32)) {
        let last = *self.queued.back().unwrap_or(&self.dir);
        if d != last && d != (-last.0, -last.1) && self.queued.len() < 3 {
            self.queued.push_back(d);
        }
        if self.state == State::Ready {
            self.state = State::Running;
            self.next_step = justui::now_ms();
        }
    }

    fn draw(&self, win: &mut Window) {
        win.clear();
        win.text(Font::Heading, PAD + 4, 14, "Serpente", ui::TEXT);
        let s = format!("Mele {}   ·   Record {}", self.score, self.best);
        let f = win.fonts;
        let w = f.width(Font::Strong, &s);
        win.text(Font::Strong, W - PAD - 4 - w, 18, &s, ui::TEXT_DIM);
        let field = Rect::new(PAD, TOP, COLS * CELL, ROWS * CELL);
        win.canvas.fill_round(field, 12, 0x6612_1A14);
        for y in 0..ROWS {
            for x in 0..COLS {
                if (x + y) % 2 == 0 {
                    win.canvas.fill(Rect::new(PAD + x * CELL, TOP + y * CELL, CELL, CELL), 0x0AFF_FFFF);
                }
            }
        }
        // The apple, with a leaf.
        let (ax, ay) = (PAD + self.apple.0 * CELL + CELL / 2, TOP + self.apple.1 * CELL + CELL / 2);
        win.canvas.fill_circle(ax, ay + 1, CELL / 2 - 3, 0xFFFF_453A);
        win.canvas.fill_circle(ax - 3, ay - 2, 3, 0x66FF_FFFF);
        win.canvas.line(ax, ay - 8, ax + 4, ay - 11, 2, 0xFF30_D158);
        let n = self.body.len() as i32;
        for (i, &(x, y)) in self.body.iter().enumerate().rev() {
            // From bright green at the head to a darker tail.
            let k = (i as i32 * 120 / n.max(1)) as u32;
            let g = 0xD8 - k.min(0x70);
            let color = 0xFF00_0000 | (0x30 << 16) | (g << 8) | 0x60;
            let r = Rect::new(PAD + x * CELL + 2, TOP + y * CELL + 2, CELL - 4, CELL - 4);
            win.canvas.fill_round(r, 7, color);
            if i == 0 {
                let (ex, ey) = (self.dir.1.abs() * 5, self.dir.0.abs() * 5);
                let (cx, cy) = (r.x + r.w / 2 + self.dir.0 * 3, r.y + r.h / 2 + self.dir.1 * 3);
                for s in [-1, 1] {
                    win.canvas.fill_circle(cx + s * ex, cy + s * ey, 3, 0xFFFF_FFFF);
                    win.canvas.fill_circle(cx + s * ex + self.dir.0, cy + s * ey + self.dir.1, 1, 0xFF10_1010);
                }
            }
        }
        let area = Rect::new(0, 0, W, H);
        match self.state {
            State::Ready => {
                let card = ui::overlay(&mut win.canvas, area, 320, 120);
                win.text_centered(Font::Heading, card.x + 160, card.y + 24, "Pronto?", ui::TEXT);
                win.text_centered(Font::Body, card.x + 160, card.y + 66, "Premi una freccia per partire", ui::TEXT_DIM);
            }
            State::Paused => {
                let card = ui::overlay(&mut win.canvas, area, 260, 100);
                win.text_centered(Font::Heading, card.x + 130, card.y + 22, "In pausa", ui::TEXT);
                win.text_centered(Font::Small, card.x + 130, card.y + 62, "Spazio per riprendere", ui::TEXT_FAINT);
            }
            State::Over => {
                let card = ui::overlay(&mut win.canvas, area, 300, 140);
                win.text_centered(Font::Title, card.x + 150, card.y + 20, "Fine partita", ui::TEXT);
                win.text_centered(Font::Body, card.x + 150, card.y + 66, &format!("{} mele", self.score), ui::TEXT_DIM);
                win.text_centered(Font::Small, card.x + 150, card.y + 102, "Invio o clic per ricominciare", ui::TEXT_FAINT);
            }
            State::Running => {}
        }
    }
}

fn main(_args: &str) -> i32 {
    let best = justrt::fs::read_to_string(&best_file()).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
    let Ok(mut win) = Window::new("Serpente", W, H) else { return 1 };
    let mut game = Game::new(best);
    let mut dirty = true;
    loop {
        if dirty {
            game.draw(&mut win);
            win.present();
            dirty = false;
        }
        let ev = if game.state == State::Running {
            let now = justui::now_ms();
            if now >= game.next_step {
                game.step();
                game.next_step = now + game.interval();
                dirty = true;
                continue;
            }
            win.event(game.next_step - now)
        } else {
            Some(win.wait())
        };
        let Some(ev) = ev else { continue };
        dirty = true;
        match ev {
            Event::Close => return 0,
            Event::Key(k) => match k.key {
                Key::Up | Key::Char('w') => game.turn((0, -1)),
                Key::Down | Key::Char('s') => game.turn((0, 1)),
                Key::Left | Key::Char('a') => game.turn((-1, 0)),
                Key::Right | Key::Char('d') => game.turn((1, 0)),
                Key::Char(' ') => {
                    game.state = match game.state {
                        State::Running => State::Paused,
                        State::Paused => {
                            game.next_step = justui::now_ms();
                            State::Running
                        }
                        s => s,
                    }
                }
                Key::Enter if game.state == State::Over => game = Game::new(game.best),
                _ => dirty = false,
            },
            Event::Press { .. } if game.state == State::Over => game = Game::new(game.best),
            _ => dirty = false,
        }
    }
}

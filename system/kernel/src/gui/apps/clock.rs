//! Orologio: the time here and in a few cities, a stopwatch with laps and
//! a timer that rings.

use crate::dev::input::{Key, KeyEvent};
use crate::gui::gfx::{Canvas, Rect};
use crate::gui::text::{self, Font};
use crate::gui::ui;
use crate::gui::wm::{Action, App};
use crate::time;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// sin of 0, 6, 12... 90 degrees, in 1/1000.
const SIN: [i32; 16] = [0, 105, 208, 309, 407, 500, 588, 669, 743, 809, 866, 914, 951, 978, 995, 1000];

/// sin and cos of `step` sixtieths of a turn, clockwise from 12 o'clock
/// (screen coordinates): (x, y) in 1/1000.
pub fn dial(step: i32) -> (i32, i32) {
    let s = step.rem_euclid(60);
    let q = s / 15;
    let i = (s % 15) as usize;
    let (a, b) = (SIN[i], SIN[15 - i]);
    match q {
        0 => (a, -b),
        1 => (b, a),
        2 => (-a, b),
        _ => (-b, -a),
    }
}

/// Cities and their offset from here in hours.
const CITIES: [(&str, i64); 5] = [("Londra", -1), ("New York", -6), ("San Francisco", -9), ("Tokyo", 8), ("Sydney", 10)];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Clock,
    Stopwatch,
    Timer,
}

pub struct Clock {
    tab: Tab,
    /// Stopwatch: time counted before the current run, and when it started.
    sw_acc: u64,
    sw_start: Option<u64>,
    laps: Vec<u64>,
    /// Timer: the length chosen, and when it ends (if running) or the time
    /// left (if paused).
    timer_len: u64,
    timer_end: Option<u64>,
    timer_left: Option<u64>,
    ringing: bool,
    hover: Option<Rect>,
    last_sec: u64,
}

fn fmt_ms(ms: u64) -> String {
    format!("{:02}:{:02},{:02}", ms / 60_000, ms / 1000 % 60, ms / 10 % 100)
}

fn fmt_left(ms: u64) -> String {
    let s = ms.div_ceil(1000);
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

/// An analogue face with hands for `h:m:s`.
fn draw_face(c: &mut Canvas, cx: i32, cy: i32, r: i32, h: u64, m: u64, s: Option<u64>) {
    c.fill_circle(cx, cy, r + 3, 0x40_00_00_00);
    c.fill_circle(cx, cy, r, 0xFFF5_F5F7);
    for i in 0..60 {
        let (dx, dy) = dial(i);
        let (r0, w, col) = if i % 5 == 0 { (r - 14, 3, 0xFF1C_1C1E) } else { (r - 7, 1, 0xFF8E_8E93) };
        let (x0, y0) = (cx + dx * r0 / 1000, cy + dy * r0 / 1000);
        let (x1, y1) = (cx + dx * (r - 4) / 1000, cy + dy * (r - 4) / 1000);
        c.line(x0, y0, x1, y1, w, col);
    }
    if r > 60 {
        for n in 1..=12 {
            let (dx, dy) = dial(n * 5);
            let (lh, _) = text::metrics(Font::Strong);
            let rr = r - 30;
            text::draw_centered(c, Font::Strong, cx + dx * rr / 1000, cy + dy * rr / 1000 - lh / 2, &format!("{n}"), 0xFF1C_1C1E);
        }
    }
    let hand = |c: &mut Canvas, step: i32, len: i32, w: i32, col: u32| {
        let (dx, dy) = dial(step);
        c.line(cx, cy, cx + dx * len / 1000, cy + dy * len / 1000, w, col);
    };
    hand(c, ((h % 12) * 5 + m / 12) as i32, r * 50 / 100, (r / 20).max(3), 0xFF1C_1C1E);
    hand(c, m as i32, r * 75 / 100, (r / 28).max(2), 0xFF1C_1C1E);
    if let Some(s) = s {
        hand(c, s as i32, r * 85 / 100, 1, ui::ORANGE);
    }
    c.fill_circle(cx, cy, (r / 22).max(3), ui::ORANGE);
}

impl Clock {
    pub fn new() -> Clock {
        Clock {
            tab: Tab::Clock,
            sw_acc: 0,
            sw_start: None,
            laps: Vec::new(),
            timer_len: 5 * 60_000,
            timer_end: None,
            timer_left: None,
            ringing: false,
            hover: None,
            last_sec: 0,
        }
    }

    fn tabs(area: Rect) -> Rect {
        Rect::new(area.x + area.w / 2 - 170, area.y + 14, 340, 34)
    }

    fn stopwatch(&self) -> u64 {
        self.sw_acc + self.sw_start.map_or(0, |s| time::uptime_ms() - s)
    }

    fn timer_remaining(&self) -> u64 {
        match (self.timer_end, self.timer_left) {
            (Some(end), _) => end.saturating_sub(time::uptime_ms()),
            (None, Some(left)) => left,
            _ => self.timer_len,
        }
    }

    /// The two round buttons at the bottom (left, right).
    fn controls(area: Rect) -> [Rect; 2] {
        let y = area.bottom() - 100;
        [Rect::new(area.x + 50, y, 80, 80), Rect::new(area.right() - 130, y, 80, 80)]
    }

    fn presets(area: Rect) -> Vec<(Rect, u64)> {
        [1u64, 3, 5, 10, 15, 30]
            .iter()
            .enumerate()
            .map(|(i, &m)| (Rect::new(area.x + area.w / 2 - 210 + i as i32 * 72, area.y + 80, 64, 34), m * 60_000))
            .collect()
    }

    fn ring(&mut self) {
        self.ringing = true;
        self.timer_end = None;
        self.timer_left = None;
        if crate::dev::audio::available() {
            // Three short rising notes.
            let mut s = crate::dev::audio::tone(880, 160);
            for hz in [1175, 1568] {
                s.samples.extend(crate::dev::audio::tone(hz, 160).samples);
            }
            let _ = crate::dev::audio::play(s);
        }
        super::super::notify("Timer", "Il tempo è scaduto.", "clock");
    }
}

impl App for Clock {
    fn title(&self) -> String {
        String::from("Orologio")
    }

    fn icon(&self) -> &str {
        "clock"
    }

    fn kind(&self) -> &'static str {
        "clock"
    }

    fn size(&self) -> (i32, i32) {
        (560, 520)
    }

    fn draw(&mut self, c: &mut Canvas, area: Rect, _blur: &Canvas, _focused: bool) {
        let sel = match self.tab {
            Tab::Clock => 0,
            Tab::Stopwatch => 1,
            Tab::Timer => 2,
        };
        ui::segmented(c, Self::tabs(area), &["Orologio", "Cronometro", "Timer"], sel);
        let cx = area.x + area.w / 2;
        match self.tab {
            Tab::Clock => {
                let d = time::from_epoch(time::now());
                draw_face(c, area.x + 140, area.y + 200, 110, d.hour, d.min, Some(d.sec));
                text::draw(c, Font::Body, area.x + 290, area.y + 96, "Qui", ui::text_dim());
                text::draw(c, Font::Clock, area.x + 286, area.y + 114, &format!("{:02}:{:02}", d.hour, d.min), ui::text());
                let mut y = area.y + 340;
                for (name, off) in CITIES {
                    let t = time::now() as i64 + off * 3600;
                    let cd = time::from_epoch(t.max(0) as u64);
                    let day = match (cd.day as i64 - d.day as i64).signum() {
                        0 => "oggi",
                        1 => "domani",
                        _ => "ieri",
                    };
                    let r = Rect::new(area.x + 24, y, area.w - 48, 30);
                    text::draw(c, Font::Strong, r.x, r.y + 4, name, ui::text());
                    let rel = if off >= 0 { format!("{day}, +{off} h") } else { format!("{day}, {off} h") };
                    text::draw(c, Font::Small, r.x + 150, r.y + 7, &rel, ui::text_dim());
                    text::draw_right(c, Font::Heading, r.right(), r.y, &format!("{:02}:{:02}", cd.hour, cd.min), ui::text());
                    c.fill(Rect::new(r.x, r.bottom() + 2, r.w, 1), ui::line());
                    y += 34;
                }
            }
            Tab::Stopwatch => {
                let ms = self.stopwatch();
                let shown = fmt_ms(ms);
                text::draw_centered(c, Font::Display, cx, area.y + 90, &shown, ui::text());
                let [l, r] = Self::controls(area);
                let running = self.sw_start.is_some();
                let left = if running { "Giro" } else { "Azzera" };
                c.fill_circle(l.x + 40, l.y + 40, 40, if self.hover == Some(l) { 0x60FF_FFFF } else { 0x40FF_FFFF });
                text::draw_centered(c, Font::Strong, l.x + 40, l.y + 30, left, ui::text());
                let (bg, label) = if running { (0x66FF_453A, "Stop") } else { (0x6630_D158, "Avvia") };
                c.fill_circle(r.x + 40, r.y + 40, 40, bg);
                text::draw_centered(c, Font::Strong, r.x + 40, r.y + 30, label, if running { 0xFFFF_6961 } else { 0xFF5D_E28A });
                let mut y = area.y + 190;
                let mut prev = 0;
                let laps: Vec<(usize, u64)> = self.laps.iter().copied().enumerate().collect();
                for (i, total) in laps.iter().rev().take(6) {
                    let lap = total - if *i > 0 { self.laps[i - 1] } else { 0 };
                    text::draw(c, Font::Body, area.x + 150, y, &format!("Giro {}", i + 1), ui::text_dim());
                    text::draw_right(c, Font::Body, area.right() - 150, y, &fmt_ms(lap), ui::text());
                    c.fill(Rect::new(area.x + 150, y + 26, area.w - 300, 1), ui::line());
                    y += 32;
                    prev = *total;
                }
                let _ = prev;
            }
            Tab::Timer => {
                let left = self.timer_remaining();
                let total = self.timer_len.max(1);
                let (rx, ry, rr) = (cx, area.y + 260, 110);
                c.arc(rx, ry, rr, 8, 1000, 0x30FF_FFFF);
                let share = (left * 1000 / total) as u32;
                if share > 0 {
                    c.arc(rx, ry, rr, 8, share.max(1), ui::ORANGE);
                }
                let (lh, _) = text::metrics(Font::Clock);
                text::draw_centered(c, Font::Clock, rx, ry - lh / 2, &fmt_left(left), ui::text());
                if self.timer_end.is_none() && self.timer_left.is_none() {
                    for (r, ms) in Self::presets(area) {
                        let sel = ms == self.timer_len;
                        c.fill_round(r, 17, if sel { ui::ORANGE } else if self.hover == Some(r) { 0x40FF_FFFF } else { 0x26FF_FFFF });
                        text::draw_centered(c, Font::Strong, r.x + r.w / 2, r.y + 8, &format!("{} min", ms / 60_000), ui::text());
                    }
                } else {
                    text::draw_centered(c, Font::Body, cx, area.y + 88, &format!("Timer di {} minuti", self.timer_len / 60_000), ui::text_dim());
                }
                if self.ringing {
                    text::draw_centered(c, Font::Strong, cx, ry + 40, "Tempo scaduto!", ui::ORANGE);
                }
                let [l, r] = Self::controls(area);
                c.fill_circle(l.x + 40, l.y + 40, 40, if self.hover == Some(l) { 0x60FF_FFFF } else { 0x40FF_FFFF });
                text::draw_centered(c, Font::Strong, l.x + 40, l.y + 30, "Annulla", ui::text());
                let running = self.timer_end.is_some();
                let (bg, label, fg) = if running {
                    (0x66FF_9F0A, "Pausa", 0xFFFF_B340)
                } else if self.timer_left.is_some() {
                    (0x6630_D158, "Riprendi", 0xFF5D_E28A)
                } else {
                    (0x6630_D158, "Avvia", 0xFF5D_E28A)
                };
                c.fill_circle(r.x + 40, r.y + 40, 40, bg);
                text::draw_centered(c, Font::Strong, r.x + 40, r.y + 30, label, fg);
            }
        }
        let _ = ui::text_faint();
    }

    fn press(&mut self, x: i32, y: i32, _double: bool, area: Rect) -> Action {
        let (ax, ay) = (area.x + x, area.y + y);
        let tabs = ui::segments(Self::tabs(area), 3);
        for (i, t) in tabs.iter().enumerate() {
            if t.contains(ax, ay) {
                self.tab = [Tab::Clock, Tab::Stopwatch, Tab::Timer][i];
                return Action::Redraw;
            }
        }
        let [l, r] = Self::controls(area);
        let now = time::uptime_ms();
        match self.tab {
            Tab::Clock => {}
            Tab::Stopwatch => {
                if r.contains(ax, ay) {
                    match self.sw_start.take() {
                        Some(s) => self.sw_acc += now - s,
                        None => self.sw_start = Some(now),
                    }
                } else if l.contains(ax, ay) {
                    if self.sw_start.is_some() {
                        self.laps.push(self.stopwatch());
                    } else {
                        self.sw_acc = 0;
                        self.laps.clear();
                    }
                }
            }
            Tab::Timer => {
                self.ringing = false;
                if r.contains(ax, ay) {
                    if let Some(end) = self.timer_end.take() {
                        self.timer_left = Some(end.saturating_sub(now));
                    } else {
                        let left = self.timer_left.take().unwrap_or(self.timer_len);
                        self.timer_end = Some(now + left);
                    }
                } else if l.contains(ax, ay) {
                    self.timer_end = None;
                    self.timer_left = None;
                } else if self.timer_end.is_none() && self.timer_left.is_none() {
                    if let Some((_, ms)) = Self::presets(area).into_iter().find(|(r, _)| r.contains(ax, ay)) {
                        self.timer_len = ms;
                    }
                }
            }
        }
        Action::Redraw
    }

    fn hover(&mut self, at: Option<(i32, i32)>, area: Rect) -> bool {
        let mut targets: Vec<Rect> = Self::controls(area).to_vec();
        targets.extend(Self::presets(area).into_iter().map(|(r, _)| r));
        let h = at.and_then(|(x, y)| targets.into_iter().find(|r| r.contains(area.x + x, area.y + y)));
        let changed = h != self.hover;
        self.hover = h;
        changed
    }

    fn key(&mut self, ev: KeyEvent) -> Action {
        match ev.key {
            Key::Char(' ') if self.tab == Tab::Stopwatch => {
                let now = time::uptime_ms();
                match self.sw_start.take() {
                    Some(s) => self.sw_acc += now - s,
                    None => self.sw_start = Some(now),
                }
                Action::Redraw
            }
            Key::Tab => {
                self.tab = match self.tab {
                    Tab::Clock => Tab::Stopwatch,
                    Tab::Stopwatch => Tab::Timer,
                    Tab::Timer => Tab::Clock,
                };
                Action::Redraw
            }
            _ => Action::None,
        }
    }

    fn tick(&mut self) -> bool {
        if self.timer_end.is_some_and(|e| time::uptime_ms() >= e) {
            self.ring();
            return true;
        }
        let sec = time::now();
        let changed = sec != self.last_sec;
        self.last_sec = sec;
        changed || self.timer_end.is_some()
    }

    /// The stopwatch runs smoothly (hundredths).
    fn animating(&self) -> bool {
        self.tab == Tab::Stopwatch && self.sw_start.is_some()
    }

    fn frame(&mut self) -> bool {
        true
    }
}

//! Calcolatrice: the four operations, percent and sign, in decimal fixed
//! point (the kernel has no floating point): twelve decimals, results up to
//! a thousand billion billion.

use crate::dev::input::{Key, KeyEvent};
use crate::gui::gfx::{Canvas, Rect};
use crate::gui::text::{self, Font};
use crate::gui::ui::{self, Anim};
use crate::gui::wm::{Action, App};
use alloc::string::String;

const SCALE: i128 = 1_000_000_000_000;
const DECIMALS: u32 = 12;
/// Larger results are an error.
const LIMIT: i128 = 1_000_000_000_000_000_000_000 * SCALE;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Op {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum K {
    Digit(u8),
    Comma,
    Op(Op),
    Equals,
    Clear,
    Sign,
    Percent,
}

const KEYS: [[(&str, K); 4]; 5] = [
    [("AC", K::Clear), ("±", K::Sign), ("%", K::Percent), ("÷", K::Op(Op::Div))],
    [("7", K::Digit(7)), ("8", K::Digit(8)), ("9", K::Digit(9)), ("×", K::Op(Op::Mul))],
    [("4", K::Digit(4)), ("5", K::Digit(5)), ("6", K::Digit(6)), ("−", K::Op(Op::Sub))],
    [("1", K::Digit(1)), ("2", K::Digit(2)), ("3", K::Digit(3)), ("+", K::Op(Op::Add))],
    [("0", K::Digit(0)), ("", K::Digit(0)), (",", K::Comma), ("=", K::Equals)],
];

pub struct Calculator {
    /// What is being typed, as text ("12,5").
    entry: String,
    /// The value shown when nothing is being typed.
    value: i128,
    /// The first operand and the operation waiting for the second.
    pending: Option<(i128, Op)>,
    /// The last operation, repeated by pressing = again.
    repeat: Option<(Op, i128)>,
    error: bool,
    hover: Option<(usize, usize)>,
    /// The key pressed last, lit for a moment.
    flash: Option<((usize, usize), Anim)>,
}

fn parse(s: &str) -> i128 {
    let (int, frac) = s.split_once(',').unwrap_or((s, ""));
    let mut v: i128 = 0;
    for d in int.bytes().filter(u8::is_ascii_digit) {
        v = v.saturating_mul(10).saturating_add((d - b'0') as i128);
    }
    let mut v = v.saturating_mul(SCALE);
    let mut unit = SCALE / 10;
    for d in frac.bytes().filter(u8::is_ascii_digit).take(DECIMALS as usize) {
        v += (d - b'0') as i128 * unit;
        unit /= 10;
    }
    if s.starts_with('-') {
        -v
    } else {
        v
    }
}

/// Groups thousands with dots, as written in Italian.
fn group(int: &str) -> String {
    let mut out = String::new();
    let n = int.len();
    for (i, ch) in int.chars().enumerate() {
        if i > 0 && (n - i) % 3 == 0 {
            out.push('.');
        }
        out.push(ch);
    }
    out
}

/// A value for the display, with at most `digits` significant digits.
fn format(v: i128, digits: usize) -> String {
    let neg = v < 0;
    let a = v.unsigned_abs();
    let int = a / SCALE as u128;
    let mut frac = a % SCALE as u128;
    let int_s = alloc::format!("{int}");
    // As many decimals as fit, rounded.
    let room = digits.saturating_sub(int_s.len()).min(DECIMALS as usize);
    let cut = 10u128.pow(DECIMALS - room as u32);
    let mut int = int;
    frac = (frac + cut / 2) / cut;
    if room > 0 && frac >= 10u128.pow(room as u32) {
        frac = 0;
        int += 1;
    } else if room == 0 && (a % SCALE as u128) >= SCALE as u128 / 2 {
        int += 1;
    }
    let mut out = String::new();
    if neg && (int != 0 || frac != 0) {
        out.push('-');
    }
    out.push_str(&group(&alloc::format!("{int}")));
    if room > 0 && frac != 0 {
        let mut f = alloc::format!("{frac:0width$}", width = room);
        while f.ends_with('0') {
            f.pop();
        }
        out.push(',');
        out.push_str(&f);
    }
    out
}

fn apply(a: i128, op: Op, b: i128) -> Option<i128> {
    let r = match op {
        Op::Add => a.checked_add(b)?,
        Op::Sub => a.checked_sub(b)?,
        Op::Mul => match a.checked_mul(b) {
            Some(p) => p / SCALE,
            // Too big for exact: drop half the decimals of each first.
            None => (a / 1_000_000).checked_mul(b / 1_000_000)?,
        },
        Op::Div => {
            if b == 0 {
                return None;
            }
            match a.checked_mul(SCALE) {
                Some(p) => p / b,
                None => (a / b).checked_mul(SCALE)?,
            }
        }
    };
    (r.abs() < LIMIT).then_some(r)
}

impl Calculator {
    pub fn new() -> Calculator {
        Calculator { entry: String::new(), value: 0, pending: None, repeat: None, error: false, hover: None, flash: None }
    }

    fn current(&self) -> i128 {
        if self.entry.is_empty() {
            self.value
        } else {
            parse(&self.entry)
        }
    }

    fn display(&self) -> String {
        if self.error {
            return String::from("Errore");
        }
        if !self.entry.is_empty() {
            let (int, frac) = match self.entry.split_once(',') {
                Some((i, f)) => (i, Some(f)),
                None => (self.entry.as_str(), None),
            };
            let (sign, int) = match int.strip_prefix('-') {
                Some(i) => ("-", i),
                None => ("", int),
            };
            let mut s = alloc::format!("{sign}{}", group(if int.is_empty() { "0" } else { int }));
            if let Some(f) = frac {
                s.push(',');
                s.push_str(f);
            }
            return s;
        }
        format(self.value, 12)
    }

    fn press_key(&mut self, k: K) {
        if self.error && k != K::Clear {
            self.error = false;
            self.entry.clear();
            self.value = 0;
            self.pending = None;
        }
        match k {
            K::Digit(d) => {
                let digits = self.entry.bytes().filter(u8::is_ascii_digit).count();
                if digits >= 15 {
                    return;
                }
                if self.entry == "0" {
                    self.entry.clear();
                } else if self.entry == "-0" {
                    self.entry = String::from("-");
                }
                self.entry.push((b'0' + d) as char);
            }
            K::Comma => {
                if self.entry.is_empty() {
                    self.entry.push('0');
                }
                if !self.entry.contains(',') {
                    self.entry.push(',');
                }
            }
            K::Clear => {
                if !self.entry.is_empty() {
                    self.entry.clear();
                } else {
                    *self = Calculator { hover: self.hover, ..Calculator::new() };
                }
            }
            K::Sign => {
                if self.entry.is_empty() {
                    self.value = -self.value;
                } else if let Some(rest) = self.entry.strip_prefix('-') {
                    self.entry = String::from(rest);
                } else {
                    self.entry.insert(0, '-');
                }
            }
            K::Percent => {
                let v = self.current();
                // 50 + 10% is 55; otherwise a hundredth.
                let r = match self.pending {
                    Some((a, Op::Add | Op::Sub)) => (a / 100).checked_mul(v).map(|p| p / SCALE),
                    _ => Some(v / 100),
                };
                match r {
                    Some(r) => self.value = r,
                    None => return self.fail(),
                }
                self.entry.clear();
            }
            K::Op(op) => {
                let v = self.current();
                let v = match self.pending {
                    Some((a, prev)) if !self.entry.is_empty() => match apply(a, prev, v) {
                        Some(r) => r,
                        None => return self.fail(),
                    },
                    _ => v,
                };
                self.value = v;
                self.pending = Some((v, op));
                self.entry.clear();
                self.repeat = None;
            }
            K::Equals => {
                let v = self.current();
                let r = match (self.pending.take(), self.repeat) {
                    (Some((a, op)), _) => {
                        self.repeat = Some((op, v));
                        apply(a, op, v)
                    }
                    (None, Some((op, b))) => apply(v, op, b),
                    (None, None) => Some(v),
                };
                match r {
                    Some(r) => self.value = r,
                    None => return self.fail(),
                }
                self.entry.clear();
            }
        }
    }

    fn fail(&mut self) {
        self.error = true;
        self.entry.clear();
        self.pending = None;
        self.repeat = None;
    }

    fn key_rect(area: Rect, row: usize, col: usize) -> Rect {
        let pad = 14;
        let gap = 10;
        let top = area.y + 128;
        let size = (area.w - 2 * pad - 3 * gap) / 4;
        let h = (area.bottom() - pad - top - 4 * gap) / 5;
        let x = area.x + pad + col as i32 * (size + gap);
        let y = top + row as i32 * (h + gap);
        if row == 4 && col == 0 {
            return Rect::new(x, y, 2 * size + gap, h); // the wide zero
        }
        Rect::new(x, y, size, h)
    }

    fn key_at(area: Rect, x: i32, y: i32) -> Option<(usize, usize)> {
        for (r, row) in KEYS.iter().enumerate() {
            for c in 0..row.len() {
                if r == 4 && c == 1 {
                    continue;
                }
                if Self::key_rect(area, r, c).contains(x, y) {
                    return Some((r, c));
                }
            }
        }
        None
    }

    fn lit(&mut self, at: (usize, usize)) {
        self.flash = Some((at, Anim::new(180)));
    }
}

impl App for Calculator {
    fn title(&self) -> String {
        String::from("Calcolatrice")
    }

    fn icon(&self) -> &str {
        "calculator"
    }

    fn kind(&self) -> &'static str {
        "calculator"
    }

    fn size(&self) -> (i32, i32) {
        (320, 470)
    }

    fn draw(&mut self, c: &mut Canvas, area: Rect, _blur: &Canvas, _focused: bool) {
        // The operation waiting, small, above the number.
        if let Some((a, op)) = self.pending {
            let sym = match op {
                Op::Add => "+",
                Op::Sub => "−",
                Op::Mul => "×",
                Op::Div => "÷",
            };
            let s = alloc::format!("{} {sym}", format(a, 12));
            text::draw_right(c, Font::Body, area.right() - 22, area.y + 18, &s, ui::text_dim());
        }
        let shown = self.display();
        let font = [Font::Display, Font::Clock, Font::Title, Font::Heading]
            .into_iter()
            .find(|&f| text::width(f, &shown) <= area.w - 44)
            .unwrap_or(Font::Heading);
        let (lh, _) = text::metrics(font);
        text::draw_right(c, font, area.right() - 22, area.y + 112 - lh, &shown, ui::text());
        for (r, row) in KEYS.iter().enumerate() {
            for (col, (label, k)) in row.iter().enumerate() {
                if r == 4 && col == 1 {
                    continue;
                }
                let rect = Self::key_rect(area, r, col);
                let active_op = matches!(k, K::Op(op) if self.pending.is_some_and(|(_, p)| p == *op) && self.entry.is_empty());
                let (mut bg, fg) = match k {
                    K::Op(_) | K::Equals if active_op => (0xFFFF_FFFF, ui::ORANGE),
                    K::Op(_) | K::Equals => (ui::ORANGE, 0xFFFF_FFFF),
                    K::Clear | K::Sign | K::Percent => (0xFFA5_A5AA, 0xFF1C_1C1E),
                    _ => (0xFF4A_4A50, 0xFFFF_FFFF),
                };
                let label = if *k == K::Clear && !self.entry.is_empty() { "C" } else { label };
                if self.hover == Some((r, col)) {
                    bg = crate::gui::gfx::blend(bg, 0xFFFF_FFFF, 40);
                }
                if let Some((at, a)) = &self.flash {
                    if *at == (r, col) {
                        bg = crate::gui::gfx::blend(bg, 0xFFFF_FFFF, 140 - a.t() * 140 / 1000);
                    }
                }
                c.fill_round(rect, rect.h / 2, bg);
                let (lh, _) = text::metrics(Font::Heading);
                if r == 4 && col == 0 {
                    text::draw(c, Font::Heading, rect.x + rect.h / 2 - 6, rect.y + (rect.h - lh) / 2, label, fg);
                } else {
                    text::draw_centered(c, Font::Heading, rect.x + rect.w / 2, rect.y + (rect.h - lh) / 2, label, fg);
                }
            }
        }
    }

    fn press(&mut self, x: i32, y: i32, _double: bool, area: Rect) -> Action {
        let Some((r, col)) = Self::key_at(area, area.x + x, area.y + y) else { return Action::None };
        self.press_key(KEYS[r][col].1);
        self.lit((r, col));
        Action::Redraw
    }

    fn hover(&mut self, at: Option<(i32, i32)>, area: Rect) -> bool {
        let h = at.and_then(|(x, y)| Self::key_at(area, area.x + x, area.y + y));
        let changed = h != self.hover;
        self.hover = h;
        changed
    }

    fn key(&mut self, ev: KeyEvent) -> Action {
        let k = match ev.key {
            Key::Char(d @ '0'..='9') => K::Digit(d as u8 - b'0'),
            Key::Char(',' | '.') => K::Comma,
            Key::Char('+') => K::Op(Op::Add),
            Key::Char('-') => K::Op(Op::Sub),
            Key::Char('*' | 'x') => K::Op(Op::Mul),
            Key::Char('/' | ':') => K::Op(Op::Div),
            Key::Char('%') => K::Percent,
            Key::Char('=') | Key::Enter => K::Equals,
            Key::Escape | Key::Delete => K::Clear,
            Key::Backspace => {
                if !self.entry.is_empty() {
                    self.entry.pop();
                    if self.entry == "-" {
                        self.entry.clear();
                    }
                }
                return Action::Redraw;
            }
            _ => return Action::None,
        };
        self.press_key(k);
        let at = KEYS.iter().enumerate().find_map(|(r, row)| row.iter().position(|(_, kk)| *kk == k).map(|c| (r, c)));
        if let Some(at) = at {
            self.lit(at);
        }
        Action::Redraw
    }

    fn animating(&self) -> bool {
        self.flash.as_ref().is_some_and(|(_, a)| !a.done())
    }

    fn frame(&mut self) -> bool {
        if self.flash.as_ref().is_some_and(|(_, a)| a.done()) {
            self.flash = None;
        }
        true
    }
}

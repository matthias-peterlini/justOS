//! A text area with several lines: word wrap, a cursor, a selection made
//! with the mouse or Shift and the arrows, copy and paste, scrolling. The
//! editor, the notes and the reminders are built on it.

use super::gfx::{Canvas, Rect};
use super::text::{self, Font};
use super::ui;
use crate::dev::input::{Key, KeyEvent};
use alloc::string::String;
use alloc::vec::Vec;

const PAD: i32 = 14;
const SELECTION: u32 = 0x660A_84FF;

pub struct TextArea {
    text: Vec<char>,
    pub cursor: usize,
    /// The other end of the selection, if there is one.
    anchor: Option<usize>,
    /// Scrolled by this many pixels.
    scroll: i32,
    pub font: Font,
    /// Edited since the last [`mark_saved`](Self::mark_saved).
    pub dirty: bool,
    pub placeholder: &'static str,
    /// Lines as char ranges, for the width they were laid out at.
    lines: Vec<(usize, usize)>,
    layout_w: i32,
    blink: bool,
    ticks: u32,
    /// Where the caret blinks (from the last drawing), if it shows.
    caret_at: Option<Rect>,
    /// Where the pointer pressed (to drag a selection).
    dragging: bool,
    /// The column the cursor tries to stay in when moving up and down.
    goal_x: Option<i32>,
    /// Source code: line numbers, four spaces for Tab, and Enter keeps the
    /// indentation (one level more after a line that opens a block).
    pub code: bool,
    /// A color for every character (syntax highlighting); ignored unless it
    /// is as long as the text.
    pub colors: Vec<u32>,
    /// Character ranges underlined with a red wave (errors).
    pub squiggles: Vec<(usize, usize)>,
    /// Counts the changes to the text (to notice one).
    pub edits: u64,
}

const GUTTER: i32 = 44;

/// Words that open a block in JustScript: the next line goes one level in.
const BLOCK_OPENERS: [&str; 22] = [
    "fn", "if", "else", "elif", "while", "for", "loop", "match", "struct", "enum", "impl", "view", "window", "column", "row", "card",
    "grid", "scroll", "group", "split", "accordion", "test",
];

impl TextArea {
    pub fn new(content: &str) -> TextArea {
        TextArea {
            text: content.chars().filter(|&c| c != '\r').collect(),
            cursor: 0,
            anchor: None,
            scroll: 0,
            font: Font::Body,
            dirty: false,
            placeholder: "",
            lines: Vec::new(),
            layout_w: -1,
            blink: true,
            ticks: 0,
            caret_at: None,
            dragging: false,
            goal_x: None,
            code: false,
            colors: Vec::new(),
            squiggles: Vec::new(),
            edits: 0,
        }
    }

    /// The character index of the cursor's line start and its line number
    /// (from 1), for a status line.
    pub fn cursor_line_col(&self) -> (usize, usize) {
        let before = &self.text[..self.cursor];
        let line = before.iter().filter(|&&c| c == '\n').count() + 1;
        let col = before.iter().rev().take_while(|&&c| c != '\n').count() + 1;
        (line, col)
    }

    /// Puts the cursor at the start of line `line` (from 1).
    pub fn go_to_line(&mut self, line: usize) {
        let mut n = 1;
        let mut pos = 0;
        for (i, &c) in self.text.iter().enumerate() {
            if n >= line {
                break;
            }
            if c == '\n' {
                n += 1;
                pos = i + 1;
            }
        }
        self.cursor = pos;
        self.anchor = None;
        self.goal_x = None;
        // Shown at the top third once drawn.
        self.scroll = ((line as i32 - 6) * self.line_h()).max(0);
    }

    /// What Enter inserts in code: the line break and the indentation.
    fn newline_indent(&self) -> String {
        let start = self.text[..self.cursor].iter().rposition(|&c| c == '\n').map_or(0, |i| i + 1);
        let line: String = self.text[start..self.cursor].iter().collect();
        let indent: String = line.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
        let first = line.trim_start().split(|c: char| !(c.is_alphanumeric() || c == '_')).next().unwrap_or("");
        let opens = BLOCK_OPENERS.contains(&first) && !line.trim_end().ends_with("->");
        let mut s = String::from("\n");
        s.push_str(&indent);
        if opens {
            s.push_str("    ");
        }
        s
    }

    pub fn text(&self) -> String {
        self.text.iter().collect()
    }

    pub fn set_text(&mut self, s: &str) {
        self.text = s.chars().filter(|&c| c != '\r').collect();
        self.cursor = 0;
        self.anchor = None;
        self.scroll = 0;
        self.layout_w = -1;
        self.dirty = false;
        self.edits += 1;
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn mark_saved(&mut self) {
        self.dirty = false;
    }

    fn line_h(&self) -> i32 {
        text::metrics(self.font).0 + 3
    }

    /// Breaks the text into lines that fit `w` pixels.
    fn layout(&mut self, w: i32) {
        if w == self.layout_w {
            return;
        }
        self.layout_w = w;
        let font = self.font;
        let text = &self.text;
        let mut lines = Vec::new();
        text::fonts(|f| {
            let mut start = 0;
            let mut x = 0;
            let mut last_space: Option<usize> = None;
            let mut i = 0;
            while i < text.len() {
                let c = text[i];
                if c == '\n' {
                    lines.push((start, i));
                    start = i + 1;
                    x = 0;
                    last_space = None;
                    i += 1;
                    continue;
                }
                let a = f.advance(font, if c == '\t' { ' ' } else { c }) * if c == '\t' { 4 } else { 1 };
                if x + a > w && i > start {
                    // Break after the last space, or here inside a long word.
                    let at = match last_space {
                        Some(s) if s > start => s + 1,
                        _ => i,
                    };
                    lines.push((start, at));
                    start = at;
                    x = text[start..i].iter().map(|&c| f.advance(font, c)).sum();
                    last_space = None;
                }
                if c == ' ' {
                    last_space = Some(i);
                }
                x += a;
                i += 1;
            }
            lines.push((start, text.len()));
        });
        self.lines = lines;
    }

    fn invalidate(&mut self) {
        self.layout_w = -1;
        self.dirty = true;
        self.edits += 1;
        self.blink = true;
        self.ticks = 0;
    }

    fn line_of(&self, pos: usize) -> usize {
        // The last line starting at or before pos.
        match self.lines.binary_search_by(|&(s, _)| s.cmp(&pos)) {
            Ok(i) => {
                // A wrapped line ends where the next starts: the cursor goes
                // to the start of the next line, except at the very end.
                let mut i = i;
                while i + 1 < self.lines.len() && self.lines[i + 1].0 == pos {
                    i += 1;
                }
                i
            }
            Err(i) => i.saturating_sub(1),
        }
    }

    fn x_of(&self, line: usize, pos: usize) -> i32 {
        let (s, e) = self.lines[line];
        let font = self.font;
        let text = &self.text;
        text::fonts(|f| text[s..pos.min(e)].iter().map(|&c| if c == '\t' { 4 * f.advance(font, ' ') } else { f.advance(font, c) }).sum())
    }

    /// The position nearest `x` pixels into line `line`.
    fn pos_at(&self, line: usize, x: i32) -> usize {
        let (s, e) = self.lines[line];
        let font = self.font;
        let text = &self.text;
        text::fonts(|f| {
            let mut pen = 0;
            for (i, &c) in text[s..e].iter().enumerate() {
                let a = if c == '\t' { 4 * f.advance(font, ' ') } else { f.advance(font, c) };
                if x < pen + a / 2 {
                    return s + i;
                }
                pen += a;
            }
            // A wrapped line: stay before the break, not after it.
            if e < text.len() && text[e] != '\n' && e > s && text[e - 1] == ' ' {
                e - 1
            } else {
                e
            }
        })
    }

    fn selection(&self) -> Option<(usize, usize)> {
        let a = self.anchor?;
        if a == self.cursor {
            return None;
        }
        Some((a.min(self.cursor), a.max(self.cursor)))
    }

    pub fn selected_text(&self) -> String {
        self.selection().map(|(a, b)| self.text[a..b].iter().collect()).unwrap_or_default()
    }

    fn delete_selection(&mut self) -> bool {
        if let Some((a, b)) = self.selection() {
            self.text.drain(a..b);
            self.cursor = a;
            self.anchor = None;
            self.invalidate();
            true
        } else {
            self.anchor = None;
            false
        }
    }

    pub fn insert(&mut self, s: &str) {
        self.delete_selection();
        let chars: Vec<char> = s.chars().filter(|&c| c != '\r').collect();
        let n = chars.len();
        self.text.splice(self.cursor..self.cursor, chars);
        self.cursor += n;
        self.invalidate();
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.cursor = self.text.len();
    }

    fn inner(&self, r: Rect) -> Rect {
        let g = if self.code { GUTTER } else { 0 };
        Rect::new(r.x + PAD + g, r.y + PAD - 4, r.w - 2 * PAD - g, r.h - 2 * PAD + 8)
    }

    /// Keeps the cursor inside the visible part.
    fn follow(&mut self, view_h: i32) {
        let line = self.line_of(self.cursor) as i32;
        let lh = self.line_h();
        let top = line * lh;
        if top < self.scroll {
            self.scroll = top;
        } else if top + lh > self.scroll + view_h {
            self.scroll = top + lh - view_h;
        }
    }

    pub fn draw(&mut self, c: &mut Canvas, r: Rect, focused: bool) {
        let inner = self.inner(r);
        self.layout(inner.w);
        let lh = self.line_h();
        let max_scroll = (self.lines.len() as i32 * lh - inner.h).max(0);
        self.scroll = self.scroll.clamp(0, max_scroll);
        let saved = c.clip();
        c.set_clip(saved.intersect(&r));
        if self.text.is_empty() && !self.placeholder.is_empty() {
            text::draw(c, self.font, inner.x, inner.y, self.placeholder, ui::text_faint());
        }
        let sel = self.selection();
        let first = (self.scroll / lh) as usize;
        let font = self.font;
        let colored = self.code && self.colors.len() == self.text.len();
        // The number of the logical line the first visible row belongs to.
        let mut number = match self.lines.get(first) {
            Some(&(s, _)) if self.code => self.text[..s].iter().filter(|&&c| c == '\n').count() + 1,
            _ => 1,
        };
        for (li, &(s, e)) in self.lines.iter().enumerate().skip(first) {
            let y = inner.y + li as i32 * lh - self.scroll;
            if y > r.bottom() {
                break;
            }
            if self.code {
                if s == 0 || self.text[s - 1] == '\n' {
                    let here = (s..=e).contains(&self.cursor);
                    let label = alloc::format!("{number}");
                    text::draw_right(c, font, inner.x - 12, y + 1, &label, if here { ui::text() } else { ui::text_faint() });
                }
                if e < self.text.len() && self.text[e] == '\n' {
                    number += 1;
                }
                for &(a, b) in &self.squiggles {
                    if a < e.max(s + 1) && b > s {
                        let x0 = inner.x + self.x_of(li, a.max(s));
                        let x1 = (inner.x + self.x_of(li, b.min(e))).max(x0 + 8);
                        let base = y + lh - 2;
                        let mut x = x0;
                        while x < x1 {
                            let up = (x - x0) / 3 % 2 == 0;
                            c.line(x, if up { base } else { base - 2 }, x + 3, if up { base - 2 } else { base }, 1, 0xFFFF_5A5F);
                            x += 3;
                        }
                    }
                }
            }
            if let Some((a, b)) = sel {
                if a < e.max(s + 1) && b > s {
                    let x0 = inner.x + self.x_of(li, a.max(s));
                    let mut x1 = inner.x + self.x_of(li, b.min(e));
                    if b > e {
                        x1 += 6; // the line break is selected too
                    }
                    c.fill(Rect::new(x0, y, (x1 - x0).max(2), lh), SELECTION);
                }
            }
            let line: String = self.text[s..e].iter().map(|&ch| if ch == '\t' { ' ' } else { ch }).collect();
            // Tabs are four spaces wide.
            let mut x = inner.x;
            if colored {
                // Runs of one color.
                let mut i = s;
                while i < e {
                    let color = self.colors[i];
                    let mut j = i;
                    let mut run = String::new();
                    while j < e && self.colors[j] == color {
                        match self.text[j] {
                            '\t' => run.push_str("    "),
                            ch => run.push(ch),
                        }
                        j += 1;
                    }
                    x += text::draw(c, font, x, y + 1, &run, color);
                    i = j;
                }
            } else if self.text[s..e].contains(&'\t') {
                for &ch in &self.text[s..e] {
                    if ch == '\t' {
                        x += 4 * text::advance(font, ' ');
                    } else {
                        let mut b = [0u8; 4];
                        x += text::draw(c, font, x, y + 1, ch.encode_utf8(&mut b), ui::text());
                    }
                }
            } else {
                text::draw(c, font, x, y + 1, &line, ui::text());
            }
        }
        self.caret_at = None;
        if focused && sel.is_none() {
            let li = self.line_of(self.cursor);
            let x = inner.x + self.x_of(li, self.cursor);
            let y = inner.y + li as i32 * lh - self.scroll;
            let caret = Rect::new(x, y + 1, 2, lh - 2);
            self.caret_at = Some(caret.intersect(&inner));
            if self.blink {
                c.fill(caret, ui::ACCENT);
            }
        }
        // A thin scroll bar when the text is taller than the area.
        if max_scroll > 0 {
            let total = self.lines.len() as i32 * lh;
            let bh = (inner.h * inner.h / total).max(24);
            let by = r.y + 4 + (r.h - 8 - bh) * self.scroll / max_scroll;
            c.fill_round(Rect::new(r.right() - 7, by, 4, bh), 2, 0x66FF_FFFF);
        }
        c.set_clip(saved);
    }

    fn pos_from_point(&mut self, r: Rect, x: i32, y: i32) -> usize {
        let inner = self.inner(r);
        self.layout(inner.w);
        let lh = self.line_h();
        let line = ((y - inner.y + self.scroll) / lh).clamp(0, self.lines.len() as i32 - 1) as usize;
        self.pos_at(line, x - inner.x)
    }

    /// A click at (`x`, `y`) on the screen; `r` is the area's rectangle.
    pub fn press(&mut self, r: Rect, x: i32, y: i32, double: bool, shift: bool) {
        let p = self.pos_from_point(r, x, y);
        if double {
            // Select the word.
            let word = |c: char| c.is_alphanumeric() || c == '_';
            let mut a = p;
            while a > 0 && word(self.text[a - 1]) {
                a -= 1;
            }
            let mut b = p;
            while b < self.text.len() && word(self.text[b]) {
                b += 1;
            }
            self.anchor = Some(a);
            self.cursor = b;
        } else {
            if shift {
                self.anchor.get_or_insert(self.cursor);
            } else {
                self.anchor = Some(p);
            }
            self.cursor = p;
            self.dragging = true;
        }
        self.goal_x = None;
        self.blink = true;
        self.ticks = 0;
    }

    pub fn drag(&mut self, r: Rect, x: i32, y: i32) -> bool {
        if !self.dragging {
            return false;
        }
        let p = self.pos_from_point(r, x, y);
        let moved = p != self.cursor;
        self.cursor = p;
        if moved {
            let inner = self.inner(r);
            self.follow(inner.h);
        }
        moved
    }

    pub fn release(&mut self) {
        self.dragging = false;
    }

    pub fn scroll_by(&mut self, lines: i32) -> bool {
        let old = self.scroll;
        self.scroll = (self.scroll + lines * self.line_h() * 2).max(0);
        // The upper limit is applied when drawing.
        let max = (self.lines.len() as i32 * self.line_h()).max(0);
        self.scroll = self.scroll.min(max);
        old != self.scroll
    }

    /// Handles a key; returns whether the area needs drawing again. `r` is
    /// the area's rectangle (for moving by pages).
    pub fn key(&mut self, ev: KeyEvent, r: Rect) -> bool {
        let inner = self.inner(r);
        self.layout(inner.w);
        let lh = self.line_h();
        let shift = ev.shift;
        let moving = matches!(ev.key, Key::Left | Key::Right | Key::Up | Key::Down | Key::Home | Key::End | Key::PageUp | Key::PageDown);
        if moving {
            if shift {
                self.anchor.get_or_insert(self.cursor);
            } else if let Some((a, b)) = self.selection() {
                // An arrow on a selection goes to its edge.
                self.cursor = if matches!(ev.key, Key::Left | Key::Up) { a } else { b };
                self.anchor = None;
                self.follow(inner.h);
                return true;
            } else {
                self.anchor = None;
            }
        }
        if !matches!(ev.key, Key::Up | Key::Down | Key::PageUp | Key::PageDown) {
            self.goal_x = None;
        }
        let vertical = |s: &mut TextArea, rows: i32| {
            let li = s.line_of(s.cursor) as i32;
            let gx = match s.goal_x {
                Some(g) => g,
                None => {
                    let g = s.x_of(li as usize, s.cursor);
                    s.goal_x = Some(g);
                    g
                }
            };
            let target = li + rows;
            if target < 0 {
                s.cursor = 0;
            } else if target >= s.lines.len() as i32 {
                s.cursor = s.text.len();
            } else {
                s.cursor = s.pos_at(target as usize, gx);
            }
        };
        match ev.key {
            Key::Char(ch) if ev.ctrl => match ch.to_ascii_lowercase() {
                'a' => self.select_all(),
                'c' => {
                    let s = self.selected_text();
                    if !s.is_empty() {
                        ui::copy(&s);
                    }
                    return false;
                }
                'x' => {
                    let s = self.selected_text();
                    if !s.is_empty() {
                        ui::copy(&s);
                        self.delete_selection();
                    }
                }
                'v' => {
                    let s = ui::paste();
                    if !s.is_empty() {
                        self.insert(&s);
                    }
                }
                _ => return false,
            },
            Key::Char(ch) if !ch.is_control() => {
                let mut b = [0u8; 4];
                self.insert(ch.encode_utf8(&mut b));
            }
            Key::Enter if self.code => {
                let s = self.newline_indent();
                self.insert(&s);
            }
            Key::Enter => self.insert("\n"),
            Key::Tab if self.code => self.insert("    "),
            Key::Tab => self.insert("\t"),
            Key::Backspace => {
                if !self.delete_selection() && self.cursor > 0 {
                    self.cursor -= 1;
                    self.text.remove(self.cursor);
                    self.invalidate();
                }
            }
            Key::Delete => {
                if !self.delete_selection() && self.cursor < self.text.len() {
                    self.text.remove(self.cursor);
                    self.invalidate();
                }
            }
            Key::Left => {
                if ev.ctrl {
                    while self.cursor > 0 && !self.text[self.cursor - 1].is_alphanumeric() {
                        self.cursor -= 1;
                    }
                    while self.cursor > 0 && self.text[self.cursor - 1].is_alphanumeric() {
                        self.cursor -= 1;
                    }
                } else {
                    self.cursor = self.cursor.saturating_sub(1);
                }
            }
            Key::Right => {
                let n = self.text.len();
                if ev.ctrl {
                    while self.cursor < n && !self.text[self.cursor].is_alphanumeric() {
                        self.cursor += 1;
                    }
                    while self.cursor < n && self.text[self.cursor].is_alphanumeric() {
                        self.cursor += 1;
                    }
                } else {
                    self.cursor = (self.cursor + 1).min(n);
                }
            }
            Key::Up => vertical(self, -1),
            Key::Down => vertical(self, 1),
            Key::PageUp => vertical(self, -(inner.h / lh).max(1)),
            Key::PageDown => vertical(self, (inner.h / lh).max(1)),
            Key::Home => {
                if ev.ctrl {
                    self.cursor = 0;
                } else {
                    self.cursor = self.lines[self.line_of(self.cursor)].0;
                }
            }
            Key::End => {
                if ev.ctrl {
                    self.cursor = self.text.len();
                } else {
                    let li = self.line_of(self.cursor);
                    self.cursor = self.pos_at(li, i32::MAX);
                }
            }
            _ => return false,
        }
        if !moving && !shift {
            if let Some(a) = self.anchor {
                if a == self.cursor {
                    self.anchor = None;
                }
            }
        }
        self.layout(inner.w);
        self.follow(inner.h);
        self.blink = true;
        self.ticks = 0;
        true
    }

    /// What a blink of the cursor changes on the screen (for
    /// [`App::changed`](super::wm::App::changed)): nothing when it does not
    /// show.
    pub fn caret(&self) -> Rect {
        self.caret_at.map_or(Rect::EMPTY, |r| r.grow(1))
    }

    /// The cursor blinks; true when it changed.
    pub fn tick(&mut self) -> bool {
        self.ticks += 1;
        if self.ticks.is_multiple_of(5) {
            self.blink = !self.blink;
            return true;
        }
        false
    }

    /// Words and characters, for a status line.
    pub fn counts(&self) -> (usize, usize) {
        let mut words = 0;
        let mut in_word = false;
        for &c in &self.text {
            if c.is_whitespace() {
                in_word = false;
            } else if !in_word {
                in_word = true;
                words += 1;
            }
        }
        (words, self.text.len())
    }
}

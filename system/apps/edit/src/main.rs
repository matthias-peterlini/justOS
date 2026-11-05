//! A full-screen text editor.
//!
//! Arrows, Home/End, Page Up/Down move; Ctrl+S saves; Ctrl+Q quits (twice
//! if there are unsaved changes); Tab inserts four spaces.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use justrt::console::{self, color::*, Key};

justrt::main!(main);

struct Editor {
    path: String,
    lines: Vec<Vec<char>>,
    cx: usize,
    cy: usize,
    top: usize,
    left: usize,
    cols: usize,
    rows: usize,
    dirty: bool,
    message: String,
    quit_armed: bool,
}

impl Editor {
    fn text_rows(&self) -> usize {
        self.rows.saturating_sub(2).max(1)
    }

    fn load(&mut self) {
        match justrt::fs::read_to_string(&self.path) {
            Ok(text) => {
                self.lines = text.split('\n').map(|l| l.trim_end_matches('\r').chars().collect()).collect();
                if self.lines.len() > 1 && self.lines.last().is_some_and(|l| l.is_empty()) {
                    self.lines.pop();
                }
                self.message = alloc::format!("{} lines", self.lines.len());
            }
            Err(justrt::Error::NotFound) => self.message = String::from("new file"),
            Err(e) => self.message = alloc::format!("cannot read: {}", e.as_str()),
        }
        if self.lines.is_empty() {
            self.lines.push(Vec::new());
        }
    }

    fn save(&mut self) {
        let mut text = String::new();
        for l in &self.lines {
            text.extend(l.iter());
            text.push('\n');
        }
        match justrt::fs::write(&self.path, text.as_bytes()) {
            Ok(()) => {
                self.dirty = false;
                self.message = alloc::format!("saved, {} bytes", text.len());
            }
            Err(e) => self.message = alloc::format!("cannot save: {}", e.as_str()),
        }
    }

    fn scroll(&mut self) {
        let rows = self.text_rows();
        if self.cy < self.top {
            self.top = self.cy;
        }
        if self.cy >= self.top + rows {
            self.top = self.cy + 1 - rows;
        }
        let width = self.cols.saturating_sub(6).max(1);
        if self.cx < self.left {
            self.left = self.cx;
        }
        if self.cx >= self.left + width {
            self.left = self.cx + 1 - width;
        }
    }

    fn render(&mut self) {
        self.scroll();
        let mut out = String::from("\x1b[H");
        // Title bar.
        let title = alloc::format!(" edit · {}{}", self.path, if self.dirty { " •" } else { "" });
        out.push_str(INVERSE);
        out.extend(title.chars().take(self.cols));
        for _ in title.chars().count()..self.cols {
            out.push(' ');
        }
        out.push_str(RESET);
        let width = self.cols.saturating_sub(6);
        for r in 0..self.text_rows() {
            let y = self.top + r;
            out.push_str(&alloc::format!("\x1b[{};1H", r + 2));
            if y < self.lines.len() {
                out.push_str(&alloc::format!("{DIM}{:>4}{RESET}  ", y + 1));
                let line = &self.lines[y];
                for x in self.left..(self.left + width).min(line.len() + 1) {
                    let c = line.get(x).copied().unwrap_or(' ');
                    if y == self.cy && x == self.cx {
                        out.push_str(INVERSE);
                        out.push(c);
                        out.push_str(RESET);
                    } else if x < line.len() {
                        out.push(c);
                    }
                }
            } else {
                out.push_str(&alloc::format!("{DIM}   ~{RESET}"));
            }
            out.push_str("\x1b[K");
        }
        // Status bar.
        out.push_str(&alloc::format!("\x1b[{};1H", self.rows));
        let status = alloc::format!(" line {}, column {}  {}", self.cy + 1, self.cx + 1, self.message);
        let keys = "Ctrl+S save  Ctrl+Q quit ";
        out.push_str(DIM);
        out.extend(status.chars().take(self.cols.saturating_sub(keys.len())));
        let used = status.chars().count().min(self.cols.saturating_sub(keys.len()));
        for _ in used..self.cols.saturating_sub(keys.len()) {
            out.push(' ');
        }
        out.push_str(keys);
        out.push_str(RESET);
        console::write(&out);
    }

    fn clamp_x(&mut self) {
        self.cx = self.cx.min(self.lines[self.cy].len());
    }

    fn edit(&mut self) {
        self.dirty = true;
        self.quit_armed = false;
    }

    /// Returns false to quit.
    fn key(&mut self, ev: console::KeyEvent) -> bool {
        self.message.clear();
        let page = self.text_rows();
        match ev.key {
            Key::Char('q') if ev.ctrl => {
                if self.dirty && !self.quit_armed {
                    self.quit_armed = true;
                    self.message = String::from("unsaved changes: Ctrl+Q again to quit without saving");
                    return true;
                }
                return false;
            }
            Key::Char('s') if ev.ctrl => self.save(),
            Key::Char(c) if !ev.ctrl => {
                self.lines[self.cy].insert(self.cx, c);
                self.cx += 1;
                self.edit();
            }
            Key::Tab => {
                for _ in 0..4 {
                    self.lines[self.cy].insert(self.cx, ' ');
                    self.cx += 1;
                }
                self.edit();
            }
            Key::Enter => {
                let rest = self.lines[self.cy].split_off(self.cx);
                self.lines.insert(self.cy + 1, rest);
                self.cy += 1;
                self.cx = 0;
                self.edit();
            }
            Key::Backspace => {
                if self.cx > 0 {
                    self.cx -= 1;
                    self.lines[self.cy].remove(self.cx);
                    self.edit();
                } else if self.cy > 0 {
                    let line = self.lines.remove(self.cy);
                    self.cy -= 1;
                    self.cx = self.lines[self.cy].len();
                    self.lines[self.cy].extend(line);
                    self.edit();
                }
            }
            Key::Delete => {
                if self.cx < self.lines[self.cy].len() {
                    self.lines[self.cy].remove(self.cx);
                    self.edit();
                } else if self.cy + 1 < self.lines.len() {
                    let line = self.lines.remove(self.cy + 1);
                    self.lines[self.cy].extend(line);
                    self.edit();
                }
            }
            Key::Left => {
                if self.cx > 0 {
                    self.cx -= 1;
                } else if self.cy > 0 {
                    self.cy -= 1;
                    self.cx = self.lines[self.cy].len();
                }
            }
            Key::Right => {
                if self.cx < self.lines[self.cy].len() {
                    self.cx += 1;
                } else if self.cy + 1 < self.lines.len() {
                    self.cy += 1;
                    self.cx = 0;
                }
            }
            Key::Up => {
                self.cy = self.cy.saturating_sub(1);
                self.clamp_x();
            }
            Key::Down => {
                self.cy = (self.cy + 1).min(self.lines.len() - 1);
                self.clamp_x();
            }
            Key::PageUp => {
                self.cy = self.cy.saturating_sub(page);
                self.clamp_x();
            }
            Key::PageDown => {
                self.cy = (self.cy + page).min(self.lines.len() - 1);
                self.clamp_x();
            }
            Key::Home => self.cx = 0,
            Key::End => self.cx = self.lines[self.cy].len(),
            _ => {}
        }
        true
    }
}

fn main(args: &str) -> i32 {
    let path = args.trim();
    if path.is_empty() {
        justrt::eprintln!("usage: edit <file>");
        return 2;
    }
    let (cols, rows) = console::size();
    if cols < 20 || rows < 5 {
        justrt::eprintln!("edit needs a screen of at least 20x5");
        return 1;
    }
    let mut ed = Editor {
        path: String::from(path),
        lines: vec![],
        cx: 0,
        cy: 0,
        top: 0,
        left: 0,
        cols,
        rows,
        dirty: false,
        message: String::new(),
        quit_armed: false,
    };
    ed.load();
    console::clear();
    loop {
        ed.render();
        if !ed.key(console::read_key()) {
            break;
        }
    }
    console::clear();
    0
}

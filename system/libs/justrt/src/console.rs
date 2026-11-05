//! The console: text out, keys in, screen control with ANSI sequences.

use crate::sys;
use alloc::string::String;
use core::fmt::{self, Write};

struct Out;

impl Write for Out {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        sys::call(sys::WRITE, sys::CONSOLE, s.as_ptr() as u64, s.len() as u64, 0, 0);
        Ok(())
    }
}

#[doc(hidden)]
pub fn print_fmt(args: fmt::Arguments<'_>) {
    let _ = Out.write_fmt(args);
}

pub fn write(s: &str) {
    let _ = Out.write_str(s);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Backspace,
    Delete,
    Tab,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    F(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    pub key: Key,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

/// A key from its code (as `READ_KEY` and window events give it).
pub fn decode(v: u64) -> Option<KeyEvent> {
    let code = v as u32;
    let key = match code {
        0 => return None,
        sys::KEY_ENTER => Key::Enter,
        sys::KEY_BACKSPACE => Key::Backspace,
        sys::KEY_DELETE => Key::Delete,
        sys::KEY_TAB => Key::Tab,
        sys::KEY_ESCAPE => Key::Escape,
        sys::KEY_UP => Key::Up,
        sys::KEY_DOWN => Key::Down,
        sys::KEY_LEFT => Key::Left,
        sys::KEY_RIGHT => Key::Right,
        sys::KEY_HOME => Key::Home,
        sys::KEY_END => Key::End,
        sys::KEY_PAGE_UP => Key::PageUp,
        sys::KEY_PAGE_DOWN => Key::PageDown,
        sys::KEY_INSERT => Key::Insert,
        c if c > sys::KEY_F && c <= sys::KEY_F + 12 => Key::F((c - sys::KEY_F) as u8),
        c => Key::Char(char::from_u32(c)?),
    };
    Some(KeyEvent { key, ctrl: v & sys::MOD_CTRL != 0, alt: v & sys::MOD_ALT != 0, shift: v & sys::MOD_SHIFT != 0 })
}

/// Waits for a key.
pub fn read_key() -> KeyEvent {
    loop {
        if let Some(k) = decode(sys::call(sys::READ_KEY, u64::MAX, 0, 0, 0, 0) as u64) {
            return k;
        }
    }
}

/// A key if one arrives within `ms` milliseconds.
pub fn poll_key(ms: u64) -> Option<KeyEvent> {
    decode(sys::call(sys::READ_KEY, ms, 0, 0, 0, 0) as u64)
}

/// Reads a line with simple editing (Backspace). `None` on Escape.
pub fn read_line() -> Option<String> {
    let mut line = String::new();
    loop {
        let k = read_key();
        match k.key {
            Key::Enter => {
                write("\n");
                return Some(line);
            }
            Key::Escape => {
                write("\n");
                return None;
            }
            Key::Backspace => {
                if line.pop().is_some() {
                    write("\x08 \x08");
                }
            }
            Key::Char(c) if !k.ctrl => {
                line.push(c);
                let mut buf = [0u8; 4];
                write(c.encode_utf8(&mut buf));
            }
            _ => {}
        }
    }
}

/// (columns, rows) of the console.
pub fn size() -> (usize, usize) {
    let v = sys::call(sys::SCREEN, 0, 0, 0, 0, 0) as u64;
    ((v >> 32) as usize, (v & 0xFFFF_FFFF) as usize)
}

pub fn clear() {
    write("\x1b[2J\x1b[H");
}

/// Moves the cursor (0-based column and row).
pub fn goto(col: usize, row: usize) {
    print_fmt(format_args!("\x1b[{};{}H", row + 1, col + 1));
}

pub mod color {
    pub const RESET: &str = "\x1b[0m";
    pub const DIM: &str = "\x1b[90m";
    pub const RED: &str = "\x1b[91m";
    pub const GREEN: &str = "\x1b[92m";
    pub const YELLOW: &str = "\x1b[93m";
    pub const BLUE: &str = "\x1b[94m";
    pub const ACCENT: &str = "\x1b[95m";
    pub const CYAN: &str = "\x1b[96m";
    pub const WHITE: &str = "\x1b[97m";
    pub const INVERSE: &str = "\x1b[30;107m";
}

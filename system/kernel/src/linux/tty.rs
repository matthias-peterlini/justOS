//! The console as a Linux terminal: cooked mode (lines, echo, backspace)
//! and raw mode (every key at once, arrows as escape sequences), switched
//! with `TCSETS` as full-screen programs do; window size from the console.

use super::{err, user_bytes};
use crate::dev::input::{self, Key, KeyEvent};
use crate::sync::IrqLock;
use crate::task;
use alloc::collections::VecDeque;
use alloc::vec::Vec;

const ICANON: u32 = 0o2;
const ECHO: u32 = 0o10;
const ICRNL: u32 = 0o400;

struct Term {
    /// struct termios as the kernel keeps it (36 bytes).
    termios: [u8; 36],
    /// Bytes typed and not read yet.
    pending: VecDeque<u8>,
    /// Keys taken from the keyboard to see whether any was typed.
    peeked: VecDeque<KeyEvent>,
}

static TERM: IrqLock<Option<Term>> = IrqLock::new(None);

fn default_termios() -> [u8; 36] {
    let mut t = [0u8; 36];
    t[0..4].copy_from_slice(&0x0500u32.to_le_bytes()); // ICRNL | IXON
    t[4..8].copy_from_slice(&0x0005u32.to_le_bytes()); // OPOST | ONLCR
    t[8..12].copy_from_slice(&0x00bfu32.to_le_bytes()); // B38400 | CS8 | CREAD
    t[12..16].copy_from_slice(&0x8a3bu32.to_le_bytes()); // ISIG ICANON ECHO ECHOE ECHOK IEXTEN ECHOCTL ECHOKE
    let cc = [3u8, 0x1c, 0x7f, 0x15, 4, 0, 1, 0, 0x11, 0x13, 0x1a, 0, 0x12, 0x0f, 0x17, 0x16, 0, 0, 0];
    t[17..36].copy_from_slice(&cc);
    t
}

fn with<T>(f: impl FnOnce(&mut Term) -> T) -> T {
    let mut g = TERM.lock();
    f(g.get_or_insert_with(|| Term { termios: default_termios(), pending: VecDeque::new(), peeked: VecDeque::new() }))
}

fn lflag() -> u32 {
    with(|t| u32::from_le_bytes(t.termios[12..16].try_into().unwrap()))
}

fn iflag() -> u32 {
    with(|t| u32::from_le_bytes(t.termios[0..4].try_into().unwrap()))
}

/// A program started: the terminal is back to its usual (cooked) state.
pub fn reset() {
    with(|t| {
        t.termios = default_termios();
        t.pending.clear();
        t.peeked.clear();
    });
}

/// The bytes a key sends, as a VT100-style terminal would.
fn key_bytes(ev: KeyEvent, raw: bool) -> Vec<u8> {
    let s: &[u8] = match ev.key {
        Key::Char(c) => {
            if ev.ctrl && c.is_ascii_alphabetic() {
                return alloc::vec![(c.to_ascii_lowercase() as u8) - b'a' + 1];
            }
            let mut b = [0u8; 4];
            let mut v = c.encode_utf8(&mut b).as_bytes().to_vec();
            if ev.alt {
                v.insert(0, 0x1b);
            }
            return v;
        }
        Key::Enter => {
            if raw && iflag() & ICRNL == 0 {
                b"\r"
            } else {
                b"\n"
            }
        }
        Key::Backspace => b"\x7f",
        Key::Tab => b"\t",
        Key::Escape => b"\x1b",
        Key::Up => b"\x1b[A",
        Key::Down => b"\x1b[B",
        Key::Right => b"\x1b[C",
        Key::Left => b"\x1b[D",
        Key::Home => b"\x1b[H",
        Key::End => b"\x1b[F",
        Key::PageUp => b"\x1b[5~",
        Key::PageDown => b"\x1b[6~",
        Key::Delete => b"\x1b[3~",
        Key::Insert => b"\x1b[2~",
        Key::F(n) => {
            return match n {
                1..=4 => alloc::vec![0x1b, b'O', b'P' + (n - 1)],
                5..=12 => {
                    let code = [15, 17, 18, 19, 20, 21, 23, 24][(n - 5) as usize];
                    alloc::format!("\x1b[{code}~").into_bytes()
                }
                _ => Vec::new(),
            };
        }
        // Taken by the interface, never typed.
        Key::PrintScreen | Key::Lock | Key::Overview | Key::PrevDesktop | Key::NextDesktop | Key::VolumeUp | Key::VolumeDown | Key::Mute => b"",
    };
    s.to_vec()
}

/// Moves typed keys into the pending bytes (raw mode only: in cooked mode
/// the line editor reads keys itself).
fn pump() {
    let raw = lflag() & ICANON == 0;
    if !raw {
        return;
    }
    while let Some(ev) = next_key() {
        let b = key_bytes(ev, true);
        with(|t| t.pending.extend(b));
    }
}

/// Whether the calling process may read the keyboard: background
/// processes wait, as a stopped job would, until they are in front again.
fn in_front() -> bool {
    crate::user::in_foreground(task::current_pid())
}

/// Waits while the calling process is in the background; an error when a
/// signal or a kill interrupts the wait.
fn wait_front() -> Result<(), i64> {
    while !in_front() {
        if let Some(e) = super::signal::interrupted() {
            return Err(e);
        }
        task::sleep_ms(20);
    }
    Ok(())
}

/// Bytes ready to read.
pub fn pending() -> usize {
    if !in_front() {
        return 0;
    }
    if lflag() & ICANON != 0 {
        // Cooked: a line is not complete until Enter, but poll() should
        // still wake when something was typed.
        return with(|t| t.pending.len()) + input_waiting() as usize;
    }
    pump();
    with(|t| t.pending.len())
}

fn input_waiting() -> bool {
    if with(|t| !t.peeked.is_empty()) {
        return true;
    }
    match input::try_read_key() {
        Some(ev) => {
            with(|t| t.peeked.push_back(ev));
            true
        }
        None => false,
    }
}

/// The next key: one peeked at before, else from the keyboard.
fn next_key() -> Option<KeyEvent> {
    with(|t| t.peeked.pop_front()).or_else(input::try_read_key)
}

pub fn read(d: &super::fd::Desc, buf: &mut [u8]) -> i64 {
    if !in_front() {
        if d.nonblocking() {
            return -err::EAGAIN;
        }
        if let Err(e) = wait_front() {
            return e;
        }
    }
    if lflag() & ICANON != 0 {
        return read_line(buf);
    }
    // Raw: VMIN / VTIME decide how long to wait.
    let (vmin, vtime) = with(|t| (t.termios[17 + 6], t.termios[17 + 5]));
    let deadline = if vtime > 0 { Some(crate::time::uptime_ms() + vtime as u64 * 100) } else { None };
    loop {
        if let Err(e) = wait_front() {
            return e;
        }
        pump();
        let n = with(|t| {
            let n = t.pending.len().min(buf.len());
            for (dst, src) in buf.iter_mut().zip(t.pending.drain(..n)) {
                *dst = src;
            }
            n
        });
        if n > 0 || vmin == 0 && vtime == 0 {
            return n as i64;
        }
        if d.nonblocking() {
            return -err::EAGAIN;
        }
        if deadline.is_some_and(|t| crate::time::uptime_ms() >= t) {
            return 0;
        }
        if let Some(e) = super::signal::interrupted() {
            return e;
        }
        task::sleep_ms(5);
    }
}

/// Cooked mode: a line with editing and echo.
fn read_line(buf: &mut [u8]) -> i64 {
    let echo = lflag() & ECHO != 0;
    // Bytes left from a longer line come first.
    let left = with(|t| {
        let n = t.pending.len().min(buf.len());
        for (dst, src) in buf.iter_mut().zip(t.pending.drain(..n)) {
            *dst = src;
        }
        n
    });
    if left > 0 {
        return left as i64;
    }
    let mut line: Vec<u8> = Vec::new();
    loop {
        if let Some(e) = super::signal::interrupted() {
            return e;
        }
        if let Err(e) = wait_front() {
            return e;
        }
        let Some(ev) = next_key() else {
            task::sleep_ms(5);
            continue;
        };
        match ev.key {
            Key::Char('d') | Key::Char('D') if ev.ctrl && line.is_empty() => return 0,
            Key::Char(c) if !ev.ctrl => {
                let mut b = [0u8; 4];
                let s = c.encode_utf8(&mut b);
                line.extend_from_slice(s.as_bytes());
                if echo {
                    crate::print!("{s}");
                }
            }
            Key::Enter => {
                line.push(b'\n');
                if echo {
                    crate::print!("\n");
                }
                break;
            }
            Key::Backspace if !line.is_empty() => {
                while line.pop().is_some_and(|b| b & 0xC0 == 0x80) {}
                if echo {
                    crate::print!("\x08 \x08");
                }
            }
            _ => {}
        }
    }
    let n = line.len().min(buf.len());
    buf[..n].copy_from_slice(&line[..n]);
    with(|t| t.pending.extend(&line[n..]));
    n as i64
}

pub fn write(data: &[u8]) {
    crate::print!("{}", alloc::string::String::from_utf8_lossy(data));
}

pub fn ioctl(req: u64, arg: u64) -> i64 {
    const TCGETS: u64 = 0x5401;
    const TCSETS: u64 = 0x5402;
    const TCSETSW: u64 = 0x5403;
    const TCSETSF: u64 = 0x5404;
    const TIOCGPGRP: u64 = 0x540F;
    const TIOCSPGRP: u64 = 0x5410;
    const TIOCGWINSZ: u64 = 0x5413;
    const TIOCSWINSZ: u64 = 0x5414;
    const TIOCSCTTY: u64 = 0x540E;
    match req {
        TCGETS => match user_bytes(arg, 36) {
            Ok(b) => {
                with(|t| b.copy_from_slice(&t.termios));
                0
            }
            Err(e) => e,
        },
        TCSETS | TCSETSW | TCSETSF => match user_bytes(arg, 36) {
            Ok(b) => {
                with(|t| {
                    t.termios.copy_from_slice(b);
                    if req == TCSETSF {
                        t.pending.clear();
                    }
                });
                0
            }
            Err(e) => e,
        },
        TIOCGWINSZ => {
            let (cols, rows) = crate::console::size();
            match user_bytes(arg, 8) {
                Ok(b) => {
                    b[0..2].copy_from_slice(&(rows as u16).to_le_bytes());
                    b[2..4].copy_from_slice(&(cols as u16).to_le_bytes());
                    b[4..8].fill(0);
                    0
                }
                Err(e) => e,
            }
        }
        TIOCSWINSZ | TIOCSCTTY | TIOCSPGRP => 0,
        TIOCGPGRP => match user_bytes(arg, 4) {
            Ok(b) => {
                b.copy_from_slice(&(task::current_pid() as i32).to_le_bytes());
                0
            }
            Err(e) => e,
        },
        _ => -err::ENOTTY,
    }
}

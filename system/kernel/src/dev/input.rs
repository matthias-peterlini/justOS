//! Input: keyboard and pointer events from drivers, characters from the
//! serial console, turned into a queue of [`Key`]s for the console.

use super::keymap::{self, Layout};
use crate::sync::IrqLock;
use crate::task::WaitQueue;
use alloc::collections::VecDeque;
use arcdk::{INPUT_BUTTON, INPUT_KEY, INPUT_REL, KEY_RELEASED};

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
    /// Print Screen: a picture of the screen.
    PrintScreen,
    /// The logo key with L: lock the computer.
    Lock,
    /// The logo key with Tab: all the windows and desktops at a glance.
    Overview,
    /// The logo key with Ctrl and Left or Right: the desktop beside.
    PrevDesktop,
    NextDesktop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    pub key: Key,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Pointer {
    pub x: i32,
    pub y: i32,
    pub buttons: u8,
    pub wheel: i32,
    /// The position is absolute (a tablet), not a sum of movements.
    pub absolute: bool,
}

struct State {
    layout: Layout,
    lshift: bool,
    rshift: bool,
    lctrl: bool,
    rctrl: bool,
    alt: bool,
    altgr: bool,
    /// The logo key (left or right).
    logo: bool,
    caps: bool,
    queue: VecDeque<KeyEvent>,
    /// While the graphical interface runs, keyboard keys go here; it passes
    /// them on (with [`inject`]) to the focused terminal.
    capture: bool,
    gui_queue: VecDeque<KeyEvent>,
    pointer: Pointer,
    pointer_seen: bool,
    /// Button presses and releases in order (button, down, x, y), so a quick
    /// click is not lost between two looks at the pointer.
    clicks: VecDeque<(u8, bool, i32, i32)>,
    serial: [u8; 8],
    serial_len: usize,
    /// The queue filled up with serial input: the console stopped reading
    /// the chip (the sender waits) until the reader takes keys.
    serial_held: bool,
}

static STATE: IrqLock<State> = IrqLock::new(State {
    layout: Layout::Us,
    lshift: false,
    rshift: false,
    lctrl: false,
    rctrl: false,
    alt: false,
    altgr: false,
    logo: false,
    caps: false,
    queue: VecDeque::new(),
    capture: false,
    gui_queue: VecDeque::new(),
    pointer: Pointer { x: 0, y: 0, buttons: 0, wheel: 0, absolute: false },
    pointer_seen: false,
    clicks: VecDeque::new(),
    serial: [0; 8],
    serial_len: 0,
    serial_held: false,
});

static WAITERS: WaitQueue = WaitQueue::new();

const QUEUE_MAX: usize = 256;

fn push(s: &mut State, ev: KeyEvent) {
    if s.queue.len() < QUEUE_MAX {
        s.queue.push_back(ev);
    }
}

/// The modifier keys held now: (ctrl, alt, shift).
pub fn mods() -> (bool, bool, bool) {
    let s = STATE.lock();
    (s.lctrl || s.rctrl, s.alt || s.altgr, s.lshift || s.rshift)
}

/// A key event from a driver (HID usage, press/release/repeat).
fn key(usage: u32, value: i32) {
    let pressed = value != KEY_RELEASED;
    let usage = usage as u8;
    {
        let mut s = STATE.lock();
        match usage {
            0xE0 => s.lctrl = pressed,
            0xE4 => s.rctrl = pressed,
            0xE1 => s.lshift = pressed,
            0xE5 => s.rshift = pressed,
            0xE2 => s.alt = pressed,
            0xE6 => s.altgr = pressed,
            0xE3 | 0xE7 => s.logo = pressed,
            0x39 if value == arcdk::KEY_PRESSED => s.caps = !s.caps,
            _ => {}
        }
        if !pressed || (0xE0..=0xE7).contains(&usage) {
            return;
        }
        let shift = s.lshift || s.rshift;
        let ctrl = s.lctrl || s.rctrl;
        let key = match usage {
            0x28 | 0x58 => Some(Key::Enter),
            0x29 => Some(Key::Escape),
            0x2A => Some(Key::Backspace),
            0x2B if s.logo => Some(Key::Overview),
            0x2B => Some(Key::Tab),
            0x50 if s.logo && ctrl => Some(Key::PrevDesktop),
            0x4F if s.logo && ctrl => Some(Key::NextDesktop),
            0x3A..=0x45 => Some(Key::F(usage - 0x3A + 1)),
            0x46 => Some(Key::PrintScreen),
            0x0F if s.logo => Some(Key::Lock), // L, wherever the layout puts it
            0x49 => Some(Key::Insert),
            0x4A => Some(Key::Home),
            0x4B => Some(Key::PageUp),
            0x4C => Some(Key::Delete),
            0x4D => Some(Key::End),
            0x4E => Some(Key::PageDown),
            0x4F => Some(Key::Right),
            0x50 => Some(Key::Left),
            0x51 => Some(Key::Down),
            0x52 => Some(Key::Up),
            _ => keymap::translate(s.layout, usage, shift, s.altgr, s.caps).map(Key::Char),
        };
        if s.capture {
            if let Some(key) = key {
                let ev = KeyEvent { key, ctrl, alt: s.alt, shift };
                if s.gui_queue.len() < QUEUE_MAX {
                    s.gui_queue.push_back(ev);
                }
            }
            return;
        }
        if key == Some(Key::Char('c')) && ctrl && crate::user::interrupt_foreground() {
            return; // Ctrl+C stops the running program
        }
        if let Some(key) = key {
            let ev = KeyEvent { key, ctrl, alt: s.alt, shift };
            push(&mut s, ev);
        }
    }
    WAITERS.notify_all();
}

/// An event reported by a driver through the ABI.
pub fn driver_event(_device: u32, kind: u32, code: u32, value: i32) {
    match kind {
        INPUT_KEY => key(code, value),
        INPUT_REL => {
            let mut s = STATE.lock();
            s.pointer_seen = true;
            match code {
                arcdk::AXIS_X => s.pointer.x = s.pointer.x.saturating_add(value),
                arcdk::AXIS_Y => s.pointer.y = s.pointer.y.saturating_add(value),
                arcdk::AXIS_WHEEL => s.pointer.wheel = s.pointer.wheel.saturating_add(value),
                _ => {}
            }
        }
        arcdk::INPUT_ABS => {
            // Absolute positions are scaled to the screen.
            let full = arcdk::ABS_MAX as i64 + 1;
            let (w, h) = crate::console::pixel_size().map(|(w, h)| (w as i64, h as i64)).unwrap_or((full, full));
            let v = value.clamp(0, arcdk::ABS_MAX) as i64;
            let mut s = STATE.lock();
            s.pointer_seen = true;
            s.pointer.absolute = true;
            match code {
                arcdk::AXIS_X => s.pointer.x = (v * w / full) as i32,
                arcdk::AXIS_Y => s.pointer.y = (v * h / full) as i32,
                _ => {}
            }
        }
        INPUT_BUTTON if code < 8 => {
            let mut s = STATE.lock();
            s.pointer_seen = true;
            if s.clicks.len() >= 32 {
                s.clicks.pop_front();
            }
            let (x, y) = (s.pointer.x, s.pointer.y);
            s.clicks.push_back((code as u8, value != 0, x, y));
            if value != 0 {
                s.pointer.buttons |= 1 << code;
            } else {
                s.pointer.buttons &= !(1 << code);
            }
        }
        _ => {}
    }
}

/// Room for another byte from the serial console. Text pasted there comes
/// faster than the shell takes it: rather than dropping keys, the console
/// stops listening (the byte stays in the chip, the sender waits) until the
/// reader has taken half the queue.
pub fn serial_room() -> bool {
    let mut s = STATE.lock();
    if s.queue.len() < QUEUE_MAX {
        return true;
    }
    s.serial_held = true;
    crate::console::serial::listen(false);
    false
}

fn pop(s: &mut State) -> Option<KeyEvent> {
    let ev = s.queue.pop_front();
    if s.serial_held && s.queue.len() <= QUEUE_MAX / 2 {
        s.serial_held = false;
        crate::console::serial::listen(true);
    }
    ev
}

/// A byte typed on the serial console (VT100 keys).
pub fn serial_byte(b: u8) {
    {
        let mut s = STATE.lock();
        let plain = |key| KeyEvent { key, ctrl: false, alt: false, shift: false };
        if s.serial_len > 0 || b == 0x1B {
            let n = s.serial_len;
            if n < s.serial.len() {
                s.serial[n] = b;
                s.serial_len += 1;
            }
            let seq = &s.serial[..s.serial_len];
            let key = match seq {
                [0x1B] | [0x1B, b'['] | [0x1B, b'[', b'0'..=b'9'] => return,
                [0x1B, b'[', b'A'] => Some(Key::Up),
                [0x1B, b'[', b'B'] => Some(Key::Down),
                [0x1B, b'[', b'C'] => Some(Key::Right),
                [0x1B, b'[', b'D'] => Some(Key::Left),
                [0x1B, b'[', b'H'] => Some(Key::Home),
                [0x1B, b'[', b'F'] => Some(Key::End),
                [0x1B, b'[', b'3', b'~'] => Some(Key::Delete),
                [0x1B, b'[', b'1', b'~'] => Some(Key::Home),
                [0x1B, b'[', b'4', b'~'] => Some(Key::End),
                [0x1B, b'[', b'5', b'~'] => Some(Key::PageUp),
                [0x1B, b'[', b'6', b'~'] => Some(Key::PageDown),
                [0x1B, x] if *x != b'[' => Some(Key::Escape),
                _ => None,
            };
            s.serial_len = 0;
            if let Some(k) = key {
                push(&mut s, plain(k));
            }
        } else {
            let ev = match b {
                b'\r' | b'\n' => plain(Key::Enter),
                0x7F | 0x08 => plain(Key::Backspace),
                b'\t' => plain(Key::Tab),
                0x03 if crate::user::interrupt_foreground() => return,
                0x01..=0x1A => KeyEvent { key: Key::Char((b'a' + b - 1) as char), ctrl: true, alt: false, shift: false },
                0x20..=0x7E => plain(Key::Char(b as char)),
                _ => return,
            };
            push(&mut s, ev);
        }
    }
    WAITERS.notify_all();
}

/// Waits for the next key.
pub fn read_key() -> KeyEvent {
    WAITERS.wait_for(|| pop(&mut STATE.lock()))
}

/// The next key if one is queued.
pub fn try_read_key() -> Option<KeyEvent> {
    pop(&mut STATE.lock())
}

pub fn layout() -> Layout {
    STATE.lock().layout
}

pub fn set_layout(layout: Layout) {
    STATE.lock().layout = layout;
}

/// Sends keyboard keys to the graphical interface instead of the terminal.
pub fn set_capture(on: bool) {
    STATE.lock().capture = on;
}

/// The next key for the graphical interface.
pub fn gui_key() -> Option<KeyEvent> {
    STATE.lock().gui_queue.pop_front()
}

/// Gives a key to the terminal (and the program in it), as if typed there.
pub fn inject(ev: KeyEvent) {
    if ev.key == Key::Char('c') && ev.ctrl && crate::user::interrupt_foreground() {
        return;
    }
    push(&mut STATE.lock(), ev);
    WAITERS.notify_all();
}

/// Where the pointing device has moved since boot, once it has reported.
/// The button presses and releases since the last call: (button, down,
/// x, y in the device's units, as [`Pointer`] gives them).
pub fn take_clicks() -> alloc::vec::Vec<(u8, bool, i32, i32)> {
    STATE.lock().clicks.drain(..).collect()
}

pub fn pointer() -> Option<Pointer> {
    let s = STATE.lock();
    s.pointer_seen.then_some(s.pointer)
}

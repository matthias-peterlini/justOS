//! Windows for justOS programs.
//!
//! A program opens a [`Window`] on the desktop, draws into its
//! [`canvas`](Window::canvas) with the same tools the system's apps use
//! (shapes, text in the system fonts, pictures: [`justgfx`]) and shows each
//! finished frame with [`present`](Window::present). Mouse and keyboard come
//! back as [`Event`]s.
//!
//! The canvas starts transparent: whatever is left transparent shows the
//! window's frosted glass, as in the built-in apps.
//!
//! Sizes and positions are in units. On a high-density screen the canvas
//! has two pixels per unit (and text comes from fonts drawn at twice the
//! size): the same drawing code gives a sharper window.
//!
//! ```ignore
//! let mut win = Window::new("Ciao", 400, 300)?;
//! loop {
//!     win.canvas.px.fill(0);
//!     win.text(Font::Title, 20, 20, "Ciao!", ui::TEXT);
//!     win.present();
//!     match win.wait() {
//!         Event::Close => break,
//!         _ => {}
//!     }
//! }
//! ```

#![no_std]

extern crate alloc;

pub mod ui;

pub use justgfx::{gfx, qoi, Canvas, Font, Fonts, Rect};
pub use justrt::console::{Key, KeyEvent};

use justdk::syscall as sys;
use justrt::sys::{call, check};

/// Something that happened to the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// The left button went down (`double`: the second click of a pair).
    Press { x: i32, y: i32, double: bool },
    /// The right button went down.
    Menu { x: i32, y: i32 },
    Release,
    /// The pointer moved (with the button `held` or not).
    Move { x: i32, y: i32, held: bool },
    Leave,
    Key(KeyEvent),
    /// The wheel: positive is down.
    Scroll(i32),
    /// The window got a new size (the canvas already has it).
    Resize { w: i32, h: i32 },
    /// The user closed the window: the program should end.
    Close,
    /// `count` files were dropped at x, y ([`Window::dropped`] says which).
    /// Only for windows made with [`DROPS`].
    Drop { x: i32, y: i32, count: usize },
}

/// [`Window::with_flags`]: the user may resize the window.
pub const RESIZABLE: u64 = sys::WIN_RESIZABLE;
/// [`Window::with_flags`]: files may be dropped on the window ([`Event::Drop`]).
pub const DROPS: u64 = sys::WIN_DROPS;

pub struct Window {
    handle: u64,
    /// What [`present`](Self::present) shows.
    pub canvas: Canvas,
    /// The system fonts.
    pub fonts: &'static Fonts,
}

static mut FONTS: Option<&'static Fonts> = None;
static mut ATLAS: Option<&'static [u8]> = None;

/// The system fonts (`sys:/system/fonts.bin`), read once.
pub fn fonts() -> justrt::Result<&'static Fonts> {
    // SAFETY: programs have a single thread.
    if let Some(f) = unsafe { FONTS } {
        return Ok(f);
    }
    let data = justrt::fs::read("sys:/system/fonts.bin")?;
    let data: &'static [u8] = alloc::boxed::Box::leak(data.into_boxed_slice());
    let f: &'static Fonts = alloc::boxed::Box::leak(alloc::boxed::Box::new(Fonts::parse(data).ok_or(justrt::Error::BadProgram)?));
    // SAFETY: as above.
    unsafe {
        ATLAS = Some(data);
        FONTS = Some(f);
    }
    Ok(f)
}

/// The system fonts for a canvas of `scale` pixels per unit: with the
/// faces drawn at twice the size (`sys:/system/fonts@2x.bin`) from 2 up.
pub fn fonts_for(scale: i32) -> justrt::Result<&'static Fonts> {
    let f = fonts()?;
    if scale < 2 || f.has_sharp() {
        return Ok(f);
    }
    // SAFETY: programs have a single thread; ATLAS was set by fonts().
    let Some(data) = (unsafe { ATLAS }) else { return Ok(f) };
    let Some(sharp) = justrt::fs::read("sys:/system/fonts@2x.bin").ok().and_then(|d| Fonts::parse(alloc::boxed::Box::leak(d.into_boxed_slice())))
    else {
        // Without them text is stretched from the normal faces.
        return Ok(f);
    };
    let mut both = Fonts::parse(data).ok_or(justrt::Error::BadProgram)?;
    both.set_sharp(sharp);
    let both: &'static Fonts = alloc::boxed::Box::leak(alloc::boxed::Box::new(both));
    // SAFETY: as above.
    unsafe { FONTS = Some(both) };
    Ok(both)
}

impl Window {
    /// A window with a content area of `w` x `h` pixels.
    pub fn new(title: &str, w: i32, h: i32) -> justrt::Result<Window> {
        Self::create(title, w, h, 0)
    }

    /// A window the user may make bigger or smaller ([`Event::Resize`]).
    pub fn resizable(title: &str, w: i32, h: i32) -> justrt::Result<Window> {
        Self::create(title, w, h, sys::WIN_RESIZABLE)
    }

    /// A window with some of [`RESIZABLE`] and [`DROPS`] (or-ed together).
    pub fn with_flags(title: &str, w: i32, h: i32, flags: u64) -> justrt::Result<Window> {
        Self::create(title, w, h, flags & (RESIZABLE | DROPS))
    }

    /// The files of the last [`Event::Drop`], as absolute paths. The program
    /// may read them even from its sandbox.
    pub fn dropped(&self) -> alloc::vec::Vec<alloc::string::String> {
        let mut buf = alloc::vec![0u8; 4096];
        loop {
            let n = call(sys::WIN_DROPPED, self.handle, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0);
            let Ok(n) = check(n) else { return alloc::vec::Vec::new() };
            let n = n as usize;
            if n > buf.len() {
                buf.resize(n, 0);
                continue;
            }
            let text = core::str::from_utf8(&buf[..n]).unwrap_or("");
            return text.lines().filter(|l| !l.is_empty()).map(alloc::string::String::from).collect();
        }
    }

    fn create(title: &str, w: i32, h: i32, flags: u64) -> justrt::Result<Window> {
        let size = (w as u64) << 32 | h as u64;
        let handle = check(call(sys::WIN_CREATE, title.as_ptr() as u64, title.len() as u64, size, flags, 0))?;
        // An older system has no WIN_SCALE: one pixel per unit.
        let scale = check(call(sys::WIN_SCALE, handle, 0, 0, 0, 0)).map_or(1, |s| s.clamp(1, 2) as i32);
        let fonts = fonts_for(scale)?;
        Ok(Window { handle, canvas: Canvas::new_layer_at(w, h, scale), fonts })
    }

    /// The width of the content in units.
    pub fn width(&self) -> i32 {
        self.canvas.w
    }

    pub fn height(&self) -> i32 {
        self.canvas.h
    }

    /// Pixels per unit of the canvas.
    pub fn scale(&self) -> i32 {
        self.canvas.scale()
    }

    /// Shows the canvas.
    pub fn present(&self) {
        let c = &self.canvas;
        let size = (c.pixel_width() as u64) << 32 | c.pixel_height() as u64;
        call(sys::WIN_PRESENT, self.handle, c.px.as_ptr() as u64, size, c.scale() as u64, 0);
    }

    pub fn set_title(&self, title: &str) {
        call(sys::WIN_TITLE, self.handle, title.as_ptr() as u64, title.len() as u64, 0, 0);
    }

    /// The next event, waiting up to `ms` milliseconds (0: only look).
    pub fn event(&mut self, ms: u64) -> Option<Event> {
        let mut raw = [0u64; 4];
        let got = call(sys::WIN_EVENT, self.handle, raw.as_mut_ptr() as u64, ms.max(1), 0, 0);
        if got != 1 {
            return None;
        }
        let ev = match raw[0] {
            sys::WIN_EV_PRESS if raw[3] == 2 => Event::Menu { x: raw[1] as i32, y: raw[2] as i32 },
            sys::WIN_EV_PRESS => Event::Press { x: raw[1] as i32, y: raw[2] as i32, double: raw[3] == 1 },
            sys::WIN_EV_RELEASE => Event::Release,
            sys::WIN_EV_MOVE => Event::Move { x: raw[1] as i32, y: raw[2] as i32, held: raw[3] != 0 },
            sys::WIN_EV_LEAVE => Event::Leave,
            sys::WIN_EV_KEY => Event::Key(justrt::console::decode(raw[1])?),
            sys::WIN_EV_SCROLL => Event::Scroll(raw[1] as i64 as i32),
            sys::WIN_EV_CLOSE => Event::Close,
            sys::WIN_EV_RESIZE => {
                let (w, h) = (raw[1] as i32, raw[2] as i32);
                let scale = (raw[3] as i32).clamp(1, 2);
                if w != self.canvas.w || h != self.canvas.h || scale != self.canvas.scale() {
                    self.canvas = Canvas::new_layer_at(w.max(1), h.max(1), scale);
                    if let Ok(f) = fonts_for(scale) {
                        self.fonts = f;
                    }
                }
                Event::Resize { w, h }
            }
            sys::WIN_EV_DROP => Event::Drop { x: raw[1] as i32, y: raw[2] as i32, count: raw[3] as usize },
            _ => return None,
        };
        Some(ev)
    }

    /// Waits for the next event.
    pub fn wait(&mut self) -> Event {
        loop {
            if let Some(e) = self.event(3_600_000) {
                return e;
            }
        }
    }

    /// Clears the canvas to transparent (the glass shows).
    pub fn clear(&mut self) {
        self.canvas.px.fill(0);
        self.canvas.reset_clip();
    }

    /// Draws text; returns its width.
    pub fn text(&mut self, font: Font, x: i32, y: i32, s: &str, color: u32) -> i32 {
        self.fonts.draw(&mut self.canvas, font, x, y, s, color)
    }

    pub fn text_centered(&mut self, font: Font, cx: i32, y: i32, s: &str, color: u32) {
        self.fonts.draw_centered(&mut self.canvas, font, cx, y, s, color)
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        call(sys::WIN_CLOSE, self.handle, 0, 0, 0, 0);
    }
}

/// A small random number generator (xorshift), seeded from the clock.
pub struct Rng(u64);

impl Rng {
    pub fn new() -> Rng {
        let seed = justrt::time::uptime_ms() ^ (justrt::time::now() << 20) ^ 0x9E37_79B9_7F4A_7C15;
        Rng(seed | 1)
    }

    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// A number in `0..n`.
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next() % n
        }
    }
}

impl Default for Rng {
    fn default() -> Rng {
        Rng::new()
    }
}

/// Milliseconds since the computer started.
pub fn now_ms() -> u64 {
    justrt::time::uptime_ms()
}

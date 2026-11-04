//! The system console: a small terminal (UTF-8, ANSI colours) drawn on the
//! framebuffer or the VGA text screen, mirrored to the serial port, with a
//! journal of everything printed since boot.

mod cp437;
pub mod fb;
mod font;
pub mod serial;
mod vga;

use crate::boot::{BootInfo, FB_RGB, FB_TEXT};
use crate::sync::IrqLock;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt::{self, Write};

/// Arc palette: the 16 ANSI colours, then the default text and background.
pub const PALETTE: [u32; 18] = [
    0x2A2633, 0xFF8A8A, 0x7EE2A8, 0xF6D06F, 0x8AB4F8, 0xC9A7FF, 0x7FDBE6, 0xD8D4E0, //
    0x6B6578, 0xFFB3B3, 0xA8F0C6, 0xFFE5A0, 0xB4D0FF, 0xE0CCFF, 0xA9EEF5, 0xFFFFFF, //
    0xF2EFF7, 0x16131D,
];
pub const DEFAULT_FG: u8 = 16;
pub const DEFAULT_BG: u8 = 17;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub glyph: u8,
    pub fg: u8,
    pub bg: u8,
}

const BLANK: Cell = Cell { glyph: b' ', fg: DEFAULT_FG, bg: DEFAULT_BG };

/// Something that shows a grid of character cells.
pub trait Surface: Send {
    fn cols(&self) -> usize;
    fn rows(&self) -> usize;
    fn draw(&mut self, col: usize, row: usize, cell: Cell, cursor: bool);
    fn scroll_up(&mut self, bg: u8);
    fn clear(&mut self, bg: u8);
    fn flush(&mut self);
}

enum Esc {
    None,
    Start,
    Csi { params: [u16; 4], count: usize },
}

// There is one screen: the size of the variants does not matter.
#[allow(clippy::large_enum_variant)]
enum Screen {
    Fb(fb::FbSurface),
    Vga(vga::VgaSurface),
    /// The graphical interface owns the screen: the terminal keeps its cells
    /// and the GUI's terminal window draws them.
    Headless(Headless),
}

impl Screen {
    fn get(&mut self) -> &mut dyn Surface {
        match self {
            Screen::Fb(s) => s,
            Screen::Vga(s) => s,
            Screen::Headless(s) => s,
        }
    }
}

struct Headless {
    cols: usize,
    rows: usize,
}

impl Surface for Headless {
    fn cols(&self) -> usize {
        self.cols
    }
    fn rows(&self) -> usize {
        self.rows
    }
    fn draw(&mut self, _: usize, _: usize, _: Cell, _: bool) {}
    fn scroll_up(&mut self, _: u8) {}
    fn clear(&mut self, _: u8) {}
    fn flush(&mut self) {}
}

/// The terminal size while the GUI owns the screen.
pub const HEADLESS_COLS: usize = 80;
pub const HEADLESS_ROWS: usize = 25;

/// The linear framebuffer, for the graphical interface.
#[derive(Clone, Copy, Debug)]
pub struct FbInfo {
    pub virt: u64,
    pub width: usize,
    pub height: usize,
    pub pitch: usize,
    /// Bit positions of red, green and blue in a pixel.
    pub shift: (u8, u8, u8),
}

static FB_INFO: IrqLock<Option<FbInfo>> = IrqLock::new(None);
/// The framebuffer the firmware set up (physical address and mapping), kept
/// for the generic display driver.
static BOOT_FB: IrqLock<Option<(u64, FbInfo)>> = IrqLock::new(None);
/// The text surface kept aside while the GUI runs, to show a fatal error.
static PANIC_FB: IrqLock<Option<fb::FbSurface>> = IrqLock::new(None);
/// Bumped whenever the terminal's cells or cursor change.
static GENERATION: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

fn touched() {
    GENERATION.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
}

struct Terminal {
    surface: Option<Screen>,
    cells: Vec<Cell>,
    cols: usize,
    rows: usize,
    col: usize,
    row: usize,
    fg: u8,
    bg: u8,
    bright: bool,
    esc: Esc,
    cursor: bool,
}

static TERM: IrqLock<Terminal> = IrqLock::new(Terminal {
    surface: None,
    cells: Vec::new(),
    cols: 0,
    rows: 0,
    col: 0,
    row: 0,
    fg: DEFAULT_FG,
    bg: DEFAULT_BG,
    bright: false,
    esc: Esc::None,
    cursor: false,
});

impl Terminal {
    fn cell(&mut self, col: usize, row: usize) -> &mut Cell {
        &mut self.cells[row * self.cols + col]
    }

    fn redraw_cell(&mut self, col: usize, row: usize, cursor: bool) {
        let cell = *self.cell(col, row);
        if let Some(s) = self.surface.as_mut() {
            s.get().draw(col, row, cell, cursor);
        }
    }

    fn hide_cursor(&mut self) {
        if self.cursor && self.col < self.cols && self.row < self.rows {
            self.redraw_cell(self.col, self.row, false);
        }
    }

    fn show_cursor(&mut self) {
        if self.cursor && self.col < self.cols && self.row < self.rows {
            self.redraw_cell(self.col, self.row, true);
        }
    }

    fn newline(&mut self) {
        self.col = 0;
        self.row += 1;
        if self.row >= self.rows {
            self.row = self.rows - 1;
            self.cells.copy_within(self.cols.., 0);
            let bg = self.bg;
            let start = (self.rows - 1) * self.cols;
            self.cells[start..].fill(Cell { bg, ..BLANK });
            if let Some(s) = self.surface.as_mut() {
                s.get().scroll_up(bg);
            }
        }
    }

    fn put_glyph(&mut self, glyph: u8) {
        if self.col >= self.cols {
            self.newline();
        }
        let fg = if self.bright && self.fg < 8 { self.fg + 8 } else { self.fg };
        let cell = Cell { glyph, fg, bg: self.bg };
        let (c, r) = (self.col, self.row);
        *self.cell(c, r) = cell;
        self.redraw_cell(c, r, false);
        self.col += 1;
    }

    fn clear(&mut self) {
        let bg = self.bg;
        self.cells.fill(Cell { bg, ..BLANK });
        if let Some(s) = self.surface.as_mut() {
            s.get().clear(bg);
        }
        self.col = 0;
        self.row = 0;
    }

    fn sgr(&mut self, params: &[u16]) {
        if params.is_empty() {
            self.fg = DEFAULT_FG;
            self.bg = DEFAULT_BG;
            self.bright = false;
        }
        for &p in params {
            match p {
                0 => {
                    self.fg = DEFAULT_FG;
                    self.bg = DEFAULT_BG;
                    self.bright = false;
                }
                1 => self.bright = true,
                2 => self.fg = 8,
                22 => self.bright = false,
                30..=37 => self.fg = (p - 30) as u8,
                39 => self.fg = DEFAULT_FG,
                40..=47 => self.bg = (p - 40) as u8,
                49 => self.bg = DEFAULT_BG,
                90..=97 => self.fg = (p - 90 + 8) as u8,
                100..=107 => self.bg = (p - 100 + 8) as u8,
                _ => {}
            }
        }
    }

    fn csi(&mut self, cmd: u8, params: &[u16]) {
        let n = |i: usize| params.get(i).copied().unwrap_or(0) as usize;
        match cmd {
            b'm' => self.sgr(params),
            b'J' if n(0) >= 2 => self.clear(),
            b'J' => {
                // 0: from the cursor to the end; 1: from the start to the cursor.
                let (c, r) = (self.col, self.row);
                for y in 0..self.rows {
                    for x in 0..self.cols {
                        let after = (y, x) >= (r, c);
                        if after == (n(0) == 0) {
                            *self.cell(x, y) = Cell { bg: self.bg, ..BLANK };
                            self.redraw_cell(x, y, false);
                        }
                    }
                }
            }
            b'K' => {
                let (c, r) = (self.col, self.row);
                let (from, to) = match n(0) {
                    1 => (0, c + 1),
                    2 => (0, self.cols),
                    _ => (c, self.cols),
                };
                for x in from..to.min(self.cols) {
                    *self.cell(x, r) = Cell { bg: self.bg, ..BLANK };
                    self.redraw_cell(x, r, false);
                }
            }
            b'H' | b'f' => {
                self.row = n(0).saturating_sub(1).min(self.rows - 1);
                self.col = n(1).saturating_sub(1).min(self.cols - 1);
            }
            b'A' => self.row = self.row.saturating_sub(n(0).max(1)),
            b'B' => self.row = (self.row + n(0).max(1)).min(self.rows - 1),
            b'D' => self.col = self.col.saturating_sub(n(0).max(1)),
            b'C' => self.col = (self.col + n(0).max(1)).min(self.cols - 1),
            b'G' => self.col = n(0).saturating_sub(1).min(self.cols - 1),
            b'd' => self.row = n(0).saturating_sub(1).min(self.rows - 1),
            _ => {}
        }
    }

    fn write_char(&mut self, c: char) {
        match core::mem::replace(&mut self.esc, Esc::None) {
            Esc::Start => {
                if c == '[' {
                    self.esc = Esc::Csi { params: [0; 4], count: 0 };
                }
                return;
            }
            Esc::Csi { mut params, mut count } => {
                match c {
                    '0'..='9' => {
                        if count == 0 {
                            count = 1;
                        }
                        let p = &mut params[(count - 1).min(3)];
                        *p = p.saturating_mul(10).saturating_add(c as u16 - b'0' as u16);
                        self.esc = Esc::Csi { params, count };
                    }
                    ';' => {
                        count = (count.max(1) + 1).min(4);
                        self.esc = Esc::Csi { params, count };
                    }
                    '\x40'..='\x7e' => self.csi(c as u8, &params[..count]),
                    // Private markers ("\x1b[?25l"): the sequence goes on.
                    '?' | '>' | '=' | ' ' => self.esc = Esc::Csi { params, count },
                    _ => {}
                }
                return;
            }
            Esc::None => {}
        }
        match c {
            '\x1b' => self.esc = Esc::Start,
            '\n' => self.newline(),
            '\r' => self.col = 0,
            '\t' => {
                let next = (self.col / 8 + 1) * 8;
                while self.col < next.min(self.cols) {
                    self.put_glyph(b' ');
                }
            }
            '\x08' => self.col = self.col.saturating_sub(1),
            c if (c as u32) < 0x20 => {}
            c => self.put_glyph(cp437::glyph(c)),
        }
    }

    fn write_str(&mut self, s: &str) {
        if self.surface.is_none() {
            return;
        }
        self.hide_cursor();
        for c in s.chars() {
            self.write_char(c);
        }
        self.show_cursor();
        self.flush();
        touched();
    }

    fn flush(&mut self) {
        if let Some(s) = self.surface.as_mut() {
            s.get().flush();
        }
    }
}

// ----------------------------------------------------------------------------
// Journal

const JOURNAL_SIZE: usize = 64 * 1024;

struct Journal {
    buf: [u8; JOURNAL_SIZE],
    len: usize,
    start: usize,
}

static JOURNAL: IrqLock<Journal> = IrqLock::new(Journal { buf: [0; JOURNAL_SIZE], len: 0, start: 0 });

impl Journal {
    fn push(&mut self, bytes: &[u8]) {
        for &b in bytes {
            let at = (self.start + self.len) % JOURNAL_SIZE;
            self.buf[at] = b;
            if self.len == JOURNAL_SIZE {
                self.start = (self.start + 1) % JOURNAL_SIZE;
            } else {
                self.len += 1;
            }
        }
    }
}

/// Everything logged since boot (oldest lines may have been dropped).
pub fn journal() -> Vec<u8> {
    let j = JOURNAL.lock();
    let mut out = vec![0u8; j.len];
    for (i, b) in out.iter_mut().enumerate() {
        *b = j.buf[(j.start + i) % JOURNAL_SIZE];
    }
    out
}

// ----------------------------------------------------------------------------
// Output

/// Where text goes: screen + serial; `journal` also records it.
fn emit(s: &str, journal: bool) {
    serial::write(s.as_bytes());
    if journal {
        JOURNAL.lock().push(s.as_bytes());
    }
    TERM.lock().write_str(s);
}

struct Out {
    journal: bool,
}

impl Write for Out {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        emit(s, self.journal);
        Ok(())
    }
}

#[doc(hidden)]
pub fn print_fmt(args: fmt::Arguments<'_>) {
    let _ = Out { journal: false }.write_fmt(args);
}

/// Prints to the console.
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => { $crate::console::print_fmt(format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($($arg:tt)*) => { $crate::console::print_fmt(format_args!("{}\n", format_args!($($arg)*))) };
}

pub const LEVEL_ERROR: u32 = 1;
pub const LEVEL_WARN: u32 = 2;
pub const LEVEL_INFO: u32 = 3;
pub const LEVEL_DEBUG: u32 = 4;

static VERBOSE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

pub fn set_verbose(on: bool) {
    VERBOSE.store(on, core::sync::atomic::Ordering::Relaxed);
}

pub fn verbose() -> bool {
    VERBOSE.load(core::sync::atomic::Ordering::Relaxed)
}

/// Logs a line: always to the journal, to the screen unless it is debug
/// output (shown with `verbose` on).
pub fn log(level: u32, source: &str, args: fmt::Arguments<'_>) {
    let (color, tag) = match level {
        LEVEL_ERROR => ("\x1b[91m", "error"),
        LEVEL_WARN => ("\x1b[93m", "warn "),
        LEVEL_INFO => ("\x1b[95m", "info "),
        _ => ("\x1b[90m", "debug"),
    };
    let ms = crate::time::uptime_ms();
    let line =
        alloc::format!("\x1b[90m[{:>5}.{:03}]\x1b[0m {}{}\x1b[0m \x1b[90m{}:\x1b[0m {}\n", ms / 1000, ms % 1000, color, tag, source, args);
    // With the graphical interface the terminal is a window for the shell:
    // log lines go to the journal (`log`) and the serial port only.
    let headless = matches!(TERM.lock().surface, Some(Screen::Headless(_)));
    let show = (level <= LEVEL_INFO || verbose()) && !headless;
    if show {
        emit(&line, true);
    } else {
        JOURNAL.lock().push(line.as_bytes());
        serial::write(line.as_bytes());
    }
}

#[macro_export]
macro_rules! info {
    ($src:expr, $($arg:tt)*) => { $crate::console::log($crate::console::LEVEL_INFO, $src, format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! warn {
    ($src:expr, $($arg:tt)*) => { $crate::console::log($crate::console::LEVEL_WARN, $src, format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! error {
    ($src:expr, $($arg:tt)*) => { $crate::console::log($crate::console::LEVEL_ERROR, $src, format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! debug {
    ($src:expr, $($arg:tt)*) => { $crate::console::log($crate::console::LEVEL_DEBUG, $src, format_args!($($arg)*)) };
}

/// Early output before the heap exists: serial only.
pub fn early(s: &str) {
    serial::write(s.as_bytes());
    JOURNAL.lock().push(s.as_bytes());
}

// ----------------------------------------------------------------------------
// Setup and control

/// Brings up the screen. Needs the heap.
pub fn init_screen(boot: &BootInfo) {
    let screen = match boot.fb_kind {
        FB_RGB if boot.fb_bpp == 32 => {
            let size = boot.fb_pitch as u64 * boot.fb_height as u64;
            crate::mem::region::map_device(boot.fb_addr, size, crate::mem::paging::WRITE_COMBINE).map(|virt| {
                // The firmware's font if the loader found one, else our own.
                let font = if boot.font_addr != 0 && boot.font_height == 16 {
                    // SAFETY: the loader copied the 4 KiB font there.
                    unsafe { &*(crate::mem::phys_to_virt(boot.font_addr) as *const [u8; 4096]) }
                } else {
                    &font::FONT
                };
                let shift = (boot.red_pos, boot.green_pos, boot.blue_pos);
                Screen::Fb(unsafe {
                    fb::FbSurface::new(
                        virt as *mut u8,
                        boot.fb_width as usize,
                        boot.fb_height as usize,
                        boot.fb_pitch as usize,
                        shift,
                        font,
                    )
                })
            })
        }
        // SAFETY: the VGA text buffer is at 0xB8000.
        FB_TEXT => Some(Screen::Vga(unsafe { vga::VgaSurface::new(crate::mem::phys_to_virt(0xB8000) as *mut u16) })),
        _ => None,
    };
    let Some(mut screen) = screen else { return };
    let (cols, rows) = (screen.get().cols(), screen.get().rows());
    let graphical = matches!(screen, Screen::Fb(_)) && boot.fb_width >= 800 && boot.fb_height >= 600;
    let mut t = TERM.lock();
    if graphical {
        // The GUI draws the screen; the terminal lives in its window. Room
        // for a full-screen grid is reserved now so that a fatal error can
        // take the screen back without allocating.
        if let Screen::Fb(fbs) = screen {
            let info = FbInfo {
                virt: fbs.base() as u64,
                width: boot.fb_width as usize,
                height: boot.fb_height as usize,
                pitch: boot.fb_pitch as usize,
                shift: (boot.red_pos, boot.green_pos, boot.blue_pos),
            };
            *FB_INFO.lock() = Some(info);
            *BOOT_FB.lock() = Some((boot.fb_addr, info));
            *PANIC_FB.lock() = Some(fbs);
        }
        t.cols = HEADLESS_COLS;
        t.rows = HEADLESS_ROWS;
        t.cells = Vec::with_capacity((cols * rows).max(HEADLESS_COLS * HEADLESS_ROWS));
        t.cells.resize(HEADLESS_COLS * HEADLESS_ROWS, BLANK);
        t.surface = Some(Screen::Headless(Headless { cols: HEADLESS_COLS, rows: HEADLESS_ROWS }));
        t.cursor = true;
        return;
    }
    t.cols = cols;
    t.rows = rows;
    t.cells = vec![BLANK; t.cols * t.rows];
    t.surface = Some(screen);
    t.clear();
    t.flush();
}

/// A display driver changed the screen mode: the interface and the screen
/// for fatal errors use the new framebuffer.
pub fn set_framebuffer(fb: FbInfo) {
    // SAFETY: the driver maps pitch * height bytes at fb.virt.
    let surface = unsafe { fb::FbSurface::new(fb.virt as *mut u8, fb.width, fb.height, fb.pitch, fb.shift, &font::FONT) };
    let cells = surface.cols() * surface.rows();
    *PANIC_FB.lock() = Some(surface);
    *FB_INFO.lock() = Some(fb);
    // Room for the fatal-error grid, reserved now (see init_screen).
    let mut t = TERM.lock();
    let need = cells.saturating_sub(t.cells.len());
    t.cells.reserve(need);
}

/// The firmware left a text screen but a display driver switched to a
/// graphics mode: the terminal leaves the screen to the interface, as in
/// `init_screen` for a graphical start.
pub fn leave_text_mode() {
    let mut t = TERM.lock();
    if !matches!(t.surface, Some(Screen::Vga(_))) {
        return;
    }
    t.cols = HEADLESS_COLS;
    t.rows = HEADLESS_ROWS;
    t.cells.clear();
    t.cells.resize(HEADLESS_COLS * HEADLESS_ROWS, BLANK);
    t.surface = Some(Screen::Headless(Headless { cols: HEADLESS_COLS, rows: HEADLESS_ROWS }));
    t.cursor = true;
}

/// The framebuffer the firmware set up: its physical address and mapping.
pub fn boot_framebuffer() -> Option<(u64, FbInfo)> {
    *BOOT_FB.lock()
}

/// The framebuffer, if the graphical interface is to use it.
pub fn framebuffer() -> Option<FbInfo> {
    *FB_INFO.lock()
}

/// Copies the terminal's cells into `out`; returns columns, rows, the cursor
/// position and whether the cursor is shown.
pub fn snapshot(out: &mut Vec<Cell>) -> (usize, usize, usize, usize, bool) {
    let t = TERM.lock();
    out.clear();
    out.extend_from_slice(&t.cells);
    (t.cols, t.rows, t.col, t.row, t.cursor)
}

/// Changes whenever the terminal does.
pub fn generation() -> u64 {
    GENERATION.load(core::sync::atomic::Ordering::Relaxed)
}

/// The character a cell shows.
pub fn cell_char(glyph: u8) -> char {
    cp437::to_char(glyph)
}

/// The 8x16 BIOS-font bitmap of a cell's glyph (one byte per row).
pub fn bios_glyph(glyph: u8) -> &'static [u8] {
    &font::FONT[glyph as usize * 16..glyph as usize * 16 + 16]
}

/// The colour of a palette entry.
pub fn palette(i: u8) -> u32 {
    PALETTE[(i as usize).min(PALETTE.len() - 1)]
}

/// Framebuffer size in pixels, if the screen is a framebuffer.
pub fn pixel_size() -> Option<(usize, usize)> {
    if let Some(fb) = *FB_INFO.lock() {
        return Some((fb.width, fb.height));
    }
    match TERM.lock().surface.as_ref() {
        Some(Screen::Fb(fb)) => Some(fb.size()),
        _ => None,
    }
}

/// Size of the text screen in cells (0, 0 without a screen).
pub fn size() -> (usize, usize) {
    let t = TERM.lock();
    (t.cols, t.rows)
}

pub fn clear() {
    serial::write(b"\x1b[2J\x1b[H");
    let mut t = TERM.lock();
    t.clear();
    t.show_cursor();
    t.flush();
    drop(t);
    touched();
}

pub fn set_cursor_visible(on: bool) {
    let mut t = TERM.lock();
    if !on {
        t.hide_cursor();
    }
    t.cursor = on;
    t.show_cursor();
    t.flush();
    drop(t);
    touched();
}

/// Draws a filled rectangle on a framebuffer screen (no-op in text mode).
pub fn fill_rect_px(x: usize, y: usize, w: usize, h: usize, color: u32) {
    if let Some(Screen::Fb(fb)) = TERM.lock().surface.as_mut() {
        fb.fill_rect(x, y, w, h, color);
        fb.flush();
    }
}

struct PanicOut;

impl Write for PanicOut {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        serial::write(s.as_bytes());
        // SAFETY: the machine is stopping; nobody else holds the terminal.
        unsafe { TERM.force_unlock() };
        TERM.lock().write_str(s);
        Ok(())
    }
}

/// Prints a fatal error and stops the machine. Allocates nothing, so it
/// works even when the heap is the problem.
pub fn panic_screen(args: fmt::Arguments<'_>) -> ! {
    crate::arch::cpu::disable_irq();
    // With the graphical interface, the details go to the serial port only
    // and the screen shows a plain message.
    if crate::gui::active() {
        struct Serial;
        impl Write for Serial {
            fn write_str(&mut self, s: &str) -> fmt::Result {
                serial::write(s.as_bytes());
                Ok(())
            }
        }
        let _ = Serial.write_str("\narcOS stopped: ");
        let _ = Serial.write_fmt(args);
        let _ = Serial.write_str("\n");
        if crate::sysupdate::on_trial() {
            let _ = Serial.write_str("This new version does not work: restarting into the previous one.\n");
        }
        if crate::gui::panic_screen() {
            crate::sysupdate::restart_after_panic();
            crate::arch::cpu::halt_forever();
        }
    }
    // SAFETY: the machine is stopping; nobody else holds these locks.
    unsafe {
        PANIC_FB.force_unlock();
        TERM.force_unlock();
    }
    if let Some(mut fbs) = PANIC_FB.lock().take() {
        let mut t = TERM.lock();
        let (cols, rows) = (fbs.cols(), fbs.rows());
        fbs.clear(DEFAULT_BG);
        t.surface = Some(Screen::Fb(fbs));
        t.cols = cols;
        t.rows = rows;
        // Within the capacity reserved at start-up: no allocation.
        t.cells.resize(cols * rows, BLANK);
        t.col = 0;
        t.row = 0;
        t.clear();
    }
    let _ = PanicOut.write_str("\n\x1b[41;97m arcOS stopped \x1b[0m\n\x1b[91m");
    let _ = PanicOut.write_fmt(args);
    if crate::sysupdate::on_trial() {
        let _ = PanicOut.write_str("\x1b[0m\n\nThis new version of arcOS does not work: restarting into the previous one.\n");
        crate::sysupdate::restart_after_panic();
    }
    let _ = PanicOut.write_str("\x1b[0m\n\nThe system has been halted. Restart the computer.\n");
    crate::arch::cpu::halt_forever();
}

//! The graphical interface: boot splash, first-start setup and the desktop.
//!
//! When the loader gives the kernel a framebuffer of at least 800x600, the
//! console stops drawing on the screen (it keeps writing to the serial port
//! and the journal) and the screen belongs to this module:
//!
//! 1. [`boot_start`] shows the splash as soon as the screen is up and
//!    [`boot_progress`] moves its bar as the kernel starts its parts;
//! 2. [`start`] runs the interface thread: it loads icons and wallpapers from
//!    `sys:/system/gui`, then shows the setup on the first start (until it
//!    is done once) and the desktop after that.
//!
//! Drawing happens in a back buffer ([`gfx::Canvas`]); only the parts that
//! changed are copied to the framebuffer, and the pointer is drawn straight
//! on the framebuffer so that moving it redraws nothing.
//!
//! Everything is laid out in units. On a high-density screen (at least
//! 2560 x 1440, unless the user chose otherwise) a unit is two pixels: the
//! interface keeps its size and is drawn with twice the detail, text from
//! fonts rendered at twice the size ([`scale`]).

pub mod gfx;
pub use justgfx::qoi;
pub mod text;

mod apps;
mod cursor;
mod desktop;
pub mod dnd;
mod lock;
pub(crate) mod res;
mod settings;
mod setup;
mod splash;
mod textarea;
mod ui;
pub mod window;
pub mod wm;

use crate::console::FbInfo;
use crate::sync::IrqLock;
use gfx::{Canvas, Rect};

/// The framebuffer and the back buffer shown on it.
pub struct Display {
    fb: FbInfo,
    pub scene: Canvas,
}

/// Pixels per unit on the screen: 1, or 2 on a high-density screen.
static SCALE: core::sync::atomic::AtomicI32 = core::sync::atomic::AtomicI32::new(1);

/// Pixels per unit on the screen now. Canvases for the screen are made at
/// this scale.
pub fn scale() -> i32 {
    SCALE.load(core::sync::atomic::Ordering::Relaxed)
}

/// The scale for a screen of `w` x `h` pixels: what the user chose
/// (`choice`, 0 for automatic), if the screen is big enough for it.
/// Automatic is 2 from 2560 x 1440 up (the interface then has the room of
/// 1280 x 720).
pub fn scale_for(w: usize, h: usize, choice: u32) -> i32 {
    let fits = |s: usize| w.is_multiple_of(s) && h.is_multiple_of(s) && w / s >= 800 && h / s >= 600;
    match choice {
        2 if fits(2) => 2,
        0 if w >= 2560 && h >= 1440 && fits(2) => 2,
        _ => 1,
    }
}

impl Display {
    /// The screen's width in units.
    pub fn width(&self) -> i32 {
        self.scene.w
    }

    pub fn height(&self) -> i32 {
        self.scene.h
    }

    /// Pixels per unit.
    pub fn scale(&self) -> i32 {
        self.scene.scale()
    }

    /// A back buffer for `fb` at `scale` pixels per unit.
    fn scene_for(fb: &FbInfo, scale: i32) -> Canvas {
        Canvas::new_at(fb.width as i32 / scale, fb.height as i32 / scale, 0xFF00_0000, scale)
    }

    /// Copies a region of the back buffer to the screen (`r` in units).
    pub fn present(&self, r: Rect) {
        let r = r.intersect(&self.scene.bounds());
        if r.is_empty() {
            return;
        }
        let k = self.scene.scale();
        let r = Rect::new(r.x * k, r.y * k, r.w * k, r.h * k);
        let stride = self.scene.pixel_width();
        let level = brightness();
        let fast = self.fb.shift == (16, 8, 0) && level >= 100;
        for y in r.y..r.bottom() {
            let src = &self.scene.px[(y * stride + r.x) as usize..(y * stride + r.right()) as usize];
            let dst = (self.fb.virt + y as u64 * self.fb.pitch as u64 + r.x as u64 * 4) as *mut u32;
            if level < 100 {
                // Software brightness: every channel scaled down.
                let k = level * 256 / 100;
                for (i, &p) in src.iter().enumerate() {
                    let (r, g, b) = ((p >> 16) & 0xFF, (p >> 8) & 0xFF, p & 0xFF);
                    let p = ((r * k) >> 8) << 16 | ((g * k) >> 8) << 8 | (b * k) >> 8;
                    let v = self.convert(p);
                    // SAFETY: as below.
                    unsafe { dst.add(i).write_volatile(v) };
                }
            } else if fast {
                // SAFETY: row y, columns r.x..r.right() lie inside the mapped
                // framebuffer (r is clipped to the screen).
                unsafe { core::ptr::copy_nonoverlapping(src.as_ptr(), dst, src.len()) };
            } else {
                for (i, &p) in src.iter().enumerate() {
                    let v = self.convert(p);
                    // SAFETY: as above.
                    unsafe { dst.add(i).write_volatile(v) };
                }
            }
        }
        crate::dev::display::flush(r.x, r.y, r.w, r.h);
    }

    /// Switches the screen to another resolution (through the display
    /// driver). The back buffer starts black at the new size.
    pub fn set_mode(&mut self, mode: justdk::DisplayMode) -> Result<(), &'static str> {
        let fb = crate::dev::display::set_mode(mode)?;
        let scale = scale_for(fb.width, fb.height, settings::get().scale);
        self.adopt(fb, scale);
        Ok(())
    }

    /// Draws at the scale the settings ask for on this screen; true if it
    /// changed (the back buffer then starts black, like after a new mode).
    pub fn apply_scale(&mut self) -> bool {
        let scale = scale_for(self.fb.width, self.fb.height, settings::get().scale);
        if scale == self.scene.scale() {
            return false;
        }
        let fb = self.fb;
        self.adopt(fb, scale);
        true
    }

    /// Draws on `fb` at `scale` from now on.
    fn adopt(&mut self, fb: FbInfo, scale: i32) {
        if scale > 1 {
            text::load_sharp();
        }
        self.fb = fb;
        self.scene = Canvas::new(0, 0, 0);
        self.scene = Display::scene_for(&fb, scale);
        SCALE.store(scale, core::sync::atomic::Ordering::Relaxed);
        let mut panic = Canvas::new_at(PANIC_W, PANIC_H, 0, scale);
        if let Some(p) = PANIC.lock().as_mut() {
            p.0 = fb;
            core::mem::swap(&mut p.1, &mut panic);
        }
    }

    /// After sleep the display driver may have set the same mode again with
    /// another framebuffer (a virtio GPU's own memory instead of the VGA
    /// window the firmware used): draw there from now on.
    pub fn follow_framebuffer(&mut self) {
        let Some(fb) = crate::console::framebuffer() else { return };
        if fb.virt == self.fb.virt || fb.width as i32 != self.scene.pixel_width() || fb.height as i32 != self.scene.pixel_height() {
            return;
        }
        self.fb = fb;
        if let Some(p) = PANIC.lock().as_mut() {
            p.0 = fb;
        }
    }

    /// The current resolution (in pixels).
    pub fn mode(&self) -> justdk::DisplayMode {
        justdk::DisplayMode { width: self.fb.width as u32, height: self.fb.height as u32 }
    }

    fn convert(&self, p: u32) -> u32 {
        let (r, g, b) = ((p >> 16) & 0xFF, (p >> 8) & 0xFF, p & 0xFF);
        r << self.fb.shift.0 | g << self.fb.shift.1 | b << self.fb.shift.2
    }

    /// Writes one pixel straight to the screen (the pointer), at pixel
    /// (`x`, `y`).
    fn put_screen(&self, x: i32, y: i32, p: u32) {
        if x < 0 || y < 0 || x >= self.scene.pixel_width() || y >= self.scene.pixel_height() {
            return;
        }
        let v = if self.fb.shift == (16, 8, 0) { p } else { self.convert(p) };
        let at = (self.fb.virt + y as u64 * self.fb.pitch as u64 + x as u64 * 4) as *mut u32;
        // SAFETY: (x, y) is on the screen.
        unsafe { at.write_volatile(v) };
    }
}

/// The display during start-up (the splash). The interface thread then
/// takes it and owns it: drawing a frame must not happen under a lock, which
/// would keep interrupts off for its whole duration.
static DISPLAY: IrqLock<Option<Display>> = IrqLock::new(None);
static ACTIVE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Runs `f` with the start-up display (only while the splash is shown).
pub fn with_display<T>(f: impl FnOnce(&mut Display) -> T) -> Option<T> {
    DISPLAY.lock().as_mut().map(f)
}

/// Is the screen graphical (the interface is in charge of it)?
pub fn active() -> bool {
    ACTIVE.load(core::sync::atomic::Ordering::Relaxed)
}

/// Called right after the console: takes the framebuffer and shows the splash.
pub fn boot_start() {
    let Some(fb) = crate::console::framebuffer() else { return };
    // The settings are not readable yet: the automatic scale until the
    // desktop starts.
    let scale = scale_for(fb.width, fb.height, 0);
    if scale > 1 {
        text::load_sharp();
    }
    SCALE.store(scale, core::sync::atomic::Ordering::Relaxed);
    let scene = Display::scene_for(&fb, scale);
    let panic = Canvas::new_at(PANIC_W, PANIC_H, 0, scale);
    *DISPLAY.lock() = Some(Display { fb, scene });
    *PANIC.lock() = Some((fb, panic));
    ACTIVE.store(true, core::sync::atomic::Ordering::Relaxed);
    splash::show(0);
}

/// The screen and a picture kept ready for a fatal error: when the kernel
/// stops, the user sees a calm message, not a page of technical text (that
/// goes to the serial port and the journal).
static PANIC: IrqLock<Option<(FbInfo, Canvas)>> = IrqLock::new(None);
const PANIC_W: i32 = 760;
const PANIC_H: i32 = 260;

/// Shows the fatal error screen; false if the screen is not graphical.
/// Allocates nothing.
pub fn panic_screen() -> bool {
    if !active() {
        return false;
    }
    // SAFETY: the machine is stopping; nobody else holds these locks.
    unsafe {
        PANIC.force_unlock();
        text::force_unlock();
    }
    let mut guard = PANIC.lock();
    let Some((fb, c)) = guard.as_mut() else { return false };
    let (w, h) = (fb.width as i32, fb.height as i32);
    let convert = |p: u32| {
        let (r, g, b) = ((p >> 16) & 0xFF, (p >> 8) & 0xFF, p & 0xFF);
        r << fb.shift.0 | g << fb.shift.1 | b << fb.shift.2
    };
    // The background: the splash's colours.
    for y in 0..h {
        let t = (y * 255 / h.max(1)) as u32;
        let row = convert(gfx::mix(0xFF1A_1428, 0xFF2C_1F3F, t));
        for x in 0..w {
            let at = (fb.virt + y as u64 * fb.pitch as u64 + x as u64 * 4) as *mut u32;
            // SAFETY: (x, y) is on the screen.
            unsafe { at.write_volatile(row) };
        }
    }
    // The message, drawn on the same gradient, in the middle.
    let k = c.scale();
    let (cw, ch) = (c.pixel_width(), c.pixel_height());
    let (ox, oy) = ((w - cw) / 2, (h - ch) / 2);
    c.reset_clip();
    for y in 0..PANIC_H {
        let t = ((oy + y * k).clamp(0, h) * 255 / h.max(1)) as u32;
        c.fill(Rect::new(0, y, PANIC_W, 1), gfx::mix(0xFF1A_1428, 0xFF2C_1F3F, t));
    }
    let cx = PANIC_W / 2;
    splash::draw_logo(c, cx, 56, 21, 0xFFF2_EFE6);
    text::draw_centered(c, text::Font::Title, cx, 104, "justOS si è dovuto fermare", 0xFFF5_F2F8);
    text::draw_centered(c, text::Font::Body, cx, 156, "Si è verificato un problema che il sistema non può risolvere da solo.", 0xB3F5_F2F8);
    text::draw_centered(c, text::Font::Body, cx, 184, "Spegni e riaccendi il computer.", 0xB3F5_F2F8);
    for y in 0..ch {
        for x in 0..cw {
            let (sx, sy) = (ox + x, oy + y);
            if sx < 0 || sy < 0 || sx >= w || sy >= h {
                continue;
            }
            let at = (fb.virt + sy as u64 * fb.pitch as u64 + sx as u64 * 4) as *mut u32;
            // SAFETY: (sx, sy) is on the screen.
            unsafe { at.write_volatile(convert(c.pixel(x, y))) };
        }
    }
    crate::dev::display::flush(0, 0, w, h);
    true
}

/// Moves the splash's bar (0..=100).
pub fn boot_progress(percent: u32) {
    splash::show(percent);
}

/// Notifications waiting for the desktop: title, text, icon.
static NOTIFICATIONS: IrqLock<alloc::vec::Vec<(alloc::string::String, alloc::string::String, alloc::string::String)>> =
    IrqLock::new(alloc::vec::Vec::new());

/// Shows a notification banner on the desktop (from any thread).
pub fn notify(title: &str, body: &str, icon: &str) {
    if !active() {
        return;
    }
    let mut q = NOTIFICATIONS.lock();
    if q.len() < 8 {
        q.push((title.into(), body.into(), icon.into()));
    }
}

/// Asks the user whether `app` may use `what` ("la cartella Immagini"), in
/// a dialog of the system; waits for the answer (None: the asking program
/// was stopped).
pub fn ask_permission(app: &crate::sandbox::App, what: &str) -> Option<bool> {
    apps::prompt::permission(app, what)
}

/// The system's file picker, for a program (`app` names it): a file to
/// open, or (`save`) where to save `name`. None if the user closed it.
pub fn pick_file(app: &str, save: bool, name: &str) -> Option<alloc::string::String> {
    apps::prompt::pick(app, save, name)
}

fn take_notification() -> Option<(alloc::string::String, alloc::string::String, alloc::string::String)> {
    let mut q = NOTIFICATIONS.lock();
    if q.is_empty() {
        None
    } else {
        Some(q.remove(0))
    }
}

/// The computer is about to sleep: the desktop locks (with a password).
static SLEEP_LOCK: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// The computer woke up: the display lost its picture, everything is
/// drawn again.
static REDRAW: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

pub fn before_sleep() {
    SLEEP_LOCK.store(true, core::sync::atomic::Ordering::Release);
}

pub fn after_sleep() {
    REDRAW.store(true, core::sync::atomic::Ordering::Release);
}

fn take_sleep_lock() -> bool {
    SLEEP_LOCK.swap(false, core::sync::atomic::Ordering::AcqRel)
}

fn take_redraw() -> bool {
    REDRAW.swap(false, core::sync::atomic::Ordering::AcqRel)
}

/// A resolution the user chose, applied by the interface thread.
static MODE_REQUEST: IrqLock<Option<justdk::DisplayMode>> = IrqLock::new(None);

/// Asks the interface to switch the screen to `mode`.
pub fn request_mode(mode: justdk::DisplayMode) {
    *MODE_REQUEST.lock() = Some(mode);
}

/// Changes the resolution and remembers it (from the shell or the
/// settings).
pub fn change_resolution(mode: justdk::DisplayMode) -> Result<(), &'static str> {
    if !active() {
        return Err("the resolution can change only with the graphical interface");
    }
    if crate::dev::display::controller().is_none() {
        return Err("no display driver: the screen stays in the mode the firmware chose");
    }
    if !crate::dev::display::modes().contains(&mode) {
        return Err("the display does not offer this resolution");
    }
    settings::update(|s| s.resolution = Some((mode.width, mode.height)));
    request_mode(mode);
    Ok(())
}

/// The screen's brightness, 20..=100 (applied in software).
static BRIGHTNESS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(100);

pub fn brightness() -> u32 {
    BRIGHTNESS.load(core::sync::atomic::Ordering::Relaxed)
}

pub fn set_brightness(level: u32) {
    BRIGHTNESS.store(level.clamp(20, 100), core::sync::atomic::Ordering::Relaxed);
}

/// Sets the volume and remembers it.
pub fn change_volume(v: u32) {
    if active() {
        settings::update(|s| s.volume = v.min(100));
    } else {
        crate::dev::audio::set_volume(v);
    }
}

fn take_mode_request() -> Option<justdk::DisplayMode> {
    MODE_REQUEST.lock().take()
}

/// The user chose another size for the interface.
static SCALE_REQUEST: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Sets the size of the interface (0: automatic, 1: normal, 2: double),
/// remembers it and applies it.
pub fn change_scale(choice: u32) -> Result<(), &'static str> {
    if !active() {
        return Err("the interface size can change only with the graphical interface");
    }
    if choice > 2 {
        return Err("the size is 0 (automatic), 1 or 2");
    }
    settings::update(|s| s.scale = choice);
    SCALE_REQUEST.store(true, core::sync::atomic::Ordering::Release);
    Ok(())
}

/// The interface size the user chose (0: automatic).
pub fn scale_choice() -> u32 {
    settings::get().scale
}

/// Whether the screen is big enough for the double size (800 x 600 units
/// or more).
pub fn can_double() -> bool {
    crate::console::pixel_size().is_some_and(|(w, h)| scale_for(w, h, 2) == 2)
}

fn take_scale_request() -> bool {
    SCALE_REQUEST.swap(false, core::sync::atomic::Ordering::AcqRel)
}

/// The resolution saved in the settings, if a driver can set it and the
/// screen is not in it already.
fn apply_saved_mode(d: &mut Display) {
    let Some((w, h)) = settings::get().resolution else { return };
    let mode = justdk::DisplayMode { width: w, height: h };
    if crate::dev::display::controller().is_some() && d.mode() != mode {
        if let Err(e) = d.set_mode(mode) {
            crate::warn!("gui", "{w}x{h}: {e}");
        }
    }
}

/// Starts the interface thread once the system is up.
pub fn start() {
    if !active() && !leave_text_mode() {
        return;
    }
    crate::task::spawn("gui", run);
    crate::linux::wayland::start();
}

/// The firmware left a text screen (an old VESA BIOS without 32-bit modes),
/// but a display driver is loaded: switch to a graphics mode with it and
/// start the interface there. False if there is no driver or no mode.
fn leave_text_mode() -> bool {
    if crate::dev::display::controller().is_none() {
        return false;
    }
    // The largest mode up to 1280x800 (modes come largest first).
    let modes = crate::dev::display::modes();
    let Some(mode) = modes.iter().copied().find(|m| m.width <= 1280 && m.height <= 800 && m.width >= 800).or(modes.first().copied())
    else {
        return false;
    };
    if let Err(e) = crate::dev::display::set_mode(mode) {
        crate::warn!("gui", "no graphics mode: {e}");
        return false;
    }
    crate::console::leave_text_mode();
    boot_start();
    true
}

fn run() {
    // Loading the pictures is the last stretch of the bar.
    splash::show(85);
    res::load(|p| splash::show(85 + p * 15 / 100));
    splash::show(100);
    crate::task::sleep_ms(250);
    let Some(mut d) = DISPLAY.lock().take() else { return };
    apply_saved_mode(&mut d);
    d.apply_scale();
    if !settings::get().setup_done {
        setup::run(&mut d);
    }
    desktop::run(&mut d);
}

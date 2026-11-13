//! Windows of programs: a program asks for a window with a system call,
//! draws its content in its own memory and hands each finished frame over
//! (`WIN_PRESENT`); the desktop shows it inside a window like the built-in
//! apps' and passes the mouse and keyboard back as events (`WIN_EVENT`).
//!
//! The picture keeps its alpha: what the program leaves transparent shows
//! the window's frosted glass.
//!
//! Sizes and positions are in units. On a screen with two pixels per unit a
//! program may draw its frames with twice the pixels (`WIN_SCALE` says how
//! many, `WIN_PRESENT` says which it drew); a frame of one pixel per unit is
//! stretched.

use super::gfx::{Canvas, Rect};
use super::wm::{Action, App};
use crate::dev::input::KeyEvent;
use crate::sync::{IrqLock, Mutex};
use crate::task::WaitQueue;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use justdk::syscall as sys;

/// Largest window content a program may ask for (in units).
pub const MAX_W: i32 = 2560;
pub const MAX_H: i32 = 1600;
/// Most pixels per unit in a frame.
pub const MAX_SCALE: i32 = 2;
const QUEUE_MAX: usize = 256;

pub struct Shared {
    pub pid: u32,
    title: String,
    icon: String,
    /// The content size asked for.
    size: (i32, i32),
    resizable: bool,
    /// The last frame, and a number that changes with each one.
    frame: Canvas,
    generation: u64,
    events: VecDeque<[u64; 4]>,
    /// The size the window has on the screen now.
    area: (i32, i32),
    /// The pixels per unit the program was last told of.
    scale: i32,
    /// The user closed it, or the program ended.
    closed: bool,
    ended: bool,
    /// Files may be dropped on it; the last ones dropped.
    drops: bool,
    dropped: Vec<String>,
}

type Handle = Arc<Mutex<Shared>>;

/// Windows by handle (handles are global; each belongs to one process).
static WINDOWS: IrqLock<Vec<(u64, Handle)>> = IrqLock::new(Vec::new());
static NEXT: IrqLock<u64> = IrqLock::new(1);
/// Windows created since the desktop last looked.
static NEW: IrqLock<Vec<Handle>> = IrqLock::new(Vec::new());
static EVENTS: WaitQueue = WaitQueue::new();

fn get(handle: u64, pid: u32) -> Option<Handle> {
    WINDOWS.lock().iter().find(|(h, w)| *h == handle && w.lock().pid == pid).map(|(_, w)| w.clone())
}

/// `WIN_CREATE`: a new window for process `pid`.
pub fn create(pid: u32, title: &str, w: i32, h: i32, flags: u64) -> Result<u64, sys::Error> {
    if !super::active() {
        return Err(sys::Error::Unknown);
    }
    if w < 1 || h < 1 || w > MAX_W || h > MAX_H {
        return Err(sys::Error::BadAddress);
    }
    let icon = super::apps::package_icon(&crate::user::current_name());
    let shared = Shared {
        pid,
        title: String::from(title),
        icon,
        size: (w, h),
        resizable: flags & sys::WIN_RESIZABLE != 0,
        frame: Canvas::new_layer(w, h),
        generation: 0,
        events: VecDeque::new(),
        area: (w, h),
        scale: super::scale(),
        closed: false,
        ended: false,
        drops: flags & sys::WIN_DROPS != 0,
        dropped: Vec::new(),
    };
    let handle = {
        let mut n = NEXT.lock();
        let h = *n;
        *n += 1;
        h
    };
    let arc = Arc::new(Mutex::new(shared));
    WINDOWS.lock().push((handle, arc.clone()));
    NEW.lock().push(arc);
    Ok(handle)
}

/// `WIN_PRESENT`: a frame of `w` x `h` pixels (0xAARRGGBB) from the
/// program's memory, drawn at `scale` pixels per unit (0 is 1).
pub fn present(pid: u32, handle: u64, pixels: &[u8], w: i32, h: i32, scale: i32) -> Result<(), sys::Error> {
    let win = get(handle, pid).ok_or(sys::Error::BadHandle)?;
    let k = scale.max(1);
    let fits = k <= MAX_SCALE && w >= k && h >= k && w % k == 0 && h % k == 0 && w <= MAX_W * k && h <= MAX_H * k;
    if !fits || pixels.len() < (w * h * 4) as usize {
        return Err(sys::Error::BadAddress);
    }
    let mut s = win.lock();
    if s.frame.pixel_width() != w || s.frame.pixel_height() != h || s.frame.scale() != k {
        s.frame = Canvas::new_layer_at(w / k, h / k, k);
    }
    for (dst, src) in s.frame.px.iter_mut().zip(pixels.as_chunks::<4>().0) {
        *dst = u32::from_le_bytes(*src);
    }
    s.generation += 1;
    Ok(())
}

/// `WIN_EVENT`: the next event, waiting up to `ms` for one.
pub fn next_event(pid: u32, handle: u64, ms: u64) -> Result<Option<[u64; 4]>, sys::Error> {
    let win = get(handle, pid).ok_or(sys::Error::BadHandle)?;
    let take = || {
        let mut s = win.lock();
        if let Some(e) = s.events.pop_front() {
            return Some(Some(e));
        }
        if s.closed {
            return Some(Some([sys::WIN_EV_CLOSE, 0, 0, 0]));
        }
        if crate::user::kill_requested() {
            return Some(None);
        }
        None
    };
    if ms == 0 {
        return Ok(take().flatten());
    }
    let ms = ms.min(3_600_000);
    Ok(EVENTS.wait_for_timeout(ms, take).flatten())
}

/// `WIN_DROPPED`: the files of the last drop, one per line.
pub fn dropped(pid: u32, handle: u64) -> Result<String, sys::Error> {
    let win = get(handle, pid).ok_or(sys::Error::BadHandle)?;
    let s = win.lock();
    let mut text = String::new();
    for p in &s.dropped {
        text.push_str(p);
        text.push('\n');
    }
    Ok(text)
}

/// `WIN_SCALE`: how many pixels per unit the window is shown with.
pub fn scale(pid: u32, handle: u64) -> Result<u64, sys::Error> {
    get(handle, pid).ok_or(sys::Error::BadHandle)?;
    Ok(super::scale() as u64)
}

/// `THEME`: the theme windows are drawn in, for programs to draw theirs in.
pub fn theme() -> u64 {
    if super::ui::theme() == super::ui::Theme::Light {
        sys::THEME_LIGHT
    } else {
        sys::THEME_DARK
    }
}

pub fn set_title(pid: u32, handle: u64, title: &str) -> Result<(), sys::Error> {
    let win = get(handle, pid).ok_or(sys::Error::BadHandle)?;
    win.lock().title = String::from(title);
    Ok(())
}

/// `WIN_CLOSE`, or the program ended: its window goes.
pub fn close(pid: u32, handle: u64) -> Result<(), sys::Error> {
    let win = get(handle, pid).ok_or(sys::Error::BadHandle)?;
    win.lock().ended = true;
    WINDOWS.lock().retain(|(h, _)| *h != handle);
    Ok(())
}

/// A process ended: all its windows close. Returns whether it had any.
pub fn process_ended(pid: u32) -> bool {
    let mut any = false;
    WINDOWS.lock().retain(|(_, w)| {
        let mut s = w.lock();
        if s.pid == pid {
            s.ended = true;
            any = true;
            false
        } else {
            true
        }
    });
    any
}

/// The title of a process's first window (to name it in messages).
pub fn title_of(pid: u32) -> Option<String> {
    WINDOWS.lock().iter().find(|(_, w)| w.lock().pid == pid).map(|(_, w)| w.lock().title.clone())
}

/// New windows for the desktop to open.
pub fn take_new() -> Vec<alloc::boxed::Box<dyn App>> {
    let list = core::mem::take(&mut *NEW.lock());
    list.into_iter()
        .map(|s| {
            let kind = intern(&s.lock().icon);
            alloc::boxed::Box::new(UserWindow { kind, shared: s, seen: u64::MAX, inside: false }) as alloc::boxed::Box<dyn App>
        })
        .collect()
}

/// Window kinds of programs, one per program (kept for good: a few bytes
/// each).
static KINDS: IrqLock<Vec<&'static str>> = IrqLock::new(Vec::new());

/// The icon of a program's windows (`program` as the process is named).
pub fn icon_for(program: &str) -> String {
    if let Some(icon) = crate::store::icon(program) {
        return icon;
    }
    // A Linux program has the icon of its .desktop file.
    let file = alloc::format!("/{program}");
    super::apps::linux_apps()
        .into_iter()
        .find(|e| matches!(&e.launch, super::apps::Launch::Spawn(p) if p.ends_with(&file)))
        .map_or_else(|| String::from("window"), |e| e.icon)
}

pub fn intern(name: &str) -> &'static str {
    let mut k = KINDS.lock();
    if let Some(s) = k.iter().find(|s| **s == name) {
        return s;
    }
    let s: &'static str = alloc::boxed::Box::leak(String::from(name).into_boxed_str());
    k.push(s);
    s
}

/// A program's window, as the desktop sees it.
struct UserWindow {
    kind: &'static str,
    shared: Handle,
    seen: u64,
    inside: bool,
}

impl UserWindow {
    fn push(&self, ev: [u64; 4]) {
        let mut s = self.shared.lock();
        if s.events.len() < QUEUE_MAX {
            // Moves pile up: keep only the latest.
            if ev[0] == sys::WIN_EV_MOVE {
                if let Some(last) = s.events.back_mut() {
                    if last[0] == sys::WIN_EV_MOVE {
                        *last = ev;
                        drop(s);
                        EVENTS.notify_all();
                        return;
                    }
                }
            }
            s.events.push_back(ev);
        }
        drop(s);
        EVENTS.notify_all();
    }
}

impl App for UserWindow {
    fn title(&self) -> String {
        self.shared.lock().title.clone()
    }

    fn icon(&self) -> &str {
        // The kind is the program's icon name.
        self.kind
    }

    fn kind(&self) -> &'static str {
        self.kind
    }

    fn size(&self) -> (i32, i32) {
        self.shared.lock().size
    }

    fn resizable(&self) -> bool {
        self.shared.lock().resizable
    }

    fn draw(&mut self, c: &mut Canvas, area: Rect, _blur: &Canvas, _focused: bool) {
        let mut s = self.shared.lock();
        self.seen = s.generation;
        let (w, h) = (area.w, area.h);
        let k = c.scale();
        if s.area != (w, h) || s.scale != k {
            s.area = (w, h);
            s.scale = k;
            if s.events.len() < QUEUE_MAX {
                s.events.push_back([sys::WIN_EV_RESIZE, w as u64, h as u64, k as u64]);
            }
            drop(s);
            EVENTS.notify_all();
            s = self.shared.lock();
        }
        c.draw_image(&s.frame, area.x, area.y, 255);
    }

    fn press(&mut self, x: i32, y: i32, double: bool, _area: Rect) -> Action {
        self.push([sys::WIN_EV_PRESS, x as u64, y as u64, double as u64]);
        Action::None
    }

    fn drag(&mut self, x: i32, y: i32, _area: Rect) -> bool {
        self.push([sys::WIN_EV_MOVE, x as u64, y as u64, 1]);
        false
    }

    fn release(&mut self, _area: Rect) -> Action {
        self.push([sys::WIN_EV_RELEASE, 0, 0, 0]);
        Action::None
    }

    fn hover(&mut self, at: Option<(i32, i32)>, _area: Rect) -> bool {
        match at {
            Some((x, y)) => {
                self.inside = true;
                self.push([sys::WIN_EV_MOVE, x as u64, y as u64, 0]);
            }
            None if self.inside => {
                self.inside = false;
                self.push([sys::WIN_EV_LEAVE, 0, 0, 0]);
            }
            None => {}
        }
        false
    }

    fn menu(&mut self, x: i32, y: i32, _area: Rect) -> Action {
        self.push([sys::WIN_EV_PRESS, x as u64, y as u64, 2]);
        Action::None
    }

    fn key(&mut self, ev: KeyEvent) -> Action {
        self.push([sys::WIN_EV_KEY, crate::user::key_code(ev), 0, 0]);
        Action::None
    }

    fn scroll(&mut self, lines: i32) -> bool {
        self.push([sys::WIN_EV_SCROLL, lines as i64 as u64, 0, 0]);
        false
    }

    /// Files dropped on a program that takes them: it may now read them
    /// (folders are left out), and hears of them with `WIN_EV_DROP`.
    fn drop_files(&mut self, paths: &[String], x: i32, y: i32, _area: Rect) -> Option<Action> {
        let pid = {
            let s = self.shared.lock();
            if !s.drops {
                return None;
            }
            s.pid
        };
        let files: Vec<String> = paths
            .iter()
            .filter_map(|p| crate::fs::abs(p).ok().filter(|q| crate::fs::exists(q) && !crate::fs::is_dir(q)).map(|q| (alloc::format!("{q}"), q)))
            .map(|(text, path)| {
                crate::user::grant(pid, path, false);
                text
            })
            .collect();
        if files.is_empty() {
            return None;
        }
        let n = files.len() as u64;
        self.shared.lock().dropped = files;
        self.push([sys::WIN_EV_DROP, x as u64, y as u64, n]);
        Some(Action::None)
    }

    fn animating(&self) -> bool {
        self.shared.lock().generation != self.seen
    }

    fn frame(&mut self) -> bool {
        true
    }

    fn close(&mut self) {
        let pid = {
            let mut s = self.shared.lock();
            s.closed = true;
            s.pid
        };
        EVENTS.notify_all();
        // A program that does not close by itself is stopped.
        crate::task::spawn("close", move || {
            crate::task::sleep_ms(2000);
            crate::user::kill(pid);
        });
    }

    fn alive(&self) -> bool {
        !self.shared.lock().ended
    }
}

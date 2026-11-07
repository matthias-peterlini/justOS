//! A Wayland toplevel as a window of the desktop: the desktop draws its
//! frame (title, buttons, glass) like every other window's, and the
//! content is what the client's surfaces composed to. Pointer and keyboard
//! go back to the client's thread as [`Input`].

use crate::dev::input::KeyEvent;
use crate::gui::gfx::{Canvas, Rect};
use crate::gui::wm::{Action, App};
use crate::net::EVENTS;
use crate::sync::{IrqLock, Mutex};
use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

/// evdev button codes.
pub const BTN_LEFT: u32 = 0x110;
pub const BTN_RIGHT: u32 = 0x111;

/// What happened to a window, for its client's thread.
pub enum Input {
    /// The pointer is at (x, y) of the content.
    Motion(i32, i32),
    Button(u32, bool),
    Leave,
    Key(KeyEvent),
    Scroll(i32),
    /// The desktop shows the content at this size now.
    Resize(i32, i32),
    Focus(bool),
    /// The desktop drew the last frame: time for the frame callbacks.
    Drawn,
    /// The user closed the window.
    Close,
}

/// A window's state, shared by the desktop and the client's thread.
pub struct Win {
    pub title: String,
    /// The content size the client drew (units).
    pub size: (i32, i32),
    pub resizable: bool,
    /// The composed content.
    pub frame: Canvas,
    /// Changes with each new frame.
    pub generation: u64,
    /// The size the desktop shows it at, and whether it has the focus.
    pub area: (i32, i32),
    pub focused: bool,
    /// The client let go of it, or went away.
    pub ended: bool,
}

impl Win {
    pub fn new(title: String, w: i32, h: i32) -> Arc<Mutex<Win>> {
        Arc::new(Mutex::new(Win {
            title,
            size: (w, h),
            resizable: true,
            frame: Canvas::new_layer(w, h),
            generation: 0,
            area: (w, h),
            focused: false,
            ended: false,
        }))
    }
}

/// Input of all the windows of one client.
pub struct Inbox {
    pub queue: IrqLock<VecDeque<(u32, Input)>>,
}

impl Inbox {
    pub fn new() -> Arc<Inbox> {
        Arc::new(Inbox { queue: IrqLock::new(VecDeque::new()) })
    }

    pub fn push(&self, id: u32, ev: Input) {
        {
            let mut q = self.queue.lock();
            // Moves pile up: keep only the latest.
            if let (Input::Motion(..), Some((last_id, last @ Input::Motion(..)))) = (&ev, q.back_mut()) {
                if *last_id == id {
                    *last = ev;
                    drop(q);
                    EVENTS.notify_all();
                    return;
                }
            }
            if q.len() < 512 {
                q.push_back((id, ev));
            }
        }
        EVENTS.notify_all();
    }

    pub fn is_empty(&self) -> bool {
        self.queue.lock().is_empty()
    }

    pub fn take(&self) -> VecDeque<(u32, Input)> {
        core::mem::take(&mut *self.queue.lock())
    }
}

struct Opening {
    win: Arc<Mutex<Win>>,
    inbox: Arc<Inbox>,
    id: u32,
    icon: String,
}

/// Windows mapped since the desktop last looked.
static NEW: IrqLock<Vec<Opening>> = IrqLock::new(Vec::new());

/// Asks the desktop to show a window; `id` is its toplevel's surface.
pub fn open(win: Arc<Mutex<Win>>, inbox: Arc<Inbox>, id: u32, icon: String) {
    NEW.lock().push(Opening { win, inbox, id, icon });
}

/// New windows for the desktop to open.
pub fn take_new() -> Vec<Box<dyn App>> {
    let list = core::mem::take(&mut *NEW.lock());
    list.into_iter()
        .map(|o| {
            let kind = crate::gui::window::intern(&o.icon);
            Box::new(WlWindow { kind, icon: o.icon, win: o.win, inbox: o.inbox, id: o.id, seen: u64::MAX, inside: false, button: 0 })
                as Box<dyn App>
        })
        .collect()
}

struct WlWindow {
    kind: &'static str,
    icon: String,
    win: Arc<Mutex<Win>>,
    inbox: Arc<Inbox>,
    id: u32,
    seen: u64,
    inside: bool,
    /// The button held since a press on the content.
    button: u32,
}

impl WlWindow {
    fn push(&self, ev: Input) {
        self.inbox.push(self.id, ev);
    }
}

impl App for WlWindow {
    fn title(&self) -> String {
        self.win.lock().title.clone()
    }

    fn icon(&self) -> &str {
        &self.icon
    }

    fn kind(&self) -> &'static str {
        self.kind
    }

    fn size(&self) -> (i32, i32) {
        self.win.lock().size
    }

    fn resizable(&self) -> bool {
        self.win.lock().resizable
    }

    fn draw(&mut self, c: &mut Canvas, area: Rect, _blur: &Canvas, focused: bool) {
        let mut evs = Vec::new();
        {
            let mut w = self.win.lock();
            if w.area != (area.w, area.h) {
                w.area = (area.w, area.h);
                evs.push(Input::Resize(area.w, area.h));
            }
            if w.focused != focused {
                w.focused = focused;
                evs.push(Input::Focus(focused));
            }
            if self.seen != w.generation {
                self.seen = w.generation;
                evs.push(Input::Drawn);
            }
            let old = c.clip();
            c.set_clip(old.intersect(&area));
            c.draw_image(&w.frame, area.x, area.y, 255);
            c.set_clip(old);
        }
        for e in evs {
            self.push(e);
        }
    }

    fn press(&mut self, x: i32, y: i32, _double: bool, _area: Rect) -> Action {
        self.push(Input::Motion(x, y));
        self.button = BTN_LEFT;
        self.push(Input::Button(BTN_LEFT, true));
        Action::None
    }

    fn drag(&mut self, x: i32, y: i32, _area: Rect) -> bool {
        self.push(Input::Motion(x, y));
        false
    }

    fn release(&mut self, _area: Rect) -> Action {
        if self.button != 0 {
            self.push(Input::Button(self.button, false));
            self.button = 0;
        }
        Action::None
    }

    fn hover(&mut self, at: Option<(i32, i32)>, _area: Rect) -> bool {
        match at {
            Some((x, y)) => {
                self.inside = true;
                self.push(Input::Motion(x, y));
            }
            None if self.inside => {
                self.inside = false;
                self.push(Input::Leave);
            }
            None => {}
        }
        false
    }

    fn menu(&mut self, x: i32, y: i32, _area: Rect) -> Action {
        self.push(Input::Motion(x, y));
        self.push(Input::Button(BTN_RIGHT, true));
        self.push(Input::Button(BTN_RIGHT, false));
        Action::None
    }

    fn key(&mut self, ev: KeyEvent) -> Action {
        self.push(Input::Key(ev));
        Action::None
    }

    fn scroll(&mut self, lines: i32) -> bool {
        self.push(Input::Scroll(lines));
        false
    }

    fn animating(&self) -> bool {
        self.win.lock().generation != self.seen
    }

    fn frame(&mut self) -> bool {
        true
    }

    fn close(&mut self) {
        self.push(Input::Close);
    }

    fn alive(&self) -> bool {
        !self.win.lock().ended
    }
}

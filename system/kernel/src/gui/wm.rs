//! Windows, as in the JustOS design: a frame of warm pink frosted glass
//! with the app's name at the top left and three round buttons (minimise
//! and full screen in dark grey, close in orange) on a light pill at the top
//! right; the content sits on a darker pane inside the frame. Their content
//! is an [`App`].
//!
//! An app with a sidebar gets two panes side by side, with a gap of frame
//! between them. A maximised window fills the whole screen, frame and all,
//! with square corners.

use super::gfx::{Canvas, Rect};
use super::text::{self, Font};
use super::ui::{self, TEXT};
use crate::dev::input::KeyEvent;
use alloc::boxed::Box;
use alloc::string::String;

pub const TITLE_H: i32 = 44;
/// The frame around the content pane, and the gap between panes.
pub const GAP: i32 = 8;
const KNOB: i32 = 24;
/// The corner that resizes a window.
const GRIP: i32 = 18;

/// What an app asks the desktop to do after an event.
pub enum Action {
    None,
    /// Draw the content again.
    Redraw,
    /// Type a command line into the terminal (opening it).
    Run(String),
    /// Open a file with the app made for it.
    OpenFile(String),
    /// Open a built-in app.
    Open(&'static str),
    /// Start an installed program that opens its own window.
    Spawn(String),
    /// Show a notification: title, text, icon.
    Notify(String, String, &'static str),
    /// The wallpaper changed: draw everything again.
    Wallpaper,
}

/// The content of a window. Positions handed to it are relative to the
/// content area unless said otherwise.
pub trait App {
    fn title(&self) -> String;
    fn icon(&self) -> &str;
    /// The size of the content area the app would like.
    fn size(&self) -> (i32, i32);
    /// Draws the content; `blur` is the blurred wallpaper (for glass).
    fn draw(&mut self, c: &mut Canvas, area: Rect, blur: &Canvas, focused: bool);
    fn press(&mut self, _x: i32, _y: i32, _double: bool, _area: Rect) -> Action {
        Action::None
    }
    /// The pointer moved with the button down, after a press on the content.
    fn drag(&mut self, _x: i32, _y: i32, _area: Rect) -> bool {
        false
    }
    fn release(&mut self, _area: Rect) -> Action {
        Action::None
    }
    /// The pointer moved over the content (`None`: it left).
    fn hover(&mut self, _at: Option<(i32, i32)>, _area: Rect) -> bool {
        false
    }
    /// The right button over the content.
    fn menu(&mut self, _x: i32, _y: i32, _area: Rect) -> Action {
        Action::None
    }
    fn key(&mut self, _ev: KeyEvent) -> Action {
        Action::None
    }
    /// Called ten times a second; true if the content changed.
    fn tick(&mut self) -> bool {
        false
    }
    /// After a [`tick`](Self::tick) that returned true: the part of the
    /// content that changed, when it is not all of it (on the screen;
    /// `area` is the content area). A blinking caret is a few pixels, not
    /// a window.
    fn changed(&mut self, _area: Rect) -> Option<Rect> {
        None
    }
    /// Does the app want a call to [`frame`](Self::frame) on every frame?
    fn animating(&self) -> bool {
        false
    }
    /// A frame while [`animating`](Self::animating); true to redraw.
    fn frame(&mut self) -> bool {
        false
    }
    /// A short name, to find an open window of the same kind.
    fn kind(&self) -> &'static str;
    /// Mouse wheel over the content.
    fn scroll(&mut self, _lines: i32) -> bool {
        false
    }
    /// Can the window be made bigger than [`size`](Self::size)?
    fn resizable(&self) -> bool {
        false
    }
    /// Width of a sidebar drawn as a card of its own at the left of the
    /// content area (0: none). The app still draws into the whole area;
    /// its main content starts at `sidebar() + GAP`.
    fn sidebar(&self) -> i32 {
        0
    }
    /// The window is closing (save what needs saving).
    fn close(&mut self) {}
    /// False when the window should go (its program ended).
    fn alive(&self) -> bool {
        true
    }
    /// Asked after each [`drag`](Self::drag): files the user started to
    /// carry out of the window (absolute paths), if any. The desktop then
    /// carries them until the button goes up.
    fn drag_files(&mut self) -> Option<alloc::vec::Vec<String>> {
        None
    }
    /// Files are carried over the content (`None`: they left); the app may
    /// light up the place they would land. True to redraw.
    fn drop_over(&mut self, _at: Option<(i32, i32)>, _paths: &[String], _area: Rect) -> bool {
        false
    }
    /// Files were let go over the content. `None`: the app does not take
    /// them (the desktop says so); otherwise what to do next.
    fn drop_files(&mut self, _paths: &[String], _x: i32, _y: i32, _area: Rect) -> Option<Action> {
        None
    }
    /// Files changed somewhere (a drop moved or copied some): show them again.
    fn refresh(&mut self) {}
}

pub struct Window {
    pub id: u32,
    pub app: Box<dyn App>,
    /// Title pill and body together.
    pub rect: Rect,
    pub minimized: bool,
    /// The size before maximising.
    pub restore: Option<Rect>,
    /// Snapped to half or a quarter of the screen: where it was before.
    pub snapped: Option<Rect>,
    /// The virtual desktop it is on.
    pub space: usize,
    /// The title button under the pointer.
    pub hover: Option<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hit {
    Title,
    Minimize,
    Maximize,
    Close,
    Resize,
    Body(i32, i32),
}

impl Window {
    pub fn new(id: u32, app: Box<dyn App>, x: i32, y: i32) -> Window {
        let (w, h) = app.size();
        let rect = Rect::new(x, y, w + 2 * GAP, h + TITLE_H + GAP);
        Window { id, app, rect, minimized: false, restore: None, snapped: None, space: 0, hover: None }
    }

    /// On the screen when desktop `space` is shown.
    pub fn shown(&self, space: usize) -> bool {
        !self.minimized && self.space == space
    }

    /// The whole window for a content area of `w` x `h`.
    pub fn outer_size(w: i32, h: i32) -> (i32, i32) {
        (w + 2 * GAP, h + TITLE_H + GAP)
    }

    /// Maximised: filling the whole screen.
    pub fn full(&self) -> bool {
        self.restore.is_some()
    }

    /// The title row.
    pub fn title_rect(&self) -> Rect {
        Rect::new(self.rect.x, self.rect.y, self.rect.w, TITLE_H)
    }

    pub fn body(&self) -> Rect {
        Rect::new(self.rect.x + GAP, self.rect.y + TITLE_H, self.rect.w - 2 * GAP, self.rect.h - TITLE_H - GAP)
    }

    /// Minimise, maximise and close, at the right end of the title row.
    pub fn buttons(&self) -> [Rect; 3] {
        let t = self.title_rect();
        let s = KNOB;
        let y = t.y + (t.h - s) / 2;
        let x = t.right() - 12 - s;
        [Rect::new(x - 2 * (s + 6), y, s, s), Rect::new(x - (s + 6), y, s, s), Rect::new(x, y, s, s)]
    }

    /// The pill behind the buttons.
    pub fn knob_pill(&self) -> Rect {
        let [a, _, c] = self.buttons();
        Rect::new(a.x - 6, a.y - 5, c.right() - a.x + 12, a.h + 10)
    }

    fn grip(&self) -> Rect {
        let b = self.body();
        Rect::new(b.right() - GRIP, b.bottom() - GRIP, GRIP + 4, GRIP + 4)
    }

    /// Everything the window paints, shadow included.
    pub fn bounds(&self) -> Rect {
        ui::with_shadow(self.rect).union(&ui::with_shadow(self.title_rect()))
    }

    pub fn hit(&self, x: i32, y: i32) -> Option<Hit> {
        let [min, max, close] = self.buttons();
        if close.contains(x, y) {
            return Some(Hit::Close);
        } else if max.contains(x, y) {
            return Some(Hit::Maximize);
        } else if min.contains(x, y) {
            return Some(Hit::Minimize);
        } else if self.title_rect().contains(x, y) {
            return Some(Hit::Title);
        }
        if self.app.resizable() && !self.full() && self.grip().contains(x, y) {
            Some(Hit::Resize)
        } else if self.body().contains(x, y) {
            let b = self.body();
            Some(Hit::Body(x - b.x, y - b.y))
        } else {
            None
        }
    }

    pub fn draw(&mut self, c: &mut Canvas, blur: &Canvas, focused: bool, clip: Rect) {
        let body = self.body();
        let full = self.full();
        if clip.overlaps(&ui::with_shadow(self.rect)) {
            // The frame, with the title row (square on a full screen).
            let frame = self.rect;
            let rad = if full { 0 } else { 26 };
            if !full {
                c.shadow(frame, rad, 22, if focused { 95 } else { 60 });
            }
            c.blit_round(blur, frame, rad, if focused { ui::FRAME_STRONG } else { ui::FRAME });
            if !full {
                c.stroke_round(frame, rad, 1, 0x66FF_FFFF);
            }
            let t = self.title_rect();
            let ink = ui::wants_ink(blur, t);
            let (fg, dim) = if ink { (ui::INK, ui::INK_DIM) } else { (TEXT, ui::TEXT_DIM) };
            let (lh, _) = text::metrics(Font::Body);
            let pill = self.knob_pill();
            let name = text::fit(Font::Body, &self.app.title(), pill.x - t.x - 40);
            text::draw(c, Font::Body, t.x + 20, t.y + (TITLE_H - lh) / 2 + 1, &name, if focused { fg } else { dim });
            ui::window_knobs(c, pill, &self.buttons(), self.hover);
        }
        if clip.overlaps(&ui::with_shadow(body)) {
            let tint = if focused { ui::GLASS } else { 0xA83A_383F };
            let rad = 18;
            let side = self.app.sidebar();
            if side > 0 {
                let sb = Rect::new(body.x, body.y, side, body.h);
                let main = Rect::new(body.x + side + GAP, body.y, body.w - side - GAP, body.h);
                c.blit_round(blur, sb, rad, tint);
                c.blit_round(blur, main, rad, tint);
            } else {
                c.blit_round(blur, body, rad, tint);
            }
            let saved = c.clip();
            c.set_clip(saved.intersect(&body));
            self.app.draw(c, body, blur, focused);
            if self.app.resizable() && !full {
                // Three short diagonal strokes in the corner.
                let (gx, gy) = (body.right() - 8, body.bottom() - 8);
                for k in [4, 8, 12] {
                    c.line(gx - k, gy, gx, gy - k, 1, 0x40FF_FFFF);
                }
            }
            c.set_clip(saved);
        }
    }

    /// The window drawn on its own, with transparent surroundings (for
    /// animations): the picture and where it goes on the screen.
    pub fn snapshot(&mut self, blur: &Canvas, focused: bool) -> (Canvas, Rect) {
        let b = self.bounds();
        let local_blur = blur.crop(b);
        let mut layer = Canvas::new_layer_at(b.w, b.h, blur.scale());
        let saved = self.rect;
        self.rect = self.rect.offset(-b.x, -b.y);
        let all = layer.bounds();
        self.draw(&mut layer, &local_blur, focused, all);
        self.rect = saved;
        (layer, b)
    }

    /// Maximises into `screen`, or goes back.
    pub fn toggle_maximize(&mut self, screen: Rect) {
        match self.restore.take() {
            Some(r) => self.rect = r,
            None => {
                self.restore = Some(self.rect);
                self.rect = screen;
            }
        }
    }

    /// The smallest size: what the app asked for, or less for resizable ones.
    pub fn min_size(&self) -> (i32, i32) {
        let (w, h) = self.app.size();
        if self.app.resizable() {
            Window::outer_size((w * 3 / 5).max(320), (h * 3 / 5).max(200))
        } else {
            Window::outer_size(w, h)
        }
    }
}

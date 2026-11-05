//! Pieces the screens are built from: glass panels, buttons, text fields,
//! and the loop that turns the mouse and keyboard into events.

use super::cursor::Cursor;
use super::gfx::{Canvas, Rect};
use super::text::{self, Font};
use super::Display;
use crate::dev::input::{self, KeyEvent};

// The JustOS palette: frosted glass in neutral greys over the wallpaper, light
// panes for the bar and the frames, darker panes for content, white text on
// the dark ones and ink on the light ones, light grey and white pills.
pub const TEXT: u32 = 0xFFFF_FFFF;
pub const TEXT_DIM: u32 = 0xFFC9_C6CE;
pub const TEXT_FAINT: u32 = 0xFF94_919A;
/// Text on light glass (the bar, window frames, light buttons).
pub const INK: u32 = 0xFF1B_1A1F;
pub const INK_DIM: u32 = 0xFF4A_4850;
/// The tint of the dark glass panes (window content, menus).
pub const GLASS: u32 = 0xC23A_383F;
pub const GLASS_STRONG: u32 = 0xE02C_2B31;
/// The tint of the light glass (bar, window frames, widgets).
pub const GLASS_LIGHT: u32 = 0x66DA_D6E2;
pub const GLASS_LIGHT_STRONG: u32 = 0x99E2_DEE8;
/// The round dark window buttons.
pub const KNOB: u32 = 0xFF3B_3A3F;
pub const BORDER: u32 = 0x29FF_FFFF;
/// The glow in the corner of a card.
pub const GLOW: u32 = 0xFF8A_3416;
/// The highlight colour (text cursor, progress, focus): ticly's ember.
pub const ACCENT: u32 = 0xFFE0_662E;
pub const ORANGE: u32 = 0xFFFF_9F0A;
pub const RED: u32 = 0xFFFF_5A4E;
pub const GREEN: u32 = 0xFF34_C46A;
/// A panel inside a window (lists, sidebars).
pub const PANEL: u32 = 0x0DFF_FFFF;
pub const PANEL_HOVER: u32 = 0x1AFF_FFFF;
/// The selected row or item: a lighter pane of glass, as in ticly's sidebars.
pub const SELECTED: u32 = 0x2EFF_FFFF;
pub const SEPARATOR: u32 = 0x17FF_FFFF;
/// The dark well of a text field.
pub const FIELD: u32 = 0xB80E_0A0A;

/// A frosted-glass panel: blurred wallpaper under a tint, a thin light edge
/// and a soft shadow.
pub fn glass(c: &mut Canvas, blur: &Canvas, r: Rect, rad: i32, tint: u32) {
    c.shadow(r, rad, 14, 70);
    c.blit_round(blur, r, rad, tint);
    c.stroke_round(r, rad, 1, BORDER);
}

/// A card: [`glass`] with a deeper shadow and a faint sheen at its top.
pub fn card(c: &mut Canvas, blur: &Canvas, r: Rect, rad: i32, tint: u32) {
    c.shadow(r, rad, 18, 80);
    c.blit_round(blur, r, rad, tint);
    let reach = (r.w.max(r.h) * 3 / 5).clamp(120, 520);
    c.glow_round(r, rad, r.x + r.w / 3, r.y, reach, 0xFFFF_FFFF, 18);
    c.stroke_round(r, rad, 1, BORDER);
}

/// Light frosted glass (the bar, window frames, widgets): the blurred
/// wallpaper lightened, with a bright edge.
pub fn light_glass(c: &mut Canvas, blur: &Canvas, r: Rect, rad: i32, shadow: bool) {
    if shadow {
        c.shadow(r, rad, 16, 55);
    }
    c.blit_round(blur, r, rad, GLASS_LIGHT);
    c.stroke_round(r, rad, 1, 0x4DFF_FFFF);
}

/// Does text on the blurred wallpaper inside `r` read better dark? (The
/// glass is light over a bright sky and dark over the woods.)
pub fn wants_ink(blur: &Canvas, r: Rect) -> bool {
    let mut sum = 0u64;
    let mut n = 0u64;
    let step_x = (r.w / 12).max(1);
    let step_y = (r.h / 4).max(1);
    let mut y = r.y + step_y / 2;
    while y < r.bottom() {
        let mut x = r.x + step_x / 2;
        while x < r.right() {
            let p = blur.get(x, y);
            let (rr, gg, bb) = ((p >> 16) & 0xFF, (p >> 8) & 0xFF, p & 0xFF);
            sum += (rr * 299 + gg * 587 + bb * 114) as u64 / 1000;
            n += 1;
            x += step_x;
        }
        y += step_y;
    }
    // The light tint adds about a quarter of white.
    n == 0 || (sum / n) * 3 / 4 + 0x40 > 128
}

/// A glyph icon (white in the atlas) drawn in another colour.
pub fn glyph(c: &mut Canvas, name: &str, size: i32, x: i32, y: i32, color: u32) {
    super::res::draw_icon_tinted(c, name, size, x, y, color);
}

/// Simple status glyphs drawn with lines: the arc hexagon, the moon of "do
/// not disturb" and the sun of brightness, `s` pixels square.
pub fn draw_hexagon(c: &mut Canvas, x: i32, y: i32, s: i32, color: u32) {
    let (cx, cy, r) = (x + s / 2, y + s / 2, s * 9 / 20);
    // Pointy top; 0.866 ~ 13/15.
    let dx = r * 13 / 15;
    let pts = [(cx, cy - r), (cx + dx, cy - r / 2), (cx + dx, cy + r / 2), (cx, cy + r), (cx - dx, cy + r / 2), (cx - dx, cy - r / 2)];
    for i in 0..6 {
        let (a, b) = (pts[i], pts[(i + 1) % 6]);
        c.line(a.0, a.1, b.0, b.1, 2, color);
    }
}

pub fn draw_moon(c: &mut Canvas, x: i32, y: i32, s: i32, color: u32, bg: u32) {
    let (cx, cy, r) = (x + s / 2, y + s / 2, s * 2 / 5);
    c.fill_circle(cx, cy, r, color);
    c.fill_circle(cx, cy, r - 2, bg);
    c.fill_circle(cx + r * 2 / 3, cy - r / 2, r * 4 / 5, bg);
}

pub fn draw_sun(c: &mut Canvas, x: i32, y: i32, s: i32, color: u32) {
    let (cx, cy) = (x + s / 2, y + s / 2);
    let r = s / 5;
    c.fill_circle(cx, cy, r, color);
    let (a, b) = (s * 3 / 10, s * 9 / 20);
    // Eight rays: the diagonals at 0.707 ~ 7/10.
    for (ux, uy) in [(10, 0), (-10, 0), (0, 10), (0, -10), (7, 7), (-7, 7), (7, -7), (-7, -7)] {
        c.line(cx + a * ux / 10, cy + a * uy / 10, cx + b * ux / 10, cy + b * uy / 10, 2, color);
    }
}

/// A battery lying down, `w` wide (and w/2 high), filled to `pct`: red when
/// low and not charging, with a lightning bolt while on mains power.
pub fn draw_battery(c: &mut Canvas, x: i32, y: i32, w: i32, pct: u8, charging: bool, color: u32) {
    let h = w / 2;
    let body = Rect::new(x, y, w - 3, h);
    c.stroke_round(body, 4, 2, (color & 0xFF_FFFF) | 0x9900_0000);
    c.fill_round(Rect::new(x + w - 3, y + h / 2 - 3, 3, 6), 1, (color & 0xFF_FFFF) | 0x9900_0000);
    let inner = body.w - 6;
    let fill = (inner * pct.min(100) as i32 / 100).max(2);
    let level = if pct <= 10 && !charging { 0xFFFF_5A52 } else { color };
    c.fill_round(Rect::new(x + 3, y + 3, fill, h - 6), 2, level);
    if charging {
        // A bolt across the middle, with a ring of the bar's colour taken
        // out around it so it reads over the fill.
        let (cx, cy) = (x + body.w / 2, y + h / 2);
        let bolt = [(cx + 2, cy - h / 2 + 1), (cx - 3, cy + 1), (cx + 2, cy - 1), (cx - 2, cy + h / 2 - 1)];
        for i in 0..3 {
            c.line(bolt[i].0, bolt[i].1, bolt[i + 1].0, bolt[i + 1].1, 3, 0xFF00_0000 | (!color & 0xFF_FFFF));
        }
        for i in 0..3 {
            c.line(bolt[i].0, bolt[i].1, bolt[i + 1].0, bolt[i + 1].1, 1, 0xFFFF_D84D);
        }
    }
}

/// Three round dark buttons on a light pill: minimise, maximise, close
/// (`hover`: the one under the pointer). `r` is the pill.
pub fn window_knobs(c: &mut Canvas, r: Rect, buttons: &[Rect; 3], hover: Option<usize>) {
    c.fill_round(r, r.h / 2, 0x33FF_FFFF);
    for (i, (b, glyph_name)) in buttons.iter().zip(["g-down", "g-max", "g-close"]).enumerate() {
        let bg = match (i, hover == Some(i)) {
            (2, true) => RED,
            (_, true) => 0xFF5A_5860,
            _ => KNOB,
        };
        c.fill_circle(b.x + b.w / 2, b.y + b.h / 2, b.w / 2, bg);
        let g = b.w * 6 / 11;
        super::res::draw_icon(c, glyph_name, g, b.x + (b.w - g) / 2, b.y + (b.h - g) / 2);
    }
}

/// The area a panel's shadow reaches.
pub fn with_shadow(r: Rect) -> Rect {
    r.grow(20).offset(0, 6)
}

/// A label in small spaced capitals ("OGGI", "CERCA NEL WEB").
pub fn caps(c: &mut Canvas, x: i32, y: i32, label: &str, color: u32) -> i32 {
    let mut cx = x;
    for ch in label.chars().flat_map(|ch| ch.to_uppercase()) {
        let mut buf = [0u8; 4];
        cx += text::draw(c, Font::Caps, cx, y, ch.encode_utf8(&mut buf), color) + 2;
    }
    cx - x
}

/// Width of [`caps`] text.
pub fn caps_width(label: &str) -> i32 {
    label.chars().flat_map(|ch| ch.to_uppercase()).map(|ch| text::advance(Font::Caps, ch) + 2).sum()
}

/// A pill-shaped button; `primary` is white with dark text.
pub fn button(c: &mut Canvas, r: Rect, label: &str, primary: bool, hover: bool) {
    if primary {
        c.shadow(r, r.h / 2, 8, if hover { 70 } else { 45 });
        c.fill_round(r, r.h / 2, if hover { 0xFFFF_FFFF } else { 0xF5FF_FFFF });
    } else {
        // The translucent grey pill of the JustOS design.
        c.fill_round(r, r.h / 2, if hover { 0x40FF_FFFF } else { 0x29FF_FFFF });
        c.stroke_round(r, r.h / 2, 1, 0x1FFF_FFFF);
    }
    let fg = if primary { 0xFF0E_0B0C } else { TEXT };
    let (lh, _) = text::metrics(Font::Strong);
    text::draw_centered(c, Font::Strong, r.x + r.w / 2, r.y + (r.h - lh) / 2, label, fg);
}

/// A button in a colour of its own.
pub fn button_color(c: &mut Canvas, r: Rect, label: &str, bg: u32, fg: u32) {
    c.fill_round(r, r.h / 2, bg);
    let (lh, _) = text::metrics(Font::Strong);
    text::draw_centered(c, Font::Strong, r.x + r.w / 2, r.y + (r.h - lh) / 2, label, fg);
}

/// A round button with a white glyph from the icon set.
pub fn icon_button(c: &mut Canvas, r: Rect, glyph: &str, hover: bool) {
    c.fill_circle(r.x + r.w / 2, r.y + r.h / 2, r.w / 2, if hover { 0x2EFF_FFFF } else { 0x12FF_FFFF });
    c.stroke_round(r, r.w / 2, 1, 0x26FF_FFFF);
    let s = r.w * 5 / 9;
    super::res::draw_icon(c, glyph, s, r.x + (r.w - s) / 2, r.y + (r.h - s) / 2);
}

/// An on/off switch (52 x 30 looks right).
pub fn toggle(c: &mut Canvas, r: Rect, on: bool) {
    c.fill_round(r, r.h / 2, if on { 0xFFFF_FFFF } else { 0x33FF_FFFF });
    let d = r.h - 6;
    let x = if on { r.right() - 3 - d } else { r.x + 3 };
    c.fill_circle(x + d / 2, r.y + 3 + d / 2, d / 2 + 1, 0x40_00_00_00);
    c.fill_circle(x + d / 2, r.y + 3 + d / 2, d / 2, if on { 0xFF1A_1414 } else { 0xFFFF_FFFF });
}

/// A horizontal slider; `value` in 0..=1000.
pub fn slider(c: &mut Canvas, r: Rect, value: u32) {
    let track = Rect::new(r.x, r.y + r.h / 2 - 3, r.w, 6);
    c.fill_round(track, 3, 0x40FF_FFFF);
    let x = r.x + (r.w as i64 * value.min(1000) as i64 / 1000) as i32;
    c.fill_round(Rect::new(track.x, track.y, (x - r.x).max(6), 6), 3, 0xFFFF_FFFF);
    c.fill_circle(x, r.y + r.h / 2, 10, 0x50_00_00_00);
    c.fill_circle(x, r.y + r.h / 2, 9, 0xFFFF_FFFF);
}

/// The slider value (0..=1000) for a pointer at `x`.
pub fn slider_value(r: Rect, x: i32) -> u32 {
    ((x - r.x).clamp(0, r.w.max(1)) as i64 * 1000 / r.w.max(1) as i64) as u32
}

/// The parts of a segmented control.
pub fn segments(r: Rect, n: usize) -> alloc::vec::Vec<Rect> {
    let n = n.max(1) as i32;
    let w = (r.w - 4) / n;
    (0..n).map(|i| Rect::new(r.x + 2 + i * w, r.y + 2, w, r.h - 4)).collect()
}

/// Tabs in a pill: `labels`, with `sel` highlighted.
pub fn segmented(c: &mut Canvas, r: Rect, labels: &[&str], sel: usize) {
    // ticly's "Mese / Settimana / Giorno": a dark pill, the choice in white.
    c.fill_round(r, r.h / 2, 0x99_0E0A0A);
    c.stroke_round(r, r.h / 2, 1, BORDER);
    let (lh, _) = text::metrics(Font::Strong);
    for (i, (s, label)) in segments(r, labels.len()).iter().zip(labels).enumerate() {
        if i == sel {
            c.fill_round(*s, s.h / 2, 0xFFFF_FFFF);
        }
        let color = if i == sel { 0xFF0E_0B0C } else { TEXT };
        text::draw_centered(c, Font::Strong, s.x + s.w / 2, s.y + (s.h - lh) / 2, label, color);
    }
}

/// A search field with its magnifier.
pub fn search_field(c: &mut Canvas, r: Rect, value: &str, placeholder: &str, focused: bool) {
    let rad = (r.h / 2).min(12);
    c.fill_round(r, rad, FIELD);
    c.stroke_round(r, rad, 1, if focused { 0x59FF_FFFF } else { BORDER });
    super::res::draw_icon(c, "g-search", 14, r.x + 12, r.y + (r.h - 14) / 2);
    let (lh, _) = text::metrics(Font::Body);
    let ty = r.y + (r.h - lh) / 2;
    if value.is_empty() {
        text::draw(c, Font::Body, r.x + 34, ty, placeholder, TEXT_FAINT);
        if focused {
            c.fill(Rect::new(r.x + 34, ty + 2, 2, lh - 4), ACCENT);
        }
    } else {
        let shown = text::fit(Font::Body, value, r.w - 50);
        let w = text::draw(c, Font::Body, r.x + 34, ty, &shown, TEXT);
        if focused {
            c.fill(Rect::new(r.x + 35 + w, ty + 2, 2, lh - 4), ACCENT);
        }
    }
}

/// A one-line text field.
pub fn text_field(c: &mut Canvas, r: Rect, value: &str, placeholder: &str, focused: bool) {
    c.fill_round(r, 14, FIELD);
    c.stroke_round(r, 14, 1, if focused { 0x59FF_FFFF } else { BORDER });
    let (lh, _) = text::metrics(Font::Heading);
    let ty = r.y + (r.h - lh) / 2;
    let shown = text::fit(Font::Heading, value, r.w - 36);
    let w = if value.is_empty() {
        text::draw(c, Font::Heading, r.x + 16, ty, placeholder, TEXT_FAINT);
        0
    } else {
        text::draw(c, Font::Heading, r.x + 16, ty, &shown, TEXT)
    };
    if focused {
        c.fill(Rect::new(r.x + 17 + w, ty + 2, 2, lh - 4), ACCENT);
    }
}

/// Applies a key to a text value; returns whether it changed.
pub fn edit_text(value: &mut alloc::string::String, ev: &KeyEvent, max: usize) -> bool {
    use crate::dev::input::Key;
    match ev.key {
        Key::Backspace => value.pop().is_some(),
        Key::Char(ch) if !ev.ctrl && !ch.is_control() && value.chars().count() < max => {
            value.push(ch);
            true
        }
        _ => false,
    }
}

/// What the event loop hands to a screen.
#[derive(Clone, Copy, Debug)]
pub enum Event {
    /// The left button went down (`double`: the second click of a pair).
    Press {
        x: i32,
        y: i32,
        double: bool,
    },
    /// The right button went down.
    Menu {
        x: i32,
        y: i32,
    },
    /// The left button went up.
    Release,
    /// The pointer moved (with the left button `held` or not).
    Move {
        x: i32,
        y: i32,
        held: bool,
    },
    /// The wheel turned (positive: scroll down).
    Wheel {
        lines: i32,
    },
    Key(KeyEvent),
    /// About ten times a second, for clocks and blinking.
    Tick,
    /// As often as the screen can be drawn, while the view animates.
    Frame,
}

/// A screen the loop drives: it handles events and draws damaged areas.
pub trait View {
    /// Returns true to leave the loop.
    fn event(&mut self, l: &mut Looper, ev: Event) -> bool;
    /// Draws everything inside `r` (the canvas is clipped to it).
    fn draw(&mut self, c: &mut Canvas, r: Rect);
    /// The screen changed size.
    fn resized(&mut self, _w: i32, _h: i32) {}
    /// Is something moving? The loop then sends [`Event::Frame`] for every
    /// frame instead of resting between events.
    fn animating(&self) -> bool {
        false
    }
}

// ----------------------------------------------------------------------------
// Animation

/// A transition that runs for a fixed time, whatever the frame rate: each
/// frame asks how far along it is.
#[derive(Clone, Copy, Debug)]
pub struct Anim {
    start: u64,
    ms: u64,
}

impl Anim {
    pub fn new(ms: u64) -> Anim {
        Anim { start: crate::time::uptime_ms(), ms: ms.max(1) }
    }

    /// Progress, 0..=1000.
    pub fn t(&self) -> u32 {
        let el = crate::time::uptime_ms().saturating_sub(self.start);
        (el.min(self.ms) * 1000 / self.ms) as u32
    }

    pub fn done(&self) -> bool {
        self.t() >= 1000
    }

    /// Eased progress (fast start, gentle stop), 0..=1000.
    pub fn out(&self) -> u32 {
        ease_out(self.t())
    }

    pub fn in_out(&self) -> u32 {
        ease_in_out(self.t())
    }

    /// Milliseconds since it started.
    pub fn elapsed(&self) -> u64 {
        crate::time::uptime_ms().saturating_sub(self.start)
    }
}

/// Cubic ease-out: 1 - (1 - t)^3, in 1/1000.
pub fn ease_out(t: u32) -> u32 {
    let u = 1000 - t.min(1000) as u64;
    (1000 - u * u * u / 1_000_000) as u32
}

/// Cubic ease-in-out, in 1/1000.
pub fn ease_in_out(t: u32) -> u32 {
    let t = t.min(1000) as u64;
    if t < 500 {
        (4 * t * t * t / 1_000_000) as u32
    } else {
        let u = 1000 - t;
        (1000 - 4 * u * u * u / 1_000_000) as u32
    }
}

/// `a` to `b` at `t` (0..=1000).
pub fn lerp(a: i32, b: i32, t: u32) -> i32 {
    a + ((b - a) as i64 * t as i64 / 1000) as i32
}

pub fn lerp_rect(a: Rect, b: Rect, t: u32) -> Rect {
    Rect::new(lerp(a.x, b.x, t), lerp(a.y, b.y, t), lerp(a.w, b.w, t), lerp(a.h, b.h, t))
}

/// `r` scaled by `permille` around its centre.
pub fn scale_rect(r: Rect, permille: i32) -> Rect {
    let w = (r.w as i64 * permille as i64 / 1000) as i32;
    let h = (r.h as i64 * permille as i64 / 1000) as i32;
    Rect::new(r.x + (r.w - w) / 2, r.y + (r.h - h) / 2, w, h)
}

/// A value that glides towards a target (hover highlights, the dock's
/// magnification): call [`step`](Self::step) every frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct Glide {
    pub value: i32,
    pub target: i32,
    last: u64,
}

impl Glide {
    pub fn new(v: i32) -> Glide {
        Glide { value: v, target: v, last: 0 }
    }

    /// Moves a share of the way to the target (about 90% in 150 ms);
    /// returns whether it is still moving.
    pub fn step(&mut self) -> bool {
        let now = crate::time::uptime_ms();
        let dt = if self.last == 0 { 16 } else { now.saturating_sub(self.last).min(100) } as i64;
        self.last = now;
        if self.value == self.target {
            self.last = 0;
            return false;
        }
        let diff = (self.target - self.value) as i64;
        let mut d = diff * (dt * 15).min(1000) / 1000;
        if d == 0 {
            d = diff.signum();
        }
        self.value += d as i32;
        if (self.target - self.value).abs() <= 2 {
            self.value = self.target;
        }
        true
    }

    pub fn moving(&self) -> bool {
        self.value != self.target
    }
}

// ----------------------------------------------------------------------------
// Clipboard

static CLIPBOARD: crate::sync::IrqLock<alloc::string::String> = crate::sync::IrqLock::new(alloc::string::String::new());

pub fn copy(text: &str) {
    *CLIPBOARD.lock() = alloc::string::String::from(text);
}

pub fn paste() -> alloc::string::String {
    CLIPBOARD.lock().clone()
}

/// Mouse, keyboard and screen updates for one interactive screen.
pub struct Looper {
    pub cursor: Cursor,
    dirty: Rect,
    last_click: (u64, i32, i32),
    last_tick: u64,
}

impl Looper {
    pub fn new(d: &Display) -> Looper {
        let (w, h) = (d.width(), d.height());
        input::set_capture(true);
        input::take_clicks();
        Looper { cursor: Cursor::new(w, h, d.scale()), dirty: Rect::EMPTY, last_click: (0, -100, -100), last_tick: 0 }
    }

    /// Marks part of the screen to be drawn again.
    pub fn damage(&mut self, r: Rect) {
        self.dirty = self.dirty.union(&r);
    }

    pub fn damage_all(&mut self, d: &Display) {
        self.damage(d.scene.bounds());
    }

    /// Waits for the next events and passes each to `handle`; redraws the
    /// damaged area with `draw` after each batch. Returns when `handle`
    /// returns true.
    pub fn run(&mut self, d: &mut Display, view: &mut dyn View) {
        loop {
            if super::take_redraw() {
                d.follow_framebuffer();
                self.damage_all(d);
            }
            if let Some(mode) = super::take_mode_request() {
                self.cursor.hide(d);
                match d.set_mode(mode) {
                    Ok(()) => {
                        self.cursor.set_scale(d.scale());
                        view.resized(d.width(), d.height());
                        self.damage_all(d);
                    }
                    Err(e) => crate::warn!("gui", "{}x{}: {e}", mode.width, mode.height),
                }
            }
            if super::take_scale_request() {
                self.cursor.hide(d);
                if d.apply_scale() {
                    self.cursor.set_scale(d.scale());
                    view.resized(d.width(), d.height());
                }
                self.damage_all(d);
            }
            let mut events: alloc::vec::Vec<Event> = alloc::vec::Vec::new();
            let (w, h) = (d.width(), d.height());
            let moved = self.cursor.poll(w, h);
            let (x, y) = (self.cursor.x, self.cursor.y);
            let b = self.cursor.buttons;
            if moved {
                events.push(Event::Move { x, y, held: b & 1 != 0 });
            }
            // Clicks in the order they happened, even if quicker than a frame.
            for (button, down, rx, ry) in input::take_clicks() {
                let (cx, cy) = self.cursor.click_at(rx, ry, w, h);
                match (button, down) {
                    (0, true) => {
                        let now = crate::time::uptime_ms();
                        let (t, px, py) = self.last_click;
                        let double = now - t < 450 && (px - cx).abs() < 6 && (py - cy).abs() < 6;
                        self.last_click = if double { (0, -100, -100) } else { (now, cx, cy) };
                        events.push(Event::Press { x: cx, y: cy, double });
                    }
                    (0, false) => events.push(Event::Release),
                    (1, true) => events.push(Event::Menu { x: cx, y: cy }),
                    _ => {}
                }
            }
            if self.cursor.scrolled != 0 {
                events.push(Event::Wheel { lines: self.cursor.scrolled });
            }
            while let Some(k) = input::gui_key() {
                events.push(Event::Key(k));
            }
            let now = crate::time::uptime_ms();
            if now - self.last_tick >= 100 {
                self.last_tick = now;
                events.push(Event::Tick);
            }
            let animating = view.animating();
            if animating {
                events.push(Event::Frame);
            }
            for ev in events {
                if view.event(self, ev) {
                    self.flush(d, view);
                    return;
                }
            }
            if moved {
                self.cursor.hide(d);
            }
            self.flush(d, view);
            if moved {
                self.cursor.show(d);
            }
            crate::task::sleep_ms(if animating { 2 } else { 8 });
        }
    }

    fn flush(&mut self, d: &mut Display, view: &mut dyn View) {
        let r = self.dirty.intersect(&d.scene.bounds());
        self.dirty = Rect::EMPTY;
        if r.is_empty() {
            return;
        }
        d.scene.set_clip(r);
        view.draw(&mut d.scene, r);
        d.scene.reset_clip();
        d.present(r);
        if r.overlaps(&self.cursor.rect()) {
            self.cursor.show(d);
        }
    }
}

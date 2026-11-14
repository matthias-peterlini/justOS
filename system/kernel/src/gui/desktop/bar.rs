//! The dynamic bar: the dock changes shape to show what is going on, and
//! always goes back to the dock at rest.
//!
//! - A program starting, a song playing, a timer: the app's icon in the
//!   dock widens into a capsule (a well with the icon's tile, smaller, a
//!   name with a line under it, and cells that move by themselves). Only
//!   the most recent one is open; the one before waits in a round bubble
//!   beside the dock (with how many wait there, when more than one). A
//!   click on the bubble swaps them; a click on the capsule opens its app.
//! - The volume or the brightness changing: for a moment the whole bar is
//!   a row of 16 cells with the value in soft pixels.
//! - A short notification: the bar grows upwards by a row with it and its
//!   buttons, and closes again by itself. Long ones keep their banners.
//! - Over a full-screen window, where the dock hides, a thin pill at the
//!   bottom shows the time and what goes on.
//!
//! Every change of shape is a [`Morph`] of [`MORPH_MS`]. What the bar
//! painted is remembered (`Desktop::bar_painted`), so that after a bigger
//! shape a smaller one clears all of it.

use super::*;
use crate::gui::wm::Live;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

/// How long a change of shape takes.
const MORPH_MS: u64 = 250;

// The capsule an icon widens into: how far its well reaches past the icon,
// how much smaller the icon's tile gets in it, its corner, the room at its
// right end, the narrowest and widest its text may be, and the room
// between the text and the cells.
const CAP_GROW: i32 = 4;
const CAP_TILE: i32 = 14;
const CAP_RAD: i32 = 18;
const CAP_END: i32 = 14;
const CAP_TEXT_MIN: i32 = 48;
const CAP_TEXT_MAX: i32 = 200;
const CAP_GAP: i32 = 12;
/// A program starting: a row of cells, three of them lit, going round.
const BUSY: i32 = 10;
const BUSY_W: i32 = 4;
const BUSY_H: i32 = 12;
const BUSY_GAP: i32 = 2;
/// Music: the five bars of an equaliser, in hundredths of their height,
/// one row per step.
const EQ: [[u8; 5]; 6] =
    [[45, 100, 65, 85, 35], [70, 55, 90, 40, 60], [30, 80, 50, 100, 45], [85, 40, 75, 55, 90], [55, 95, 35, 70, 50], [40, 60, 100, 45, 75]];
const EQ_W: i32 = 4;
const EQ_GAP: i32 = 3;
const EQ_H: i32 = 18;
const EQ_TOTAL: i32 = 5 * EQ_W + 4 * EQ_GAP;
/// The cells of a timer's soft-pixel digits (thousandths of a unit).
const TIMER_CELL: i32 = 2600;
/// How often the cells that move by themselves take a step (ms): eight
/// times a second.
const STEP_MS: u64 = 125;

// The level (volume, brightness): its height and corner, the room at its
// ends and between its parts, its 16 cells, the cells of its symbol and
// digits, how long it stays after the last change, how far into the morph
// the dock's content gives way to it, and a volume key's step.
const LEVEL_H: i32 = 64;
const LEVEL_RAD: i32 = 26;
const LEVEL_PAD: i32 = 22;
const LEVEL_GAP: i32 = 16;
const LEVEL_CELLS: i32 = 16;
const LEVEL_CELL_W: i32 = 13;
const LEVEL_CELL_H: i32 = 26;
const LEVEL_CELL_GAP: i32 = 4;
const LEVEL_SYM: i32 = 4000;
const LEVEL_STAY: u64 = 1500;
const LEVEL_SWAP: u32 = 450;
const VOLUME_STEP: u32 = 6;
/// The volume before the mute key, to go back to.
static UNMUTE: AtomicU32 = AtomicU32::new(50);

// A short notification: its row above the dock, how much the bar grows for
// it, and what is short.
const NOTE_ROW: i32 = 52;
const NOTE_LIFT: i32 = 60;
const SHORT_TITLE: usize = 40;
const SHORT_BODY: usize = 60;

/// The room between the dock and the bubble.
const BUBBLE_GAP: i32 = 12;

// The pill over a full-screen window: its height, the cells of its digits,
// the room at its ends, and the cells of a program starting in it.
const PILL_H: i32 = 28;
const PILL_CELL: i32 = 2200;
const PILL_PAD: i32 = 14;
const PILL_BUSY: i32 = 5;

/// A change of shape: from one value (0..=1000) to another in
/// [`MORPH_MS`], eased out.
#[derive(Clone, Copy)]
pub(super) struct Morph {
    from: u32,
    to: u32,
    anim: Anim,
}

impl Morph {
    pub(super) fn new() -> Morph {
        Morph { from: 0, to: 0, anim: Anim::new(1) }
    }

    pub(super) fn value(&self) -> u32 {
        ui::lerp(self.from as i32, self.to as i32, self.anim.out()) as u32
    }

    /// Towards `to` from where it is now; whether that is a change.
    fn set(&mut self, to: u32) -> bool {
        if to == self.to {
            return false;
        }
        self.from = self.value();
        self.to = to;
        self.anim = Anim::new(MORPH_MS);
        true
    }

    /// Still changing (and a little after, for a slow frame).
    fn moving(&self) -> bool {
        self.from != self.to && self.anim.elapsed() < MORPH_MS + 40
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ActKind {
    Starting,
    Music,
    Timer,
    Volume,
    Brightness,
    Note,
}

impl ActKind {
    /// Shown as a capsule in the dock.
    fn capsule(self) -> bool {
        matches!(self, ActKind::Starting | ActKind::Music | ActKind::Timer)
    }

    fn level(self) -> bool {
        matches!(self, ActKind::Volume | ActKind::Brightness)
    }
}

/// Something going on that the bar shows.
pub(super) struct Activity {
    kind: ActKind,
    /// The window kind of the dock item its capsule grows out of.
    key: String,
    icon: String,
    /// A program's or a song's name, a timer's time left, a notification's
    /// title; and the line under it (a timer's name).
    title: String,
    sub: String,
    /// A timer at zero.
    ringing: bool,
    /// The level shown, 0..=100.
    value: u32,
    /// The app a notification opens ("Apri").
    app: Option<&'static str>,
    /// When it last came forward (uptime): the most recent has the capsule.
    front: u64,
    /// When it goes by itself (uptime; 0: while it lasts).
    until: u64,
    /// How far it is out.
    open: Morph,
    /// Going: it closes, then it is dropped.
    leaving: bool,
}

impl Activity {
    fn new(kind: ActKind, key: String, icon: String, title: String, sub: String, now: u64) -> Activity {
        Activity { kind, key, icon, title, sub, ringing: false, value: 0, app: None, front: now, until: 0, open: Morph::new(), leaving: false }
    }
}

/// The parts of the bar that take a press besides the dock's icons.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum BarHit {
    Close,
    Open,
    Note,
    Bubble,
    Pill,
    Level,
}

/// `col` with its opacity at `t` (0..=1000) of what it is.
fn fade(col: u32, t: u32) -> u32 {
    ((((col >> 24) * t.min(1000)) / 1000) << 24) | (col & 0xFF_FFFF)
}

/// The width of `s` in the clock's soft-pixel digits with cells `cell`.
fn digits_w(s: &str, cell: i32) -> i32 {
    clock_rows(s)[0].len() as i32 * cell / 1000
}

/// `s` in soft-pixel digits from `x`, in the middle of `r` from top to bottom.
fn draw_digits(c: &mut Canvas, s: &str, x: i32, r: Rect, cell: i32, col: u32) {
    let rows = clock_rows(s);
    let rows: Vec<&str> = rows.iter().map(|r| r.as_str()).collect();
    ui::soft_pixels_at(c, &rows, x * 1000, r.y * 1000 + (r.h * 1000 - 5 * cell) / 2, cell, &|_| col);
}

fn busy_w(n: i32) -> i32 {
    n * BUSY_W + (n - 1) * BUSY_GAP
}

/// The step of the cells that move by themselves.
fn step() -> u64 {
    time::uptime_ms() / STEP_MS
}

/// `n` cells `h` high from the left of `r`, three lit going round (a
/// program starting: how far it is cannot be known).
fn draw_busy(c: &mut Canvas, r: Rect, n: i32, h: i32, t: u32) {
    let y = r.y + (r.h - h) / 2;
    let p = (step() % (n as u64 + 3)) as i32;
    for k in 0..n {
        let cell = Rect::new(r.x + k * (BUSY_W + BUSY_GAP), y, BUSY_W, h);
        if (p - 3..p).contains(&k) {
            c.fill_round_grad(cell, 2, fade(ui::ORANGE_GRAD.0, t), fade(ui::ORANGE_GRAD.1, t));
        } else {
            c.fill_round(cell, 2, fade(ui::cell(), t));
        }
    }
}

/// The equaliser's five bars, at most `h` high, from the left of `r`.
fn draw_eq(c: &mut Canvas, r: Rect, h: i32, t: u32) {
    let bars = &EQ[(step() % EQ.len() as u64) as usize];
    let bottom = r.y + (r.h + h) / 2;
    for (k, &pct) in bars.iter().enumerate() {
        let bh = (h * pct as i32 / 100).max(3);
        c.fill_round(Rect::new(r.x + k as i32 * (EQ_W + EQ_GAP), bottom - bh, EQ_W, bh), 1, fade(ui::ORANGE, t));
    }
}

/// The line under a starting program's name.
fn start_line(note: Option<&str>) -> String {
    match note {
        Some(n) if n != apps::STARTING => String::from(n),
        _ => String::from("Si sta avviando…"),
    }
}

/// The text of a capsule: as wide as its longer line (a song's time at
/// its widest, so that the capsule does not change width every second).
fn text_w(a: &Activity) -> i32 {
    let sub: String = if a.kind == ActKind::Music {
        a.sub.chars().map(|ch| if ch.is_ascii_digit() { '0' } else { ch }).collect()
    } else {
        a.sub.clone()
    };
    text::width(Font::Strong, &a.title).max(text::width(Font::Small, &sub)).clamp(CAP_TEXT_MIN, CAP_TEXT_MAX)
}

/// What a capsule shows right of its tile: how wide.
fn content_w(a: &Activity) -> i32 {
    match a.kind {
        ActKind::Starting => text_w(a) + CAP_GAP + busy_w(BUSY),
        ActKind::Music => text_w(a) + CAP_GAP + EQ_TOTAL,
        ActKind::Timer => digits_w(&a.title, TIMER_CELL) + 8 + text::width(Font::Small, &a.sub),
        _ => 0,
    }
}

/// How much wider a dock item gets with its capsule open. The content
/// starts where the icon ended (the tile gets smaller by as much).
fn cap_extra(a: &Activity) -> i32 {
    content_w(a) + CAP_END - CAP_GROW
}

/// A capsule on the dock icon `r` widened by `extra`: its well, and the
/// part that moves by itself (the equaliser, the cells).
fn cap_parts(a: &Activity, r: Rect, extra: i32) -> (Rect, Rect) {
    let g = ui::lerp(0, CAP_GROW, a.open.value());
    let well = Rect::new(r.x - g, r.y - g, r.w + extra + 2 * g, r.h + 2 * g);
    let x = r.x + r.w + text_w(a) + CAP_GAP;
    let tail = match a.kind {
        ActKind::Music => Rect::new(x, well.y, EQ_TOTAL, well.h),
        ActKind::Starting => Rect::new(x, well.y, busy_w(BUSY), well.h),
        _ => Rect::EMPTY,
    };
    (well, tail)
}

/// The width of what the pill shows after the time.
fn pill_ind_w(a: &Activity) -> i32 {
    match a.kind {
        ActKind::Music => EQ_TOTAL,
        ActKind::Starting => busy_w(PILL_BUSY),
        _ => digits_w(&a.title, PILL_CELL),
    }
}

fn level_w() -> i32 {
    2 * LEVEL_PAD + 7 * LEVEL_SYM / 1000 + 2 * LEVEL_GAP + cells_w() + digits_w("100", LEVEL_SYM)
}

fn cells_w() -> i32 {
    LEVEL_CELLS * LEVEL_CELL_W + (LEVEL_CELLS - 1) * LEVEL_CELL_GAP
}

/// The built-in app a notification with `icon` comes from, which "Apri"
/// opens (none for the system's own).
fn note_app(icon: &str) -> Option<&'static str> {
    apps::BUILT_IN
        .iter()
        .find(|(id, _, ic)| (*ic == icon || *id == icon) && !matches!(*id, "about" | "trash" | "viewer"))
        .map(|(id, _, _)| *id)
}

impl Desktop {
    // ------------------------------------------------------------------
    // What is where

    /// The activity with the capsule: the most recent of those that have one.
    fn primary(&self) -> Option<usize> {
        self.acts.iter().enumerate().filter(|(_, a)| a.kind.capsule() && !a.leaving).max_by_key(|(_, a)| a.front).map(|(i, _)| i)
    }

    /// The others, waiting in the bubble, the most recent first.
    fn waiting(&self) -> Vec<usize> {
        let p = self.primary();
        let mut v: Vec<usize> = (0..self.acts.len()).filter(|&i| Some(i) != p && self.acts[i].kind.capsule() && !self.acts[i].leaving).collect();
        v.sort_by_key(|&i| core::cmp::Reverse(self.acts[i].front));
        v
    }

    fn level_act(&self) -> Option<&Activity> {
        self.acts.iter().find(|a| a.kind.level())
    }

    fn note_act(&self) -> Option<&Activity> {
        self.acts.iter().find(|a| a.kind == ActKind::Note)
    }

    /// How far the bar has become the level (0: not at all).
    pub(super) fn level_t(&self) -> u32 {
        self.level_act().map_or(0, |a| a.open.value())
    }

    /// How far the bar has grown for a notification.
    pub(super) fn note_t(&self) -> u32 {
        if self.dock_shown() {
            self.note_act().map_or(0, |a| a.open.value())
        } else {
            0
        }
    }

    /// The dock is there to be pressed (not turned into the level).
    pub(super) fn dock_live(&self) -> bool {
        self.dock_shown() && self.level_t() == 0
    }

    /// The capsule out (or coming or going) on the dock item of window kind
    /// `kind`.
    fn cap_index(&self, kind: &str) -> Option<usize> {
        self.acts
            .iter()
            .enumerate()
            .filter(|(_, a)| a.kind.capsule() && a.key == kind && a.open.value() > 0)
            .max_by_key(|(_, a)| a.open.value())
            .map(|(i, _)| i)
    }

    pub(super) fn cap_of(&self, kind: &str) -> Option<&Activity> {
        self.cap_index(kind).map(|i| &self.acts[i])
    }

    /// How much wider each dock item is now for its capsule.
    pub(super) fn cap_extras(&self, items: &[DockItem]) -> Vec<i32> {
        items.iter().map(|it| self.cap_of(&it.kind).map_or(0, |a| ui::lerp(0, cap_extra(a), a.open.value()))).collect()
    }

    /// The dock icons `rects` with their capsules: what a press or the
    /// pointer may be on.
    pub(super) fn wide_rects(&self, items: &[DockItem], rects: &[Rect]) -> Vec<Rect> {
        let extras = self.cap_extras(items);
        rects.iter().zip(&extras).map(|(r, &e)| Rect::new(r.x, r.y, r.w + e, r.h)).collect()
    }

    /// The room the bubble takes beside a dock of icons `icon` wide.
    pub(super) fn bubble_room(&self, icon: i32) -> i32 {
        ui::lerp(0, BUBBLE_GAP + icon + 2 * DOCK_PAD + 4, self.bubble.value())
    }

    /// The bubble, all the way out, beside the dock `bg`.
    fn bubble_rect(&self, bg: Rect) -> Rect {
        Rect::new(bg.right() + BUBBLE_GAP, bg.y, bg.h, bg.h)
    }

    /// The level, at the bottom in the middle.
    fn level_rect(&self) -> Rect {
        let w = level_w();
        Rect::new((self.w - w) / 2, self.h - 12 - LEVEL_H, w, LEVEL_H)
    }

    /// The level's symbol, cells and digits.
    fn level_parts(&self) -> (Rect, Rect, Rect) {
        let r = self.level_rect();
        let sym_w = 7 * LEVEL_SYM / 1000;
        let mut x = r.x + LEVEL_PAD;
        let sym = Rect::new(x, r.y, sym_w, r.h);
        x += sym_w + LEVEL_GAP;
        let cells = Rect::new(x, r.y + (r.h - LEVEL_CELL_H) / 2, cells_w(), LEVEL_CELL_H);
        x += cells_w() + LEVEL_GAP;
        (sym, cells, Rect::new(x, r.y, digits_w("100", LEVEL_SYM), r.h))
    }

    /// The bar's panel around the dock `bg`: grown upwards for a
    /// notification, or on its way to being the level.
    pub(super) fn bar_panel(&self, bg: Rect) -> Rect {
        let lift = NOTE_LIFT * self.note_t() as i32 / 1000;
        let p = Rect::new(bg.x, bg.y - lift, bg.w, bg.h + lift);
        match self.level_t() {
            0 => p,
            t => ui::lerp_rect(p, self.level_rect(), t),
        }
    }

    /// A notification's row, all the way out, above the dock `bg`.
    fn note_row(&self, bg: Rect) -> Rect {
        Rect::new(bg.x + DOCK_PAD, bg.y - NOTE_LIFT + DOCK_PAD, bg.w - 2 * DOCK_PAD, NOTE_ROW)
    }

    /// "Chiudi" and, when there is an app to open, "Apri", at the right end
    /// of the notification's row.
    fn note_buttons(row: Rect, open: bool) -> (Rect, Option<Rect>) {
        let y = row.y + (row.h - 32) / 2;
        let mut x = row.right() - 6;
        let open = open.then(|| {
            x -= 72;
            let r = Rect::new(x, y, 72, 32);
            x -= 8;
            r
        });
        (Rect::new(x - 86, y, 86, 32), open)
    }

    /// The pill over a full-screen window, while something goes on: where,
    /// and what it shows.
    fn pill(&self) -> Option<(Rect, usize)> {
        if self.dock_shown() {
            return None;
        }
        let i = self.primary()?;
        let (clock, _) = clock_strings();
        let w = 2 * PILL_PAD + digits_w(&clock, PILL_CELL) + 21 + pill_ind_w(&self.acts[i]);
        Some((Rect::new((self.w - w) / 2, self.h - 8 - PILL_H, w, PILL_H), i))
    }

    /// What the pill `r` shows after the time and a thin line.
    fn pill_ind(&self, r: Rect) -> Rect {
        let (clock, _) = clock_strings();
        let x = r.x + PILL_PAD + digits_w(&clock, PILL_CELL) + 21;
        Rect::new(x, r.y, r.right() - PILL_PAD - x, r.h)
    }

    /// What the bar may paint besides the dock `d` and its icons.
    pub(super) fn bar_extent(&self, d: Rect) -> Rect {
        let mut z = Rect::EMPTY;
        if self.acts.iter().any(|a| a.kind == ActKind::Note) {
            z = z.union(&ui::with_shadow(Rect::new(d.x, d.y - NOTE_LIFT, d.w, d.h + NOTE_LIFT)));
        }
        if self.acts.iter().any(|a| a.kind.level()) {
            z = z.union(&ui::with_shadow(self.level_rect()));
        }
        if self.bubble.value() > 0 || self.bubble.moving() {
            z = z.union(&ui::with_shadow(self.bubble_rect(d)).grow(6));
        }
        if let Some((r, _)) = self.pill() {
            z = z.union(&ui::with_shadow(r));
        }
        z
    }

    /// The capsules on the screen now: the activity, its well and the part
    /// that moves by itself; over a full-screen window, the pill.
    fn live_rects(&self) -> Vec<(usize, Rect, Rect)> {
        let mut out = Vec::new();
        if self.level_t() > 0 {
            return out;
        }
        if !self.dock_shown() {
            if let Some((r, i)) = self.pill() {
                out.push((i, r, self.pill_ind(r)));
            }
            return out;
        }
        if !self.acts.iter().any(|a| a.kind.capsule()) {
            return out;
        }
        let items = self.dock_items();
        let (_, rects, _) = self.dock_geometry(&items);
        let extras = self.cap_extras(&items);
        for (k, it) in items.iter().enumerate() {
            if let Some(i) = self.cap_index(&it.kind) {
                let (well, tail) = cap_parts(&self.acts[i], rects[k], extras[k]);
                out.push((i, well, tail));
            }
        }
        out
    }

    // ------------------------------------------------------------------
    // Changes

    /// The bar's whole area, and wherever it was painted before.
    pub(super) fn damage_bar(&self, l: &mut Looper) {
        l.damage(self.dock_zone().union(&self.bar_painted.replace(Rect::EMPTY)));
    }

    pub(super) fn bar_moving(&self) -> bool {
        self.bubble.moving() || self.acts.iter().any(|a| a.open.moving())
    }

    /// Where each activity goes: the most recent capsule open, the others
    /// closed (the next of them in the bubble), levels and notifications
    /// out until they leave. Whether anything changed.
    fn bar_targets(&mut self) -> bool {
        let primary = self.primary();
        let waiting = self.waiting();
        let mut changed = false;
        for (i, a) in self.acts.iter_mut().enumerate() {
            let out = !a.leaving && (!a.kind.capsule() || Some(i) == primary);
            changed |= a.open.set(if out { 1000 } else { 0 });
        }
        if let Some(&j) = waiting.first() {
            if self.bubble_icon != self.acts[j].icon {
                self.bubble_icon = self.acts[j].icon.clone();
                changed = true;
            }
        }
        changed | self.bubble.set(if waiting.is_empty() { 0 } else { 1000 })
    }

    /// Ten times a second: what goes on now (programs starting, what the
    /// apps say, the volume and the brightness), and the cells that move.
    pub(super) fn bar_tick(&mut self, l: &mut Looper) {
        let now = time::uptime_ms();
        let (mut shape, mut content) = (false, false);
        let mut wants: Vec<(ActKind, String, String, String, String, bool)> = Vec::new();
        for s in &self.starting {
            wants.push((ActKind::Starting, s.kind.clone(), s.icon.clone(), s.label.clone(), start_line(s.note.as_deref()), false));
        }
        for w in &self.windows {
            match w.app.activity() {
                Some(Live::Music { title, elapsed }) => {
                    wants.push((ActKind::Music, w.app.kind().into(), w.app.icon().into(), title, format!("Musica · {elapsed}"), false))
                }
                Some(Live::Timer { left, label, ringing }) => wants.push((ActKind::Timer, w.app.kind().into(), w.app.icon().into(), left, label, ringing)),
                None => {}
            }
        }
        for (kind, key, icon, title, sub, ringing) in &wants {
            match self.acts.iter_mut().find(|a| a.kind == *kind && a.key == *key) {
                Some(a) => {
                    if a.leaving {
                        // Back before it had gone (a song played again).
                        a.leaving = false;
                        a.front = now;
                        shape = true;
                    }
                    if *ringing && !a.ringing {
                        // A timer that rings comes out of the bubble.
                        a.front = now;
                        shape = true;
                    }
                    if a.title != *title || a.sub != *sub || a.ringing != *ringing {
                        let before = content_w(a);
                        a.title.clone_from(title);
                        a.sub.clone_from(sub);
                        a.ringing = *ringing;
                        if content_w(a) == before {
                            content = true;
                        } else {
                            shape = true;
                        }
                    }
                }
                None => {
                    let mut a = Activity::new(*kind, key.clone(), icon.clone(), title.clone(), sub.clone(), now);
                    a.ringing = *ringing;
                    self.acts.push(a);
                    shape = true;
                }
            }
        }
        for a in self.acts.iter_mut().filter(|a| a.kind.capsule() && !a.leaving) {
            if !wants.iter().any(|w| w.0 == a.kind && w.1 == a.key) {
                a.leaving = true;
                shape = true;
            }
        }
        // Levels and notifications go after a while; a notification stays
        // while the pointer is on it.
        let on_note = self.note_t() > 0 && {
            let (_, _, bg) = self.dock_geometry(&self.dock_items());
            self.note_row(bg).contains(self.mouse.0, self.mouse.1)
        };
        for a in self.acts.iter_mut().filter(|a| a.until != 0 && !a.leaving && now >= a.until) {
            if a.kind == ActKind::Note && on_note {
                a.until = now + 1000;
            } else {
                a.leaving = true;
                shape = true;
            }
        }
        // The volume or the brightness changed (the keys, the settings, an app).
        let levels = (crate::dev::audio::volume(), crate::gui::brightness());
        if let Some((vol, bri)) = self.levels {
            if vol != levels.0 {
                self.show_level(l, ActKind::Volume, levels.0);
            } else if bri != levels.1 {
                self.show_level(l, ActKind::Brightness, levels.1);
            }
        }
        self.levels = Some(levels);
        shape |= self.bar_targets();
        self.acts.retain(|a| !(a.leaving && a.open.value() == 0 && !a.open.moving()));
        let step = step();
        let stepped = step != self.bar_phase;
        self.bar_phase = step;
        if shape {
            self.damage_bar(l);
            return;
        }
        // Only what changed inside the capsules: the equaliser's bars, the
        // cells, a timer's digits.
        for (i, well, tail) in self.live_rects() {
            let a = &self.acts[i];
            if content || a.ringing {
                l.damage(well.grow(2));
            } else if stepped && matches!(a.kind, ActKind::Music | ActKind::Starting) {
                l.damage(tail);
            }
        }
    }

    /// The volume or the brightness changed: the bar shows it for a moment
    /// (not while the control centre, with its own sliders, is open).
    fn show_level(&mut self, l: &mut Looper, kind: ActKind, value: u32) {
        let control = self.popup.as_ref().is_some_and(|p| p.kind == PopupKind::Control);
        if control || self.lock.is_some() || self.ending.is_some() {
            return;
        }
        let now = time::uptime_ms();
        let until = now + MORPH_MS + LEVEL_STAY;
        match self.acts.iter_mut().find(|a| a.kind.level()) {
            Some(a) => {
                a.kind = kind;
                a.value = value;
                a.until = until;
                a.leaving = false;
            }
            None => {
                let mut a = Activity::new(kind, String::new(), String::new(), String::new(), String::new(), now);
                a.value = value;
                a.until = until;
                self.acts.push(a);
            }
        }
        self.mag.target = 0;
        self.hover = None;
        self.bar_targets();
        self.damage_bar(l);
    }

    /// A volume key: up, down, or mute and back.
    pub(super) fn volume_key(&mut self, l: &mut Looper, key: Key) {
        let v = crate::dev::audio::volume();
        let next = match key {
            Key::VolumeUp => (v + VOLUME_STEP).min(100),
            Key::VolumeDown => v.saturating_sub(VOLUME_STEP),
            _ if v > 0 => {
                UNMUTE.store(v, Ordering::Relaxed);
                0
            }
            _ => UNMUTE.load(Ordering::Relaxed).max(VOLUME_STEP),
        };
        crate::gui::change_volume(next);
        let now = (crate::dev::audio::volume(), crate::gui::brightness());
        self.levels = Some(now);
        self.show_level(l, ActKind::Volume, now.0);
    }

    /// A press on the level's cells sets it there.
    fn set_level_at(&mut self, l: &mut Looper, x: i32) {
        let Some(kind) = self.level_act().map(|a| a.kind) else { return };
        let (_, cells, _) = self.level_parts();
        let v = ((ui::slider_value(cells, x) + 5) / 10).min(100);
        if kind == ActKind::Brightness {
            crate::gui::set_brightness(v.max(20));
            l.damage(self.screen());
        } else {
            crate::gui::change_volume(v);
        }
        let now = (crate::dev::audio::volume(), crate::gui::brightness());
        self.levels = Some(now);
        self.show_level(l, kind, if kind == ActKind::Brightness { now.1 } else { now.0 });
    }

    /// A short notification comes out of the bar, when the dock is there
    /// to show it; whether it did (if not, it gets a banner).
    pub(super) fn bar_note(&mut self, l: &mut Looper, title: &str, body: &str, icon: &str) -> bool {
        let busy = self.launcher.is_some() || self.overview.is_some() || self.lock.is_some() || self.popup.is_some() || self.ending.is_some();
        if busy || self.full_screen() || title.chars().count() > SHORT_TITLE || body.chars().count() > SHORT_BODY {
            return false;
        }
        let now = time::uptime_ms();
        let app = note_app(icon);
        let until = now + MORPH_MS + NOTE_STAY;
        match self.acts.iter_mut().find(|a| a.kind == ActKind::Note) {
            Some(a) => {
                a.title = title.into();
                a.sub = body.into();
                a.icon = icon.into();
                a.app = app;
                a.until = until;
                a.leaving = false;
            }
            None => {
                let mut a = Activity::new(ActKind::Note, String::new(), icon.into(), title.into(), body.into(), now);
                a.app = app;
                a.until = until;
                self.acts.push(a);
            }
        }
        self.mag.target = 0;
        self.hover = None;
        self.bar_targets();
        self.damage_bar(l);
        true
    }

    fn dismiss_note(&mut self, l: &mut Looper) {
        for a in self.acts.iter_mut().filter(|a| a.kind == ActKind::Note) {
            a.leaving = true;
        }
        self.bar_targets();
        self.damage_bar(l);
    }

    /// A program starting from `kind` was launched again: its capsule
    /// comes to the front.
    pub(super) fn bar_front(&mut self, l: &mut Looper, kind: &str) {
        let now = time::uptime_ms();
        for a in self.acts.iter_mut().filter(|a| a.kind == ActKind::Starting && a.key == kind) {
            a.front = now;
        }
        if self.bar_targets() {
            self.damage_bar(l);
        }
    }

    // ------------------------------------------------------------------
    // Input

    /// The part of the bar at (`x`, `y`) besides the dock's icons.
    pub(super) fn bar_hit(&self, x: i32, y: i32) -> Option<BarHit> {
        if self.acts.is_empty() && self.bubble.value() == 0 {
            return None;
        }
        if !self.dock_shown() {
            if self.level_t() > 0 && self.level_rect().contains(x, y) {
                return Some(BarHit::Level);
            }
            return self.pill().filter(|(r, _)| r.contains(x, y)).map(|_| BarHit::Pill);
        }
        let (_, _, bg) = self.dock_geometry(&self.dock_items());
        if self.level_t() > 0 {
            return self.bar_panel(bg).contains(x, y).then_some(BarHit::Level);
        }
        if self.note_t() >= 500 {
            if let Some(a) = self.note_act() {
                let row = self.note_row(bg);
                let (close, open) = Self::note_buttons(row, a.app.is_some());
                if close.contains(x, y) {
                    return Some(BarHit::Close);
                }
                if open.is_some_and(|o| o.contains(x, y)) {
                    return Some(BarHit::Open);
                }
                if row.contains(x, y) {
                    return Some(BarHit::Note);
                }
            }
        }
        (self.bubble.value() >= 500 && self.bubble_rect(bg).contains(x, y)).then_some(BarHit::Bubble)
    }

    /// A press on the bar's own parts; whether it took it.
    pub(super) fn bar_press(&mut self, l: &mut Looper, x: i32, y: i32) -> bool {
        let Some(hit) = self.bar_hit(x, y) else { return false };
        match hit {
            BarHit::Close => self.dismiss_note(l),
            BarHit::Open => {
                let app = self.note_act().and_then(|a| a.app);
                self.dismiss_note(l);
                if let Some(id) = app {
                    self.open_id(l, id);
                }
            }
            BarHit::Note => {}
            BarHit::Bubble => {
                // The one in the bubble swaps with the one in the dock.
                if let Some(&j) = self.waiting().first() {
                    self.acts[j].front = time::uptime_ms();
                    self.bar_targets();
                    self.damage_bar(l);
                }
            }
            BarHit::Pill => {
                if let Some(i) = self.primary() {
                    let key = self.acts[i].key.clone();
                    if let Some(item) = self.dock_items().into_iter().find(|it| it.kind == key) {
                        self.dock_press(l, &item);
                    }
                }
            }
            BarHit::Level => self.set_level_at(l, x),
        }
        true
    }

    // ------------------------------------------------------------------
    // Drawing

    /// The bar: the dock with what goes on, or over a full-screen window
    /// the pill and the level.
    pub(super) fn draw_bar(&self, c: &mut Canvas) {
        // Remember all it may have painted, to clear it after a change.
        self.bar_painted.set(self.bar_painted.get().union(&self.dock_zone()));
        if self.dock_shown() {
            self.draw_dock(c);
            return;
        }
        let level = self.level_t();
        let pill = self.pill();
        if level > 0 {
            let to = self.level_rect();
            let from = pill.map_or(ui::scale_rect(to, 250), |(r, _)| r);
            let panel = ui::lerp_rect(from, to, level);
            let rad = ui::lerp(PILL_H / 2, LEVEL_RAD, level);
            ui::drop_shadow(c, panel, rad, 14, 55 * level / 1000);
            ui::panel(c, panel, rad);
            if level >= LEVEL_SWAP {
                self.draw_level(c, panel, level);
            }
            return;
        }
        if let Some((r, i)) = pill {
            self.draw_pill(c, r, i);
        }
    }

    /// The bar's panel around the dock `bg` (grown for a notification, or
    /// becoming the level) with its shadow, the bubble and the
    /// notification. None when the bar is the level now: then nothing of
    /// the dock is drawn.
    pub(super) fn bar_frame(&self, c: &mut Canvas, bg: Rect) -> Option<Rect> {
        let level = self.level_t();
        let panel = self.bar_panel(bg);
        let rad = ui::lerp(DOCK_RAD, LEVEL_RAD, level);
        ui::drop_shadow(c, panel, rad, 16, 55);
        ui::panel(c, panel, rad);
        if level >= LEVEL_SWAP {
            self.draw_level(c, panel, level);
            return None;
        }
        if level == 0 {
            self.draw_bubble(c, bg);
        }
        self.draw_bar_note(c, bg, panel);
        Some(panel)
    }

    /// The level's symbol, cells and value inside `panel`, coming in as
    /// the morph ends (`t`).
    fn draw_level(&self, c: &mut Canvas, panel: Rect, t: u32) {
        let Some(a) = self.level_act() else { return };
        let k = t.saturating_sub(LEVEL_SWAP) * 1000 / (1000 - LEVEL_SWAP);
        let saved = c.clip();
        c.set_clip(saved.intersect(&panel));
        let (sym, cells, digits) = self.level_parts();
        let glyph: &[&str] = match a.kind {
            ActKind::Brightness => &G_SUN[..],
            _ if a.value == 0 => &G_MUTE[..],
            _ => &G_VOLUME[..],
        };
        let fg = fade(ui::text(), k);
        ui::soft_pixels_in(c, glyph, sym, LEVEL_SYM, fg);
        let filled = ((a.value.min(100) * LEVEL_CELLS as u32 + 50) / 100) as i32;
        for i in 0..LEVEL_CELLS {
            let cell = Rect::new(cells.x + i * (LEVEL_CELL_W + LEVEL_CELL_GAP), cells.y, LEVEL_CELL_W, cells.h);
            if i >= filled {
                c.fill_round(cell, 5, fade(ui::cell(), k));
            } else if a.kind == ActKind::Volume {
                c.fill_round_grad(cell, 5, fade(ui::ORANGE_GRAD.0, k), fade(ui::ORANGE_GRAD.1, k));
            } else {
                c.fill_round(cell, 5, fg);
            }
        }
        draw_digits(c, &format!("{}", a.value), digits.x, digits, LEVEL_SYM, fg);
        c.set_clip(saved);
    }

    /// A short notification's row above the dock `bg`, inside `panel`.
    fn draw_bar_note(&self, c: &mut Canvas, bg: Rect, panel: Rect) {
        let t = self.note_t();
        let Some(a) = self.note_act().filter(|_| t > 0) else { return };
        let row = self.note_row(bg);
        let saved = c.clip();
        c.set_clip(saved.intersect(&panel));
        res::draw_icon_faded(c, &a.icon, 40, row.x + 6, row.y + (row.h - 40) / 2, t * 255 / 1000);
        let (close, open) = Self::note_buttons(row, a.app.is_some());
        let x = row.x + 58;
        let w = close.x - 14 - x;
        let (th, _) = text::metrics(Font::Strong);
        let (sh, _) = text::metrics(Font::Small);
        let y = row.y + (row.h - th - sh) / 2;
        text::draw(c, Font::Strong, x, y, &text::fit(Font::Strong, &a.title, w), fade(ui::text(), t));
        text::draw(c, Font::Small, x, y + th, &text::fit(Font::Small, &a.sub, w), fade(ui::text_dim(), t));
        ui::button(c, close, "Chiudi", false, self.bar_hover == Some(BarHit::Close));
        if let Some(o) = open {
            ui::button(c, o, "Apri", true, self.bar_hover == Some(BarHit::Open));
        }
        c.set_clip(saved);
    }

    /// The bubble beside the dock `bg`: the icon of the activity waiting,
    /// and how many wait when more than one.
    fn draw_bubble(&self, c: &mut Canvas, bg: Rect) {
        let t = self.bubble.value();
        if t == 0 {
            return;
        }
        let r = ui::scale_rect(self.bubble_rect(bg), ui::lerp(400, 1000, t));
        let rad = r.h / 2;
        ui::drop_shadow(c, r, rad, 16, 55 * t / 1000);
        ui::panel(c, r, rad);
        if self.bar_hover == Some(BarHit::Bubble) {
            c.fill_round(r, rad, ui::sel());
        }
        let s = (r.w * 3 / 5) & !1;
        res::draw_icon_faded(c, &self.bubble_icon, s, r.x + (r.w - s) / 2, r.y + (r.h - s) / 2, t * 255 / 1000);
        let n = self.waiting().len();
        if n >= 2 && t >= 1000 {
            let label = format!("{n}");
            let bw = (text::width(Font::Small, &label) + 12).max(20);
            let b = Rect::new(r.right() - bw + 4, r.y - 4, bw, 20);
            c.fill_round_grad(b, 10, ui::ORANGE_GRAD.0, ui::ORANGE_GRAD.1);
            let (lh, _) = text::metrics(Font::Small);
            text::draw_centered(c, Font::Small, b.x + b.w / 2, b.y + (b.h - lh) / 2, &label, ui::WHITE);
        }
    }

    /// The capsule of `a` on the dock icon `r` (its icon `icon`, lifted
    /// by `lift` when it bounces), widened by `extra`. Returns its tile.
    pub(super) fn draw_capsule(&self, c: &mut Canvas, icon: &str, a: &Activity, r: Rect, extra: i32, lift: i32) -> Rect {
        let t = a.open.value();
        let (well, tail) = cap_parts(a, r, extra);
        let rad = ui::lerp(r.w / 4, CAP_RAD, t);
        c.fill_round(well, rad, fade(ui::well(), t));
        c.stroke_round(well, rad, 1, fade(ui::line(), t));
        if a.ringing {
            // A timer at zero pulses orange, once a second.
            let ph = (time::uptime_ms() % 1000) as u32;
            let tri = if ph < 500 { ph * 2 } else { (1000 - ph) * 2 };
            c.fill_round(well, rad, fade(((40 + tri * 120 / 1000) << 24) | (ui::ORANGE & 0xFF_FFFF), t));
        }
        let ts = ui::lerp(r.w, r.w - CAP_TILE, t) & !1;
        let tile = Rect::new(r.x + ui::lerp(0, 3, t), r.y + (r.h - ts) / 2 - lift, ts, ts);
        res::draw_icon(c, icon, ts, tile.x, tile.y);
        let saved = c.clip();
        c.set_clip(saved.intersect(&well));
        let x = r.x + r.w;
        let (fg, dim) = (fade(ui::text(), t), fade(ui::text_dim(), t));
        if a.kind == ActKind::Timer {
            draw_digits(c, &a.title, x, well, TIMER_CELL, fg);
            let (sh, _) = text::metrics(Font::Small);
            text::draw(c, Font::Small, x + digits_w(&a.title, TIMER_CELL) + 8, well.y + (well.h - sh) / 2, &a.sub, dim);
        } else {
            let w = text_w(a);
            let (th, _) = text::metrics(Font::Strong);
            let (sh, _) = text::metrics(Font::Small);
            let y = well.y + (well.h - th - sh) / 2;
            text::draw(c, Font::Strong, x, y, &text::fit(Font::Strong, &a.title, w), fg);
            text::draw(c, Font::Small, x, y + th, &text::fit(Font::Small, &a.sub, w), dim);
            if a.kind == ActKind::Music {
                draw_eq(c, tail, EQ_H, t);
            } else {
                draw_busy(c, tail, BUSY, BUSY_H, t);
            }
        }
        c.set_clip(saved);
        tile
    }

    /// The pill `r` over a full-screen window: the time, a thin line, and
    /// the activity `i` (the equaliser, the cells, a timer's time left).
    fn draw_pill(&self, c: &mut Canvas, r: Rect, i: usize) {
        let a = &self.acts[i];
        let rad = PILL_H / 2;
        ui::drop_shadow(c, r, rad, 12, 50);
        ui::panel(c, r, rad);
        if self.bar_hover == Some(BarHit::Pill) {
            c.fill_round(r, rad, ui::sel());
        }
        let (clock, _) = clock_strings();
        draw_digits(c, &clock, r.x + PILL_PAD, r, PILL_CELL, ui::text());
        let sx = r.x + PILL_PAD + digits_w(&clock, PILL_CELL) + 10;
        c.fill(Rect::new(sx, r.y + (r.h - 14) / 2, 1, 14), ui::line());
        let ind = self.pill_ind(r);
        match a.kind {
            ActKind::Music => draw_eq(c, ind, 14, 1000),
            ActKind::Starting => draw_busy(c, ind, PILL_BUSY, 10, 1000),
            _ => draw_digits(c, &a.title, ind.x, ind, PILL_CELL, if a.ringing { ui::ORANGE } else { ui::text_dim() }),
        }
    }
}

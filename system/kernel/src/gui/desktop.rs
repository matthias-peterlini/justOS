//! The desktop: the clock floating at the top, the widgets, the dock (the
//! JustOS mark, the apps, the status), the launcher, the windows and the
//! animations between them. There is no bar.
//!
//! Everything that moves is driven by time ([`ui::Anim`]), not by frames:
//! on a slow machine an animation shows fewer frames but takes as long.
//! A window that opens, closes, shrinks into the dock or zooms is drawn once
//! into a picture of its own ([`Window::snapshot`]) and that picture is
//! scaled and faded on every frame, so the app is not redrawn meanwhile.

use super::apps::{self, Launch};
use super::dnd;
use super::gfx::{Canvas, Rect};
use super::lock::{Lock, Outcome};
use super::res;
use super::settings;
use super::splash::{draw_logo, draw_mark};
use super::text::{self, Font};
use super::ui::{self, Anim, Event, Glide, Looper, View, TEXT, TEXT_DIM, TEXT_FAINT};
use super::wm::{self, Action, App, Hit, Window};
use super::Display;
use crate::dev::input::{self, Key, KeyEvent};
use crate::fs;
use crate::time::{self, DateTime};
use crate::util::Size;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const WEEKDAYS: [&str; 7] = ["Lunedì", "Martedì", "Mercoledì", "Giovedì", "Venerdì", "Sabato", "Domenica"];
const WEEKDAYS_SHORT: [&str; 7] = ["lun", "mar", "mer", "gio", "ven", "sab", "dom"];
pub const MONTHS: [&str; 12] =
    ["gennaio", "febbraio", "marzo", "aprile", "maggio", "giugno", "luglio", "agosto", "settembre", "ottobre", "novembre", "dicembre"];
const MONTHS_SHORT: [&str; 12] = ["gen", "feb", "mar", "apr", "mag", "giu", "lug", "ago", "set", "ott", "nov", "dic"];

/// The apps kept in the dock, in order (app id, icon, name).
const PINNED: [(&str, &str, &str); 10] = [
    ("files", "files", "File"),
    ("launcher", "apps", "Applicazioni"),
    ("textedit", "text-editor", "Editor di testo"),
    ("notes", "notes", "Note"),
    ("calendar", "calendar", "Calendario"),
    ("photos", "photos", "Foto"),
    ("music", "music", "Musica"),
    ("calculator", "calculator", "Calcolatrice"),
    ("store", "store", "Negozio"),
    ("settings", "settings", "Impostazioni"),
];
const ICON: i32 = 52;
/// How much bigger the icon under the pointer gets.
const MAGNIFY: i32 = 30;
const DOCK_GAP: i32 = 10;
const DOCK_PAD: i32 = 11;
const SEP_W: i32 = 19;
/// The dark pill with the JustOS mark at the left end of the dock (it opens
/// the system menu), and the room between the dock's pills and its icons.
const MARK_W: i32 = 64;
const PILL_GAP: i32 = 12;
/// The control centre: its width, the account row above the panel, the
/// sliders' width and the height of the switches' column.
const CC_W: i32 = 380;
const CC_HEAD: i32 = 64;
const CC_SLIDER: i32 = 64;
const CC_COL: i32 = 200;
/// The blue of a switch that is on (the network).
const BLUE: u32 = 0xFF2F_7BEF;

/// "16:02" and "Gio 13 Ott".
fn clock_strings() -> (String, String) {
    let d = now();
    (
        format!("{:02}:{:02}", d.hour, d.min),
        format!(
            "{} {} {}",
            capitalize(WEEKDAYS_SHORT[d.weekday as usize % 7]),
            d.day,
            capitalize(MONTHS_SHORT[(d.month as usize + 11) % 12])
        ),
    )
}

/// One icon in the dock.
#[derive(Clone)]
struct DockItem {
    /// The app id ("launcher" for the launcher, "trash").
    id: String,
    icon: String,
    label: String,
    /// The window kind it stands for.
    kind: String,
    /// What it starts when nothing of its kind is open.
    launch: Option<Launch>,
    /// A separator comes before it.
    split: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuAct {
    About,
    Lock,
    Screenshot,
    Settings,
    Store,
    Restart,
    PowerOff,
    Sleep,
    NextWallpaper,
    Display,
    Activity,
    Overview,
    Widgets,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PopupKind {
    Menu,
    /// The control centre: the account, quick switches, volume, brightness
    /// and the power buttons, above the status in the dock.
    Control,
    /// The calendar and the notifications, under the clock.
    Notices,
}

/// The switches of the control centre.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tile {
    Network,
    Sound,
    Quiet,
}

/// What can be pressed in the control centre.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ControlHit {
    Tile(Tile),
    Volume,
    Brightness,
    Account,
    Settings,
    PowerOff,
    Lock,
    Restart,
}

struct Popup {
    kind: PopupKind,
    rect: Rect,
    items: Vec<(&'static str, Option<MenuAct>)>,
    anim: Anim,
    /// It unfolds upwards (it opens from the dock).
    up: bool,
    /// Drawn once more after the animation ended: a slow frame may have
    /// shown it half open.
    settled: bool,
}

impl Popup {
    fn menu(x: i32, y: i32, items: Vec<(&'static str, Option<MenuAct>)>, screen_w: i32) -> Popup {
        let w = 230;
        let h = items.iter().map(|(_, a)| if a.is_some() { 34 } else { 11 }).sum::<i32>() + 12;
        let rect = Rect::new(x.min(screen_w - w - 10).max(10), y, w, h);
        Popup { kind: PopupKind::Menu, rect, items, anim: Anim::new(140), up: false, settled: false }
    }

    /// The same popup, moved to end `gap` above `top` and unfolding upwards.
    fn above(mut self, top: i32, gap: i32) -> Popup {
        self.rect.y = top - gap - self.rect.h;
        self.up = true;
        self
    }

    /// The rectangle of each item that can be chosen.
    fn item_rects(&self) -> Vec<(Rect, usize)> {
        let mut y = self.rect.y + 6;
        let mut out = Vec::new();
        for (i, (_, act)) in self.items.iter().enumerate() {
            if act.is_some() {
                out.push((Rect::new(self.rect.x + 6, y, self.rect.w - 12, 32), i));
                y += 34;
            } else {
                y += 11;
            }
        }
        out
    }

    /// The part shown so far (popups unfold from the edge they open at).
    fn shown(&self) -> Rect {
        let t = self.anim.out();
        let r = ui::with_shadow(self.rect);
        let h = ui::lerp(r.h / 3, r.h, t);
        if self.up {
            Rect::new(r.x, r.bottom() - h, r.w, h)
        } else {
            Rect::new(r.x, r.y, r.w, h)
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FxKind {
    Open,
    Close,
    Minimize,
    Restore,
    /// Maximise or back: from this rectangle (the window's old bounds).
    Zoom(Rect),
}

/// A window in motion: its picture and where it goes.
struct Fx {
    id: u32,
    kind: FxKind,
    anim: Anim,
    snap: Canvas,
    /// The window's bounds when at rest.
    at: Rect,
    /// The dock icon a minimised window shrinks into.
    dock: Rect,
    /// Where it was drawn last frame.
    last: Rect,
}

impl Fx {
    fn ms(kind: FxKind) -> u64 {
        match kind {
            FxKind::Open => 230,
            FxKind::Close => 170,
            FxKind::Minimize | FxKind::Restore => 340,
            FxKind::Zoom(_) => 230,
        }
    }

    /// Where the picture goes now, and how opaque it is.
    fn place(&self) -> (Rect, u32) {
        match self.kind {
            FxKind::Open => {
                let t = self.anim.out();
                (ui::scale_rect(self.at, ui::lerp(860, 1000, t)), t * 255 / 1000)
            }
            FxKind::Close => {
                let t = self.anim.out();
                (ui::scale_rect(self.at, ui::lerp(1000, 900, t)), (1000 - t) * 255 / 1000)
            }
            FxKind::Minimize => {
                let t = self.anim.in_out();
                (ui::lerp_rect(self.at, self.dock, t), 255 - t * 175 / 1000)
            }
            FxKind::Restore => {
                let t = self.anim.in_out();
                (ui::lerp_rect(self.dock, self.at, t), 80 + t * 175 / 1000)
            }
            FxKind::Zoom(from) => (ui::lerp_rect(from, self.at, self.anim.out()), 255),
        }
    }
}

/// A notification banner at the top right.
struct Note {
    title: String,
    body: String,
    icon: String,
    born: Anim,
}

const NOTE_W: i32 = 340;
const NOTE_H: i32 = 76;
const NOTE_IN: u64 = 320;
const NOTE_STAY: u64 = 4500;

impl Note {
    /// Slide offset 0..=1000 (1000: fully in).
    fn slide(&self) -> u32 {
        let e = self.born.elapsed();
        if e < NOTE_IN {
            ui::ease_out((e * 1000 / NOTE_IN) as u32)
        } else if e < NOTE_IN + NOTE_STAY {
            1000
        } else {
            let out = (e - NOTE_IN - NOTE_STAY).min(NOTE_IN);
            1000 - ui::ease_in_out((out * 1000 / NOTE_IN) as u32)
        }
    }

    fn moving(&self) -> bool {
        let e = self.born.elapsed();
        e < NOTE_IN || (NOTE_IN + NOTE_STAY..NOTE_IN * 2 + NOTE_STAY + 50).contains(&e)
    }

    fn gone(&self) -> bool {
        self.born.elapsed() > NOTE_IN * 2 + NOTE_STAY + 50
    }
}

enum Drag {
    Move(u32, i32, i32),
    /// Resizing: the window and the pointer's offset from its corner.
    Resize(u32, i32, i32),
    /// A press on a window's content: moves go to the app.
    Body(u32),
    Volume,
    Brightness,
}

/// Files carried with the pointer out of a window, until the button goes up.
struct Carry {
    paths: Vec<String>,
    /// The icon drawn under the pointer.
    icon: &'static str,
    /// The window they came from.
    from: u32,
    /// The window under them that was told last.
    over: Option<u32>,
    /// The dock icon under them, when its app takes them.
    dock: Option<usize>,
    /// Ctrl or Alt held: they will be copied (a "+" on the icon).
    copy: bool,
}

/// What a carried icon may cover around the pointer.
fn carry_rect(x: i32, y: i32) -> Rect {
    Rect::new(x - 100, y - 56, 200, 140)
}

/// Does a dock icon take files dropped on it?
fn takes_files(item: &DockItem) -> bool {
    matches!(item.kind.as_str(), "trash" | "files" | "textedit" | "photos" | "viewer" | "music" | "arcstudio")
}

struct Launcher {
    query: String,
    entries: Vec<apps::Entry>,
    anim: Anim,
    closing: bool,
    /// Drawn once more after fading in (see [`Popup::settled`]).
    settled: bool,
    /// What to start once it has faded out.
    then: Option<Launch>,
    scroll: i32,
    /// Calculations, settings and files for the query.
    found: Vec<apps::search::Found>,
    index: apps::search::Index,
}

/// The rows of search results shown at most.
const FOUND_ROWS: usize = 6;

impl Launcher {
    fn visible(&self) -> Vec<&apps::Entry> {
        let q = self.query.to_lowercase();
        self.entries.iter().filter(|e| q.is_empty() || e.name.to_lowercase().contains(&q)).collect()
    }

    /// The height the search results take above the apps.
    fn found_h(&self) -> i32 {
        match self.found.len().min(FOUND_ROWS) as i32 {
            0 => 0,
            n => n * 44 + 12 + 18,
        }
    }

    /// How far it has faded in, 0..=1000.
    fn shown(&self) -> u32 {
        let t = self.anim.out();
        if self.closing {
            1000 - t
        } else {
            t
        }
    }
}

/// The overview (Mission Control): the desktops at the top, the windows of
/// this one side by side below.
struct Overview {
    anim: Anim,
    closing: bool,
    /// Drawn once more after coming in (see [`Popup::settled`]).
    settled: bool,
    /// A picture of every window shown on some desktop, taken on opening:
    /// the window, the picture, its bounds and its frame then.
    snaps: Vec<(u32, Canvas, Rect, Rect)>,
    /// A window pressed: which, where, and whether it is being dragged.
    held: Option<(u32, i32, i32, bool)>,
}

impl Overview {
    /// How far it has come in, 0..=1000.
    fn shown(&self) -> u32 {
        let t = self.anim.out();
        if self.closing {
            1000 - t
        } else {
            t
        }
    }
}

const MAX_SPACES: usize = 6;
/// How long the desktop's name stays up after a switch.
const SPACE_HUD_MS: u64 = 900;

enum Ending {
    PowerOff,
    Restart,
}

struct Desktop {
    w: i32,
    h: i32,
    sharp: Canvas,
    blur: Canvas,
    windows: Vec<Window>,
    next_id: u32,
    drag: Option<Drag>,
    launcher: Option<Launcher>,
    popup: Option<Popup>,
    fx: Vec<Fx>,
    notes: Vec<Note>,
    /// The dock item under the pointer.
    hover: Option<usize>,
    /// The dock's magnification, 0..=1000.
    mag: Glide,
    /// An icon bouncing because its app is starting.
    bounce: Option<(String, Anim)>,
    mouse: (i32, i32),
    minute: u64,
    second: u64,
    cascade: i32,
    /// The window under the pointer that was told about it last.
    hovered: Option<u32>,
    ending: Option<(Ending, Anim)>,
    /// A full-screen window is in front (no bar, no dock).
    was_full: bool,
    /// Full screen, pointer at the bottom edge: the dock is shown.
    dock_reveal: bool,
    /// The notifications shown so far (newest last), for the notification
    /// centre: title, text, icon, "hh:mm".
    history: Vec<(String, String, String, String)>,
    /// Do not disturb: notifications go to the centre without a banner.
    quiet: bool,
    /// The lock screen, while the computer is locked.
    lock: Option<Lock>,
    /// When the user last pressed a key or moved the pointer (uptime, ms).
    last_input: u64,
    /// Where the window being dragged would snap if dropped now.
    snap_preview: Option<Rect>,
    /// The virtual desktop shown, and how many there are.
    space: usize,
    spaces: usize,
    overview: Option<Overview>,
    /// The desktop's name, shown for a moment after a switch.
    space_hud: Option<Anim>,
    /// Files being dragged out of a window.
    carry: Option<Carry>,
    /// The charge shown in the bar, and whether it charges (a laptop).
    battery: Option<(u8, bool)>,
}

fn now() -> DateTime {
    time::from_epoch(time::now())
}

impl Desktop {
    /// The clock and the date, floating at the top in the middle (in the
    /// title row of a full-screen window, on one line): they open the
    /// notifications.
    fn clock_btn(&self) -> Rect {
        let (clock, date) = clock_strings();
        if self.full_screen() {
            let w = text::width(Font::Strong, &clock) + 12 + text::width(Font::Strong, &date) + 28;
            return Rect::new((self.w - w) / 2, (wm::TITLE_H - 32) / 2, w, 32);
        }
        let w = text::width(Font::Title, &clock).max(text::width(Font::Strong, &date)) + 40;
        let (th, _) = text::metrics(Font::Title);
        let (sh, _) = text::metrics(Font::Strong);
        Rect::new((self.w - w) / 2, 10, w, th + sh + 12)
    }

    /// A dot for each desktop under the clock, when there is more than one:
    /// it opens the overview.
    fn spaces_btn(&self) -> Option<Rect> {
        let cb = self.clock_btn();
        let w = self.spaces as i32 * 14 + 18;
        (self.spaces > 1 && !self.full_screen()).then(|| Rect::new(self.w / 2 - w / 2, cb.bottom() + 2, w, 20))
    }

    /// What the clock and the desktop dots may paint.
    fn clock_zone(&self) -> Rect {
        let cb = self.clock_btn();
        let mut r = cb;
        if let Some(sp) = self.spaces_btn() {
            r = r.union(&sp);
        }
        // Room for a clock that grows by a digit, and for the hover pill.
        r.grow(24)
    }

    fn widgets(&self) -> [Rect; 3] {
        let w = 300;
        let y = 16;
        [Rect::new(16, y, w, 132), Rect::new(16, y + 144, w, 222), Rect::new(16, y + 378, w, 120)]
    }

    fn show_widgets(&self) -> usize {
        if !settings::get().widgets {
            0
        } else if self.h >= 700 {
            3
        } else if self.h >= 560 {
            2
        } else {
            0
        }
    }

    // ------------------------------------------------------------------
    // The dock

    fn dock_items(&self) -> Vec<DockItem> {
        let mut v: Vec<DockItem> = PINNED
            .iter()
            .map(|&(id, icon, label)| DockItem {
                id: id.into(),
                icon: icon.into(),
                label: label.into(),
                kind: apps::kind_of(id).into(),
                launch: if id == "launcher" { None } else { Some(Launch::App(id)) },
                split: false,
            })
            .collect();
        // Running apps that are not pinned, after a separator.
        let mut first = true;
        for w in &self.windows {
            let kind = w.app.kind();
            if kind == "trash" || v.iter().any(|d| d.kind == kind) {
                continue;
            }
            v.push(DockItem {
                id: String::new(),
                icon: w.app.icon().into(),
                label: apps::app_name(w.app.kind(), &w.app.title()),
                kind: kind.into(),
                launch: None,
                split: first,
            });
            first = false;
        }
        v.push(DockItem {
            id: "trash".into(),
            icon: "trash".into(),
            label: "Cestino".into(),
            kind: "trash".into(),
            launch: Some(Launch::App("trash")),
            split: true,
        });
        v
    }

    /// The dock's base layout, before magnification, with icons `icon`
    /// units square: (x of each icon, total width). The mark comes first and
    /// the status last, `status_w` wide.
    fn dock_base(items: &[DockItem], status_w: i32, icon: i32) -> (Vec<i32>, i32) {
        let mut xs = Vec::new();
        let mut x = DOCK_PAD + MARK_W + PILL_GAP;
        for (i, it) in items.iter().enumerate() {
            if it.split && i > 0 {
                x += SEP_W;
            }
            xs.push(x);
            x += icon + DOCK_GAP;
        }
        (xs, x - DOCK_GAP + PILL_GAP + status_w + DOCK_PAD)
    }

    /// The size of the dock's icons: [`ICON`], or less when so many windows
    /// are open that the dock would not fit on the screen.
    fn dock_icon(&self, items: &[DockItem]) -> i32 {
        let (_, total) = Self::dock_base(items, self.status_w(), ICON);
        let room = self.w - 24;
        if total <= room || items.is_empty() {
            return ICON;
        }
        let n = items.len() as i32;
        (ICON - (total - room + n - 1) / n).max(32)
    }

    /// The width of the status pill: network, sound and, on a laptop, the
    /// battery's ring.
    fn status_w(&self) -> i32 {
        if self.battery.is_some() {
            136
        } else {
            92
        }
    }

    /// The dock's background at rest (no icon magnified).
    fn dock_rest(&self) -> Rect {
        let items = self.dock_items();
        let icon = self.dock_icon(&items);
        let (_, total) = Self::dock_base(&items, self.status_w(), icon);
        let dock_h = icon + 2 * DOCK_PAD + 4;
        Rect::new((self.w - total) / 2, self.h - dock_h - 12, total, dock_h)
    }

    /// The JustOS mark at the left end of the dock `bg`.
    fn mark_pill(bg: Rect) -> Rect {
        Rect::new(bg.x + DOCK_PAD, bg.y + DOCK_PAD, MARK_W, bg.h - 2 * DOCK_PAD - 4)
    }

    /// The status at the right end of the dock `bg`.
    fn status_pill(&self, bg: Rect) -> Rect {
        let w = self.status_w();
        Rect::new(bg.right() - DOCK_PAD - w, bg.y + DOCK_PAD, w, bg.h - 2 * DOCK_PAD - 4)
    }

    /// The icons' rectangles on the screen, magnified around the pointer,
    /// and the dock's background.
    fn dock_layout(&self, items: &[DockItem]) -> (Vec<Rect>, Rect) {
        let icon = self.dock_icon(items);
        let (xs, total) = Self::dock_base(items, self.status_w(), icon);
        let dock_h = icon + 2 * DOCK_PAD + 4;
        let base_x = (self.w - total) / 2;
        let bottom = self.h - 12 - DOCK_PAD - 4;
        let mag = self.mag.value.clamp(0, 1000) as i64;
        let range = (icon + DOCK_GAP) as i64 * 5 / 2;
        let mx = self.mouse.0 as i64;
        let mut sizes: Vec<i32> = xs
            .iter()
            .map(|&x| {
                if mag == 0 {
                    return icon;
                }
                let c = (base_x + x + icon / 2) as i64;
                let d = (mx - c).abs();
                let f = if d >= range { 0 } else { 1000 - d * d * 1000 / (range * range) };
                icon + (MAGNIFY as i64 * mag * f / 1_000_000) as i32
            })
            .collect();
        // A full dock grows only as far as the screen's edges.
        let room = (self.w - 8 - total).max(0);
        let grown: i32 = sizes.iter().map(|s| s - icon).sum();
        if grown > room {
            for s in sizes.iter_mut() {
                *s = icon + (*s - icon) * room / grown;
            }
        }
        let extra: i32 = sizes.iter().map(|s| s - icon).sum();
        let total_now = total + extra;
        let x0 = (self.w - total_now) / 2;
        let mut rects = Vec::new();
        let mut shift = 0;
        for (i, &x) in xs.iter().enumerate() {
            let s = sizes[i] & !1;
            rects.push(Rect::new(x0 + x + shift, bottom - s, s, s));
            shift += sizes[i] - icon;
        }
        // Keep the point under the pointer where it was, so the icon being
        // aimed at does not slide away as the others grow.
        if mag > 0 && !xs.is_empty() {
            let k = xs.iter().rposition(|&x| base_x + x - DOCK_GAP / 2 <= self.mouse.0).unwrap_or(0);
            let f = ((self.mouse.0 - (base_x + xs[k])) as i64 * 1000 / icon as i64).clamp(0, 1000);
            let now_at = rects[k].x as i64 + rects[k].w as i64 * f / 1000;
            let shift = ((mx - now_at) * mag / 1000) as i32;
            let shift = shift.clamp(-extra / 2, extra / 2);
            for r in rects.iter_mut() {
                r.x += shift;
            }
            let bg = Rect::new(x0 + shift, self.h - dock_h - 12, total_now, dock_h);
            return (rects, bg);
        }
        let bg = Rect::new(x0, self.h - dock_h - 12, total_now, dock_h);
        (rects, bg)
    }

    fn dock(&self) -> Rect {
        self.dock_layout(&self.dock_items()).1
    }

    /// The area the dock may paint (icons grow above it).
    fn dock_zone(&self) -> Rect {
        let d = self.dock();
        Rect::new(d.x - MAGNIFY * 4 - 30, d.y - MAGNIFY - 60, d.w + MAGNIFY * 8 + 60, d.h + MAGNIFY + 80)
    }

    /// The dock icon of a window kind (where it minimises to).
    fn dock_icon_of(&self, kind: &str) -> Rect {
        let items = self.dock_items();
        let (rects, bg) = self.dock_layout(&items);
        items.iter().position(|d| d.kind == kind).map(|i| rects[i]).unwrap_or(Rect::new(bg.x + bg.w / 2 - 20, bg.y, 40, 40))
    }

    /// Where windows go: under the clock, above the dock.
    fn work_area(&self) -> Rect {
        let (th, _) = text::metrics(Font::Title);
        let (sh, _) = text::metrics(Font::Strong);
        let top = 10 + th + sh + 12 + 8;
        let dock_top = self.h - (ICON + 2 * DOCK_PAD + 4) - 12;
        Rect::new(8, top, self.w - 16, dock_top - top - 8)
    }

    fn screen(&self) -> Rect {
        Rect::new(0, 0, self.w, self.h)
    }

    /// Where a maximised window goes: the whole screen.
    fn full_area(&self) -> Rect {
        self.screen()
    }

    /// Is a full-screen window in front? Then the dock hides.
    fn full_screen(&self) -> bool {
        self.launcher.is_none() && self.focused().and_then(|id| self.window(id)).is_some_and(|w| w.full())
    }

    /// Is the dock on the screen?
    fn dock_shown(&self) -> bool {
        !self.full_screen() || self.dock_reveal
    }

    // ------------------------------------------------------------------
    // Windows

    fn window_mut(&mut self, id: u32) -> Option<&mut Window> {
        self.windows.iter_mut().find(|w| w.id == id)
    }

    fn window(&self, id: u32) -> Option<&Window> {
        self.windows.iter().find(|w| w.id == id)
    }

    fn focused(&self) -> Option<u32> {
        self.windows.iter().rev().find(|w| w.shown(self.space)).map(|w| w.id)
    }

    fn moving(&self, id: u32) -> bool {
        self.fx.iter().any(|f| f.id == id)
    }

    fn raise(&mut self, l: &mut Looper, id: u32) {
        // A window on another desktop: go there.
        if let Some(space) = self.window(id).map(|w| w.space).filter(|&s| s != self.space) {
            self.go_to_space(l, space);
        }
        if let Some(i) = self.windows.iter().position(|w| w.id == id) {
            let win = self.windows.remove(i);
            let was_min = win.minimized;
            l.damage(win.bounds());
            self.windows.push(win);
            if was_min {
                self.start_fx(l, id, FxKind::Restore);
                if let Some(w) = self.window_mut(id) {
                    w.minimized = false;
                }
            }
            // The window that lost focus redraws its title.
            if let Some(prev) = self.windows.iter().rev().nth(1) {
                l.damage(prev.bounds());
            }
            l.damage(self.clock_zone());
        }
    }

    /// Starts an animation of window `id` (it is drawn as a picture until
    /// the end).
    fn start_fx(&mut self, l: &mut Looper, id: u32, kind: FxKind) {
        self.fx.retain(|f| f.id != id || matches!(f.kind, FxKind::Close));
        let focused = self.focused() == Some(id) || kind == FxKind::Open;
        let blur = core::mem::replace(&mut self.blur, Canvas::new(0, 0, 0));
        let dock = self.window(id).map(|w| self.dock_icon_of(w.app.kind())).unwrap_or_default();
        let Some(win) = self.window_mut(id) else {
            self.blur = blur;
            return;
        };
        let (snap, at) = win.snapshot(&blur, focused);
        self.blur = blur;
        l.damage(at);
        self.fx.push(Fx { id, kind, anim: Anim::new(Fx::ms(kind)), snap, at, dock, last: at });
    }

    fn open(&mut self, l: &mut Looper, app: Box<dyn App>, single: bool) -> u32 {
        if single {
            if let Some(id) = self.windows.iter().rev().find(|w| w.app.kind() == app.kind()).map(|w| w.id) {
                self.raise(l, id);
                return id;
            }
        }
        let area = self.work_area();
        let (aw, ah) = Window::outer_size(app.size().0, app.size().1);
        let (aw, full_h) = (aw.min(area.w), ah.min(area.h));
        let x = (area.x + (area.w - aw) / 2 + 120 + self.cascade * 26).min(area.right() - aw).max(area.x);
        let y = (area.y + 14 + self.cascade * 24).min(area.bottom() - full_h).max(area.y);
        self.cascade = (self.cascade + 1) % 6;
        let id = self.next_id;
        self.next_id += 1;
        let mut win = Window::new(id, app, x, y);
        win.space = self.space;
        win.rect.w = aw;
        win.rect.h = full_h;
        let kind = String::from(win.app.kind());
        if let Some(prev) = self.windows.last() {
            l.damage(prev.bounds());
        }
        self.windows.push(win);
        self.start_fx(l, id, FxKind::Open);
        l.damage(self.dock_zone());
        l.damage(self.clock_zone());
        self.bounce_icon(&kind);
        id
    }

    fn bounce_icon(&mut self, kind: &str) {
        self.bounce = Some((String::from(kind), Anim::new(650)));
    }

    fn open_id(&mut self, l: &mut Looper, id: &'static str) {
        if id == "launcher" {
            self.toggle_launcher(l);
            return;
        }
        if let Some(app) = apps::open(id) {
            let single = apps::single(id);
            self.open(l, app, single);
        }
    }

    fn close_window(&mut self, l: &mut Looper, id: u32) {
        if !self.windows.iter().any(|w| w.id == id) {
            return;
        }
        self.start_fx(l, id, FxKind::Close);
        if let Some(i) = self.windows.iter().position(|w| w.id == id) {
            let mut win = self.windows.remove(i);
            win.app.close();
            l.damage(win.bounds());
        }
        l.damage(self.dock_zone());
        l.damage(self.clock_zone());
        if let Some(top) = self.windows.last() {
            l.damage(top.bounds());
        }
    }

    fn minimize(&mut self, l: &mut Looper, id: u32) {
        self.start_fx(l, id, FxKind::Minimize);
        if let Some(top) = self.windows.iter().rev().filter(|w| w.id != id && w.shown(self.space)).map(|w| w.bounds()).next() {
            l.damage(top);
        }
        l.damage(self.clock_zone());
    }

    fn zoom(&mut self, l: &mut Looper, id: u32) {
        let screen = self.full_area();
        let Some(win) = self.window_mut(id) else { return };
        let before = win.bounds();
        win.toggle_maximize(screen);
        l.damage(before);
        let after = win.bounds();
        l.damage(after);
        self.start_fx(l, id, FxKind::Zoom(before));
    }

    /// Where a window dropped at (`x`, `y`) snaps: a half at the left or
    /// right edge, a quarter near a corner, the whole screen at the top.
    fn snap_zone(&self, x: i32, y: i32) -> Option<Rect> {
        let a = self.work_area();
        const EDGE: i32 = 4;
        const CORNER: i32 = 90;
        let (left, right) = (x <= EDGE, x >= self.w - 1 - EDGE);
        if !left && !right {
            return (y <= EDGE).then(|| self.full_area());
        }
        let (hw, hh) = (a.w / 2 - 4, a.h / 2 - 4);
        let x0 = if left { a.x } else { a.right() - hw };
        Some(if y < a.y + CORNER {
            Rect::new(x0, a.y, hw, hh)
        } else if y > a.bottom() - CORNER {
            Rect::new(x0, a.bottom() - hh, hw, hh)
        } else {
            Rect::new(x0, a.y, hw, a.h)
        })
    }

    /// One of the snap zones by name, for the keyboard (Ctrl+Alt with the
    /// arrows or U, I, J, K, as Rectangle does on the Mac).
    fn snap_named(&self, key: Key) -> Option<Rect> {
        let a = self.work_area();
        let (top, mid, bottom) = (a.y + 1, a.y + a.h / 2, a.bottom() - 2);
        let right = self.w - 1;
        match key {
            Key::Left => self.snap_zone(0, mid),
            Key::Right => self.snap_zone(right, mid),
            Key::Char('u') | Key::Char('U') => self.snap_zone(0, top),
            Key::Char('i') | Key::Char('I') => self.snap_zone(right, top),
            Key::Char('j') | Key::Char('J') => self.snap_zone(0, bottom),
            Key::Char('k') | Key::Char('K') => self.snap_zone(right, bottom),
            _ => None,
        }
    }

    /// Fits window `id` into `zone` (the whole screen means maximised).
    fn snap(&mut self, l: &mut Looper, id: u32, zone: Rect) {
        if zone == self.full_area() {
            if self.window(id).is_some_and(|w| !w.full()) {
                self.zoom(l, id);
            }
            return;
        }
        let Some(win) = self.window_mut(id) else { return };
        let before = win.bounds();
        if let Some(r) = win.restore.take() {
            win.rect = r;
        }
        if win.snapped.is_none() {
            win.snapped = Some(win.rect);
        }
        win.rect = zone;
        l.damage(before);
        l.damage(win.bounds());
        self.start_fx(l, id, FxKind::Zoom(before));
        l.damage(self.clock_zone());
    }

    /// Back to the size and place it had before snapping or maximising.
    fn unsnap(&mut self, l: &mut Looper, id: u32) {
        if self.window(id).is_some_and(|w| w.full()) {
            self.zoom(l, id);
            return;
        }
        let Some(win) = self.window_mut(id) else { return };
        let Some(r) = win.snapped.take() else { return };
        let before = win.bounds();
        win.rect = r;
        l.damage(before);
        l.damage(win.bounds());
        self.start_fx(l, id, FxKind::Zoom(before));
    }

    // ------------------------------------------------------------------
    // Virtual desktops

    /// Shows desktop `n` (the last one if there are fewer); a window being
    /// dragged comes along.
    fn go_to_space(&mut self, l: &mut Looper, n: usize) {
        let n = n.min(self.spaces - 1);
        if let Some(Drag::Move(id, ..)) = self.drag {
            if let Some(w) = self.window_mut(id) {
                w.space = n;
            }
        }
        if n != self.space {
            self.space = n;
            l.damage(self.screen());
        }
        if self.overview.is_none() {
            self.space_hud = Some(Anim::new(SPACE_HUD_MS));
            l.damage(ui::with_shadow(self.space_hud_rect()));
        }
    }

    /// Moves window `id` to desktop `n` (a new one just past the last) and
    /// follows it there.
    fn send_to_space(&mut self, l: &mut Looper, id: u32, n: usize) {
        if n >= self.spaces && self.spaces < MAX_SPACES {
            self.spaces += 1;
        }
        let n = n.min(self.spaces - 1);
        if let Some(i) = self.windows.iter().position(|w| w.id == id) {
            let mut w = self.windows.remove(i);
            w.space = n;
            self.windows.push(w);
        }
        self.go_to_space(l, n);
    }

    /// Removes desktop `n`: its windows join the one before.
    fn remove_space(&mut self, l: &mut Looper, n: usize) {
        if self.spaces <= 1 || n >= self.spaces {
            return;
        }
        let to = n.saturating_sub(1);
        let shift = |s: usize| match s {
            s if s == n => to,
            s if s > n => s - 1,
            s => s,
        };
        for w in self.windows.iter_mut() {
            w.space = shift(w.space);
        }
        self.space = shift(self.space);
        self.spaces -= 1;
        l.damage(self.screen());
    }

    fn space_hud_rect(&self) -> Rect {
        Rect::new((self.w - 220) / 2, (self.h - 96) / 2, 220, 96)
    }

    /// Opens the overview: every window is drawn once into a picture, at
    /// most half the screen wide.
    fn open_overview(&mut self, l: &mut Looper) {
        if self.overview.is_some() {
            return;
        }
        self.launcher = None;
        self.popup = None;
        self.drag = None;
        self.snap_preview = None;
        self.space_hud = None;
        let focused = self.focused();
        let mut snaps = Vec::new();
        for w in self.windows.iter_mut().filter(|w| !w.minimized) {
            let (pic, at) = w.snapshot(&self.blur, Some(w.id) == focused);
            snaps.push((w.id, shrink(pic, self.w / 2), at, w.rect));
        }
        self.overview = Some(Overview { anim: Anim::new(300), closing: false, settled: false, snaps, held: None });
        l.damage(self.screen());
    }

    fn close_overview(&mut self, l: &mut Looper) {
        if let Some(o) = &mut self.overview {
            if !o.closing {
                o.closing = true;
                o.anim = Anim::new(240);
                o.held = None;
            }
        }
        l.damage(self.screen());
    }

    /// The desktops at the top of the overview, and the button that adds
    /// one (while there is room).
    fn space_thumbs(&self) -> (Vec<Rect>, Option<Rect>) {
        let n = self.spaces as i32;
        let more = self.spaces < MAX_SPACES;
        let gap = 20;
        let tw = 180.min((self.w - 60) / (n + 1) - gap);
        let th = tw * self.h / self.w.max(1);
        let pw = if more { th.min(56) } else { 0 };
        let total = n * tw + (n - 1) * gap + if more { gap + pw } else { 0 };
        let mut x = (self.w - total) / 2;
        let y = 24;
        let mut v = Vec::new();
        for _ in 0..n {
            v.push(Rect::new(x, y, tw, th));
            x += tw + gap;
        }
        (v, more.then(|| Rect::new(x, y + (th - pw) / 2, pw, pw)))
    }

    /// Each window of this desktop in the overview, side by side below the
    /// desktops: its id and where its frame goes.
    fn overview_slots(&self) -> Vec<(u32, Rect)> {
        let Some(o) = &self.overview else { return Vec::new() };
        let wins: Vec<(u32, Rect)> =
            o.snaps.iter().filter(|s| self.window(s.0).is_some_and(|w| w.shown(self.space))).map(|s| (s.0, s.3)).collect();
        let n = wins.len() as i32;
        if n == 0 {
            return Vec::new();
        }
        let top = self.space_thumbs().0.first().map_or(24, |t| t.bottom()) + 50;
        let area = Rect::new(40, top, self.w - 80, self.h - top - 50);
        let cols = (1..=n).find(|c| c * c >= n).unwrap_or(1);
        let rows = (n + cols - 1) / cols;
        let (cw, ch) = (area.w / cols, area.h / rows);
        wins.iter()
            .enumerate()
            .map(|(i, &(id, r))| {
                let (col, row) = (i as i32 % cols, i as i32 / cols);
                // The last row is centred.
                let in_row = if row == rows - 1 { n - row * cols } else { cols };
                let x0 = area.x + (area.w - in_row * cw) / 2;
                let s = 1000.min((cw - 40) * 1000 / r.w.max(1)).min((ch - 48) * 1000 / r.h.max(1)).max(40);
                let (w, h) = (r.w * s / 1000, r.h * s / 1000);
                (id, Rect::new(x0 + col * cw + (cw - w) / 2, area.y + row * ch + (ch - 28 - h) / 2, w, h))
            })
            .collect()
    }

    fn overview_press(&mut self, l: &mut Looper, x: i32, y: i32) {
        if self.overview.as_ref().is_none_or(|o| o.closing) {
            return;
        }
        let (thumbs, plus) = self.space_thumbs();
        if self.spaces > 1 {
            if let Some(i) = thumbs.iter().position(|t| space_close(*t).contains(x, y)) {
                self.remove_space(l, i);
                return;
            }
        }
        if let Some(i) = thumbs.iter().position(|t| t.contains(x, y)) {
            self.go_to_space(l, i);
            self.close_overview(l);
            return;
        }
        if plus.is_some_and(|p| p.contains(x, y)) {
            self.spaces += 1;
            l.damage(self.screen());
            return;
        }
        let hit = self.overview_slots().into_iter().rev().find(|(_, f)| f.contains(x, y)).map(|(id, _)| id);
        match (hit, &mut self.overview) {
            (Some(id), Some(o)) => o.held = Some((id, x, y, false)),
            _ => self.close_overview(l),
        }
    }

    /// The end of a press in the overview: a click brings the window to the
    /// front, a drag onto a desktop moves it there.
    fn overview_release(&mut self, l: &mut Looper) {
        let Some((id, _, _, dragging)) = self.overview.as_mut().and_then(|o| o.held.take()) else { return };
        l.damage(self.screen());
        if !dragging {
            self.raise(l, id);
            self.close_overview(l);
            return;
        }
        let (x, y) = self.mouse;
        let (thumbs, plus) = self.space_thumbs();
        let to = match thumbs.iter().position(|t| t.grow(8).contains(x, y)) {
            Some(i) => i,
            None if plus.is_some_and(|p| p.grow(8).contains(x, y)) => {
                self.spaces += 1;
                self.spaces - 1
            }
            None => return,
        };
        if let Some(w) = self.window_mut(id) {
            w.space = to;
        }
    }

    /// Types a command line into the terminal (opening it if needed).
    fn run(&mut self, l: &mut Looper, cmd: &str) {
        self.open_id(l, "terminal");
        let plain = |key| KeyEvent { key, ctrl: false, alt: false, shift: false };
        for ch in cmd.chars() {
            input::inject(plain(Key::Char(ch)));
        }
        input::inject(plain(Key::Enter));
    }

    fn notify(&mut self, l: &mut Looper, title: String, body: String, icon: String) {
        let (clock, _) = clock_strings();
        if self.history.len() >= 30 {
            self.history.remove(0);
        }
        self.history.push((title.clone(), body.clone(), icon.clone(), clock));
        if let Some(p) = &self.popup {
            if p.kind == PopupKind::Notices {
                l.damage(ui::with_shadow(p.rect));
            }
        }
        if self.quiet {
            return;
        }
        self.notes.retain(|n| !n.gone());
        if self.notes.len() >= 3 {
            self.notes.remove(0);
        }
        self.notes.push(Note { title, body, icon, born: Anim::new(1) });
        l.damage(self.notes_zone());
    }

    /// Where banners come in: the top right corner (under the title row of
    /// a full-screen window).
    fn notes_top(&self) -> i32 {
        if self.full_screen() {
            wm::TITLE_H
        } else {
            0
        }
    }

    fn notes_zone(&self) -> Rect {
        Rect::new(self.w - NOTE_W - 60, self.notes_top(), NOTE_W + 60, 3 * (NOTE_H + 10) + 30)
    }

    fn note_rect(&self, i: usize, slide: u32) -> Rect {
        let x_in = self.w - NOTE_W - 18;
        let x = ui::lerp(self.w + 20, x_in, slide);
        Rect::new(x, self.notes_top() + 12 + i as i32 * (NOTE_H + 10), NOTE_W, NOTE_H)
    }

    fn act(&mut self, l: &mut Looper, from: Option<u32>, action: Action) {
        let damage_window = |s: &mut Desktop, l: &mut Looper| {
            if let Some(w) = from.and_then(|id| s.windows.iter().find(|w| w.id == id)) {
                l.damage(w.bounds());
            }
        };
        match action {
            Action::None => {}
            Action::Redraw => damage_window(self, l),
            Action::Run(cmd) => {
                damage_window(self, l);
                self.run(l, &cmd);
            }
            Action::OpenFile(path) => {
                damage_window(self, l);
                self.open_file(l, &path);
            }
            Action::Open(id) => {
                damage_window(self, l);
                self.open_id(l, id);
            }
            Action::Spawn(path) => {
                damage_window(self, l);
                self.spawn(l, &path);
            }
            Action::Notify(title, body, icon) => {
                damage_window(self, l);
                self.notify(l, title, body, String::from(icon));
            }
            Action::Wallpaper => {
                let (s, b) = res::wallpaper(&settings::get().wallpaper, self.w, self.h);
                self.sharp = s;
                self.blur = b;
                l.damage(self.screen());
            }
        }
    }

    fn open_file(&mut self, l: &mut Looper, path: &str) {
        match apps::for_file(path) {
            apps::Opener::App(app) => {
                self.open(l, app, false);
            }
            apps::Opener::Spawn(p) => self.spawn(l, &p),
            apps::Opener::Run(cmd) => self.run(l, &cmd),
            apps::Opener::Install(file) => {
                let name = String::from(file.rsplit('/').next().unwrap_or(&file));
                self.notify(l, name, String::from("Installazione in corso…"), String::from("store"));
                let spawned = crate::task::spawn("install", move || {
                    let name = file.rsplit('/').next().unwrap_or(&file);
                    match crate::store::install_file(&file) {
                        Ok(msg) => crate::gui::notify(name, &msg, "store"),
                        Err(e) => crate::gui::notify(name, &format!("Non installato: {e}"), "store"),
                    }
                });
                if spawned.is_none() {
                    self.notify(l, String::from("Installazione"), String::from("Memoria insufficiente."), String::from("store"));
                }
            }
            apps::Opener::None => {
                let name = String::from(path.rsplit('/').next().unwrap_or(path));
                self.notify(l, name, String::from("Nessuna app può aprire questo tipo di file."), String::from("file"));
            }
        }
    }

    /// Starts a program that opens a window of its own.
    fn spawn(&mut self, l: &mut Looper, path: &str) {
        let name = apps::program_name(path);
        let home = fs::abs("sys:/home").unwrap_or_else(|_| Path::root("sys"));
        match fs::abs(path).map_err(|_| justdk::syscall::Error::NotFound).and_then(|p| crate::user::spawn(&p, "", home)) {
            Ok(_) => self.bounce = Some((format!("@{path}"), Anim::new(900))),
            Err(e) => self.notify(l, name, format!("Non si è potuto avviare: {}", e.as_str()), String::from("window")),
        }
    }

    fn launch(&mut self, l: &mut Looper, e: Launch) {
        match e {
            Launch::App(id) => self.open_id(l, id),
            Launch::Spawn(path) => self.spawn(l, &path),
            Launch::Run(cmd) => self.run(l, &cmd),
            Launch::File(path) => {
                if fs::abs(&path).is_ok_and(|p| fs::is_dir(&p)) {
                    self.open(l, Box::new(apps::Files::new(&path)), false);
                } else {
                    self.open_file(l, &path);
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // The launcher

    fn toggle_launcher(&mut self, l: &mut Looper) {
        match &mut self.launcher {
            Some(la) if !la.closing => {
                la.closing = true;
                la.anim = Anim::new(200);
            }
            Some(la) => {
                la.closing = false;
                la.settled = false;
                la.anim = Anim::new(260);
            }
            None => {
                self.launcher =
                    Some(Launcher { query: String::new(), entries: apps::catalog(), anim: Anim::new(260), closing: false, settled: false, then: None, scroll: 0, found: Vec::new(), index: None });
            }
        }
        self.popup = None;
        l.damage(self.screen());
    }

    fn close_launcher(&mut self, l: &mut Looper, then: Option<Launch>) {
        if let Some(la) = &mut self.launcher {
            if !la.closing {
                la.closing = true;
                la.anim = Anim::new(200);
            }
            la.then = then;
        }
        l.damage(self.screen());
    }

    fn launcher_tiles(&self, count: usize, scroll: i32) -> Vec<Rect> {
        let cols = 7.min(((self.w - 80) / 128) as usize).max(1);
        let (tw, th) = (128, 120);
        let total_w = cols as i32 * tw;
        let x0 = (self.w - total_w) / 2;
        (0..count).map(|i| Rect::new(x0 + (i % cols) as i32 * tw, 150 + (i / cols) as i32 * th - scroll, tw, th)).collect()
    }

    fn search_rect(&self) -> Rect {
        Rect::new(self.w / 2 - 200, 74, 400, 40)
    }

    /// The rows of search results, under the search field.
    fn found_rows(&self, n: usize) -> Vec<Rect> {
        let w = 560.min(self.w - 32);
        (0..n.min(FOUND_ROWS)).map(|i| Rect::new(self.w / 2 - w / 2 + 6, 134 + i as i32 * 44, w - 12, 42)).collect()
    }

    /// Does what a search result is for: copies a result, opens the rest.
    fn choose_found(&mut self, l: &mut Looper, i: usize) {
        let Some(la) = &self.launcher else { return };
        let Some(f) = la.found.get(i) else { return };
        if let Some(v) = &f.copy {
            ui::copy(v);
            let v = v.clone();
            self.close_launcher(l, None);
            self.notify(l, String::from("Calcolo"), format!("{v} copiato negli appunti."), String::from("calculator"));
            return;
        }
        let then = f.launch.clone();
        self.close_launcher(l, then);
    }

    // ------------------------------------------------------------------
    // Menus and the control centre

    fn menu_act(&mut self, l: &mut Looper, act: MenuAct) {
        match act {
            MenuAct::Restart => self.end(l, Ending::Restart),
            MenuAct::PowerOff => self.end(l, Ending::PowerOff),
            MenuAct::Sleep => crate::sleep::request("sleep chosen from the menu"),
            MenuAct::About => self.open_id(l, "about"),
            MenuAct::Lock => self.lock_screen(l),
            MenuAct::Screenshot => self.screenshot(l),
            MenuAct::Settings => self.open_id(l, "settings"),
            MenuAct::Display => self.open_id(l, "settings"),
            MenuAct::Store => self.open_id(l, "store"),
            MenuAct::Activity => self.open_id(l, "monitor"),
            MenuAct::Overview => self.open_overview(l),
            MenuAct::Widgets => {
                settings::update(|s| s.widgets = !s.widgets);
                for w in self.widgets() {
                    l.damage(ui::with_shadow(w));
                }
            }
            MenuAct::NextWallpaper => {
                let cur = settings::get().wallpaper;
                let i = res::WALLPAPERS.iter().position(|(n, _)| *n == cur).unwrap_or(0);
                let next = res::WALLPAPERS[(i + 1) % res::WALLPAPERS.len()].0;
                settings::update(|s| s.wallpaper = String::from(next));
                self.act(l, None, Action::Wallpaper);
            }
        }
    }

    /// Locks the computer: the lock screen covers everything.
    fn lock_screen(&mut self, l: &mut Looper) {
        if self.lock.is_none() {
            self.close_popup(l);
            if self.launcher.is_some() {
                self.close_launcher(l, None);
            }
            self.drag = None;
            self.lock = Some(Lock::new());
            l.damage(self.screen());
        }
    }

    /// Saves a picture of the screen in Immagini.
    fn screenshot(&mut self, l: &mut Looper) {
        let screen = self.screen();
        let mut shot = Canvas::new_at(self.w, self.h, 0xFF00_0000, super::scale());
        self.paint_desktop(&mut shot, screen);
        let d = now();
        let dir = "sys:/home/Immagini";
        let name = format!("Schermata {}-{:02}-{:02} alle {:02}.{:02}.{:02}.qoi", d.year, d.month, d.day, d.hour, d.min, d.sec);
        let saved = fs::abs(dir).and_then(|p| {
            if !fs::exists(&p) {
                fs::make_dir(&p)?;
            }
            fs::write_file(&p.join(&name), &super::qoi::encode(&shot))
        });
        match saved {
            Ok(()) => self.notify(l, String::from("Schermata salvata"), format!("In Immagini: {name}"), String::from("photos")),
            Err(e) => self.notify(l, String::from("Schermata non salvata"), format!("{e}"), String::from("photos")),
        }
    }

    fn end(&mut self, l: &mut Looper, how: Ending) {
        crate::fs::sync_all();
        for w in self.windows.iter_mut() {
            w.app.close();
        }
        self.ending = Some((how, Anim::new(700)));
        l.damage(self.screen());
    }

    fn show_popup(&mut self, l: &mut Looper, p: Popup) {
        if let Some(old) = self.popup.take() {
            l.damage(self.popup_area(&old));
        }
        l.damage(self.popup_area(&p));
        // The dock settles: the popup may open from it.
        self.mag.target = 0;
        self.popup = Some(p);
    }

    fn close_popup(&mut self, l: &mut Looper) {
        if let Some(p) = self.popup.take() {
            l.damage(self.popup_area(&p));
        }
    }

    /// What a popup paints: the control centre darkens the whole screen
    /// towards the bottom.
    fn popup_area(&self, p: &Popup) -> Rect {
        if p.kind == PopupKind::Control {
            self.screen()
        } else {
            ui::with_shadow(p.rect)
        }
    }

    // The control centre, as in the JustOS design: the account on the
    // darkened wallpaper, then a panel of grey glass with the switches, two
    // tall sliders (volume, brightness) and the power buttons. It opens
    // above the status at the right end of the dock.

    fn control_rect(&self) -> Rect {
        let dock = self.dock_rest();
        let status = self.status_pill(dock);
        let h = CC_HEAD + 12 + 14 + CC_COL + 14 + 56 + 14;
        let x = (status.right() + 6 - CC_W).clamp(12, (self.w - CC_W - 12).max(12));
        Rect::new(x, dock.y - 14 - h, CC_W, h)
    }

    /// The account row and the panel under it.
    fn control_parts(&self) -> (Rect, Rect) {
        let r = self.control_rect();
        (Rect::new(r.x, r.y, r.w, CC_HEAD), Rect::new(r.x, r.y + CC_HEAD + 12, r.w, r.h - CC_HEAD - 12))
    }

    /// The switches, one above the other at the left of the panel.
    fn control_tiles(&self) -> [(Tile, Rect); 3] {
        let (_, p) = self.control_parts();
        let w = p.w - 28 - 2 * CC_SLIDER - 20;
        let at = |row: i32| Rect::new(p.x + 14, p.y + 14 + row * 70, w, 60);
        [(Tile::Network, at(0)), (Tile::Sound, at(1)), (Tile::Quiet, at(2))]
    }

    /// The sliders: volume and brightness, side by side at the right.
    fn control_sliders(&self) -> [Rect; 2] {
        let (_, p) = self.control_parts();
        let x = p.right() - 14 - 2 * CC_SLIDER - 10;
        [Rect::new(x, p.y + 14, CC_SLIDER, CC_COL), Rect::new(x + CC_SLIDER + 10, p.y + 14, CC_SLIDER, CC_COL)]
    }

    /// The bottom row: power off (wide), lock, restart and, on a laptop, the
    /// battery's charge.
    fn control_buttons(&self) -> [Rect; 4] {
        let (_, p) = self.control_parts();
        let y = p.y + 14 + CC_COL + 14;
        let bat = if self.battery.is_some() { 104 } else { 0 };
        let right = p.right() - 14 - if bat > 0 { bat + 10 } else { 0 };
        let restart = Rect::new(right - 56, y, 56, 56);
        let lock = Rect::new(restart.x - 10 - 56, y, 56, 56);
        let off = Rect::new(p.x + 14, y, lock.x - 10 - (p.x + 14), 56);
        [off, lock, restart, Rect::new(p.right() - 14 - bat, y, bat, 56)]
    }

    /// The round settings button at the right of the account row.
    fn settings_btn(&self) -> Rect {
        let (head, _) = self.control_parts();
        Rect::new(head.right() - 50, head.y + (head.h - 46) / 2, 46, 46)
    }

    fn control_hit(&self, x: i32, y: i32) -> Option<ControlHit> {
        if let Some((t, _)) = self.control_tiles().iter().find(|(_, r)| r.contains(x, y)) {
            return Some(ControlHit::Tile(*t));
        }
        let [vol, bri] = self.control_sliders();
        if vol.contains(x, y) {
            return Some(ControlHit::Volume);
        }
        if bri.contains(x, y) {
            return Some(ControlHit::Brightness);
        }
        let [off, lock, restart, _] = self.control_buttons();
        let (head, _) = self.control_parts();
        if off.contains(x, y) {
            Some(ControlHit::PowerOff)
        } else if lock.contains(x, y) {
            Some(ControlHit::Lock)
        } else if restart.contains(x, y) {
            Some(ControlHit::Restart)
        } else if self.settings_btn().contains(x, y) {
            Some(ControlHit::Settings)
        } else if head.contains(x, y) {
            Some(ControlHit::Account)
        } else {
            None
        }
    }

    fn control_press(&mut self, l: &mut Looper, hit: ControlHit, y: i32) {
        match hit {
            ControlHit::Volume => {
                self.drag = Some(Drag::Volume);
                self.set_volume_at(l, y);
            }
            ControlHit::Brightness => {
                self.drag = Some(Drag::Brightness);
                self.set_brightness_at(l, y);
            }
            ControlHit::Tile(Tile::Network) | ControlHit::Settings | ControlHit::Account => {
                self.close_popup(l);
                self.open_id(l, "settings");
            }
            ControlHit::Tile(Tile::Sound) => {
                let v = if crate::dev::audio::volume() == 0 { 60 } else { 0 };
                super::change_volume(v);
                l.damage(ui::with_shadow(self.control_rect()));
                l.damage(self.dock_zone());
            }
            ControlHit::Tile(Tile::Quiet) => {
                self.quiet = !self.quiet;
                l.damage(ui::with_shadow(self.control_rect()));
            }
            ControlHit::Lock => self.lock_screen(l),
            ControlHit::Restart => {
                self.close_popup(l);
                self.end(l, Ending::Restart);
            }
            ControlHit::PowerOff => {
                self.close_popup(l);
                self.end(l, Ending::PowerOff);
            }
        }
    }

    /// The value (0..=100) of a tall slider for a pointer at `y`.
    fn tall_value(r: Rect, y: i32) -> u32 {
        ((r.bottom() - y).clamp(0, r.h) * 100 / r.h.max(1)) as u32
    }

    fn set_volume_at(&mut self, l: &mut Looper, y: i32) {
        super::change_volume(Self::tall_value(self.control_sliders()[0], y));
        l.damage(ui::with_shadow(self.control_rect()));
        l.damage(self.dock_zone());
    }

    fn set_brightness_at(&mut self, l: &mut Looper, y: i32) {
        super::set_brightness(Self::tall_value(self.control_sliders()[1], y).max(20));
        l.damage(self.screen());
    }

    // The notification centre: the month, and the notifications.

    fn notices_rect(&self) -> Rect {
        let top = self.clock_btn().bottom() + 10;
        let w = 640.min(self.w - 32);
        Rect::new((self.w - w) / 2, top, w, 450.min(self.h - top - 110))
    }

    fn notices_parts(&self) -> (Rect, Rect) {
        let r = self.notices_rect();
        let half = (r.w - 10) / 2;
        (Rect::new(r.x, r.y, half, r.h), Rect::new(r.x + half + 10, r.y, r.w - half - 10, r.h))
    }

    fn clear_btn(&self) -> Rect {
        let (_, n) = self.notices_parts();
        Rect::new(n.x + (n.w - 150) / 2, n.bottom() - 46, 150, 32)
    }

    // ------------------------------------------------------------------
    // Input

    fn press(&mut self, l: &mut Looper, x: i32, y: i32, double: bool) {
        if self.ending.is_some() {
            return;
        }
        if self.overview.is_some() {
            self.overview_press(l, x, y);
            return;
        }
        // Notifications go away when clicked.
        for i in (0..self.notes.len()).rev() {
            let r = self.note_rect(i, self.notes[i].slide());
            if r.contains(x, y) {
                l.damage(ui::with_shadow(r));
                self.notes.remove(i);
                l.damage(self.notes_zone());
                return;
            }
        }
        if let Some(p) = &self.popup {
            if p.kind == PopupKind::Control && p.rect.contains(x, y) {
                if let Some(hit) = self.control_hit(x, y) {
                    self.control_press(l, hit, y);
                }
                return;
            }
            if p.kind == PopupKind::Notices && p.rect.contains(x, y) {
                if !self.history.is_empty() && self.clear_btn().contains(x, y) {
                    self.history.clear();
                    l.damage(ui::with_shadow(p.rect));
                } else if self.notices_parts().0.contains(x, y) {
                    self.close_popup(l);
                    self.open_id(l, "calendar");
                }
                return;
            }
            let choice = p.item_rects().into_iter().find(|(r, _)| r.contains(x, y)).and_then(|(_, i)| p.items[i].1);
            self.close_popup(l);
            if let Some(act) = choice {
                self.menu_act(l, act);
            }
            return;
        }
        if let Some(la) = &self.launcher {
            if la.closing {
                return;
            }
            let visible: Vec<Launch> = la.visible().iter().map(|e| e.launch.clone()).collect();
            let top = la.found_h();
            let tiles = self.launcher_tiles(visible.len(), la.scroll - top);
            if let Some(i) = self.found_rows(la.found.len()).iter().position(|r| r.contains(x, y)) {
                self.choose_found(l, i);
            } else if let Some(i) = tiles.iter().position(|t| t.contains(x, y) && y > 130 + top) {
                let what = visible[i].clone();
                self.close_launcher(l, Some(what));
            } else if !self.search_rect().contains(x, y) {
                self.close_launcher(l, None);
            }
            return;
        }
        let items = self.dock_items();
        let (rects, bg) = self.dock_layout(&items);
        let mark = Self::mark_pill(bg);
        if self.dock_shown() && mark.contains(x, y) {
            let mut items = alloc::vec![
                ("Info su JustOS", Some(MenuAct::About)),
                ("", None),
                ("Impostazioni di sistema…", Some(MenuAct::Settings)),
                ("Negozio…", Some(MenuAct::Store)),
                ("Monitor attività", Some(MenuAct::Activity)),
                ("Desktop e finestre (F3)", Some(MenuAct::Overview)),
                ("Schermata", Some(MenuAct::Screenshot)),
                ("", None),
                ("Blocca schermo", Some(MenuAct::Lock)),
            ];
            if crate::sleep::available().is_ok() {
                items.push(("Sospendi", Some(MenuAct::Sleep)));
            }
            items.push(("Riavvia…", Some(MenuAct::Restart)));
            items.push(("Spegni…", Some(MenuAct::PowerOff)));
            let rest = Self::mark_pill(self.dock_rest());
            let p = Popup::menu(rest.x, 0, items, self.w).above(rest.y - DOCK_PAD, 12);
            self.show_popup(l, p);
            return;
        }
        if self.dock_shown() && self.status_pill(bg).contains(x, y) {
            let p = Popup { kind: PopupKind::Control, rect: self.control_rect(), items: Vec::new(), anim: Anim::new(180), up: true, settled: false };
            self.show_popup(l, p);
            return;
        }
        if self.spaces_btn().is_some_and(|b| b.contains(x, y)) {
            self.open_overview(l);
            return;
        }
        if self.clock_btn().contains(x, y) {
            let p = Popup { kind: PopupKind::Notices, rect: self.notices_rect(), items: Vec::new(), anim: Anim::new(180), up: false, settled: false };
            self.show_popup(l, p);
            return;
        }
        if self.dock_shown() && (bg.contains(x, y) || rects.iter().any(|r| r.contains(x, y))) {
            if let Some(i) = rects.iter().position(|r| r.grow(DOCK_GAP / 2).contains(x, y)) {
                self.dock_press(l, &items[i]);
            }
            return;
        }
        // Windows, the top one first.
        let hit = self
            .windows
            .iter()
            .rev()
            .filter(|w| w.shown(self.space) && !self.moving(w.id))
            .find_map(|w| w.hit(x, y).map(|h| (w.id, h)));
        if let Some((id, hit)) = hit {
            if self.focused() != Some(id) {
                self.raise(l, id);
            }
            match hit {
                Hit::Close => self.close_window(l, id),
                Hit::Minimize => self.minimize(l, id),
                Hit::Maximize => self.zoom(l, id),
                Hit::Title => {
                    if double {
                        self.zoom(l, id);
                    } else if let Some(win) = self.window(id) {
                        self.drag = Some(Drag::Move(id, x - win.rect.x, y - win.rect.y));
                    }
                }
                Hit::Resize => {
                    if let Some(win) = self.window(id) {
                        self.drag = Some(Drag::Resize(id, win.rect.right() - x, win.rect.bottom() - y));
                    }
                }
                Hit::Body(bx, by) => {
                    let Some(win) = self.window_mut(id) else { return };
                    let body = win.body();
                    let action = win.app.press(bx, by, double, body);
                    self.drag = Some(Drag::Body(id));
                    self.act(l, Some(id), action);
                }
            }
        }
    }

    fn dock_press(&mut self, l: &mut Looper, item: &DockItem) {
        if item.id == "launcher" {
            self.toggle_launcher(l);
            return;
        }
        // An open window of this kind: show it (the top one).
        let kind = item.kind.as_str();
        let top = self.windows.iter().rev().find(|w| w.app.kind() == kind).map(|w| (w.id, w.minimized));
        match top {
            Some((id, minimized)) => {
                if !minimized && self.focused() == Some(id) && item.id != "trash" {
                    // Already in front: bring up any minimised sibling.
                    if let Some(other) = self.windows.iter().find(|w| w.app.kind() == kind && w.minimized).map(|w| w.id) {
                        self.raise(l, other);
                    }
                } else {
                    self.raise(l, id);
                }
            }
            None => {
                if let Some(launch) = item.launch.clone() {
                    self.launch(l, launch);
                }
            }
        }
    }

    fn key(&mut self, l: &mut Looper, k: KeyEvent) {
        if self.ending.is_some() {
            return;
        }
        if self.overview.is_some() {
            match k.key {
                Key::Escape | Key::Enter | Key::Overview | Key::F(3) => self.close_overview(l),
                Key::Left | Key::PrevDesktop => self.go_to_space(l, self.space.saturating_sub(1)),
                Key::Right | Key::NextDesktop => self.go_to_space(l, self.space + 1),
                _ => {}
            }
            l.damage(self.screen());
            return;
        }
        if k.ctrl && k.key == Key::Char(' ') {
            self.toggle_launcher(l);
            return;
        }
        match k.key {
            Key::Lock => return self.lock_screen(l),
            Key::PrintScreen => {
                self.close_popup(l);
                return self.screenshot(l);
            }
            _ => {}
        }
        if self.popup.is_some() && k.key == Key::Escape {
            self.close_popup(l);
            return;
        }
        if let Some(la) = &mut self.launcher {
            if la.closing {
                return;
            }
            match k.key {
                Key::Escape => self.close_launcher(l, None),
                Key::Enter => {
                    // The first app, or else the first thing found.
                    let first = la.visible().first().map(|e| e.launch.clone());
                    if first.is_some() {
                        self.close_launcher(l, first);
                    } else if !la.found.is_empty() {
                        self.choose_found(l, 0);
                    }
                }
                _ => {
                    if ui::edit_text(&mut la.query, &k, 40) {
                        la.scroll = 0;
                        la.found = apps::search::find(&la.query, &mut la.index);
                    }
                    l.damage(self.screen());
                }
            }
            return;
        }
        // The overview, and the desktop beside: with the logo key as in
        // Windows, F3 as on a Mac, Ctrl+Alt+Page Up/Down.
        let page = k.ctrl && k.alt && !k.shift;
        match k.key {
            Key::Overview | Key::F(3) => return self.open_overview(l),
            Key::PrevDesktop => return self.go_to_space(l, self.space.saturating_sub(1)),
            Key::NextDesktop => return self.go_to_space(l, self.space + 1),
            Key::PageUp if page => return self.go_to_space(l, self.space.saturating_sub(1)),
            Key::PageDown if page => return self.go_to_space(l, self.space + 1),
            _ => {}
        }
        // Alt+Tab: the next window of this desktop comes to the front.
        if k.alt && k.key == Key::Tab {
            let mine = || self.windows.iter().filter(|w| w.space == self.space);
            if let Some(id) = mine().find(|w| !self.moving(w.id)).map(|w| w.id) {
                if mine().count() > 1 {
                    self.raise(l, id);
                }
            }
            return;
        }
        if let Some(id) = self.focused() {
            // Ctrl+Alt+Shift and Left or Right: the window to the desktop
            // beside (a new one after the last).
            if k.ctrl && k.alt && k.shift {
                match k.key {
                    Key::Left | Key::PageUp => {
                        if self.space > 0 {
                            self.send_to_space(l, id, self.space - 1);
                        }
                        return;
                    }
                    Key::Right | Key::PageDown => return self.send_to_space(l, id, self.space + 1),
                    _ => {}
                }
            }
            if k.ctrl && matches!(k.key, Key::Char('q') | Key::Char('Q')) {
                self.close_window(l, id);
                return;
            }
            // F11: full screen and back.
            if k.key == Key::F(11) {
                self.zoom(l, id);
                return;
            }
            // Ctrl+Alt: snap to a half or a quarter, Up fills the screen,
            // Down puts the window back.
            if k.ctrl && k.alt && self.window(id).is_some_and(|w| w.app.resizable()) {
                match k.key {
                    Key::Up => return self.snap(l, id, self.full_area()),
                    Key::Down => return self.unsnap(l, id),
                    key => {
                        if let Some(zone) = self.snap_named(key) {
                            return self.snap(l, id, zone);
                        }
                    }
                }
            }
            if let Some(win) = self.window_mut(id) {
                let action = win.app.key(k);
                self.act(l, Some(id), action);
            }
        }
    }

    fn moved(&mut self, l: &mut Looper, x: i32, y: i32, held: bool) {
        // The clock lights up under the pointer.
        let cb = self.clock_btn();
        let spaces = self.spaces_btn().unwrap_or(Rect::EMPTY);
        let was_on = cb.contains(self.mouse.0, self.mouse.1) || spaces.contains(self.mouse.0, self.mouse.1);
        if was_on != (cb.contains(x, y) || spaces.contains(x, y)) {
            l.damage(self.clock_zone());
        }
        let was = self.mouse;
        self.mouse = (x, y);
        if held && self.carry.is_some() {
            self.carry_move(l, was, x, y);
            return;
        }
        if let Some(o) = &mut self.overview {
            if let Some((_, px, py, dragging)) = &mut o.held {
                if held && ((x - *px).abs() > 6 || (y - *py).abs() > 6) {
                    *dragging = true;
                }
            }
            l.damage(self.screen());
            return;
        }
        if held {
            match self.drag {
                Some(Drag::Move(id, dx, dy)) => {
                    let area = self.screen();
                    let zone = if self.window(id).is_some_and(|w| w.app.resizable()) { self.snap_zone(x, y) } else { None };
                    if zone != self.snap_preview {
                        for r in [self.snap_preview, zone].into_iter().flatten() {
                            l.damage(r.grow(4));
                        }
                        self.snap_preview = zone;
                    }
                    let mut new_drag = None;
                    if let Some(win) = self.window_mut(id) {
                        let before = win.bounds();
                        if let Some(prev) = win.snapped.take() {
                            // Dragging a snapped window: back to its size,
                            // under the pointer.
                            let off = (dx * prev.w / win.rect.w.max(1)).clamp(20, prev.w - 20);
                            win.rect = Rect::new(x - off, y - dy, prev.w, prev.h);
                            new_drag = Some(Drag::Move(id, off, dy));
                        } else if win.restore.take().is_some() {
                            // Dragging a maximised window: back to its size,
                            // under the pointer.
                            let (w, h) = (win.rect.w, win.rect.h);
                            let (ow, oh) = Window::outer_size(win.app.size().0, win.app.size().1);
                            let ow = ow.min(w);
                            win.rect = Rect::new(x - ow / 2, y - 20, ow, oh.min(h));
                            new_drag = Some(Drag::Move(id, ow / 2, 20));
                        } else {
                            win.rect.x = (x - dx).clamp(area.x - win.rect.w + 80, area.right() - 80);
                            win.rect.y = (y - dy).clamp(0, area.bottom() - 60);
                        }
                        let after = win.bounds();
                        l.damage(before.union(&after));
                    }
                    if new_drag.is_some() {
                        self.drag = new_drag;
                    }
                    return;
                }
                Some(Drag::Resize(id, dx, dy)) => {
                    let area = self.work_area();
                    if let Some(win) = self.window_mut(id) {
                        let before = win.bounds();
                        let (mw, mh) = win.min_size();
                        win.rect.w = (x + dx - win.rect.x).clamp(mw, area.right() - win.rect.x);
                        win.rect.h = (y + dy - win.rect.y).clamp(mh, (area.bottom() - win.rect.y).max(mh));
                        let after = win.bounds();
                        l.damage(before.union(&after));
                    }
                    return;
                }
                Some(Drag::Body(id)) => {
                    let mut out = None;
                    if let Some(win) = self.window_mut(id) {
                        let body = win.body();
                        if win.app.drag(x - body.x, y - body.y, body) {
                            l.damage(win.bounds());
                        }
                        // Files dragged out: the desktop carries them now.
                        out = win.app.drag_files().filter(|p| !p.is_empty());
                        if out.is_some() {
                            l.damage(win.bounds());
                        }
                    }
                    if let Some(paths) = out {
                        let icon = fs::abs(&paths[0]).map_or("file", |p| apps::icon_for(p.name(), fs::is_dir(&p)));
                        self.carry = Some(Carry { paths, icon, from: id, over: None, dock: None, copy: false });
                        self.carry_move(l, (x, y), x, y);
                    }
                    return;
                }
                Some(Drag::Volume) => {
                    self.set_volume_at(l, y);
                    return;
                }
                Some(Drag::Brightness) => {
                    self.set_brightness_at(l, y);
                    return;
                }
                None => {}
            }
        }
        if let Some(p) = &self.popup {
            l.damage(p.rect);
        }
        if let Some(la) = &self.launcher {
            if !la.closing {
                l.damage(Rect::new(0, 130, self.w, self.h - 130));
            }
            return;
        }
        // Full screen: the dock comes up at the bottom edge.
        if self.full_screen() {
            let dock_zone = self.dock_zone();
            let dock_reveal = y >= self.h - 3 || (self.dock_reveal && dock_zone.contains(x, y));
            if dock_reveal != self.dock_reveal {
                self.dock_reveal = dock_reveal;
                l.damage(dock_zone);
            }
        } else if self.dock_reveal {
            self.dock_reveal = false;
        }
        // The dock magnifies around the pointer.
        let zone = self.dock();
        let near = Rect::new(zone.x - 20, zone.y - MAGNIFY - 10, zone.w + 40, zone.h + MAGNIFY + 20);
        let inside = near.contains(x, y) && self.popup.is_none() && self.dock_shown();
        self.mag.target = if inside { 1000 } else { 0 };
        let items = self.dock_items();
        let hover = if inside {
            let (rects, _) = self.dock_layout(&items);
            rects.iter().position(|r| r.grow(DOCK_GAP / 2).contains(x, y) || (x >= r.x - DOCK_GAP / 2 && x < r.right() + DOCK_GAP / 2 && y > r.y - 4))
        } else {
            None
        };
        if inside || self.mag.moving() || hover != self.hover {
            l.damage(self.dock_zone());
        }
        self.hover = hover;
        // Window buttons light up; the app under the pointer hears of it.
        let under = self
            .windows
            .iter()
            .rev()
            .filter(|w| w.shown(self.space) && !self.moving(w.id))
            .find(|w| w.rect.contains(x, y))
            .map(|w| w.id);
        if self.hovered != under {
            if let Some(old) = self.hovered.and_then(|id| self.windows.iter_mut().find(|w| w.id == id)) {
                let body = old.body();
                let redraw = old.app.hover(None, body) | old.hover.take().is_some();
                if redraw {
                    l.damage(old.bounds());
                }
            }
            self.hovered = under;
        }
        if let Some(win) = under.and_then(|id| self.windows.iter_mut().find(|w| w.id == id)) {
            let btn = win.buttons().iter().position(|b| b.contains(x, y));
            if btn != win.hover {
                win.hover = btn;
                l.damage(ui::with_shadow(win.title_rect()));
            }
            let body = win.body();
            let at = if body.contains(x, y) { Some((x - body.x, y - body.y)) } else { None };
            if win.app.hover(at, body) {
                l.damage(body);
            }
        }
    }

    // ------------------------------------------------------------------
    // Files carried between windows

    /// The pointer moved while carrying files: the icon follows it, and the
    /// window or dock icon under it hears of them.
    fn carry_move(&mut self, l: &mut Looper, was: (i32, i32), x: i32, y: i32) {
        l.damage(carry_rect(was.0, was.1));
        l.damage(carry_rect(x, y));
        let Some(mut carry) = self.carry.take() else { return };
        let (ctrl, alt, _) = input::mods();
        carry.copy = ctrl || alt;
        // The dock magnifies as usual; some of its icons take files.
        let zone = self.dock();
        let near = Rect::new(zone.x - 20, zone.y - MAGNIFY - 10, zone.w + 40, zone.h + MAGNIFY + 20);
        let inside = near.contains(x, y) && self.popup.is_none() && self.dock_shown();
        self.mag.target = if inside { 1000 } else { 0 };
        let items = self.dock_items();
        let hover = if inside {
            let (rects, _) = self.dock_layout(&items);
            rects.iter().position(|r| r.grow(DOCK_GAP / 2).contains(x, y) || (x >= r.x - DOCK_GAP / 2 && x < r.right() + DOCK_GAP / 2 && y > r.y - 4))
        } else {
            None
        };
        if inside || self.mag.moving() || hover != self.hover {
            l.damage(self.dock_zone());
        }
        self.hover = hover;
        carry.dock = hover.filter(|&i| takes_files(&items[i]));
        // The window under the pointer, if not the dock.
        let under = if inside {
            None
        } else {
            self.windows.iter().rev().filter(|w| w.shown(self.space) && !self.moving(w.id)).find(|w| w.rect.contains(x, y)).map(|w| w.id)
        };
        if carry.over != under {
            if let Some(old) = carry.over.and_then(|id| self.windows.iter_mut().find(|w| w.id == id)) {
                let body = old.body();
                if old.app.drop_over(None, &carry.paths, body) {
                    l.damage(old.bounds());
                }
            }
            carry.over = under;
        }
        if let Some(win) = under.and_then(|id| self.windows.iter_mut().find(|w| w.id == id)) {
            let body = win.body();
            let at = body.contains(x, y).then(|| (x - body.x, y - body.y));
            if win.app.drop_over(at, &carry.paths, body) {
                l.damage(win.bounds());
            }
        }
        self.carry = Some(carry);
    }

    /// The button went up while carrying files: they go to the window or
    /// the dock icon under the pointer, or back where they were.
    fn carry_drop(&mut self, l: &mut Looper, carry: Carry) {
        let (x, y) = self.mouse;
        l.damage(carry_rect(x, y));
        // The window they came from forgets the press.
        if let Some(win) = self.window_mut(carry.from) {
            let body = win.body();
            let _ = win.app.release(body);
            l.damage(win.bounds());
        }
        if let Some(i) = carry.dock {
            let items = self.dock_items();
            let Some(item) = items.get(i) else { return };
            l.damage(self.dock_zone());
            if item.kind == "trash" {
                if let Err(e) = dnd::to_bin(&carry.paths) {
                    self.notify(l, String::from("Cestino"), e, String::from("trash"));
                }
                self.refresh_all(l);
                return;
            }
            let mut refused = Vec::new();
            for p in carry.paths.iter().take(8) {
                match apps::open_with(&item.kind, p) {
                    Some(app) => {
                        self.open(l, app, false);
                    }
                    None => refused.push(fs::abs(p).map(|p| String::from(p.name())).unwrap_or_default()),
                }
            }
            if !refused.is_empty() {
                let what = if refused.len() == 1 { refused.remove(0) } else { format!("{} elementi", refused.len()) };
                self.notify(l, item.label.clone(), format!("Non apre {what}."), item.icon.clone());
            }
            return;
        }
        let Some(id) = carry.over else { return };
        let Some(win) = self.window_mut(id) else { return };
        let body = win.body();
        let _ = win.app.drop_over(None, &carry.paths, body);
        l.damage(win.bounds());
        if !body.contains(x, y) {
            return;
        }
        if let Some(action) = win.app.drop_files(&carry.paths, x - body.x, y - body.y, body) {
            if id != carry.from && self.focused() != Some(id) {
                self.raise(l, id);
            }
            self.act(l, Some(id), action);
            self.refresh_all(l);
        }
    }

    /// Files moved or copied: every window shows them again.
    fn refresh_all(&mut self, l: &mut Looper) {
        for w in self.windows.iter_mut() {
            w.app.refresh();
            if w.shown(self.space) {
                l.damage(w.bounds());
            }
        }
    }

    fn draw_carry(&self, c: &mut Canvas, carry: &Carry) {
        let (x, y) = self.mouse;
        let (ix, iy) = (x - 26, y - 20);
        c.shadow(Rect::new(ix + 6, iy + 8, 40, 40), 12, 12, 50);
        res::draw_icon_faded(c, carry.icon, 52, ix, iy, 215);
        if carry.paths.len() > 1 {
            let n = format!("{}", carry.paths.len());
            let w = (text::width(Font::Small, &n) + 12).max(22);
            let b = Rect::new(ix + 52 - w / 2, iy - 6, w, 22);
            c.fill_round(b, 11, 0xFFFF_453A);
            let (sh, _) = text::metrics(Font::Small);
            text::draw_centered(c, Font::Small, b.x + b.w / 2, b.y + (b.h - sh) / 2, &n, TEXT);
        }
        if carry.copy {
            let (cx, cy) = (ix + 48, iy + 46);
            c.fill_circle(cx, cy, 11, 0xFF30_D158);
            c.fill(Rect::new(cx - 5, cy - 1, 10, 2), TEXT);
            c.fill(Rect::new(cx - 1, cy - 5, 2, 10), TEXT);
        }
        let label = text::fit(Font::Small, &dnd::describe(&carry.paths), 170);
        let w = text::width(Font::Small, &label) + 18;
        // Under the icon, or over it at the bottom of the screen.
        let below = iy + 58;
        let pill = Rect::new(x - w / 2, if below + 22 > self.h - 2 { iy - 30 } else { below }, w, 22);
        c.fill_round(pill, 11, 0xD91C_1C1E);
        let (sh, _) = text::metrics(Font::Small);
        text::draw_centered(c, Font::Small, x, pill.y + (pill.h - sh) / 2, &label, TEXT);
    }

    fn released(&mut self, l: &mut Looper) {
        if self.overview.is_some() {
            self.overview_release(l);
            return;
        }
        if let Some(carry) = self.carry.take() {
            self.drag = None;
            self.carry_drop(l, carry);
            return;
        }
        match self.drag {
            Some(Drag::Body(id)) => {
                if let Some(win) = self.window_mut(id) {
                    let body = win.body();
                    let action = win.app.release(body);
                    self.act(l, Some(id), action);
                }
            }
            Some(Drag::Move(id, ..)) => {
                if let Some(zone) = self.snap_preview.take() {
                    l.damage(zone.grow(4));
                    self.snap(l, id, zone);
                }
            }
            _ => {}
        }
        self.drag = None;
    }

    // ------------------------------------------------------------------
    // Drawing

    /// The clock and the date floating at the top, in white or ink,
    /// whichever reads on the wallpaper (on one line in the title row of a
    /// full-screen window), and the desktops' dots under them.
    fn draw_clock(&self, c: &mut Canvas) {
        let cb = self.clock_btn();
        let (clock, date) = clock_strings();
        let lit = cb.contains(self.mouse.0, self.mouse.1) || self.popup.as_ref().is_some_and(|p| p.kind == PopupKind::Notices);
        if self.full_screen() {
            // On the window's frame, like its name.
            let ink = ui::wants_ink(&self.blur, cb);
            if lit {
                c.fill_round(cb, cb.h / 2, if ink { 0x1F00_0000 } else { 0x26FF_FFFF });
            }
            let fg = if ink { ui::INK } else { TEXT };
            let (sh, _) = text::metrics(Font::Strong);
            let y = cb.y + (cb.h - sh) / 2;
            let w = text::draw(c, Font::Strong, cb.x + 14, y, &clock, fg);
            text::draw(c, Font::Strong, cb.x + 14 + w + 12, y, &date, fg);
            return;
        }
        let ink = ui::luminance(&self.blur, cb) > 150;
        let (fg, shade, hover_bg) = if ink { (ui::INK, 0x40FF_FFFF, 0x1F00_0000) } else { (TEXT, 0x4D00_0000, 0x26FF_FFFF) };
        if lit {
            c.fill_round(cb, 20, hover_bg);
        }
        let (th, _) = text::metrics(Font::Title);
        let cx = cb.x + cb.w / 2;
        // A soft shade under the text keeps it readable on busy pictures.
        for (dy, col) in [(1, shade), (0, fg)] {
            text::draw_centered(c, Font::Title, cx, cb.y + 5 + dy, &clock, col);
            text::draw_centered(c, Font::Strong, cx, cb.y + 5 + th + dy, &date, col);
        }
        if let Some(sp) = self.spaces_btn() {
            if sp.contains(self.mouse.0, self.mouse.1) {
                c.fill_round(sp, sp.h / 2, hover_bg);
            }
            for i in 0..self.spaces {
                let col = if i == self.space { fg } else { (fg & 0xFF_FFFF) | 0x5900_0000 };
                c.fill_circle(sp.x + 16 + i as i32 * 14, sp.y + sp.h / 2, 4, col);
            }
        }
    }

    fn draw_widgets(&self, c: &mut Canvas, r: Rect) {
        let n = self.show_widgets();
        let ws = self.widgets();
        let d = now();
        let name = settings::get().name;
        // Light glass; ink or white text, whichever reads on the wallpaper:
        // (text, dimmed text).
        let pane = |c: &mut Canvas, g: Rect| -> (u32, u32) {
            ui::light_glass(c, &self.blur, g, 24, true);
            if ui::wants_ink(&self.blur, g) {
                (ui::INK, ui::INK_DIM)
            } else {
                (TEXT, TEXT_DIM)
            }
        };
        if n > 0 && r.overlaps(&ui::with_shadow(ws[0])) {
            let g = ws[0];
            let (fg, dim) = pane(c, g);
            let hello = match d.hour {
                5..=11 => "Buongiorno",
                12..=17 => "Buon pomeriggio",
                _ => "Buonasera",
            };
            // The greeting (the clock floats at the top already).
            let date = format!("{} {} {}", WEEKDAYS[d.weekday as usize % 7], d.day, MONTHS[(d.month as usize + 11) % 12]);
            ui::caps(c, g.x + 22, g.y + 20, &date, dim);
            text::draw(c, Font::Title, g.x + 20, g.y + 40, &text::fit(Font::Title, hello, g.w - 40), fg);
            if !name.is_empty() {
                text::draw(c, Font::Heading, g.x + 22, g.y + 88, &text::fit(Font::Heading, &name, g.w - 44), dim);
            }
        }
        if n > 1 && r.overlaps(&ui::with_shadow(ws[1])) {
            let g = ws[1];
            let (fg, dim) = pane(c, g);
            draw_month(c, g, &d, fg, dim);
        }
        if n > 2 && r.overlaps(&ui::with_shadow(ws[2])) {
            let g = ws[2];
            let (fg, dim) = pane(c, g);
            ui::caps(c, g.x + 20, g.y + 20, "Sistema", dim);
            let (free, total) = crate::mem::frame::stats();
            let used = total.saturating_sub(free);
            let bar = Rect::new(g.x + 20, g.y + 50, g.w - 40, 8);
            c.fill_round(bar, 4, (fg & 0xFF_FFFF) | 0x2600_0000);
            let fw = (bar.w as u64 * used).checked_div(total).unwrap_or(0) as i32;
            c.fill_round(Rect::new(bar.x, bar.y, fw.max(8), bar.h), 4, fg);
            text::draw(c, Font::Small, g.x + 20, g.y + 66, &format!("Memoria: {} in uso su {}", Size(used), Size(total)), dim);
            let up = time::uptime_ms() / 60_000;
            let net = if crate::net::online() { "connessa" } else { "assente" };
            text::draw(c, Font::Small, g.x + 20, g.y + 88, &format!("Acceso da {} h {} min · rete {net}", up / 60, up % 60), dim);
        }
    }

    fn draw_dock(&self, c: &mut Canvas) {
        let items = self.dock_items();
        let (rects, bg) = self.dock_layout(&items);
        ui::light_glass(c, &self.blur, bg, 26, true);
        // The JustOS mark on a dark pill: it opens the system menu.
        let mark = Self::mark_pill(bg);
        let menu = self.popup.as_ref().is_some_and(|p| p.kind == PopupKind::Menu && p.up);
        let on_mark = menu || mark.contains(self.mouse.0, self.mouse.1);
        c.fill_round(mark, mark.h / 2, if on_mark { 0xFF2C_2A29 } else { 0xFF14_1414 });
        draw_mark(c, mark.x + mark.w / 2, mark.y + mark.h / 2, 22, 0xFFF2_EFE6);
        // The status on a white pill: network, sound and the battery's
        // ring; it opens the control centre.
        let sp = self.status_pill(bg);
        let open = self.popup.as_ref().is_some_and(|p| p.kind == PopupKind::Control);
        let on_status = open || sp.contains(self.mouse.0, self.mouse.1);
        c.fill_round(sp, sp.h / 2, if on_status { 0xF2FF_FFFF } else { 0xB8FF_FFFF });
        let gy = sp.y + (sp.h - 22) / 2;
        let net = if crate::net::online() { ui::INK } else { 0x661B_1A1F };
        ui::glyph(c, "g-wifi", 22, sp.x + 16, gy, net);
        let vol = if crate::dev::audio::volume() == 0 { "g-mute" } else { "g-volume" };
        ui::glyph(c, vol, 22, sp.x + 52, gy, if self.quiet { ui::INK_DIM } else { ui::INK });
        if let Some((pct, charging)) = self.battery {
            draw_battery_ring(c, sp.x + 106, sp.y + sp.h / 2, 16, 5, pct, charging, 0x331B_1A1F, ui::INK);
        }
        for (i, it) in items.iter().enumerate() {
            let r = rects[i];
            if it.split && i > 0 {
                let x = r.x - (SEP_W + DOCK_GAP) / 2 - 1;
                c.fill(Rect::new(x, bg.y + 16, 1, bg.h - 32), 0x40FF_FFFF);
            }
            let mut lift = 0;
            if let Some((what, a)) = &self.bounce {
                if *what == it.kind || (what.starts_with('@') && it.icon == *what) {
                    // Two hops, the second lower.
                    let t = a.t() as i64;
                    let (phase, height) = if t < 600 { (t * 1000 / 600, 22) } else { ((t - 600) * 1000 / 400, 10) };
                    lift = (height * 4 * phase * (1000 - phase) / 1_000_000) as i32;
                }
            }
            if self.carry.as_ref().is_some_and(|c| c.dock == Some(i)) {
                // It takes the files carried over it.
                c.fill_round(r.grow(6), 18, 0x59FF_FFFF);
            }
            res::draw_icon(c, &it.icon, r.w, r.x, r.y - lift);
            let open = self.windows.iter().any(|w| w.app.kind() == it.kind) && it.id != "trash";
            if open {
                c.fill_circle(r.x + r.w / 2, bg.bottom() - 6, 2, 0xE6FF_FFFF);
            }
        }
        if let Some(i) = self.hover.filter(|&i| i < items.len()) {
            let r = rects[i];
            let label = &items[i].label;
            let w = text::width(Font::Strong, label) + 24;
            let tip = Rect::new(r.x + r.w / 2 - w / 2, r.y - 44, w, 30);
            ui::glass(c, &self.blur, tip, 15, ui::GLASS_STRONG);
            text::draw_centered(c, Font::Strong, tip.x + w / 2, tip.y + 6, label, TEXT);
        }
    }

    fn draw_launcher(&self, c: &mut Canvas, la: &Launcher) {
        let t = la.shown();
        let full = t >= 1000;
        let screen = self.screen();
        if full {
            c.blit(&self.blur, 0, 0, screen);
        } else {
            let cl = c.clip();
            c.blit_faded(&self.blur, cl.x, cl.y, cl, t * 255 / 1000);
        }
        c.fill(c.clip(), ((0x8C * t / 1000) << 24) | 0x10_0A16);
        let alpha = |col: u32| (((col >> 24) * t / 1000) << 24) | (col & 0xFF_FFFF);
        let s = self.search_rect();
        let s = Rect::new(s.x, s.y - (1000 - t as i32) * 20 / 1000, s.w, s.h);
        c.fill_round(s, 20, alpha(0x33FF_FFFF));
        c.stroke_round(s, 20, 1, alpha(ui::BORDER));
        res::draw_icon_faded(c, "g-search", 16, s.x + 16, s.y + 12, t * 255 / 1000);
        let (lh, _) = text::metrics(Font::Body);
        if la.query.is_empty() {
            text::draw(c, Font::Body, s.x + 42, s.y + (s.h - lh) / 2, "Cerca", alpha(TEXT_FAINT));
        } else {
            let w = text::draw(c, Font::Body, s.x + 42, s.y + (s.h - lh) / 2, &la.query, alpha(TEXT));
            c.fill(Rect::new(s.x + 44 + w, s.y + 10, 2, s.h - 20), alpha(0xFFFF_FFFF));
        }
        let visible = la.visible();
        let top = la.found_h();
        if visible.is_empty() && la.found.is_empty() {
            text::draw_centered(c, Font::Body, self.w / 2, 190, "Nessun risultato", alpha(TEXT_DIM));
        }
        // Calculations, settings and files, in a glass list.
        let rows = self.found_rows(la.found.len());
        if let (Some(first), Some(last)) = (rows.first(), rows.last()) {
            let panel = Rect::new(first.x - 6, first.y - 6, first.w + 12, last.bottom() - first.y + 12);
            c.fill_round(panel, 18, alpha(0x40FF_FFFF));
            c.stroke_round(panel, 18, 1, alpha(ui::BORDER));
            for (f, r) in la.found.iter().zip(&rows) {
                if full && r.contains(self.mouse.0, self.mouse.1) {
                    c.fill_round(*r, 12, 0x22FF_FFFF);
                }
                res::draw_icon_faded(c, f.icon, 28, r.x + 10, r.y + 7, t * 255 / 1000);
                let font = if f.copy.is_some() { Font::Heading } else { Font::Strong };
                let (lh, _) = text::metrics(font);
                let tag_w = text::width(Font::Small, f.section) + 12;
                let name = text::fit(font, &f.name, r.w / 2);
                let w = text::draw(c, font, r.x + 50, r.y + (r.h - lh) / 2, &name, alpha(TEXT));
                let detail = text::fit(Font::Small, &f.detail, r.w - 50 - w - 24 - tag_w);
                let (sh, _) = text::metrics(Font::Small);
                text::draw(c, Font::Small, r.x + 62 + w, r.y + (r.h - sh) / 2, &detail, alpha(TEXT_FAINT));
                text::draw_right(c, Font::Small, r.right() - 12, r.y + (r.h - sh) / 2, f.section, alpha(TEXT_DIM));
            }
        }
        // Tiles zoom in from a little larger, as in Launchpad.
        let zoom = ui::lerp(1120, 1000, t);
        let cx = self.w / 2;
        let saved = c.clip();
        c.set_clip(saved.intersect(&Rect::new(0, 128 + top, self.w, self.h - 128 - top)));
        for (e, tile) in visible.iter().zip(self.launcher_tiles(visible.len(), la.scroll - top)) {
            let tile = Rect::new(cx + (tile.x - cx) * zoom / 1000, 150 + (tile.y - 150) * zoom / 1000, tile.w, tile.h);
            if tile.bottom() < 128 || tile.y > self.h {
                continue;
            }
            if full && tile.contains(self.mouse.0, self.mouse.1) {
                c.fill_round(tile.grow(-6), 18, 0x22FF_FFFF);
            }
            let size = (64 * zoom / 1000) & !1;
            res::draw_icon_faded(c, &e.icon, size, tile.x + (tile.w - size) / 2, tile.y + 12, t * 255 / 1000);
            let label = text::fit(Font::Body, &e.name, tile.w - 12);
            text::draw_centered(c, Font::Body, tile.x + tile.w / 2, tile.y + 86, &label, alpha(TEXT));
        }
        c.set_clip(saved);
        text::draw_centered(c, Font::Small, self.w / 2, self.h - 36, "Ctrl+Spazio apre e chiude · Esc per uscire", alpha(TEXT_FAINT));
    }

    fn draw_popup(&self, c: &mut Canvas, p: &Popup) {
        let saved = c.clip();
        c.set_clip(saved.intersect(&p.shown()));
        match p.kind {
            PopupKind::Menu => {
                ui::glass(c, &self.blur, p.rect, 16, ui::GLASS_STRONG);
                let mut y = p.rect.y + 6;
                for (label, act) in &p.items {
                    if act.is_none() {
                        c.fill(Rect::new(p.rect.x + 14, y + 5, p.rect.w - 28, 1), ui::SEPARATOR);
                        y += 11;
                        continue;
                    }
                    let r = Rect::new(p.rect.x + 6, y, p.rect.w - 12, 32);
                    if r.contains(self.mouse.0, self.mouse.1) {
                        c.fill_round(r, 8, ui::SELECTED);
                    }
                    text::draw(c, Font::Body, r.x + 12, r.y + 7, label, TEXT);
                    y += 34;
                }
            }
            PopupKind::Control => self.draw_control(c),
            PopupKind::Notices => self.draw_notices(c),
        }
        c.set_clip(saved);
    }

    fn draw_control(&self, c: &mut Canvas) {
        let (head, panel) = self.control_parts();
        let hover = self.control_hit(self.mouse.0, self.mouse.1);
        let lit = |h: ControlHit| hover == Some(h);
        // The account, on the darkened wallpaper, and the settings.
        let account = crate::ticly::account();
        let (name, mail, initials) = match &account {
            Some(a) => (if a.name.is_empty() { a.email.clone() } else { a.name.clone() }, a.email.clone(), a.initials()),
            None => {
                let n = settings::get().name;
                let n = if n.is_empty() { String::from("Utente") } else { n };
                let i: String = n.chars().take(1).flat_map(|ch| ch.to_uppercase()).collect();
                (n, String::from("Nessun account ticly"), i)
            }
        };
        let sb = self.settings_btn();
        if lit(ControlHit::Account) {
            c.fill_round(Rect::new(head.x, head.y + 4, sb.x - 8 - head.x, head.h - 8), (head.h - 8) / 2, 0x1FFF_FFFF);
        }
        let (acx, acy, ar) = (head.x + 6 + 26, head.y + head.h / 2, 26);
        c.fill_circle(acx, acy, ar, 0xFFF4_F2F5);
        let (hh, _) = text::metrics(Font::Heading);
        text::draw_centered(c, Font::Heading, acx, acy - hh / 2, &initials, ui::INK);
        let tx = acx + ar + 14;
        let tw = sb.x - tx - 10;
        text::draw(c, Font::Heading, tx, acy - hh + 1, &text::fit(Font::Heading, &name, tw), TEXT);
        text::draw(c, Font::Small, tx, acy + 3, &text::fit(Font::Small, &mail, tw), TEXT_DIM);
        ui::glass(c, &self.blur, sb, sb.h / 2, if lit(ControlHit::Settings) { 0xD95A_5860 } else { 0xB33A_383F });
        draw_gear(c, sb.x + sb.w / 2, sb.y + sb.h / 2, 11, TEXT);

        // The panel of grey glass.
        ui::card(c, &self.blur, panel, 32, 0xB85C_5961);
        // The switches: a round symbol, the name and the state; the network
        // turns blue when it is on, the others white.
        let online = crate::net::online();
        let vol = crate::dev::audio::volume();
        for (tile, r) in self.control_tiles() {
            let (on, title, state) = match tile {
                Tile::Network => match crate::net::address() {
                    Some(a) if online => (true, "Rete", format!("{a}")),
                    _ => (false, "Rete", String::from("Assente")),
                },
                Tile::Sound => (vol > 0, "Suono", String::from(if vol > 0 { "Attivo" } else { "Muto" })),
                Tile::Quiet => (self.quiet, "Non disturbare", String::from(if self.quiet { "Attivo" } else { "Spento" })),
            };
            let hot = lit(ControlHit::Tile(tile));
            let (bg, knob, fg, dim) = match (on, tile) {
                (true, Tile::Network) => (if hot { 0xFF4A_8DF5 } else { BLUE }, 0xFF5C_9AF7, TEXT, 0xE0FF_FFFF),
                (true, _) => (if hot { 0xFFFF_FFFF } else { 0xFFF4_F2F5 }, 0xFFE2_E0E6, ui::INK, ui::INK_DIM),
                (false, _) => (if hot { 0x40FF_FFFF } else { 0x29FF_FFFF }, 0xFF6E_6B74, TEXT, TEXT_DIM),
            };
            c.fill_round(r, 22, bg);
            let (cx, cy, rad) = (r.x + 10 + 20, r.y + r.h / 2, 20);
            c.fill_circle(cx, cy, rad, knob);
            match tile {
                Tile::Network => ui::glyph(c, "g-wifi", 20, cx - 10, cy - 10, fg),
                Tile::Sound => ui::glyph(c, if vol > 0 { "g-volume" } else { "g-mute" }, 20, cx - 10, cy - 10, fg),
                Tile::Quiet => ui::draw_moon(c, cx - 10, cy - 10, 20, fg, knob),
            }
            let tx = cx + rad + 10;
            let tw = r.right() - tx - 10;
            text::draw(c, Font::Strong, tx, r.y + 11, &text::fit(Font::Strong, title, tw), fg);
            text::draw(c, Font::Small, tx, r.y + 31, &text::fit(Font::Small, &state, tw), dim);
        }
        // Volume and brightness: tall dark pills filled with white from the
        // bottom, their symbol at the foot.
        for (i, s) in self.control_sliders().iter().enumerate() {
            let v = if i == 0 { vol } else { super::brightness() };
            c.fill_round(*s, s.w / 2, 0xF214_1316);
            let fill = (s.h * v.min(100) as i32 / 100).max(s.w);
            c.fill_round(Rect::new(s.x, s.bottom() - fill, s.w, fill), s.w / 2, 0xFFF4_F2F5);
            let (gx, gy) = (s.x + (s.w - 24) / 2, s.bottom() - s.w / 2 - 12);
            if i == 0 {
                ui::glyph(c, if vol == 0 { "g-mute" } else { "g-volume" }, 24, gx, gy, ui::INK);
            } else {
                ui::draw_sun(c, gx, gy, 24, ui::INK);
            }
        }
        // Power off on a wide white pill; lock and restart; the battery.
        let [off, lock, restart, bat] = self.control_buttons();
        c.fill_round(off, off.h / 2, if lit(ControlHit::PowerOff) { 0xFFFF_FFFF } else { 0xFFF4_F2F5 });
        let (sh, _) = text::metrics(Font::Strong);
        let gx = off.x + (off.w - 22 - 10 - text::width(Font::Strong, "Spegni")) / 2;
        ui::glyph(c, "g-power", 22, gx, off.y + (off.h - 22) / 2, ui::INK);
        text::draw(c, Font::Strong, gx + 32, off.y + (off.h - sh) / 2, "Spegni", ui::INK);
        for (r, h) in [(lock, ControlHit::Lock), (restart, ControlHit::Restart)] {
            let (cx, cy) = (r.x + r.w / 2, r.y + r.h / 2);
            c.fill_circle(cx, cy, r.w / 2, if lit(h) { 0x40FF_FFFF } else { 0x29FF_FFFF });
            if h == ControlHit::Lock {
                draw_lock(c, cx, cy, 22, TEXT);
            } else {
                ui::glyph(c, "g-reload", 22, cx - 11, cy - 11, TEXT);
            }
        }
        if let Some((pct, charging)) = self.battery {
            c.fill_round(bat, bat.h / 2, 0x29FF_FFFF);
            draw_battery_ring(c, bat.x + 28, bat.y + bat.h / 2, 15, 4, pct, charging, 0x33FF_FFFF, TEXT);
            text::draw(c, Font::Strong, bat.x + 52, bat.y + (bat.h - sh) / 2, &format!("{pct} %"), TEXT);
        }
    }

    fn draw_notices(&self, c: &mut Canvas) {
        const PANE: u32 = 0xA66E_6B74;
        let (cal, list) = self.notices_parts();
        ui::card(c, &self.blur, cal, 30, PANE);
        ui::card(c, &self.blur, list, 30, PANE);
        draw_month(c, Rect::new(cal.x + 6, cal.y + 8, cal.w - 12, cal.h - 16), &now(), TEXT, TEXT_DIM);
        if self.history.is_empty() {
            let (lh, _) = text::metrics(Font::Body);
            text::draw_centered(c, Font::Body, list.x + list.w / 2, list.y + list.h / 2 - lh, "Nessuna notifica", TEXT_DIM);
            return;
        }
        let saved = c.clip();
        c.set_clip(saved.intersect(&Rect::new(list.x, list.y + 8, list.w, list.h - 64)));
        let mut y = list.y + 10;
        for (title, body, icon, at) in self.history.iter().rev() {
            if y > list.bottom() - 64 {
                break;
            }
            let r = Rect::new(list.x + 10, y, list.w - 20, 84);
            c.fill_round(r, 26, 0x29FF_FFFF);
            c.stroke_round(r, 26, 1, 0x33FF_FFFF);
            res::draw_icon(c, icon, 52, r.x + 14, r.y + 16);
            let tx = r.x + 78;
            let tw = r.right() - tx - 14;
            let tw_title = tw - text::width(Font::Small, at) - 8;
            text::draw(c, Font::Heading, tx, r.y + 16, &text::fit(Font::Heading, title, tw_title), TEXT);
            text::draw_right(c, Font::Small, r.right() - 16, r.y + 20, at, TEXT_DIM);
            if let Some(line) = text::wrap(Font::Small, body, tw).first() {
                text::draw(c, Font::Small, tx, r.y + 46, line, TEXT_DIM);
            }
            y += 92;
        }
        c.set_clip(saved);
        let cb = self.clear_btn();
        ui::button(c, cb, "Cancella tutto", false, cb.contains(self.mouse.0, self.mouse.1));
    }

    fn draw_overview(&self, c: &mut Canvas, o: &Overview) {
        let t = o.shown();
        let cl = c.clip();
        c.blit(&self.sharp, cl.x, cl.y, cl);
        c.blit_faded(&self.blur, cl.x, cl.y, cl, t * 255 / 1000);
        c.fill(cl, ((0x66 * t / 1000) << 24) | 0x10_0A16);
        let alpha = |col: u32| (((col >> 24) * t / 1000) << 24) | (col & 0xFF_FFFF);
        let fade = t * 255 / 1000;
        let (mx, my) = self.mouse;
        let still = t >= 1000 && !o.closing;
        let dragging = o.held.filter(|h| h.3);
        // The desktops come down from the top, each with its windows.
        let drop = (1000 - t as i32) * 60 / 1000;
        let (thumbs, plus) = self.space_thumbs();
        let (sh, _) = text::metrics(Font::Small);
        for (i, th) in thumbs.iter().enumerate() {
            let th = th.offset(0, -drop);
            c.draw_scaled(&self.sharp, th, fade);
            let saved = c.clip();
            c.set_clip(saved.intersect(&th));
            for (id, pic, at, r) in &o.snaps {
                if self.window(*id).is_some_and(|w| w.shown(i)) {
                    let to = Rect::new(th.x + r.x * th.w / self.w, th.y + r.y * th.h / self.h, r.w * th.w / self.w, r.h * th.h / self.h);
                    c.draw_scaled(pic, place_snap(*at, *r, to), fade);
                }
            }
            c.set_clip(saved);
            let target = dragging.is_some() && th.grow(8).contains(mx, my);
            if target {
                c.stroke_round(th.grow(3), 8, 3, ui::ACCENT);
            } else if i == self.space {
                c.stroke_round(th.grow(3), 8, 3, alpha(0xF0FF_FFFF));
            } else {
                c.stroke_round(th, 6, 1, alpha(0x59FF_FFFF));
            }
            text::draw_centered(c, Font::Small, th.x + th.w / 2, th.bottom() + 8, &format!("Desktop {}", i + 1), alpha(TEXT));
            if still && dragging.is_none() && self.spaces > 1 && th.grow(10).contains(mx, my) {
                let b = space_close(th);
                c.fill_circle(b.x + b.w / 2, b.y + b.h / 2, b.w / 2, ui::GLASS_STRONG | 0xFF00_0000);
                c.stroke_round(b, b.w / 2, 1, ui::BORDER);
                text::draw_centered(c, Font::Small, b.x + b.w / 2, b.y + (b.h - sh) / 2, "×", TEXT);
            }
        }
        if let Some(p) = plus {
            let p = p.offset(0, -drop);
            let hot = (still && p.contains(mx, my)) || (dragging.is_some() && p.grow(8).contains(mx, my));
            c.fill_round(p, 10, alpha(if hot { 0x4DFF_FFFF } else { 0x26FF_FFFF }));
            c.stroke_round(p, 10, 1, if dragging.is_some() && hot { ui::ACCENT } else { alpha(ui::BORDER) });
            let (hh, _) = text::metrics(Font::Heading);
            text::draw_centered(c, Font::Heading, p.x + p.w / 2, p.y + (p.h - hh) / 2, "+", alpha(TEXT));
        }
        // This desktop's windows glide from their places to the grid.
        let slots = self.overview_slots();
        if slots.is_empty() {
            text::draw_centered(c, Font::Body, self.w / 2, self.h / 2, "Nessuna finestra su questo desktop", alpha(TEXT_DIM));
        }
        let mut lifted = None;
        for &(id, to) in &slots {
            let Some((_, pic, at, r)) = o.snaps.iter().find(|s| s.0 == id) else { continue };
            if dragging.is_some_and(|h| h.0 == id) {
                lifted = Some((pic, *at, *r, to));
                continue;
            }
            let frame = ui::lerp_rect(*r, to, t);
            c.draw_scaled(pic, place_snap(*at, *r, frame), 255);
            if still && dragging.is_none() && to.contains(mx, my) {
                c.stroke_round(to.grow(4), 16, 3, 0xD9FF_FFFF);
                let title = self.window(id).map(|w| w.app.title()).unwrap_or_default();
                let name = text::fit(Font::Strong, &title, to.w.max(180));
                let w = text::width(Font::Strong, &name) + 24;
                let tip = Rect::new(to.x + to.w / 2 - w / 2, to.bottom() + 10, w, 28);
                c.fill_round(tip, 14, ui::GLASS_STRONG);
                text::draw_centered(c, Font::Strong, tip.x + w / 2, tip.y + 5, &name, TEXT);
            }
        }
        // The window being dragged follows the pointer, on top.
        if let (Some((pic, at, r, to)), Some((_, px, py, _))) = (lifted, dragging) {
            let frame = to.offset(mx - px, my - py);
            c.draw_scaled(pic, place_snap(at, r, frame), 220);
        }
        let hint = "Clic su una finestra per sceglierla · trascinala su un desktop per spostarla · Esc per uscire";
        text::draw_centered(c, Font::Small, self.w / 2, self.h - 30, hint, alpha(TEXT_FAINT));
    }

    /// "Desktop 2" and a dot for each desktop, for a moment after a switch.
    fn draw_space_hud(&self, c: &mut Canvas, a: &Anim) {
        let left = SPACE_HUD_MS.saturating_sub(a.elapsed());
        let t = if left < 250 { (left * 1000 / 250) as u32 } else { 1000 };
        let alpha = |col: u32| (((col >> 24) * t / 1000) << 24) | (col & 0xFF_FFFF);
        let r = self.space_hud_rect();
        c.fill_round(r, 22, alpha(ui::GLASS_STRONG));
        c.stroke_round(r, 22, 1, alpha(ui::BORDER));
        text::draw_centered(c, Font::Heading, r.x + r.w / 2, r.y + 22, &format!("Desktop {}", self.space + 1), alpha(TEXT));
        let n = self.spaces as i32;
        let x0 = r.x + r.w / 2 - (n - 1) * 9;
        for i in 0..n {
            let col = if i as usize == self.space { 0xFFFF_FFFF } else { 0x59FF_FFFF };
            c.fill_circle(x0 + i * 18, r.y + 68, 4, alpha(col));
        }
    }

    fn draw_notes(&self, c: &mut Canvas) {
        for (i, n) in self.notes.iter().enumerate() {
            let r = self.note_rect(i, n.slide());
            if r.x >= self.w {
                continue;
            }
            ui::card(c, &self.blur, r, 24, 0xCC5E_5B64);
            res::draw_icon(c, &n.icon, 40, r.x + 14, r.y + 18);
            text::draw(c, Font::Strong, r.x + 66, r.y + 14, &text::fit(Font::Strong, &n.title, r.w - 84), TEXT);
            let lines = text::wrap(Font::Small, &n.body, r.w - 84);
            for (k, line) in lines.iter().take(2).enumerate() {
                text::draw(c, Font::Small, r.x + 66, r.y + 36 + k as i32 * 17, line, TEXT_DIM);
            }
        }
    }

    fn draw_ending(&self, c: &mut Canvas, a: &Anim) {
        let t = a.out();
        c.fill(c.clip(), (t * 255 / 1000) << 24);
        if t > 300 {
            let o = (t - 300) * 255 / 700;
            draw_logo(c, self.w / 2, self.h / 2 - 20, 35, (o << 24) | 0xF2_EFE6);
        }
    }
}

/// Where a window's picture goes when its frame `r` is drawn at `to` (the
/// picture covers `at`, shadow included).
fn place_snap(at: Rect, r: Rect, to: Rect) -> Rect {
    let sx = |v: i32| (v as i64 * to.w as i64 / r.w.max(1) as i64) as i32;
    let sy = |v: i32| (v as i64 * to.h as i64 / r.h.max(1) as i64) as i32;
    Rect::new(to.x + sx(at.x - r.x), to.y + sy(at.y - r.y), sx(at.w), sy(at.h))
}

/// A picture made at most `max_w` wide.
fn shrink(pic: Canvas, max_w: i32) -> Canvas {
    if pic.w <= max_w || pic.w == 0 {
        return pic;
    }
    let mut small = Canvas::new_layer_at(max_w, (pic.h * max_w / pic.w).max(1), pic.scale());
    let all = small.bounds();
    small.draw_scaled(&pic, all, 255);
    small
}

/// The button that removes a desktop, on its thumbnail's corner.
fn space_close(thumb: Rect) -> Rect {
    Rect::new(thumb.x - 9, thumb.y - 9, 22, 22)
}

fn capitalize(s: &str) -> String {
    let mut ch = s.chars();
    ch.next().map(|f| f.to_uppercase().chain(ch).collect()).unwrap_or_default()
}

/// A gear `2 * r` wide: a ring with eight teeth.
fn draw_gear(c: &mut Canvas, cx: i32, cy: i32, r: i32, color: u32) {
    c.arc(cx, cy, r * 3 / 4, (r * 3 / 10).max(2), 1000, color);
    // The diagonals at 0.707 ~ 7/10.
    for (ux, uy) in [(10, 0), (-10, 0), (0, 10), (0, -10), (7, 7), (-7, 7), (7, -7), (-7, -7)] {
        let a = r * 6 / 10;
        c.line(cx + a * ux / 10, cy + a * uy / 10, cx + r * ux / 10, cy + r * uy / 10, (r / 4).max(2), color);
    }
}

/// A padlock `s` tall, centred on (`cx`, `cy`).
fn draw_lock(c: &mut Canvas, cx: i32, cy: i32, s: i32, color: u32) {
    let sw = s / 2;
    c.stroke_round(Rect::new(cx - sw / 2, cy - s / 2, sw, s / 2 + 4), sw / 2, 2, color);
    let bw = s * 3 / 4;
    c.fill_round(Rect::new(cx - bw / 2, cy - s / 16, bw, s * 9 / 16), 3, color);
}

/// The battery as a ring around (`cx`, `cy`): `track` all round, the
/// charge in green (red when low), and a bolt in `bolt` while charging.
#[allow(clippy::too_many_arguments)]
fn draw_battery_ring(c: &mut Canvas, cx: i32, cy: i32, rad: i32, thick: i32, pct: u8, charging: bool, track: u32, bolt: u32) {
    c.arc(cx, cy, rad, thick, 1000, track);
    let ring = if pct <= 10 && !charging { 0xFFFF_5A52 } else { ui::GREEN };
    c.arc(cx, cy, rad, thick, u32::from(pct.min(100)) * 10, ring);
    if charging {
        let h = (rad - thick) * 2 - 6;
        let pts = [(cx + 2, cy - h / 2), (cx - 3, cy + 1), (cx + 2, cy - 1), (cx - 2, cy + h / 2)];
        for i in 0..3 {
            c.line(pts[i].0, pts[i].1, pts[i + 1].0, pts[i + 1].1, 2, bolt);
        }
    }
}

/// The month of `d` in `g`: its name, the weekdays and the days, today in a
/// filled circle.
fn draw_month(c: &mut Canvas, g: Rect, d: &DateTime, fg: u32, dim: u32) {
    let title = format!("{} {}", capitalize(MONTHS[(d.month as usize + 11) % 12]), d.year);
    text::draw(c, Font::Heading, g.x + 20, g.y + 14, &title, fg);
    let cell = (g.w - 30) / 7;
    for (i, h) in ["L", "M", "M", "G", "V", "S", "D"].iter().enumerate() {
        let w = ui::caps_width(h);
        ui::caps(c, g.x + 15 + i as i32 * cell + (cell - w) / 2, g.y + 48, h, dim);
    }
    let first = time::from_epoch(time::to_epoch(d.year, d.month, 1, 12, 0, 0)).weekday as i32;
    let days = time::days_in_month(d.year, d.month) as i32;
    let row_h = ((g.h - 90) / 6).clamp(24, 52);
    for day in 1..=days {
        let slot = first + day - 1;
        let (col, row) = (slot % 7, slot / 7);
        let cx = g.x + 15 + col * cell + cell / 2;
        let cy = g.y + 82 + row * row_h;
        let label = format!("{day}");
        if day as u64 == d.day {
            c.fill_circle(cx, cy, 12, fg);
            let on = if fg == TEXT { ui::INK } else { TEXT };
            text::draw_centered(c, Font::Strong, cx, cy - 9, &label, on);
        } else {
            text::draw_centered(c, Font::Body, cx, cy - 9, &label, if col >= 5 { dim } else { fg });
        }
    }
}

use crate::fs::Path;

impl View for Desktop {
    fn resized(&mut self, w: i32, h: i32) {
        self.w = w;
        self.h = h;
        let (sharp, blur) = res::wallpaper(&settings::get().wallpaper, w, h);
        self.sharp = sharp;
        self.blur = blur;
        self.popup = None;
        self.overview = None;
        self.fx.clear();
        let area = self.work_area();
        let full = self.full_area();
        for win in self.windows.iter_mut() {
            if win.restore.is_some() {
                win.rect = full; // maximised: fill the new screen
            } else {
                win.rect.w = win.rect.w.min(area.w);
                win.rect.h = win.rect.h.min(area.h);
                win.rect.x = win.rect.x.clamp(area.x, (area.right() - win.rect.w).max(area.x));
                win.rect.y = win.rect.y.clamp(area.y, (area.bottom() - win.rect.h).max(area.y));
            }
        }
    }

    fn animating(&self) -> bool {
        if let Some(lock) = &self.lock {
            return lock.animating();
        }
        !self.fx.is_empty()
            || self.launcher.as_ref().is_some_and(|l| !l.settled || l.closing)
            || self.popup.as_ref().is_some_and(|p| !p.settled)
            || self.mag.moving()
            || self.bounce.is_some()
            || self.notes.iter().any(|n| n.moving())
            || self.ending.is_some()
            || self.windows.iter().any(|w| w.shown(self.space) && w.app.animating())
            || self.overview.as_ref().is_some_and(|o| !o.settled || o.closing || o.held.is_some())
            || self.space_hud.is_some()
    }

    fn event(&mut self, l: &mut Looper, ev: Event) -> bool {
        let quit = self.handle(l, ev);
        // Into or out of full screen: the bar and the dock come and go.
        let full = self.full_screen();
        if full != self.was_full {
            self.was_full = full;
            self.dock_reveal = false;
            l.damage(self.screen());
        }
        quit
    }

    fn draw(&mut self, c: &mut Canvas, r: Rect) {
        self.paint(c, r);
    }
}

impl Desktop {
    /// Events while the computer is locked: only the lock screen sees them
    /// (and the clock keeps going).
    fn handle_locked(&mut self, l: &mut Looper, ev: Event) {
        let Some(lock) = &mut self.lock else { return };
        let outcome = match ev {
            Event::Key(k) => lock.key(k),
            Event::Press { .. } => lock.press(),
            Event::Tick => {
                let d = now();
                let minute = d.hour * 60 + d.min;
                if minute != self.minute {
                    self.minute = minute;
                    Outcome::Redraw
                } else {
                    Outcome::Stay
                }
            }
            Event::Frame => {
                if lock.finished() {
                    self.lock = None;
                    self.last_input = time::uptime_ms();
                    self.minute = u64::MAX; // the bar and the widgets show the time again
                }
                Outcome::Redraw
            }
            _ => Outcome::Stay,
        };
        if outcome != Outcome::Stay {
            l.damage(self.screen());
        }
    }

    /// Locks the screen after the minutes chosen in the settings without
    /// input, when a password is set.
    fn idle_lock(&mut self, l: &mut Looper) {
        let s = settings::get();
        if s.lock_after == 0 || s.password.is_empty() || self.ending.is_some() {
            return;
        }
        if time::uptime_ms().saturating_sub(self.last_input) > u64::from(s.lock_after) * 60_000 {
            self.lock_screen(l);
        }
    }

    fn handle(&mut self, l: &mut Looper, ev: Event) -> bool {
        if matches!(ev, Event::Press { .. } | Event::Key(_) | Event::Move { .. } | Event::Wheel { .. }) {
            self.last_input = time::uptime_ms();
        }
        if self.lock.is_some() {
            self.handle_locked(l, ev);
            return false;
        }
        if matches!(ev, Event::Tick) {
            self.idle_lock(l);
            if self.lock.is_some() {
                return false;
            }
        }
        match ev {
            Event::Press { x, y, double } => self.press(l, x, y, double),
            Event::Menu { x, y } => {
                if self.launcher.is_some() || self.overview.is_some() || self.ending.is_some() {
                    return false;
                }
                let on_window = self
                    .windows
                    .iter()
                    .rev()
                    .filter(|w| w.shown(self.space) && !self.moving(w.id))
                    .find(|w| w.rect.contains(x, y))
                    .map(|w| w.id);
                if let Some(id) = on_window {
                    if let Some(win) = self.window_mut(id) {
                        let b = win.body();
                        if b.contains(x, y) {
                            let action = win.app.menu(x - b.x, y - b.y, b);
                            self.act(l, Some(id), action);
                        }
                    }
                    return false;
                }
                if self.dock().contains(x, y) {
                    return false;
                }
                let widgets = if settings::get().widgets { "Nascondi i widget" } else { "Mostra i widget" };
                let p = Popup::menu(
                    x,
                    y,
                    alloc::vec![
                        ("Cambia sfondo", Some(MenuAct::NextWallpaper)),
                        (widgets, Some(MenuAct::Widgets)),
                        ("Impostazioni Schermo…", Some(MenuAct::Display)),
                        ("", None),
                        ("Info su JustOS", Some(MenuAct::About)),
                    ],
                    self.w,
                );
                self.show_popup(l, p);
            }
            Event::Release => self.released(l),
            Event::Move { x, y, held } => self.moved(l, x, y, held),
            Event::Wheel { lines } => {
                let (x, y) = self.mouse;
                if let Some(la) = &mut self.launcher {
                    let n = la.visible().len() as i32;
                    let cols = 7.min((self.w - 80) / 128).max(1);
                    let rows = (n + cols - 1) / cols;
                    let max = (rows * 120 + 170 + la.found_h() - (self.h - 70)).max(0);
                    la.scroll = (la.scroll + lines * 60).clamp(0, max);
                    l.damage(self.screen());
                    return false;
                }
                if self.overview.is_some() {
                    return false;
                }
                let space = self.space;
                if let Some(win) = self.windows.iter_mut().rev().find(|w| w.shown(space) && w.rect.contains(x, y)) {
                    if win.body().contains(x, y) && win.app.scroll(lines) {
                        l.damage(win.bounds());
                    }
                }
            }
            Event::Key(k) => self.key(l, k),
            Event::Tick => {
                let d = now();
                let minute = d.hour * 60 + d.min;
                if minute != self.minute {
                    self.minute = minute;
                    l.damage(self.clock_zone());
                    for w in self.widgets().iter().take(self.show_widgets()) {
                        l.damage(ui::with_shadow(*w));
                    }
                }
                let second = time::uptime_ms() / 5000;
                if second != self.second {
                    self.second = second;
                    if self.show_widgets() > 2 {
                        l.damage(ui::with_shadow(self.widgets()[2]));
                    }
                    let battery = crate::acpi::power::battery();
                    if battery != self.battery {
                        // The dock may change width: damage it before and after.
                        l.damage(self.dock_zone());
                        self.battery = battery;
                        l.damage(self.dock_zone());
                    }
                }
                for w in self.windows.iter_mut() {
                    if !w.minimized && w.app.tick() {
                        let body = w.body();
                        l.damage(w.app.changed(body).map_or(body, |r| r.intersect(&body)));
                    }
                }
                // Programs that ended take their windows with them.
                let gone: Vec<u32> = self.windows.iter().filter(|w| !w.app.alive()).map(|w| w.id).collect();
                for id in gone {
                    self.close_window(l, id);
                }
                // New windows of programs, and notifications from the system.
                for app in super::window::take_new() {
                    self.open(l, app, false);
                }
                // Questions of the sandbox, and file pickers for programs.
                for app in super::apps::prompt::take_new() {
                    self.open(l, app, false);
                }
                while let Some((title, body, icon)) = super::take_notification() {
                    self.notify(l, title, body, icon);
                }
                if super::take_sleep_lock() && settings::has_password() {
                    self.lock_screen(l);
                }
                if self.notes.iter().any(|n| n.gone()) {
                    self.notes.retain(|n| !n.gone());
                    l.damage(self.notes_zone());
                }
            }
            Event::Frame => {
                // Windows in motion.
                let mut finished = Vec::new();
                for (i, f) in self.fx.iter_mut().enumerate() {
                    let (r, _) = f.place();
                    let r = r.grow(2);
                    l.damage(f.last.union(&r));
                    f.last = r;
                    if f.anim.done() {
                        finished.push(i);
                    }
                }
                for &i in finished.iter().rev() {
                    let f = self.fx.remove(i);
                    l.damage(f.last.union(&f.at));
                    if f.kind == FxKind::Minimize {
                        if let Some(w) = self.window_mut(f.id) {
                            w.minimized = true;
                        }
                        l.damage(self.dock_zone());
                    }
                }
                if let Some(la) = &mut self.launcher {
                    if !la.settled || la.closing {
                        l.damage(Rect::new(0, 0, self.w, self.h));
                        la.settled = la.anim.done();
                    }
                    if la.closing && la.anim.done() {
                        let then = self.launcher.take().and_then(|la| la.then);
                        if let Some(what) = then {
                            self.launch(l, what);
                        }
                    }
                }
                let area = self.popup.as_ref().map(|p| self.popup_area(p));
                if let (Some(p), Some(area)) = (self.popup.as_mut(), area) {
                    if !p.settled {
                        l.damage(area);
                        p.settled = p.anim.done();
                    }
                }
                if self.mag.step() {
                    l.damage(self.dock_zone());
                }
                if let Some((_, a)) = &self.bounce {
                    l.damage(self.dock_zone());
                    if a.done() {
                        self.bounce = None;
                    }
                }
                if self.notes.iter().any(|n| n.moving()) {
                    l.damage(self.notes_zone());
                }
                let space = self.space;
                for w in self.windows.iter_mut() {
                    if w.shown(space) && w.app.animating() && w.app.frame() {
                        l.damage(w.body());
                    }
                }
                if let Some(o) = &mut self.overview {
                    if !o.settled || o.closing || o.held.is_some() {
                        l.damage(Rect::new(0, 0, self.w, self.h));
                        o.settled = o.anim.done();
                    }
                    if o.closing && o.anim.done() {
                        self.overview = None;
                        l.damage(self.screen());
                    }
                }
                if let Some(a) = &self.space_hud {
                    l.damage(ui::with_shadow(self.space_hud_rect()));
                    if a.elapsed() > SPACE_HUD_MS {
                        self.space_hud = None;
                    }
                }
                if let Some((how, a)) = &self.ending {
                    l.damage(self.screen());
                    if a.elapsed() > 900 {
                        match how {
                            Ending::Restart => crate::acpi::reboot(),
                            Ending::PowerOff => crate::acpi::power_off(),
                        }
                    }
                }
            }
        }
        false
    }

    fn paint(&mut self, c: &mut Canvas, r: Rect) {
        if let Some(lock) = &self.lock {
            let covered = lock.anim.done() && lock.leaving.is_none();
            if covered {
                lock.draw(c, &self.blur, r, &now());
                return;
            }
        }
        self.paint_desktop(c, r);
        if let Some(lock) = &self.lock {
            lock.draw(c, &self.blur, r, &now());
        }
    }

    fn paint_desktop(&mut self, c: &mut Canvas, r: Rect) {
        if let Some(la) = &self.launcher {
            if la.shown() >= 1000 {
                self.draw_launcher(c, la);
                return;
            }
        }
        if let Some(o) = &self.overview {
            self.draw_overview(c, o);
            if !self.notes.is_empty() && r.overlaps(&self.notes_zone()) {
                self.draw_notes(c);
            }
            return;
        }
        c.blit(&self.sharp, r.x, r.y, r);
        self.draw_widgets(c, r);
        let focused = self.focused();
        let blur = &self.blur;
        let fx = &self.fx;
        let dragged = match self.drag {
            Some(Drag::Move(id, ..)) => Some(id),
            _ => None,
        };
        for w in self.windows.iter_mut() {
            if let (Some(zone), true) = (self.snap_preview, Some(w.id) == dragged) {
                // Where the window will go, under it.
                if r.overlaps(&zone.grow(4)) {
                    c.blit_round(blur, zone, 22, ui::GLASS_LIGHT);
                    c.stroke_round(zone, 22, 2, 0x80FF_FFFF);
                }
            }
            if w.shown(self.space) && r.overlaps(&w.bounds()) && !fx.iter().any(|f| f.id == w.id) {
                let f = Some(w.id) == focused;
                w.draw(c, blur, f, r);
            }
        }
        for f in &self.fx {
            let (dst, opacity) = f.place();
            if r.overlaps(&dst) {
                c.draw_scaled(&f.snap, dst, opacity);
            }
        }
        // The control centre darkens the screen towards the bottom.
        if let Some(p) = self.popup.as_ref().filter(|p| p.kind == PopupKind::Control) {
            let a = 0xC8 * p.anim.out() / 1000;
            c.fill_round_grad(self.screen(), 0, 0, a << 24);
        }
        if self.dock_shown() && r.overlaps(&self.dock_zone()) {
            self.draw_dock(c);
        }
        if r.overlaps(&self.clock_zone()) {
            self.draw_clock(c);
        }
        if let Some(p) = &self.popup {
            if r.overlaps(&ui::with_shadow(p.rect)) {
                self.draw_popup(c, p);
            }
        }
        if !self.notes.is_empty() && r.overlaps(&self.notes_zone()) {
            self.draw_notes(c);
        }
        if let Some(a) = &self.space_hud {
            if r.overlaps(&ui::with_shadow(self.space_hud_rect())) {
                self.draw_space_hud(c, a);
            }
        }
        if let Some(carry) = &self.carry {
            if r.overlaps(&carry_rect(self.mouse.0, self.mouse.1)) {
                self.draw_carry(c, carry);
            }
        }
        if let Some(la) = &self.launcher {
            self.draw_launcher(c, la);
        }
        if let Some((_, a)) = &self.ending {
            self.draw_ending(c, a);
        }
    }
}

pub fn run(d: &mut Display) {
    let (w, h) = (d.width(), d.height());
    let (sharp, blur) = res::wallpaper(&settings::get().wallpaper, w, h);
    let mut desk = Desktop {
        w,
        h,
        sharp,
        blur,
        windows: Vec::new(),
        next_id: 1,
        drag: None,
        launcher: None,
        popup: None,
        fx: Vec::new(),
        notes: Vec::new(),
        hover: None,
        mag: Glide::new(0),
        bounce: None,
        mouse: (-1, -1),
        minute: u64::MAX,
        second: 0,
        cascade: 0,
        hovered: None,
        ending: None,
        was_full: false,
        dock_reveal: false,
        history: Vec::new(),
        quiet: false,
        // With a password, the computer starts locked.
        lock: settings::has_password().then(Lock::new),
        last_input: time::uptime_ms(),
        snap_preview: None,
        space: 0,
        spaces: 1,
        overview: None,
        space_hud: None,
        carry: None,
        battery: None,
    };
    let mut l = Looper::new(d);
    l.damage_all(d);
    startup_sound();
    l.run(d, &mut desk);
}

/// The chime when the desktop appears, if there is a sound card and the
/// user wants it.
fn startup_sound() {
    if !settings::get().startup_sound || !crate::dev::audio::available() {
        return;
    }
    let file = fs::abs("sys:/system/sounds/avvio.wav").and_then(|p| fs::read_file(&p));
    match file.map(|b| crate::dev::audio::parse_wav(&b)) {
        Ok(Ok(sound)) => {
            let _ = crate::dev::audio::play(sound);
        }
        Ok(Err(e)) => crate::warn!("gui", "startup sound: {e}"),
        Err(e) => crate::warn!("gui", "startup sound: {e}"),
    }
}

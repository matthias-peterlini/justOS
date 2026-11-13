//! The desktop: the clock floating at the top, the widgets, the dock (the
//! justOS mark, the apps, the status), the launcher, the windows and the
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
use super::splash::draw_logo;
use super::text::{self, Font};
use super::ui::{self, Anim, Event, Glide, Looper, View};
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
const DOCK_GAP: i32 = 12;
const DOCK_PAD: i32 = 11;
const DOCK_RAD: i32 = 28;
/// The room a separator adds between two icons: a thin line in the middle
/// of two gaps.
const SEP_W: i32 = 14;
/// The status well at the right end of the dock: the room at its ends and
/// between its symbols, and the size of their cells (thousandths of a unit).
const STATUS_PAD: i32 = 16;
const STATUS_GAP: i32 = 12;
const STATUS_CELL: i32 = 4000;
/// The control centre is drawn from the justOS design for a screen of
/// 1024 x 768: its measures are the design's pixels, scaled to the screen
/// by `Desktop::cc`. The panel's width, padding, corner and the room
/// between its cards; the height of each row of cards (the account, the
/// network and Bluetooth, do not disturb, brightness, volume, the
/// buttons); its distance above the dock.
const CC_W: i32 = 352;
const CC_PAD: i32 = 14;
const CC_RAD: i32 = 26;
const CC_GAP: i32 = 10;
const CC_ROWS: [i32; 6] = [60, 62, 62, 38, 38, 40];
const CC_ABOVE: i32 = 14;
/// The gradients of the tiles of switches that are on, and of the battery.
const BLUE_GRAD: (u32, u32) = (0xFF5A_A9FF, 0xFF25_63EB);
const VIOLET_GRAD: (u32, u32) = (0xFFA7_8BFA, 0xFF6D_3FE0);
/// The justOS mark's tile and the cream of its "J".
const MARK_GRAD: (u32, u32) = (0xFF30_3032, 0xFF14_1414);
const CREAM: u32 = 0xFFF2_EFE6;
/// Light or dark overlays straight on the wallpaper (the clock under the
/// pointer, the launcher, the overview), where the theme does not reach:
/// the same in both themes.
const WALL_LIT: u32 = 0x26FF_FFFF;
const WALL_LIT_INK: u32 = 0x1F00_0000;
const WALL_HOVER: u32 = 0x22FF_FFFF;
const WALL_FIELD: u32 = 0x33FF_FFFF;
const WALL_PANEL: u32 = 0x40FF_FFFF;
const WALL_EDGE: u32 = 0x59FF_FFFF;
const WALL_EDGE_STRONG: u32 = 0xE6FF_FFFF;
/// The edge of where a dragged window will snap, over a dark or a light picture.
const WALL_SNAP: u32 = 0x80FF_FFFF;
const WALL_SNAP_INK: u32 = (ui::INK_DIM & 0xFF_FFFF) | 0x9900_0000;
/// The soft shadow under the clock on a dark picture, the light halo under
/// it on a light one.
const WALL_SHADE: u32 = 0x4D00_0000;
const WALL_SHADE_INK: u32 = 0x40FF_FFFF;

// Soft-pixel symbols ('#': a filled cell, see `ui::soft_pixels_at`).
const G_WIFI: [&str; 5] = [".###.", "#...#", "..#..", ".#.#.", "..#.."];
const G_BLUETOOTH: [&str; 5] = ["..#..", "#.##.", ".##..", "#.##.", "..#.."];
const G_MOON: [&str; 5] = ["..##.", ".##..", ".##..", ".###.", "..###"];
const G_SUN: [&str; 5] = ["#.#.#", ".###.", "#####", ".###.", "#.#.#"];
const G_VOLUME: [&str; 5] = ["..#..", "###.#", "###.#", "###.#", "..#.."];
const G_MUTE: [&str; 5] = ["..#....", "###.#.#", "###..#.", "###.#.#", "..#...."];
const G_LOCK: [&str; 5] = [".###.", ".#.#.", "#####", "#####", "#####"];
const G_POWER: [&str; 5] = ["..#..", "#.#.#", "#.#.#", "#...#", ".###."];
const G_RESTART: [&str; 5] = [".##.#", "#..##", "#....", "#...#", ".###."];
const G_PLUG: [&str; 5] = [".#.#.", ".#.#.", "#####", ".###.", "..#.."];
const G_GEAR: [&str; 7] = ["..#.#..", ".#####.", "##...##", ".#...#.", "##...##", ".#####.", "..#.#.."];
/// The justOS mark: the "J" of the wordmark and the orange cursor ('o').
const G_MARK: [&str; 7] = ["#####.ooo", "...#..ooo", "...#..ooo", "...#..ooo", "...#..ooo", "#..#..ooo", ".##...ooo"];
/// The digits of the desktop clock, three cells by five, and its colon.
const DIGITS: [[&str; 5]; 10] = [
    ["###", "#.#", "#.#", "#.#", "###"],
    [".#.", "##.", ".#.", ".#.", "###"],
    ["###", "..#", "###", "#..", "###"],
    ["###", "..#", "###", "..#", "###"],
    ["#.#", "#.#", "###", "..#", "..#"],
    ["###", "#..", "###", "..#", "###"],
    ["###", "#..", "###", "#.#", "###"],
    ["###", "..#", "..#", "..#", "..#"],
    ["###", "#.#", "###", "#.#", "###"],
    ["###", "#.#", "###", "..#", "###"],
];
const COLON: [&str; 5] = [".", "#", ".", "#", "."];
/// The cell of the clock's digits (thousandths of a unit), and the room
/// above them, between them and the date, and under the date.
const CLOCK_CELL: i32 = 10_000;
const CLOCK_TOP: i32 = 4;
const CLOCK_DATE_GAP: i32 = 6;
const CLOCK_BOTTOM: i32 = 6;

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
    Bluetooth,
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
    matches!(item.kind.as_str(), "trash" | "files" | "textedit" | "photos" | "viewer" | "music" | "juststudio")
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

/// A program launched whose first window has not come yet (Firefox takes
/// most of a minute): its icon waits in the dock, hopping, where its
/// window's icon will be.
struct Starting {
    /// The program file: launching it again meanwhile starts no second copy.
    path: String,
    /// The window kind its windows will have (their icon), and its name.
    kind: String,
    icon: String,
    label: String,
    pid: u32,
    /// When it was launched, and when the hops started (uptime, ms).
    since: u64,
    hop: u64,
    /// How high the icon was drawn last.
    lift: i32,
}

/// How long a program may take to show its first window before its icon
/// leaves the dock.
const START_TIMEOUT_MS: u64 = 120_000;
/// A starting program's icon: one hop, the rest after it, and how high.
const HOP_MS: u64 = 620;
const HOP_REST_MS: u64 = 480;
const HOP_H: i32 = 12;

/// How high a starting program's icon is now, its hops having begun at
/// `hop`: the arc of a ball, then a pause on the dock.
fn start_lift(hop: u64) -> i32 {
    let t = time::uptime_ms().saturating_sub(hop) % (HOP_MS + HOP_REST_MS);
    if t >= HOP_MS {
        return 0;
    }
    let p = (t * 1000 / HOP_MS) as i64;
    (HOP_H as i64 * 4 * p * (1000 - p) / 1_000_000) as i32
}

/// How far each new window goes right and down from the one opened before.
const CASCADE: i32 = 28;

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
    /// Programs launched that have no window yet.
    starting: Vec<Starting>,
    mouse: (i32, i32),
    minute: u64,
    second: u64,
    /// The window opened last: the next one goes a step from it.
    last_new: Option<u32>,
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
        let (cw, _) = clock_size(&clock);
        let w = cw.max(text::width(Font::Strong, &date)) + 40;
        Rect::new((self.w - w) / 2, 10, w, clock_height())
    }

    /// A dot for each desktop under the clock, when there is more than one:
    /// it opens the overview.
    fn spaces_btn(&self) -> Option<Rect> {
        let cb = self.clock_btn();
        let w = self.spaces as i32 * 14 + 18;
        (self.spaces > 1 && !self.full_screen()).then(|| Rect::new(self.w / 2 - w / 2, cb.bottom() + 2, w, 20))
    }

    /// Is a window over the clock (one taller than the work area, or moved
    /// up there)? Then the clock is part of the desktop under it, and clicks
    /// there are the window's.
    fn clock_covered(&self) -> bool {
        if self.full_screen() {
            return false;
        }
        let mut cb = self.clock_btn();
        if let Some(sp) = self.spaces_btn() {
            cb = cb.union(&sp);
        }
        self.windows.iter().any(|w| w.shown(self.space) && w.rect.overlaps(&cb))
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
        // Programs starting, where their first window's icon will go.
        for s in &self.starting {
            if v.iter().any(|d| d.kind == s.kind) {
                continue;
            }
            v.push(DockItem {
                id: String::new(),
                icon: s.icon.clone(),
                label: s.label.clone(),
                kind: s.kind.clone(),
                // Pressed again: it hops (no second copy).
                launch: Some(Launch::Spawn(s.path.clone())),
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
        // The mark, as wide as an icon, and a separator after it.
        let mut x = DOCK_PAD + icon + DOCK_GAP + SEP_W;
        for (i, it) in items.iter().enumerate() {
            if it.split && i > 0 {
                x += SEP_W;
            }
            xs.push(x);
            x += icon + DOCK_GAP;
        }
        (xs, x + status_w + DOCK_PAD)
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

    /// The width of the status well: network, sound and, on a laptop, the
    /// battery (room for the widest symbol of each).
    fn status_w(&self) -> i32 {
        let w = |g: &[&str]| ui::soft_size(g, STATUS_CELL).0 / 1000;
        let mut total = 2 * STATUS_PAD + w(&G_WIFI) + STATUS_GAP + w(&G_MUTE);
        if self.battery.is_some() {
            total += STATUS_GAP + battery_rows(100)[0].len() as i32 * STATUS_CELL / 1000;
        }
        total
    }

    /// The dock's background at rest (no icon magnified).
    fn dock_rest(&self) -> Rect {
        let items = self.dock_items();
        let icon = self.dock_icon(&items);
        let (_, total) = Self::dock_base(&items, self.status_w(), icon);
        let dock_h = icon + 2 * DOCK_PAD + 4;
        Rect::new((self.w - total) / 2, self.h - dock_h - 12, total, dock_h)
    }

    /// The justOS mark at the left end of the dock at rest (where the
    /// system menu opens from).
    fn mark_rest(&self) -> Rect {
        let rest = self.dock_rest();
        let icon = rest.h - 2 * DOCK_PAD - 4;
        Rect::new(rest.x + DOCK_PAD, rest.y + DOCK_PAD, icon, icon)
    }

    /// The status at the right end of the dock `bg`.
    fn status_pill(&self, bg: Rect) -> Rect {
        let w = self.status_w();
        Rect::new(bg.right() - DOCK_PAD - w, bg.y + DOCK_PAD, w, bg.h - 2 * DOCK_PAD - 4)
    }

    /// The icons' rectangles on the screen, magnified around the pointer,
    /// and the dock's background.
    fn dock_layout(&self, items: &[DockItem]) -> (Vec<Rect>, Rect) {
        let (_, rects, bg) = self.dock_geometry(items);
        (rects, bg)
    }

    /// The justOS mark (it opens the system menu) and the icons, magnified
    /// around the pointer, and the dock's background.
    fn dock_geometry(&self, items: &[DockItem]) -> (Rect, Vec<Rect>, Rect) {
        let icon = self.dock_icon(items);
        let (apps, total) = Self::dock_base(items, self.status_w(), icon);
        // The mark grows like an icon.
        let xs: Vec<i32> = core::iter::once(DOCK_PAD).chain(apps).collect();
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
            let mark = rects.remove(0);
            return (mark, rects, bg);
        }
        let bg = Rect::new(x0, self.h - dock_h - 12, total_now, dock_h);
        let mark = rects.remove(0);
        (mark, rects, bg)
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

    /// What the hopping icons of programs starting may paint.
    fn starting_zone(&self) -> Rect {
        let items = self.dock_items();
        let (rects, bg) = self.dock_layout(&items);
        let mut zone = Rect::EMPTY;
        for (it, r) in items.iter().zip(&rects) {
            if self.starting.iter().any(|s| s.kind == it.kind) {
                let top = r.y - HOP_H - 4;
                zone = zone.union(&Rect::new(r.x - 4, top, r.w + 8, bg.bottom() - top));
            }
        }
        zone
    }

    /// Where windows go: under the clock, above the dock.
    fn work_area(&self) -> Rect {
        let top = 10 + clock_height() + 8;
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
        let work = self.work_area();
        let (aw, ah) = Window::outer_size(app.size().0, app.size().1);
        // A window taller than that that cannot be made smaller (a Windows
        // dialog on a small screen) covers the clock too, rather than
        // losing its bottom; any other fits in the work area.
        let area = if ah > work.h && !app.resizable() { Rect::new(work.x, 4, work.w, work.bottom() - 4) } else { work };
        let (aw, full_h) = (aw.min(area.w), ah.min(area.h));
        let (x, y) = self.place_new(aw, full_h, area);
        let id = self.next_id;
        self.next_id += 1;
        self.last_new = Some(id);
        let mut win = Window::new(id, app, x, y);
        win.space = self.space;
        win.rect.w = aw;
        win.rect.h = full_h;
        let kind = String::from(win.app.kind());
        if let Some(prev) = self.windows.last() {
            l.damage(prev.bounds());
        }
        // A program that was starting has its window: its icon in the dock
        // is the window's now.
        if self.starting.iter().any(|s| s.kind == kind) {
            l.damage(self.dock_zone());
            self.starting.retain(|s| s.kind != kind);
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

    /// Where a new window `w` x `h` goes in `area`: in the middle (a little
    /// above it), or [`CASCADE`] right and down from the window opened
    /// before it while that one is still out on this desktop, so that the
    /// start of that one's title row still shows. Past the bottom it stays
    /// at the bottom (there is little room below most windows); past the
    /// right edge it goes back to the middle. Its title row is always
    /// inside the area.
    fn place_new(&self, w: i32, h: i32, area: Rect) -> (i32, i32) {
        // Of the room left, two fifths above and three below.
        let middle = (area.x + (area.w - w) / 2, area.y + (area.h - h) * 2 / 5);
        let prev = self.last_new.and_then(|id| self.window(id)).filter(|p| p.shown(self.space) && !p.full() && p.snapped.is_none());
        let (mut x, mut y) = match prev {
            Some(p) => (p.rect.x + CASCADE, p.rect.y + CASCADE),
            None => middle,
        };
        y = y.min(area.bottom() - h);
        if x < area.x || y < area.y || x + w > area.right() {
            (x, y) = middle;
        }
        (x.clamp(area.x, (area.right() - w).max(area.x)), y.clamp(area.y, (area.bottom() - h).max(area.y)))
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
            Action::RedrawAll => l.damage(self.screen()),
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
        // Still starting: no second copy, its icon just hops again.
        if let Some(s) = self.starting.iter_mut().find(|s| s.path == path) {
            s.hop = time::uptime_ms();
            l.damage(self.starting_zone());
            return;
        }
        let name = apps::program_name(path);
        let home = fs::abs("sys:/home").unwrap_or_else(|_| Path::root("sys"));
        match fs::abs(path).map_err(|_| justdk::syscall::Error::NotFound).and_then(|p| crate::user::spawn(&p, "", home)) {
            Ok(pid) => {
                // A big Linux program (Firefox) takes a while to show its
                // window: say that it is on its way.
                let linux = apps::linux_app_for(path);
                if let Some(e) = &linux {
                    self.notify(l, e.name.clone(), String::from("Si sta avviando: la finestra arriva tra poco."), e.icon.clone());
                }
                // The icon its windows will have (and so their kind, see
                // `window::icon_for` and `window::create`), when known.
                let icon = match linux {
                    Some(e) => Some(e.icon),
                    None if path.to_ascii_lowercase().ends_with(".app") => {
                        let file = path.rsplit('/').next().unwrap_or(path);
                        Some(apps::package_icon(&file[..file.len() - 4]))
                    }
                    None => None,
                };
                match icon {
                    // Its icon waits in the dock, hopping, until its first
                    // window comes. One already open just hops once.
                    Some(icon) if !self.windows.iter().any(|w| w.app.kind() == icon) => {
                        let now = time::uptime_ms();
                        l.damage(self.dock_zone());
                        self.starting.push(Starting { path: String::from(path), kind: icon.clone(), icon, label: name, pid, since: now, hop: now, lift: 0 });
                        l.damage(self.dock_zone());
                    }
                    Some(icon) => self.bounce_icon(&icon),
                    None => self.bounce = Some((format!("@{path}"), Anim::new(900))),
                }
            }
            Err(e) => self.notify(l, name, format!("Non si è potuto avviare: {}", e.as_str()), String::from("window")),
        }
    }

    /// Programs starting: the icon of one goes when it ended without a
    /// window (saying so) or after [`START_TIMEOUT_MS`]; [`Desktop::open`]
    /// takes it when its window comes.
    fn check_starting(&mut self, l: &mut Looper) {
        if self.starting.is_empty() {
            return;
        }
        let now = time::uptime_ms();
        // Which go, and whether because the program ended.
        let ended: Vec<(usize, bool)> = self
            .starting
            .iter()
            .enumerate()
            .filter_map(|(i, s)| {
                let gone = !crate::user::is_running(s.pid);
                (gone || now.saturating_sub(s.since) >= START_TIMEOUT_MS).then_some((i, gone))
            })
            .collect();
        if ended.is_empty() {
            return;
        }
        l.damage(self.dock_zone());
        for &(i, gone) in ended.iter().rev() {
            let s = self.starting.remove(i);
            if gone {
                self.notify(l, s.label, String::from("Non si è avviato."), s.icon);
            }
        }
        l.damage(self.dock_zone());
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

    /// What a popup paints.
    fn popup_area(&self, p: &Popup) -> Rect {
        ui::with_shadow(p.rect)
    }

    // The control centre, as in the justOS design: a panel above the status
    // at the right end of the dock, with cards in it: the account and the
    // settings, the network and Bluetooth, do not disturb, brightness and
    // volume, and the buttons to lock, restart and power off.

    /// A length of the design, in units: the design's frame (1024 x 768)
    /// fitted to the screen, at 0.6 to 1 times its size.
    fn cc(&self, px: i32) -> i32 {
        let k = (self.w * 1000 / 1024).min(self.h * 1000 / 768).clamp(600, 1000);
        (px * k + 500) / 1000
    }

    /// The panel: over the status well, inside the screen, above the dock.
    fn control_panel(&self) -> Rect {
        let w = self.cc(CC_W);
        let h = 2 * self.cc(CC_PAD) + CC_ROWS.iter().map(|&r| self.cc(r)).sum::<i32>() + (CC_ROWS.len() as i32 - 1) * self.cc(CC_GAP);
        let rest = self.dock_rest();
        let status = self.status_pill(rest);
        let x = (status.x + status.w / 2 - w / 2).min(self.w - 16 - w).max(16);
        Rect::new(x, rest.y - self.cc(CC_ABOVE) - h, w, h)
    }

    /// The panel's rows of cards, top to bottom.
    fn cc_rows(&self) -> [Rect; 6] {
        let p = self.control_panel();
        let pad = self.cc(CC_PAD);
        let mut y = p.y + pad;
        CC_ROWS.map(|h| {
            let r = Rect::new(p.x + pad, y, p.w - 2 * pad, self.cc(h));
            y += r.h + self.cc(CC_GAP);
            r
        })
    }

    /// All of the control centre.
    fn control_rect(&self) -> Rect {
        self.control_panel()
    }

    /// The account's card (it opens the settings).
    fn control_account(&self) -> Rect {
        self.cc_rows()[0]
    }

    /// The switches: the network and Bluetooth side by side, do not disturb
    /// under them.
    fn control_tiles(&self) -> [(Tile, Rect); 3] {
        let rows = self.cc_rows();
        let gap = self.cc(CC_GAP);
        let q = rows[1];
        let half = (q.w - gap) / 2;
        [
            (Tile::Network, Rect::new(q.x, q.y, half, q.h)),
            (Tile::Bluetooth, Rect::new(q.x + half + gap, q.y, q.w - half - gap, q.h)),
            (Tile::Quiet, rows[2]),
        ]
    }

    /// The sliders' cards: volume and brightness (drawn brightness first).
    fn control_sliders(&self) -> [Rect; 2] {
        let rows = self.cc_rows();
        [rows[4], rows[3]]
    }

    /// The row of cells inside a slider's card; its symbol is at the left.
    fn slider_bar(&self, s: Rect) -> Rect {
        let x = s.x + self.cc(42);
        Rect::new(x, s.y, s.right() - self.cc(12) - x, s.h)
    }

    /// The buttons at the bottom: lock, restart, power off.
    fn control_buttons(&self) -> [Rect; 3] {
        let b = self.cc_rows()[5];
        let gap = self.cc(CC_GAP);
        let third = (b.w - 2 * gap) / 3;
        [
            Rect::new(b.x, b.y, third, b.h),
            Rect::new(b.x + third + gap, b.y, third, b.h),
            Rect::new(b.x + 2 * (third + gap), b.y, b.w - 2 * (third + gap), b.h),
        ]
    }

    /// The settings: a tile at the right end of the account's card.
    fn settings_btn(&self) -> Rect {
        let a = self.control_account();
        let d = self.cc(36);
        Rect::new(a.right() - self.cc(12) - d, a.y + (a.h - d) / 2, d, d)
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
        let [lock, restart, off] = self.control_buttons();
        if off.contains(x, y) {
            Some(ControlHit::PowerOff)
        } else if lock.contains(x, y) {
            Some(ControlHit::Lock)
        } else if restart.contains(x, y) {
            Some(ControlHit::Restart)
        } else if self.settings_btn().contains(x, y) {
            Some(ControlHit::Settings)
        } else if self.control_account().contains(x, y) {
            Some(ControlHit::Account)
        } else {
            None
        }
    }

    /// Is `x` on the symbol at the left of the slider `s`, before its cells?
    fn on_slider_foot(&self, s: Rect, x: i32) -> bool {
        x < self.slider_bar(s).x - self.cc(6)
    }

    fn control_press(&mut self, l: &mut Looper, hit: ControlHit, x: i32) {
        let [vol, bri] = self.control_sliders();
        match hit {
            ControlHit::Volume if self.on_slider_foot(vol, x) => {
                // The speaker: the sound off, or back on.
                let v = if crate::dev::audio::volume() == 0 { 60 } else { 0 };
                super::change_volume(v);
                l.damage(ui::with_shadow(self.control_rect()));
                l.damage(self.dock_zone());
            }
            ControlHit::Volume => {
                self.drag = Some(Drag::Volume);
                self.set_volume_at(l, x);
            }
            ControlHit::Brightness => {
                if !self.on_slider_foot(bri, x) {
                    self.drag = Some(Drag::Brightness);
                    self.set_brightness_at(l, x);
                }
            }
            ControlHit::Tile(Tile::Network | Tile::Bluetooth) | ControlHit::Settings | ControlHit::Account => {
                self.close_popup(l);
                self.open_id(l, "settings");
            }
            ControlHit::Tile(Tile::Quiet) => {
                self.quiet = !self.quiet;
                l.damage(ui::with_shadow(self.control_rect()));
                l.damage(self.dock_zone());
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

    /// The value (0..=100) of a slider's cells `r` for a pointer at `x`.
    fn bar_value(r: Rect, x: i32) -> u32 {
        ((ui::slider_value(r, x) + 5) / 10).min(100)
    }

    fn set_volume_at(&mut self, l: &mut Looper, x: i32) {
        super::change_volume(Self::bar_value(self.slider_bar(self.control_sliders()[0]), x));
        l.damage(ui::with_shadow(self.control_rect()));
        l.damage(self.dock_zone());
    }

    fn set_brightness_at(&mut self, l: &mut Looper, x: i32) {
        super::set_brightness(Self::bar_value(self.slider_bar(self.control_sliders()[1]), x).max(20));
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
                    self.control_press(l, hit, x);
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
        let (mark, rects, bg) = self.dock_geometry(&items);
        if self.dock_shown() && mark.contains(x, y) {
            let mut items = alloc::vec![
                ("Info su justOS", Some(MenuAct::About)),
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
            let rest = self.mark_rest();
            let p = Popup::menu(rest.x, 0, items, self.w).above(rest.y - DOCK_PAD, 12);
            self.show_popup(l, p);
            return;
        }
        if self.dock_shown() && self.status_pill(bg).contains(x, y) {
            let p = Popup { kind: PopupKind::Control, rect: self.control_rect(), items: Vec::new(), anim: Anim::new(180), up: true, settled: false };
            self.show_popup(l, p);
            return;
        }
        let covered = self.clock_covered();
        if !covered && self.spaces_btn().is_some_and(|b| b.contains(x, y)) {
            self.open_overview(l);
            return;
        }
        if !covered && self.clock_btn().contains(x, y) {
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
                    self.set_volume_at(l, x);
                    return;
                }
                Some(Drag::Brightness) => {
                    self.set_brightness_at(l, x);
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
            c.fill_round(b, 11, ui::ACCENT);
            let (sh, _) = text::metrics(Font::Small);
            text::draw_centered(c, Font::Small, b.x + b.w / 2, b.y + (b.h - sh) / 2, &n, ui::WHITE);
        }
        if carry.copy {
            let (cx, cy) = (ix + 48, iy + 46);
            c.fill_circle(cx, cy, 11, ui::GREEN);
            c.fill(Rect::new(cx - 5, cy - 1, 10, 2), ui::WHITE);
            c.fill(Rect::new(cx - 1, cy - 5, 2, 10), ui::WHITE);
        }
        let label = text::fit(Font::Small, &dnd::describe(&carry.paths), 170);
        let w = text::width(Font::Small, &label) + 18;
        // Under the icon, or over it at the bottom of the screen.
        let below = iy + 58;
        let pill = Rect::new(x - w / 2, if below + 22 > self.h - 2 { iy - 30 } else { below }, w, 22);
        ui::drop_shadow(c, pill, 11, 8, 50);
        ui::panel(c, pill, 11);
        let (sh, _) = text::metrics(Font::Small);
        text::draw_centered(c, Font::Small, x, pill.y + (pill.h - sh) / 2, &label, ui::text());
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
    /// whichever reads on the wallpaper, the digits in soft pixels (on one
    /// line in the title row of a full-screen window), and the desktops'
    /// dots under them.
    fn draw_clock(&self, c: &mut Canvas) {
        let cb = self.clock_btn();
        let (clock, date) = clock_strings();
        let lit = (cb.contains(self.mouse.0, self.mouse.1) && !self.clock_covered())
            || self.popup.as_ref().is_some_and(|p| p.kind == PopupKind::Notices);
        if self.full_screen() {
            // On the window's frame, like its name.
            if lit {
                c.fill_round(cb, 12, ui::sel());
            }
            let (sh, _) = text::metrics(Font::Strong);
            let y = cb.y + (cb.h - sh) / 2;
            let w = text::draw(c, Font::Strong, cb.x + 14, y, &clock, ui::text());
            text::draw(c, Font::Strong, cb.x + 14 + w + 12, y, &date, ui::text_dim());
            return;
        }
        let ink = ui::luminance(&self.blur, cb) > 150;
        let (fg, shade, hover_bg) = if ink { (ui::INK, WALL_SHADE_INK, WALL_LIT_INK) } else { (ui::WHITE, WALL_SHADE, WALL_LIT) };
        if lit {
            c.fill_round(cb, 20, hover_bg);
        }
        let cx = cb.x + cb.w / 2;
        let rows = clock_rows(&clock);
        let rows: Vec<&str> = rows.iter().map(|s| s.as_str()).collect();
        let (cw, ch) = clock_size(&clock);
        let (x, y) = ((cx - cw / 2) * 1000, (cb.y + CLOCK_TOP) * 1000);
        // A soft shade under the digits keeps them readable on busy
        // pictures: the same digits a little lower, a few times, faintly.
        let faint = (((shade >> 24) / 2) << 24) | (shade & 0xFF_FFFF);
        for (dx, dy, col) in [(0, 2000, faint), (-1000, 2000, faint), (1000, 2000, faint), (0, 1000, shade)] {
            ui::soft_pixels_at(c, &rows, x + dx, y + dy, CLOCK_CELL, &|_| col);
        }
        ui::soft_pixels_at(c, &rows, x, y, CLOCK_CELL, &|_| fg);
        let dy = cb.y + CLOCK_TOP + ch + CLOCK_DATE_GAP;
        for (off, col) in [(1, shade), (0, fg)] {
            text::draw_centered(c, Font::Strong, cx, dy + off, &date, col);
        }
        if let Some(sp) = self.spaces_btn() {
            if sp.contains(self.mouse.0, self.mouse.1) {
                c.fill_round(sp, sp.h / 2, hover_bg);
            }
            for i in 0..self.spaces {
                let col = if i != self.space {
                    if ink {
                        ui::INK_DIM
                    } else {
                        ui::WHITE_FAINT
                    }
                } else {
                    fg
                };
                c.fill_circle(sp.x + 16 + i as i32 * 14, sp.y + sp.h / 2, 4, col);
            }
        }
    }

    fn draw_widgets(&self, c: &mut Canvas, r: Rect) {
        let n = self.show_widgets();
        let ws = self.widgets();
        let d = now();
        let name = settings::get().name;
        // A panel of the theme: (text, dimmed text).
        let pane = |c: &mut Canvas, g: Rect| -> (u32, u32) {
            ui::light_glass(c, &self.blur, g, 24, true);
            (ui::text(), ui::text_dim())
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
            c.fill_round(bar, 4, ui::cell());
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
        let (mark, rects, bg) = self.dock_geometry(&items);
        ui::drop_shadow(c, bg, DOCK_RAD, 16, 55);
        ui::panel(c, bg, DOCK_RAD);
        // The justOS mark on a dark tile, like an app's icon: it opens the
        // system menu.
        let menu = self.popup.as_ref().is_some_and(|p| p.kind == PopupKind::Menu && p.up);
        let on_mark = menu || mark.contains(self.mouse.0, self.mouse.1);
        draw_mark_tile(c, mark, on_mark);
        // The status in a well: network, sound and the battery; it opens
        // the control centre.
        let sp = self.status_pill(bg);
        let open = self.popup.as_ref().is_some_and(|p| p.kind == PopupKind::Control);
        let on_status = open || sp.contains(self.mouse.0, self.mouse.1);
        let rad = 18.min(sp.h / 2);
        ui::well_pane(c, sp, rad);
        if on_status {
            c.fill_round(sp, rad, ui::sel());
        }
        let net = if crate::net::online() { ui::text() } else { ui::text_faint() };
        let sound: &[&str] = if crate::dev::audio::volume() == 0 { &G_MUTE } else { &G_VOLUME };
        let sound_col = if self.quiet { ui::text_dim() } else { ui::text() };
        let battery = self.battery.map(|(pct, _)| battery_rows(pct));
        let battery: Option<Vec<&str>> = battery.as_ref().map(|rows| rows.iter().map(|s| s.as_str()).collect());
        let mut symbols: Vec<(&[&str], u32)> = alloc::vec![(&G_WIFI[..], net), (sound, sound_col)];
        if let Some(rows) = &battery {
            symbols.push((rows.as_slice(), ui::text()));
        }
        let widths: Vec<i32> = symbols.iter().map(|(g, _)| ui::soft_size(g, STATUS_CELL).0).collect();
        let total = widths.iter().sum::<i32>() + (symbols.len() as i32 - 1) * STATUS_GAP * 1000;
        let mut x = sp.x * 1000 + (sp.w * 1000 - total) / 2;
        let charge = self.battery.map_or(ui::text(), |(pct, charging)| battery_color(pct, charging));
        for ((g, col), w) in symbols.iter().zip(&widths) {
            let (_, h) = ui::soft_size(g, STATUS_CELL);
            let col = *col;
            ui::soft_pixels_at(c, g, x, sp.y * 1000 + (sp.h * 1000 - h) / 2, STATUS_CELL, &|k| if k == b'f' { charge } else { col });
            x += w + STATUS_GAP * 1000;
        }
        let sep = (ui::text() & 0xFF_FFFF) | 0x2900_0000;
        let sep_at = |c: &mut Canvas, r: Rect| {
            let x = r.x - (SEP_W + DOCK_GAP) / 2 - 1;
            c.fill_round(Rect::new(x, bg.y + (bg.h - 4 - 30) / 2, 2, 30), 1, sep);
        };
        if let Some(first) = rects.first() {
            sep_at(c, *first);
        }
        for (i, it) in items.iter().enumerate() {
            let r = rects[i];
            if it.split && i > 0 {
                sep_at(c, r);
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
            // A program starting: it hops until its window comes.
            if let Some(s) = self.starting.iter().find(|s| s.kind == it.kind) {
                lift = start_lift(s.hop);
            }
            if self.carry.as_ref().is_some_and(|c| c.dock == Some(i)) {
                // It takes the files carried over it.
                c.fill_round(r.grow(6), 18, ui::sel());
                c.stroke_round(r.grow(6), 18, 2, ui::ACCENT);
            }
            res::draw_icon(c, &it.icon, r.w, r.x, r.y - lift);
            let open = self.windows.iter().any(|w| w.app.kind() == it.kind) && it.id != "trash";
            if open {
                c.fill_round(Rect::new(r.x + r.w / 2 - 2, bg.bottom() - 12, 5, 5), 1, ui::ORANGE);
            }
        }
        if let Some(i) = self.hover.filter(|&i| i < items.len()) {
            let r = rects[i];
            let label = &items[i].label;
            let w = text::width(Font::Strong, label) + 24;
            let tip = Rect::new(r.x + r.w / 2 - w / 2, r.y - 44, w, 30);
            ui::drop_shadow(c, tip, 12, 10, 60);
            ui::panel(c, tip, 12);
            let (lh, _) = text::metrics(Font::Strong);
            text::draw_centered(c, Font::Strong, tip.x + w / 2, tip.y + (tip.h - lh) / 2, label, ui::text());
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
        c.fill_round(s, 20, alpha(WALL_FIELD));
        c.stroke_round(s, 20, 1, alpha(ui::GLASS_EDGE));
        res::draw_icon_faded(c, "g-search", 16, s.x + 16, s.y + 12, t * 255 / 1000);
        let (lh, _) = text::metrics(Font::Body);
        if la.query.is_empty() {
            text::draw(c, Font::Body, s.x + 42, s.y + (s.h - lh) / 2, "Cerca", alpha(ui::WHITE_FAINT));
        } else {
            let w = text::draw(c, Font::Body, s.x + 42, s.y + (s.h - lh) / 2, &la.query, alpha(ui::WHITE));
            c.fill(Rect::new(s.x + 44 + w, s.y + 10, 2, s.h - 20), alpha(ui::ACCENT));
        }
        let visible = la.visible();
        let top = la.found_h();
        if visible.is_empty() && la.found.is_empty() {
            text::draw_centered(c, Font::Body, self.w / 2, 190, "Nessun risultato", alpha(ui::WHITE_DIM));
        }
        // Calculations, settings and files, in a glass list.
        let rows = self.found_rows(la.found.len());
        if let (Some(first), Some(last)) = (rows.first(), rows.last()) {
            let panel = Rect::new(first.x - 6, first.y - 6, first.w + 12, last.bottom() - first.y + 12);
            c.fill_round(panel, 18, alpha(WALL_PANEL));
            c.stroke_round(panel, 18, 1, alpha(ui::GLASS_EDGE));
            for (f, r) in la.found.iter().zip(&rows) {
                if full && r.contains(self.mouse.0, self.mouse.1) {
                    c.fill_round(*r, 12, WALL_HOVER);
                }
                res::draw_icon_faded(c, f.icon, 28, r.x + 10, r.y + 7, t * 255 / 1000);
                let font = if f.copy.is_some() { Font::Heading } else { Font::Strong };
                let (lh, _) = text::metrics(font);
                let tag_w = text::width(Font::Small, f.section) + 12;
                let name = text::fit(font, &f.name, r.w / 2);
                let w = text::draw(c, font, r.x + 50, r.y + (r.h - lh) / 2, &name, alpha(ui::WHITE));
                let detail = text::fit(Font::Small, &f.detail, r.w - 50 - w - 24 - tag_w);
                let (sh, _) = text::metrics(Font::Small);
                text::draw(c, Font::Small, r.x + 62 + w, r.y + (r.h - sh) / 2, &detail, alpha(ui::WHITE_FAINT));
                text::draw_right(c, Font::Small, r.right() - 12, r.y + (r.h - sh) / 2, f.section, alpha(ui::WHITE_DIM));
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
                c.fill_round(tile.grow(-6), 18, WALL_HOVER);
            }
            let size = (64 * zoom / 1000) & !1;
            res::draw_icon_faded(c, &e.icon, size, tile.x + (tile.w - size) / 2, tile.y + 12, t * 255 / 1000);
            let label = text::fit(Font::Body, &e.name, tile.w - 12);
            text::draw_centered(c, Font::Body, tile.x + tile.w / 2, tile.y + 86, &label, alpha(ui::WHITE));
        }
        c.set_clip(saved);
        text::draw_centered(c, Font::Small, self.w / 2, self.h - 36, "Ctrl+Spazio apre e chiude · Esc per uscire", alpha(ui::WHITE_FAINT));
    }

    fn draw_popup(&self, c: &mut Canvas, p: &Popup) {
        let saved = c.clip();
        c.set_clip(saved.intersect(&p.shown()));
        match p.kind {
            PopupKind::Menu => {
                ui::drop_shadow(c, p.rect, 18, 14, 70);
                ui::panel(c, p.rect, 18);
                let (lh, _) = text::metrics(Font::Body);
                let mut y = p.rect.y + 6;
                for (label, act) in &p.items {
                    if act.is_none() {
                        c.fill(Rect::new(p.rect.x + 14, y + 5, p.rect.w - 28, 1), ui::line());
                        y += 11;
                        continue;
                    }
                    let r = Rect::new(p.rect.x + 6, y, p.rect.w - 12, 32);
                    if r.contains(self.mouse.0, self.mouse.1) {
                        c.fill_round(r, 12, ui::sel());
                    }
                    text::draw(c, Font::Body, r.x + 12, r.y + (r.h - lh) / 2, label, ui::text());
                    y += 34;
                }
            }
            PopupKind::Control => self.draw_control(c),
            PopupKind::Notices => self.draw_notices(c),
        }
        c.set_clip(saved);
    }

    /// The fonts of the control centre's cards: names and their states
    /// (smaller ones when the design is drawn small).
    fn cc_fonts(&self) -> (Font, Font) {
        if self.cc(14) >= 13 {
            (Font::Strong, Font::Small)
        } else {
            (Font::Small, Font::Small)
        }
    }

    /// A name and, under it, its state, in the middle of `r` from its left
    /// end, at most `w` wide.
    fn cc_label(&self, c: &mut Canvas, x: i32, r: Rect, w: i32, name: &str, state: &str) {
        let (nf, sf) = self.cc_fonts();
        let (nh, _) = text::metrics(nf);
        let (sh, _) = text::metrics(sf);
        let y = r.y + (r.h - nh - sh) / 2;
        text::draw(c, nf, x, y, &text::fit(nf, name, w), ui::text());
        text::draw(c, sf, x, y + nh, &text::fit(sf, state, w), ui::text_dim());
    }

    fn draw_control(&self, c: &mut Canvas) {
        let hover = self.control_hit(self.mouse.0, self.mouse.1);
        let lit = |h: ControlHit| hover == Some(h);
        let panel = self.control_panel();
        let rad = self.cc(CC_RAD);
        let card_rad = self.cc(18);
        ui::drop_shadow(c, panel, rad, 18, 80);
        ui::panel(c, panel, rad);
        let card = |c: &mut Canvas, r: Rect, hot: bool| {
            ui::well_pane(c, r, card_rad);
            if hot {
                c.fill_round(r, card_rad, ui::sel());
            }
        };
        let pad = self.cc(12);

        // Who uses this computer: the initial on an orange disc, the name
        // and the account, the battery, and the settings at the right.
        let acc = self.control_account();
        card(c, acc, lit(ControlHit::Account));
        let d = self.cc(36);
        let photo = Rect::new(acc.x + pad, acc.y + (acc.h - d) / 2, d, d);
        c.fill_round_grad(photo, d / 2, ui::ORANGE_GRAD.0, ui::ORANGE_GRAD.1);
        c.sheen_round(photo, d / 2, 1, ui::TILE_SHEEN);
        let name = settings::get().name;
        let name = if name.is_empty() { String::from("Utente") } else { name };
        let initial: String = name.chars().next().map(|ch| ch.to_uppercase().collect()).unwrap_or_default();
        let pf = if d >= 32 { Font::Heading } else { Font::Strong };
        let (ph, _) = text::metrics(pf);
        text::draw_centered(c, pf, photo.x + d / 2, photo.y + (d - ph) / 2, &initial, ui::WHITE);
        let sb = self.settings_btn();
        let grad = if lit(ControlHit::Settings) { ui::lighten_grad(ui::knob_grad(), 30) } else { ui::knob_grad() };
        ui::tile(c, sb, sb.w / 4, grad);
        tile_glyph(c, &G_GEAR, sb, ui::knob_glyph());
        // The battery's charge, or the power supply.
        let (_, sf) = self.cc_fonts();
        let (sh, _) = text::metrics(sf);
        let cell = self.cc(3000);
        let (power, label, col) = match self.battery {
            Some((pct, charging)) => (battery_rows(pct), format!("{pct} %"), battery_color(pct, charging)),
            None => (G_PLUG.iter().map(|s| String::from(*s)).collect(), String::from("Alimentatore"), ui::text_dim()),
        };
        let power: Vec<&str> = power.iter().map(|s| s.as_str()).collect();
        let (gw, gh) = ui::soft_size(&power, cell);
        let lw = text::width(sf, &label);
        let right = sb.x - self.cc(12);
        let gx = right - lw - self.cc(6) - gw / 1000;
        let dim = ui::text_dim();
        ui::soft_pixels_at(c, &power, gx * 1000, acc.y * 1000 + (acc.h * 1000 - gh) / 2, cell, &|k| if k == b'f' { col } else { dim });
        text::draw(c, sf, right - lw, acc.y + (acc.h - sh) / 2, &label, dim);
        let tx = photo.right() + pad;
        self.cc_label(c, tx, acc, gx - self.cc(10) - tx, &name, "Account locale");

        // The switches: a tile with a symbol, coloured when on, grey when
        // off; the name and the state.
        let online = crate::net::online();
        for (tile, r) in self.control_tiles() {
            let (on, title, state, symbol, color): (bool, &str, String, &[&str], (u32, u32)) = match tile {
                Tile::Network => match crate::net::address() {
                    Some(a) if online => (true, "Rete", format!("{a}"), &G_WIFI[..], BLUE_GRAD),
                    _ => (false, "Rete", String::from("Non connessa"), &G_WIFI[..], BLUE_GRAD),
                },
                Tile::Bluetooth => (false, "Bluetooth", String::from("Assente"), &G_BLUETOOTH[..], BLUE_GRAD),
                Tile::Quiet => (self.quiet, "Non disturbare", String::from(if self.quiet { "Attivo" } else { "Spento" }), &G_MOON[..], VIOLET_GRAD),
            };
            let hot = lit(ControlHit::Tile(tile));
            card(c, r, hot);
            let s = self.cc(38);
            let t = Rect::new(r.x + pad, r.y + (r.h - s) / 2, s, s);
            let (grad, fg) = if on { (color, ui::WHITE) } else { (ui::knob_grad(), ui::knob_glyph()) };
            ui::tile(c, t, s / 4, grad);
            tile_glyph(c, symbol, t, fg);
            let tx = t.right() + self.cc(10);
            let mut end = r.right() - pad;
            if tile == Tile::Quiet {
                // Its switch at the right.
                let (sw, sh) = (self.cc(44), self.cc(26));
                let at = Rect::new(r.right() - pad - sw, r.y + (r.h - sh) / 2, sw, sh);
                ui::toggle(c, at, self.quiet);
                end = at.x - self.cc(10);
            }
            self.cc_label(c, tx, r, end - tx, title, &state);
        }

        // Brightness and volume: a row of cells each, a symbol at the left
        // (the speaker turns the sound off and on).
        let vol = crate::dev::audio::volume();
        let [vs, bs] = self.control_sliders();
        let glyph_cell = self.cc(3000);
        for (s, value, symbol, accent, hit) in [
            (bs, super::brightness(), &G_SUN[..], false, ControlHit::Brightness),
            (vs, vol, if vol == 0 { &G_MUTE[..] } else { &G_VOLUME[..] }, true, ControlHit::Volume),
        ] {
            card(c, s, false);
            let bar = self.slider_bar(s);
            let foot = Rect::new(s.x + pad, s.y, bar.x - self.cc(12) - s.x - pad, s.h);
            if accent && lit(hit) && self.on_slider_foot(s, self.mouse.0) {
                c.fill_round(foot.grow(-self.cc(4)), self.cc(8), ui::sel());
            }
            let (gw, gh) = ui::soft_size(symbol, glyph_cell);
            let gx = foot.x * 1000 + (foot.w * 1000 - gw) / 2;
            let fg = ui::text();
            ui::soft_pixels_at(c, symbol, gx, s.y * 1000 + (s.h * 1000 - gh) / 2, glyph_cell, &|_| fg);
            ui::cell_bar(c, Rect::new(bar.x, bar.y + (bar.h - self.cc(14)) / 2, bar.w, self.cc(14)), value.min(100) * 10, accent);
        }

        // Lock and restart, and power off in orange.
        let [lock, restart, off] = self.control_buttons();
        for (r, hit, symbol, label, primary) in [
            (lock, ControlHit::Lock, &G_LOCK, "Blocca", false),
            (restart, ControlHit::Restart, &G_RESTART, "Riavvia", false),
            (off, ControlHit::PowerOff, &G_POWER, "Spegni", true),
        ] {
            ui::button(c, r, "", primary, lit(hit));
            let fg = if primary { ui::WHITE } else { ui::text() };
            let bf = if self.cc(14) >= 13 { Font::Strong } else { Font::Small };
            let cell = self.cc(2000).max(1000);
            let (gw, gh) = ui::soft_size(symbol, cell);
            let lw = text::width(bf, label);
            let gap = self.cc(8);
            let x = r.x + (r.w - gw / 1000 - gap - lw) / 2;
            ui::soft_pixels_at(c, symbol, x * 1000, r.y * 1000 + (r.h * 1000 - gh) / 2, cell, &|_| fg);
            let (lh, _) = text::metrics(bf);
            text::draw(c, bf, x + gw / 1000 + gap, r.y + (r.h - lh) / 2, label, fg);
        }
    }

    fn draw_notices(&self, c: &mut Canvas) {
        let (cal, list) = self.notices_parts();
        for r in [cal, list] {
            ui::drop_shadow(c, r, 26, 18, 80);
            ui::panel(c, r, 26);
        }
        draw_month(c, Rect::new(cal.x + 6, cal.y + 8, cal.w - 12, cal.h - 16), &now(), ui::text(), ui::text_dim());
        if self.history.is_empty() {
            let (lh, _) = text::metrics(Font::Body);
            text::draw_centered(c, Font::Body, list.x + list.w / 2, list.y + list.h / 2 - lh, "Nessuna notifica", ui::text_dim());
            return;
        }
        let saved = c.clip();
        c.set_clip(saved.intersect(&Rect::new(list.x, list.y + 8, list.w, list.h - 64)));
        let mut y = list.y + 12;
        for (title, body, icon, at) in self.history.iter().rev() {
            if y > list.bottom() - 64 {
                break;
            }
            let r = Rect::new(list.x + 12, y, list.w - 24, 78);
            ui::well_pane(c, r, 18);
            res::draw_icon(c, icon, 44, r.x + 14, r.y + 17);
            let tx = r.x + 72;
            let tw = r.right() - tx - 14;
            let tw_title = tw - text::width(Font::Small, at) - 8;
            text::draw(c, Font::Strong, tx, r.y + 16, &text::fit(Font::Strong, title, tw_title), ui::text());
            text::draw_right(c, Font::Small, r.right() - 14, r.y + 18, at, ui::text_faint());
            for (k, line) in text::wrap(Font::Small, body, tw).iter().take(2).enumerate() {
                text::draw(c, Font::Small, tx, r.y + 38 + k as i32 * 17, line, ui::text_dim());
            }
            y += 88;
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
                c.stroke_round(th.grow(3), 8, 3, alpha(WALL_EDGE_STRONG));
            } else {
                c.stroke_round(th, 6, 1, alpha(WALL_EDGE));
            }
            text::draw_centered(c, Font::Small, th.x + th.w / 2, th.bottom() + 8, &format!("Desktop {}", i + 1), alpha(ui::WHITE));
            if still && dragging.is_none() && self.spaces > 1 && th.grow(10).contains(mx, my) {
                let b = space_close(th);
                c.fill_circle(b.x + b.w / 2, b.y + b.h / 2, b.w / 2, ui::GLASS_STRONG | 0xFF00_0000);
                c.stroke_round(b, b.w / 2, 1, ui::GLASS_EDGE);
                text::draw_centered(c, Font::Small, b.x + b.w / 2, b.y + (b.h - sh) / 2, "×", ui::WHITE);
            }
        }
        if let Some(p) = plus {
            let p = p.offset(0, -drop);
            let hot = (still && p.contains(mx, my)) || (dragging.is_some() && p.grow(8).contains(mx, my));
            c.fill_round(p, 10, alpha(if hot { WALL_PANEL } else { WALL_LIT }));
            c.stroke_round(p, 10, 1, if dragging.is_some() && hot { ui::ACCENT } else { alpha(ui::GLASS_EDGE) });
            let (hh, _) = text::metrics(Font::Heading);
            text::draw_centered(c, Font::Heading, p.x + p.w / 2, p.y + (p.h - hh) / 2, "+", alpha(ui::WHITE));
        }
        // This desktop's windows glide from their places to the grid.
        let slots = self.overview_slots();
        if slots.is_empty() {
            text::draw_centered(c, Font::Body, self.w / 2, self.h / 2, "Nessuna finestra su questo desktop", alpha(ui::WHITE_DIM));
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
                c.stroke_round(to.grow(4), 16, 3, WALL_EDGE_STRONG);
                let title = self.window(id).map(|w| w.app.title()).unwrap_or_default();
                let name = text::fit(Font::Strong, &title, to.w.max(180));
                let w = text::width(Font::Strong, &name) + 24;
                let tip = Rect::new(to.x + to.w / 2 - w / 2, to.bottom() + 10, w, 28);
                c.fill_round(tip, 14, ui::GLASS_STRONG);
                text::draw_centered(c, Font::Strong, tip.x + w / 2, tip.y + 5, &name, ui::WHITE);
            }
        }
        // The window being dragged follows the pointer, on top.
        if let (Some((pic, at, r, to)), Some((_, px, py, _))) = (lifted, dragging) {
            let frame = to.offset(mx - px, my - py);
            c.draw_scaled(pic, place_snap(at, r, frame), 220);
        }
        let hint = "Clic su una finestra per sceglierla · trascinala su un desktop per spostarla · Esc per uscire";
        text::draw_centered(c, Font::Small, self.w / 2, self.h - 30, hint, alpha(ui::WHITE_FAINT));
    }

    /// "Desktop 2" and a dot for each desktop, for a moment after a switch.
    fn draw_space_hud(&self, c: &mut Canvas, a: &Anim) {
        let left = SPACE_HUD_MS.saturating_sub(a.elapsed());
        let t = if left < 250 { (left * 1000 / 250) as u32 } else { 1000 };
        let alpha = |col: u32| (((col >> 24) * t / 1000) << 24) | (col & 0xFF_FFFF);
        let r = self.space_hud_rect();
        c.fill_round(r, 22, alpha(ui::GLASS_STRONG));
        c.stroke_round(r, 22, 1, alpha(ui::GLASS_EDGE));
        text::draw_centered(c, Font::Heading, r.x + r.w / 2, r.y + 22, &format!("Desktop {}", self.space + 1), alpha(ui::WHITE));
        let n = self.spaces as i32;
        let x0 = r.x + r.w / 2 - (n - 1) * 9;
        for i in 0..n {
            let col = if i as usize == self.space { ui::WHITE } else { WALL_EDGE };
            c.fill_circle(x0 + i * 18, r.y + 68, 4, alpha(col));
        }
    }

    fn draw_notes(&self, c: &mut Canvas) {
        for (i, n) in self.notes.iter().enumerate() {
            let r = self.note_rect(i, n.slide());
            if r.x >= self.w {
                continue;
            }
            ui::drop_shadow(c, r, 22, 18, 80);
            ui::panel(c, r, 22);
            res::draw_icon(c, &n.icon, 40, r.x + 14, r.y + 18);
            text::draw(c, Font::Strong, r.x + 66, r.y + 14, &text::fit(Font::Strong, &n.title, r.w - 84), ui::text());
            let lines = text::wrap(Font::Small, &n.body, r.w - 84);
            for (k, line) in lines.iter().take(2).enumerate() {
                text::draw(c, Font::Small, r.x + 66, r.y + 36 + k as i32 * 17, line, ui::text_dim());
            }
        }
    }

    fn draw_ending(&self, c: &mut Canvas, a: &Anim) {
        let t = a.out();
        c.fill(c.clip(), (t * 255 / 1000) << 24);
        if t > 300 {
            let o = (t - 300) * 255 / 700;
            draw_logo(c, self.w / 2, self.h / 2 - 20, 35, (o << 24) | (CREAM & 0xFF_FFFF));
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

/// "16:02" in the clock's soft-pixel digits, one row of cells per string,
/// a column of space between characters.
fn clock_rows(s: &str) -> [String; 5] {
    let mut rows: [String; 5] = Default::default();
    let mut first = true;
    for ch in s.chars() {
        let glyph = match ch.to_digit(10) {
            Some(d) => &DIGITS[d as usize],
            None if ch == ':' => &COLON,
            None => continue,
        };
        for (row, part) in rows.iter_mut().zip(glyph.iter()) {
            if !first {
                row.push('.');
            }
            row.push_str(part);
        }
        first = false;
    }
    rows
}

/// The size of the clock's digits for `s`, in units.
fn clock_size(s: &str) -> (i32, i32) {
    let cols = clock_rows(s)[0].len() as i32;
    (cols * CLOCK_CELL / 1000, 5 * CLOCK_CELL / 1000)
}

/// The height of the clock and the date under it, floating at the top.
fn clock_height() -> i32 {
    let (sh, _) = text::metrics(Font::Strong);
    CLOCK_TOP + 5 * CLOCK_CELL / 1000 + CLOCK_DATE_GAP + sh + CLOCK_BOTTOM
}

/// The battery as soft pixels: its outline and, inside, up to four cells of
/// charge ('f').
fn battery_rows(pct: u8) -> Vec<String> {
    let n = match pct {
        0 => 0,
        p => (usize::from(p.min(100)) * 4).div_ceil(100),
    };
    let fill: String = (0..4).map(|i| if i < n { 'f' } else { '.' }).collect();
    alloc::vec![
        String::from("########."),
        String::from("#......##"),
        format!("#.{fill}.##"),
        String::from("#......##"),
        String::from("########."),
    ]
}

/// The colour of the battery's charge: green while charging, red when low.
fn battery_color(pct: u8, charging: bool) -> u32 {
    if charging {
        ui::GREEN
    } else if pct <= 10 {
        ui::RED
    } else {
        ui::text()
    }
}

/// A soft-pixel symbol in the middle of tile `t`, about two thirds of it
/// wide, its cells a whole number of units (crisp).
fn tile_glyph(c: &mut Canvas, rows: &[&str], t: Rect, color: u32) {
    let (w, h) = ui::soft_size(rows, 1);
    let n = w.max(h).max(1);
    let cell = ((t.w * 13 + 10 * n) / (20 * n)).max(1) * 1000;
    ui::soft_pixels_in(c, rows, t, cell, color);
}

/// The justOS mark on its dark tile, as large as a dock icon (`hot`: under
/// the pointer, a little lighter).
fn draw_mark_tile(c: &mut Canvas, r: Rect, hot: bool) {
    let grad = if hot { ui::lighten_grad(MARK_GRAD, 25) } else { MARK_GRAD };
    ui::tile(c, r, r.w / 4, grad);
    // Cells of a thirteenth of the tile, as on justapps.it.
    ui::soft_pixels_keyed(c, &G_MARK, r, r.w * 1000 / 13, &|k| if k == b'o' { ui::ORANGE } else { CREAM });
}

/// The month of `d` in `g`: its name, the weekdays and the days, today on
/// an orange tile.
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
            let s = 26.min(row_h);
            ui::tile(c, Rect::new(cx - s / 2, cy - s / 2, s, s), s / 4, ui::ORANGE_GRAD);
            let (lh, _) = text::metrics(Font::Strong);
            text::draw_centered(c, Font::Strong, cx, cy - lh / 2, &label, ui::WHITE);
        } else {
            let (lh, _) = text::metrics(Font::Body);
            text::draw_centered(c, Font::Body, cx, cy - lh / 2, &label, if col >= 5 { dim } else { fg });
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
            || !self.starting.is_empty()
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
                        ("Info su justOS", Some(MenuAct::About)),
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
                // Windows of Linux programs (Wayland).
                for app in crate::linux::wayland::take_new() {
                    self.open(l, app, false);
                }
                // Questions of the sandbox, and file pickers for programs.
                for app in super::apps::prompt::take_new() {
                    self.open(l, app, false);
                }
                self.check_starting(l);
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
                // Icons of programs starting hop: drawn again only when
                // one moved.
                let mut hopped = false;
                for s in self.starting.iter_mut() {
                    let lift = start_lift(s.hop);
                    hopped |= lift != s.lift;
                    s.lift = lift;
                }
                if hopped {
                    l.damage(self.starting_zone());
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
        let clock_under = self.clock_covered();
        if clock_under && r.overlaps(&self.clock_zone()) {
            self.draw_clock(c);
        }
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
                    // A dark edge over a light picture, a white one over a dark one.
                    let edge = if ui::wants_ink(blur, zone) { WALL_SNAP_INK } else { WALL_SNAP };
                    c.stroke_round(zone, 22, 2, edge);
                }
            }
            if w.shown(self.space) && r.overlaps(&w.bounds()) && !fx.iter().any(|f| f.id == w.id) {
                let f = Some(w.id) == focused;
                w.draw(c, blur, f, r);
            }
        }
        for f in self.fx.iter_mut() {
            let (dst, opacity) = f.place();
            if r.overlaps(&dst) {
                c.draw_scaled(&f.snap, dst, opacity);
                // Time has passed since the frame event that asked for this
                // drawing: remember where the picture really went, so that
                // the next frame clears all of it.
                f.last = f.last.union(&dst.grow(2));
            }
        }
        if self.dock_shown() && r.overlaps(&self.dock_zone()) {
            self.draw_dock(c);
        }
        if !clock_under && r.overlaps(&self.clock_zone()) {
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
        starting: Vec::new(),
        mouse: (-1, -1),
        minute: u64::MAX,
        second: 0,
        last_new: None,
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

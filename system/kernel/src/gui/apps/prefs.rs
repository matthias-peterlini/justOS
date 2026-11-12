//! Impostazioni: the settings, in sections, as other systems have them. At
//! the left a search field and the list of sections; at the right the
//! section chosen, as cards of rows: a label (with a line that explains it)
//! on the left, its control on the right. Privacy says what each app may use
//! outside its sandbox ([`crate::sandbox`]).
//!
//! A section is a list of cards, each a list of [`Row`]s
//! ([`Prefs::cards`]); [`Prefs::page`] lays them out, and drawing, clicks,
//! the pointer and the search all work from that layout. A new setting is a
//! row there, plus what its [`Ctl`] draws ([`Prefs::draw_ctl`]), where its
//! parts are ([`Prefs::parts`]) and what a click does ([`Prefs::act`]).

use crate::dev::input::{Key, KeyEvent};
use crate::dev::keymap::Layout;
use crate::gui::gfx::{Canvas, Rect};
use crate::gui::res;
use crate::gui::settings;
use crate::gui::text::{self, Font};
use crate::gui::ui;
use crate::gui::wm::{Action, App, GAP};
use crate::sandbox::Grants;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use justdk::DisplayMode;

/// Resolutions offered when the display has them, in this order.
const COMMON_MODES: &[(u32, u32)] = &[
    (1024, 768),
    (1280, 720),
    (1280, 800),
    (1366, 768),
    (1440, 900),
    (1600, 900),
    (1680, 1050),
    (1920, 1080),
    (1920, 1200),
    (2560, 1440),
    (2560, 1600),
    (3840, 2160),
];

/// The sidebar's width.
const SIDEBAR: i32 = 210;
/// Where the list of sections starts (under the search field), the height
/// of a section's row and the step from one to the next.
const SIDE_TOP: i32 = 62;
const SIDE_ROW: i32 = 38;
const SIDE_STEP: i32 = 40;
/// The margin round the content, and its widest column.
const MARGIN: i32 = 28;
const COLUMN: i32 = 720;
/// The margin inside a card, its corner, and the gap between cards.
const INSET: i32 = 16;
const CARD_RAD: i32 = 18;
const CARD_GAP: i32 = 18;
/// The lowest row.
const ROW_MIN: i32 = 52;
/// The header above a card of search results (the section's tile and name).
const HEAD_H: i32 = 30;
/// How long a row stays lit after a search led to it.
const FLASH_MS: u64 = 1400;

/// The choices for locking after a while without input (minutes; 0: never).
const LOCK_AFTER: [(u32, &str); 4] = [(0, "Mai"), (2, "2 min"), (5, "5 min"), (15, "15 min")];

/// The sizes of the interface (pixels per unit; 0: by the screen).
const SIZES: [(u32, &str); 3] = [(0, "Auto"), (1, "100%"), (2, "200%")];

/// The themes of windows and panels.
const THEMES: [(ui::Theme, &str); 2] = [(ui::Theme::Dark, "Scuro"), (ui::Theme::Light, "Chiaro")];

/// The keyboard layouts.
const LAYOUTS: [(Layout, &str); 2] = [(Layout::It, "Italiano"), (Layout::Us, "English (US)")];

// The labels of the rows a note can appear under.
const L_NAME: &str = "Il tuo nome";
const L_SETUP: &str = "Configurazione iniziale";
const L_MODES: &str = "Risoluzione";
const L_SCALE: &str = "Dimensione interfaccia";
const L_LAYOUT: &str = "Disposizione dei tasti";
const L_PASSWORD: &str = "Password";

/// The sections, in the order of the sidebar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Section {
    General,
    Look,
    Screen,
    Sound,
    Keyboard,
    Lock,
    Privacy,
}

const SECTIONS: [Section; 7] =
    [Section::General, Section::Look, Section::Screen, Section::Sound, Section::Keyboard, Section::Lock, Section::Privacy];

impl Section {
    fn name(self) -> &'static str {
        match self {
            Section::General => "Generale",
            Section::Look => "Aspetto",
            Section::Screen => "Schermo",
            Section::Sound => "Suono",
            Section::Keyboard => "Tastiera",
            Section::Lock => "Blocco e password",
            Section::Privacy => "Privacy",
        }
    }

    /// The gradient of its tile, top and bottom.
    fn grad(self) -> (u32, u32) {
        match self {
            Section::General => (0xFFA0_A0A6, 0xFF62_6267),
            Section::Look => (0xFFA7_8BFA, 0xFF6D_3FE0),
            Section::Screen => (0xFF5A_A9FF, 0xFF25_63EB),
            Section::Sound => (0xFFFF_6B81, 0xFFE1_1D48),
            Section::Keyboard => (0xFF5F_E08A, 0xFF1F_A64F),
            Section::Lock => ui::ORANGE_GRAD,
            Section::Privacy => (0xFF7C_8CFF, 0xFF3F_4FD8),
        }
    }

    /// Its symbol in soft pixels, five cells by five.
    fn glyph(self) -> &'static [&'static str; 5] {
        match self {
            // A cog.
            Section::General => &["#.#.#", ".###.", "##.##", ".###.", "#.#.#"],
            // A circle half dark, half light.
            Section::Look => &[".###.", "##..#", "##..#", "##..#", ".###."],
            // A screen on its stand.
            Section::Screen => &["#####", "#...#", "#####", "..#..", ".###."],
            // A loudspeaker.
            Section::Sound => &["..#..", "###.#", "###.#", "###.#", "..#.."],
            // Keys and the space bar.
            Section::Keyboard => &["#.#.#", ".....", "#.#.#", ".....", ".###."],
            // A padlock.
            Section::Lock => &[".###.", ".#.#.", "#####", "##.##", "#####"],
            // A raised hand.
            Section::Privacy => &["#.#.#", "#.#.#", "#####", "#####", ".###."],
        }
    }

    /// A few words under the title, if any.
    fn intro(self) -> &'static str {
        match self {
            Section::Privacy => "Le app del Negozio vedono solo la propria cartella. Qui scegli a cos'altro possono accedere.",
            _ => "",
        }
    }
}

/// The loudspeaker next to the volume, and the same when it is silent.
const GLYPH_VOLUME: [&str; 5] = ["..#..", "###.#", "###.#", "###.#", "..#.."];
const GLYPH_MUTE: [&str; 5] = ["..#..", "###..", "###..", "###..", "..#.."];

/// The control of a row: what it shows and what it changes.
#[derive(Clone, PartialEq)]
enum Ctl {
    /// A value to read, at the right.
    Info(String),
    /// Nothing: the row is a line of text.
    Text,
    /// The name field and "Salva".
    Name,
    /// The first-start setup again, at the next start.
    Setup,
    /// Dark or light: a small picture of a window in each.
    Themes,
    /// The wallpapers, a grid of previews.
    Wallpapers,
    /// The resolutions the display offers.
    Modes,
    /// The size of the interface.
    Scale,
    /// The volume: cells that fill up.
    Volume,
    /// A short tone.
    Test,
    /// The sound when the desktop appears.
    Chime,
    /// The keyboard layout.
    Layout,
    /// The password field and its button.
    Password,
    /// When the screen locks by itself.
    LockAfter,
    /// What app `i` was allowed, asked again.
    Forget(usize),
    /// Permission `j` of app `i`, on or off.
    Perm(usize, usize),
    /// A search result: the section it is in, and the row's label.
    Go(Section, String),
}

impl Ctl {
    /// Laid out under its label, the whole width of the card.
    fn block(&self) -> bool {
        matches!(self, Ctl::Wallpapers | Ctl::Modes)
    }
}

/// A row of a card: a label, a line under it (may be empty) and a control.
#[derive(Clone)]
struct Row {
    label: String,
    desc: String,
    ctl: Ctl,
}

fn row(label: &str, desc: &str, ctl: Ctl) -> Row {
    Row { label: String::from(label), desc: String::from(desc), ctl }
}

/// A card of rows; search results have their section above them.
struct Card {
    head: Option<Section>,
    rows: Vec<Row>,
}

fn card(rows: Vec<Row>) -> Card {
    Card { head: None, rows }
}

/// A row where it is drawn.
struct Placed {
    r: Rect,
    ctl_r: Rect,
    /// The top-left corner of the label, and how wide it and its line may be.
    text_x: i32,
    text_y: i32,
    text_w: i32,
    /// The first of its card (no line above it).
    first: bool,
    row: Row,
}

/// The content pane laid out.
struct Page {
    title: String,
    title_y: i32,
    intro: Vec<String>,
    intro_y: i32,
    cards: Vec<(Rect, Option<Section>)>,
    rows: Vec<Placed>,
    /// The left edge and width of the column.
    x: i32,
    w: i32,
    /// Everything from the top, margins included.
    height: i32,
}

/// What is under a point of the window.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Target {
    Search,
    Side(Section),
    /// Part `k` of the control of row `i` of the page (a segment, a
    /// wallpaper, a field or the button beside it...).
    Ctl(usize, usize),
    /// Search result row `i` of the page.
    Result(usize),
}

pub struct Prefs {
    section: Section,
    /// What is typed in the search field, and whether keys go there.
    query: String,
    searching: bool,
    /// The search result the keyboard is on.
    pick: usize,
    name: String,
    editing: bool,
    /// A new password being typed.
    password: String,
    editing_password: bool,
    /// A word in place of a row's line after a change: the row's label and
    /// the word.
    note: (&'static str, &'static str),
    /// The wallpaper previews, and the wallpaper behind the pictures of the
    /// themes (which one, and the picture).
    thumbs: Vec<Canvas>,
    backdrop: Option<(String, Canvas)>,
    /// Resolutions the display driver offers (empty without one).
    modes: Vec<DisplayMode>,
    /// The volume is being dragged.
    sliding: bool,
    /// What the apps were allowed.
    grants: Vec<Grants>,
    scroll: i32,
    max_scroll: i32,
    /// The content pane's height at the last drawing.
    view_h: i32,
    /// What is under the pointer (only things that light up).
    hot: Option<Target>,
    /// A row lit for a moment after a search led to it: its label and
    /// until when; `reveal`: scroll to it at the next drawing.
    flash: Option<(String, u64)>,
    reveal: bool,
}

/// `s` in lower case without accents, for the search.
fn fold(s: &str) -> String {
    s.chars()
        .flat_map(char::to_lowercase)
        .map(|ch| match ch {
            'à' | 'á' | 'â' | 'ä' => 'a',
            'è' | 'é' | 'ê' | 'ë' => 'e',
            'ì' | 'í' | 'î' | 'ï' => 'i',
            'ò' | 'ó' | 'ô' | 'ö' => 'o',
            'ù' | 'ú' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            ch => ch,
        })
        .collect()
}

/// `s` with a capital first letter.
fn capitalised(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    if let Some(first) = chars.next() {
        out.extend(first.to_uppercase());
        out.push_str(chars.as_str());
    }
    out
}

/// The width of a button with `label`.
fn button_w(label: &str) -> i32 {
    (text::width(Font::Strong, label) + 36).max(88)
}

/// The width of a segmented control with `labels`.
fn segmented_w(labels: &[&str]) -> i32 {
    let widest = labels.iter().map(|l| text::width(Font::Strong, l)).max().unwrap_or(0);
    (widest + 28).max(64) * labels.len() as i32 + 4
}

/// The pictures of the themes in a row `inner` wide: their size.
fn theme_size(inner: i32) -> (i32, i32) {
    let w = ((inner - 16) / 2).min(168);
    (w, w * 5 / 8)
}

/// The wallpapers in a row `inner` wide: columns, and each one's size.
fn thumb_grid(inner: i32) -> (i32, i32, i32) {
    let cols = ((inner + 12) / (96 + 12)).clamp(1, 4);
    let w = ((inner - (cols - 1) * 12) / cols).min(160);
    (cols, w, w * 5 / 8)
}

/// The resolutions in a row `inner` wide: columns and the buttons' width.
fn mode_grid(inner: i32) -> (i32, i32) {
    let cols = ((inner + 8) / (112 + 8)).clamp(1, 4);
    (cols, ((inner - (cols - 1) * 8) / cols).min(150))
}

/// A text field with its button at the right.
fn field_and_button(r: Rect, label: &str) -> (Rect, Rect) {
    let bw = button_w(label).min(100);
    (Rect::new(r.x, r.y, r.w - bw - 10, r.h), Rect::new(r.right() - bw, r.y + 2, bw, r.h - 4))
}

/// The cells of the volume, right of its loudspeaker.
fn volume_bar(r: Rect) -> Rect {
    Rect::new(r.x + 30, r.y, r.w - 30, r.h)
}

/// A small picture of a window in the theme of palette `p`, over the
/// wallpaper `backdrop` (as big as `r`).
fn draw_theme(c: &mut Canvas, r: Rect, p: &ui::Palette, backdrop: Option<&Canvas>) {
    match backdrop {
        Some(b) => c.blit_round_from(b, 0, 0, r, 12, 0),
        None => c.fill_round_grad(r, 12, 0xFF3B_2E5A, 0xFFE0_8F6E),
    }
    let saved = c.clip();
    c.set_clip(saved.intersect(&r));
    let win = Rect::new(r.x + r.w * 12 / 100, r.y + r.h * 17 / 100, r.w * 76 / 100, r.h * 70 / 100);
    let opaque = |col: u32| col | 0xFF00_0000;
    c.shadow(win, 8, 6, 70);
    c.fill_round_grad(win, 8, opaque(p.panel_top), opaque(p.panel_bottom));
    c.stroke_round(win, 8, 1, p.line);
    c.sheen_round(win, 8, 1, p.sheen);
    // The title and the window's buttons.
    let k = 6;
    for i in 0..3 {
        let kr = Rect::new(win.right() - 6 - (3 - i) * (k + 3) + 3, win.y + 5, k, k);
        let g = if i == 2 { ui::ORANGE_GRAD } else { (p.knob_top, p.knob_bottom) };
        c.fill_round_grad(kr, 2, g.0, g.1);
    }
    c.fill_round(Rect::new(win.x + 8, win.y + 6, win.w / 5, 4), 2, p.text_dim);
    // A sidebar with a row chosen, and the content: a few lines and a
    // button.
    let body = Rect::new(win.x + 4, win.y + 16, win.w - 8, win.h - 20);
    let side = Rect::new(body.x, body.y, body.w * 30 / 100, body.h);
    let main = Rect::new(side.right() + 3, body.y, body.right() - side.right() - 3, body.h);
    for pane in [side, main] {
        c.fill_round(pane, 5, p.well);
        c.stroke_round(pane, 5, 1, p.line);
    }
    for i in 0..3 {
        let rr = Rect::new(side.x + 3, side.y + 4 + i * 9, side.w - 6, 7);
        if i == 0 {
            c.fill_round(rr, 3, p.sel);
        }
        c.fill_round(Rect::new(rr.x + 3, rr.y + 2, rr.w * 3 / 5, 3), 1, if i == 0 { p.text } else { p.text_dim });
    }
    c.fill_round(Rect::new(main.x + 6, main.y + 6, main.w * 45 / 100, 4), 2, p.text);
    c.fill_round(Rect::new(main.x + 6, main.y + 15, main.w * 70 / 100, 3), 1, p.text_dim);
    c.fill_round(Rect::new(main.x + 6, main.y + 22, main.w * 55 / 100, 3), 1, p.text_dim);
    let btn = Rect::new(main.x + 6, main.bottom() - 12, main.w * 30 / 100, 7);
    c.fill_round_grad(btn, 3, ui::ORANGE_GRAD.0, ui::ORANGE_GRAD.1);
    c.set_clip(saved);
}

/// A section's tile with its symbol.
fn draw_tile(c: &mut Canvas, r: Rect, s: Section) {
    ui::tile(c, r, r.w / 4, s.grad());
    ui::soft_pixels_in(c, s.glyph(), r, r.w * 130, ui::WHITE);
}

impl Prefs {
    pub fn new() -> Prefs {
        let offered = crate::dev::display::modes();
        let modes =
            COMMON_MODES.iter().map(|&(width, height)| DisplayMode { width, height }).filter(|m| offered.contains(m)).take(12).collect();
        Prefs {
            section: Section::General,
            query: String::new(),
            searching: false,
            pick: 0,
            name: settings::get().name,
            editing: false,
            password: String::new(),
            editing_password: false,
            note: ("", ""),
            thumbs: Vec::new(),
            backdrop: None,
            modes,
            sliding: false,
            grants: crate::sandbox::all(),
            scroll: 0,
            max_scroll: 0,
            view_h: 0,
            hot: None,
            flash: None,
            reveal: false,
        }
    }

    // ------------------------------------------------------------------
    // What the sections hold

    /// The cards of section `s`.
    fn cards(&self, s: Section) -> Vec<Card> {
        match s {
            Section::General => vec![
                card(vec![row(L_NAME, "Come ti saluta justOS", Ctl::Name)]),
                card(vec![
                    row(L_SETUP, "La rifai al prossimo avvio", Ctl::Setup),
                    row("Versione", "", Ctl::Info(format!("justOS {}", crate::VERSION))),
                ]),
            ],
            Section::Look => {
                vec![card(vec![row("Tema", "Finestre, menu e pannelli", Ctl::Themes)]), card(vec![row("Sfondo", "", Ctl::Wallpapers)])]
            }
            Section::Screen => {
                let modes = if self.modes.is_empty() {
                    let (w, h) = crate::console::pixel_size().unwrap_or((0, 0));
                    row(L_MODES, "Scelta dal firmware: nessun driver video la può cambiare.", Ctl::Info(format!("{w} × {h}")))
                } else {
                    row(L_MODES, &crate::dev::display::controller().unwrap_or_default(), Ctl::Modes)
                };
                let scale = if crate::gui::can_double() {
                    row(L_SCALE, "Testo e finestre più grandi", Ctl::Scale)
                } else {
                    row(L_SCALE, "Il 200% vuole uno schermo di almeno 1600 × 1200", Ctl::Info(String::from("100%")))
                };
                vec![card(vec![modes]), card(vec![scale])]
            }
            Section::Sound => {
                if crate::dev::audio::available() {
                    vec![
                        card(vec![row("Volume", "", Ctl::Volume), row("Suono di prova", "Un breve tono al volume scelto", Ctl::Test)]),
                        card(vec![row("Suono all'avvio", "Quando compare la scrivania", Ctl::Chime)]),
                    ]
                } else {
                    vec![card(vec![row("Nessuna scheda audio trovata.", "Volume e suoni compaiono quando ce n'è una.", Ctl::Text)])]
                }
            }
            Section::Keyboard => vec![card(vec![row(L_LAYOUT, "Le lettere e i simboli dei tasti", Ctl::Layout)])],
            Section::Lock => {
                let pw =
                    if settings::has_password() { "Chiesta per sbloccare il computer" } else { "Nessuna: il computer si sblocca senza" };
                vec![card(vec![row(L_PASSWORD, pw, Ctl::Password), row("Blocca dopo", "Senza usare il computer", Ctl::LockAfter)])]
            }
            Section::Privacy => {
                if self.grants.is_empty() {
                    return vec![card(vec![row("Nessuna app ha ancora chiesto qualcosa.", "", Ctl::Text)])];
                }
                self.grants
                    .iter()
                    .enumerate()
                    .map(|(i, g)| {
                        let name = if g.name.is_empty() { g.id.as_str() } else { g.name.as_str() };
                        let mut rows = vec![row(name, &g.id, Ctl::Forget(i))];
                        for (j, (perm, allow)) in g.perms.iter().enumerate() {
                            let label = capitalised(&crate::sandbox::describe(perm));
                            rows.push(row(&label, if *allow { "consentito" } else { "negato" }, Ctl::Perm(i, j)));
                        }
                        card(rows)
                    })
                    .collect()
            }
        }
    }

    /// The rows whose label (or section) has what is typed in the search
    /// field, a card per section. A line of text ("no audio card") is no
    /// setting: it is found only by its section's name.
    fn results(&self) -> Vec<Card> {
        let q = fold(self.query.trim());
        let mut out = Vec::new();
        for s in SECTIONS {
            let all = fold(s.name()).contains(q.as_str());
            let rows: Vec<Row> = self
                .cards(s)
                .into_iter()
                .flat_map(|c| c.rows)
                .filter(|r| !r.label.is_empty() && (all || (r.ctl != Ctl::Text && fold(&r.label).contains(q.as_str()))))
                .map(|r| row(&r.label, &r.desc, Ctl::Go(s, r.label.clone())))
                .collect();
            if !rows.is_empty() {
                out.push(Card { head: Some(s), rows });
            }
        }
        out
    }

    /// The sections in the sidebar: those with results while searching.
    fn visible(&self) -> Vec<Section> {
        if self.query.trim().is_empty() {
            return SECTIONS.to_vec();
        }
        self.results().iter().filter_map(|c| c.head).collect()
    }

    // ------------------------------------------------------------------
    // Layout

    fn search_rect(area: Rect) -> Rect {
        Rect::new(area.x + 12, area.y + 14, SIDEBAR - 24, 34)
    }

    fn side_rect(area: Rect, k: usize) -> Rect {
        Rect::new(area.x + 8, area.y + SIDE_TOP + k as i32 * SIDE_STEP, SIDEBAR - 16, SIDE_ROW)
    }

    /// The content pane, right of the sidebar.
    fn main(area: Rect) -> Rect {
        Rect::new(area.x + SIDEBAR + GAP, area.y, area.w - SIDEBAR - GAP, area.h)
    }

    /// The choices of a segmented control, and the one chosen.
    fn choices(ctl: &Ctl) -> (Vec<&'static str>, usize) {
        let s = settings::get();
        match ctl {
            Ctl::Scale => (SIZES.iter().map(|(_, l)| *l).collect(), SIZES.iter().position(|(k, _)| *k == s.scale).unwrap_or(0)),
            Ctl::Layout => (LAYOUTS.iter().map(|(_, l)| *l).collect(), LAYOUTS.iter().position(|(k, _)| *k == s.layout).unwrap_or(0)),
            Ctl::LockAfter => {
                (LOCK_AFTER.iter().map(|(_, l)| *l).collect(), LOCK_AFTER.iter().position(|(m, _)| *m == s.lock_after).unwrap_or(2))
            }
            _ => (Vec::new(), 0),
        }
    }

    /// The size of a control in a card `inner` wide.
    fn ctl_size(&self, ctl: &Ctl, inner: i32) -> (i32, i32) {
        let (bh, _) = text::metrics(Font::Body);
        let (sh, _) = text::metrics(Font::Small);
        match ctl {
            Ctl::Info(s) => (text::width(Font::Body, s).min(inner), bh),
            Ctl::Text => (0, 0),
            Ctl::Name | Ctl::Password => (inner.min(320), 38),
            Ctl::Setup => (button_w("Ripeti"), 34),
            Ctl::Test => (button_w("Prova"), 34),
            Ctl::Forget(_) => (button_w("Chiedi di nuovo"), 34),
            Ctl::Chime | Ctl::Perm(..) => (52, 30),
            Ctl::Volume => (inner.min(290), 26),
            Ctl::Scale | Ctl::Layout | Ctl::LockAfter => (segmented_w(&Self::choices(ctl).0).min(inner), 32),
            Ctl::Go(..) => (text::width(Font::Heading, "›"), bh),
            Ctl::Themes => {
                let (tw, th) = theme_size(inner);
                (2 * tw + 16, th + 10 + bh)
            }
            Ctl::Wallpapers => {
                let (cols, _, th) = thumb_grid(inner);
                let rows = (res::WALLPAPERS.len() as i32 + cols - 1) / cols;
                (inner, rows * (th + 6 + sh) + (rows - 1) * 14)
            }
            Ctl::Modes => {
                let (cols, _) = mode_grid(inner);
                let rows = (self.modes.len() as i32 + cols - 1) / cols;
                (inner, rows * 34 + (rows - 1).max(0) * 8)
            }
        }
    }

    /// Places a row at `y` in a card at `x`, `w` wide: the control at the
    /// right of the label when both fit, else under it.
    fn place(&self, row: Row, x: i32, y: i32, w: i32, first: bool) -> Placed {
        let inner = w - 2 * INSET;
        let (bh, _) = text::metrics(Font::Body);
        let (sh, _) = text::metrics(Font::Small);
        let text_h = match (row.label.is_empty(), row.desc.is_empty()) {
            (true, _) => 0,
            (false, true) => bh,
            (false, false) => bh + 2 + sh,
        };
        let (cw, ch) = self.ctl_size(&row.ctl, inner);
        let tx = x + INSET;
        let label_w = text::width(Font::Body, &row.label).max(text::width(Font::Small, &row.desc).min(150));
        if !row.ctl.block() && (cw == 0 || label_w + 24 + cw <= inner) {
            let h = (text_h.max(ch) + 24).max(ROW_MIN);
            let ctl_r = Rect::new(x + w - INSET - cw, y + (h - ch) / 2, cw, ch);
            let text_w = if cw > 0 { ctl_r.x - 16 - tx } else { inner };
            // Beside a tall control the label stays at the top.
            let text_y = if ch > 48 { ctl_r.y + 4 } else { y + (h - text_h) / 2 };
            return Placed { r: Rect::new(x, y, w, h), ctl_r, text_x: tx, text_y, text_w, first, row };
        }
        let top = if text_h > 0 { 14 } else { INSET };
        let gap = if text_h > 0 { 12 } else { 0 };
        let h = top + text_h + gap + ch + INSET;
        let ctl_r = Rect::new(tx, y + top + text_h + gap, cw.min(inner), ch);
        Placed { r: Rect::new(x, y, w, h), ctl_r, text_x: tx, text_y: y + top, text_w: inner, first, row }
    }

    /// The content pane laid out: the section, or the search results.
    fn page(&self, area: Rect) -> Page {
        let main = Self::main(area);
        let margin = if main.w >= 480 { MARGIN } else { 16 };
        let w = (main.w - 2 * margin).clamp(120, COLUMN);
        let x = main.x + (main.w - w) / 2;
        let top = main.y - self.scroll;
        let searching = !self.query.trim().is_empty();
        let (title, cards) = if searching {
            (format!("Risultati per “{}”", self.query.trim()), self.results())
        } else {
            (String::from(self.section.name()), self.cards(self.section))
        };
        let (dh, _) = text::metrics(Font::Display);
        let (bh, _) = text::metrics(Font::Body);
        let title_y = top + 22;
        let mut y = title_y + dh + 16;
        let intro = match self.section.intro() {
            s if !searching && !s.is_empty() => text::wrap(Font::Body, s, w - 8),
            _ => Vec::new(),
        };
        let intro_y = y - 4;
        if !intro.is_empty() {
            y += intro.len() as i32 * (bh + 3) + 12;
        }
        let mut rows = Vec::new();
        let mut boxes = Vec::new();
        for card in cards {
            if card.head.is_some() {
                y += HEAD_H;
            }
            let start = y;
            for (i, r) in card.rows.into_iter().enumerate() {
                let p = self.place(r, x, y, w, i == 0);
                y = p.r.bottom();
                rows.push(p);
            }
            boxes.push((Rect::new(x, start, w, y - start), card.head));
            y += CARD_GAP;
        }
        Page { title, title_y, intro, intro_y, cards: boxes, rows, x, w, height: y - CARD_GAP + MARGIN - top }
    }

    /// Where the parts of a row's control are (what a click can hit).
    fn parts(&self, p: &Placed) -> Vec<Rect> {
        let r = p.ctl_r;
        match &p.row.ctl {
            Ctl::Name => {
                let (f, b) = field_and_button(r, "Salva");
                vec![f, b]
            }
            Ctl::Password => {
                let (f, b) = field_and_button(r, "Imposta");
                vec![f, b]
            }
            Ctl::Setup | Ctl::Test | Ctl::Forget(_) | Ctl::Chime | Ctl::Perm(..) => vec![r],
            Ctl::Volume => vec![volume_bar(r).grow(8)],
            Ctl::Scale | Ctl::Layout | Ctl::LockAfter => ui::segments(r, Self::choices(&p.row.ctl).0.len()),
            Ctl::Themes => {
                let (tw, _) = theme_size(r.w);
                (0..THEMES.len() as i32).map(|i| Rect::new(r.x + i * (tw + 16), r.y, tw, r.h)).collect()
            }
            Ctl::Wallpapers => {
                let (sh, _) = text::metrics(Font::Small);
                let (cols, tw, th) = thumb_grid(r.w);
                (0..res::WALLPAPERS.len() as i32)
                    .map(|i| Rect::new(r.x + (i % cols) * (tw + 12), r.y + (i / cols) * (th + 6 + sh + 14), tw, th))
                    .collect()
            }
            Ctl::Modes => {
                let (cols, bw) = mode_grid(r.w);
                (0..self.modes.len() as i32).map(|i| Rect::new(r.x + (i % cols) * (bw + 8), r.y + (i / cols) * 42, bw, 34)).collect()
            }
            Ctl::Info(_) | Ctl::Text | Ctl::Go(..) => Vec::new(),
        }
    }

    /// What is at (`ax`, `ay`).
    fn hit(&self, page: &Page, area: Rect, ax: i32, ay: i32) -> Option<Target> {
        if Self::search_rect(area).contains(ax, ay) {
            return Some(Target::Search);
        }
        if let Some((_, s)) = self.visible().into_iter().enumerate().find(|(k, _)| Self::side_rect(area, *k).contains(ax, ay)) {
            return Some(Target::Side(s));
        }
        if !Self::main(area).contains(ax, ay) {
            return None;
        }
        for (i, p) in page.rows.iter().enumerate() {
            if matches!(p.row.ctl, Ctl::Go(..)) {
                if p.r.contains(ax, ay) {
                    return Some(Target::Result(i));
                }
                continue;
            }
            if let Some(k) = self.parts(p).iter().position(|r| r.contains(ax, ay)) {
                return Some(Target::Ctl(i, k));
            }
        }
        None
    }

    // ------------------------------------------------------------------
    // Drawing

    fn draw_sidebar(&self, c: &mut Canvas, area: Rect) {
        ui::search_field(c, Self::search_rect(area), &self.query, "Cerca", self.searching);
        let searching = !self.query.trim().is_empty();
        let (bh, _) = text::metrics(Font::Body);
        for (k, s) in self.visible().into_iter().enumerate() {
            let r = Self::side_rect(area, k);
            let chosen = !searching && s == self.section;
            if chosen {
                c.fill_round(r, 12, ui::sel());
            } else if self.hot == Some(Target::Side(s)) {
                c.fill_round(r, 12, ui::hover());
            }
            draw_tile(c, Rect::new(r.x + 7, r.y + (r.h - 26) / 2, 26, 26), s);
            let font = if chosen { Font::Strong } else { Font::Body };
            let name = text::fit(font, s.name(), r.w - 52);
            text::draw(c, font, r.x + 44, r.y + (r.h - bh) / 2, &name, ui::text());
        }
    }

    /// The label of a placed row and the line under it (or the note about
    /// the last change there).
    fn draw_label(&self, c: &mut Canvas, p: &Placed) {
        let (bh, _) = text::metrics(Font::Body);
        let fg = if p.row.ctl == Ctl::Text { ui::text_dim() } else { ui::text() };
        let label = text::fit(Font::Body, &p.row.label, p.text_w);
        text::draw(c, Font::Body, p.text_x, p.text_y, &label, fg);
        let (desc, color) = if !self.note.0.is_empty() && self.note.0 == p.row.label {
            (self.note.1, ui::ORANGE)
        } else {
            (p.row.desc.as_str(), ui::text_dim())
        };
        if !desc.is_empty() {
            let desc = text::fit(Font::Small, desc, p.text_w);
            text::draw(c, Font::Small, p.text_x, p.text_y + bh + 2, &desc, color);
        }
    }

    /// The control of placed row `i`.
    fn draw_ctl(&mut self, c: &mut Canvas, i: usize, p: &Placed) {
        let r = p.ctl_r;
        let hot = |k: usize| self.hot == Some(Target::Ctl(i, k));
        match &p.row.ctl {
            Ctl::Info(s) => {
                let s = text::fit(Font::Body, s, r.w + 2);
                text::draw_right(c, Font::Body, r.right(), r.y, &s, ui::text_dim());
            }
            Ctl::Text => {}
            Ctl::Name => {
                let (f, b) = field_and_button(r, "Salva");
                ui::text_field(c, f, &self.name, "Nome", self.editing);
                ui::button(c, b, "Salva", true, hot(1));
            }
            Ctl::Password => {
                let has = settings::has_password();
                let dots: String = self.password.chars().map(|_| '•').collect();
                let (f, b) = field_and_button(r, "Imposta");
                ui::text_field(c, f, &dots, if has { "Nuova password" } else { "Nessuna" }, self.editing_password);
                let label = if self.editing_password || !has { "Imposta" } else { "Togli" };
                ui::button(c, b, label, self.editing_password, hot(1));
            }
            Ctl::Setup => ui::button(c, r, "Ripeti", false, hot(0)),
            Ctl::Test => ui::button(c, r, "Prova", false, hot(0)),
            Ctl::Forget(_) => ui::button(c, r, "Chiedi di nuovo", false, hot(0)),
            Ctl::Chime => ui::toggle(c, r, settings::get().startup_sound),
            Ctl::Perm(a, j) => {
                let on = self.grants.get(*a).and_then(|g| g.perms.get(*j)).is_some_and(|(_, allow)| *allow);
                ui::toggle(c, r, on);
            }
            Ctl::Volume => {
                let v = crate::dev::audio::volume();
                let glyph = if v == 0 { &GLYPH_MUTE } else { &GLYPH_VOLUME };
                ui::soft_pixels_in(c, glyph, Rect::new(r.x, r.y, 18, r.h), 3200, ui::text_dim());
                ui::slider(c, volume_bar(r), v * 10);
            }
            Ctl::Scale | Ctl::Layout | Ctl::LockAfter => {
                let (labels, sel) = Self::choices(&p.row.ctl);
                ui::segmented(c, r, &labels, sel);
            }
            Ctl::Go(..) => {
                let (hh, _) = text::metrics(Font::Heading);
                text::draw(c, Font::Heading, r.x, r.y + (r.h - hh) / 2 - 1, "›", ui::text_dim());
            }
            Ctl::Themes => {
                let (tw, th) = theme_size(r.w);
                let wallpaper = settings::get().wallpaper;
                let stale = match &self.backdrop {
                    Some((n, b)) => *n != wallpaper || b.w != tw || b.h != th || b.scale() != c.scale(),
                    None => true,
                };
                if stale {
                    let pic = res::wallpaper_thumb(&wallpaper, tw, th);
                    self.backdrop = Some((wallpaper, pic));
                }
                let backdrop = self.backdrop.as_ref().map(|(_, b)| b);
                for (k, part) in self.parts(p).iter().enumerate() {
                    let pic = Rect::new(part.x, part.y, tw, th);
                    let (theme, label) = THEMES[k];
                    let pal = if theme == ui::Theme::Light { &ui::LIGHT } else { &ui::DARK };
                    draw_theme(c, pic, pal, backdrop);
                    let chosen = theme == ui::theme();
                    if chosen {
                        c.stroke_round(pic.grow(4), 16, 2, ui::ORANGE);
                    } else {
                        c.stroke_round(pic, 12, 1, ui::line());
                    }
                    let font = if chosen { Font::Strong } else { Font::Body };
                    let color = if chosen { ui::text() } else { ui::text_dim() };
                    text::draw_centered(c, font, pic.x + tw / 2, pic.bottom() + 10, label, color);
                }
            }
            Ctl::Wallpapers => {
                let (_, tw, th) = thumb_grid(r.w);
                if self.thumbs.first().is_none_or(|t| t.w != tw || t.h != th || t.scale() != c.scale()) {
                    // A new size, or the interface changed size: sharper (or
                    // smaller) previews.
                    self.thumbs = res::WALLPAPERS.iter().map(|(n, _)| res::wallpaper_thumb(n, tw, th)).collect();
                }
                let current = settings::get().wallpaper;
                for (k, t) in self.parts(p).iter().enumerate() {
                    let (name, label) = res::WALLPAPERS[k];
                    if name == current {
                        c.stroke_round(t.grow(4), 15, 2, ui::ORANGE);
                    }
                    c.blit_round_from(&self.thumbs[k], 0, 0, *t, 11, 0);
                    let color = if name == current { ui::text() } else { ui::text_dim() };
                    text::draw_centered(c, Font::Small, t.x + t.w / 2, t.bottom() + 6, label, color);
                }
            }
            Ctl::Modes => {
                let (w, h) = crate::console::pixel_size().unwrap_or((0, 0));
                let current = DisplayMode { width: w as u32, height: h as u32 };
                for (k, b) in self.parts(p).iter().enumerate() {
                    let m = self.modes[k];
                    ui::button(c, *b, &format!("{}×{}", m.width, m.height), m == current, hot(k));
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Doing

    /// Shows section `s`.
    fn open(&mut self, s: Section) {
        self.section = s;
        self.query.clear();
        self.searching = false;
        self.pick = 0;
        self.scroll = 0;
        self.hot = None;
        if s == Section::Privacy {
            self.grants = crate::sandbox::all();
        }
    }

    /// Shows the row a search result stands for, lit for a moment.
    fn go(&mut self, s: Section, label: &str) {
        self.open(s);
        self.flash = Some((String::from(label), crate::time::uptime_ms() + FLASH_MS));
        self.reveal = true;
    }

    /// The search result the keyboard is on: its section and label.
    fn picked(&self) -> Option<(Section, String)> {
        self.results().into_iter().flat_map(|c| c.rows).nth(self.pick).and_then(|r| match r.ctl {
            Ctl::Go(s, label) => Some((s, label)),
            _ => None,
        })
    }

    fn set_volume(v: i32) {
        crate::gui::change_volume(v.clamp(0, 100) as u32);
    }

    fn save_password(&mut self) {
        let removing = self.password.is_empty();
        if removing && !settings::has_password() {
            self.editing_password = true; // nothing to remove: type one
            return;
        }
        settings::set_password(&self.password);
        self.password.clear();
        self.editing_password = false;
        self.note = (L_PASSWORD, if removing { "Password tolta" } else { "Password impostata" });
    }

    fn save_name(&mut self) {
        let name = String::from(self.name.trim());
        settings::update(|s| s.name = name);
        self.editing = false;
        self.note = (L_NAME, "Nome salvato");
    }

    /// A click on part `k` of the control of placed row `p`, at `ax`.
    fn act(&mut self, p: &Placed, k: usize, ax: i32) -> Action {
        match &p.row.ctl {
            Ctl::Name => {
                if k == 0 {
                    self.editing = true;
                } else {
                    self.save_name();
                }
            }
            Ctl::Password => {
                if k == 0 {
                    self.editing_password = true;
                } else {
                    // "Togli" with an empty field removes the password.
                    self.save_password();
                }
            }
            Ctl::Setup => {
                settings::update(|s| s.setup_done = false);
                self.note = (L_SETUP, "Al prossimo avvio");
            }
            Ctl::Themes => {
                settings::update(|s| s.theme = THEMES[k].0);
                // Every window and panel changes colour.
                return Action::RedrawAll;
            }
            Ctl::Wallpapers => {
                settings::update(|s| s.wallpaper = String::from(res::WALLPAPERS[k].0));
                return Action::Wallpaper;
            }
            Ctl::Modes => {
                if crate::gui::change_resolution(self.modes[k]).is_err() {
                    self.note = (L_MODES, "Risoluzione non disponibile");
                }
            }
            Ctl::Scale => {
                if crate::gui::change_scale(SIZES[k].0).is_err() {
                    self.note = (L_SCALE, "Dimensione non disponibile");
                }
            }
            Ctl::Volume => {
                self.sliding = true;
                Self::set_volume((ui::slider_value(volume_bar(p.ctl_r), ax) / 10) as i32);
            }
            Ctl::Test => {
                let _ = crate::dev::audio::play(crate::dev::audio::tone(660, 300));
                return Action::None;
            }
            Ctl::Chime => settings::update(|s| s.startup_sound = !s.startup_sound),
            Ctl::Layout => {
                settings::update(|s| s.layout = LAYOUTS[k].0);
                self.note = (L_LAYOUT, "Tastiera cambiata");
            }
            Ctl::LockAfter => settings::update(|s| s.lock_after = LOCK_AFTER[k].0),
            Ctl::Forget(i) => {
                if let Some(g) = self.grants.get(*i) {
                    crate::sandbox::forget(&g.id);
                }
                self.grants = crate::sandbox::all();
            }
            Ctl::Perm(i, j) => {
                if let Some(g) = self.grants.get(*i) {
                    if let Some((perm, allow)) = g.perms.get(*j) {
                        crate::sandbox::set(&g.id, &g.name, perm, Some(!allow));
                    }
                }
                self.grants = crate::sandbox::all();
            }
            Ctl::Info(_) | Ctl::Text | Ctl::Go(..) => {}
        }
        Action::Redraw
    }
}

impl App for Prefs {
    fn title(&self) -> String {
        String::from("Impostazioni")
    }

    fn icon(&self) -> &str {
        "settings"
    }

    fn kind(&self) -> &'static str {
        "settings"
    }

    fn size(&self) -> (i32, i32) {
        (900, 580)
    }

    fn resizable(&self) -> bool {
        true
    }

    fn sidebar(&self) -> i32 {
        SIDEBAR
    }

    fn draw(&mut self, c: &mut Canvas, area: Rect, _blur: &Canvas, _focused: bool) {
        self.draw_sidebar(c, area);
        let main = Self::main(area);
        let mut page = self.page(area);
        self.view_h = main.h;
        self.max_scroll = (page.height - main.h).max(0);
        let mut scroll = self.scroll.clamp(0, self.max_scroll);
        if self.reveal {
            // Scroll to the row a search led to, when it is out of sight.
            self.reveal = false;
            let label = self.flash.as_ref().map(|(l, _)| l.as_str()).unwrap_or("");
            if let Some(p) = page.rows.iter().find(|p| p.row.label == label) {
                if p.r.bottom() > main.bottom() - 16 {
                    scroll = (p.r.y - main.y - 90).clamp(0, self.max_scroll);
                }
            }
        }
        if scroll != self.scroll {
            self.scroll = scroll;
            page = self.page(area);
        }
        let saved = c.clip();
        c.set_clip(saved.intersect(&main.grow(-1)));
        let (bh, _) = text::metrics(Font::Body);
        let title = text::fit(Font::Display, &page.title, page.w);
        text::draw(c, Font::Display, page.x + 2, page.title_y, &title, ui::text());
        for (k, line) in page.intro.iter().enumerate() {
            text::draw(c, Font::Body, page.x + 2, page.intro_y + k as i32 * (bh + 3), line, ui::text_dim());
        }
        if page.rows.is_empty() && !self.query.trim().is_empty() {
            text::draw(c, Font::Body, page.x + 2, page.intro_y + 6, "Nessuna impostazione trovata.", ui::text_dim());
        }
        for (r, head) in &page.cards {
            if let Some(s) = head {
                let tile = Rect::new(r.x + 2, r.y - HEAD_H + 2, 20, 20);
                draw_tile(c, tile, *s);
                text::draw(c, Font::Strong, tile.right() + 10, tile.y + (20 - bh) / 2, s.name(), ui::text());
            }
            ui::well_pane(c, *r, CARD_RAD);
            c.fill_round(*r, CARD_RAD, ui::pane());
        }
        let searching = !self.query.trim().is_empty();
        for (i, p) in page.rows.iter().enumerate() {
            if p.r.bottom() < main.y || p.r.y > main.bottom() {
                continue;
            }
            let lit = if searching { i == self.pick } else { self.flash.as_ref().is_some_and(|(l, _)| *l == p.row.label) };
            if lit {
                c.fill_round(p.r.grow(-4), 12, ui::sel());
            } else if self.hot == Some(Target::Result(i)) {
                c.fill_round(p.r.grow(-4), 12, ui::hover());
            }
            if !p.first {
                c.fill(Rect::new(p.r.x + INSET, p.r.y, p.r.w - 2 * INSET, 1), ui::line());
            }
            self.draw_label(c, p);
            self.draw_ctl(c, i, p);
        }
        if self.max_scroll > 0 {
            // Where the view is in the page.
            let track = main.h - 24;
            let h = (track * main.h / page.height.max(1)).max(28);
            let y = main.y + 12 + (track - h) * self.scroll / self.max_scroll;
            c.fill_round(Rect::new(main.right() - 9, y, 4, h), 2, ui::text_faint());
        }
        c.set_clip(saved);
    }

    fn press(&mut self, x: i32, y: i32, _double: bool, area: Rect) -> Action {
        let (ax, ay) = (area.x + x, area.y + y);
        let page = self.page(area);
        let target = self.hit(&page, area, ax, ay);
        self.note = ("", "");
        self.flash = None;
        // A click anywhere else leaves the fields.
        self.editing = false;
        self.editing_password = false;
        self.searching = false;
        match target {
            None => Action::Redraw,
            Some(Target::Search) => {
                self.searching = true;
                Action::Redraw
            }
            Some(Target::Side(s)) => {
                self.open(s);
                Action::Redraw
            }
            Some(Target::Result(i)) => {
                if let Ctl::Go(s, label) = &page.rows[i].row.ctl {
                    self.go(*s, label);
                }
                Action::Redraw
            }
            Some(Target::Ctl(i, k)) => self.act(&page.rows[i], k, ax),
        }
    }

    fn drag(&mut self, x: i32, _y: i32, area: Rect) -> bool {
        if self.sliding {
            let page = self.page(area);
            if let Some(p) = page.rows.iter().find(|p| p.row.ctl == Ctl::Volume) {
                Self::set_volume((ui::slider_value(volume_bar(p.ctl_r), area.x + x) / 10) as i32);
            }
        }
        self.sliding
    }

    fn release(&mut self, _area: Rect) -> Action {
        self.sliding = false;
        Action::None
    }

    fn hover(&mut self, at: Option<(i32, i32)>, area: Rect) -> bool {
        let t = at.and_then(|(x, y)| {
            let page = self.page(area);
            let t = self.hit(&page, area, area.x + x, area.y + y)?;
            // Only what lights up under the pointer.
            let lights = match t {
                Target::Side(_) | Target::Result(_) => true,
                Target::Ctl(i, k) => match page.rows[i].row.ctl {
                    Ctl::Name | Ctl::Password => k == 1,
                    Ctl::Setup | Ctl::Test | Ctl::Forget(_) | Ctl::Modes => true,
                    _ => false,
                },
                Target::Search => false,
            };
            lights.then_some(t)
        });
        let changed = t != self.hot;
        self.hot = t;
        changed
    }

    fn scroll(&mut self, lines: i32) -> bool {
        let old = self.scroll;
        self.scroll = (self.scroll + lines * 40).clamp(0, self.max_scroll);
        old != self.scroll
    }

    fn tick(&mut self) -> bool {
        if self.flash.as_ref().is_some_and(|(_, until)| crate::time::uptime_ms() >= *until) {
            self.flash = None;
            return true;
        }
        false
    }

    fn key(&mut self, ev: KeyEvent) -> Action {
        if self.editing_password {
            match ev.key {
                Key::Enter => self.save_password(),
                Key::Escape => {
                    self.password.clear();
                    self.editing_password = false;
                }
                _ => {
                    ui::edit_text(&mut self.password, &ev, 64);
                }
            }
            return Action::Redraw;
        }
        if self.editing {
            match ev.key {
                Key::Enter => self.save_name(),
                Key::Escape => {
                    self.name = settings::get().name;
                    self.editing = false;
                }
                _ => {
                    ui::edit_text(&mut self.name, &ev, 32);
                }
            }
            return Action::Redraw;
        }
        let searching = !self.query.trim().is_empty();
        match ev.key {
            Key::Escape if self.searching || !self.query.is_empty() => {
                self.query.clear();
                self.searching = false;
                self.pick = 0;
                self.scroll = 0;
            }
            Key::Up | Key::Down if searching => {
                let n = self.results().iter().map(|c| c.rows.len()).sum::<usize>();
                self.pick = if ev.key == Key::Up { self.pick.saturating_sub(1) } else { (self.pick + 1).min(n.saturating_sub(1)) };
            }
            Key::Up | Key::Down => {
                let i = SECTIONS.iter().position(|s| *s == self.section).unwrap_or(0);
                let j = if ev.key == Key::Up { i.saturating_sub(1) } else { (i + 1).min(SECTIONS.len() - 1) };
                if i == j {
                    return Action::None;
                }
                self.open(SECTIONS[j]);
            }
            Key::Enter if searching => match self.picked() {
                Some((s, label)) => self.go(s, &label),
                None => return Action::None,
            },
            Key::PageUp | Key::PageDown => {
                let step = (self.view_h - 60).max(40);
                let d = if ev.key == Key::PageUp { -step } else { step };
                let old = self.scroll;
                self.scroll = (self.scroll + d).clamp(0, self.max_scroll);
                if old == self.scroll {
                    return Action::None;
                }
            }
            Key::Backspace if self.searching => {
                self.query.pop();
                self.pick = 0;
                self.scroll = 0;
            }
            // Typing with nothing else to type into searches.
            Key::Char(ch) if !ev.ctrl && !ev.alt && !ch.is_control() && self.query.chars().count() < 40 => {
                self.searching = true;
                self.query.push(ch);
                self.pick = 0;
                self.scroll = 0;
            }
            _ => return Action::None,
        }
        Action::Redraw
    }
}

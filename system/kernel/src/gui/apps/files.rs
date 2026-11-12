//! File: the file manager. Places on the left, the folder on the right,
//! with tabs above it when more than one folder is open.

use crate::fs::{self, Path};
use crate::gui::dnd;
use crate::gui::gfx::{Canvas, Rect};
use crate::gui::res;
use crate::gui::text::{self, Font};
use crate::gui::ui;
use crate::gui::wm::{Action, App};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const SIDEBAR: i32 = 180;
const TILE_W: i32 = 104;
const TILE_H: i32 = 104;
const HEADER: i32 = 52;
/// The tab bar's height, when there are tabs.
const TABS_H: i32 = 38;

const PLACES: [(&str, &str); 9] = [
    ("Home", "sys:/home"),
    ("Scrivania", "sys:/home/Scrivania"),
    ("Documenti", "sys:/home/Documenti"),
    ("Scaricati", "sys:/home/Scaricati"),
    ("Immagini", "sys:/home/Immagini"),
    ("Musica", "sys:/home/Musica"),
    ("Video", "sys:/home/Video"),
    ("Cestino", "sys:/home/Cestino"),
    ("Sistema", "sys:/"),
];

struct Item {
    name: String,
    dir: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Do {
    Open,
    Rename,
    ToBin,
    Delete,
    NewFolder,
    EmptyBin,
    OpenTab,
}

/// What a click on a selected item does once the button goes up without a
/// drag (with a drag, the whole selection goes).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Up {
    Nothing,
    /// That item alone stays selected.
    Only,
    /// Ctrl+click: that item leaves the selection.
    Deselect,
}

/// Where files carried over the window would land.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    /// A folder in the grid.
    Item(usize),
    /// The folder shown.
    Here,
    Place(usize),
    Tab(usize),
}

pub struct Files {
    trash: bool,
    /// The folder of each tab; the one shown is `dir`.
    tabs: Vec<Path>,
    tab: usize,
    dir: Path,
    items: Vec<Item>,
    /// The items selected, and the one the keyboard is on (the last clicked).
    selection: Vec<usize>,
    selected: Option<usize>,
    /// A press on an item: where (in the content), which, and what the
    /// selection becomes if the button goes up without a drag.
    press_at: Option<(i32, i32, usize, Up)>,
    /// Files dragged out of the window, for the desktop to take.
    out: Option<Vec<String>>,
    /// Where files carried over the window would land now.
    drop_at: Option<Target>,
    scroll: i32,
    /// A menu from the right button: where, and its choices.
    menu: Option<(Rect, Vec<(&'static str, Do)>)>,
    /// An item being renamed, and the name typed so far.
    renaming: Option<(usize, String)>,
    hover_menu: Option<usize>,
    /// The grid's rectangle at the last drawing.
    grid_at: Rect,
}

const MENU_ROW: i32 = 32;
/// A place that takes the files carried over it.
const DROP_FILL: u32 = 0x400A_84FF;
const DROP_EDGE: u32 = 0xFF0A_84FF;

/// The icon of a file or folder.
pub fn icon_for(name: &str, dir: bool) -> &'static str {
    if dir {
        return match name {
            "Documenti" => "folder-documents",
            "Scaricati" => "folder-downloads",
            "Immagini" => "folder-pictures",
            "Musica" => "folder-music",
            "Video" => "folder-videos",
            "home" => "folder-home",
            "Scrivania" => "folder-desktop",
            "Cestino" => "trash",
            _ => "folder",
        };
    }
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "pdf" => "file-pdf",
        "jpg" | "jpeg" | "png" | "qoi" | "bmp" | "gif" => "file-image",
        "txt" | "md" | "odt" | "doc" | "docx" | "rtf" => "file-text",
        "zip" | "tar" | "gz" | "xz" | "jdp" | "7z" => "file-archive",
        "py" | "rs" | "c" | "h" | "js" | "sh" | "toml" | "json" | "html" | "css" => "file-code",
        "mp3" | "wav" | "ogg" | "flac" => "file-audio",
        "jt" | "jpk" => "file-script",
        "exe" => "file-exe",
        "app" => "window",
        _ => "file",
    }
}

impl Files {
    pub fn new(path: &str) -> Files {
        let dir = fs::abs(path).unwrap_or_else(|_| Path::root("sys"));
        let mut f = Files {
            trash: false,
            tabs: alloc::vec![dir.clone()],
            tab: 0,
            dir,
            items: Vec::new(),
            selection: Vec::new(),
            selected: None,
            press_at: None,
            out: None,
            drop_at: None,
            scroll: 0,
            menu: None,
            renaming: None,
            hover_menu: None,
            grid_at: Rect::EMPTY,
        };
        f.reload();
        f
    }

    /// The bin: a Files window of its own kind.
    pub fn trash() -> Files {
        let mut f = Files::new("sys:/home/Cestino");
        f.trash = true;
        f
    }

    fn reload(&mut self) {
        self.items = fs::list_dir(&self.dir)
            .map(|list| list.into_iter().filter(|(_, st)| !st.hidden).map(|(e, st)| Item { name: e.name, dir: st.kind == justfs::Kind::Dir }).collect())
            .unwrap_or_default();
        self.selection.clear();
        self.selected = None;
        self.scroll = 0;
    }

    /// Only item `i` selected.
    fn select_only(&mut self, i: usize) {
        self.selection.clear();
        self.selection.push(i);
        self.selected = Some(i);
    }

    fn select_none(&mut self) {
        self.selection.clear();
        self.selected = None;
    }

    /// The names of the selected items, in the folder's order.
    fn selected_names(&self) -> Vec<String> {
        self.items.iter().enumerate().filter(|(i, _)| self.selection.contains(i)).map(|(_, it)| it.name.clone()).collect()
    }

    /// The selected items as absolute paths.
    fn selected_paths(&self) -> Vec<String> {
        self.selected_names().iter().map(|n| format!("{}", self.dir.join(n))).collect()
    }

    /// The item under a point of the window (absolute coordinates).
    fn item_at(&self, ax: i32, ay: i32, area: Rect) -> Option<usize> {
        let (grid, cols) = self.grid(area);
        if !grid.contains(ax, ay) {
            return None;
        }
        let col = (ax - grid.x) / TILE_W;
        let row = (ay - grid.y + self.scroll) / TILE_H;
        let i = (row * cols + col) as usize;
        (col < cols && i < self.items.len()).then_some(i)
    }

    fn place_rect(i: usize, area: Rect) -> Rect {
        Rect::new(area.x + 10, area.y + 40 + i as i32 * 36, SIDEBAR - 12, 32)
    }

    /// Where files carried to a point of the window would land.
    fn target_at(&self, ax: i32, ay: i32, area: Rect, paths: &[String]) -> Option<Target> {
        if self.trash {
            return (self.content(area).contains(ax, ay) || area.contains(ax, ay)).then_some(Target::Here);
        }
        if let Some(i) = (0..PLACES.len()).find(|&i| Self::place_rect(i, area).contains(ax, ay)) {
            return Some(Target::Place(i));
        }
        if self.tabs.len() > 1 {
            let (rects, _) = self.tab_rects(area);
            if let Some(i) = rects.iter().position(|r| r.contains(ax, ay)) {
                return (i != self.tab).then_some(Target::Tab(i));
            }
        }
        if let Some(i) = self.item_at(ax, ay, area) {
            let it = &self.items[i];
            let path = format!("{}", self.dir.join(&it.name));
            if it.dir && !paths.contains(&path) {
                return Some(Target::Item(i));
            }
        }
        if !self.content(area).contains(ax, ay) {
            return None;
        }
        // Here: unless everything comes from here (moving it would do nothing).
        let here = format!("{}", self.dir);
        let from_here = paths.iter().all(|p| fs::abs(p).is_ok_and(|p| format!("{}", p.parent()) == here));
        (!from_here || dnd_copy_held()).then_some(Target::Here)
    }

    /// The folder a target stands for.
    fn target_dir(&self, t: Target) -> Option<Path> {
        match t {
            Target::Item(i) => self.items.get(i).map(|it| self.dir.join(&it.name)),
            Target::Here => Some(self.dir.clone()),
            Target::Place(i) => {
                let p = fs::abs(PLACES.get(i)?.1).ok()?;
                if !fs::exists(&p) {
                    let _ = fs::make_dir(&p);
                }
                Some(p)
            }
            Target::Tab(i) => self.tabs.get(i).cloned(),
        }
    }

    fn do_action(&mut self, act: Do) -> Action {
        let selected = self.selected.and_then(|i| self.items.get(i).map(|it| (i, it.name.clone())));
        match act {
            Do::Open => {
                if let Some((i, _)) = selected {
                    return self.open_item(i);
                }
            }
            Do::Rename => {
                if let Some((i, name)) = selected {
                    self.renaming = Some((i, name));
                }
            }
            Do::ToBin => {
                let paths = self.selected_paths();
                if !paths.is_empty() {
                    let r = dnd::to_bin(&paths);
                    self.reload();
                    if let Err(e) = r {
                        return Action::Notify(String::from("Cestino"), format!("Non si può spostare nel Cestino: {e}"), "trash");
                    }
                }
            }
            Do::Delete => {
                let names = self.selected_names();
                for name in names {
                    if let Err(e) = fs::remove_all(&self.dir.join(&name)) {
                        self.reload();
                        return Action::Notify(name, format!("Non si può eliminare: {e}"), "trash");
                    }
                }
                self.reload();
            }
            Do::NewFolder => {
                let mut name = String::from("Nuova cartella");
                let mut n = 2;
                while fs::exists(&self.dir.join(&name)) {
                    name = format!("Nuova cartella {n}");
                    n += 1;
                }
                if let Err(e) = fs::make_dir(&self.dir.join(&name)) {
                    return Action::Notify(String::from("Nuova cartella"), format!("Non si può creare: {e}"), "folder");
                }
                self.reload();
                if let Some(i) = self.items.iter().position(|it| it.name == name) {
                    self.select_only(i);
                    self.renaming = Some((i, name));
                }
            }
            Do::EmptyBin => {
                for it in &self.items {
                    let p = self.dir.join(&it.name);
                    let _ = if it.dir { fs::remove_all(&p) } else { fs::remove(&p) };
                }
                self.reload();
            }
            Do::OpenTab => {
                if let Some((_, name)) = selected {
                    self.new_tab(self.dir.join(&name));
                }
            }
        }
        Action::Redraw
    }

    /// Opens `path` in a new tab after the others, and shows it.
    fn new_tab(&mut self, path: Path) {
        if !fs::is_dir(&path) {
            return;
        }
        self.renaming = None;
        self.tabs.push(path.clone());
        self.tab = self.tabs.len() - 1;
        self.dir = path;
        self.reload();
    }

    fn switch_tab(&mut self, i: usize) {
        if i == self.tab || i >= self.tabs.len() {
            return;
        }
        self.renaming = None;
        self.tab = i;
        self.dir = self.tabs[i].clone();
        self.reload();
    }

    fn close_tab(&mut self, i: usize) {
        if self.tabs.len() <= 1 || i >= self.tabs.len() {
            return;
        }
        self.tabs.remove(i);
        if i < self.tab {
            self.tab -= 1;
        } else if i == self.tab {
            self.tab = i.min(self.tabs.len() - 1);
            self.dir = self.tabs[self.tab].clone();
            self.renaming = None;
            self.reload();
        }
    }

    fn tabs_h(&self) -> i32 {
        if self.tabs.len() > 1 {
            TABS_H
        } else {
            0
        }
    }

    /// The tabs, and the button that adds one, above the folder.
    fn tab_rects(&self, area: Rect) -> (Vec<Rect>, Rect) {
        let bar = Rect::new(area.x + SIDEBAR + 14, area.y + 7, area.w - SIDEBAR - 22, TABS_H - 8);
        let plus = Rect::new(bar.right() - 26, bar.y + (bar.h - 26) / 2, 26, 26);
        let n = self.tabs.len() as i32;
        let w = ((bar.w - 34 - (n - 1) * 4) / n.max(1)).min(200);
        let rects = (0..n).map(|i| Rect::new(bar.x + i * (w + 4), bar.y, w, bar.h)).collect();
        (rects, plus)
    }

    /// The button that opens a new tab, in the folder's header.
    fn new_tab_btn(&self, area: Rect) -> Rect {
        let c = self.content(area);
        let count = format!("{} elementi", self.items.len());
        Rect::new(c.right() - text::width(Font::Small, &count) - 16 - 40, c.y + 12, 28, 28)
    }

    fn finish_rename(&mut self) {
        let Some((i, name)) = self.renaming.take() else { return };
        let name = String::from(name.trim());
        let Some(old) = self.items.get(i).map(|it| it.name.clone()) else { return };
        if name.is_empty() || name == old || name.contains('/') || name.contains(':') {
            return;
        }
        if fs::rename(&self.dir.join(&old), &self.dir.join(&name)).is_ok() {
            self.reload();
            if let Some(i) = self.items.iter().position(|it| it.name == name) {
                self.select_only(i);
            }
        }
    }

    fn menu_rect(x: i32, y: i32, n: usize, area: Rect) -> Rect {
        let (w, h) = (210, n as i32 * MENU_ROW + 10);
        Rect::new(x.min(area.right() - w - 6), y.min(area.bottom() - h - 6), w, h)
    }

    fn go(&mut self, path: Path) {
        if fs::is_dir(&path) {
            self.tabs[self.tab] = path.clone();
            self.dir = path;
            self.reload();
        }
    }

    /// The folder's part of the window, under the tabs.
    fn content(&self, area: Rect) -> Rect {
        let t = self.tabs_h();
        Rect::new(area.x + SIDEBAR + 6, area.y + t, area.w - SIDEBAR - 6, area.h - t)
    }

    fn grid(&self, area: Rect) -> (Rect, i32) {
        let c = self.content(area);
        let g = Rect::new(c.x + 12, c.y + HEADER, c.w - 24, c.h - HEADER - 8);
        let cols = (g.w / TILE_W).max(1);
        (g, cols)
    }

    fn open_item(&mut self, i: usize) -> Action {
        let item = &self.items[i];
        let path = self.dir.join(&item.name);
        if item.dir {
            self.go(path);
            return Action::Redraw;
        }
        Action::OpenFile(format!("{path}"))
    }

    fn title_of(&self) -> String {
        Self::name_of(&self.dir)
    }

    /// A folder's name as shown: the place's name for the places.
    fn name_of(dir: &Path) -> String {
        match PLACES.iter().find(|(_, p)| fs::abs(p).is_ok_and(|q| q == *dir)) {
            Some((n, _)) => String::from(*n),
            None => {
                let n = dir.name();
                if n.is_empty() {
                    format!("{}:", dir.volume)
                } else {
                    String::from(n)
                }
            }
        }
    }
}

/// The icon of a folder by the name shown for it (places included).
fn place_icon(name: &str) -> &'static str {
    match name {
        "Home" => "folder-home",
        "Sistema" => "computer",
        _ => icon_for(name, true),
    }
}

/// Is Ctrl or Alt (without Shift) held, so that a drop copies?
fn dnd_copy_held() -> bool {
    let (ctrl, alt, shift) = crate::dev::input::mods();
    (ctrl || alt) && !shift
}

/// The button that closes a tab, at its right end.
fn tab_close(tab: Rect) -> Rect {
    Rect::new(tab.right() - 24, tab.y + (tab.h - 18) / 2, 18, 18)
}

impl App for Files {
    fn title(&self) -> String {
        self.title_of()
    }

    fn icon(&self) -> &str {
        if self.trash {
            "trash"
        } else {
            "files"
        }
    }

    fn kind(&self) -> &'static str {
        if self.trash {
            "trash"
        } else {
            "files"
        }
    }

    fn resizable(&self) -> bool {
        true
    }

    fn sidebar(&self) -> i32 {
        SIDEBAR - 2
    }

    fn size(&self) -> (i32, i32) {
        (SIDEBAR + 18 + 24 + TILE_W * 4 + 8, 430)
    }

    fn draw(&mut self, c: &mut Canvas, area: Rect, _blur: &Canvas, _focused: bool) {
        // Places.
        crate::gui::ui::caps(c, area.x + 22, area.y + 18, "Risorse", ui::text_faint());
        for (i, (name, path)) in PLACES.iter().enumerate() {
            let r = Self::place_rect(i, area);
            if fs::abs(path).is_ok_and(|p| p == self.dir) {
                c.fill_round(r, 10, crate::gui::ui::sel());
            }
            if self.drop_at == Some(Target::Place(i)) {
                c.fill_round(r, 10, DROP_FILL);
                c.stroke_round(r, 10, 2, DROP_EDGE);
            }
            res::draw_icon(c, place_icon(name), 20, r.x + 10, r.y + 6);
            text::draw(c, Font::Body, r.x + 40, r.y + 7, name, ui::text());
        }
        // The tabs.
        let (sh, _) = text::metrics(Font::Small);
        let (hh, _) = text::metrics(Font::Heading);
        if self.tabs.len() > 1 {
            let (rects, plus) = self.tab_rects(area);
            for (i, r) in rects.iter().enumerate() {
                let on = i == self.tab;
                c.fill_round(*r, 10, if on { crate::gui::ui::sel() } else { 0x14FF_FFFF });
                if self.drop_at == Some(Target::Tab(i)) {
                    c.fill_round(*r, 10, DROP_FILL);
                    c.stroke_round(*r, 10, 2, DROP_EDGE);
                }
                let name = Self::name_of(&self.tabs[i]);
                res::draw_icon(c, place_icon(&name), 16, r.x + 9, r.y + (r.h - 16) / 2);
                let name = text::fit(Font::Small, &name, r.w - 58);
                text::draw(c, Font::Small, r.x + 31, r.y + (r.h - sh) / 2, &name, if on { ui::text() } else { ui::text_dim() });
                let x = tab_close(*r);
                text::draw_centered(c, Font::Small, x.x + x.w / 2, x.y + (x.h - sh) / 2, "×", ui::text_dim());
            }
            c.fill_circle(plus.x + 13, plus.y + 13, 13, 0x1FFF_FFFF);
            text::draw_centered(c, Font::Heading, plus.x + 13, plus.y + (26 - hh) / 2, "+", ui::text());
        }
        // The folder.
        let content = self.content(area);
        let back = Rect::new(content.x + 12, content.y + 12, 28, 28);
        c.fill_circle(back.x + 14, back.y + 14, 14, 0x26FF_FFFF);
        res::draw_icon(c, "g-back", 14, back.x + 7, back.y + 7);
        let nt = self.new_tab_btn(area);
        let title = text::fit(Font::Heading, &self.title_of(), nt.x - content.x - 64);
        text::draw(c, Font::Heading, content.x + 52, content.y + 14, &title, ui::text());
        let count = format!("{} elementi", self.items.len());
        let cw = text::width(Font::Small, &count);
        text::draw(c, Font::Small, content.right() - cw - 16, content.y + 20, &count, ui::text_dim());
        if !self.trash {
            c.fill_circle(nt.x + 14, nt.y + 14, 14, 0x26FF_FFFF);
            text::draw_centered(c, Font::Heading, nt.x + 14, nt.y + (28 - hh) / 2, "+", ui::text());
        }
        let (grid, cols) = self.grid(area);
        let saved = c.clip();
        c.set_clip(saved.intersect(&grid));
        if self.items.is_empty() {
            text::draw_centered(c, Font::Body, grid.x + grid.w / 2, grid.y + 60, "La cartella è vuota", ui::text_dim());
        }
        for (i, item) in self.items.iter().enumerate() {
            let (col, row) = (i as i32 % cols, i as i32 / cols);
            let t = Rect::new(grid.x + col * TILE_W, grid.y + row * TILE_H - self.scroll, TILE_W, TILE_H);
            if t.bottom() < grid.y || t.y > grid.bottom() {
                continue;
            }
            if self.selection.contains(&i) {
                c.fill_round(t.grow(-4), 14, 0x33FF_FFFF);
            }
            if self.drop_at == Some(Target::Item(i)) {
                c.fill_round(t.grow(-4), 14, DROP_FILL);
                c.stroke_round(t.grow(-4), 14, 2, DROP_EDGE);
            }
            res::draw_icon(c, icon_for(&item.name, item.dir), 52, t.x + (TILE_W - 52) / 2, t.y + 10);
            match &self.renaming {
                Some((k, name)) if *k == i => {
                    let w = (text::width(Font::Small, name) + 14).clamp(40, TILE_W + 20);
                    let f = Rect::new(t.x + TILE_W / 2 - w / 2, t.y + 68, w, 22);
                    c.fill_round(f, 6, 0xFFFF_FFFF);
                    let tw = text::draw(c, Font::Small, f.x + 7, f.y + 2, &text::fit(Font::Small, name, f.w - 12), 0xFF1C_1C1E);
                    c.fill(Rect::new(f.x + 8 + tw, f.y + 4, 1, 14), crate::gui::ui::ACCENT);
                }
                _ => {
                    let label = text::fit(Font::Small, &item.name, TILE_W - 10);
                    text::draw_centered(c, Font::Small, t.x + TILE_W / 2, t.y + 70, &label, ui::text());
                }
            }
        }
        c.set_clip(saved);
        if self.drop_at == Some(Target::Here) {
            // The whole folder takes them.
            c.stroke_round(content.grow(-4), 14, 2, DROP_EDGE);
        }
        self.grid_at = grid;
        if let Some((r, items)) = &self.menu {
            c.shadow(*r, 12, 12, 80);
            c.fill_round(*r, 12, 0xF52A_2234);
            c.stroke_round(*r, 12, 1, crate::gui::ui::line());
            for (k, (label, act)) in items.iter().enumerate() {
                let row = Rect::new(r.x + 5, r.y + 5 + k as i32 * MENU_ROW, r.w - 10, MENU_ROW - 2);
                if self.hover_menu == Some(k) {
                    c.fill_round(row, 7, crate::gui::ui::sel());
                }
                let color = if matches!(act, Do::Delete | Do::EmptyBin) && self.hover_menu != Some(k) { 0xFFFF_6961 } else { ui::text() };
                text::draw(c, Font::Body, row.x + 12, row.y + 6, label, color);
            }
        }
    }

    fn menu(&mut self, x: i32, y: i32, area: Rect) -> Action {
        self.finish_rename();
        let (ax, ay) = (area.x + x, area.y + y);
        let (grid, _) = self.grid(area);
        if !grid.contains(ax, ay) {
            return Action::None;
        }
        let items: Vec<(&'static str, Do)> = if let Some(i) = self.item_at(ax, ay, area) {
            if !self.selection.contains(&i) {
                self.select_only(i);
            }
            self.selected = Some(i);
            if self.selection.len() > 1 {
                if self.trash {
                    alloc::vec![("Elimina definitivamente", Do::Delete)]
                } else {
                    alloc::vec![("Sposta nel Cestino", Do::ToBin)]
                }
            } else if self.trash {
                alloc::vec![("Elimina definitivamente", Do::Delete)]
            } else if self.items[i].dir {
                alloc::vec![("Apri", Do::Open), ("Apri in una nuova scheda", Do::OpenTab), ("Rinomina", Do::Rename), ("Sposta nel Cestino", Do::ToBin)]
            } else {
                alloc::vec![("Apri", Do::Open), ("Rinomina", Do::Rename), ("Sposta nel Cestino", Do::ToBin)]
            }
        } else if self.trash {
            if self.items.is_empty() {
                return Action::Redraw;
            }
            alloc::vec![("Svuota il Cestino", Do::EmptyBin)]
        } else {
            self.select_none();
            alloc::vec![("Nuova cartella", Do::NewFolder)]
        };
        self.menu = Some((Self::menu_rect(ax, ay, items.len(), area), items));
        self.hover_menu = None;
        Action::Redraw
    }

    fn hover(&mut self, at: Option<(i32, i32)>, area: Rect) -> bool {
        let Some((r, items)) = &self.menu else { return false };
        let h = at.and_then(|(x, y)| {
            let (ax, ay) = (area.x + x, area.y + y);
            (r.contains(ax, ay)).then(|| ((ay - r.y - 5) / MENU_ROW) as usize).filter(|&k| k < items.len())
        });
        let changed = h != self.hover_menu;
        self.hover_menu = h;
        changed
    }

    fn press(&mut self, x: i32, y: i32, double: bool, area: Rect) -> Action {
        let (ax, ay) = (area.x + x, area.y + y);
        if let Some((r, items)) = self.menu.take() {
            if r.contains(ax, ay) {
                let k = ((ay - r.y - 5) / MENU_ROW) as usize;
                if let Some(&(_, act)) = items.get(k) {
                    return self.do_action(act);
                }
            }
            return Action::Redraw;
        }
        self.finish_rename();
        if self.tabs.len() > 1 {
            let (rects, plus) = self.tab_rects(area);
            if let Some(i) = rects.iter().position(|r| tab_close(*r).grow(3).contains(ax, ay)) {
                self.close_tab(i);
                return Action::Redraw;
            }
            if let Some(i) = rects.iter().position(|r| r.contains(ax, ay)) {
                self.switch_tab(i);
                return Action::Redraw;
            }
            if plus.contains(ax, ay) {
                self.new_tab(self.dir.clone());
                return Action::Redraw;
            }
        }
        if !self.trash && self.new_tab_btn(area).contains(ax, ay) {
            self.new_tab(self.dir.clone());
            return Action::Redraw;
        }
        for (i, (_, path)) in PLACES.iter().enumerate() {
            if Self::place_rect(i, area).contains(ax, ay) {
                if let Ok(p) = fs::abs(path) {
                    if !fs::exists(&p) {
                        let _ = fs::make_dir(&p);
                    }
                    self.go(p);
                }
                return Action::Redraw;
            }
        }
        let content = self.content(area);
        let back = Rect::new(content.x + 12, content.y + 12, 28, 28);
        if back.contains(ax, ay) {
            let parent = Path::parse("..", &self.dir).unwrap_or_else(|_| self.dir.clone());
            self.go(parent);
            return Action::Redraw;
        }
        let (grid, _) = self.grid(area);
        if grid.contains(ax, ay) {
            let (ctrl, _, shift) = crate::dev::input::mods();
            match self.item_at(ax, ay, area) {
                Some(i) if double => {
                    self.select_only(i);
                    return self.open_item(i);
                }
                // Ctrl: one more, or (without a drag: Ctrl also copies what
                // is dragged) one less.
                Some(i) if ctrl => {
                    let had = self.selection.contains(&i);
                    if !had {
                        self.selection.push(i);
                    }
                    self.selected = Some(i);
                    self.press_at = Some((x, y, i, if had { Up::Deselect } else { Up::Nothing }));
                }
                // Shift: all from the last one clicked.
                Some(i) if shift && self.selected.is_some() => {
                    let from = self.selected.unwrap_or(i);
                    self.selection = (from.min(i)..=from.max(i)).collect();
                    self.press_at = Some((x, y, i, Up::Nothing));
                }
                Some(i) => {
                    // A press on a selection keeps it (to drag it all);
                    // without a drag it ends up the only one selected.
                    let keep = self.selection.contains(&i);
                    if !keep {
                        self.select_only(i);
                    }
                    self.selected = Some(i);
                    self.press_at = Some((x, y, i, if keep && self.selection.len() > 1 { Up::Only } else { Up::Nothing }));
                }
                None if ctrl || shift => {}
                None => self.select_none(),
            }
            return Action::Redraw;
        }
        Action::None
    }

    fn drag(&mut self, x: i32, y: i32, _area: Rect) -> bool {
        let Some((px, py, _, _)) = self.press_at else { return false };
        if (x - px).abs() > 6 || (y - py).abs() > 6 {
            self.press_at = None;
            let paths = self.selected_paths();
            if !paths.is_empty() {
                self.out = Some(paths);
            }
        }
        false
    }

    fn release(&mut self, _area: Rect) -> Action {
        match self.press_at.take() {
            Some((_, _, i, Up::Only)) => {
                self.select_only(i);
                Action::Redraw
            }
            Some((_, _, i, Up::Deselect)) => {
                self.selection.retain(|&k| k != i);
                self.selected = self.selection.last().copied();
                Action::Redraw
            }
            _ => Action::None,
        }
    }

    fn drag_files(&mut self) -> Option<Vec<String>> {
        self.out.take()
    }

    fn drop_over(&mut self, at: Option<(i32, i32)>, paths: &[String], area: Rect) -> bool {
        let t = at.and_then(|(x, y)| self.target_at(area.x + x, area.y + y, area, paths));
        let changed = t != self.drop_at;
        self.drop_at = t;
        changed
    }

    fn drop_files(&mut self, paths: &[String], x: i32, y: i32, area: Rect) -> Option<Action> {
        self.drop_at = None;
        let t = self.target_at(area.x + x, area.y + y, area, paths)?;
        let dir = self.target_dir(t)?;
        let r = if self.trash { dnd::to_bin(paths) } else { dnd::transfer(paths, &dir, dnd::copying(paths, &dir)) };
        Some(match r {
            Ok(_) => Action::Redraw,
            Err(e) => Action::Notify(String::from("File"), e, "files"),
        })
    }

    /// Lists the folder again, keeping what was selected and the scroll; a
    /// folder that went away gives way to the nearest one still there.
    fn refresh(&mut self) {
        while !fs::is_dir(&self.dir) && self.dir.inner != "/" {
            self.dir = self.dir.parent();
            self.tabs[self.tab] = self.dir.clone();
        }
        let names = self.selected_names();
        let scroll = self.scroll;
        self.reload();
        self.scroll = scroll;
        self.selection = self.items.iter().enumerate().filter(|(_, it)| names.contains(&it.name)).map(|(i, _)| i).collect();
        self.selected = self.selection.last().copied();
        self.renaming = None;
    }

    fn key(&mut self, ev: crate::dev::input::KeyEvent) -> Action {
        use crate::dev::input::Key;
        if let Some((_, name)) = &mut self.renaming {
            match ev.key {
                Key::Enter => self.finish_rename(),
                Key::Escape => self.renaming = None,
                _ => {
                    crate::gui::ui::edit_text(name, &ev, 60);
                }
            }
            return Action::Redraw;
        }
        if self.menu.take().is_some() && ev.key == Key::Escape {
            return Action::Redraw;
        }
        let n = self.items.len();
        // Tabs: Ctrl+T opens one on this folder, Ctrl+W closes it, Ctrl+Tab
        // (with Shift backwards) goes to the next.
        if ev.ctrl && matches!(ev.key, Key::Char('a' | 'A')) {
            self.selection = (0..n).collect();
            self.selected = n.checked_sub(1);
            return Action::Redraw;
        }
        if ev.ctrl && !self.trash {
            match ev.key {
                Key::Char('t' | 'T') => self.new_tab(self.dir.clone()),
                Key::Char('w' | 'W') => self.close_tab(self.tab),
                Key::Tab if ev.shift => self.switch_tab((self.tab + self.tabs.len() - 1) % self.tabs.len()),
                Key::Tab => self.switch_tab((self.tab + 1) % self.tabs.len()),
                _ => return Action::None,
            }
            return Action::Redraw;
        }
        match ev.key {
            Key::Delete => self.do_action(if self.trash { Do::Delete } else { Do::ToBin }),
            Key::F(2) => self.do_action(Do::Rename),
            Key::Enter => match self.selected {
                Some(i) => self.open_item(i),
                None => Action::None,
            },
            Key::Backspace => {
                let parent = Path::parse("..", &self.dir).unwrap_or_else(|_| self.dir.clone());
                self.go(parent);
                Action::Redraw
            }
            Key::Right | Key::Down if n > 0 => {
                self.select_only(self.selected.map_or(0, |i| (i + 1).min(n - 1)));
                Action::Redraw
            }
            Key::Left | Key::Up if n > 0 => {
                self.select_only(self.selected.map_or(0, |i| i.saturating_sub(1)));
                Action::Redraw
            }
            _ => Action::None,
        }
    }

    fn scroll(&mut self, lines: i32) -> bool {
        let rows = (self.items.len() as i32 + 3) / 4;
        let max = (rows * TILE_H - 300).max(0);
        let old = self.scroll;
        self.scroll = (self.scroll + lines * 40).clamp(0, max);
        old != self.scroll
    }
}

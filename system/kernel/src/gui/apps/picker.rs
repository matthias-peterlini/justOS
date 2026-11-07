//! A sheet for choosing a file to open, or a name and folder to save in,
//! drawn over the app that asked (as macOS sheets are).

use crate::dev::input::{Key, KeyEvent};
use crate::fs::{self, Path};
use crate::gui::gfx::{Canvas, Rect};
use crate::gui::res;
use crate::gui::text::{self, Font};
use crate::gui::ui::{self, TEXT, TEXT_DIM, TEXT_FAINT};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

pub const TEXT_FILES: &[&str] = &["txt", "md", "toml", "json", "rs", "c", "h", "sh", "py", "ini", "cfg", "log", "csv", "html", "css", "js", "jt"];

const PLACES: [(&str, &str); 6] = [
    ("Home", "sys:/home"),
    ("Scrivania", "sys:/home/Scrivania"),
    ("Documenti", "sys:/home/Documenti"),
    ("Scaricati", "sys:/home/Scaricati"),
    ("Immagini", "sys:/home/Immagini"),
    ("Musica", "sys:/home/Musica"),
];
const ROW: i32 = 30;

pub enum Picked {
    Nothing,
    Cancel,
    Open(String),
    Save(String),
}

pub struct Picker {
    saving: bool,
    dir: Path,
    items: Vec<(String, bool)>,
    filter: &'static [&'static str],
    selected: Option<usize>,
    name: String,
    scroll: i32,
    hover: Option<Rect>,
}

impl Picker {
    pub fn open(dir: &str, filter: &'static [&'static str]) -> Picker {
        let mut p = Picker::new(dir, filter, false);
        p.reload();
        p
    }

    pub fn save(dir: &str, name: &str) -> Picker {
        let mut p = Picker::new(dir, &[], true);
        p.name = String::from(name);
        p.reload();
        p
    }

    fn new(dir: &str, filter: &'static [&'static str], saving: bool) -> Picker {
        let dir = fs::abs(dir).unwrap_or_else(|_| Path::root("sys"));
        if !fs::exists(&dir) {
            let _ = fs::make_dir(&dir);
        }
        Picker { saving, dir, items: Vec::new(), filter, selected: None, name: String::new(), scroll: 0, hover: None }
    }

    fn reload(&mut self) {
        let mut items: Vec<(String, bool)> = fs::list_dir(&self.dir)
            .map(|l| l.into_iter().map(|(e, st)| (e.name, st.kind == justfs::Kind::Dir)).collect())
            .unwrap_or_default();
        let filter = self.filter;
        let saving = self.saving;
        items.retain(|(n, dir)| {
            *dir || saving || filter.is_empty() || n.rsplit_once('.').is_some_and(|(_, e)| filter.contains(&e.to_ascii_lowercase().as_str()))
        });
        items.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())));
        self.items = items;
        self.selected = None;
        self.scroll = 0;
    }

    fn sheet(area: Rect) -> Rect {
        let w = (area.w - 40).min(560);
        let h = (area.h - 30).min(380);
        Rect::new(area.x + (area.w - w) / 2, area.y + 6, w, h)
    }

    fn list(s: Rect, saving: bool) -> Rect {
        let top = if saving { 104 } else { 56 };
        Rect::new(s.x + 160, s.y + top, s.w - 176, s.h - top - 62)
    }

    fn place(s: Rect, i: usize) -> Rect {
        Rect::new(s.x + 10, s.y + 56 + i as i32 * 32, 140, 30)
    }

    fn buttons(s: Rect) -> [Rect; 2] {
        let y = s.bottom() - 50;
        [Rect::new(s.right() - 236, y, 104, 36), Rect::new(s.right() - 122, y, 106, 36)]
    }

    fn field(s: Rect) -> Rect {
        Rect::new(s.x + 160, s.y + 52, s.w - 176, 40)
    }

    fn back(s: Rect) -> Rect {
        Rect::new(s.x + 16, s.y + 14, 28, 28)
    }

    pub fn draw(&mut self, c: &mut Canvas, area: Rect) {
        c.fill(area, 0x6610_0A16);
        let s = Self::sheet(area);
        c.shadow(s, 18, 16, 90);
        c.fill_round(s, 18, 0xF62A_2234);
        c.stroke_round(s, 18, 1, ui::BORDER);
        ui::icon_button(c, Self::back(s), "g-back", self.hover == Some(Self::back(s)));
        let title = match self.dir.name() {
            "" => format!("{}:", self.dir.volume),
            n => String::from(n),
        };
        text::draw(c, Font::Strong, s.x + 56, s.y + 18, &text::fit(Font::Strong, &title, s.w - 80), TEXT);
        for (i, (name, path)) in PLACES.iter().enumerate() {
            let r = Self::place(s, i);
            if fs::abs(path).is_ok_and(|p| p == self.dir) {
                c.fill_round(r, 8, ui::PANEL_HOVER);
            }
            res::draw_icon(c, super::files::icon_for(name, true), 18, r.x + 8, r.y + 6);
            text::draw(c, Font::Body, r.x + 34, r.y + 6, name, TEXT);
        }
        if self.saving {
            text::draw(c, Font::Small, s.x + 160, s.y + 34, "Salva come", TEXT_DIM);
            ui::text_field(c, Self::field(s), &self.name, "Nome del file", true);
        }
        let list = Self::list(s, self.saving);
        c.fill_round(list, 10, 0x33_00_00_00);
        let saved = c.clip();
        c.set_clip(saved.intersect(&list));
        if self.items.is_empty() {
            text::draw_centered(c, Font::Small, list.x + list.w / 2, list.y + 20, "Nessun file", TEXT_FAINT);
        }
        for (i, (name, dir)) in self.items.iter().enumerate() {
            let r = Rect::new(list.x + 4, list.y + 4 + i as i32 * ROW - self.scroll, list.w - 8, ROW - 2);
            if r.bottom() < list.y || r.y > list.bottom() {
                continue;
            }
            if self.selected == Some(i) {
                c.fill_round(r, 6, ui::SELECTED);
            }
            res::draw_icon(c, super::files::icon_for(name, *dir), 20, r.x + 6, r.y + 4);
            let color = if *dir || !self.saving { TEXT } else { TEXT_FAINT };
            text::draw(c, Font::Body, r.x + 34, r.y + 5, &text::fit(Font::Body, name, r.w - 44), color);
        }
        c.set_clip(saved);
        let [cancel, ok] = Self::buttons(s);
        ui::button(c, cancel, "Annulla", false, self.hover == Some(cancel));
        ui::button(c, ok, if self.saving { "Salva" } else { "Apri" }, true, self.hover == Some(ok));
    }

    fn confirm(&mut self) -> Picked {
        if self.saving {
            let name = self.name.trim();
            if name.is_empty() || name.contains('/') || name.contains(':') {
                return Picked::Nothing;
            }
            let name = if name.contains('.') { String::from(name) } else { format!("{name}.txt") };
            return Picked::Save(format!("{}", self.dir.join(&name)));
        }
        match self.selected.and_then(|i| self.items.get(i)).cloned() {
            Some((name, true)) => {
                self.dir = self.dir.join(&name);
                self.reload();
                Picked::Nothing
            }
            Some((name, false)) => Picked::Open(format!("{}", self.dir.join(&name))),
            None => Picked::Nothing,
        }
    }

    pub fn press(&mut self, x: i32, y: i32, double: bool, area: Rect) -> Picked {
        let s = Self::sheet(area);
        if !s.contains(x, y) {
            return Picked::Nothing;
        }
        let [cancel, ok] = Self::buttons(s);
        if cancel.contains(x, y) {
            return Picked::Cancel;
        }
        if ok.contains(x, y) {
            return self.confirm();
        }
        if Self::back(s).contains(x, y) {
            if let Ok(parent) = Path::parse("..", &self.dir) {
                self.dir = parent;
                self.reload();
            }
            return Picked::Nothing;
        }
        for (i, (_, path)) in PLACES.iter().enumerate() {
            if Self::place(s, i).contains(x, y) {
                if let Ok(p) = fs::abs(path) {
                    if !fs::exists(&p) {
                        let _ = fs::make_dir(&p);
                    }
                    self.dir = p;
                    self.reload();
                }
                return Picked::Nothing;
            }
        }
        let list = Self::list(s, self.saving);
        if list.contains(x, y) {
            let i = ((y - list.y - 4 + self.scroll) / ROW) as usize;
            if i < self.items.len() {
                self.selected = Some(i);
                let (name, dir) = self.items[i].clone();
                if self.saving && !dir {
                    self.name = name;
                } else if double {
                    return self.confirm();
                }
            } else {
                self.selected = None;
            }
        }
        Picked::Nothing
    }

    pub fn hover(&mut self, at: Option<(i32, i32)>, area: Rect) -> bool {
        let s = Self::sheet(area);
        let [cancel, ok] = Self::buttons(s);
        let h = at.and_then(|(x, y)| [cancel, ok, Self::back(s)].into_iter().find(|r| r.contains(x, y)));
        let changed = h != self.hover;
        self.hover = h;
        changed
    }

    pub fn scroll(&mut self, lines: i32) -> bool {
        let old = self.scroll;
        self.scroll = (self.scroll + lines * ROW).clamp(0, (self.items.len() as i32 * ROW - 160).max(0));
        old != self.scroll
    }

    pub fn key(&mut self, ev: KeyEvent) -> Picked {
        match ev.key {
            Key::Escape => Picked::Cancel,
            Key::Enter => self.confirm(),
            Key::Up => {
                self.selected = Some(self.selected.map_or(0, |i| i.saturating_sub(1)));
                Picked::Nothing
            }
            Key::Down if !self.items.is_empty() => {
                self.selected = Some(self.selected.map_or(0, |i| (i + 1).min(self.items.len() - 1)));
                Picked::Nothing
            }
            _ => {
                if self.saving {
                    ui::edit_text(&mut self.name, &ev, 60);
                }
                Picked::Nothing
            }
        }
    }
}

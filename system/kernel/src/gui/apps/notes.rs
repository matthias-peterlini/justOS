//! Note: notes in `sys:/home/Note`, one text file each, newest first. The
//! first line is the title; everything is saved as you type.

use crate::dev::input::{Key, KeyEvent};
use crate::fs;
use crate::gui::gfx::{Canvas, Rect};
use crate::gui::res;
use crate::gui::text::{self, Font};
use crate::gui::textarea::TextArea;
use crate::gui::ui;
use crate::gui::wm::{Action, App};
use crate::time;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const DIR: &str = "sys:/home/Note";
const SIDEBAR: i32 = 230;
const ROW: i32 = 62;

struct NoteFile {
    path: String,
    title: String,
    preview: String,
    modified: u64,
}

pub struct Notes {
    list: Vec<NoteFile>,
    current: Option<usize>,
    area: TextArea,
    search: String,
    searching: bool,
    /// When the text changed last (saved a moment later).
    edited: Option<u64>,
    scroll: i32,
    hover: Option<usize>,
    tr: Rect,
}

fn summary(text: &str) -> (String, String) {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let title = lines.next().unwrap_or("Nuova nota");
    let preview = lines.next().unwrap_or("Nessun altro testo");
    (String::from(title), String::from(preview))
}

/// "oggi 14:05", "ieri", "12/03/26".
pub fn when(secs: u64) -> String {
    let d = time::from_epoch(secs);
    let now = time::from_epoch(time::now());
    let days = |x: &time::DateTime| time::to_epoch(x.year, x.month, x.day, 12, 0, 0) / 86_400;
    match days(&now).saturating_sub(days(&d)) {
        0 => format!("{:02}:{:02}", d.hour, d.min),
        1 => String::from("ieri"),
        _ => format!("{:02}/{:02}/{:02}", d.day, d.month, d.year % 100),
    }
}

impl Notes {
    pub fn new() -> Notes {
        if let Ok(p) = fs::abs(DIR) {
            if !fs::exists(&p) {
                let _ = fs::make_dir(&p);
                let _ = fs::write_file(
                    &p.join("Benvenuto.txt"),
                    "Benvenuto in Note\n\nScrivi qui le tue idee: la prima riga diventa il titolo e tutto si salva da solo.\n\nPremi + per una nota nuova.\n"
                        .as_bytes(),
                );
            }
        }
        let mut n = Notes {
            list: Vec::new(),
            current: None,
            area: TextArea::new(""),
            search: String::new(),
            searching: false,
            edited: None,
            scroll: 0,
            hover: None,
            tr: Rect::new(0, 0, 400, 400),
        };
        n.area.placeholder = "Scrivi una nota…";
        n.reload();
        if !n.list.is_empty() {
            n.select(0);
        }
        n
    }

    fn reload(&mut self) {
        let current = self.current.and_then(|i| self.list.get(i)).map(|n| n.path.clone());
        let mut list = Vec::new();
        if let Ok(dir) = fs::abs(DIR) {
            for (e, st) in fs::list_dir(&dir).unwrap_or_default() {
                if st.kind != justfs::Kind::File || !e.name.ends_with(".txt") {
                    continue;
                }
                let p = dir.join(&e.name);
                let text = fs::read_file(&p).map(|b| String::from(String::from_utf8_lossy(&b))).unwrap_or_default();
                let (title, preview) = summary(&text);
                list.push(NoteFile { path: format!("{p}"), title, preview, modified: st.modified });
            }
        }
        list.sort_by(|a, b| b.modified.cmp(&a.modified));
        self.list = list;
        self.current = current.and_then(|c| self.list.iter().position(|n| n.path == c));
    }

    fn visible(&self) -> Vec<usize> {
        let q = self.search.to_lowercase();
        (0..self.list.len())
            .filter(|&i| q.is_empty() || self.list[i].title.to_lowercase().contains(&q) || self.list[i].preview.to_lowercase().contains(&q))
            .collect()
    }

    fn save(&mut self) {
        self.edited = None;
        let Some(i) = self.current else { return };
        let path = self.list[i].path.clone();
        let text = self.area.text();
        if let Ok(p) = fs::abs(&path) {
            let _ = fs::write_file(&p, text.as_bytes());
        }
        self.area.mark_saved();
        let (title, preview) = summary(&text);
        let n = &mut self.list[i];
        n.title = title;
        n.preview = preview;
        n.modified = time::now();
    }

    fn select(&mut self, i: usize) {
        if self.edited.is_some() {
            self.save();
        }
        let Some(n) = self.list.get(i) else { return };
        let text = fs::abs(&n.path).and_then(|p| fs::read_file(&p)).map(|b| String::from(String::from_utf8_lossy(&b))).unwrap_or_default();
        self.area.set_text(&text);
        self.current = Some(i);
    }

    fn new_note(&mut self) {
        if self.edited.is_some() {
            self.save();
        }
        let path = super::textedit::free_name(DIR, "Nota", "txt");
        if let Ok(p) = fs::abs(&path) {
            let _ = fs::write_file(&p, b"");
        }
        self.list.insert(0, NoteFile { path, title: String::from("Nuova nota"), preview: String::from("Nessun altro testo"), modified: time::now() });
        self.current = Some(0);
        self.area.set_text("");
        self.search.clear();
        self.scroll = 0;
    }

    fn delete(&mut self) {
        let Some(i) = self.current else { return };
        let n = self.list.remove(i);
        // To the bin, so it can come back.
        if let (Ok(from), Ok(bin)) = (fs::abs(&n.path), fs::abs("sys:/home/Cestino")) {
            if !fs::exists(&bin) {
                let _ = fs::make_dir(&bin);
            }
            let name = from.name();
            let to = bin.join(name);
            if fs::rename(&from, &to).is_err() {
                let _ = fs::remove(&from);
            }
        }
        self.edited = None;
        self.current = None;
        if !self.list.is_empty() {
            self.select(i.min(self.list.len() - 1));
        } else {
            self.area.set_text("");
        }
    }

    fn search_rect(area: Rect) -> Rect {
        Rect::new(area.x + 12, area.y + 12, SIDEBAR - 24, 32)
    }

    fn list_rect(area: Rect) -> Rect {
        Rect::new(area.x + 8, area.y + 54, SIDEBAR - 16, area.h - 62)
    }

    fn buttons(area: Rect) -> [Rect; 2] {
        let y = area.y + 12;
        [Rect::new(area.right() - 90, y, 32, 32), Rect::new(area.right() - 48, y, 32, 32)]
    }

    fn text_rect(area: Rect) -> Rect {
        Rect::new(area.x + SIDEBAR + 6, area.y + 52, area.w - SIDEBAR - 16, area.h - 62)
    }
}

impl App for Notes {
    fn title(&self) -> String {
        String::from("Note")
    }

    fn icon(&self) -> &str {
        "notes"
    }

    fn kind(&self) -> &'static str {
        "notes"
    }

    fn size(&self) -> (i32, i32) {
        (760, 480)
    }

    fn resizable(&self) -> bool {
        true
    }

    fn sidebar(&self) -> i32 {
        SIDEBAR - 2
    }

    fn draw(&mut self, c: &mut Canvas, area: Rect, _blur: &Canvas, focused: bool) {
        // The list.
        ui::search_field(c, Self::search_rect(area), &self.search, "Cerca", self.searching && focused);
        let lr = Self::list_rect(area);
        let saved = c.clip();
        c.set_clip(saved.intersect(&lr));
        let visible = self.visible();
        if visible.is_empty() {
            text::draw_centered(c, Font::Small, lr.x + lr.w / 2, lr.y + 20, "Nessuna nota", ui::text_faint());
        }
        for (k, &i) in visible.iter().enumerate() {
            let r = Rect::new(lr.x, lr.y + k as i32 * ROW - self.scroll, lr.w, ROW - 4);
            if r.bottom() < lr.y || r.y > lr.bottom() {
                continue;
            }
            let n = &self.list[i];
            if self.current == Some(i) {
                c.fill_round(r, 10, ui::sel());
            } else if self.hover == Some(i) {
                c.fill_round(r, 10, ui::pane());
            }
            text::draw(c, Font::Strong, r.x + 12, r.y + 9, &text::fit(Font::Strong, &n.title, r.w - 24), ui::text());
            let w = text::draw(c, Font::Small, r.x + 12, r.y + 33, &when(n.modified), ui::text());
            text::draw(c, Font::Small, r.x + 20 + w, r.y + 33, &text::fit(Font::Small, &n.preview, r.w - 40 - w), ui::text_dim());
        }
        c.set_clip(saved);
        // The note.
        let [plus, bin] = Self::buttons(area);
        ui::icon_button(c, plus, "g-plus", false);
        ui::icon_button(c, bin, "g-trash", false);
        if let Some(i) = self.current {
            let d = time::from_epoch(self.list[i].modified);
            let date = format!("{} {} {}, {:02}:{:02}", d.day, super::super::desktop::MONTHS[(d.month as usize + 11) % 12], d.year, d.hour, d.min);
            text::draw_centered(c, Font::Small, area.x + SIDEBAR + (area.w - SIDEBAR) / 2 - 40, area.y + 20, &date, ui::text_faint());
            let tr = Self::text_rect(area);
            self.tr = tr;
            self.area.draw(c, tr, focused && !self.searching);
        } else {
            let cx = area.x + SIDEBAR + (area.w - SIDEBAR) / 2;
            res::draw_icon_faded(c, "notes", 64, cx - 32, area.y + area.h / 2 - 60, 120);
            text::draw_centered(c, Font::Body, cx, area.y + area.h / 2 + 14, "Nessuna nota selezionata", ui::text_dim());
        }
    }

    fn press(&mut self, x: i32, y: i32, double: bool, area: Rect) -> Action {
        let (ax, ay) = (area.x + x, area.y + y);
        let [plus, bin] = Self::buttons(area);
        if plus.contains(ax, ay) {
            self.new_note();
            self.searching = false;
            return Action::Redraw;
        }
        if bin.contains(ax, ay) {
            self.delete();
            return Action::Redraw;
        }
        if Self::search_rect(area).contains(ax, ay) {
            self.searching = true;
            return Action::Redraw;
        }
        let lr = Self::list_rect(area);
        if lr.contains(ax, ay) {
            let k = ((ay - lr.y + self.scroll) / ROW) as usize;
            if let Some(&i) = self.visible().get(k) {
                self.select(i);
            }
            self.searching = false;
            return Action::Redraw;
        }
        let tr = Self::text_rect(area);
        if tr.contains(ax, ay) {
            self.searching = false;
            if self.current.is_none() {
                self.new_note();
            }
            self.area.press(tr, ax, ay, double, false);
            return Action::Redraw;
        }
        Action::None
    }

    fn drag(&mut self, x: i32, y: i32, area: Rect) -> bool {
        self.area.drag(Self::text_rect(area), area.x + x, area.y + y)
    }

    fn release(&mut self, _area: Rect) -> Action {
        self.area.release();
        Action::None
    }

    fn hover(&mut self, at: Option<(i32, i32)>, area: Rect) -> bool {
        let lr = Self::list_rect(area);
        let h = at.and_then(|(x, y)| {
            let (ax, ay) = (area.x + x, area.y + y);
            if !lr.contains(ax, ay) {
                return None;
            }
            self.visible().get(((ay - lr.y + self.scroll) / ROW) as usize).copied()
        });
        let changed = h != self.hover;
        self.hover = h;
        changed
    }

    fn key(&mut self, ev: KeyEvent) -> Action {
        if ev.ctrl && matches!(ev.key, Key::Char('n') | Key::Char('N')) {
            self.new_note();
            return Action::Redraw;
        }
        if self.searching {
            match ev.key {
                Key::Escape => {
                    self.search.clear();
                    self.searching = false;
                }
                Key::Enter => self.searching = false,
                _ => {
                    ui::edit_text(&mut self.search, &ev, 40);
                }
            }
            self.scroll = 0;
            return Action::Redraw;
        }
        if self.current.is_none() {
            if matches!(ev.key, Key::Char(_) | Key::Enter) {
                self.new_note();
            } else {
                return Action::None;
            }
        }
        if self.area.key(ev, self.tr) {
            if self.area.dirty {
                self.edited = Some(time::uptime_ms());
                // The title in the list follows the first line.
                if let Some(i) = self.current {
                    let (title, preview) = summary(&self.area.text());
                    self.list[i].title = title;
                    self.list[i].preview = preview;
                }
            }
            return Action::Redraw;
        }
        Action::None
    }

    fn scroll(&mut self, lines: i32) -> bool {
        self.area.scroll_by(lines)
    }

    fn tick(&mut self) -> bool {
        if self.edited.is_some_and(|t| time::uptime_ms() - t > 800) {
            self.save();
        }
        self.area.tick()
    }

    fn changed(&mut self, _area: Rect) -> Option<Rect> {
        Some(self.area.caret())
    }

    fn close(&mut self) {
        if self.edited.is_some() {
            self.save();
        }
        // Empty notes are not kept.
        for n in &self.list {
            if let Ok(p) = fs::abs(&n.path) {
                if fs::stat(&p).is_ok_and(|s| s.size == 0) {
                    let _ = fs::remove(&p);
                }
            }
        }
    }
}

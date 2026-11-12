//! Editor di testo: plain text files, with a toolbar (new, open, save), a
//! chooser for files and a status line. Changes are saved when the window
//! closes, as on macOS.

use super::picker::{Picker, Picked};
use crate::dev::input::{Key, KeyEvent};
use crate::fs;
use crate::gui::gfx::{Canvas, Rect};
use crate::gui::text::{self, Font};
use crate::gui::textarea::TextArea;
use crate::gui::ui;
use crate::gui::wm::{Action, App};
use alloc::format;
use alloc::string::String;

pub struct TextEdit {
    path: Option<String>,
    area: TextArea,
    picker: Option<Picker>,
    hover: Option<usize>,
    note: String,
    /// Where the text was drawn last.
    tr: Rect,
}

const TOOLBAR: i32 = 50;

/// A file name not taken yet in `dir`: "Senza titolo.txt", "Senza titolo 2.txt"...
pub fn free_name(dir: &str, stem: &str, ext: &str) -> String {
    for n in 1..1000 {
        let name = if n == 1 { format!("{stem}.{ext}") } else { format!("{stem} {n}.{ext}") };
        let path = format!("{dir}/{name}");
        if !fs::abs(&path).is_ok_and(|p| fs::exists(&p)) {
            return path;
        }
    }
    format!("{dir}/{stem}.{ext}")
}

impl TextEdit {
    pub fn new(path: Option<&str>) -> TextEdit {
        let mut t = TextEdit { path: None, area: TextArea::new(""), picker: None, hover: None, note: String::new(), tr: Rect::new(0, 0, 660, 400) };
        t.area.placeholder = "Scrivi qui…";
        if let Some(p) = path {
            t.load(p);
        }
        t
    }

    fn load(&mut self, path: &str) {
        match fs::abs(path).and_then(|p| fs::read_file(&p)) {
            Ok(bytes) => {
                self.area.set_text(&String::from_utf8_lossy(&bytes));
                self.path = Some(String::from(path));
                self.note.clear();
            }
            Err(e) => self.note = format!("Non si può aprire: {e}"),
        }
    }

    fn save_to(&mut self, path: &str) -> bool {
        let r = fs::abs(path).and_then(|p| fs::write_file(&p, self.area.text().as_bytes()));
        match r {
            Ok(()) => {
                self.path = Some(String::from(path));
                self.area.mark_saved();
                self.note = String::from("Salvato");
                true
            }
            Err(e) => {
                self.note = format!("Non salvato: {e}");
                false
            }
        }
    }

    fn save(&mut self) {
        match self.path.clone() {
            Some(p) => {
                self.save_to(&p);
            }
            None => self.picker = Some(Picker::save("sys:/home/Documenti", "Senza titolo.txt")),
        }
    }

    fn buttons(area: Rect) -> [Rect; 3] {
        let y = area.y + 10;
        [Rect::new(area.x + 14, y, 84, 32), Rect::new(area.x + 106, y, 84, 32), Rect::new(area.x + 198, y, 84, 32)]
    }

    fn text_rect(area: Rect) -> Rect {
        Rect::new(area.x + 10, area.y + TOOLBAR + 2, area.w - 20, area.h - TOOLBAR - 34)
    }

    fn name(&self) -> String {
        match &self.path {
            Some(p) => String::from(p.rsplit('/').next().unwrap_or(p)),
            None => String::from("Senza titolo"),
        }
    }
}

impl App for TextEdit {
    fn title(&self) -> String {
        if self.area.dirty {
            format!("{} — modificato", self.name())
        } else {
            self.name()
        }
    }

    fn icon(&self) -> &str {
        "text-editor"
    }

    fn kind(&self) -> &'static str {
        "textedit"
    }

    fn size(&self) -> (i32, i32) {
        (680, 480)
    }

    fn resizable(&self) -> bool {
        true
    }

    fn draw(&mut self, c: &mut Canvas, area: Rect, _blur: &Canvas, focused: bool) {
        for (i, (b, label)) in Self::buttons(area).iter().zip(["Nuovo", "Apri…", "Salva"]).enumerate() {
            ui::button(c, *b, label, false, self.hover == Some(i));
        }
        if !self.note.is_empty() {
            text::draw_right(c, Font::Small, area.right() - 18, area.y + 18, &self.note, ui::text_dim());
        }
        let tr = Self::text_rect(area);
        self.tr = tr;
        c.fill_round(tr, 14, 0xF21C_1622);
        self.area.draw(c, tr, focused && self.picker.is_none());
        let (words, chars) = self.area.counts();
        let status = format!("{words} parole · {chars} caratteri");
        text::draw(c, Font::Small, area.x + 20, area.bottom() - 24, &status, ui::text_dim());
        if let Some(p) = &self.path {
            text::draw_right(c, Font::Small, area.right() - 20, area.bottom() - 24, &text::fit(Font::Small, p, area.w / 2), ui::text_faint());
        }
        if let Some(p) = &mut self.picker {
            p.draw(c, area);
        }
    }

    fn press(&mut self, x: i32, y: i32, double: bool, area: Rect) -> Action {
        let (ax, ay) = (area.x + x, area.y + y);
        if let Some(p) = &mut self.picker {
            match p.press(ax, ay, double, area) {
                Picked::Nothing => {}
                Picked::Cancel => self.picker = None,
                Picked::Open(path) => {
                    self.picker = None;
                    if self.path.is_none() && self.area.is_empty() {
                        self.load(&path);
                    } else {
                        return Action::OpenFile(path);
                    }
                }
                Picked::Save(path) => {
                    self.picker = None;
                    self.save_to(&path);
                }
            }
            return Action::Redraw;
        }
        let b = Self::buttons(area);
        if b[0].contains(ax, ay) {
            return Action::Open("textedit-new");
        }
        if b[1].contains(ax, ay) {
            self.picker = Some(Picker::open("sys:/home/Documenti", super::picker::TEXT_FILES));
            return Action::Redraw;
        }
        if b[2].contains(ax, ay) {
            self.save();
            return Action::Redraw;
        }
        let tr = Self::text_rect(area);
        if tr.contains(ax, ay) {
            self.area.press(tr, ax, ay, double, false);
            return Action::Redraw;
        }
        Action::None
    }

    fn drag(&mut self, x: i32, y: i32, area: Rect) -> bool {
        self.picker.is_none() && self.area.drag(Self::text_rect(area), area.x + x, area.y + y)
    }

    fn release(&mut self, _area: Rect) -> Action {
        self.area.release();
        Action::None
    }

    fn hover(&mut self, at: Option<(i32, i32)>, area: Rect) -> bool {
        let h = at.and_then(|(x, y)| Self::buttons(area).iter().position(|b| b.contains(area.x + x, area.y + y)));
        let mut changed = h != self.hover;
        self.hover = h;
        if let Some(p) = &mut self.picker {
            changed |= p.hover(at.map(|(x, y)| (area.x + x, area.y + y)), area);
        }
        changed
    }

    fn key(&mut self, ev: KeyEvent) -> Action {
        if let Some(p) = &mut self.picker {
            match p.key(ev) {
                Picked::Cancel => self.picker = None,
                Picked::Save(path) => {
                    self.picker = None;
                    self.save_to(&path);
                }
                Picked::Open(path) => {
                    self.picker = None;
                    self.load(&path);
                }
                Picked::Nothing => {}
            }
            return Action::Redraw;
        }
        if ev.ctrl {
            match ev.key {
                Key::Char('s') | Key::Char('S') => {
                    self.save();
                    return Action::Redraw;
                }
                Key::Char('o') | Key::Char('O') => {
                    self.picker = Some(Picker::open("sys:/home/Documenti", super::picker::TEXT_FILES));
                    return Action::Redraw;
                }
                Key::Char('n') | Key::Char('N') => return Action::Open("textedit-new"),
                _ => {}
            }
        }
        if self.area.key(ev, self.tr) {
            self.note.clear();
            return Action::Redraw;
        }
        Action::None
    }

    fn scroll(&mut self, lines: i32) -> bool {
        if let Some(p) = &mut self.picker {
            return p.scroll(lines);
        }
        self.area.scroll_by(lines)
    }

    /// A file dropped on an empty, untitled editor opens in it; on one with
    /// a text already, it opens in a window of its own.
    fn drop_files(&mut self, paths: &[String], _x: i32, _y: i32, _area: Rect) -> Option<Action> {
        let first = paths.iter().find(|p| fs::abs(p).is_ok_and(|p| fs::exists(&p) && !fs::is_dir(&p)))?;
        if self.picker.is_none() && self.path.is_none() && !self.area.dirty && self.area.text().is_empty() {
            self.load(first);
            return Some(Action::Redraw);
        }
        Some(Action::OpenFile(first.clone()))
    }

    fn tick(&mut self) -> bool {
        self.picker.is_none() && self.area.tick()
    }

    fn changed(&mut self, _area: Rect) -> Option<Rect> {
        Some(self.area.caret())
    }

    fn close(&mut self) {
        if !self.area.dirty {
            return;
        }
        match self.path.clone() {
            Some(p) => {
                self.save_to(&p);
            }
            None if !self.area.is_empty() => {
                let dir = "sys:/home/Documenti";
                if let Ok(d) = fs::abs(dir) {
                    if !fs::exists(&d) {
                        let _ = fs::make_dir(&d);
                    }
                }
                let path = free_name(dir, "Senza titolo", "txt");
                self.save_to(&path);
                super::super::notify("Editor di testo", &format!("Il testo è stato salvato in Documenti come «{}».", self.name()), "text-editor");
            }
            None => {}
        }
    }
}

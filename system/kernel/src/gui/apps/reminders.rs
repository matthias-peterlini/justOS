//! Promemoria: a list of things to do, ticked off with a click. Kept in
//! `sys:/home/Promemoria.txt`, one per line ("[x] " in front of the done
//! ones), so it is readable anywhere.

use crate::dev::input::{Key, KeyEvent};
use crate::fs;
use crate::gui::gfx::{Canvas, Rect};
use crate::gui::text::{self, Font};
use crate::gui::ui::{self, Anim, TEXT, TEXT_DIM, TEXT_FAINT};
use crate::gui::wm::{Action, App};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const FILE: &str = "sys:/home/Promemoria.txt";
const ROW: i32 = 44;

struct Item {
    text: String,
    done: bool,
    /// Ticked a moment ago: it fades before moving down.
    ticked: Option<Anim>,
}

pub struct Reminders {
    items: Vec<Item>,
    input: String,
    show_done: bool,
    scroll: i32,
    hover: Option<usize>,
    blink: bool,
    ticks: u32,
}

impl Reminders {
    pub fn new() -> Reminders {
        let text = fs::abs(FILE).and_then(|p| fs::read_file(&p)).map(|b| String::from(String::from_utf8_lossy(&b)));
        let text = text.unwrap_or_else(|_| String::from("Prova le app di JustOS\nScegli uno sfondo nelle Impostazioni\n[x] Avvia JustOS\n"));
        let items = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| match l.strip_prefix("[x] ") {
                Some(t) => Item { text: String::from(t), done: true, ticked: None },
                None => Item { text: String::from(l), done: false, ticked: None },
            })
            .collect();
        Reminders { items, input: String::new(), show_done: false, scroll: 0, hover: None, blink: true, ticks: 0 }
    }

    fn save(&self) {
        let mut out = String::new();
        for it in &self.items {
            if it.done {
                out.push_str("[x] ");
            }
            out.push_str(&it.text);
            out.push('\n');
        }
        if let Ok(p) = fs::abs(FILE) {
            let _ = fs::write_file(&p, out.as_bytes());
        }
    }

    /// The rows in order: open ones, then the done ones if shown. An item
    /// ticked a moment ago stays where it was until it has faded.
    fn rows(&self) -> Vec<usize> {
        let mut v: Vec<usize> = (0..self.items.len()).filter(|&i| !self.items[i].done || self.items[i].ticked.is_some()).collect();
        if self.show_done {
            v.extend((0..self.items.len()).filter(|&i| self.items[i].done && self.items[i].ticked.is_none()));
        }
        v
    }

    fn list(area: Rect) -> Rect {
        Rect::new(area.x + 16, area.y + 92, area.w - 32, area.h - 162)
    }

    fn input_rect(area: Rect) -> Rect {
        Rect::new(area.x + 16, area.bottom() - 58, area.w - 32, 44)
    }

    fn toggle_done(area: Rect) -> Rect {
        Rect::new(area.right() - 176, area.y + 26, 160, 32)
    }

    fn add(&mut self) {
        let t = self.input.trim();
        if !t.is_empty() {
            self.items.insert(0, Item { text: String::from(t), done: false, ticked: None });
            self.input.clear();
            self.save();
        }
    }
}

impl App for Reminders {
    fn title(&self) -> String {
        String::from("Promemoria")
    }

    fn icon(&self) -> &str {
        "reminders"
    }

    fn kind(&self) -> &'static str {
        "reminders"
    }

    fn size(&self) -> (i32, i32) {
        (460, 520)
    }

    fn resizable(&self) -> bool {
        true
    }

    fn draw(&mut self, c: &mut Canvas, area: Rect, _blur: &Canvas, focused: bool) {
        let open = self.items.iter().filter(|i| !i.done).count();
        text::draw(c, Font::Title, area.x + 20, area.y + 14, "Da fare", ui::ORANGE);
        text::draw(c, Font::Body, area.x + 22, area.y + 58, &format!("{open} da completare"), TEXT_DIM);
        let tb = Self::toggle_done(area);
        ui::button(c, tb, if self.show_done { "Nascondi fatti" } else { "Mostra fatti" }, false, false);
        let list = Self::list(area);
        let saved = c.clip();
        c.set_clip(saved.intersect(&list));
        let rows = self.rows();
        if rows.is_empty() {
            text::draw_centered(c, Font::Body, list.x + list.w / 2, list.y + 40, "Niente da fare. Ottimo!", TEXT_FAINT);
        }
        for (k, &i) in rows.iter().enumerate() {
            let it = &self.items[i];
            let r = Rect::new(list.x, list.y + k as i32 * ROW - self.scroll, list.w, ROW);
            if r.bottom() < list.y || r.y > list.bottom() {
                continue;
            }
            if self.hover == Some(i) {
                c.fill_round(r, 10, ui::PANEL);
            }
            let fade = it.ticked.as_ref().map_or(255, |a| 255 - a.t() * 155 / 1000);
            let (cx, cy) = (r.x + 22, r.y + ROW / 2);
            if it.done {
                c.fill_circle(cx, cy, 11, ui::ORANGE);
                c.line(cx - 5, cy, cx - 1, cy + 4, 2, 0xFFFF_FFFF);
                c.line(cx - 1, cy + 4, cx + 5, cy - 4, 2, 0xFFFF_FFFF);
            } else {
                c.fill_circle(cx, cy, 11, 0x80FF_FFFF);
                c.fill_circle(cx, cy, 9, 0xFF2A_2234);
            }
            let color = if it.done { (fade.min(150) << 24) | 0xF5_F2F8 } else { TEXT };
            let label = text::fit(Font::Body, &it.text, r.w - 60);
            let w = text::draw(c, Font::Body, r.x + 46, r.y + 12, &label, color);
            if it.done {
                c.fill(Rect::new(r.x + 46, r.y + 22, w, 1), color);
            }
            c.fill(Rect::new(r.x + 46, r.bottom() - 1, r.w - 46, 1), ui::SEPARATOR);
        }
        c.set_clip(saved);
        let ir = Self::input_rect(area);
        c.fill_round(ir, 12, 0x26FF_FFFF);
        c.fill_circle(ir.x + 22, ir.y + ir.h / 2, 11, ui::ORANGE);
        c.fill(Rect::new(ir.x + 17, ir.y + ir.h / 2 - 1, 11, 2), 0xFFFF_FFFF);
        c.fill(Rect::new(ir.x + 21, ir.y + ir.h / 2 - 5, 2, 11), 0xFFFF_FFFF);
        let (lh, _) = text::metrics(Font::Body);
        let ty = ir.y + (ir.h - lh) / 2;
        if self.input.is_empty() {
            text::draw(c, Font::Body, ir.x + 46, ty, "Nuovo promemoria", TEXT_FAINT);
        }
        let w = text::draw(c, Font::Body, ir.x + 46, ty, &text::fit(Font::Body, &self.input, ir.w - 60), TEXT);
        if focused && self.blink {
            c.fill(Rect::new(ir.x + 47 + w, ty + 2, 2, lh - 4), ui::ORANGE);
        }
    }

    fn press(&mut self, x: i32, y: i32, _double: bool, area: Rect) -> Action {
        let (ax, ay) = (area.x + x, area.y + y);
        if Self::toggle_done(area).contains(ax, ay) {
            self.show_done = !self.show_done;
            return Action::Redraw;
        }
        let list = Self::list(area);
        if list.contains(ax, ay) {
            let k = ((ay - list.y + self.scroll) / ROW) as usize;
            if let Some(&i) = self.rows().get(k) {
                let it = &mut self.items[i];
                it.done = !it.done;
                it.ticked = if it.done { Some(Anim::new(700)) } else { None };
                self.save();
            }
            return Action::Redraw;
        }
        Action::None
    }

    fn hover(&mut self, at: Option<(i32, i32)>, area: Rect) -> bool {
        let list = Self::list(area);
        let h = at.and_then(|(x, y)| {
            let (ax, ay) = (area.x + x, area.y + y);
            if !list.contains(ax, ay) {
                return None;
            }
            self.rows().get(((ay - list.y + self.scroll) / ROW) as usize).copied()
        });
        let changed = h != self.hover;
        self.hover = h;
        changed
    }

    fn menu(&mut self, _x: i32, _y: i32, _area: Rect) -> Action {
        // Right click on a row: delete it.
        if let Some(i) = self.hover.take() {
            self.items.remove(i);
            self.save();
            return Action::Redraw;
        }
        Action::None
    }

    fn key(&mut self, ev: KeyEvent) -> Action {
        self.blink = true;
        match ev.key {
            Key::Enter => self.add(),
            _ => {
                if !ui::edit_text(&mut self.input, &ev, 120) {
                    return Action::None;
                }
            }
        }
        Action::Redraw
    }

    fn scroll(&mut self, lines: i32) -> bool {
        let old = self.scroll;
        self.scroll = (self.scroll + lines * ROW).clamp(0, (self.rows().len() as i32 * ROW - 200).max(0));
        old != self.scroll
    }

    fn tick(&mut self) -> bool {
        self.ticks += 1;
        if self.ticks.is_multiple_of(5) {
            self.blink = !self.blink;
            return true;
        }
        false
    }

    fn animating(&self) -> bool {
        self.items.iter().any(|i| i.ticked.is_some())
    }

    fn frame(&mut self) -> bool {
        for it in self.items.iter_mut() {
            if it.ticked.as_ref().is_some_and(|a| a.done()) {
                it.ticked = None;
            }
        }
        true
    }
}

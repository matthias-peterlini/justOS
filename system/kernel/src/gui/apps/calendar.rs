//! Calendario: a month at a time, with the Italian holidays and your
//! events. Events are kept in `sys:/home/Calendario.txt`, one per line:
//! date, time (or nothing) and text, separated by tabs.

use crate::dev::input::{Key, KeyEvent};
use crate::fs;
use crate::gui::desktop::MONTHS;
use crate::gui::gfx::{Canvas, Rect};
use crate::gui::text::{self, Font};
use crate::gui::ui::{self, TEXT, TEXT_DIM, TEXT_FAINT};
use crate::gui::wm::{Action, App};
use crate::time;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const FILE: &str = "sys:/home/Calendario.txt";
const DAYS: [&str; 7] = ["lun", "mar", "mer", "gio", "ven", "sab", "dom"];
const WEEKDAYS: [&str; 7] = ["Lunedì", "Martedì", "Mercoledì", "Giovedì", "Venerdì", "Sabato", "Domenica"];
const SIDE: i32 = 250;

#[derive(Clone)]
struct Event {
    date: (u64, u64, u64),
    time: String,
    text: String,
}

pub struct Calendar {
    year: u64,
    month: u64,
    selected: (u64, u64, u64),
    events: Vec<Event>,
    input: String,
    hover: Option<Rect>,
    blink: bool,
    ticks: u32,
}

/// Easter Sunday (the Gregorian computus): (month, day).
fn easter(y: u64) -> (u64, u64) {
    let a = y % 19;
    let b = y / 100;
    let c = y % 100;
    let d = b / 4;
    let e = b % 4;
    let f = (b + 8) / 25;
    let g = (b - f + 1) / 3;
    let h = (19 * a + b - d - g + 15) % 30;
    let i = c / 4;
    let k = c % 4;
    let l = (32 + 2 * e + 2 * i - h - k) % 7;
    let m = (a + 11 * h + 22 * l) / 451;
    let month = (h + l - 7 * m + 114) / 31;
    let day = (h + l - 7 * m + 114) % 31 + 1;
    (month, day)
}

/// The national holiday on a day, if any.
fn holiday(y: u64, m: u64, d: u64) -> Option<&'static str> {
    let fixed = match (m, d) {
        (1, 1) => Some("Capodanno"),
        (1, 6) => Some("Epifania"),
        (4, 25) => Some("Festa della Liberazione"),
        (5, 1) => Some("Festa del Lavoro"),
        (6, 2) => Some("Festa della Repubblica"),
        (8, 15) => Some("Ferragosto"),
        (11, 1) => Some("Ognissanti"),
        (12, 8) => Some("Immacolata"),
        (12, 25) => Some("Natale"),
        (12, 26) => Some("Santo Stefano"),
        _ => None,
    };
    if fixed.is_some() {
        return fixed;
    }
    let (em, ed) = easter(y);
    if (m, d) == (em, ed) {
        return Some("Pasqua");
    }
    // Easter Monday: the day after (it may fall in the next month).
    let next = if ed == time::days_in_month(y, em) { (em + 1, 1) } else { (em, ed + 1) };
    if (m, d) == next {
        return Some("Lunedì dell'Angelo");
    }
    None
}

fn weekday(y: u64, m: u64, d: u64) -> usize {
    time::from_epoch(time::to_epoch(y, m, d, 12, 0, 0)).weekday as usize % 7
}

impl Calendar {
    pub fn new() -> Calendar {
        let now = time::from_epoch(time::now());
        let mut c = Calendar {
            year: now.year,
            month: now.month,
            selected: (now.year, now.month, now.day),
            events: Vec::new(),
            input: String::new(),
            hover: None,
            blink: true,
            ticks: 0,
        };
        c.load();
        c
    }

    fn load(&mut self) {
        let Ok(bytes) = fs::abs(FILE).and_then(|p| fs::read_file(&p)) else { return };
        for line in String::from_utf8_lossy(&bytes).lines() {
            let mut it = line.splitn(3, '\t');
            let (Some(date), Some(t), Some(text)) = (it.next(), it.next(), it.next()) else { continue };
            let mut d = date.split('-').filter_map(|p| p.parse::<u64>().ok());
            let (Some(y), Some(m), Some(day)) = (d.next(), d.next(), d.next()) else { continue };
            self.events.push(Event { date: (y, m, day), time: String::from(t), text: String::from(text) });
        }
    }

    fn save(&self) {
        let mut out = String::new();
        for e in &self.events {
            out.push_str(&format!("{:04}-{:02}-{:02}\t{}\t{}\n", e.date.0, e.date.1, e.date.2, e.time, e.text));
        }
        if let Ok(p) = fs::abs(FILE) {
            let _ = fs::write_file(&p, out.as_bytes());
        }
    }

    fn events_on(&self, date: (u64, u64, u64)) -> Vec<(usize, &Event)> {
        let mut v: Vec<(usize, &Event)> = self.events.iter().enumerate().filter(|(_, e)| e.date == date).collect();
        v.sort_by(|a, b| a.1.time.cmp(&b.1.time));
        v
    }

    fn grid(area: Rect) -> Rect {
        Rect::new(area.x + 16, area.y + 66, area.w - SIDE - 30, area.h - 80)
    }

    fn cell(area: Rect, slot: i32) -> Rect {
        let g = Self::grid(area);
        let (cw, ch) = (g.w / 7, (g.h - 26) / 6);
        Rect::new(g.x + (slot % 7) * cw, g.y + 26 + (slot / 7) * ch, cw, ch)
    }

    fn nav(area: Rect) -> [Rect; 3] {
        let x = area.x + area.w - SIDE - 14 - 170;
        let y = area.y + 18;
        [Rect::new(x, y, 32, 32), Rect::new(x + 40, y, 88, 32), Rect::new(x + 136, y, 32, 32)]
    }

    fn input_rect(area: Rect) -> Rect {
        Rect::new(area.right() - SIDE, area.bottom() - 58, SIDE - 16, 42)
    }

    fn first_slot(&self) -> i32 {
        weekday(self.year, self.month, 1) as i32
    }

    fn shift(&mut self, months: i64) {
        let total = self.year as i64 * 12 + self.month as i64 - 1 + months;
        self.year = (total / 12) as u64;
        self.month = (total % 12) as u64 + 1;
    }

    fn add(&mut self) {
        let t = self.input.trim();
        if t.is_empty() {
            return;
        }
        // "14:30 Dentista": a time in front.
        let (time, text) = match t.split_once(' ') {
            Some((h, rest)) if h.len() <= 5 && h.contains(':') && h.chars().all(|c| c.is_ascii_digit() || c == ':') => {
                let (hh, mm) = h.split_once(':').unwrap_or((h, "0"));
                (format!("{:02}:{:02}", hh.parse::<u32>().unwrap_or(0).min(23), mm.parse::<u32>().unwrap_or(0).min(59)), String::from(rest.trim()))
            }
            _ => (String::new(), String::from(t)),
        };
        self.events.push(Event { date: self.selected, time, text });
        self.input.clear();
        self.save();
    }
}

impl App for Calendar {
    fn title(&self) -> String {
        String::from("Calendario")
    }

    fn icon(&self) -> &str {
        "calendar"
    }

    fn kind(&self) -> &'static str {
        "calendar"
    }

    fn size(&self) -> (i32, i32) {
        (860, 540)
    }

    fn resizable(&self) -> bool {
        true
    }

    fn draw(&mut self, c: &mut Canvas, area: Rect, _blur: &Canvas, focused: bool) {
        let month = MONTHS[(self.month as usize + 11) % 12];
        let mut title = String::from(month);
        if let Some(f) = title.get_mut(0..1) {
            f.make_ascii_uppercase();
        }
        let w = text::draw(c, Font::Title, area.x + 20, area.y + 14, &title, TEXT);
        text::draw(c, Font::Title, area.x + 30 + w, area.y + 14, &format!("{}", self.year), TEXT_DIM);
        let [prev, today, next] = Self::nav(area);
        ui::icon_button(c, prev, "g-back", self.hover == Some(prev));
        ui::button(c, today, "Oggi", false, self.hover == Some(today));
        ui::icon_button(c, next, "g-forward", self.hover == Some(next));
        let g = Self::grid(area);
        let cw = g.w / 7;
        for (i, d) in DAYS.iter().enumerate() {
            text::draw_right(c, Font::Small, g.x + (i as i32 + 1) * cw - 10, g.y + 4, d, TEXT_FAINT);
        }
        let now = time::from_epoch(time::now());
        let first = self.first_slot();
        let days = time::days_in_month(self.year, self.month) as i32;
        for slot in 0..42 {
            let r = Self::cell(area, slot);
            c.fill(Rect::new(r.x, r.y, r.w, 1), ui::SEPARATOR);
            let day = slot - first + 1;
            if day < 1 || day > days {
                continue;
            }
            let date = (self.year, self.month, day as u64);
            if self.selected == date {
                c.fill_round(r.grow(-2), 10, ui::PANEL_HOVER);
            }
            let label = format!("{day}");
            let is_today = (now.year, now.month, now.day) == date;
            let hol = holiday(date.0, date.1, date.2);
            let (lx, ly) = (r.right() - 12, r.y + 6);
            if is_today {
                let lw = text::width(Font::Strong, &label);
                c.fill_circle(lx - lw / 2, ly + 10, 13, ui::ORANGE);
                text::draw_right(c, Font::Strong, lx, ly, &label, 0xFFFF_FFFF);
            } else {
                let color = if hol.is_some() || slot % 7 == 6 { 0xFFFF_6961 } else if slot % 7 == 5 { TEXT_DIM } else { TEXT };
                text::draw_right(c, Font::Strong, lx, ly, &label, color);
            }
            let mut y = r.y + 30;
            if let Some(h) = hol {
                if r.h > 50 {
                    text::draw(c, Font::Small, r.x + 6, y, &text::fit(Font::Small, h, r.w - 10), 0xFFFF_6961);
                    y += 16;
                }
            }
            for (_, e) in self.events_on(date) {
                if y + 14 > r.bottom() {
                    break;
                }
                c.fill_round(Rect::new(r.x + 4, y, r.w - 8, 16), 4, 0x660A_84FF);
                text::draw(c, Font::Small, r.x + 8, y, &text::fit(Font::Small, &e.text, r.w - 14), TEXT);
                y += 18;
            }
        }
        // The selected day.
        let side = Rect::new(area.right() - SIDE - 6, area.y + 6, SIDE, area.h - 12);
        c.fill_round(side, 18, 0x22_00_00_00);
        let (y, m, d) = self.selected;
        text::draw(c, Font::Body, side.x + 16, side.y + 16, WEEKDAYS[weekday(y, m, d)], ui::ORANGE);
        text::draw(c, Font::Title, side.x + 16, side.y + 36, &format!("{d} {}", MONTHS[(m as usize + 11) % 12]), TEXT);
        let mut ey = side.y + 92;
        if let Some(h) = holiday(y, m, d) {
            text::draw(c, Font::Strong, side.x + 16, ey, h, 0xFFFF_6961);
            ey += 30;
        }
        let list = self.events_on(self.selected);
        if list.is_empty() && holiday(y, m, d).is_none() {
            text::draw(c, Font::Body, side.x + 16, ey, "Nessun evento", TEXT_FAINT);
        }
        for (_, e) in list {
            if ey > side.bottom() - 110 {
                break;
            }
            c.fill_round(Rect::new(side.x + 14, ey, 4, 36), 2, ui::ACCENT);
            text::draw(c, Font::Strong, side.x + 26, ey, &text::fit(Font::Strong, &e.text, side.w - 40), TEXT);
            let when = if e.time.is_empty() { String::from("tutto il giorno") } else { e.time.clone() };
            text::draw(c, Font::Small, side.x + 26, ey + 20, &when, TEXT_DIM);
            ey += 48;
        }
        let ir = Self::input_rect(area);
        c.fill_round(ir, 12, 0x26FF_FFFF);
        let (lh, _) = text::metrics(Font::Body);
        let ty = ir.y + (ir.h - lh) / 2;
        if self.input.is_empty() {
            text::draw(c, Font::Body, ir.x + 14, ty, "Nuovo evento (es. 9:30 Riunione)", TEXT_FAINT);
        }
        let w = text::draw(c, Font::Body, ir.x + 14, ty, &text::fit(Font::Body, &self.input, ir.w - 24), TEXT);
        if focused && self.blink {
            c.fill(Rect::new(ir.x + 15 + w, ty + 2, 2, lh - 4), ui::ACCENT);
        }
        text::draw(c, Font::Small, ir.x + 4, ir.y - 22, "Clic destro su un evento per toglierlo", TEXT_FAINT);
    }

    fn press(&mut self, x: i32, y: i32, double: bool, area: Rect) -> Action {
        let (ax, ay) = (area.x + x, area.y + y);
        let [prev, today, next] = Self::nav(area);
        if prev.contains(ax, ay) {
            self.shift(-1);
            return Action::Redraw;
        }
        if next.contains(ax, ay) {
            self.shift(1);
            return Action::Redraw;
        }
        if today.contains(ax, ay) {
            let now = time::from_epoch(time::now());
            self.year = now.year;
            self.month = now.month;
            self.selected = (now.year, now.month, now.day);
            return Action::Redraw;
        }
        let first = self.first_slot();
        let days = time::days_in_month(self.year, self.month) as i32;
        for slot in 0..42 {
            let day = slot - first + 1;
            if day >= 1 && day <= days && Self::cell(area, slot).contains(ax, ay) {
                self.selected = (self.year, self.month, day as u64);
                let _ = double;
                return Action::Redraw;
            }
        }
        Action::None
    }

    fn menu(&mut self, x: i32, y: i32, area: Rect) -> Action {
        // Right click in the day panel: remove the event under the pointer.
        let side_x = area.right() - SIDE - 6;
        if area.x + x < side_x {
            return Action::None;
        }
        let mut ey = area.y + 6 + 92;
        let (yy, m, d) = self.selected;
        if holiday(yy, m, d).is_some() {
            ey += 30;
        }
        let list: Vec<usize> = self.events_on(self.selected).iter().map(|(i, _)| *i).collect();
        for i in list {
            if (ey..ey + 44).contains(&(area.y + y)) {
                self.events.remove(i);
                self.save();
                return Action::Redraw;
            }
            ey += 48;
        }
        Action::None
    }

    fn hover(&mut self, at: Option<(i32, i32)>, area: Rect) -> bool {
        let h = at.and_then(|(x, y)| Self::nav(area).into_iter().find(|r| r.contains(area.x + x, area.y + y)));
        let changed = h != self.hover;
        self.hover = h;
        changed
    }

    fn key(&mut self, ev: KeyEvent) -> Action {
        self.blink = true;
        match ev.key {
            Key::Enter => self.add(),
            Key::PageUp => self.shift(-1),
            Key::PageDown => self.shift(1),
            _ => {
                if !ui::edit_text(&mut self.input, &ev, 80) {
                    return Action::None;
                }
            }
        }
        Action::Redraw
    }

    fn scroll(&mut self, lines: i32) -> bool {
        self.shift(lines.signum() as i64);
        true
    }

    fn tick(&mut self) -> bool {
        self.ticks += 1;
        if self.ticks.is_multiple_of(5) {
            self.blink = !self.blink;
            return true;
        }
        false
    }
}

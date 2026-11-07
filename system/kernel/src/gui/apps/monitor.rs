//! Monitor attività: what is running and how busy the processor and the
//! memory are, with a minute of history. Programs can be stopped from here.

use crate::gui::gfx::{Canvas, Rect};
use crate::gui::text::{self, Font};
use crate::gui::ui::{self, TEXT, TEXT_DIM, TEXT_FAINT};
use crate::gui::wm::{Action, App};
use crate::time;
use crate::util::Size;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const HISTORY: usize = 60;
const ROW: i32 = 30;

struct Row {
    name: String,
    /// Share of the processor in the last second, in 1/1000.
    cpu: u32,
    threads: usize,
    memory: Option<u64>,
    /// A program that can be stopped.
    pid: Option<u32>,
}

pub struct Monitor {
    tab: usize,
    rows: Vec<Row>,
    cpu_history: Vec<u32>,
    mem_history: Vec<u32>,
    /// CPU time of each thread at the last sample, and when it was.
    last: BTreeMap<u32, u64>,
    last_at: u64,
    selected: Option<usize>,
    scroll: i32,
}

impl Monitor {
    pub fn new() -> Monitor {
        let mut m =
            Monitor { tab: 0, rows: Vec::new(), cpu_history: Vec::new(), mem_history: Vec::new(), last: BTreeMap::new(), last_at: 0, selected: None, scroll: 0 };
        m.sample();
        m
    }

    fn sample(&mut self) {
        let now = time::uptime_ms();
        // Processor time available: the interval on every processor.
        let span = now.saturating_sub(self.last_at).max(1) * crate::arch::smp::count() as u64;
        let threads = crate::task::list();
        let mut busy = 0u64;
        // Group the threads: one row per program, one for the system.
        let mut groups: BTreeMap<u32, (String, u64, usize)> = BTreeMap::new();
        let mut seen = BTreeMap::new();
        for t in &threads {
            let before = self.last.get(&(t.tid as u32)).copied().unwrap_or(t.cpu_ms);
            let used = t.cpu_ms.saturating_sub(before);
            seen.insert(t.tid as u32, t.cpu_ms);
            if t.name == "idle" {
                continue;
            }
            busy += used;
            let e = groups.entry(t.pid).or_insert_with(|| (if t.pid == 0 { String::from("justOS (sistema)") } else { t.name.clone() }, 0, 0));
            e.1 += used;
            e.2 += 1;
        }
        let first = self.last.is_empty();
        self.last = seen;
        self.last_at = now;
        let procs = crate::user::processes();
        let selected_pid = self.selected.and_then(|i| self.rows.get(i)).and_then(|r| r.pid);
        let mut rows: Vec<Row> = groups
            .into_iter()
            .map(|(pid, (name, used, n))| {
                let p = procs.iter().find(|p| p.pid == pid);
                Row {
                    name: p.map_or(name, |p| p.name.clone()),
                    cpu: if first { 0 } else { (used * 1000 / span).min(1000) as u32 },
                    threads: n,
                    memory: p.map(|p| p.memory),
                    pid: (pid != 0).then_some(pid),
                }
            })
            .collect();
        rows.sort_by(|a, b| b.cpu.cmp(&a.cpu).then_with(|| a.name.cmp(&b.name)));
        self.selected = selected_pid.and_then(|pid| rows.iter().position(|r| r.pid == Some(pid)));
        self.rows = rows;
        if !first {
            push(&mut self.cpu_history, (busy * 1000 / span).min(1000) as u32);
        }
        let (free, total) = crate::mem::frame::stats();
        push(&mut self.mem_history, (total.saturating_sub(free) * 1000 / total.max(1)) as u32);
    }

    fn tabs(area: Rect) -> Rect {
        Rect::new(area.x + area.w / 2 - 110, area.y + 14, 220, 32)
    }

    fn table(area: Rect) -> Rect {
        Rect::new(area.x + 12, area.y + 92, area.w - 24, area.h - 92 - 150)
    }

    fn graph(area: Rect) -> Rect {
        Rect::new(area.x + 12, area.bottom() - 140, area.w - 300, 128)
    }

    fn stop_btn(area: Rect) -> Rect {
        Rect::new(area.right() - 140, area.y + 14, 124, 32)
    }
}

fn push(v: &mut Vec<u32>, x: u32) {
    if v.len() >= HISTORY {
        v.remove(0);
    }
    v.push(x);
}

fn draw_graph(c: &mut Canvas, g: Rect, data: &[u32], color: u32) {
    c.fill_round(g, 12, 0x33_00_00_00);
    for k in 1..4 {
        c.fill(Rect::new(g.x + 8, g.y + g.h * k / 4, g.w - 16, 1), 0x14FF_FFFF);
    }
    let inner = g.grow(-8);
    let n = HISTORY as i32;
    let bar = (inner.w / n).max(1);
    for (i, &v) in data.iter().enumerate() {
        let x = inner.right() - (data.len() as i32 - i as i32) * bar;
        let h = (inner.h * v as i32 / 1000).max(1);
        c.fill(Rect::new(x, inner.bottom() - h, bar - 1, h), color);
    }
}

impl App for Monitor {
    fn title(&self) -> String {
        String::from("Monitor attività")
    }

    fn icon(&self) -> &str {
        "activity"
    }

    fn kind(&self) -> &'static str {
        "monitor"
    }

    fn size(&self) -> (i32, i32) {
        (720, 520)
    }

    fn resizable(&self) -> bool {
        true
    }

    fn draw(&mut self, c: &mut Canvas, area: Rect, _blur: &Canvas, _focused: bool) {
        ui::segmented(c, Self::tabs(area), &["CPU", "Memoria"], self.tab);
        let stop = Self::stop_btn(area);
        let can_stop = self.selected.and_then(|i| self.rows.get(i)).is_some_and(|r| r.pid.is_some());
        if can_stop {
            ui::button_color(c, stop, "Termina", 0x66FF_453A, 0xFFFF_6961);
        }
        let t = Self::table(area);
        let cols = [t.x + 14, t.right() - 300, t.right() - 190, t.right() - 90];
        let heads = ["Nome", if self.tab == 0 { "% CPU" } else { "Memoria" }, "Thread", "PID"];
        for (x, h) in cols.iter().zip(heads) {
            text::draw(c, Font::Small, *x, area.y + 66, h, TEXT_FAINT);
        }
        c.fill(Rect::new(t.x, t.y - 4, t.w, 1), ui::SEPARATOR);
        let saved = c.clip();
        c.set_clip(saved.intersect(&t));
        let mut order: Vec<usize> = (0..self.rows.len()).collect();
        if self.tab == 1 {
            order.sort_by(|&a, &b| self.rows[b].memory.cmp(&self.rows[a].memory));
        }
        for (k, &i) in order.iter().enumerate() {
            let r = &self.rows[i];
            let y = t.y + k as i32 * ROW - self.scroll;
            let row = Rect::new(t.x, y, t.w, ROW);
            if self.selected == Some(i) {
                c.fill_round(row, 8, ui::SELECTED);
            } else if k % 2 == 1 {
                c.fill(row, 0x0CFF_FFFF);
            }
            text::draw(c, Font::Body, cols[0], y + 5, &text::fit(Font::Body, &r.name, cols[1] - cols[0] - 20), TEXT);
            let v = if self.tab == 0 {
                format!("{},{}", r.cpu / 10, r.cpu % 10)
            } else {
                r.memory.map_or(String::from("—"), |m| format!("{}", Size(m)))
            };
            text::draw(c, Font::Body, cols[1], y + 5, &v, TEXT);
            text::draw(c, Font::Body, cols[2], y + 5, &format!("{}", r.threads), TEXT_DIM);
            text::draw(c, Font::Body, cols[3], y + 5, &r.pid.map_or(String::from("—"), |p| format!("{p}")), TEXT_DIM);
        }
        c.set_clip(saved);
        // History and totals.
        let g = Self::graph(area);
        let side = Rect::new(g.right() + 12, g.y, area.right() - 12 - g.right() - 12, g.h);
        c.fill_round(side, 12, 0x33_00_00_00);
        if self.tab == 0 {
            draw_graph(c, g, &self.cpu_history, ui::GREEN);
            let now = self.cpu_history.last().copied().unwrap_or(0);
            text::draw(c, Font::Small, side.x + 16, side.y + 14, "Utilizzo", TEXT_DIM);
            text::draw(c, Font::Title, side.x + 16, side.y + 32, &format!("{},{} %", now / 10, now % 10), TEXT);
            let threads: usize = self.rows.iter().map(|r| r.threads).sum();
            text::draw(c, Font::Small, side.x + 16, side.y + 78, &format!("{} thread · {} programmi", threads, crate::user::count()), TEXT_DIM);
            let up = time::uptime_ms() / 1000;
            text::draw(c, Font::Small, side.x + 16, side.y + 98, &format!("Acceso da {} h {:02} min", up / 3600, up / 60 % 60), TEXT_DIM);
        } else {
            draw_graph(c, g, &self.mem_history, ui::ACCENT);
            let (free, total) = crate::mem::frame::stats();
            text::draw(c, Font::Small, side.x + 16, side.y + 14, "In uso", TEXT_DIM);
            text::draw(c, Font::Title, side.x + 16, side.y + 32, &format!("{}", Size(total.saturating_sub(free))), TEXT);
            text::draw(c, Font::Small, side.x + 16, side.y + 78, &format!("Libera: {}", Size(free)), TEXT_DIM);
            text::draw(c, Font::Small, side.x + 16, side.y + 98, &format!("Totale: {}", Size(total)), TEXT_DIM);
        }
    }

    fn press(&mut self, x: i32, y: i32, _double: bool, area: Rect) -> Action {
        let (ax, ay) = (area.x + x, area.y + y);
        for (i, s) in ui::segments(Self::tabs(area), 2).iter().enumerate() {
            if s.contains(ax, ay) {
                self.tab = i;
                return Action::Redraw;
            }
        }
        if Self::stop_btn(area).contains(ax, ay) {
            if let Some(pid) = self.selected.and_then(|i| self.rows.get(i)).and_then(|r| r.pid) {
                crate::user::kill(pid);
                self.selected = None;
            }
            return Action::Redraw;
        }
        let t = Self::table(area);
        if t.contains(ax, ay) {
            let k = ((ay - t.y + self.scroll) / ROW) as usize;
            let mut order: Vec<usize> = (0..self.rows.len()).collect();
            if self.tab == 1 {
                order.sort_by(|&a, &b| self.rows[b].memory.cmp(&self.rows[a].memory));
            }
            self.selected = order.get(k).copied();
            return Action::Redraw;
        }
        Action::None
    }

    fn scroll(&mut self, lines: i32) -> bool {
        let old = self.scroll;
        self.scroll = (self.scroll + lines * ROW).clamp(0, (self.rows.len() as i32 * ROW - 200).max(0));
        old != self.scroll
    }

    fn tick(&mut self) -> bool {
        if time::uptime_ms() - self.last_at >= 1000 {
            self.sample();
            return true;
        }
        false
    }
}

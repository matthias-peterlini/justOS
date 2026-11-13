//! Disegno: draw with the mouse. Pencil, brush, eraser, lines, rectangles,
//! circles and the paint bucket; a palette, three sizes, undo, and saving to
//! the Immagini folder (QOI pictures, which Foto and Anteprima open), with
//! the user's permission; Ctrl+Maiusc+S saves and Ctrl+O opens through the
//! system's file picker.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use justrt::Error;
use justui::{qoi, ui, Canvas, Event, Font, Key, Rect, Window};

justrt::main!(main);

const PICTURES: &str = "sys:/home/Immagini";

const W: i32 = 880;
const H: i32 = 540;
const BAR: i32 = 58;
const SIDE: i32 = 64;
const UNDO: usize = 12;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tool {
    Pencil,
    Brush,
    Eraser,
    Line,
    Rect,
    Circle,
    Bucket,
}

const TOOLS: [(Tool, &str); 7] = [
    (Tool::Pencil, "Matita"),
    (Tool::Brush, "Pennello"),
    (Tool::Eraser, "Gomma"),
    (Tool::Line, "Linea"),
    (Tool::Rect, "Rettangolo"),
    (Tool::Circle, "Cerchio"),
    (Tool::Bucket, "Riempi"),
];

const COLORS: [u32; 14] = [
    0xFF1C_1C1E, 0xFF8E_8E93, 0xFFFF_FFFF, 0xFFFF_3B30, 0xFFFF_9500, 0xFFFF_CC00, 0xFF34_C759, 0xFF00_C7BE, 0xFF30_B0C7, 0xFF00_7AFF, 0xFF58_56D6,
    0xFFAF_52DE, 0xFFFF_2D55, 0xFFA2_845E,
];

const SIZES: [i32; 3] = [2, 6, 14];

struct App {
    paper: Canvas,
    tool: Tool,
    color: u32,
    size: usize,
    undo: Vec<Vec<u32>>,
    /// While drawing: the last point, and where a shape started.
    last: Option<(i32, i32)>,
    start: Option<(i32, i32)>,
    /// The picture before a shape (the shape is redrawn as the mouse moves).
    before: Option<Vec<u32>>,
    note: String,
    note_until: u64,
    hover: Option<Rect>,
    saved_as: Option<String>,
}

fn paper_rect() -> Rect {
    Rect::new(SIDE + 8, BAR + 8, W - SIDE - 24, H - BAR - 24)
}

fn tool_rect(i: usize) -> Rect {
    Rect::new(10, BAR + 8 + i as i32 * 54, 46, 46)
}

fn color_rect(i: usize) -> Rect {
    Rect::new(16 + i as i32 * 32, 14, 26, 26)
}

fn size_rect(i: usize) -> Rect {
    Rect::new(16 + COLORS.len() as i32 * 32 + 16 + i as i32 * 34, 12, 30, 30)
}

fn buttons() -> [Rect; 3] {
    [Rect::new(W - 300, 12, 88, 32), Rect::new(W - 204, 12, 88, 32), Rect::new(W - 108, 12, 92, 32)]
}

/// Draws the tool's little picture in `fg`.
fn tool_icon(c: &mut Canvas, t: Tool, r: Rect, color: u32, fg: u32) {
    let (cx, cy) = (r.x + r.w / 2, r.y + r.h / 2);
    match t {
        Tool::Pencil => {
            c.line(cx - 10, cy + 10, cx + 8, cy - 8, 4, fg);
            c.fill_circle(cx - 11, cy + 11, 2, color);
        }
        Tool::Brush => {
            c.line(cx + 2, cy - 2, cx + 10, cy - 10, 3, fg);
            c.fill_circle(cx - 4, cy + 4, 7, color);
        }
        Tool::Eraser => {
            c.fill_round(Rect::new(cx - 11, cy - 7, 22, 14), 4, fg);
            c.fill_round(Rect::new(cx - 11, cy - 7, 9, 14), 4, 0xFFFF_6B8A);
        }
        Tool::Line => c.line(cx - 11, cy + 11, cx + 11, cy - 11, 3, fg),
        Tool::Rect => c.stroke_round(Rect::new(cx - 11, cy - 8, 22, 16), 2, 2, fg),
        Tool::Circle => c.arc(cx, cy, 11, 2, 1000, fg),
        Tool::Bucket => {
            c.fill_round(Rect::new(cx - 9, cy - 6, 16, 16), 3, fg);
            c.fill_circle(cx + 10, cy + 8, 3, color);
        }
    }
}

impl App {
    fn new() -> App {
        let r = paper_rect();
        App {
            paper: Canvas::new(r.w, r.h, 0xFFFF_FFFF),
            tool: Tool::Brush,
            color: COLORS[9],
            size: 1,
            undo: Vec::new(),
            last: None,
            start: None,
            before: None,
            note: String::new(),
            note_until: 0,
            hover: None,
            saved_as: None,
        }
    }

    fn remember(&mut self) {
        if self.undo.len() >= UNDO {
            self.undo.remove(0);
        }
        self.undo.push(self.paper.px.clone());
    }

    fn width(&self) -> i32 {
        match self.tool {
            Tool::Pencil => SIZES[self.size] / 2 + 1,
            Tool::Eraser => SIZES[self.size] * 2,
            _ => SIZES[self.size],
        }
    }

    fn ink(&self) -> u32 {
        if self.tool == Tool::Eraser {
            0xFFFF_FFFF
        } else {
            self.color
        }
    }

    fn stroke(&mut self, a: (i32, i32), b: (i32, i32)) {
        let w = self.width();
        let ink = self.ink();
        self.paper.line(a.0, a.1, b.0, b.1, w, ink);
    }

    fn shape(&mut self, a: (i32, i32), b: (i32, i32)) {
        if let Some(before) = &self.before {
            self.paper.px.copy_from_slice(before);
        }
        let w = self.width();
        let ink = self.color;
        match self.tool {
            Tool::Line => self.paper.line(a.0, a.1, b.0, b.1, w, ink),
            Tool::Rect => {
                let r = Rect::new(a.0.min(b.0), a.1.min(b.1), (a.0 - b.0).abs() + 1, (a.1 - b.1).abs() + 1);
                self.paper.stroke_round(r, 2, w.max(1), ink);
            }
            Tool::Circle => {
                let (dx, dy) = (b.0 - a.0, b.1 - a.1);
                let rad = justui::gfx::isqrt((dx * dx + dy * dy) as u32) as i32;
                self.paper.arc(a.0, a.1, rad, w.max(1), 1000, ink);
            }
            _ => {}
        }
    }

    /// Fills the area of one colour around (x, y).
    fn bucket(&mut self, x: i32, y: i32) {
        let (w, h) = (self.paper.w, self.paper.h);
        let target = self.paper.get(x, y);
        let ink = self.color;
        if target == ink {
            return;
        }
        let close = |p: u32| {
            let d = |s: u32| ((p >> s) & 0xFF).abs_diff((target >> s) & 0xFF);
            d(16) + d(8) + d(0) < 60
        };
        let mut seen = vec![false; (w * h) as usize];
        let mut stack = vec![(x, y)];
        while let Some((x, y)) = stack.pop() {
            // Scan left and right along the row.
            let mut l = x;
            while l > 0 && !seen[(y * w + l - 1) as usize] && close(self.paper.get(l - 1, y)) {
                l -= 1;
            }
            let mut r = x;
            while r < w - 1 && !seen[(y * w + r + 1) as usize] && close(self.paper.get(r + 1, y)) {
                r += 1;
            }
            for i in l..=r {
                let k = (y * w + i) as usize;
                seen[k] = true;
                self.paper.px[k] = ink;
                for ny in [y - 1, y + 1] {
                    if ny >= 0 && ny < h && !seen[(ny * w + i) as usize] && close(self.paper.get(i, ny)) {
                        stack.push((i, ny));
                    }
                }
            }
        }
    }

    /// A new name in Immagini; None if Disegno may not use the folder (the
    /// system asks the user the first time).
    fn next_in_pictures() -> Option<String> {
        if justrt::fs::make_dir(PICTURES) == Err(Error::Denied) {
            return None;
        }
        let mut n = 1;
        loop {
            let p = format!("{PICTURES}/Disegno {n}.qoi");
            match justrt::fs::File::open(&p) {
                Ok(_) => n += 1,
                Err(Error::Denied) => return None,
                Err(_) => return Some(p),
            }
        }
    }

    /// Saves where it was saved before, else in Immagini, else (or with
    /// `choose`) where the user says in the system's file picker.
    fn save(&mut self, choose: bool) {
        let path = match &self.saved_as {
            Some(p) if !choose => Some(p.clone()),
            _ if choose => None,
            _ => Self::next_in_pictures(),
        };
        let path = match path {
            Some(p) => p,
            None => match justrt::app::pick_save("Disegno.qoi") {
                Ok(p) => p,
                Err(_) => return,
            },
        };
        let data = qoi::encode(&self.paper);
        self.note = match justrt::fs::write(&path, &data) {
            Ok(()) => {
                let name = String::from(path.rsplit('/').next().unwrap_or(&path));
                let place = if path.starts_with(PICTURES) { " in Immagini" } else { "" };
                self.saved_as = Some(path);
                format!("Salvato{place} come «{name}»")
            }
            Err(e) => format!("Non salvato: {}", e.as_str()),
        };
        self.note_until = justui::now_ms() + 3000;
    }

    /// Opens a picture the user chooses in the system's file picker.
    fn open(&mut self) {
        let Ok(path) = justrt::app::pick_open() else { return };
        self.load(path, true);
    }

    /// Puts a picture on the paper. `own`: the user chose it in the picker,
    /// so saving goes back to it; a dropped one may only be read, so saving
    /// makes a new picture.
    fn load(&mut self, path: String, own: bool) {
        match justrt::fs::read(&path).ok().and_then(|d| qoi::decode(&d)) {
            Some(img) => {
                self.remember();
                self.paper.px.fill(0xFFFF_FFFF);
                let r = Rect::new(0, 0, img.w.min(self.paper.w), img.h.min(self.paper.h));
                self.paper.blit(&img, 0, 0, r);
                if !own {
                    let name = path.rsplit('/').next().unwrap_or(&path);
                    self.note = format!("Aperto «{name}»");
                    self.note_until = justui::now_ms() + 3000;
                }
                self.saved_as = own.then_some(path);
            }
            None => {
                self.note = String::from("Non è un'immagine QOI");
                self.note_until = justui::now_ms() + 3000;
            }
        }
    }

    fn draw(&self, win: &mut Window) {
        win.clear();
        let f = win.fonts;
        let c = &mut win.canvas;
        for (i, &col) in COLORS.iter().enumerate() {
            let r = color_rect(i);
            if col == self.color {
                c.fill_circle(r.x + 13, r.y + 13, 16, ui::text());
            }
            c.fill_circle(r.x + 13, r.y + 13, 12, col);
        }
        for (i, &s) in SIZES.iter().enumerate() {
            let r = size_rect(i);
            if i == self.size {
                ui::tile(c, r, 9, ui::knob_grad());
            } else {
                c.fill_round(r, 9, ui::cell());
            }
            c.fill_circle(r.x + 15, r.y + 15, (s / 2 + 1).min(9), ui::text());
        }
        let [undo, clear, save] = buttons();
        ui::button(c, f, undo, "Annulla", false, self.hover == Some(undo));
        ui::button(c, f, clear, "Pulisci", false, self.hover == Some(clear));
        ui::button(c, f, save, "Salva", true, self.hover == Some(save));
        for (i, (t, _)) in TOOLS.iter().enumerate() {
            let r = tool_rect(i);
            let chosen = *t == self.tool;
            if chosen {
                ui::tile(c, r, 12, ui::ORANGE_GRAD);
            } else {
                c.fill_round(r, 12, ui::cell());
                if self.hover == Some(r) {
                    c.fill_round(r, 12, ui::sel());
                }
            }
            tool_icon(c, *t, r, self.color, if chosen { ui::WHITE } else { ui::text() });
        }
        let p = paper_rect();
        c.shadow(p, 6, 10, 80);
        c.blit(&self.paper, 0, 0, p);
        if let Some(i) = (0..TOOLS.len()).find(|&i| Some(tool_rect(i)) == self.hover) {
            let r = tool_rect(i);
            let label = TOOLS[i].1;
            let w = f.width(Font::Small, label) + 16;
            let tip = Rect::new(r.right() + 8, r.y + 12, w, 24);
            c.fill_round(tip, 8, 0xE61E_1626);
            f.draw(c, Font::Small, tip.x + 8, tip.y + 3, label, ui::WHITE);
        }
        if !self.note.is_empty() && justui::now_ms() < self.note_until {
            let w = f.width(Font::Body, &self.note) + 32;
            let r = Rect::new(p.x + (p.w - w) / 2, p.bottom() - 52, w, 36);
            c.fill_round(r, 18, 0xE61E_1626);
            f.draw_centered(c, Font::Body, r.x + w / 2, r.y + 8, &self.note, ui::WHITE);
        }
    }

    fn to_paper(x: i32, y: i32) -> (i32, i32) {
        let p = paper_rect();
        (x - p.x, y - p.y)
    }
}

fn main(_args: &str) -> i32 {
    let Ok(mut win) = Window::with_flags("Disegno", W, H, justui::DROPS) else { return 1 };
    let mut app = App::new();
    let mut dirty = true;
    loop {
        if dirty {
            app.draw(&mut win);
            win.present();
        }
        let waiting_note = !app.note.is_empty() && justui::now_ms() < app.note_until;
        let ev = if waiting_note { win.event(app.note_until - justui::now_ms()) } else { Some(win.wait()) };
        dirty = true;
        let Some(ev) = ev else {
            app.note.clear();
            continue;
        };
        match ev {
            Event::Close => return 0,
            Event::Press { x, y, .. } => {
                if let Some(i) = (0..COLORS.len()).find(|&i| color_rect(i).grow(3).contains(x, y)) {
                    app.color = COLORS[i];
                    if app.tool == Tool::Eraser {
                        app.tool = Tool::Brush;
                    }
                } else if let Some(i) = (0..SIZES.len()).find(|&i| size_rect(i).contains(x, y)) {
                    app.size = i;
                } else if let Some(i) = (0..TOOLS.len()).find(|&i| tool_rect(i).contains(x, y)) {
                    app.tool = TOOLS[i].0;
                } else if buttons()[0].contains(x, y) {
                    if let Some(px) = app.undo.pop() {
                        app.paper.px = px;
                    }
                } else if buttons()[1].contains(x, y) {
                    app.remember();
                    app.paper.px.fill(0xFFFF_FFFF);
                } else if buttons()[2].contains(x, y) {
                    app.save(false);
                } else if paper_rect().contains(x, y) {
                    let at = App::to_paper(x, y);
                    app.remember();
                    match app.tool {
                        Tool::Bucket => app.bucket(at.0, at.1),
                        Tool::Line | Tool::Rect | Tool::Circle => {
                            app.start = Some(at);
                            app.before = Some(app.paper.px.clone());
                        }
                        _ => {
                            app.stroke(at, at);
                            app.last = Some(at);
                        }
                    }
                } else {
                    dirty = false;
                }
            }
            Event::Move { x, y, held } => {
                let at = App::to_paper(x, y);
                if held {
                    if let Some(last) = app.last {
                        app.stroke(last, at);
                        app.last = Some(at);
                    } else if let Some(s) = app.start {
                        app.shape(s, at);
                    } else {
                        dirty = false;
                    }
                } else {
                    let mut targets: Vec<Rect> = (0..TOOLS.len()).map(tool_rect).collect();
                    targets.extend(buttons());
                    let h = targets.into_iter().find(|r| r.contains(x, y));
                    dirty = h != app.hover;
                    app.hover = h;
                }
            }
            Event::Release => {
                app.last = None;
                app.start = None;
                app.before = None;
            }
            Event::Key(k) => {
                if k.ctrl && matches!(k.key, Key::Char('z')) {
                    if let Some(px) = app.undo.pop() {
                        app.paper.px = px;
                    }
                } else if k.ctrl && matches!(k.key, Key::Char('s' | 'S')) {
                    app.save(k.shift);
                } else if k.ctrl && matches!(k.key, Key::Char('o' | 'O')) {
                    app.open();
                } else {
                    dirty = false;
                }
            }
            // A picture dragged from File onto the window.
            Event::Drop { .. } => match win.dropped().into_iter().find(|p| p.to_ascii_lowercase().ends_with(".qoi")) {
                Some(path) => app.load(path, false),
                None => {
                    app.note = String::from("Disegno apre le immagini QOI");
                    app.note_until = justui::now_ms() + 3000;
                }
            },
            _ => dirty = false,
        }
    }
}

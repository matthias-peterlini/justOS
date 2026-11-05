//! ticly AI, in the look of ai.ticly.it: chats kept in
//! `sys:/home/.ticly/ai`, a choice of models, answers that stream in as they
//! are written. No model is connected yet, so every question gets a note
//! saying so (see [`ticly::chat`]).

use super::ticly::{self as tui, Login};
use crate::dev::input::{Key, KeyEvent};
use crate::fs;
use crate::gui::gfx::{Canvas, Rect};
use crate::gui::text::{self, Font};
use crate::gui::ui::{self, TEXT, TEXT_DIM, TEXT_FAINT};
use crate::gui::wm::{Action, App};
use crate::sync::IrqLock;
use crate::ticly::{self, Account, Model};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use justjson::Value;

const SIDEBAR: i32 = 286;
const DIR: &str = "sys:/home/.ticly/ai";
const PURPLE: u32 = 0xFF8C_52FF;

#[derive(Clone)]
struct Chat {
    id: String,
    title: String,
    time: u64,
    model: String,
    messages: Vec<(String, String)>,
}

impl Chat {
    fn to_json(&self) -> String {
        let msgs: Vec<Value> = self.messages.iter().map(|(r, t)| Value::object(alloc::vec![("role", Value::string(r)), ("content", Value::string(t))])).collect();
        Value::object(alloc::vec![
            ("title", Value::string(&self.title)),
            ("time", Value::Num(self.time as f64)),
            ("model", Value::string(&self.model)),
            ("messages", Value::Arr(msgs)),
        ])
        .to_string()
    }

    fn save(&self) {
        let _ = ensure_dir();
        if let Ok(p) = fs::abs(&format!("{DIR}/{}.json", self.id)) {
            let _ = fs::write_file(&p, self.to_json().as_bytes());
        }
    }
}

fn ensure_dir() -> Result<(), fs::Error> {
    for d in ["sys:/home/.ticly", DIR] {
        let p = fs::abs(d)?;
        if !fs::exists(&p) {
            fs::make_dir(&p)?;
        }
    }
    Ok(())
}

fn load_chats() -> Vec<Chat> {
    let Ok(dir) = fs::abs(DIR) else { return Vec::new() };
    let mut v: Vec<Chat> = fs::list_dir(&dir)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(e, _)| {
            let id = e.name.strip_suffix(".json")?.to_string();
            let b = fs::read_file(&dir.join(&e.name)).ok()?;
            let j = justjson::parse_bytes(&b)?;
            Some(Chat {
                id,
                title: j.get("title").str().to_string(),
                time: j.get("time").as_f64().unwrap_or(0.0) as u64,
                model: j.get("model").str().to_string(),
                messages: j.get("messages").as_array().iter().map(|m| (m.get("role").str().to_string(), m.get("content").str().to_string())).collect(),
            })
        })
        .collect();
    v.sort_by(|a, b| b.time.cmp(&a.time));
    v
}

/// What the answering thread hands back.
#[derive(Default)]
struct Stream {
    text: String,
    done: bool,
    error: Option<String>,
    thinking: bool,
}

pub struct TiclyAi {
    account: Option<Account>,
    login: Login,
    chats: Vec<Chat>,
    current: Option<usize>,
    input: String,
    search: String,
    searching: bool,
    model: Model,
    models: Vec<Model>,
    model_menu: bool,
    stream: Option<Arc<IrqLock<Stream>>>,
    scroll: i32,
    content_h: i32,
    hover: Option<Rect>,
}

impl TiclyAi {
    pub fn new() -> TiclyAi {
        let models = ticly::default_models();
        let t = TiclyAi {
            account: ticly::account(),
            login: Login::new(),
            chats: load_chats(),
            current: None,
            input: String::new(),
            search: String::new(),
            searching: false,
            model: models[0].clone(),
            models,
            model_menu: false,
            stream: None,
            scroll: 0,
            content_h: 0,
            hover: None,
        };
        t
    }

    fn main_rect(area: Rect) -> Rect {
        Rect::new(area.x + SIDEBAR + 10, area.y, area.w - SIDEBAR - 10, area.h)
    }

    fn new_chat_btn(area: Rect) -> Rect {
        Rect::new(area.x + 16, area.y + 74, SIDEBAR - 32, 44)
    }

    fn search_rect(area: Rect) -> Rect {
        Rect::new(area.x + 16, area.y + 130, SIDEBAR - 32, 40)
    }

    fn sign_out_rect(area: Rect) -> Rect {
        Rect::new(area.x + 16, area.bottom() - 58, SIDEBAR - 32, 42)
    }

    fn user_rect(area: Rect) -> Rect {
        Rect::new(area.x + 16, area.bottom() - 126, SIDEBAR - 32, 60)
    }

    fn credits_rect(area: Rect) -> Rect {
        Rect::new(area.x + 16, area.bottom() - 212, SIDEBAR - 32, 76)
    }

    /// The chat rows with their group headers: (header or None, chat index, rect).
    fn chat_rows(&self, area: Rect) -> Vec<(Option<&'static str>, usize, Rect)> {
        let now = crate::time::now();
        let q = self.search.to_lowercase();
        let top = area.y + 186;
        let bottom = Self::credits_rect(area).y - 10;
        let mut y = top;
        let mut out = Vec::new();
        let mut last: Option<&'static str> = None;
        for (i, ch) in self.chats.iter().enumerate() {
            if !q.is_empty() && !ch.title.to_lowercase().contains(&q) {
                continue;
            }
            let age = now.saturating_sub(ch.time);
            let group = if age < 86_400 {
                "Oggi"
            } else if age < 7 * 86_400 {
                "Ultimi 7 giorni"
            } else {
                "Meno recenti"
            };
            let header = if last != Some(group) {
                last = Some(group);
                y += 30;
                Some(group)
            } else {
                None
            };
            let r = Rect::new(area.x + 16, y, SIDEBAR - 32, 40);
            if r.bottom() > bottom {
                break;
            }
            out.push((header, i, r));
            y += 44;
        }
        out
    }

    fn input_rect(&self, area: Rect) -> Rect {
        let m = Self::main_rect(area);
        let w = (m.w - 120).min(780);
        if self.current.is_none() {
            Rect::new(m.x + (m.w - w) / 2, m.y + m.h / 2 + 20, w, 112)
        } else {
            Rect::new(m.x + (m.w - w) / 2, m.bottom() - 132, w, 112)
        }
    }

    fn model_pill(&self, area: Rect) -> Rect {
        let r = self.input_rect(area);
        let w = text::width(Font::Strong, &self.model.name) + 64;
        Rect::new(r.x + 54, r.bottom() - 48, w.min(r.w - 140), 36)
    }

    fn send_btn(&self, area: Rect) -> Rect {
        let r = self.input_rect(area);
        Rect::new(r.right() - 52, r.bottom() - 50, 40, 40)
    }

    fn chips(&self, area: Rect) -> Vec<(&'static str, &'static str, Rect)> {
        let r = self.input_rect(area);
        let items = [("Spiegami", "Spiegami in modo semplice "), ("Idee", "Dammi delle idee per "), ("Scrivi", "Scrivi "), ("Programma", "Scrivi il codice per ")];
        let mut x = r.x;
        items
            .iter()
            .map(|&(label, prompt)| {
                let w = text::width(Font::Strong, label) + 64;
                let rr = Rect::new(x, r.bottom() + 20, w, 42);
                x += w + 10;
                (label, prompt, rr)
            })
            .collect()
    }

    fn model_menu_rows(&self, area: Rect) -> Vec<(usize, Rect)> {
        let p = self.model_pill(area);
        let n = self.models.len().min(12);
        let up = self.current.is_some();
        (0..n)
            .map(|i| {
                let y = if up { p.y - 8 - (n - i) as i32 * 36 } else { p.bottom() + 8 + i as i32 * 36 };
                (i, Rect::new(p.x, y, 300, 34))
            })
            .collect()
    }

    fn send(&mut self, text_in: String) {
        if self.account.is_none() {
            return;
        }
        let text_in = String::from(text_in.trim());
        if text_in.is_empty() || self.stream.is_some() {
            return;
        }
        let idx = match self.current {
            Some(i) => i,
            None => {
                let title: String = text_in.chars().take(40).collect();
                let now = crate::time::now();
                self.chats.insert(0, Chat { id: format!("{now}-{}", crate::rand::u64() % 10000), title, time: now, model: self.model.id.clone(), messages: Vec::new() });
                0
            }
        };
        self.current = Some(idx);
        let chat = &mut self.chats[idx];
        chat.messages.push((String::from("user"), text_in));
        chat.messages.push((String::from("assistant"), String::new()));
        chat.time = crate::time::now();
        chat.model = self.model.id.clone();
        let mut history: Vec<(String, String)> = chat.messages[..chat.messages.len() - 1].to_vec();
        history.insert(0, (String::from("system"), String::from("Sei ticly AI, l'assistente dell'ecosistema ticly, dentro JustOS. Rispondi in italiano, in modo chiaro.")));
        // Keep the request under the gateway's 8000 characters.
        while history.len() > 2 && Value::Arr(history.iter().map(|(r, t)| Value::object(alloc::vec![("role", Value::string(r)), ("content", Value::string(t))])).collect()).to_string().len() > 7000 {
            history.remove(1);
        }
        let st = Arc::new(IrqLock::new(Stream { thinking: true, ..Default::default() }));
        self.stream = Some(st.clone());
        self.input.clear();
        self.scroll = i32::MAX;
        let model = self.model.id.clone();
        crate::task::spawn("ticly-ai", move || {
            let r = ticly::chat(&model, &history);
            let mut s = st.lock();
            if let Err(e) = r {
                s.error = Some(e);
            }
            s.done = true;
        });
    }

    /// Copies what the answer has so far into the chat.
    fn pump(&mut self) -> bool {
        let Some(st) = self.stream.clone() else { return false };
        let Some(i) = self.current else { return false };
        let (text, done, error) = {
            let s = st.lock();
            (s.text.clone(), s.done, s.error.clone())
        };
        if let Some(last) = self.chats[i].messages.last_mut() {
            last.1 = match &error {
                Some(e) if text.is_empty() => super::ticly::net_error(e),
                _ => text,
            };
        }
        if done {
            self.stream = None;
            self.chats[i].save();
        }
        self.scroll = i32::MAX;
        true
    }

    fn draw_sidebar(&mut self, c: &mut Canvas, area: Rect, a: &Account) {
        tui::wordmark(c, area.x + 24, area.y + 26, "Ai");
        let nb = Self::new_chat_btn(area);
        ui::button(c, nb, "+  Nuova chat", true, self.hover == Some(nb));
        ui::search_field(c, Self::search_rect(area), &self.search, "Cerca nelle chat", self.searching);
        for (header, i, r) in self.chat_rows(area) {
            if let Some(h) = header {
                ui::caps(c, r.x + 10, r.y - 22, h, TEXT_FAINT);
            }
            let sel = self.current == Some(i);
            c.fill_round(r, 12, if sel { ui::SELECTED } else if self.hover == Some(r) { ui::PANEL_HOVER } else { ui::PANEL });
            text::draw(c, Font::Body, r.x + 14, r.y + 11, &text::fit(Font::Body, &self.chats[i].title, r.w - 28), TEXT);
        }
        if self.chats.is_empty() {
            let r = Rect::new(area.x + 16, area.y + 196, SIDEBAR - 32, 70);
            c.fill_round(r, 14, ui::PANEL);
            for (k, l) in text::wrap(Font::Small, "Le tue conversazioni compariranno qui.", r.w - 28).iter().take(2).enumerate() {
                text::draw(c, Font::Small, r.x + 14, r.y + 16 + k as i32 * 18, l, TEXT_DIM);
            }
        }
        // No model answers yet: say so where the credits will be.
        let cr = Self::credits_rect(area);
        c.fill_round(cr, 16, ui::PANEL);
        ui::caps(c, cr.x + 16, cr.y + 16, "Anteprima", TEXT_FAINT);
        for (k, l) in text::wrap(Font::Small, "Le risposte non sono ancora disponibili.", cr.w - 32).iter().take(2).enumerate() {
            text::draw(c, Font::Small, cr.x + 16, cr.y + 36 + k as i32 * 17, l, TEXT_DIM);
        }
        tui::user_card(c, Self::user_rect(area), a);
        let so = Self::sign_out_rect(area);
        tui::sign_out_row(c, so, "Esci", self.hover == Some(so));
    }

    fn draw_blob(c: &mut Canvas, cx: i32, cy: i32, r: i32) {
        // ticly AI's purple friend: a soft blob with two eyes.
        // Eight bumps around the edge (offsets in 1/10 of the radius / 6).
        for (dx, dy) in [(0, -10), (7, -7), (10, 0), (7, 7), (0, 10), (-7, 7), (-10, 0), (-7, -7)] {
            c.fill_circle(cx + dx * r / 60, cy + dy * r / 60, r * 5 / 6, PURPLE);
        }
        c.fill_circle(cx, cy, r, PURPLE);
        c.fill_round(Rect::new(cx - r / 3 - 2, cy - r / 4, r / 7, r / 3), 3, 0xFFFF_FFFF);
        c.fill_round(Rect::new(cx + r / 5, cy - r / 4, r / 7, r / 3), 3, 0xFFFF_FFFF);
    }

    fn draw_input(&mut self, c: &mut Canvas, area: Rect, focused: bool) {
        let r = self.input_rect(area);
        c.fill_round(r, 22, 0xB30E_0A0A);
        c.stroke_round(r, 22, 1, ui::BORDER);
        let tx = r.x + 22;
        if self.input.is_empty() {
            text::draw(c, Font::Heading, tx, r.y + 18, "Chiedi qualcosa a ticly ai", TEXT_FAINT);
        } else {
            let lines = text::wrap(Font::Heading, &self.input, r.w - 44);
            let start = lines.len().saturating_sub(2);
            for (k, l) in lines[start..].iter().enumerate() {
                text::draw(c, Font::Heading, tx, r.y + 16 + k as i32 * 24, l, TEXT);
            }
        }
        if focused && (crate::time::uptime_ms() / 530) % 2 == 0 {
            let lines = text::wrap(Font::Heading, &self.input, r.w - 44);
            let last = lines.last().cloned().unwrap_or_default();
            let row = lines.len().saturating_sub(1).min(1) as i32;
            let w = text::width(Font::Heading, &last);
            c.fill(Rect::new(tx + w + 1, r.y + 18 + row * 24, 2, 20), ui::ACCENT);
        }
        crate::gui::res::draw_icon(c, "photos", 22, r.x + 18, r.bottom() - 41);
        let p = self.model_pill(area);
        c.fill_round(p, p.h / 2, if self.hover == Some(p) || self.model_menu { 0x26FF_FFFF } else { 0x14FF_FFFF });
        c.stroke_round(p, p.h / 2, 1, 0x26FF_FFFF);
        crate::gui::res::draw_icon(c, "g-grid", 14, p.x + 12, p.y + 11);
        let (lh, _) = text::metrics(Font::Strong);
        text::draw(c, Font::Strong, p.x + 34, p.y + (p.h - lh) / 2, &text::fit(Font::Strong, &self.model.name, p.w - 60), TEXT);
        crate::gui::res::draw_icon(c, "g-down", 12, p.right() - 22, p.y + 12);
        let s = self.send_btn(area);
        let can = !self.input.trim().is_empty() && self.stream.is_none();
        c.fill_circle(s.x + s.w / 2, s.y + s.h / 2, s.w / 2, if can { 0xFFFF_FFFF } else { 0x26FF_FFFF });
        let arrow = if can { 0xFF0E_0B0C } else { 0x80FF_FFFF };
        let (cx, cy) = (s.x + s.w / 2, s.y + s.h / 2);
        c.line(cx, cy + 8, cx, cy - 8, 2, arrow);
        c.line(cx - 6, cy - 2, cx, cy - 8, 2, arrow);
        c.line(cx + 6, cy - 2, cx, cy - 8, 2, arrow);
    }

    fn draw_welcome(&mut self, c: &mut Canvas, area: Rect, a: &Account) {
        let r = self.input_rect(area);
        Self::draw_blob(c, r.x + 30, r.y - 196, 28);
        let hello = format!("{}, ", tui::greeting());
        let w = text::draw(c, Font::Title, r.x, r.y - 150, &hello, TEXT);
        text::draw(c, Font::Title, r.x + w, r.y - 150, &a.first_name(), 0xFFB8_B0B8);
        text::draw(c, Font::Heading, r.x, r.y - 96, "Come posso aiutarti oggi?", TEXT_DIM);
        for (label, _, rr) in self.chips(area) {
            c.fill_round(rr, rr.h / 2, if self.hover == Some(rr) { 0x26FF_FFFF } else { 0x0FFF_FFFF });
            c.stroke_round(rr, rr.h / 2, 1, 0x26FF_FFFF);
            c.fill_circle(rr.x + 22, rr.y + rr.h / 2, 13, 0x1FFF_FFFF);
            let icon = match label {
                "Spiegami" => "info",
                "Idee" => "g-plus",
                "Scrivi" => "text-editor",
                _ => "code",
            };
            crate::gui::res::draw_icon(c, icon, 14, rr.x + 15, rr.y + rr.h / 2 - 7);
            let (lh, _) = text::metrics(Font::Strong);
            text::draw(c, Font::Strong, rr.x + 44, rr.y + (rr.h - lh) / 2, label, TEXT);
        }
    }

    /// Draws a message with a little Markdown: headings, lists, code blocks.
    fn draw_markdown(c: &mut Canvas, x: i32, mut y: i32, w: i32, body: &str) -> i32 {
        let mut in_code = false;
        for raw in body.split('\n') {
            if raw.trim_start().starts_with("```") {
                in_code = !in_code;
                y += 4;
                continue;
            }
            if in_code {
                c.fill(Rect::new(x - 8, y - 2, w + 16, 22), 0x99_0A0808);
                text::draw(c, Font::Mono, x, y + 1, &text::fit(Font::Mono, raw, w), 0xFFE6_D8C8);
                y += 22;
                continue;
            }
            let line = raw.replace("**", "").replace('`', "");
            let (font, color, indent, bullet, content) = if let Some(h) = line.strip_prefix("### ").or(line.strip_prefix("## ")).or(line.strip_prefix("# ")) {
                (Font::Heading, TEXT, 0, false, h.to_string())
            } else if let Some(b) = line.trim_start().strip_prefix("- ").or(line.trim_start().strip_prefix("* ")) {
                (Font::Body, TEXT, 18, true, b.to_string())
            } else {
                (Font::Body, TEXT, 0, false, line.clone())
            };
            if content.trim().is_empty() {
                y += 10;
                continue;
            }
            let (lh, _) = text::metrics(font);
            for (k, l) in text::wrap(font, &content, w - indent).iter().enumerate() {
                if bullet && k == 0 {
                    c.fill_circle(x + 6, y + lh / 2 + 1, 3, TEXT_DIM);
                }
                text::draw(c, font, x + indent, y, l, color);
                y += lh + 6;
            }
        }
        y
    }

    fn draw_chat(&mut self, c: &mut Canvas, area: Rect, i: usize) {
        let m = Self::main_rect(area);
        let input = self.input_rect(area);
        let view = Rect::new(m.x, m.y + 10, m.w, input.y - m.y - 20);
        let col_w = input.w;
        let x0 = input.x;
        // Measure, then draw from the scroll position.
        let saved = c.clip();
        c.set_clip(saved.intersect(&view));
        let mut y = view.y + 20 - self.scroll.min(i32::MAX / 2).max(0);
        let start_y = y;
        let msgs = self.chats[i].messages.clone();
        let thinking = self.stream.as_ref().is_some_and(|s| s.lock().thinking);
        for (k, (role, body)) in msgs.iter().enumerate() {
            if role == "user" {
                let lines = text::wrap(Font::Body, body, col_w * 2 / 3);
                let bw = lines.iter().map(|l| text::width(Font::Body, l)).max().unwrap_or(0) + 32;
                let bh = lines.len() as i32 * 22 + 22;
                let r = Rect::new(x0 + col_w - bw, y, bw, bh);
                c.fill_round(r, 18, 0x26FF_FFFF);
                for (j, l) in lines.iter().enumerate() {
                    text::draw(c, Font::Body, r.x + 16, r.y + 11 + j as i32 * 22, l, TEXT);
                }
                y += bh + 18;
            } else {
                Self::draw_blob(c, x0 + 14, y + 14, 12);
                if body.is_empty() && k == msgs.len() - 1 && self.stream.is_some() {
                    let dots = ((crate::time::uptime_ms() / 350) % 4) as usize;
                    let label = if thinking { "Sto pensando" } else { "Scrivo" };
                    text::draw(c, Font::Body, x0 + 40, y + 4, &format!("{label}{}", ".".repeat(dots)), TEXT_DIM);
                    y += 40;
                } else {
                    y = Self::draw_markdown(c, x0 + 40, y + 4, col_w - 40, body) + 16;
                }
            }
        }
        self.content_h = y - start_y + 20;
        c.set_clip(saved);
    }
}

impl App for TiclyAi {
    fn title(&self) -> String {
        String::from("ticly AI")
    }

    fn icon(&self) -> &str {
        "ticly-ai"
    }

    fn kind(&self) -> &'static str {
        "ticly-ai"
    }

    fn size(&self) -> (i32, i32) {
        (1060, 680)
    }

    fn resizable(&self) -> bool {
        true
    }

    fn sidebar(&self) -> i32 {
        if self.account.is_some() {
            SIDEBAR
        } else {
            0
        }
    }

    fn draw(&mut self, c: &mut Canvas, area: Rect, blur: &Canvas, focused: bool) {
        let Some(a) = self.account.clone() else {
            self.login.draw(c, blur, area, "Ai");
            return;
        };
        self.draw_sidebar(c, area, &a);
        let m = Self::main_rect(area);
        let saved = c.clip();
        c.set_clip(saved.intersect(&m));
        match self.current {
            None => self.draw_welcome(c, area, &a),
            Some(i) => {
                // Keep the end in view while an answer comes in.
                let view_h = self.input_rect(area).y - m.y - 30;
                let max = (self.content_h - view_h).max(0);
                self.scroll = self.scroll.clamp(0, max);
                self.draw_chat(c, area, i);
                let max = (self.content_h - view_h).max(0);
                if self.scroll > max {
                    self.scroll = max;
                }
            }
        }
        self.draw_input(c, area, focused && !self.searching);
        if self.model_menu {
            let rows = self.model_menu_rows(area);
            if let (Some(first), Some(last)) = (rows.first(), rows.last()) {
                let bg = Rect::new(first.1.x - 6, first.1.y - 6, first.1.w + 12, last.1.bottom() - first.1.y + 12);
                ui::glass(c, blur, bg, 16, ui::GLASS_STRONG);
            }
            for (k, r) in rows {
                let md = &self.models[k];
                if self.hover == Some(r) {
                    c.fill_round(r, 10, ui::SELECTED);
                }
                let color = if md.unlocked { TEXT } else { TEXT_FAINT };
                text::draw(c, Font::Body, r.x + 12, r.y + 8, &text::fit(Font::Body, &md.name, r.w - 90), color);
                if !md.unlocked {
                    text::draw_right(c, Font::Small, r.right() - 12, r.y + 10, &md.min_plan, 0xFFE0_A050);
                } else if md.id == self.model.id {
                    let (x, y) = (r.right() - 24, r.y + r.h / 2);
                    c.line(x, y, x + 4, y + 4, 2, TEXT);
                    c.line(x + 4, y + 4, x + 11, y - 4, 2, TEXT);
                }
            }
        }
        c.set_clip(saved);
    }

    fn press(&mut self, x: i32, y: i32, _double: bool, area: Rect) -> Action {
        let (ax, ay) = (area.x + x, area.y + y);
        if self.account.is_none() {
            self.login.press(ax, ay, area);
            return Action::Redraw;
        }
        if self.model_menu {
            self.model_menu = false;
            for (k, r) in self.model_menu_rows(area) {
                if r.contains(ax, ay) {
                    let md = self.models[k].clone();
                    if md.unlocked {
                        self.model = md;
                    } else {
                        return Action::Notify(md.name, format!("Serve il piano {} di ticly.", md.min_plan), "ticly-ai");
                    }
                }
            }
            return Action::Redraw;
        }
        self.searching = Self::search_rect(area).contains(ax, ay);
        if self.searching {
            return Action::Redraw;
        }
        if Self::new_chat_btn(area).contains(ax, ay) {
            self.current = None;
            self.input.clear();
            return Action::Redraw;
        }
        if Self::sign_out_rect(area).contains(ax, ay) {
            ticly::sign_out();
            self.account = None;
            self.login = Login::new();
            return Action::Redraw;
        }
        for (_, i, r) in self.chat_rows(area) {
            if r.contains(ax, ay) {
                self.current = Some(i);
                self.scroll = i32::MAX;
                if let Some(md) = self.models.iter().find(|m| m.id == self.chats[i].model) {
                    self.model = md.clone();
                }
                return Action::Redraw;
            }
        }
        if self.model_pill(area).contains(ax, ay) {
            self.model_menu = true;
            return Action::Redraw;
        }
        if self.send_btn(area).contains(ax, ay) {
            let t = self.input.clone();
            self.send(t);
            return Action::Redraw;
        }
        if self.current.is_none() {
            for (_, prompt, r) in self.chips(area) {
                if r.contains(ax, ay) {
                    self.input = String::from(prompt);
                    return Action::Redraw;
                }
            }
        }
        Action::Redraw
    }

    fn hover(&mut self, at: Option<(i32, i32)>, area: Rect) -> bool {
        let mut t: Vec<Rect> = Vec::new();
        if self.account.is_none() {
            t = self.login.hover_targets(area);
            let h = at.and_then(|(x, y)| t.into_iter().find(|r| r.contains(area.x + x, area.y + y)));
            let changed = h != self.login.hover;
            self.login.hover = h;
            return changed;
        }
        t.extend([Self::new_chat_btn(area), Self::sign_out_rect(area), self.model_pill(area), self.send_btn(area)]);
        t.extend(self.chat_rows(area).into_iter().map(|(_, _, r)| r));
        if self.current.is_none() {
            t.extend(self.chips(area).into_iter().map(|(_, _, r)| r));
        }
        if self.model_menu {
            t.extend(self.model_menu_rows(area).into_iter().map(|(_, r)| r));
        }
        let h = at.and_then(|(x, y)| t.into_iter().find(|r| r.contains(area.x + x, area.y + y)));
        let changed = h != self.hover;
        self.hover = h;
        changed
    }

    fn key(&mut self, ev: KeyEvent) -> Action {
        if self.account.is_none() {
            return if self.login.key(ev) { Action::Redraw } else { Action::None };
        }
        if ev.key == Key::Escape {
            self.model_menu = false;
            self.searching = false;
            return Action::Redraw;
        }
        if self.searching {
            return if ui::edit_text(&mut self.search, &ev, 40) { Action::Redraw } else { Action::None };
        }
        if ev.key == Key::Enter && !ev.shift {
            let t = self.input.clone();
            self.send(t);
            return Action::Redraw;
        }
        if ev.ctrl && matches!(ev.key, Key::Char('v') | Key::Char('V')) {
            self.input.push_str(&ui::paste());
            return Action::Redraw;
        }
        if ev.ctrl && matches!(ev.key, Key::Char('n') | Key::Char('N')) {
            self.current = None;
            return Action::Redraw;
        }
        if ev.key == Key::Enter {
            self.input.push('\n');
            return Action::Redraw;
        }
        if ui::edit_text(&mut self.input, &ev, 6000) {
            return Action::Redraw;
        }
        Action::None
    }

    fn scroll(&mut self, lines: i32) -> bool {
        if self.current.is_none() {
            return false;
        }
        self.scroll = (self.scroll.min(self.content_h) + lines * 48).max(0);
        true
    }

    fn tick(&mut self) -> bool {
        if self.account.is_none() {
            if let Some(a) = self.login.poll() {
                self.account = Some(a);
                }
            return true;
        }
        true
    }

    fn animating(&self) -> bool {
        self.stream.is_some()
    }

    fn frame(&mut self) -> bool {
        self.pump()
    }
}

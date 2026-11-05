//! An app's screen in a window: an app bar with the title and, below it,
//! the activity's views, laid out and drawn in the light Material look.

use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use justdex::android::{view, ViewKind};
use justdex::{Ref, Throw, Vm};
use justui::{Event, Font, Fonts, Key, Rect, Window};

const BG: u32 = 0xFFFA_FAFA;
const PRIMARY: u32 = 0xFF62_00EE;
const TEXT: u32 = 0xFF21_2121;
const HINT: u32 = 0xFF9E_9E9E;
const BAR_H: i32 = 56;
const GAP: i32 = 8;
const MARGIN: i32 = 16;

struct Screen {
    win: Window,
    fonts: &'static Fonts,
    /// Where each view that reacts to the mouse is (last drawn on top).
    hits: Vec<(Rect, Ref)>,
    focus: Option<Ref>,
    scroll: i32,
    content_h: i32,
    toast: Option<(String, u64)>,
    error: Option<String>,
    name: String,
    /// Placing the children of a horizontal layout.
    in_row: bool,
}

fn font_for(size: f32, bold: bool) -> Font {
    match size as i32 {
        0 => {
            if bold {
                Font::Strong
            } else {
                Font::Body
            }
        }
        s if s <= 12 => Font::Small,
        s if s <= 17 => {
            if bold {
                Font::Strong
            } else {
                Font::Body
            }
        }
        s if s <= 23 => Font::Heading,
        s if s <= 33 => Font::Title,
        _ => Font::Display,
    }
}

impl Screen {
    /// Lays out `v` at (`x`, `y`) in width `w`; draws it if `draw`.
    /// Gives its height.
    fn place(&mut self, v: &Ref, x: i32, y: i32, w: i32, draw: bool) -> i32 {
        let Some(info) = view(v) else { return 0 };
        if info.visibility == 8 {
            return 0;
        }
        let show = draw && info.visibility == 0;
        let f = self.fonts;
        let [pl, pt, pr, pb] = info.padding.map(|p| p / 2);
        let inner = Rect::new(x + pl, y + pt, (w - pl - pr).max(1), 0);
        let kind = info.kind;
        let color = info.text_color.unwrap_or(TEXT);
        let font = font_for(info.text_size, info.bold);
        let center = info.gravity & 0x7 == 1 || info.gravity & 0x11 == 0x11;
        let (lh, _) = f.metrics(font);
        let text = info.text.clone();
        let children = info.children.clone();
        let vertical = info.vertical;
        let bg = info.bg;
        let clickable = !info.on_click.is_null();
        let enabled = info.enabled;
        let checked = info.checked;
        let hint = info.hint.clone();
        let (progress, max) = (info.progress, info.max.max(1));
        drop(info);
        let body_h = match kind {
            ViewKind::Group => {
                if vertical {
                    let mut cy = inner.y;
                    for c in &children {
                        let h = self.place(c, inner.x, cy, inner.w, false);
                        if h > 0 {
                            cy += h + GAP;
                        }
                    }
                    (cy - inner.y - GAP).max(0)
                } else {
                    let n = children.iter().filter(|c| view(c).is_some_and(|i| i.visibility != 8)).count().max(1) as i32;
                    let cw = (inner.w - GAP * (n - 1)) / n;
                    let mut cx = inner.x;
                    let mut h = 0;
                    for c in &children {
                        let ch = self.place(c, cx, inner.y, cw, false);
                        if ch > 0 {
                            h = h.max(ch);
                            cx += cw + GAP;
                        }
                    }
                    h
                }
            }
            ViewKind::Text => f.wrap(font, &text, inner.w).len().max(1) as i32 * lh,
            ViewKind::Button => 40,
            ViewKind::Edit => 44,
            ViewKind::Check => 32,
            ViewKind::Progress => 12,
            ViewKind::Image => 64,
            ViewKind::Plain => 0,
        };
        let h = body_h + pt + pb;
        let h = if kind == ViewKind::Plain { h.max(if bg.is_some() { 1 } else { 0 }) } else { h };
        if !draw {
            return h;
        }
        let area = Rect::new(x, y, w, h);
        if show {
            if let Some(c) = bg {
                self.win.canvas.fill(area, c);
            }
        }
        let c = &mut self.win.canvas;
        match kind {
            ViewKind::Group => {
                let outer = self.in_row;
                self.in_row = !vertical;
                if vertical {
                    let mut cy = inner.y;
                    for ch in &children {
                        let hh = self.place(ch, inner.x, cy, inner.w, show);
                        if hh > 0 {
                            cy += hh + GAP;
                        }
                    }
                } else {
                    let n = children.iter().filter(|c| view(c).is_some_and(|i| i.visibility != 8)).count().max(1) as i32;
                    let cw = (inner.w - GAP * (n - 1)) / n;
                    let mut cx = inner.x;
                    for ch in &children {
                        if self.place(ch, cx, inner.y, cw, show) > 0 {
                            cx += cw + GAP;
                        }
                    }
                }
                self.in_row = outer;
            }
            ViewKind::Text if show => {
                for (i, line) in f.wrap(font, &text, inner.w).iter().enumerate() {
                    let ly = inner.y + i as i32 * lh;
                    if center {
                        f.draw_centered(c, font, inner.x + inner.w / 2, ly, line, color);
                    } else {
                        f.draw(c, font, inner.x, ly, line, color);
                    }
                }
            }
            ViewKind::Button if show => {
                let label = text.to_uppercase();
                let tw = f.width(Font::Strong, &label) + 32;
                // In a column a button is as wide as its label; in a row it fills its cell.
                let bw = if self.in_row { inner.w } else { tw.clamp(88, inner.w) };
                let r = Rect::new(inner.x, inner.y, bw, 40);
                c.shadow(r, 4, 3, 40);
                c.fill_round(r, 4, if enabled { bg.unwrap_or(PRIMARY) } else { 0xFFBD_BDBD });
                let (blh, _) = f.metrics(Font::Strong);
                let fg = if info_has_color(v) { color } else { 0xFFFF_FFFF };
                f.draw_centered(c, Font::Strong, r.x + r.w / 2, r.y + (40 - blh) / 2, &label, fg);
            }
            ViewKind::Edit if show => {
                let focused = self.focus.as_ref().is_some_and(|x| Rc::ptr_eq(x, v));
                let (elh, _) = f.metrics(font);
                let ty = inner.y + (40 - elh) / 2;
                if text.is_empty() {
                    f.draw(c, font, inner.x + 2, ty, &hint, HINT);
                } else {
                    let shown = f.fit(font, &text, inner.w - 6);
                    f.draw(c, font, inner.x + 2, ty, &shown, color);
                }
                if focused {
                    let cx = inner.x + 3 + if text.is_empty() { 0 } else { f.width(font, &f.fit(font, &text, inner.w - 6)) };
                    c.fill(Rect::new(cx, ty, 2, elh), PRIMARY);
                }
                c.fill(Rect::new(inner.x, inner.y + 40, inner.w, if focused { 2 } else { 1 }), if focused { PRIMARY } else { HINT });
            }
            ViewKind::Check if show => {
                let b = Rect::new(inner.x + 2, inner.y + 7, 18, 18);
                if checked {
                    c.fill_round(b, 2, PRIMARY);
                    c.line(b.x + 4, b.y + 9, b.x + 8, b.y + 13, 2, 0xFFFF_FFFF);
                    c.line(b.x + 8, b.y + 13, b.x + 14, b.y + 5, 2, 0xFFFF_FFFF);
                } else {
                    c.stroke_round(b, 2, 2, 0xFF75_7575);
                }
                f.draw(c, font, inner.x + 32, inner.y + (32 - lh) / 2, &text, color);
            }
            ViewKind::Progress if show => {
                let r = Rect::new(inner.x, inner.y + 4, inner.w, 4);
                c.fill(r, 0xFFD1_C4E9);
                c.fill(Rect::new(r.x, r.y, (r.w as i64 * progress.clamp(0, max) as i64 / max as i64) as i32, 4), PRIMARY);
            }
            ViewKind::Image if show => {
                c.fill_round(Rect::new(inner.x, inner.y, 64, 64), 8, 0xFFE0_E0E0);
            }
            _ => {}
        }
        if show && enabled && (clickable || matches!(kind, ViewKind::Button | ViewKind::Edit | ViewKind::Check)) {
            self.hits.push((area, v.clone()));
        }
        h
    }

    fn draw(&mut self, vm: &mut Vm) {
        let (w, h) = (self.win.width(), self.win.height());
        let f = self.fonts;
        self.hits.clear();
        self.win.canvas.reset_clip();
        self.win.canvas.fill(Rect::new(0, 0, w, h), BG);
        if let Some(root) = vm.ui.content.clone() {
            self.content_h = self.place(&root, MARGIN, 0, w - 2 * MARGIN, false);
            let room = h - BAR_H - 2 * MARGIN;
            self.scroll = self.scroll.clamp(0, (self.content_h - room).max(0));
            self.win.canvas.set_clip(Rect::new(0, BAR_H, w, h - BAR_H));
            self.place(&root, MARGIN, BAR_H + MARGIN - self.scroll, w - 2 * MARGIN, true);
            self.win.canvas.reset_clip();
        }
        // The app bar.
        let bar = Rect::new(0, 0, w, BAR_H);
        self.win.canvas.fill(bar, PRIMARY);
        self.win.canvas.fill(Rect::new(0, BAR_H, w, 3), 0x3000_0000);
        let title = vm.ui.title.clone().unwrap_or_else(|| self.name.clone());
        let (lh, _) = f.metrics(Font::Heading);
        let shown = f.fit(Font::Heading, &title, w - 2 * MARGIN);
        f.draw(&mut self.win.canvas, Font::Heading, MARGIN, (BAR_H - lh) / 2, &shown, 0xFFFF_FFFF);
        if let Some((t, _)) = &self.toast {
            let lines = f.wrap(Font::Body, t, w - 96);
            let (blh, _) = f.metrics(Font::Body);
            let tw = lines.iter().map(|l| f.width(Font::Body, l)).max().unwrap_or(0) + 32;
            let th = lines.len() as i32 * blh + 20;
            let r = Rect::new((w - tw) / 2, h - th - 48, tw, th);
            self.win.canvas.fill_round(r, th.min(24) / 2 + 4, 0xE632_3232);
            for (i, l) in lines.iter().enumerate() {
                f.draw_centered(&mut self.win.canvas, Font::Body, w / 2, r.y + 10 + i as i32 * blh, l, 0xFFFF_FFFF);
            }
        }
        if let Some(e) = &self.error {
            let r = Rect::new(MARGIN, BAR_H + MARGIN, w - 2 * MARGIN, 0);
            let lines = f.wrap(Font::Body, e, r.w - 32);
            let (blh, _) = f.metrics(Font::Body);
            let (slh, _) = f.metrics(Font::Strong);
            let card = Rect::new(r.x, r.y, r.w, slh + lines.len() as i32 * blh + 40);
            self.win.canvas.fill(Rect::new(0, BAR_H, w, h - BAR_H), 0x8000_0000);
            self.win.canvas.fill_round(card, 8, 0xFFFF_FFFF);
            f.draw(&mut self.win.canvas, Font::Strong, card.x + 16, card.y + 14, &format!("{} si è arrestata", self.name), 0xFFB0_0020);
            for (i, l) in lines.iter().enumerate() {
                f.draw(&mut self.win.canvas, Font::Body, card.x + 16, card.y + 22 + slh + i as i32 * blh, l, TEXT);
            }
        }
        self.win.present();
    }

    /// What a call into the app did: an exception stops the app (it is
    /// shown); `System.exit` ends the program.
    fn check(&mut self, vm: &mut Vm, r: Result<(), Throw>) -> Option<i32> {
        match r {
            Ok(()) => None,
            Err(Throw::Exit(code)) => Some(code),
            Err(Throw::Ex(ex)) => {
                let s = vm.describe(&ex);
                justrt::eprintln!("dalvik: {s}");
                self.error = Some(s);
                vm.ui.dirty = true;
                None
            }
        }
    }

    fn edits(&self, vm: &Vm) -> Vec<Ref> {
        let mut out = Vec::new();
        fn walk(v: &Ref, out: &mut Vec<Ref>) {
            let Some(i) = view(v) else { return };
            if i.kind == ViewKind::Edit && i.visibility == 0 && i.enabled {
                out.push(v.clone());
            }
            for c in &i.children {
                walk(c, out);
            }
        }
        if let Some(r) = &vm.ui.content {
            walk(r, &mut out);
        }
        out
    }
}


fn info_has_color(v: &Ref) -> bool {
    view(v).is_some_and(|i| i.text_color.is_some())
}


pub fn run(vm: &mut Vm, class: &str, name: &str) -> i32 {
    let win = match Window::resizable(name, 400, 680) {
        Ok(w) => w,
        Err(e) => {
            justrt::eprintln!("dalvik: no window: {}", e.as_str());
            return 1;
        }
    };
    let fonts = win.fonts;
    let mut s = Screen { win, fonts, hits: Vec::new(), focus: None, scroll: 0, content_h: 0, toast: None, error: None, name: name.into(), in_row: false };
    let started = vm.start_activity(class).map(|_| ());
    if let Some(code) = s.check(vm, started) {
        return code;
    }
    s.focus = s.edits(vm).into_iter().next();
    let mut title = String::new();
    loop {
        if vm.ui.finished {
            break;
        }
        let now = justrt::time::uptime_ms();
        if let Some((t, ms)) = vm.ui.toasts.pop() {
            vm.ui.toasts.clear();
            s.toast = Some((t, now + ms));
            vm.ui.dirty = true;
        }
        if s.toast.as_ref().is_some_and(|(_, until)| *until <= now) {
            s.toast = None;
            vm.ui.dirty = true;
        }
        let want = vm.ui.title.clone().unwrap_or_else(|| s.name.clone());
        if want != title {
            s.win.set_title(&want);
            title = want;
        }
        if vm.ui.dirty {
            vm.ui.dirty = false;
            s.draw(vm);
        }
        let mut wait = 1000;
        if s.error.is_none() {
            if let Some(t) = vm.next_timer() {
                wait = wait.min(t.saturating_sub(now).max(1));
            }
        }
        if let Some((_, until)) = &s.toast {
            wait = wait.min(until.saturating_sub(now).max(1));
        }
        if let Some(ev) = s.win.event(wait) {
            match ev {
                Event::Close => break,
                Event::Resize { .. } => vm.ui.dirty = true,
                Event::Scroll(d) => {
                    s.scroll += d * 40;
                    vm.ui.dirty = true;
                }
                _ if s.error.is_some() => {}
                Event::Press { x, y, .. } => {
                    let hit = s.hits.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, v)| v.clone());
                    if let Some(v) = hit {
                        if view(&v).is_some_and(|i| i.kind == ViewKind::Edit) {
                            s.focus = Some(v.clone());
                        }
                        let r = vm.click(&v);
                        if let Some(code) = s.check(vm, r) {
                            return code;
                        }
                    }
                    vm.ui.dirty = true;
                }
                Event::Key(k) => match k.key {
                    Key::Escape => {
                        let r = vm.back();
                        if let Some(code) = s.check(vm, r) {
                            return code;
                        }
                    }
                    Key::Tab => {
                        let edits = s.edits(vm);
                        let at = s.focus.as_ref().and_then(|f| edits.iter().position(|e| Rc::ptr_eq(e, f)));
                        s.focus = edits.get(at.map(|i| (i + 1) % edits.len()).unwrap_or(0)).cloned();
                        vm.ui.dirty = true;
                    }
                    Key::Backspace => {
                        if let Some(f) = s.focus.clone() {
                            vm.type_text(&f, "", true);
                        }
                    }
                    Key::Char(c) if !k.ctrl => {
                        if let Some(f) = s.focus.clone() {
                            let mut b = [0u8; 4];
                            vm.type_text(&f, c.encode_utf8(&mut b), false);
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        if s.error.is_none() {
            let r = vm.run_timers();
            if let Some(code) = s.check(vm, r) {
                return code;
            }
        }
    }
    let r = vm.stop_activity();
    s.check(vm, r).unwrap_or(0)
}


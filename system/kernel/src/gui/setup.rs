//! The first start: a short guided setup (welcome, name, keyboard,
//! wallpaper), shown until it has been completed once.

use super::gfx::{Canvas, Rect};
use super::res;
use super::settings;
use super::splash::draw_logo;
use super::text::{self, Font};
use super::ui::{self, Event, Looper, View, TEXT, TEXT_DIM};
use super::Display;
use crate::dev::input::Key;
use crate::dev::keymap::Layout;
use crate::fs;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const STEPS: usize = 5;

struct Setup {
    step: usize,
    name: String,
    layout: Layout,
    wall: usize,
    w: i32,
    h: i32,
    sharp: Canvas,
    blur: Canvas,
    thumbs: Vec<Canvas>,
    mouse: (i32, i32),
    done: bool,
}

/// Where things are on the current step.
struct Layout_ {
    card: Rect,
    primary: Rect,
    back: Option<Rect>,
    options: Vec<Rect>,
    field: Option<Rect>,
}

impl Setup {
    fn card(&self) -> Rect {
        let (cw, ch) = (600.min(self.w - 40), 460.min(self.h - 40));
        Rect::new((self.w - cw) / 2, (self.h - ch) / 2, cw, ch)
    }

    fn layout(&self) -> Layout_ {
        let card = self.card();
        let cx = card.x + card.w / 2;
        let by = card.bottom() - 96;
        let (primary, back) = match self.step {
            0 | 4 => (Rect::new(cx - 110, by, 220, 50), None),
            _ => (Rect::new(cx + 8, by, 170, 46), Some(Rect::new(cx - 178, by, 170, 46))),
        };
        let mut options = Vec::new();
        let mut field = None;
        match self.step {
            1 => field = Some(Rect::new(cx - 190, card.y + 230, 380, 54)),
            2 => {
                options.push(Rect::new(cx - 236, card.y + 222, 228, 74));
                options.push(Rect::new(cx + 8, card.y + 222, 228, 74));
            }
            3 => {
                // Four to a row.
                for i in 0..res::WALLPAPERS.len() as i32 {
                    options.push(Rect::new(cx - 263 + (i % 4) * 134, card.y + 128 + (i / 4) * 116, 124, 78));
                }
            }
            _ => {}
        }
        Layout_ { card, primary, back, options, field }
    }

    fn primary_label(&self) -> &'static str {
        match self.step {
            0 => "INIZIA",
            4 => "Vai alla scrivania",
            _ => "Avanti",
        }
    }

    fn can_go_on(&self) -> bool {
        self.step != 1 || !self.name.trim().is_empty()
    }

    fn next(&mut self) {
        if !self.can_go_on() {
            return;
        }
        if self.step == 4 {
            self.finish();
            return;
        }
        self.step += 1;
    }

    fn set_wallpaper(&mut self, i: usize) {
        if i != self.wall {
            self.wall = i;
            let (s, b) = res::wallpaper(res::WALLPAPERS[i].0, self.w, self.h);
            self.sharp = s;
            self.blur = b;
        }
    }

    fn finish(&mut self) {
        let (name, layout, wall) = (String::from(self.name.trim()), self.layout, res::WALLPAPERS[self.wall].0);
        settings::update(|s| {
            s.name = name;
            s.layout = layout;
            s.wallpaper = String::from(wall);
            s.setup_done = true;
        });
        for dir in ["Scrivania", "Documenti", "Scaricati", "Immagini", "Musica", "Video", "Progetti"] {
            if let Ok(p) = fs::abs(&format!("sys:/home/{dir}")) {
                if !fs::exists(&p) {
                    let _ = fs::make_dir(&p);
                }
            }
        }
        self.done = true;
    }
}

impl View for Setup {
    fn resized(&mut self, w: i32, h: i32) {
        self.w = w;
        self.h = h;
        let (sharp, blur) = res::wallpaper(res::WALLPAPERS[self.wall].0, w, h);
        self.sharp = sharp;
        self.blur = blur;
    }

    fn event(&mut self, l: &mut Looper, ev: Event) -> bool {
        let lay = self.layout();
        let before = (self.step, self.name.clone(), self.layout, self.wall);
        match ev {
            Event::Move { x, y, .. } => {
                let hit = |r: &Rect| r.contains(x, y);
                let was =
                    (lay.primary.contains(self.mouse.0, self.mouse.1), lay.back.is_some_and(|b| b.contains(self.mouse.0, self.mouse.1)));
                self.mouse = (x, y);
                if was != (hit(&lay.primary), lay.back.is_some_and(|b| hit(&b))) {
                    l.damage(lay.card);
                }
            }
            Event::Press { x, y, .. } => {
                if lay.primary.contains(x, y) {
                    self.next();
                } else if lay.back.is_some_and(|b| b.contains(x, y)) {
                    self.step -= 1;
                } else if let Some(i) = lay.options.iter().position(|o| o.contains(x, y)) {
                    match self.step {
                        2 => self.layout = if i == 0 { Layout::It } else { Layout::Us },
                        3 => self.set_wallpaper(i),
                        _ => {}
                    }
                }
            }
            Event::Key(k) => match k.key {
                Key::Enter => self.next(),
                Key::Escape if self.step > 0 => self.step -= 1,
                Key::Left if self.step == 2 => self.layout = Layout::It,
                Key::Right if self.step == 2 => self.layout = Layout::Us,
                Key::Left if self.step == 3 && self.wall > 0 => self.set_wallpaper(self.wall - 1),
                Key::Right if self.step == 3 && self.wall + 1 < res::WALLPAPERS.len() => self.set_wallpaper(self.wall + 1),
                _ if self.step == 1 => {
                    ui::edit_text(&mut self.name, &k, 32);
                }
                _ => {}
            },
            _ => {}
        }
        if before.3 != self.wall {
            l.damage(Rect::new(0, 0, self.w, self.h));
        } else if before != (self.step, self.name.clone(), self.layout, self.wall) {
            l.damage(ui::with_shadow(lay.card));
        }
        self.done
    }

    fn draw(&mut self, c: &mut Canvas, r: Rect) {
        // The wallpaper, softened and darkened, behind a glass card.
        c.blit(&self.blur, r.x, r.y, r);
        c.fill(r, 0x5014_0E1C);
        let lay = self.layout();
        let card = lay.card;
        if !r.overlaps(&ui::with_shadow(card)) {
            return;
        }
        ui::glass(c, &self.blur, card, 30, 0xB023_1A2E);
        let cx = card.x + card.w / 2;
        let mut y = card.y + 44;
        match self.step {
            0 => {
                draw_logo(c, cx, y + 40, 35, 0xFFF2_EFE6);
                y += 110;
                text::draw_centered(c, Font::Title, cx, y, "Benvenuto in justOS", TEXT);
                text::draw_centered(c, Font::Body, cx, y + 56, "Prepariamo il computer in pochi passi.", TEXT_DIM);
                text::draw_centered(c, Font::Body, cx, y + 78, "Ci vuole meno di un minuto.", TEXT_DIM);
            }
            1 => {
                res::draw_icon(c, "user", 72, cx - 36, y);
                text::draw_centered(c, Font::Title, cx, y + 88, "Come ti chiami?", TEXT);
                text::draw_centered(c, Font::Body, cx, y + 140, "Il tuo nome compare sulla scrivania.", TEXT_DIM);
                if let Some(f) = lay.field {
                    ui::text_field(c, f, &self.name, "Il tuo nome", true);
                }
            }
            2 => {
                res::draw_icon(c, "keyboard", 72, cx - 36, y);
                text::draw_centered(c, Font::Title, cx, y + 88, "Scegli la tastiera", TEXT);
                text::draw_centered(c, Font::Body, cx, y + 140, "Potrai cambiarla dopo nelle Impostazioni.", TEXT_DIM);
                for (i, o) in lay.options.iter().enumerate() {
                    let sel = (i == 0) == (self.layout == Layout::It);
                    option_tile(
                        c,
                        *o,
                        if i == 0 { "Italiano" } else { "English (US)" },
                        if i == 0 { "Layout italiano" } else { "US layout" },
                        sel,
                    );
                }
            }
            3 => {
                text::draw_centered(c, Font::Title, cx, y, "Scegli lo sfondo", TEXT);
                text::draw_centered(c, Font::Body, cx, y + 52, "Ne puoi scegliere un altro quando vuoi.", TEXT_DIM);
                for (i, o) in lay.options.iter().enumerate() {
                    let sel = i == self.wall;
                    if sel {
                        c.stroke_round(o.grow(4), 18, 3, 0xFFFF_FFFF);
                    }
                    draw_thumb(c, &self.thumbs[i], *o, 14);
                    text::draw_centered(
                        c,
                        Font::Strong,
                        o.x + o.w / 2,
                        o.bottom() + 12,
                        res::WALLPAPERS[i].1,
                        if sel { TEXT } else { TEXT_DIM },
                    );
                }
            }
            _ => {
                draw_logo(c, cx, y + 40, 35, 0xFFF2_EFE6);
                y += 110;
                let title = format!("Tutto pronto, {}!", self.name.trim());
                text::draw_centered(c, Font::Title, cx, y, &text::fit(Font::Title, &title, card.w - 60), TEXT);
                text::draw_centered(c, Font::Body, cx, y + 56, "justOS è configurato. Buon lavoro!", TEXT_DIM);
            }
        }
        let (mx, my) = self.mouse;
        let enabled = self.can_go_on();
        if let Some(b) = lay.back {
            ui::button(c, b, "Indietro", false, b.contains(mx, my));
        }
        if enabled {
            ui::button(c, lay.primary, self.primary_label(), true, lay.primary.contains(mx, my));
        } else {
            c.fill_round(lay.primary, lay.primary.h / 2, 0x40FF_FFFF);
            let (lh, _) = text::metrics(Font::Strong);
            text::draw_centered(
                c,
                Font::Strong,
                lay.primary.x + lay.primary.w / 2,
                lay.primary.y + (lay.primary.h - lh) / 2,
                self.primary_label(),
                0x80FF_FFFF,
            );
        }
        // Where we are: a dot per step, the current one wider.
        let total = STEPS as i32 * 14 + 14;
        let mut dx = cx - total / 2;
        for i in 0..STEPS {
            let wide = i == self.step;
            let w = if wide { 26 } else { 8 };
            c.fill_round(Rect::new(dx, card.bottom() - 30, w, 8), 4, if wide { 0xFFFF_FFFF } else { 0x55FF_FFFF });
            dx += w + 6;
        }
    }
}

fn option_tile(c: &mut Canvas, r: Rect, title: &str, sub: &str, selected: bool) {
    c.fill_round(r, 18, if selected { 0x40FF_FFFF } else { 0x1AFF_FFFF });
    c.stroke_round(r, 18, if selected { 2 } else { 1 }, if selected { 0xFFFF_FFFF } else { ui::BORDER });
    text::draw(c, Font::Heading, r.x + 20, r.y + 14, title, TEXT);
    text::draw(c, Font::Small, r.x + 20, r.y + 42, sub, TEXT_DIM);
    if selected {
        c.fill_circle(r.right() - 26, r.y + r.h / 2, 11, 0xFFFF_FFFF);
        c.fill_circle(r.right() - 26, r.y + r.h / 2, 5, 0xFF2F_7BEF);
    }
}

/// A picture with rounded corners.
pub fn draw_thumb(c: &mut Canvas, img: &Canvas, r: Rect, rad: i32) {
    c.blit_round_from(img, 0, 0, r, rad, 0);
}

pub fn run(d: &mut Display) {
    let (w, h) = (d.width(), d.height());
    let s = settings::get();
    let wall = res::WALLPAPERS.iter().position(|(n, _)| *n == s.wallpaper).unwrap_or(0);
    let (sharp, blur) = res::wallpaper(res::WALLPAPERS[wall].0, w, h);
    let thumbs = res::WALLPAPERS.iter().map(|(n, _)| res::wallpaper_thumb(n, 124, 78)).collect();
    let mut setup =
        Setup { step: 0, name: s.name.clone(), layout: s.layout, wall, w, h, sharp, blur, thumbs, mouse: (-1, -1), done: false };
    let mut l = Looper::new(d);
    l.damage_all(d);
    l.run(d, &mut setup);
    let _ = &setup.sharp;
}

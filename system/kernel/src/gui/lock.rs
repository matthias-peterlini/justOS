//! The lock screen: the time over the blurred wallpaper and, when a
//! password is set, a field to type it. Shown at start, with the logo key
//! and L, from the system menu and after some minutes without input.

use super::gfx::{Canvas, Rect};
use super::settings;
use super::text::{self, Font};
use super::ui::{self, Anim, TEXT, TEXT_DIM, TEXT_FAINT};
use crate::dev::input::{Key, KeyEvent};
use crate::time::DateTime;
use alloc::format;
use alloc::string::String;

const WEEKDAYS: [&str; 7] = ["Lunedì", "Martedì", "Mercoledì", "Giovedì", "Venerdì", "Sabato", "Domenica"];

pub struct Lock {
    input: String,
    /// A wrong password: the field shakes.
    wrong: Option<Anim>,
    /// Fading in.
    pub anim: Anim,
    /// Fading out after the right password.
    pub leaving: Option<Anim>,
}

/// What the lock screen wants after an event.
#[derive(PartialEq, Eq)]
pub enum Outcome {
    Stay,
    Redraw,
    Unlock,
}

impl Lock {
    pub fn new() -> Lock {
        Lock { input: String::new(), wrong: None, anim: Anim::new(260), leaving: None }
    }

    pub fn animating(&self) -> bool {
        !self.anim.done() || self.wrong.as_ref().is_some_and(|a| !a.done()) || self.leaving.is_some()
    }

    /// Gone after fading out.
    pub fn finished(&self) -> bool {
        self.leaving.as_ref().is_some_and(|a| a.done())
    }

    fn field(w: i32, h: i32) -> Rect {
        Rect::new(w / 2 - 150, h / 2 + 96, 300, 50)
    }

    fn try_unlock(&mut self) -> Outcome {
        if settings::check_password(&self.input) {
            self.leaving = Some(Anim::new(220));
            Outcome::Unlock
        } else {
            self.input.clear();
            self.wrong = Some(Anim::new(420));
            Outcome::Redraw
        }
    }

    pub fn key(&mut self, k: KeyEvent) -> Outcome {
        if self.leaving.is_some() {
            return Outcome::Stay;
        }
        if !settings::has_password() {
            return match k.key {
                Key::Enter | Key::Char(' ') | Key::Escape => self.try_unlock(),
                _ => Outcome::Stay,
            };
        }
        match k.key {
            Key::Enter => self.try_unlock(),
            Key::Escape => {
                self.input.clear();
                Outcome::Redraw
            }
            _ => {
                if ui::edit_text(&mut self.input, &k, 64) {
                    self.wrong = None;
                    Outcome::Redraw
                } else {
                    Outcome::Stay
                }
            }
        }
    }

    pub fn press(&mut self) -> Outcome {
        if self.leaving.is_none() && !settings::has_password() {
            return self.try_unlock();
        }
        Outcome::Stay
    }

    /// Draws the lock screen over the whole screen.
    pub fn draw(&self, c: &mut Canvas, blur: &Canvas, r: Rect, now: &DateTime) {
        let (w, h) = (blur.w, blur.h);
        let shown = match &self.leaving {
            Some(a) => 1000 - a.out(),
            None => self.anim.out(),
        };
        if shown < 1000 {
            // Fading: the desktop underneath shows through.
            let mut layer = Canvas::new_at(w, h, 0, blur.scale());
            self.paint(&mut layer, blur, Rect::new(0, 0, w, h), now);
            c.draw_scaled(&layer, Rect::new(0, 0, w, h), shown * 255 / 1000);
        } else {
            self.paint(c, blur, r, now);
        }
    }

    fn paint(&self, c: &mut Canvas, blur: &Canvas, r: Rect, now: &DateTime) {
        c.blit(blur, r.x, r.y, r);
        c.fill(r, 0x4D00_0000);
        let (w, h) = (blur.w, blur.h);
        let cx = w / 2;
        let date = format!("{} {} {}", WEEKDAYS[now.weekday as usize % 7], now.day, super::desktop::MONTHS[(now.month as usize + 11) % 12]);
        text::draw_centered(c, Font::Heading, cx, h / 2 - 210, &date, TEXT_DIM);
        text::draw_centered(c, Font::Clock, cx, h / 2 - 176, &format!("{:02}:{:02}", now.hour, now.min), TEXT);

        // The account: a round badge with the initial, and the name.
        let s = settings::get();
        let name = if s.name.is_empty() { String::from("justOS") } else { s.name.clone() };
        let ay = h / 2 - 10;
        c.fill_circle(cx, ay, 44, ui::ACCENT);
        let initial: String = name.chars().next().map(|ch| ch.to_uppercase().collect()).unwrap_or_default();
        let (lh, _) = text::metrics(Font::Title);
        text::draw_centered(c, Font::Title, cx, ay - lh / 2, &initial, 0xFFFF_FFFF);
        text::draw_centered(c, Font::Heading, cx, ay + 54, &name, TEXT);

        let mut field = Self::field(w, h);
        if let Some(a) = &self.wrong {
            if !a.done() {
                // A short shake, dying out.
                let t = a.t() as i32;
                let swing = [0, -12, 10, -8, 6, -3, 0];
                field.x += swing[(t * 6 / 1000).clamp(0, 6) as usize];
            }
        }
        if s.password.is_empty() {
            ui::button(c, Rect::new(cx - 90, field.y, 180, 46), "Entra", true, false);
            text::draw_centered(c, Font::Small, cx, field.bottom() + 18, "Premi Invio o fai clic", TEXT_FAINT);
        } else {
            let dots: String = self.input.chars().map(|_| '•').collect();
            ui::text_field(c, field, &dots, "Password", true);
            let (hint, color) = if self.wrong.is_some() { ("Password errata", ui::RED) } else { ("Scrivi la password e premi Invio", TEXT_FAINT) };
            text::draw_centered(c, Font::Small, cx, field.bottom() + 16, hint, color);
        }
    }
}

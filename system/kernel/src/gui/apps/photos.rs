//! Foto: the pictures in `sys:/home/Immagini` (and the wallpapers), as a
//! grid of thumbnails; one opens large, with the arrows going through them.

use crate::dev::input::{Key, KeyEvent};
use crate::fs;
use crate::gui::gfx::{Canvas, Rect};
use crate::gui::qoi;
use crate::gui::res;
use crate::gui::text::{self, Font};
use crate::gui::ui::{self, Anim};
use crate::gui::wm::{Action, App};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const THUMB: i32 = 150;
const GAP: i32 = 8;

struct Photo {
    path: String,
    name: String,
    thumb: Option<Canvas>,
    size: (i32, i32),
}

pub struct Photos {
    photos: Vec<Photo>,
    /// The picture shown large, its scaled copy and the zoom-in animation.
    open: Option<(usize, Option<Canvas>, Anim)>,
    scroll: i32,
    hover: Option<usize>,
    /// Visible thumbnails still to make.
    more: bool,
    /// The content area at the last drawing.
    last: Rect,
}

fn decode(path: &str) -> Option<Canvas> {
    let data = fs::abs(path).and_then(|p| fs::read_file(&p)).ok()?;
    qoi::decode(&data)
}

/// `img` scaled to fit `w` x `h` units, keeping its shape (with the screen's
/// pixels per unit).
fn fit(img: &Canvas, w: i32, h: i32) -> Canvas {
    let s = (w as i64 * 1000 / img.w as i64).min(h as i64 * 1000 / img.h as i64).min(1000);
    img.scaled_at(((img.w as i64 * s / 1000) as i32).max(1), ((img.h as i64 * s / 1000) as i32).max(1), crate::gui::scale())
}

/// `img` scaled and cropped to fill a square.
fn square(img: &Canvas, side: i32) -> Canvas {
    let m = img.w.min(img.h);
    let crop = img.crop(Rect::new((img.w - m) / 2, (img.h - m) / 2, m, m));
    crop.scaled_at(side, side, crate::gui::scale())
}

impl Photos {
    pub fn new() -> Photos {
        let mut photos = Vec::new();
        let mut add = |dir: &str| {
            let Ok(d) = fs::abs(dir) else { return };
            for (e, _) in fs::list_dir(&d).unwrap_or_default() {
                if e.name.to_ascii_lowercase().ends_with(".qoi") {
                    let path = format!("{}", d.join(&e.name));
                    let name = String::from(e.name.rsplit_once('.').map_or(e.name.as_str(), |(s, _)| s));
                    photos.push(Photo { path, name, thumb: None, size: (0, 0) });
                }
            }
        };
        add("sys:/home/Immagini");
        add("sys:/system/gui/wallpapers");
        Photos { photos, open: None, scroll: 0, hover: None, more: true, last: Rect::new(0, 0, 820, 540) }
    }

    fn grid(area: Rect) -> (Rect, i32) {
        let g = Rect::new(area.x + 16, area.y + 64, area.w - 32, area.h - 76);
        let cols = ((g.w + GAP) / (THUMB + GAP)).max(1);
        (g, cols)
    }

    fn tile(area: Rect, i: usize, scroll: i32) -> Rect {
        let (g, cols) = Self::grid(area);
        let side = (g.w - (cols - 1) * GAP) / cols;
        let (c, r) = (i as i32 % cols, i as i32 / cols);
        Rect::new(g.x + c * (side + GAP), g.y + r * (side + GAP) - scroll, side, side)
    }

    fn show(&mut self, i: usize, area: Rect) {
        let img = decode(&self.photos[i].path).map(|img| fit(&img, area.w - 40, area.h - 90));
        self.open = Some((i, img, Anim::new(260)));
    }

    fn arrows(area: Rect) -> [Rect; 3] {
        let y = area.y + 14;
        [Rect::new(area.x + 16, y, 36, 36), Rect::new(area.right() - 96, y, 36, 36), Rect::new(area.right() - 52, y, 36, 36)]
    }
}

impl App for Photos {
    fn title(&self) -> String {
        match &self.open {
            Some((i, _, _)) => self.photos[*i].name.clone(),
            None => String::from("Foto"),
        }
    }

    fn icon(&self) -> &str {
        "photos"
    }

    fn kind(&self) -> &'static str {
        "photos"
    }

    fn size(&self) -> (i32, i32) {
        (820, 540)
    }

    fn resizable(&self) -> bool {
        true
    }

    fn draw(&mut self, c: &mut Canvas, area: Rect, _blur: &Canvas, _focused: bool) {
        self.last = area;
        if let Some((i, img, anim)) = &self.open {
            let [back, prev, next] = Self::arrows(area);
            ui::icon_button(c, back, "g-grid", false);
            ui::icon_button(c, prev, "g-back", false);
            ui::icon_button(c, next, "g-forward", false);
            let p = &self.photos[*i];
            text::draw_centered(c, Font::Strong, area.x + area.w / 2, area.y + 16, &p.name, ui::text());
            text::draw_centered(c, Font::Small, area.x + area.w / 2, area.y + 38, &format!("{} di {}", i + 1, self.photos.len()), ui::text_dim());
            let stage = Rect::new(area.x + 20, area.y + 66, area.w - 40, area.h - 80);
            match img {
                Some(img) => {
                    let t = anim.out();
                    let full = Rect::new(stage.x + (stage.w - img.w) / 2, stage.y + (stage.h - img.h) / 2, img.w, img.h);
                    if t >= 1000 {
                        c.blit_round_from(img, 0, 0, full, 10, 0);
                    } else {
                        c.draw_scaled(img, ui::scale_rect(full, ui::lerp(820, 1000, t)), t * 255 / 1000);
                    }
                }
                None => text::draw_centered(c, Font::Body, stage.x + stage.w / 2, stage.y + stage.h / 2, "Immagine non leggibile", ui::text_dim()),
            }
            return;
        }
        text::draw(c, Font::Title, area.x + 20, area.y + 14, "Libreria", ui::text());
        text::draw_right(c, Font::Small, area.right() - 20, area.y + 28, &format!("{} foto", self.photos.len()), ui::text_dim());
        let (g, _) = Self::grid(area);
        if self.photos.is_empty() {
            res::draw_icon_faded(c, "photos", 72, area.x + area.w / 2 - 36, area.y + 160, 140);
            text::draw_centered(c, Font::Body, area.x + area.w / 2, area.y + 250, "Metti le immagini (QOI) in Immagini", ui::text_faint());
        }
        let saved = c.clip();
        c.set_clip(saved.intersect(&g));
        // Thumbnails are made on demand, a few per frame.
        let mut budget = 3;
        self.more = false;
        for i in 0..self.photos.len() {
            let t = Self::tile(area, i, self.scroll);
            if t.bottom() < g.y || t.y > g.bottom() {
                continue;
            }
            if self.photos[i].thumb.as_ref().is_some_and(|th| th.scale() != c.scale()) {
                // The interface changed size.
                self.photos[i].thumb = None;
            }
            if self.photos[i].thumb.is_none() {
                if budget > 0 {
                    budget -= 1;
                    let thumb = match decode(&self.photos[i].path) {
                        Some(img) => {
                            self.photos[i].size = (img.w, img.h);
                            square(&img, THUMB)
                        }
                        None => Canvas::new(THUMB, THUMB, 0xFF33_2A40),
                    };
                    self.photos[i].thumb = Some(thumb);
                } else {
                    self.more = true;
                }
            }
            match &self.photos[i].thumb {
                Some(th) => c.blit_round_from(th, (th.w - t.w).max(0) / 2, (th.h - t.h).max(0) / 2, t, 8, 0),
                None => c.fill_round(t, 8, ui::pane()),
            }
            if self.hover == Some(i) {
                c.fill_round(t, 8, 0x26FF_FFFF);
                let label = text::fit(Font::Small, &self.photos[i].name, t.w - 16);
                c.fill_round(Rect::new(t.x + 6, t.bottom() - 28, t.w - 12, 22), 8, 0x99_00_00_00);
                text::draw(c, Font::Small, t.x + 12, t.bottom() - 26, &label, ui::text());
            }
        }
        c.set_clip(saved);
    }

    fn press(&mut self, x: i32, y: i32, _double: bool, area: Rect) -> Action {
        let (ax, ay) = (area.x + x, area.y + y);
        if let Some((i, _, _)) = self.open {
            let [back, prev, next] = Self::arrows(area);
            let n = self.photos.len();
            if back.contains(ax, ay) {
                self.open = None;
            } else if prev.contains(ax, ay) {
                self.show((i + n - 1) % n, area);
            } else if next.contains(ax, ay) {
                self.show((i + 1) % n, area);
            }
            return Action::Redraw;
        }
        let (g, _) = Self::grid(area);
        if g.contains(ax, ay) {
            if let Some(i) = (0..self.photos.len()).find(|&i| Self::tile(area, i, self.scroll).contains(ax, ay)) {
                self.show(i, area);
                return Action::Redraw;
            }
        }
        Action::None
    }

    fn hover(&mut self, at: Option<(i32, i32)>, area: Rect) -> bool {
        if self.open.is_some() {
            return false;
        }
        let h = at.and_then(|(x, y)| (0..self.photos.len()).find(|&i| Self::tile(area, i, self.scroll).contains(area.x + x, area.y + y)));
        let changed = h != self.hover;
        self.hover = h;
        changed
    }

    fn key(&mut self, ev: KeyEvent) -> Action {
        let Some((i, _, _)) = self.open else { return Action::None };
        let n = self.photos.len();
        let area = self.last;
        match ev.key {
            Key::Escape => self.open = None,
            Key::Left => self.show((i + n - 1) % n, area),
            Key::Right | Key::Char(' ') => self.show((i + 1) % n, area),
            _ => return Action::None,
        }
        Action::Redraw
    }

    fn scroll(&mut self, lines: i32) -> bool {
        if self.open.is_some() {
            return false;
        }
        let old = self.scroll;
        let rows = (self.photos.len() as i32 + 4) / 5;
        self.scroll = (self.scroll + lines * 50).clamp(0, (rows * (THUMB + GAP) - 300).max(0));
        old != self.scroll
    }

    fn animating(&self) -> bool {
        self.open.as_ref().is_some_and(|(_, _, a)| !a.done()) || (self.more && self.open.is_none())
    }

    fn frame(&mut self) -> bool {
        true
    }
}

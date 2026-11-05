//! A picture (QOI) in a window.

use crate::fs;
use crate::gui::gfx::{Canvas, Rect};
use crate::gui::qoi;
use crate::gui::text::{self, Font};
use crate::gui::ui::TEXT_DIM;
use crate::gui::wm::App;
use alloc::string::String;

pub struct Viewer {
    name: String,
    img: Option<Canvas>,
}

impl Viewer {
    pub fn new(path: &str) -> Viewer {
        let img = fs::abs(path).and_then(|p| fs::read_file(&p)).ok().and_then(|d| qoi::decode(&d)).map(|img| {
            // Fit 640 x 420 units (with the screen's pixels per unit).
            let (w, h) = (img.w, img.h);
            let s = (640 * 1000 / w).min(420 * 1000 / h).min(1000);
            img.scaled_at((w * s / 1000).max(1), (h * s / 1000).max(1), crate::gui::scale())
        });
        let name = String::from(path.rsplit('/').next().unwrap_or(path));
        Viewer { name, img }
    }
}

impl App for Viewer {
    fn title(&self) -> String {
        self.name.clone()
    }

    fn icon(&self) -> &str {
        "image-viewer"
    }

    fn kind(&self) -> &'static str {
        "viewer"
    }

    fn size(&self) -> (i32, i32) {
        match &self.img {
            Some(i) => (i.w + 24, i.h + 24),
            None => (360, 160),
        }
    }

    fn draw(&mut self, c: &mut Canvas, area: Rect, _blur: &Canvas, _focused: bool) {
        match &self.img {
            Some(img) => {
                let r = Rect::new(area.x + 12, area.y + 12, img.w, img.h);
                c.blit_round_from(img, 0, 0, r, 12, 0);
            }
            None => text::draw_centered(c, Font::Body, area.x + area.w / 2, area.y + 60, "Immagine non leggibile", TEXT_DIM),
        }
    }
}

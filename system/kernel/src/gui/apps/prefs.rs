//! Impostazioni: name, keyboard, wallpaper, screen resolution, sound, the
//! first-start setup, and Privacy: what each app may use outside its
//! sandbox ([`crate::sandbox`]).

use crate::dev::input::{Key, KeyEvent};
use crate::dev::keymap::Layout;
use crate::gui::gfx::{Canvas, Rect};
use crate::gui::res;
use crate::gui::settings;
use crate::gui::text::{self, Font};
use crate::gui::ui;
use crate::gui::wm::{Action, App};
use crate::sandbox::Grants;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use justdk::DisplayMode;

/// Resolutions offered when the display has them, in this order.
const COMMON_MODES: &[(u32, u32)] = &[
    (1024, 768),
    (1280, 720),
    (1280, 800),
    (1366, 768),
    (1440, 900),
    (1600, 900),
    (1680, 1050),
    (1920, 1080),
    (1920, 1200),
    (2560, 1440),
    (2560, 1600),
    (3840, 2160),
];
/// Where the right column starts.
const RIGHT: i32 = 492;

pub struct Prefs {
    name: String,
    editing: bool,
    thumbs: Vec<Canvas>,
    note: &'static str,
    /// Resolutions the display driver offers (empty without one).
    modes: Vec<DisplayMode>,
    /// The volume slider is being dragged.
    sliding: bool,
    /// A new password being typed.
    password: String,
    editing_password: bool,
    /// The Privacy page is shown, with what the apps were allowed.
    privacy: Option<Vec<Grants>>,
    scroll: i32,
}

/// A line of the Privacy page.
enum Row {
    App(usize),
    Perm(usize, usize),
}

/// The choices for locking after a while without input (minutes; 0: never).
const LOCK_AFTER: [(u32, &str); 4] = [(0, "Mai"), (2, "2 min"), (5, "5 min"), (15, "15 min")];

/// The sizes of the interface (pixels per unit; 0: by the screen).
const SIZES: [(u32, &str); 3] = [(0, "Auto"), (1, "100%"), (2, "200%")];

/// The themes of windows and panels.
const THEMES: [(ui::Theme, &str); 2] = [(ui::Theme::Dark, "Scuro"), (ui::Theme::Light, "Chiaro")];

impl Prefs {
    pub fn new() -> Prefs {
        let thumbs = Self::thumbs();
        let offered = crate::dev::display::modes();
        let modes =
            COMMON_MODES.iter().map(|&(width, height)| DisplayMode { width, height }).filter(|m| offered.contains(m)).take(12).collect();
        Prefs {
            name: settings::get().name,
            editing: false,
            thumbs,
            note: "",
            modes,
            sliding: false,
            password: String::new(),
            editing_password: false,
            privacy: None,
            scroll: 0,
        }
    }

    fn thumbs() -> Vec<Canvas> {
        res::WALLPAPERS.iter().map(|(n, _)| res::wallpaper_thumb(n, 104, 65)).collect()
    }

    /// The choice of the interface size, next to the "Schermo" heading.
    fn sizes(area: Rect) -> Rect {
        Rect::new(area.x + RIGHT + 150, area.y + 12, 214, 32)
    }

    /// The choice of the theme, next to the "Sfondo" heading.
    fn themes(area: Rect) -> Rect {
        Rect::new(area.x + 296, area.y + 202, 180, 32)
    }

    fn privacy_btn(area: Rect) -> Rect {
        Rect::new(area.x + 248, area.y + 466, 200, 38)
    }

    fn back_btn(area: Rect) -> Rect {
        Rect::new(area.x + 20, area.y + 16, 28, 28)
    }

    /// Where the Privacy page's lines go.
    fn list_area(area: Rect) -> Rect {
        Rect::new(area.x + 16, area.y + 92, area.w - 32, area.h - 100)
    }

    fn rows(grants: &[Grants], area: Rect, scroll: i32) -> Vec<(Rect, Row)> {
        let list = Self::list_area(area);
        let mut v = Vec::new();
        let mut y = list.y - scroll;
        for (i, g) in grants.iter().enumerate() {
            v.push((Rect::new(list.x, y, list.w, 44), Row::App(i)));
            y += 48;
            for j in 0..g.perms.len() {
                v.push((Rect::new(list.x + 16, y, list.w - 32, 38), Row::Perm(i, j)));
                y += 42;
            }
            y += 14;
        }
        v
    }

    fn toggle_of(r: Rect) -> Rect {
        Rect::new(r.right() - 60, r.y + 4, 52, 30)
    }

    fn forget_of(r: Rect) -> Rect {
        Rect::new(r.right() - 130, r.y + 4, 122, 36)
    }

    fn draw_privacy(&self, c: &mut Canvas, area: Rect, grants: &[Grants]) {
        ui::icon_button(c, Self::back_btn(area), "g-back", false);
        text::draw(c, Font::Strong, area.x + 60, area.y + 20, "Privacy", ui::text());
        text::draw(
            c,
            Font::Small,
            area.x + 24,
            area.y + 58,
            "Le app del Negozio vedono solo la propria cartella. Qui scegli a cos'altro possono accedere.",
            ui::text_dim(),
        );
        let list = Self::list_area(area);
        if grants.is_empty() {
            text::draw_centered(c, Font::Body, list.x + list.w / 2, list.y + 60, "Nessuna app ha ancora chiesto qualcosa.", ui::text_faint());
            return;
        }
        let saved = c.clip();
        c.set_clip(saved.intersect(&list));
        for (r, row) in Self::rows(grants, area, self.scroll) {
            if r.bottom() < list.y || r.y > list.bottom() {
                continue;
            }
            match row {
                Row::App(i) => {
                    let g = &grants[i];
                    c.fill_round(r, 12, 0x22FF_FFFF);
                    let name = if g.name.is_empty() { g.id.as_str() } else { g.name.as_str() };
                    let w = text::draw(c, Font::Strong, r.x + 14, r.y + 11, name, ui::text());
                    text::draw(c, Font::Small, r.x + 14 + w + 10, r.y + 14, &g.id, ui::text_faint());
                    ui::button(c, Self::forget_of(r), "Chiedi di nuovo", false, false);
                }
                Row::Perm(i, j) => {
                    let (perm, allow) = &grants[i].perms[j];
                    let what = crate::sandbox::describe(perm);
                    let mut label = String::new();
                    let mut chars = what.chars();
                    if let Some(first) = chars.next() {
                        label.extend(first.to_uppercase());
                        label.push_str(chars.as_str());
                    }
                    text::draw(c, Font::Body, r.x + 8, r.y + 9, &label, ui::text());
                    text::draw_right(c, Font::Small, r.right() - 72, r.y + 11, if *allow { "consentito" } else { "negato" }, ui::text_faint());
                    ui::toggle(c, Self::toggle_of(r), *allow);
                }
            }
        }
        c.set_clip(saved);
    }

    fn press_privacy(&mut self, ax: i32, ay: i32, area: Rect) -> Action {
        if Self::back_btn(area).contains(ax, ay) {
            self.privacy = None;
            return Action::Redraw;
        }
        let Some(grants) = &self.privacy else { return Action::None };
        if !Self::list_area(area).contains(ax, ay) {
            return Action::None;
        }
        for (r, row) in Self::rows(grants, area, self.scroll) {
            match row {
                Row::App(i) if Self::forget_of(r).contains(ax, ay) => {
                    crate::sandbox::forget(&grants[i].id);
                }
                Row::Perm(i, j) if Self::toggle_of(r).contains(ax, ay) => {
                    let g = &grants[i];
                    let (perm, allow) = &g.perms[j];
                    crate::sandbox::set(&g.id, &g.name, perm, Some(!allow));
                }
                _ => continue,
            }
            self.privacy = Some(crate::sandbox::all());
            return Action::Redraw;
        }
        Action::None
    }

    fn mode_btn(area: Rect, i: usize) -> Rect {
        Rect::new(area.x + RIGHT + (i as i32 % 3) * 124, area.y + 70 + (i as i32 / 3) * 40, 116, 34)
    }

    fn volume_bar(area: Rect) -> Rect {
        Rect::new(area.x + RIGHT + 34, area.y + 268, 236, 24)
    }

    fn chime_switch(area: Rect) -> Rect {
        Rect::new(area.x + RIGHT + 312, area.y + 314, 52, 30)
    }

    fn test_btn(area: Rect) -> Rect {
        Rect::new(area.x + RIGHT + 282, area.y + 264, 82, 32)
    }

    fn set_volume(v: i32) {
        crate::gui::change_volume(v.clamp(0, 100) as u32);
    }

    fn password_field(area: Rect) -> Rect {
        Rect::new(area.x + RIGHT, area.y + 396, 230, 44)
    }

    fn password_btn(area: Rect) -> Rect {
        Rect::new(area.x + RIGHT + 240, area.y + 399, 124, 38)
    }

    fn lock_after(area: Rect) -> Rect {
        Rect::new(area.x + RIGHT + 112, area.y + 454, 252, 32)
    }

    fn save_password(&mut self) {
        let removing = self.password.is_empty();
        if removing && !settings::has_password() {
            self.editing_password = true; // nothing to remove: type one
            return;
        }
        settings::set_password(&self.password);
        self.password.clear();
        self.editing_password = false;
        self.note = if removing { "Password tolta" } else { "Password impostata" };
    }

    fn field(area: Rect) -> Rect {
        Rect::new(area.x + 24, area.y + 48, 300, 46)
    }

    fn save_btn(area: Rect) -> Rect {
        Rect::new(area.x + 336, area.y + 52, 110, 38)
    }

    fn layout_btns(area: Rect) -> [Rect; 2] {
        [Rect::new(area.x + 24, area.y + 146, 150, 38), Rect::new(area.x + 184, area.y + 146, 150, 38)]
    }

    /// Four wallpapers to a row.
    fn thumb(area: Rect, i: usize) -> Rect {
        let i = i as i32;
        Rect::new(area.x + 24 + (i % 4) * 116, area.y + 238 + (i / 4) * 94, 104, 65)
    }

    fn redo_btn(area: Rect) -> Rect {
        Rect::new(area.x + 24, area.y + 466, 214, 38)
    }

    fn save_name(&mut self) {
        let name = String::from(self.name.trim());
        settings::update(|s| s.name = name);
        self.editing = false;
        self.note = "Nome salvato";
    }
}

impl App for Prefs {
    fn title(&self) -> String {
        String::from("Impostazioni")
    }

    fn icon(&self) -> &str {
        "settings"
    }

    fn kind(&self) -> &'static str {
        "settings"
    }

    fn size(&self) -> (i32, i32) {
        (880, 530)
    }

    fn draw(&mut self, c: &mut Canvas, area: Rect, _blur: &Canvas, _focused: bool) {
        if let Some(grants) = &self.privacy {
            self.draw_privacy(c, area, grants);
            return;
        }
        if self.thumbs.first().is_some_and(|t| t.scale() != c.scale()) {
            // The interface changed size: sharper (or smaller) previews.
            self.thumbs = Self::thumbs();
        }
        let s = settings::get();
        text::draw(c, Font::Strong, area.x + 24, area.y + 20, "Il tuo nome", ui::text());
        ui::text_field(c, Self::field(area), &self.name, "Nome", self.editing);
        ui::button(c, Self::save_btn(area), "Salva", true, false);

        text::draw(c, Font::Strong, area.x + 24, area.y + 118, "Tastiera", ui::text());
        for (i, b) in Self::layout_btns(area).iter().enumerate() {
            let sel = (i == 0) == (s.layout == Layout::It);
            ui::button(c, *b, if i == 0 { "Italiano" } else { "English (US)" }, sel, false);
        }

        text::draw(c, Font::Strong, area.x + 24, area.y + 210, "Sfondo", ui::text());
        let tr = Self::themes(area);
        text::draw_right(c, Font::Body, tr.x - 12, area.y + 210, "Tema", ui::text_dim());
        let sel = THEMES.iter().position(|(t, _)| *t == ui::theme()).unwrap_or(0);
        let labels: Vec<&str> = THEMES.iter().map(|(_, l)| *l).collect();
        ui::segmented(c, tr, &labels, sel);
        for (i, (name, label)) in res::WALLPAPERS.iter().enumerate() {
            let r = Self::thumb(area, i);
            if *name == s.wallpaper {
                c.stroke_round(r.grow(3), 14, 2, 0xFFFF_FFFF);
            }
            c.blit_round_from(&self.thumbs[i], 0, 0, r, 12, 0);
            text::draw_centered(c, Font::Small, r.x + r.w / 2, r.bottom() + 8, label, ui::text_dim());
        }

        text::draw(c, Font::Strong, area.x + 24, area.y + 438, "Sistema", ui::text());
        ui::button(c, Self::redo_btn(area), "Configurazione iniziale", false, false);
        ui::button(c, Self::privacy_btn(area), "Privacy delle app", false, false);
        if !self.note.is_empty() {
            text::draw(c, Font::Small, area.x + 24, area.y + 510, self.note, ui::text_faint());
        }

        // Screen
        let x = area.x + RIGHT;
        text::draw(c, Font::Strong, x, area.y + 20, "Schermo", ui::text());
        if crate::gui::can_double() {
            let sel = SIZES.iter().position(|(k, _)| *k == s.scale).unwrap_or(0);
            let labels: Vec<&str> = SIZES.iter().map(|(_, l)| *l).collect();
            ui::segmented(c, Self::sizes(area), &labels, sel);
        }
        let (w, h) = crate::console::pixel_size().unwrap_or((0, 0));
        let current = DisplayMode { width: w as u32, height: h as u32 };
        if self.modes.is_empty() {
            text::draw(c, Font::Small, x, area.y + 48, &format!("{w} × {h}"), ui::text_dim());
            text::draw(c, Font::Small, x, area.y + 72, "Risoluzione scelta dal firmware:", ui::text_faint());
            text::draw(c, Font::Small, x, area.y + 92, "nessun driver video la può cambiare.", ui::text_faint());
        } else {
            let name = crate::dev::display::controller().unwrap_or_default();
            text::draw(c, Font::Small, x, area.y + 48, &text::fit(Font::Small, &name, 360), ui::text_dim());
            for (i, m) in self.modes.iter().enumerate() {
                let label = format!("{}×{}", m.width, m.height);
                ui::button(c, Self::mode_btn(area, i), &label, *m == current, false);
            }
        }

        // Sound
        text::draw(c, Font::Strong, x, area.y + 236, "Suono", ui::text());
        if crate::dev::audio::available() {
            let v = crate::dev::audio::volume();
            let bar = Self::volume_bar(area);
            res::draw_icon(c, if v == 0 { "g-mute" } else { "g-volume" }, 18, x, bar.y + 3);
            ui::slider(c, bar, v * 10);
            ui::button(c, Self::test_btn(area), "Prova", false, false);
            text::draw(c, Font::Body, x, area.y + 319, "Suono all'avvio", ui::text());
            ui::toggle(c, Self::chime_switch(area), s.startup_sound);
        } else {
            text::draw(c, Font::Small, x, area.y + 264, "Nessuna scheda audio trovata.", ui::text_faint());
        }

        // Security
        text::draw(c, Font::Strong, x, area.y + 368, "Password", ui::text());
        let has = !s.password.is_empty();
        let dots: String = self.password.chars().map(|_| '•').collect();
        ui::text_field(c, Self::password_field(area), &dots, if has { "Nuova password" } else { "Nessuna" }, self.editing_password);
        let label = if self.editing_password || !has { "Imposta" } else { "Togli" };
        ui::button(c, Self::password_btn(area), label, self.editing_password, false);
        text::draw(c, Font::Body, x, area.y + 460, "Blocca dopo", ui::text());
        let sel = LOCK_AFTER.iter().position(|(m, _)| *m == s.lock_after).unwrap_or(2);
        let labels: Vec<&str> = LOCK_AFTER.iter().map(|(_, l)| *l).collect();
        ui::segmented(c, Self::lock_after(area), &labels, sel);
    }

    fn press(&mut self, x: i32, y: i32, _double: bool, area: Rect) -> Action {
        let (ax, ay) = (area.x + x, area.y + y);
        if self.privacy.is_some() {
            return self.press_privacy(ax, ay, area);
        }
        self.note = "";
        if Self::privacy_btn(area).contains(ax, ay) {
            self.privacy = Some(crate::sandbox::all());
            self.scroll = 0;
            self.editing = false;
            self.editing_password = false;
            return Action::Redraw;
        }
        if Self::field(area).contains(ax, ay) {
            self.editing = true;
            return Action::Redraw;
        }
        if Self::save_btn(area).contains(ax, ay) {
            self.save_name();
            return Action::Redraw;
        }
        self.editing = false;
        if Self::password_field(area).contains(ax, ay) {
            self.editing_password = true;
            return Action::Redraw;
        }
        if Self::password_btn(area).contains(ax, ay) {
            // "Togli" with an empty field removes the password.
            self.save_password();
            return Action::Redraw;
        }
        self.editing_password = false;
        for (i, seg) in ui::segments(Self::lock_after(area), LOCK_AFTER.len()).iter().enumerate() {
            if seg.contains(ax, ay) {
                settings::update(|s| s.lock_after = LOCK_AFTER[i].0);
                return Action::Redraw;
            }
        }
        for (i, seg) in ui::segments(Self::themes(area), THEMES.len()).iter().enumerate() {
            if seg.contains(ax, ay) {
                settings::update(|s| s.theme = THEMES[i].0);
                // Every window and panel changes colour.
                return Action::RedrawAll;
            }
        }
        for (i, b) in Self::layout_btns(area).iter().enumerate() {
            if b.contains(ax, ay) {
                settings::update(|s| s.layout = if i == 0 { Layout::It } else { Layout::Us });
                self.note = "Tastiera cambiata";
                return Action::Redraw;
            }
        }
        for (i, (name, _)) in res::WALLPAPERS.iter().enumerate() {
            if Self::thumb(area, i).contains(ax, ay) {
                settings::update(|s| s.wallpaper = String::from(*name));
                return Action::Wallpaper;
            }
        }
        if crate::gui::can_double() {
            for (i, seg) in ui::segments(Self::sizes(area), SIZES.len()).iter().enumerate() {
                if seg.contains(ax, ay) {
                    self.note = match crate::gui::change_scale(SIZES[i].0) {
                        Ok(()) => "",
                        Err(_) => "Dimensione non disponibile",
                    };
                    return Action::Redraw;
                }
            }
        }
        for (i, m) in self.modes.iter().enumerate() {
            if Self::mode_btn(area, i).contains(ax, ay) {
                self.note = match crate::gui::change_resolution(*m) {
                    Ok(()) => "",
                    Err(_) => "Risoluzione non disponibile",
                };
                return Action::Redraw;
            }
        }
        if crate::dev::audio::available() {
            if Self::volume_bar(area).grow(8).contains(ax, ay) {
                self.sliding = true;
                Self::set_volume((ui::slider_value(Self::volume_bar(area), ax) / 10) as i32);
                return Action::Redraw;
            }
            if Self::chime_switch(area).contains(ax, ay) {
                settings::update(|s| s.startup_sound = !s.startup_sound);
                return Action::Redraw;
            }
            if Self::test_btn(area).contains(ax, ay) {
                let _ = crate::dev::audio::play(crate::dev::audio::tone(660, 300));
                return Action::None;
            }
        }
        if Self::redo_btn(area).contains(ax, ay) {
            settings::update(|s| s.setup_done = false);
            self.note = "Al prossimo avvio";
            return Action::Redraw;
        }
        Action::Redraw
    }

    fn drag(&mut self, x: i32, _y: i32, area: Rect) -> bool {
        if self.sliding {
            Self::set_volume((ui::slider_value(Self::volume_bar(area), area.x + x) / 10) as i32);
        }
        self.sliding
    }

    fn release(&mut self, _area: Rect) -> Action {
        self.sliding = false;
        Action::None
    }

    fn scroll(&mut self, lines: i32) -> bool {
        let Some(grants) = &self.privacy else { return false };
        let rows: i32 = grants.iter().map(|g| 62 + 42 * g.perms.len() as i32).sum();
        let old = self.scroll;
        self.scroll = (self.scroll + lines * 30).clamp(0, (rows - 380).max(0));
        old != self.scroll
    }

    fn key(&mut self, ev: KeyEvent) -> Action {
        if self.editing_password {
            if ev.key == Key::Enter {
                self.save_password();
            } else {
                ui::edit_text(&mut self.password, &ev, 64);
            }
            return Action::Redraw;
        }
        if !self.editing {
            return Action::None;
        }
        if ev.key == Key::Enter {
            self.save_name();
        } else {
            ui::edit_text(&mut self.name, &ev, 32);
        }
        Action::Redraw
    }
}

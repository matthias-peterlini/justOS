//! Info sistema: what this computer is, its devices and drivers, and its
//! volumes.

use crate::gui::gfx::{Canvas, Rect};
use crate::gui::res;
use crate::gui::splash::draw_mark;
use crate::gui::text::{self, Font};
use crate::gui::ui;
use crate::gui::wm::{Action, App};
use crate::util::Size;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

const ROW: i32 = 46;

pub struct About {
    tab: usize,
    scroll: i32,
}

impl About {
    pub fn new() -> About {
        About { tab: 0, scroll: 0 }
    }

    fn tabs(area: Rect) -> Rect {
        Rect::new(area.x + area.w / 2 - 170, area.y + 14, 340, 32)
    }

    fn list(area: Rect) -> Rect {
        Rect::new(area.x + 14, area.y + 62, area.w - 28, area.h - 72)
    }

    fn update_btn(area: Rect) -> Rect {
        Rect::new(area.x + 250, area.y + 348, 170, 34)
    }

    fn second_btn(area: Rect) -> Rect {
        let b = Self::update_btn(area);
        Rect::new(b.right() + 10, b.y, 170, b.h)
    }
}

/// What the second button next to the update button does.
enum Second {
    /// Keep this version (an update or a change of slot waits).
    Cancel,
    /// Start the version in the other slot (it worked).
    Switch(String),
}

fn second_action() -> Option<Second> {
    use crate::update::State;
    match crate::update::state() {
        State::Staged(_) => Some(Second::Cancel),
        State::Checking | State::Downloading { .. } | State::Installing { .. } => None,
        // On trial, a version that does not work goes back by itself.
        _ if crate::sysupdate::on_trial() => None,
        _ => crate::sysupdate::slots().previous.filter(|p| p != crate::VERSION).map(Second::Switch),
    }
}

fn percent(done: u64, total: u64) -> u64 {
    (done * 100 / total.max(1)).min(100)
}

/// An icon for a kind of device.
fn device_icon(d: &crate::dev::Device) -> &'static str {
    let i = &d.info;
    match (i.bus, i.class) {
        (justdk::BUS_USB, 0x08) | (justdk::BUS_SD, _) => "usb",
        (justdk::BUS_USB, 0x03) => "keyboard",
        (_, 0x01) => "archive",
        (_, 0x02) => "wifi",
        (_, 0x03) => "screenshot",
        (_, 0x04) => "music",
        (_, 0x0C) => "usb",
        _ => "computer",
    }
}

impl App for About {
    fn title(&self) -> String {
        String::from("Info sistema")
    }

    fn icon(&self) -> &str {
        "info"
    }

    fn kind(&self) -> &'static str {
        "about"
    }

    fn size(&self) -> (i32, i32) {
        (700, 470)
    }

    fn draw(&mut self, c: &mut Canvas, area: Rect, _blur: &Canvas, _focused: bool) {
        ui::segmented(c, Self::tabs(area), &["Panoramica", "Dispositivi", "Dischi"], self.tab);
        match self.tab {
            0 => {
                let cx = area.x + 130;
                c.fill_circle(cx, area.y + 170, 74, 0xFF14_1414);
                draw_mark(c, cx, area.y + 170, 56, 0xFFF2_EFE6);
                let x = area.x + 250;
                text::draw(c, Font::Title, x, area.y + 76, "justOS", ui::text());
                text::draw(c, Font::Body, x, area.y + 116, &format!("Versione {}", crate::VERSION), ui::text_dim());
                let mut brand = [0u8; 48];
                let cpu = match crate::arch::smp::count() {
                    1 => crate::arch::cpu::brand(&mut brand).trim().to_string(),
                    n => format!("{} · {n} core", crate::arch::cpu::brand(&mut brand).trim()),
                };
                let (free, total) = crate::mem::frame::stats();
                let up = crate::time::uptime_ms() / 1000;
                let (sw, sh) = crate::console::pixel_size().unwrap_or((0, 0));
                let video = crate::dev::display::controller().unwrap_or_else(|| String::from("Framebuffer del firmware"));
                let sound = crate::dev::audio::list().first().map(|o| o.name.clone()).unwrap_or_else(|| String::from("Nessuna"));
                let mut rows = alloc::vec![
                    ("Processore", cpu),
                    ("Memoria", format!("{} ({} libera)", Size(total), Size(free))),
                    ("Schermo", format!("{sw} × {sh}")),
                    ("Grafica", video),
                    ("Audio", sound),
                ];
                // Laptops: the battery and the temperature, from ACPI.
                let mut power = Vec::new();
                if let Some((pct, plugged)) = crate::acpi::power::battery() {
                    power.push(format!("Batteria {pct}%{}", if plugged { ", collegata" } else { "" }));
                }
                if let Some(t) = crate::acpi::power::hottest() {
                    power.push(format!("{},{} °C", t / 10, t % 10));
                }
                if !power.is_empty() {
                    rows.push(("Energia", power.join(" · ")));
                }
                rows.push(("Acceso da", format!("{} h {} min {} s", up / 3600, up / 60 % 60, up % 60)));
                let mut y = area.y + 150;
                for (k, v) in rows {
                    text::draw(c, Font::Strong, x, y, k, ui::text());
                    text::draw(c, Font::Body, x + 110, y, &text::fit(Font::Body, &v, area.right() - x - 130), ui::text_dim());
                    y += 27;
                }
                use crate::update::State;
                let state = crate::update::state();
                let b = Self::update_btn(area);
                let label = match &state {
                    State::Checking => String::from("Controllo…"),
                    State::Available(v) => format!("Installa {v}"),
                    State::Downloading { done, total, .. } => format!("Scarico… {}%", percent(*done, *total)),
                    State::Installing { done, total, .. } => format!("Installo… {}%", percent(*done, *total)),
                    State::Staged(_) => String::from("Riavvia"),
                    _ => String::from("Cerca aggiornamenti"),
                };
                ui::button(c, b, &label, matches!(state, State::Available(_) | State::Staged(_)), false);
                match second_action() {
                    Some(Second::Cancel) => ui::button(c, Self::second_btn(area), "Annulla", false, false),
                    Some(Second::Switch(v)) => {
                        let verb = if crate::update::newer(&v, crate::VERSION) { "Passa a" } else { "Torna a" };
                        ui::button(c, Self::second_btn(area), &format!("{verb} {v}"), false, false);
                    }
                    None => {}
                }
                let (line, color) = match &state {
                    State::NotChecked | State::Checking => (String::new(), ui::text_faint()),
                    State::UpToDate => (String::from("justOS è aggiornato."), ui::GREEN),
                    State::Available(v) => (format!("È disponibile justOS {v}."), ui::ORANGE),
                    State::Released(v) => (format!("justOS {v} è nelle release su GitHub."), ui::ORANGE),
                    State::Downloading { version, done, total } => (format!("Scarico justOS {version}: {} di {}", Size(*done), Size(*total)), ui::text_dim()),
                    State::Installing { version, .. } => (format!("Scrivo justOS {version} accanto a questo sistema…"), ui::text_dim()),
                    State::Staged(v) => (format!("justOS {v} parte al prossimo riavvio."), ui::GREEN),
                    State::Failed(e) => (format!("Non riuscito: {e}"), ui::text_faint()),
                };
                text::draw(c, Font::Small, b.x + 4, b.bottom() + 10, &text::fit(Font::Small, &line, area.right() - b.x - 20), color);
                text::draw_centered(c, Font::Small, area.x + area.w / 2, area.bottom() - 30, "Scritto da zero, in Rust · © Matthias Peterlini", ui::text_faint());
            }
            1 => {
                let l = Self::list(area);
                let saved = c.clip();
                c.set_clip(saved.intersect(&l));
                let devs: Vec<crate::dev::Device> = crate::dev::list().into_iter().filter(|d| !d.gone && d.needs_driver()).collect();
                for (i, d) in devs.iter().enumerate() {
                    let r = Rect::new(l.x, l.y + i as i32 * ROW - self.scroll, l.w, ROW - 4);
                    if r.bottom() < l.y || r.y > l.bottom() {
                        continue;
                    }
                    c.fill_round(r, 10, ui::pane());
                    res::draw_icon(c, device_icon(d), 28, r.x + 10, r.y + 7);
                    text::draw(c, Font::Strong, r.x + 48, r.y + 4, &text::fit(Font::Strong, &d.description(), r.w - 200), ui::text());
                    text::draw(c, Font::Small, r.x + 48, r.y + 24, &d.location(), ui::text_faint());
                    let (label, color) = match (&d.driver, d.builtin, d.has_driver()) {
                        (Some(drv), _, _) => (drv.clone(), ui::GREEN),
                        (None, Some(b), _) => (String::from(b), ui::GREEN),
                        (_, _, true) => (String::from("gestito"), ui::GREEN),
                        _ => (String::from("senza driver"), ui::ORANGE),
                    };
                    c.fill_circle(r.right() - 130, r.y + r.h / 2, 4, color);
                    text::draw(c, Font::Small, r.right() - 120, r.y + 13, &text::fit(Font::Small, &label, 110), ui::text_dim());
                }
                c.set_clip(saved);
            }
            _ => {
                let l = Self::list(area);
                let mut y = l.y;
                for v in crate::fs::volumes() {
                    let (total, free) = v.space();
                    let r = Rect::new(l.x, y, l.w, 74);
                    c.fill_round(r, 12, ui::pane());
                    res::draw_icon(c, if v.source == "memory" { "computer" } else { "archive" }, 40, r.x + 12, r.y + 16);
                    let name = if v.label.is_empty() { format!("{}:", v.name) } else { format!("{} ({}:)", v.label, v.name) };
                    text::draw(c, Font::Strong, r.x + 66, r.y + 10, &name, ui::text());
                    let ro = if v.read_only() { " · sola lettura" } else { "" };
                    text::draw_right(c, Font::Small, r.right() - 14, r.y + 12, &format!("{} liberi su {}{ro}", Size(free), Size(total)), ui::text_dim());
                    let bar = Rect::new(r.x + 66, r.y + 42, r.w - 82, 10);
                    c.fill_round(bar, 5, ui::cell());
                    let used = total.saturating_sub(free);
                    let w = (bar.w as u64 * used / total.max(1)) as i32;
                    c.fill_round(Rect::new(bar.x, bar.y, w.max(10), bar.h), 5, ui::ACCENT);
                    text::draw(c, Font::Small, r.x + 66, r.y + 54, &v.source, ui::text_faint());
                    y += 84;
                }
            }
        }
    }

    fn press(&mut self, x: i32, y: i32, _double: bool, area: Rect) -> Action {
        for (i, s) in ui::segments(Self::tabs(area), 3).iter().enumerate() {
            if s.contains(area.x + x, area.y + y) {
                self.tab = i;
                self.scroll = 0;
                return Action::Redraw;
            }
        }
        if self.tab != 0 {
            return Action::None;
        }
        use crate::update::State;
        if Self::update_btn(area).contains(area.x + x, area.y + y) {
            match crate::update::state() {
                State::Available(_) => crate::update::install_in_background(),
                State::Staged(_) => crate::acpi::reboot(),
                State::Checking | State::Downloading { .. } | State::Installing { .. } => {}
                _ => crate::update::check(false),
            }
            return Action::Redraw;
        }
        if Self::second_btn(area).contains(area.x + x, area.y + y) {
            let done = match second_action() {
                Some(Second::Cancel) => crate::sysupdate::cancel(),
                Some(Second::Switch(_)) => crate::sysupdate::rollback().map(|_| ()),
                None => return Action::None,
            };
            if let Err(e) = done {
                crate::gui::notify("Sistema", &e, "about");
            }
            return Action::Redraw;
        }
        Action::None
    }

    fn scroll(&mut self, lines: i32) -> bool {
        if self.tab != 1 {
            return false;
        }
        let old = self.scroll;
        let n = crate::dev::list().len() as i32;
        self.scroll = (self.scroll + lines * ROW).clamp(0, (n * ROW - 300).max(0));
        old != self.scroll
    }

    fn tick(&mut self) -> bool {
        // The uptime changes every second.
        // (and the answer about updates comes in).
        self.tab == 0 && crate::time::uptime_ms() % 1000 < 100
    }
}

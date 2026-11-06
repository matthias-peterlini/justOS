//! Negozio: JustOS apps that come with the system, and real Linux and Windows
//! programs from the internet (the catalog, or any GitHub project found by
//! searching). Each has a page; one click downloads, installs, opens or
//! removes it, with a ring that fills as the download goes. Aggiornamenti
//! lists the new versions of installed apps ([`crate::appupdate`]).

use crate::appupdate::{self, Update};
use crate::dev::input::{Key, KeyEvent};
use crate::gui::gfx::{Canvas, Rect};
use crate::gui::res;
use crate::gui::text::{self, Font};
use crate::gui::ui::{self, TEXT, TEXT_DIM, TEXT_FAINT};
use crate::gui::wm::{Action, App};
use crate::store::{self, Asset, CatalogApp, GhRepo, PackageInfo, Platform};
use crate::sync::IrqLock;
use crate::util::Size;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

const SIDEBAR: i32 = 210;
const SECTIONS: [(&str, &str); 9] = [
    ("Scopri", "g-grid"),
    ("Giochi", "games"),
    ("Creatività", "photos"),
    ("Sviluppatori", "code"),
    ("Strumenti", "terminal"),
    ("Internet", "g-wifi"),
    ("Windows", "window"),
    ("Installate", "download"),
    ("Aggiornamenti", "refresh"),
];
const UPDATES: usize = 8;
const CARD_H: i32 = 86;

/// An update not installed yet for the app `id`.
fn update_for(id: &str) -> Option<Update> {
    appupdate::list().into_iter().find(|u| u.id == id && u.state != appupdate::State::Installed)
}

/// "oggi alle 18:20", "3/11 alle 9:05" or "mai".
fn when(t: u64) -> String {
    if t == 0 {
        return String::from("mai");
    }
    let (d, today) = (crate::time::from_epoch(t), crate::time::from_epoch(crate::time::now()));
    if (d.year, d.month, d.day) == (today.year, today.month, today.day) {
        format!("oggi alle {}:{:02}", d.hour, d.min)
    } else {
        format!("{}/{} alle {}:{:02}", d.day, d.month, d.hour, d.min)
    }
}

/// Something the store shows: an app on this computer or one from the
/// internet.
#[derive(Clone)]
enum Item {
    Local(PackageInfo),
    Remote(CatalogApp),
}

impl Item {
    fn id(&self) -> &str {
        match self {
            Item::Local(p) => &p.id,
            Item::Remote(a) => &a.id,
        }
    }

    fn name(&self) -> &str {
        match self {
            Item::Local(p) => &p.name,
            Item::Remote(a) => &a.name,
        }
    }

    fn category(&self) -> &str {
        match self {
            Item::Local(p) => &p.category,
            Item::Remote(a) => &a.category,
        }
    }

    fn about(&self) -> &str {
        match self {
            Item::Local(p) => &p.about,
            Item::Remote(a) => &a.about,
        }
    }

    fn developer(&self) -> &str {
        match self {
            Item::Local(p) => &p.developer,
            Item::Remote(a) => &a.developer,
        }
    }

    fn platform(&self) -> Platform {
        match self {
            Item::Local(p) => p.platform,
            Item::Remote(a) => a.platform,
        }
    }

    fn installed(&self) -> Option<PackageInfo> {
        store::installed(self.id())
    }

    fn matches(&self, q: &str) -> bool {
        let q = q.to_lowercase();
        self.name().to_lowercase().contains(&q)
            || self.about().to_lowercase().contains(&q)
            || self.category().to_lowercase().contains(&q)
            || matches!(self, Item::Remote(a) if a.tags.iter().any(|t| t.contains(&q)) || a.command.contains(&q))
    }

    fn in_section(&self, s: usize) -> bool {
        let cat = self.category();
        match s {
            0 => true,
            1 => cat == "Giochi",
            2 => cat == "Creatività",
            3 => cat == "Sviluppatori",
            4 => cat == "Strumenti",
            5 => cat == "Internet",
            6 => self.platform() == Platform::Windows,
            7 => self.installed().is_some(),
            _ => false,
        }
    }
}

/// Searches and release lookups on GitHub, done by a background thread.
#[derive(Clone)]
enum Remote {
    Idle,
    Busy,
    Repos(String, Vec<GhRepo>),
    Release(GhRepo, String, Vec<Asset>),
    Failed(String),
}

static REMOTE: IrqLock<Remote> = IrqLock::new(Remote::Idle);
static CATALOG_READY: IrqLock<bool> = IrqLock::new(false);

/// What the content shows.
#[derive(Clone)]
enum View {
    List,
    Page(Item),
    GitHub,
}

pub struct Store {
    items: Vec<Item>,
    section: usize,
    view: View,
    hover: Option<Rect>,
    scroll: i32,
    search: String,
    searching: bool,
    /// Installations started from this window.
    watching: Vec<String>,
    /// Shown under the title while the catalog is fetched.
    note: String,
}

fn load_items() -> Vec<Item> {
    let mut v: Vec<Item> = store::available().into_iter().map(Item::Local).collect();
    v.extend(store::catalog().into_iter().map(Item::Remote));
    v
}

impl Store {
    pub fn new() -> Store {
        *CATALOG_READY.lock() = false;
        crate::task::spawn("store-catalog", || {
            let _ = store::refresh_catalog();
            *CATALOG_READY.lock() = true;
        });
        Store {
            items: load_items(),
            section: 0,
            view: View::List,
            hover: None,
            scroll: 0,
            search: String::new(),
            searching: false,
            watching: Vec::new(),
            note: String::from("Aggiorno il catalogo…"),
        }
    }

    fn shown(&self) -> Vec<usize> {
        (0..self.items.len())
            .filter(|&i| {
                let it = &self.items[i];
                if self.search.is_empty() {
                    it.in_section(self.section)
                } else {
                    it.matches(&self.search)
                }
            })
            .collect()
    }

    fn featured(&self) -> Option<usize> {
        self.items.iter().position(|i| matches!(i, Item::Remote(a) if a.featured && a.id == "ripgrep")).or_else(|| self.items.iter().position(|i| matches!(i, Item::Remote(a) if a.featured)))
    }

    fn content(area: Rect) -> Rect {
        Rect::new(area.x + SIDEBAR + 8, area.y + 8, area.w - SIDEBAR - 16, area.h - 16)
    }

    fn banner(area: Rect) -> Rect {
        let c = Self::content(area);
        Rect::new(c.x + 12, c.y + 56, c.w - 24, 168)
    }

    fn show_banner(&self) -> bool {
        self.section == 0 && self.search.is_empty() && self.featured().is_some()
    }

    fn grid_top(&self, area: Rect) -> i32 {
        let c = Self::content(area);
        if self.show_banner() {
            Self::banner(area).bottom() + 48
        } else {
            c.y + 60
        }
    }

    fn card(&self, area: Rect, k: usize) -> Rect {
        let c = Self::content(area);
        let cols = if c.w > 620 { 2 } else { 1 };
        let w = (c.w - 24 - (cols - 1) * 12) / cols;
        let (col, row) = (k as i32 % cols, k as i32 / cols);
        Rect::new(c.x + 12 + col * (w + 12), self.grid_top(area) + row * (CARD_H + 12) - self.scroll, w, CARD_H)
    }

    /// The "search on GitHub" row under the results.
    fn github_row(&self, area: Rect) -> Rect {
        let n = self.shown().len();
        let c = Self::content(area);
        let cols = if c.w > 620 { 2 } else { 1 };
        let rows = (n as i32 + cols - 1) / cols;
        Rect::new(c.x + 12, self.grid_top(area) + rows * (CARD_H + 12) - self.scroll, c.w - 24, 56)
    }

    fn get_btn(card: Rect) -> Rect {
        Rect::new(card.right() - 104, card.y + (card.h - 32) / 2, 90, 32)
    }

    fn place(area: Rect, i: usize) -> Rect {
        Rect::new(area.x + 12, area.y + 98 + i as i32 * 40, SIDEBAR - 24, 36)
    }

    fn search_rect(area: Rect) -> Rect {
        Rect::new(area.x + 14, area.y + 16, SIDEBAR - 28, 38)
    }

    fn back_btn(area: Rect) -> Rect {
        let c = Self::content(area);
        Rect::new(c.x + 12, c.y + 12, 36, 36)
    }

    fn page_btns(area: Rect) -> [Rect; 2] {
        let c = Self::content(area);
        [Rect::new(c.x + 170, c.y + 150, 120, 36), Rect::new(c.x + 300, c.y + 150, 120, 36)]
    }

    /// Draws an app's icon: its own picture once installed, else a tile in
    /// its colour with its initials.
    fn draw_icon(c: &mut Canvas, it: &Item, size: i32, x: i32, y: i32) {
        if let Some(icon) = store::icon(it.id()) {
            res::draw_icon(c, &icon, size, x, y);
            return;
        }
        match it {
            Item::Local(p) => res::draw_icon(c, if p.gui { "window" } else { "terminal" }, size, x, y),
            Item::Remote(a) => draw_tile(c, Rect::new(x, y, size, size), &a.name, a.color, a.platform),
        }
    }

    /// The state of an item's button: (label, primary); "" while installing.
    fn button_state(&self, it: &Item) -> (&'static str, bool) {
        if store::job(it.id()).is_some_and(|j| j.result.is_none()) {
            return ("", false);
        }
        match it.installed() {
            Some(_) if update_for(it.id()).is_some_and(|u| matches!(u.state, appupdate::State::Available | appupdate::State::Failed(_))) => ("Aggiorna", true),
            Some(_) => ("Apri", false),
            None => ("Ottieni", true),
        }
    }

    fn draw_button(&self, c: &mut Canvas, r: Rect, it: &Item) {
        let progress = match store::job(it.id()).filter(|j| j.result.is_none()) {
            Some(j) => Some((j.done, j.total)),
            None => match update_for(it.id()).map(|u| u.state) {
                Some(appupdate::State::Downloading { done, total }) => Some((done, total)),
                _ => None,
            },
        };
        if let Some((done, total)) = progress {
            draw_ring(c, r, done, total);
            return;
        }
        let (label, primary) = self.button_state(it);
        ui::button(c, r, label, primary, self.hover == Some(r));
    }

    /// The button of an item was pressed.
    fn act(&mut self, it: &Item) -> Action {
        if store::job(it.id()).is_some_and(|j| j.result.is_none()) {
            return Action::None;
        }
        if self.button_state(it).0 == "Aggiorna" {
            return self.update(it.id());
        }
        if let Some(p) = it.installed() {
            return if p.gui { Action::Spawn(store::program_path(&p)) } else { Action::Run(p.open.clone()) };
        }
        match it {
            Item::Local(p) => match store::install(&p.id) {
                Ok(_) => {
                    let how = if p.gui { "La trovi tra le applicazioni." } else { "Si usa nel Terminale." };
                    Action::Notify(format!("{} installata", p.name), String::from(how), "store")
                }
                Err(e) => Action::Notify(p.name.clone(), format!("Installazione non riuscita: {e}"), "store"),
            },
            Item::Remote(a) => {
                if !crate::net::online() {
                    return Action::Notify(a.name.clone(), String::from("Serve una connessione a internet."), "store");
                }
                store::start_install(a.clone());
                self.watching.push(a.id.clone());
                Action::Redraw
            }
        }
    }

    /// Updates one app ("all": every one) in the background.
    fn update(&mut self, id: &str) -> Action {
        if !crate::net::online() && update_for(id).is_some_and(|u| u.state != appupdate::State::Ready) {
            return Action::Notify(String::from("Aggiornamenti"), String::from("Serve una connessione a internet."), "store");
        }
        appupdate::install_in_background(id);
        Action::Redraw
    }

    // ------------------------------------------------------------------------
    // Aggiornamenti

    fn check_btn(area: Rect) -> Rect {
        let c = Self::content(area);
        Rect::new(c.right() - 150, c.y + 12, 136, 34)
    }

    fn all_btn(area: Rect) -> Rect {
        let c = Self::content(area);
        Rect::new(c.right() - 296, c.y + 12, 136, 34)
    }

    fn auto_row(area: Rect) -> Rect {
        let c = Self::content(area);
        Rect::new(c.x + 12, c.y + 84, c.w - 24, 60)
    }

    fn auto_toggle(area: Rect) -> Rect {
        let r = Self::auto_row(area);
        Rect::new(r.right() - 70, r.y + 15, 52, 30)
    }

    fn update_rows(&self, area: Rect, n: usize) -> Vec<Rect> {
        let c = Self::content(area);
        (0..n).map(|i| Rect::new(c.x + 12, c.y + 158 + i as i32 * (CARD_H + 10) - self.scroll, c.w - 24, CARD_H)).collect()
    }

    fn draw_updates(&mut self, c: &mut Canvas, area: Rect) {
        let ct = Self::content(area);
        text::draw(c, Font::Title, ct.x + 14, ct.y + 8, "Aggiornamenti", TEXT);
        let list = appupdate::list();
        let open = list.iter().filter(|u| u.state != appupdate::State::Installed).count();
        let check = Self::check_btn(area);
        if appupdate::checking() {
            let t = ((crate::time::uptime_ms() / 2) % 1000) as u32;
            c.arc(check.x + 20, check.y + 17, 10, 3, 1000, 0x26FF_FFFF);
            c.arc(check.x + 20, check.y + 17, 10, 3, 250 + t / 4, 0xFFFF_FFFF);
            text::draw(c, Font::Body, check.x + 40, check.y + 7, "Controllo…", TEXT_DIM);
        } else {
            ui::button(c, check, "Controlla ora", false, self.hover == Some(check));
        }
        if open > 1 {
            let all = Self::all_btn(area);
            ui::button(c, all, "Aggiorna tutte", true, self.hover == Some(all));
        }
        let mut line = format!("Ultimo controllo: {}", when(appupdate::last_check()));
        if let Some(e) = appupdate::last_error() {
            line.push_str(&format!(" · {e}"));
        }
        text::draw(c, Font::Small, ct.x + 16, ct.y + 56, &text::fit(Font::Small, &line, ct.w - 40), TEXT_FAINT);
        let row = Self::auto_row(area);
        c.fill_round(row, 16, ui::PANEL);
        text::draw(c, Font::Strong, row.x + 18, row.y + 11, "Aggiorna le app automaticamente", TEXT);
        text::draw(c, Font::Small, row.x + 18, row.y + 33, "Una volta al giorno; un'app aperta si aggiorna quando la chiudi", TEXT_DIM);
        ui::toggle(c, Self::auto_toggle(area), appupdate::auto());
        if list.is_empty() {
            text::draw_centered(c, Font::Body, ct.x + ct.w / 2, ct.y + 200, "Le tue app sono aggiornate", TEXT_FAINT);
        }
        let saved = c.clip();
        c.set_clip(saved.intersect(&Rect::new(ct.x, ct.y + 152, ct.w, ct.h - 152)));
        for (r, u) in self.update_rows(area, list.len()).into_iter().zip(list.iter()) {
            if r.bottom() < ct.y + 152 || r.y > ct.bottom() {
                continue;
            }
            c.fill_round(r, 18, ui::PANEL);
            match store::icon(&u.id) {
                Some(icon) => res::draw_icon(c, &icon, 58, r.x + 14, r.y + 14),
                None => res::draw_icon(c, "window", 58, r.x + 14, r.y + 14),
            }
            text::draw(c, Font::Strong, r.x + 86, r.y + 14, &text::fit(Font::Strong, &u.name, r.w - 240), TEXT);
            text::draw(c, Font::Small, r.x + 86, r.y + 38, &format!("Versione {} (prima {})", u.version, u.current), TEXT_DIM);
            let note = if !u.new_permissions.is_empty() && u.state != appupdate::State::Installed {
                let what: Vec<String> = u.new_permissions.iter().map(|p| crate::sandbox::describe(p)).collect();
                format!("Chiede anche: {}", what.join(", "))
            } else {
                match &u.state {
                    appupdate::State::Ready if u.in_use() => String::from("Si installa quando chiudi l'app"),
                    appupdate::State::Ready => String::from("Scaricato"),
                    appupdate::State::Installed => String::from("Aggiornata"),
                    appupdate::State::Failed(e) => format!("Non riuscito: {e}"),
                    appupdate::State::Downloading { .. } => String::from("Scarico…"),
                    appupdate::State::Available => String::new(),
                }
            };
            text::draw(c, Font::Small, r.x + 86, r.y + 58, &text::fit(Font::Small, &note, r.w - 220), TEXT_FAINT);
            let btn = Self::get_btn(r);
            match &u.state {
                appupdate::State::Downloading { done, total } => draw_ring(c, btn, *done, *total),
                appupdate::State::Installed => {}
                appupdate::State::Ready if !u.needs_approval() && u.in_use() => {}
                _ => ui::button(c, btn, "Aggiorna", true, self.hover == Some(btn)),
            }
        }
        c.set_clip(saved);
    }

    fn press_updates(&mut self, area: Rect, ax: i32, ay: i32) -> Action {
        if Self::check_btn(area).contains(ax, ay) && !appupdate::checking() {
            if !crate::net::online() {
                return Action::Notify(String::from("Aggiornamenti"), String::from("Serve una connessione a internet."), "store");
            }
            appupdate::check_in_background();
            return Action::Redraw;
        }
        let list = appupdate::list();
        if list.iter().filter(|u| u.state != appupdate::State::Installed).count() > 1 && Self::all_btn(area).contains(ax, ay) {
            return self.update("all");
        }
        if Self::auto_toggle(area).contains(ax, ay) || Self::auto_row(area).contains(ax, ay) {
            appupdate::set_auto(!appupdate::auto());
            return Action::Redraw;
        }
        for (r, u) in self.update_rows(area, list.len()).into_iter().zip(list.iter()) {
            if Self::get_btn(r).contains(ax, ay) && u.state != appupdate::State::Installed && !matches!(u.state, appupdate::State::Downloading { .. }) {
                return self.update(&u.id);
            }
        }
        Action::Redraw
    }

    fn remove(&mut self, it: &Item) -> Action {
        let name = it.name().to_string();
        match store::remove(it.id()) {
            Ok(()) => Action::Notify(format!("{name} rimossa"), String::from("Puoi installarla di nuovo quando vuoi."), "store"),
            Err(e) => Action::Notify(name, format!("Non si è potuta rimuovere: {e}"), "store"),
        }
    }

    fn start_github_search(&mut self) {
        let q = self.search.trim().to_string();
        if q.is_empty() {
            return;
        }
        *REMOTE.lock() = Remote::Busy;
        self.view = View::GitHub;
        self.scroll = 0;
        crate::task::spawn("store-github", move || {
            let r = match store::github_search(&q) {
                Ok(v) => Remote::Repos(q, v),
                Err(e) => Remote::Failed(e),
            };
            *REMOTE.lock() = r;
        });
    }

    fn open_repo(&mut self, repo: GhRepo) {
        *REMOTE.lock() = Remote::Busy;
        crate::task::spawn("store-github", move || {
            let r = match store::github_release(&repo.full_name) {
                Ok((tag, assets)) => Remote::Release(repo, tag, assets),
                Err(e) => Remote::Failed(e),
            };
            *REMOTE.lock() = r;
        });
    }

    fn repo_rows(area: Rect, n: usize, scroll: i32) -> Vec<Rect> {
        let c = Self::content(area);
        (0..n).map(|i| Rect::new(c.x + 12, c.y + 64 + i as i32 * 70 - scroll, c.w - 24, 62)).collect()
    }

    fn draw_github(&mut self, c: &mut Canvas, area: Rect) {
        let ct = Self::content(area);
        ui::icon_button(c, Self::back_btn(area), "g-back", self.hover == Some(Self::back_btn(area)));
        let state = REMOTE.lock().clone();
        match state {
            Remote::Idle | Remote::Busy => {
                text::draw(c, Font::Title, ct.x + 60, ct.y + 10, "GitHub", TEXT);
                let t = ((crate::time::uptime_ms() / 2) % 1000) as u32;
                c.arc(ct.x + ct.w / 2, ct.y + 180, 18, 3, 1000, 0x26FF_FFFF);
                c.arc(ct.x + ct.w / 2, ct.y + 180, 18, 3, 250 + t / 4, 0xFFFF_FFFF);
                text::draw_centered(c, Font::Body, ct.x + ct.w / 2, ct.y + 214, "Cerco su GitHub…", TEXT_DIM);
            }
            Remote::Failed(e) => {
                text::draw(c, Font::Title, ct.x + 60, ct.y + 10, "GitHub", TEXT);
                text::draw_centered(c, Font::Body, ct.x + ct.w / 2, ct.y + 180, &text::fit(Font::Body, &e, ct.w - 40), TEXT_DIM);
            }
            Remote::Repos(q, repos) => {
                text::draw(c, Font::Title, ct.x + 60, ct.y + 10, &text::fit(Font::Title, &format!("«{q}» su GitHub"), ct.w - 80), TEXT);
                if repos.is_empty() {
                    text::draw_centered(c, Font::Body, ct.x + ct.w / 2, ct.y + 180, "Nessun progetto trovato", TEXT_FAINT);
                }
                for (r, repo) in Self::repo_rows(area, repos.len(), self.scroll).into_iter().zip(repos.iter()) {
                    if r.bottom() < ct.y + 56 || r.y > ct.bottom() {
                        continue;
                    }
                    c.fill_round(r, 14, if self.hover == Some(r) { ui::PANEL_HOVER } else { ui::PANEL });
                    let name = repo.full_name.rsplit('/').next().unwrap_or("");
                    draw_tile(c, Rect::new(r.x + 10, r.y + 9, 44, 44), name, 0xFF3B_4252, Platform::Linux);
                    text::draw(c, Font::Strong, r.x + 66, r.y + 12, &text::fit(Font::Strong, &repo.full_name, r.w - 200), TEXT);
                    text::draw(c, Font::Small, r.x + 66, r.y + 34, &text::fit(Font::Small, &repo.description, r.w - 200), TEXT_DIM);
                    text::draw_right(c, Font::Small, r.right() - 16, r.y + 23, &format!("★ {}", repo.stars), TEXT_FAINT);
                }
            }
            Remote::Release(repo, tag, assets) => {
                text::draw(c, Font::Title, ct.x + 60, ct.y + 10, &text::fit(Font::Title, &repo.full_name, ct.w - 80), TEXT);
                text::draw(c, Font::Body, ct.x + 14, ct.y + 62, &text::fit(Font::Body, &repo.description, ct.w - 30), TEXT_DIM);
                ui::caps(c, ct.x + 14, ct.y + 98, &format!("Ultima versione {tag}"), TEXT_FAINT);
                if assets.is_empty() {
                    text::draw(c, Font::Body, ct.x + 14, ct.y + 130, "Nessuna versione per JustOS (Linux statico o Windows x64) in questa release.", TEXT_DIM);
                }
                for (r, a) in Self::repo_rows(area, assets.len(), 0).into_iter().map(|r| r.offset(0, 64)).zip(assets.iter()) {
                    if r.y > ct.bottom() {
                        break;
                    }
                    c.fill_round(r, 14, ui::PANEL);
                    platform_chip(c, r.x + 14, r.y + 19, a.platform);
                    text::draw(c, Font::Strong, r.x + 96, r.y + 12, &text::fit(Font::Strong, &a.name, r.w - 240), TEXT);
                    text::draw(c, Font::Small, r.x + 96, r.y + 34, &format!("{}", Size(a.size)), TEXT_DIM);
                    let it = Item::Remote(self.asset_app(&repo, a));
                    self.draw_button(c, Self::get_btn(r), &it);
                }
                text::draw(c, Font::Small, ct.x + 14, ct.bottom() - 26, "Le app di GitHub non sono verificate da JustOS: installa solo progetti di cui ti fidi.", TEXT_FAINT);
            }
        }
    }

    /// A catalog entry for one file of a GitHub release.
    fn asset_app(&self, repo: &GhRepo, a: &Asset) -> CatalogApp {
        let mut app = store::app_from_repo(repo, a.platform);
        app.asset = a.name.clone();
        if a.platform == Platform::Windows {
            app.id = format!("{}-win", app.id);
            app.command = format!("{}w", app.command);
            app.open = format!("{} --help", app.command);
        }
        app
    }

    fn draw_page(&mut self, c: &mut Canvas, area: Rect, it: &Item) {
        let ct = Self::content(area);
        let back = Self::back_btn(area);
        ui::icon_button(c, back, "g-back", self.hover == Some(back));
        Self::draw_icon(c, it, 128, ct.x + 20, ct.y + 58);
        text::draw(c, Font::Title, ct.x + 170, ct.y + 60, &text::fit(Font::Title, it.name(), ct.w - 190), TEXT);
        text::draw(c, Font::Body, ct.x + 170, ct.y + 102, &text::fit(Font::Body, &format!("{} · {}", it.developer(), it.category()), ct.w - 300), TEXT_DIM);
        platform_chip(c, ct.x + 170, ct.y + 124, it.platform());
        let [get, remove] = Self::page_btns(area);
        self.draw_button(c, get, it);
        if it.installed().is_some() && store::job(it.id()).is_none_or(|j| j.result.is_some()) {
            let h = self.hover == Some(remove);
            c.fill_round(remove, remove.h / 2, if h { 0x40FF_5A4E } else { 0x26FF_5A4E });
            let (lh, _) = text::metrics(Font::Strong);
            text::draw_centered(c, Font::Strong, remove.x + remove.w / 2, remove.y + (remove.h - lh) / 2, "Rimuovi", 0xFFFF_8A80);
        }
        if let Some(j) = store::job(it.id()).filter(|j| j.result.is_none()) {
            let what = match j.total {
                Some(t) => format!("{} · {} di {}", j.phase, Size(j.done), Size(t)),
                None if j.done > 0 => format!("{} · {}", j.phase, Size(j.done)),
                None => String::from(j.phase),
            };
            text::draw(c, Font::Small, get.right() + 16, get.y + 10, &what, TEXT_DIM);
        }
        c.fill(Rect::new(ct.x + 16, ct.y + 206, ct.w - 32, 1), ui::SEPARATOR);
        let installed = it.installed();
        let (version, size, license, kind) = match it {
            Item::Local(p) => (
                installed.as_ref().map(|i| i.version.clone()).filter(|v| !v.is_empty()).unwrap_or_else(|| p.version.clone()),
                format!("{}", Size(p.size)), String::from("—"), String::from(if p.gui { "App" } else { "Terminale" })),
            Item::Remote(a) => (
                installed.as_ref().map(|p| p.version.clone()).filter(|v| !v.is_empty()).unwrap_or_else(|| String::from("Ultima")),
                String::from(if a.github.is_empty() { "—" } else { "GitHub" }),
                if a.license.is_empty() { String::from("—") } else { a.license.clone() },
                String::from("Terminale"),
            ),
        };
        let facts = [("Versione", version), ("Fonte", size), ("Licenza", license), ("Tipo", kind)];
        let fw = (ct.w - 32) / facts.len() as i32;
        for (k, (label, value)) in facts.iter().enumerate() {
            let x = ct.x + 16 + k as i32 * fw + fw / 2;
            let lw = ui::caps_width(label);
            ui::caps(c, x - lw / 2, ct.y + 222, label, TEXT_FAINT);
            text::draw_centered(c, Font::Strong, x, ct.y + 242, &text::fit(Font::Strong, value, fw - 10), TEXT_DIM);
            if k > 0 {
                c.fill(Rect::new(ct.x + 16 + k as i32 * fw, ct.y + 222, 1, 36), ui::SEPARATOR);
            }
        }
        c.fill(Rect::new(ct.x + 16, ct.y + 276, ct.w - 32, 1), ui::SEPARATOR);
        text::draw(c, Font::Heading, ct.x + 16, ct.y + 292, "Descrizione", TEXT);
        let details = match it {
            Item::Local(p) => p.details.replace("\\n", "\n"),
            Item::Remote(a) => a.details.clone(),
        };
        let mut body = String::from(it.about());
        if !details.is_empty() {
            body.push_str("\n\n");
            body.push_str(&details);
        }
        let declared = match it {
            Item::Local(p) => p.permissions.clone(),
            Item::Remote(_) => Vec::new(),
        };
        if !declared.is_empty() {
            let what: Vec<String> = declared.iter().map(|p| crate::sandbox::describe(p)).collect();
            body.push_str(&format!("\n\nPermessi: può chiederti di usare {}. Lo decidi tu la prima volta, e puoi cambiarlo in Impostazioni, alla voce Privacy delle app.", what.join(", ")));
        }
        if let Some(u) = update_for(it.id()) {
            body.push_str(&format!("\n\nÈ disponibile la versione {} (hai la {}).", u.version, u.current));
        }
        if let Item::Remote(a) = it {
            body.push_str(&format!("\n\nSi usa nel Terminale con il comando «{}».", a.command));
            if a.platform == Platform::Windows {
                body.push_str(" È un programma per Windows: JustOS lo avvia con winrun, che conosce una parte delle funzioni di Windows.");
            }
        }
        let mut y = ct.y + 326;
        for para in body.split('\n') {
            for line in text::wrap(Font::Body, para, ct.w - 40) {
                if y > ct.bottom() - 20 {
                    break;
                }
                text::draw(c, Font::Body, ct.x + 16, y, &line, TEXT_DIM);
                y += 22;
            }
            if para.is_empty() {
                y += 4;
            }
        }
    }
}

/// The ring that fills as a download goes, in place of a button.
fn draw_ring(c: &mut Canvas, r: Rect, done: u64, total: Option<u64>) {
    let (cx, cy) = (r.x + r.w / 2, r.y + r.h / 2);
    let frac = match total {
        Some(t) if t > 0 => (done * 1000 / t) as u32,
        _ => ((crate::time::uptime_ms() / 3) % 1000) as u32,
    };
    c.arc(cx, cy, 14, 3, 1000, 0x33FF_FFFF);
    c.arc(cx, cy, 14, 3, frac.max(30), 0xFFFF_FFFF);
    c.fill_round(Rect::new(cx - 4, cy - 4, 8, 8), 2, 0xFFFF_FFFF);
}

/// A tile in a colour with initials (an app with no picture yet).
fn draw_tile(c: &mut Canvas, r: Rect, name: &str, color: u32, platform: Platform) {
    let rad = r.w * 30 / 128;
    c.fill_round_grad(r, rad, color, crate::gui::gfx::mix(color, 0xFF10_0C10, 110));
    let mut initials: String = name.chars().filter(|ch| ch.is_alphanumeric()).take(2).collect();
    if let Some(f) = initials.chars().next() {
        initials = f.to_uppercase().chain(initials.chars().skip(1)).collect();
    }
    let font = if r.w >= 96 {
        Font::Title
    } else if r.w >= 56 {
        Font::Display
    } else {
        Font::Strong
    };
    let (lh, _) = text::metrics(font);
    text::draw_centered(c, font, r.x + r.w / 2, r.y + (r.h - lh) / 2 - r.h / 16, &initials, 0xFFFF_FFFF);
    if r.w >= 56 {
        let bw = r.w * 34 / 128;
        let badge = Rect::new(r.right() - bw - r.w / 12, r.bottom() - r.h * 34 / 128, bw, r.h * 22 / 128);
        c.fill_round(badge, badge.h / 3, 0x8C00_0000);
        let label = if platform == Platform::Windows { "win" } else { ">_" };
        let (sh, _) = text::metrics(Font::Small);
        text::draw_centered(c, Font::Small, badge.x + badge.w / 2, badge.y + (badge.h - sh) / 2, label, 0xFFFF_FFFF);
    }
}

/// A small pill naming the platform.
fn platform_chip(c: &mut Canvas, x: i32, y: i32, p: Platform) {
    let label = p.name();
    let w = ui::caps_width(label) + 18;
    let r = Rect::new(x, y, w, 22);
    let bg = match p {
        Platform::Linux => 0x33F5_A623,
        Platform::Windows => 0x333B_82F6,
        Platform::Justos => 0x33E0_662E,
    };
    c.fill_round(r, 11, bg);
    ui::caps(c, x + 9, y + 6, label, TEXT);
}

impl App for Store {
    fn title(&self) -> String {
        String::from("Negozio")
    }

    fn icon(&self) -> &str {
        "store"
    }

    fn kind(&self) -> &'static str {
        "store"
    }

    fn size(&self) -> (i32, i32) {
        (920, 600)
    }

    fn resizable(&self) -> bool {
        true
    }

    fn sidebar(&self) -> i32 {
        SIDEBAR
    }

    fn draw(&mut self, c: &mut Canvas, area: Rect, _blur: &Canvas, focused: bool) {
        ui::search_field(c, Self::search_rect(area), &self.search, "Cerca app", self.searching && focused);
        ui::caps(c, area.x + 26, area.y + 74, "Categorie", TEXT_FAINT);
        for (i, (name, icon)) in SECTIONS.iter().enumerate() {
            let r = Self::place(area, i);
            if i == self.section && self.search.is_empty() && matches!(self.view, View::List) {
                c.fill_round(r, 12, ui::SELECTED);
            } else if self.hover == Some(r) {
                c.fill_round(r, 12, ui::PANEL);
            }
            res::draw_icon(c, icon, 20, r.x + 10, r.y + 8);
            text::draw(c, if i == self.section { Font::Strong } else { Font::Body }, r.x + 40, r.y + 9, name, TEXT);
            let n = if i == UPDATES { appupdate::pending_count() } else { 0 };
            if n > 0 {
                let label = format!("{n}");
                let w = text::width(Font::Small, &label) + 12;
                let badge = Rect::new(r.right() - w - 8, r.y + 8, w, 20);
                c.fill_round(badge, 10, ui::ACCENT);
                let (sh, _) = text::metrics(Font::Small);
                text::draw_centered(c, Font::Small, badge.x + w / 2, badge.y + (20 - sh) / 2, &label, 0xFFFF_FFFF);
            }
        }
        let ct = Self::content(area);
        let saved = c.clip();
        c.set_clip(saved.intersect(&ct));
        match self.view.clone() {
            View::Page(it) => {
                self.draw_page(c, area, &it);
                c.set_clip(saved);
                return;
            }
            View::GitHub => {
                self.draw_github(c, area);
                c.set_clip(saved);
                return;
            }
            View::List => {}
        }
        if self.section == UPDATES && self.search.is_empty() {
            self.draw_updates(c, area);
            c.set_clip(saved);
            return;
        }
        let title = if self.search.is_empty() { SECTIONS[self.section].0 } else { "Risultati" };
        text::draw(c, Font::Title, ct.x + 14, ct.y + 8, title, TEXT);
        if !self.note.is_empty() {
            text::draw_right(c, Font::Small, ct.right() - 16, ct.y + 22, &self.note, TEXT_FAINT);
        }
        let shown = self.shown();
        if self.show_banner() {
            let b = Self::banner(area);
            if let Some(fi) = self.featured() {
                let it = self.items[fi].clone();
                c.fill_round_grad(b, 22, 0xFF2A_1712, 0xFF14_0D0B);
                c.glow_round(b, 22, b.x + b.w / 5, b.y, b.w * 2 / 3, ui::GLOW, 170);
                c.stroke_round(b, 22, 1, ui::BORDER);
                ui::caps(c, b.x + 28, b.y + 24, "In evidenza", 0xCCFF_FFFF);
                text::draw(c, Font::Title, b.x + 28, b.y + 44, it.name(), TEXT);
                for (k, l) in text::wrap(Font::Body, it.about(), b.w - 260).iter().take(2).enumerate() {
                    text::draw(c, Font::Body, b.x + 28, b.y + 88 + k as i32 * 22, l, TEXT_DIM);
                }
                Self::draw_icon(c, &it, 112, b.right() - 150, b.y + 28);
                let btn = Rect::new(b.x + 28, b.bottom() - 48, 100, 34);
                self.draw_button(c, btn, &it);
            }
            text::draw(c, Font::Heading, ct.x + 14, b.bottom() + 14, "Tutte le app", TEXT);
        }
        if shown.is_empty() && self.search.is_empty() {
            let msg = if self.section == 7 { "Nessuna app installata dal Negozio" } else { "Nessuna app" };
            text::draw_centered(c, Font::Body, ct.x + ct.w / 2, self.grid_top(area) + 30, msg, TEXT_FAINT);
        }
        for (k, &i) in shown.iter().enumerate() {
            let r = self.card(area, k);
            if r.bottom() < ct.y || r.y > ct.bottom() {
                continue;
            }
            let it = self.items[i].clone();
            c.fill_round(r, 18, if self.hover == Some(r) { ui::PANEL_HOVER } else { ui::PANEL });
            Self::draw_icon(c, &it, 58, r.x + 14, r.y + 14);
            text::draw(c, Font::Strong, r.x + 86, r.y + 14, &text::fit(Font::Strong, it.name(), r.w - 210), TEXT);
            platform_chip(c, r.x + 86, r.y + 36, it.platform());
            text::draw(c, Font::Small, r.x + 86, r.y + 62, &text::fit(Font::Small, it.about(), r.w - 210), TEXT_FAINT);
            self.draw_button(c, Self::get_btn(r), &it);
        }
        if !self.search.is_empty() {
            let g = self.github_row(area);
            if g.y < ct.bottom() {
                c.fill_round(g, 18, if self.hover == Some(g) { ui::PANEL_HOVER } else { ui::PANEL });
                res::draw_icon(c, "g-search", 20, g.x + 18, g.y + 18);
                text::draw(c, Font::Strong, g.x + 50, g.y + 10, &text::fit(Font::Strong, &format!("Cerca «{}» su GitHub", self.search), g.w - 80), TEXT);
                text::draw(c, Font::Small, g.x + 50, g.y + 32, "Programmi Linux e Windows dai progetti open source", TEXT_DIM);
            }
        }
        c.set_clip(saved);
    }

    fn press(&mut self, x: i32, y: i32, _double: bool, area: Rect) -> Action {
        let (ax, ay) = (area.x + x, area.y + y);
        if Self::search_rect(area).contains(ax, ay) {
            self.searching = true;
            self.view = View::List;
            return Action::Redraw;
        }
        self.searching = false;
        for i in 0..SECTIONS.len() {
            if Self::place(area, i).contains(ax, ay) {
                self.section = i;
                self.view = View::List;
                self.scroll = 0;
                self.search.clear();
                return Action::Redraw;
            }
        }
        match self.view.clone() {
            View::Page(it) => {
                if Self::back_btn(area).contains(ax, ay) {
                    self.view = View::List;
                    return Action::Redraw;
                }
                let [get, remove] = Self::page_btns(area);
                if get.contains(ax, ay) {
                    return self.act(&it);
                }
                if remove.contains(ax, ay) && it.installed().is_some() {
                    return self.remove(&it);
                }
                return Action::None;
            }
            View::GitHub => {
                if Self::back_btn(area).contains(ax, ay) {
                    let st = REMOTE.lock().clone();
                    match st {
                        Remote::Release(..) if !self.search.is_empty() => self.start_github_search(),
                        _ => self.view = View::List,
                    }
                    return Action::Redraw;
                }
                let st = REMOTE.lock().clone();
                match st {
                    Remote::Repos(_, repos) => {
                        for (r, repo) in Self::repo_rows(area, repos.len(), self.scroll).into_iter().zip(repos) {
                            if r.contains(ax, ay) {
                                self.open_repo(repo);
                                return Action::Redraw;
                            }
                        }
                    }
                    Remote::Release(repo, _, assets) => {
                        for (r, a) in Self::repo_rows(area, assets.len(), 0).into_iter().map(|r| r.offset(0, 64)).zip(assets.iter()) {
                            if Self::get_btn(r).contains(ax, ay) {
                                let it = Item::Remote(self.asset_app(&repo, a));
                                return self.act(&it);
                            }
                        }
                    }
                    _ => {}
                }
                return Action::Redraw;
            }
            View::List => {}
        }
        if self.section == UPDATES && self.search.is_empty() {
            return self.press_updates(area, ax, ay);
        }
        if self.show_banner() {
            let b = Self::banner(area);
            if let Some(fi) = self.featured() {
                let it = self.items[fi].clone();
                if Rect::new(b.x + 28, b.bottom() - 48, 100, 34).contains(ax, ay) {
                    return self.act(&it);
                }
                if b.contains(ax, ay) {
                    self.view = View::Page(it);
                    return Action::Redraw;
                }
            }
        }
        for (k, &i) in self.shown().iter().enumerate() {
            let r = self.card(area, k);
            let it = self.items[i].clone();
            if Self::get_btn(r).contains(ax, ay) {
                return self.act(&it);
            }
            if r.contains(ax, ay) {
                self.view = View::Page(it);
                return Action::Redraw;
            }
        }
        if !self.search.is_empty() && self.github_row(area).contains(ax, ay) {
            self.start_github_search();
        }
        Action::Redraw
    }

    fn hover(&mut self, at: Option<(i32, i32)>, area: Rect) -> bool {
        let mut targets: Vec<Rect> = (0..SECTIONS.len()).map(|i| Self::place(area, i)).collect();
        match &self.view {
            View::Page(_) => {
                targets.push(Self::back_btn(area));
                targets.extend(Self::page_btns(area));
            }
            View::GitHub => {
                targets.push(Self::back_btn(area));
                if let Remote::Repos(_, r) = &*REMOTE.lock() {
                    targets.extend(Self::repo_rows(area, r.len(), self.scroll));
                }
            }
            View::List if self.section == UPDATES && self.search.is_empty() => {
                targets.push(Self::check_btn(area));
                targets.push(Self::all_btn(area));
                for r in self.update_rows(area, appupdate::list().len()) {
                    targets.push(Self::get_btn(r));
                }
            }
            View::List => {
                for k in 0..self.shown().len() {
                    let r = self.card(area, k);
                    targets.push(Self::get_btn(r));
                    targets.push(r);
                }
                if !self.search.is_empty() {
                    targets.push(self.github_row(area));
                }
            }
        }
        let h = at.and_then(|(x, y)| targets.into_iter().find(|r| r.contains(area.x + x, area.y + y)));
        let changed = h != self.hover;
        self.hover = h;
        changed
    }

    fn key(&mut self, ev: KeyEvent) -> Action {
        if ev.key == Key::Escape {
            self.view = View::List;
            self.search.clear();
            self.searching = false;
            return Action::Redraw;
        }
        if self.searching && ev.key == Key::Enter && !self.search.trim().is_empty() && self.shown().is_empty() {
            self.start_github_search();
            return Action::Redraw;
        }
        if self.searching && ui::edit_text(&mut self.search, &ev, 40) {
            self.scroll = 0;
            self.view = View::List;
            return Action::Redraw;
        }
        Action::None
    }

    fn scroll(&mut self, lines: i32) -> bool {
        if matches!(self.view, View::Page(_)) {
            return false;
        }
        let old = self.scroll;
        let n = match &self.view {
            View::GitHub => match &*REMOTE.lock() {
                Remote::Repos(_, r) => r.len() as i32 * 70,
                _ => 0,
            },
            _ if self.section == UPDATES && self.search.is_empty() => appupdate::list().len() as i32 * (CARD_H + 10) + 160,
            _ => (self.shown().len() as i32 + 1) / 2 * (CARD_H + 12) + 300,
        };
        self.scroll = (self.scroll + lines * 40).clamp(0, (n - 300).max(0));
        old != self.scroll
    }

    fn animating(&self) -> bool {
        self.watching.iter().any(|id| store::job(id).is_some())
            || matches!(self.view, View::GitHub) && matches!(*REMOTE.lock(), Remote::Busy)
            || appupdate::checking()
            || appupdate::list().iter().any(|u| matches!(u.state, appupdate::State::Downloading { .. }))
    }

    fn frame(&mut self) -> bool {
        // Finished installations: tell, and forget them.
        let mut done = Vec::new();
        for id in &self.watching {
            if let Some(Some(r)) = store::job(id).map(|j| j.result) {
                done.push((id.clone(), r));
            }
        }
        for (id, r) in done {
            store::clear_job(&id);
            self.watching.retain(|w| *w != id);
            let name = store::installed(&id).map(|p| p.name).unwrap_or_else(|| id.clone());
            match r {
                Ok(()) => crate::gui::notify(&format!("{name} installata"), "La trovi tra le applicazioni; si usa nel Terminale.", "store"),
                Err(e) => crate::gui::notify(&name, &format!("Installazione non riuscita: {e}"), "store"),
            }
        }
        true
    }

    fn tick(&mut self) -> bool {
        if core::mem::take(&mut *CATALOG_READY.lock()) {
            self.items = load_items();
            self.note.clear();
            return true;
        }
        // Updates change from the service's thread.
        matches!(self.view, View::GitHub) || self.section == UPDATES || appupdate::pending_count() > 0
    }
}

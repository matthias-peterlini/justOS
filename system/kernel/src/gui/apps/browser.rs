//! ticly browser: tabs, an address bar and web pages drawn by [`justweb`]
//! (HTML and CSS, PNG, JPEG and GIF pictures, JavaScript), in the look of
//! the ticly browser. A page loads on a thread of its own: the page, its
//! stylesheets, then its pictures four at a time; the page is laid out
//! again as they come in and when the window changes size. Its scripts run
//! on another thread, which then hands them the page's events and timers.
//! Cookies are kept while JustOS runs. The start page searches with
//! DuckDuckGo and leads into ticly.

use super::ticly as tui;
use crate::dev::input::{Key, KeyEvent};
use crate::fs;
use crate::gui::gfx::{Canvas, Rect};
use crate::gui::res;
use crate::gui::text::{self, Font};
use crate::gui::ui::{self, TEXT, TEXT_DIM, TEXT_FAINT};
use crate::gui::wm::{Action, App};
use crate::net::http::{self, Request};
use crate::sync::{IrqLock, Mutex};
use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use justweb::document::Script;
use justweb::dom::NodeId;
use justweb::script::Scripts;
use justweb::{Document, Hit, WebFonts};
use core::sync::atomic::{AtomicUsize, Ordering};

const TABS_H: i32 = 46;
const TOOLBAR_H: i32 = 54;
const BAR_H: i32 = TABS_H + TOOLBAR_H;
const CHROME: u32 = 0xFF0E_0B0C;
const SEARCH: &str = "https://html.duckduckgo.com/html/?q=";
const LOGIN: &str = "https://login.ticly.it/login";
const WORKSPACE: &str = "https://do.ticly.cloud/";
const ACCEPT_PAGE: &str = "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8";
const ACCEPT_LANGUAGE: &str = "it-IT,it;q=0.9,en;q=0.8";
/// Laying out a deep page recurses a lot.
const STACK: u64 = 512 << 10;
/// The script interpreter recurses even more.
const SCRIPT_STACK: u64 = 2 << 20;
/// How often a page's events and timers are looked at.
const SCRIPT_TICK_MS: u64 = 15;
/// Threads fetching the pictures of a page.
const PICTURE_THREADS: usize = 4;
const MAX_TABS: usize = 12;
const SIGN_UP_Q: &str = "Non hai ancora un account?";
const SIGN_UP: &str = "Registrati gratis";

// ---------------------------------------------------------------------------
// Faces

static WEB_FONTS: Mutex<Option<&'static WebFonts>> = Mutex::new(None);

/// The faces pages are drawn with (`sys:/system/webfonts.bin`), read once.
fn web_fonts() -> Option<&'static WebFonts> {
    let mut g = WEB_FONTS.lock();
    if g.is_none() {
        let raw = fs::abs("sys:/system/webfonts.bin").and_then(|p| fs::read_file(&p)).ok()?;
        let data: &'static [u8] = Box::leak(justzip::gunzip(&raw)?.into_boxed_slice());
        let mut fonts = justgfx::Fonts::parse(data)?;
        // On a high-density screen a 14-pixel face is drawn with the 28.
        fonts.double_sizes();
        *g = Some(Box::leak(Box::new(WebFonts::new(fonts))));
    }
    *g
}

/// The faces if they are loaded (never waits).
fn loaded_fonts() -> Option<&'static WebFonts> {
    WEB_FONTS.try_lock().and_then(|g| *g)
}

// ---------------------------------------------------------------------------
// Cookies

struct Cookie {
    domain: String,
    /// Only for the host that set it (no Domain attribute).
    host_only: bool,
    path: String,
    name: String,
    value: String,
    secure: bool,
}

static JAR: IrqLock<Vec<Cookie>> = IrqLock::new(Vec::new());

fn domain_matches(host: &str, domain: &str) -> bool {
    host == domain || (host.len() > domain.len() && host.ends_with(domain) && host.as_bytes()[host.len() - domain.len() - 1] == b'.')
}

/// The Cookie header for a request.
fn cookie_header(u: &http::Url) -> String {
    let path = u.path.split('?').next().unwrap_or("/");
    let jar = JAR.lock();
    let mut out = String::new();
    for c in jar.iter() {
        let host_ok = if c.host_only { u.host == c.domain } else { domain_matches(&u.host, &c.domain) };
        if host_ok && path.starts_with(c.path.as_str()) && (!c.secure || u.https) {
            if !out.is_empty() {
                out.push_str("; ");
            }
            out.push_str(&c.name);
            out.push('=');
            out.push_str(&c.value);
        }
    }
    out
}

/// Keeps the cookies a reply sets.
fn keep_cookies(u: &http::Url, headers: &[(String, String)]) {
    for (k, v) in headers {
        if !k.eq_ignore_ascii_case("set-cookie") {
            continue;
        }
        let mut parts = v.split(';');
        let Some((name, value)) = parts.next().and_then(|p| p.split_once('=')) else { continue };
        let mut c = Cookie { domain: u.host.clone(), host_only: true, path: String::from("/"), name: String::from(name.trim()), value: String::from(value.trim()), secure: false };
        let mut drop = c.name.is_empty();
        for a in parts {
            let (ak, av) = a.split_once('=').unwrap_or((a, ""));
            let av = av.trim();
            match ak.trim().to_ascii_lowercase().as_str() {
                "domain" => {
                    let d = av.trim_start_matches('.').to_ascii_lowercase();
                    if domain_matches(&u.host, &d) && d.contains('.') {
                        c.domain = d;
                        c.host_only = false;
                    } else {
                        drop = true;
                    }
                }
                "path" if av.starts_with('/') => c.path = String::from(av),
                "secure" => c.secure = true,
                "max-age" if av.parse::<i64>().is_ok_and(|n| n <= 0) => drop = true,
                "expires" if av.contains("1970") => drop = true,
                _ => {}
            }
        }
        let mut jar = JAR.lock();
        jar.retain(|o| !(o.name == c.name && o.domain == c.domain && o.path == c.path));
        if !drop {
            jar.push(c);
        }
        if jar.len() > 800 {
            jar.remove(0);
        }
    }
}

// ---------------------------------------------------------------------------
// Fetching

struct Got {
    /// Where the content came from, after redirects.
    url: String,
    status: u16,
    content_type: String,
    body: Vec<u8>,
}

/// Fetches an address with the session's cookies, following redirects.
fn fetch(address: &str, method: &str, body: Option<&str>, accept: &str) -> Result<Got, String> {
    let mut url = String::from(address.split('#').next().unwrap_or(address));
    let mut method = String::from(method);
    let mut body = body.map(String::from);
    for _ in 0..12 {
        let u = http::Url::parse(&url).ok_or_else(|| format!("indirizzo non valido: {url}"))?;
        let cookies = cookie_header(&u);
        let mut headers: Vec<(&str, &str)> = alloc::vec![("Accept", accept), ("Accept-Language", ACCEPT_LANGUAGE)];
        if !cookies.is_empty() {
            headers.push(("Cookie", &cookies));
        }
        if let Some(b) = &body {
            // Forms send fields; scripts mostly send JSON.
            let json = b.trim_start().starts_with(['{', '[']);
            headers.push(("Content-Type", if json { "application/json" } else { "application/x-www-form-urlencoded" }));
        }
        let req = Request { method: &method, url: &url, headers: &headers, body: body.as_deref().map(str::as_bytes) };
        let r = http::fetch_once(&req)?;
        keep_cookies(&u, &r.headers);
        if matches!(r.status, 301 | 302 | 303 | 307 | 308) {
            if let Some(loc) = r.header("location") {
                url = justweb::url::join(&url, loc);
                if r.status == 303 || (matches!(r.status, 301 | 302) && method == "POST") {
                    method = String::from("GET");
                    body = None;
                }
                continue;
            }
        }
        let encoding = r.header("content-encoding").map(|e| e.to_ascii_lowercase()).unwrap_or_default();
        let content_type = String::from(r.header("content-type").unwrap_or(""));
        let data = if encoding.contains("gzip") {
            justzip::gunzip(&r.body).ok_or("dati compressi non validi")?
        } else if encoding.contains("deflate") {
            justzip::zlib(&r.body).or_else(|| justzip::inflate_raw(&r.body).map(|d| d.0)).ok_or("dati compressi non validi")?
        } else {
            r.body
        };
        return Ok(Got { url, status: r.status, content_type, body: data });
    }
    Err(String::from("troppi reindirizzamenti"))
}

/// Can the browser show this kind of content (or should it be saved)?
fn viewable(ct: &str, body: &[u8]) -> bool {
    let ct = ct.to_ascii_lowercase();
    if ct.is_empty() {
        return !body.iter().take(512).any(|&b| b == 0) || justweb::image::size(body).is_some();
    }
    ct.starts_with("text/") || ct.contains("html") || ct.contains("xml") || ct.contains("json") || ct.contains("javascript") || matches!(ct.split(';').next().unwrap_or(""), "image/png" | "image/jpeg" | "image/jpg" | "image/gif")
}

/// Saves a download in the Downloads folder; returns its path.
fn save_download(url: &str, body: &[u8]) -> Result<String, String> {
    let last = url.split(['?', '#']).next().unwrap_or("").rsplit('/').next().unwrap_or("");
    let name = justweb::url::percent_decode(last);
    let name: String = name.chars().filter(|c| !matches!(c, '/' | '\\' | ':' | '\0')).collect();
    let name = if name.is_empty() { String::from("download") } else { name };
    let dir = fs::abs("sys:/home/Scaricati").map_err(|e| format!("{e:?}"))?;
    if !fs::exists(&dir) {
        let _ = fs::make_dir(&dir);
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (String::from(s), format!(".{e}")),
        _ => (name.clone(), String::new()),
    };
    let mut file = name.clone();
    for k in 2..100 {
        if !fs::exists(&dir.join(&file)) {
            break;
        }
        file = format!("{stem} {k}{ext}");
    }
    fs::write_file(&dir.join(&file), body).map_err(|e| format!("{e:?}"))?;
    Ok(format!("sys:/home/Scaricati/{file}"))
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// A page of the browser's own, in the ticly look.
fn own_page(title: &str, heading: &str, text: &str) -> String {
    format!(
        "<html><head><title>{}</title></head><body style='margin:0;background:#141011;color:#fff;font-family:sans-serif'>\
         <div style='max-width:560px;margin:90px auto;padding:36px;border-radius:22px;background:#1f1817;border:1px solid #352b29'>\
         <p style='font-size:22px;font-weight:bold;margin:0 0 18px'>ticly</p><h1 style='font-size:28px;margin:0 0 12px'>{}</h1>\
         <p style='color:#b8aca7;line-height:1.5'>{}</p></div></body></html>",
        escape(title),
        escape(heading),
        escape(text)
    )
}

// ---------------------------------------------------------------------------
// Pages

/// A page shared between the window and the threads loading it.
#[derive(Default)]
struct Page {
    doc: Option<Document>,
    /// Bumped whenever what is shown changes.
    version: u64,
    loading: bool,
    error: Option<String>,
    /// The address after redirects.
    url: String,
    title: String,
    /// The size the window shows the page at.
    want_w: i32,
    want_h: i32,
    /// A thread is laying the page out.
    busy: bool,
    /// The tab went elsewhere: stop loading.
    stop: bool,
    /// Pictures: (done, asked for).
    pictures: (usize, usize),
    /// Where <meta http-equiv=refresh> sends the page.
    redirect: Option<String>,
    /// What the page's scripts asked the window to do (open an address,
    /// send a form).
    hits: Vec<Hit>,
}

type Shared = Arc<Mutex<Page>>;

fn fail(page: &Shared, e: String) {
    let mut p = page.lock();
    p.error = Some(e);
    p.loading = false;
    p.version += 1;
}

/// Loads a page (on its own thread).
fn load(page: Shared, address: String, method: String, body: Option<String>) {
    let Some(fonts) = web_fonts() else {
        fail(&page, String::from("mancano i caratteri delle pagine web"));
        return;
    };
    let got = match fetch(&address, &method, body.as_deref(), ACCEPT_PAGE) {
        Ok(g) => g,
        Err(e) => {
            fail(&page, e);
            return;
        }
    };
    if page.lock().stop {
        return;
    }
    let mut doc = if viewable(&got.content_type, &got.body) {
        Document::new(&got.url, &got.body, &got.content_type)
    } else {
        let html = match save_download(&got.url, &got.body) {
            Ok(path) => own_page("Download", "File scaricato", &format!("{path} ({} kB) è nella cartella Scaricati.", got.body.len().div_ceil(1024))),
            Err(e) => own_page("Download", "Download non riuscito", &e),
        };
        Document::new(&got.url, html.as_bytes(), "text/html")
    };
    let _ = got.status;
    for _ in 0..4 {
        let want = doc.wanted_sheets();
        if want.is_empty() {
            break;
        }
        for u in want {
            let text = fetch(&u, "GET", None, "text/css,*/*;q=0.1").ok().filter(|g| g.status < 400).map(|g| String::from_utf8_lossy(&g.body).into_owned());
            doc.add_sheet(&u, text.as_deref());
        }
        if page.lock().stop {
            return;
        }
    }
    let (w, h) = {
        let p = page.lock();
        (p.want_w.max(320), p.want_h.max(240))
    };
    doc.lay_out(fonts, w, h);
    {
        let mut p = page.lock();
        if p.stop {
            return;
        }
        p.url = got.url.clone();
        p.title = doc.title.clone();
        p.redirect = doc.refresh();
        let scripts = doc.scripts();
        p.doc = Some(doc);
        p.version += 1;
        if !scripts.is_empty() {
            let pg = page.clone();
            if crate::task::spawn_big("web-script", SCRIPT_STACK, move || run_scripts(pg, scripts)).is_none() {
                crate::warn!("web", "no memory for the scripts of {}", got.url);
            }
        }
    }
    pictures(&page);
    let mut p = page.lock();
    p.loading = false;
    p.version += 1;
}

/// Fetches the pictures a page wants, a few at a time.
fn pictures(page: &Shared) {
    for _ in 0..3 {
        let want = {
            let mut p = page.lock();
            if p.stop {
                return;
            }
            let w = p.doc.as_mut().map(|d| d.wanted_pictures()).unwrap_or_default();
            p.pictures.1 += w.len();
            w
        };
        if want.is_empty() {
            return;
        }
        let queue = Arc::new(IrqLock::new(want));
        let running = Arc::new(AtomicUsize::new(PICTURE_THREADS));
        for _ in 1..PICTURE_THREADS {
            let (page, queue, done) = (page.clone(), queue.clone(), running.clone());
            let started = crate::task::spawn("web-pictures", move || {
                picture_worker(&page, &queue);
                done.fetch_sub(1, Ordering::Release);
            });
            if started.is_none() {
                running.fetch_sub(1, Ordering::Release);
            }
        }
        picture_worker(page, &queue);
        running.fetch_sub(1, Ordering::Release);
        while running.load(Ordering::Acquire) > 0 {
            crate::task::sleep_ms(20);
        }
    }
}

fn picture_worker(page: &Shared, queue: &IrqLock<Vec<String>>) {
    loop {
        let Some(u) = queue.lock().pop() else { return };
        if page.lock().stop {
            return;
        }
        let canvas = fetch(&u, "GET", None, "image/png,image/jpeg,image/gif,image/*;q=0.8,*/*;q=0.5")
            .ok()
            .filter(|g| g.status < 400)
            .and_then(|g| justweb::image::decode(&g.body));
        let mut p = page.lock();
        if let Some(d) = p.doc.as_mut() {
            d.add_picture(&u, canvas);
        }
        p.pictures.0 += 1;
        p.version += 1;
    }
}

/// Steady milliseconds for the page's scripts.
fn script_now() -> f64 {
    crate::time::uptime_ms() as f64
}

/// `fetch` and `XMLHttpRequest` of the page's scripts.
fn script_fetch(method: &str, url: &str, body: Option<&str>) -> Result<(u16, String, String, String), String> {
    let g = fetch(url, method, body, "*/*")?;
    Ok((g.status, g.content_type, String::from_utf8_lossy(&g.body).into_owned(), g.url))
}

/// Writes what the page's scripts printed to the journal.
fn log_console(s: &mut Scripts) {
    for line in s.take_console() {
        crate::debug!("web-js", "{line}");
    }
}

/// Runs the scripts of a page, then its events and timers, until the tab
/// leaves the page (on a thread of its own: the interpreter recurses deeply
/// and a slow script must not hold up the window). The page is locked
/// while a script runs; the window only ever tries the lock.
fn run_scripts(page: Shared, list: Vec<Script>) {
    let mut sources = Vec::new();
    for s in list {
        match s {
            Script::Inline(src) => sources.push(src),
            Script::External(u) => {
                if let Some(g) = fetch(&u, "GET", None, "*/*").ok().filter(|g| g.status < 400) {
                    sources.push(String::from_utf8_lossy(&g.body).into_owned());
                }
            }
        }
        if page.lock().stop {
            return;
        }
    }
    let epoch = crate::time::unix_now() as f64 * 1000.0 - script_now();
    let mut scripts = {
        let mut p = page.lock();
        let Some(doc) = p.doc.as_mut() else { return };
        Scripts::new(doc, script_now(), epoch, Some(script_fetch))
    };
    // Each script on its own, so the window can paint in between.
    for src in sources.iter().map(|s| Some(s.as_str())).chain([None]) {
        let mut p = page.lock();
        if p.stop {
            return;
        }
        let Some(doc) = p.doc.as_mut() else { return };
        match src {
            Some(src) => scripts.run(doc, src, script_now()),
            None => scripts.loaded(doc, script_now()),
        }
        publish(&mut p, &mut scripts);
        drop(p);
        log_console(&mut scripts);
        crate::task::yield_now();
    }
    loop {
        crate::task::sleep_ms(SCRIPT_TICK_MS);
        let Some(mut p) = page.try_lock() else { continue };
        if p.stop {
            return;
        }
        let Some(doc) = p.doc.as_mut() else { return };
        if scripts.pump(doc, script_now()) {
            publish(&mut p, &mut scripts);
            drop(p);
            log_console(&mut scripts);
        }
    }
}

/// Shows what the scripts changed.
fn publish(p: &mut Page, scripts: &mut Scripts) {
    if let Some(d) = &p.doc {
        p.url = d.url.clone();
        p.title = d.title.clone();
    }
    p.hits.extend(scripts.take_hits());
    p.version += 1;
}

/// Lays a page out again for the window's size (on its own thread).
fn relayout(page: Shared) {
    let Some(fonts) = web_fonts() else { return };
    let mut p = page.lock();
    let (w, h) = (p.want_w.max(320), p.want_h.max(240));
    if let Some(d) = p.doc.as_mut() {
        d.lay_out(fonts, w, h);
    }
    p.busy = false;
    p.version += 1;
}

/// What was typed into the address bar: an address or a search.
fn resolve(input: &str) -> String {
    let t = input.trim();
    let lower = t.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return String::from(t);
    }
    let hostish = !t.contains(' ') && (t.starts_with("localhost") || (t.contains('.') && !t.starts_with('.') && !t.ends_with('.')));
    if hostish {
        // Machines of the local network rarely speak https.
        let host = t.split(['/', ':']).next().unwrap_or("");
        let local = host == "localhost" || host.split('.').count() == 4 && host.split('.').all(|p| p.parse::<u8>().is_ok());
        return format!("{}://{t}", if local { "http" } else { "https" });
    }
    format!("{SEARCH}{}", justweb::url::form_encode(t))
}

fn host_of(url: &str) -> String {
    let h = justweb::url::host(url);
    String::from(h.strip_prefix("www.").unwrap_or(h))
}

// ---------------------------------------------------------------------------
// The window

struct Tab {
    /// None: the start page.
    page: Option<Shared>,
    history: Vec<String>,
    at: usize,
    scroll: i32,
    /// The page as last painted, and what it was painted for.
    view: Option<Canvas>,
    painted: (u64, i32, i32, i32, bool),
    dirty: bool,
    doc_h: i32,
    title: String,
    url: String,
    loading: bool,
    error: Option<String>,
    progress: (usize, usize),
}

impl Tab {
    fn new() -> Tab {
        Tab {
            page: None,
            history: alloc::vec![String::new()],
            at: 0,
            scroll: 0,
            view: None,
            painted: (0, 0, 0, 0, false),
            dirty: true,
            doc_h: 0,
            title: String::from("Nuova scheda"),
            url: String::new(),
            loading: false,
            error: None,
            progress: (0, 0),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    None,
    Address,
    Search,
    Find,
    Page,
}

/// The menu of the ⋮ button.
const MENU: [&str; 5] = ["Nuova scheda", "Trova nella pagina", "Ricarica", "Pagina iniziale", "Chiudi scheda"];

pub struct Browser {
    tabs: Vec<Tab>,
    cur: usize,
    focus: Focus,
    address: String,
    search: String,
    find: Option<String>,
    find_missing: bool,
    menu: bool,
    /// An open <select>: node, options, chosen, where.
    select: Option<(NodeId, Vec<String>, usize, Rect)>,
    hover: Option<Rect>,
    hover_link: Option<String>,
    area: Rect,
    ticks: u64,
    caret_on: bool,
}

impl Browser {
    pub fn new(url: Option<&str>) -> Browser {
        let mut b = Browser {
            tabs: alloc::vec![Tab::new()],
            cur: 0,
            focus: Focus::Search,
            address: String::new(),
            search: String::new(),
            find: None,
            find_missing: false,
            menu: false,
            select: None,
            hover: None,
            hover_link: None,
            area: Rect::new(0, 0, 1100, 720),
            ticks: 0,
            caret_on: true,
        };
        // Read the faces in the background, so the first page is quick.
        crate::task::spawn("web-fonts", || {
            let _ = web_fonts();
        });
        if let Some(u) = url {
            b.navigate(String::from(u));
        }
        b
    }

    fn tab(&mut self) -> &mut Tab {
        &mut self.tabs[self.cur]
    }

    fn content(area: Rect) -> Rect {
        Rect::new(area.x, area.y + BAR_H, area.w, (area.h - BAR_H).max(0))
    }

    // --- geometry of the chrome

    fn tab_rects(&self, area: Rect) -> Vec<Rect> {
        let x0 = area.x + 112;
        let avail = area.w - 112 - 70;
        let n = self.tabs.len() as i32;
        let w = (avail / n.max(1) - 6).clamp(60, 230);
        (0..n).map(|i| Rect::new(x0 + i * (w + 6), area.y + 8, w, 32)).collect()
    }

    fn tab_close(r: Rect) -> Rect {
        Rect::new(r.right() - 28, r.y + 6, 20, 20)
    }

    fn new_tab_rect(&self, area: Rect) -> Rect {
        let last = self.tab_rects(area).last().copied().unwrap_or(Rect::new(area.x + 112, area.y + 8, 0, 32));
        Rect::new(last.right() + 8, area.y + 9, 30, 30)
    }

    fn tool_buttons(area: Rect) -> [Rect; 4] {
        let y = area.y + TABS_H + 10;
        [0, 1, 2, 3].map(|i| Rect::new(area.x + 12 + i * 38, y, 34, 34))
    }

    fn address_rect(area: Rect) -> Rect {
        let x = area.x + 12 + 4 * 38 + 8;
        Rect::new(x, area.y + TABS_H + 8, area.right() - 96 - x, 38)
    }

    fn counter_rect(area: Rect) -> Rect {
        Rect::new(area.right() - 84, area.y + TABS_H + 13, 28, 28)
    }

    fn menu_button(area: Rect) -> Rect {
        Rect::new(area.right() - 46, area.y + TABS_H + 10, 34, 34)
    }

    fn menu_rows(area: Rect) -> Vec<Rect> {
        let b = Self::menu_button(area);
        (0..MENU.len() as i32).map(|i| Rect::new(b.right() - 240, b.bottom() + 12 + i * 40, 240, 38)).collect()
    }

    fn find_rect(area: Rect) -> Rect {
        let c = Self::content(area);
        Rect::new(c.right() - 380, c.y + 12, 360, 46)
    }

    // --- geometry of the start page

    fn start_cards(area: Rect) -> (Rect, Rect) {
        let c = Self::content(area);
        let w = (c.w - 64).min(980);
        let h = 506.min(c.h - 32).max(380);
        let lw = w * 575 / 1000;
        let x = c.x + (c.w - w) / 2;
        let y = c.y + (c.h - h) / 2;
        (Rect::new(x, y, lw, h), Rect::new(x + lw + 14, y, w - lw - 14, h))
    }

    /// The top of the right card's content (centred in the card).
    fn start_top(area: Rect) -> i32 {
        let (_, r) = Self::start_cards(area);
        r.y + ((r.h - 344) / 2).max(16)
    }

    fn start_search(area: Rect) -> Rect {
        let (_, r) = Self::start_cards(area);
        Rect::new(r.x + 36, Self::start_top(area) + 26, r.w - 72, 46)
    }

    /// Search, sign in, workspace, and the "sign up" link.
    fn start_buttons(area: Rect) -> [Rect; 4] {
        let (_, r) = Self::start_cards(area);
        let top = Self::start_top(area);
        let w = r.w - 72;
        let (qw, rw) = (text::width(Font::Small, SIGN_UP_Q), text::width(Font::Strong, SIGN_UP));
        let x = r.x + (r.w - qw - rw - 6) / 2;
        [
            Rect::new(r.x + 36, top + 90, w, 46),
            Rect::new(r.x + 36, top + 198, w, 46),
            Rect::new(r.x + 36, top + 256, w, 46),
            Rect::new(x + qw + 6, top + 318, rw, 22),
        ]
    }

    // --- navigation

    fn navigate(&mut self, url: String) {
        let t = self.tab();
        t.history.truncate(t.at + 1);
        t.history.push(url.clone());
        t.at = t.history.len() - 1;
        self.open(url, "GET", None);
    }

    /// Shows an address of the history (without adding to it).
    fn open(&mut self, url: String, method: &str, body: Option<String>) {
        let (w, h) = (Self::content(self.area).w, Self::content(self.area).h);
        self.focus = if url.is_empty() { Focus::Search } else { Focus::None };
        self.find = None;
        self.select = None;
        self.hover_link = None;
        let t = &mut self.tabs[self.cur];
        if let Some(p) = t.page.take() {
            p.lock().stop = true;
        }
        t.scroll = 0;
        t.error = None;
        t.dirty = true;
        t.doc_h = 0;
        t.url = url.clone();
        if url.is_empty() {
            t.title = String::from("Nuova scheda");
            t.loading = false;
            t.view = None;
            self.address.clear();
            return;
        }
        t.title = host_of(&url);
        t.loading = true;
        t.progress = (0, 0);
        let page = Arc::new(Mutex::new(Page { loading: true, url: url.clone(), want_w: w, want_h: h, ..Default::default() }));
        t.page = Some(page.clone());
        self.address = url.clone();
        let method = String::from(method);
        if crate::task::spawn_big("web", STACK, move || load(page.clone(), url, method, body)).is_none() {
            let t = &mut self.tabs[self.cur];
            t.loading = false;
            t.error = Some(String::from("memoria insufficiente"));
        }
    }

    fn back(&mut self) {
        let t = self.tab();
        if t.at > 0 {
            t.at -= 1;
            let u = t.history[t.at].clone();
            self.open(u, "GET", None);
        }
    }

    fn forward(&mut self) {
        let t = self.tab();
        if t.at + 1 < t.history.len() {
            t.at += 1;
            let u = t.history[t.at].clone();
            self.open(u, "GET", None);
        }
    }

    fn reload(&mut self) {
        let t = self.tab();
        let u = t.history[t.at].clone();
        self.open(u, "GET", None);
    }

    fn new_tab(&mut self) {
        if self.tabs.len() < MAX_TABS {
            self.tabs.push(Tab::new());
            self.cur = self.tabs.len() - 1;
            self.address.clear();
            self.focus = Focus::Search;
            self.find = None;
        }
    }

    fn close_tab(&mut self, i: usize) {
        if let Some(p) = &self.tabs[i].page {
            p.lock().stop = true;
        }
        if self.tabs.len() == 1 {
            self.tabs[0] = Tab::new();
            self.address.clear();
            self.focus = Focus::Search;
            return;
        }
        self.tabs.remove(i);
        if self.cur >= self.tabs.len() || self.cur > i {
            self.cur = self.cur.saturating_sub(1).min(self.tabs.len() - 1);
        }
        self.switch(self.cur);
    }

    fn switch(&mut self, i: usize) {
        self.cur = i;
        self.address = self.tabs[i].url.clone();
        self.focus = if self.tabs[i].page.is_none() { Focus::Search } else { Focus::None };
        self.find = None;
        self.select = None;
        self.tabs[i].dirty = true;
    }

    /// Follows a link of the page (to an anchor of the same page: scrolls).
    fn follow(&mut self, url: String) {
        if let Some((base, frag)) = url.split_once('#') {
            let here = self.tab().url.split('#').next().unwrap_or("").to_string();
            if base == here {
                let y = self.tab().page.as_ref().and_then(|p| p.try_lock().and_then(|p| p.doc.as_ref().and_then(|d| d.anchor(frag))));
                if let Some(y) = y {
                    let t = self.tab();
                    t.scroll = y;
                    t.dirty = true;
                    return;
                }
            }
        }
        if url.to_ascii_lowercase().starts_with("mailto:") {
            return;
        }
        self.navigate(url);
    }

    fn scroll_by(&mut self, dy: i32) -> bool {
        let view_h = Self::content(self.area).h;
        let t = self.tab();
        let max = (t.doc_h - view_h).max(0);
        let s = (t.scroll + dy).clamp(0, max);
        if s != t.scroll {
            t.scroll = s;
            true
        } else {
            false
        }
    }

    /// Runs `f` on the current page's document, if it is not busy.
    fn with_doc<T>(&mut self, f: impl FnOnce(&mut Document) -> T) -> Option<T> {
        let page = self.tabs[self.cur].page.clone()?;
        let mut p = page.try_lock()?;
        let r = p.doc.as_mut().map(f);
        p.version += 1;
        r
    }

    fn handle_hit(&mut self, hit: Option<Hit>) -> Action {
        self.tab().dirty = true;
        match hit {
            Some(Hit::Open(u)) => self.follow(u),
            Some(Hit::Submit { method, url, body }) => {
                let t = self.tab();
                t.history.truncate(t.at + 1);
                t.history.push(url.clone());
                t.at = t.history.len() - 1;
                self.open(url, &method, body);
            }
            Some(Hit::Focus(_)) => self.focus = Focus::Page,
            // The page's scripts take it from here.
            Some(Hit::Changed | Hit::Script) => {}
            Some(Hit::Choose(n, opts, sel)) => {
                let c = Self::content(self.area);
                let h = (opts.len().min(10) as i32) * 34 + 12;
                let at = self.hover.unwrap_or(Rect::new(c.x + c.w / 2 - 130, c.y + 80, 260, 0));
                let y = if at.y + h > c.bottom() { (c.bottom() - h - 8).max(c.y) } else { at.y };
                self.select = Some((n, opts, sel, Rect::new(at.x.min(c.right() - 270), y, 260, h)));
            }
            None => {
                if self.focus == Focus::Page {
                    self.focus = Focus::None;
                }
            }
        }
        Action::Redraw
    }

    // --- drawing

    fn draw_chrome(&mut self, c: &mut Canvas, area: Rect, focused: bool) {
        let bar = Rect::new(area.x, area.y, area.w, BAR_H);
        c.fill(bar, CHROME);
        c.glow_round(bar, 0, area.x + 60, area.y, 520, ui::GLOW, 150);
        c.fill(Rect::new(area.x, bar.bottom() - 1, area.w, 1), 0x24FF_FFFF);
        tui::wordmark(c, area.x + 20, area.y + 11, "");
        // Tabs.
        let rects = self.tab_rects(area);
        for (i, r) in rects.iter().enumerate() {
            let active = i == self.cur;
            let t = &self.tabs[i];
            c.fill_round(*r, 12, if active { 0xFF2A_201E } else if self.hover == Some(*r) { 0x1AFF_FFFF } else { 0x0AFF_FFFF });
            if active {
                c.glow_round(*r, 12, r.x + 20, r.y, r.w * 2 / 3, ui::GLOW, 90);
                c.stroke_round(*r, 12, 1, 0x2EFF_FFFF);
            }
            // The site's letter in a little circle (a spinner while loading).
            let (cx, cy) = (r.x + 18, r.y + 16);
            if t.loading {
                let a = (self.ticks % 8) as i32;
                c.fill_circle(cx, cy, 9, 0x26FF_FFFF);
                let (dx, dy) = [(0, -6), (4, -4), (6, 0), (4, 4), (0, 6), (-4, 4), (-6, 0), (-4, -4)][a as usize];
                c.fill_circle(cx + dx, cy + dy, 3, ui::ACCENT);
            } else {
                c.fill_circle(cx, cy, 9, 0xFF3A_302D);
                let letter: String = t.title.chars().next().map(|ch| ch.to_lowercase().collect()).unwrap_or_else(|| String::from("n"));
                let (lh, _) = text::metrics(Font::Small);
                text::draw_centered(c, Font::Small, cx, cy - lh / 2, &letter, TEXT_DIM);
            }
            let tw = r.w - 64;
            if tw > 10 {
                text::draw(c, Font::Body, r.x + 34, r.y + 7, &text::fit(Font::Body, &t.title, tw), if active { TEXT } else { TEXT_DIM });
            }
            if active || self.hover == Some(Self::tab_close(*r)) || r.w > 120 {
                let x = Self::tab_close(*r);
                if self.hover == Some(x) {
                    c.fill_circle(x.x + 10, x.y + 10, 10, 0x26FF_FFFF);
                }
                res::draw_icon(c, "g-close", 10, x.x + 5, x.y + 5);
            }
        }
        let nt = self.new_tab_rect(area);
        if self.hover == Some(nt) {
            c.fill_circle(nt.x + 15, nt.y + 15, 15, 0x1FFF_FFFF);
        }
        res::draw_icon(c, "g-plus", 16, nt.x + 7, nt.y + 7);
        // Toolbar.
        let t = &self.tabs[self.cur];
        let enabled = [t.at > 0, t.at + 1 < t.history.len(), true, true];
        for (i, (r, glyph)) in Self::tool_buttons(area).iter().zip(["g-back", "g-forward", if t.loading { "g-close" } else { "g-reload" }, "g-home"]).enumerate() {
            if self.hover == Some(*r) && enabled[i] {
                c.fill_circle(r.x + 17, r.y + 17, 17, 0x1FFF_FFFF);
            }
            let s = if i == 2 && t.loading { 13 } else { 17 };
            if enabled[i] {
                res::draw_icon(c, glyph, s, r.x + (34 - s) / 2, r.y + (34 - s) / 2);
            } else {
                res::draw_icon_faded(c, glyph, s, r.x + (34 - s) / 2, r.y + (34 - s) / 2, 90);
            }
        }
        let ar = Self::address_rect(area);
        let editing = focused && self.focus == Focus::Address;
        c.fill_round(ar, ar.h / 2, 0xFF12_0E0E);
        c.stroke_round(ar, ar.h / 2, 1, if editing { 0x59FF_FFFF } else { 0x26FF_FFFF });
        let secure = t.url.starts_with("https://");
        if t.page.is_some() && !editing {
            // A padlock for secure pages.
            let (lx, ly) = (ar.x + 16, ar.y + 13);
            let col = if secure { TEXT_DIM } else { ui::ORANGE };
            c.stroke_round(Rect::new(lx + 2, ly - 1, 8, 9), 4, 2, col);
            c.fill_round(Rect::new(lx, ly + 4, 12, 9), 2, col);
        } else {
            res::draw_icon(c, "g-search", 14, ar.x + 16, ar.y + 12);
        }
        let (lh, _) = text::metrics(Font::Body);
        let ty = ar.y + (ar.h - lh) / 2;
        let tx = ar.x + 40;
        if self.address.is_empty() && !(editing && !self.address.is_empty()) {
            text::draw(c, Font::Body, tx, ty, "Cerca o digita un indirizzo", TEXT_FAINT);
            if editing && self.caret_on {
                c.fill(Rect::new(tx, ty + 2, 2, lh - 4), ui::ACCENT);
            }
        } else if editing {
            let shown = tail_fit(Font::Body, &self.address, ar.w - 64);
            let w = text::draw(c, Font::Body, tx, ty, &shown, TEXT);
            if self.caret_on {
                c.fill(Rect::new(tx + w + 1, ty + 2, 2, lh - 4), ui::ACCENT);
            }
        } else {
            // The host bright, the rest dimmer.
            let shown = self.address.strip_prefix("https://").unwrap_or(&self.address);
            let (host, rest) = match shown.find('/') {
                Some(i) => (&shown[..i], &shown[i..]),
                None => (shown, ""),
            };
            let w = text::draw(c, Font::Body, tx, ty, &text::fit(Font::Body, host, ar.w - 64), TEXT);
            if ar.w - 64 - w > 30 && rest != "/" {
                text::draw(c, Font::Body, tx + w, ty, &text::fit(Font::Body, rest, ar.w - 64 - w), TEXT_FAINT);
            }
        }
        // Tab counter and menu.
        let cr = Self::counter_rect(area);
        if self.hover == Some(cr) {
            c.fill_circle(cr.x + 14, cr.y + 14, 17, 0x1FFF_FFFF);
        }
        c.stroke_round(cr.grow(-3), 5, 2, TEXT);
        let n = format!("{}", self.tabs.len());
        let (sh, _) = text::metrics(Font::Strong);
        text::draw_centered(c, Font::Strong, cr.x + 14, cr.y + (28 - sh) / 2, &n, TEXT);
        let mb = Self::menu_button(area);
        if self.hover == Some(mb) || self.menu {
            c.fill_circle(mb.x + 17, mb.y + 17, 17, 0x1FFF_FFFF);
        }
        res::draw_icon(c, "g-more", 18, mb.x + 8, mb.y + 8);
        // Loading progress under the toolbar.
        if t.loading {
            let (done, all) = t.progress;
            let frac = if t.page.as_ref().is_some_and(|_| t.doc_h > 0) { 400 + 600 * done as i32 / all.max(1) as i32 } else { 150 + (self.ticks % 20) as i32 * 10 };
            c.fill(Rect::new(area.x, bar.bottom() - 2, area.w * frac.min(1000) / 1000, 2), ui::ACCENT);
        }
    }

    fn draw_start(&mut self, c: &mut Canvas, blur: &Canvas, area: Rect, focused: bool) {
        let content = Self::content(area);
        c.blit_round(blur, content, 0, 0x5A0E_0A0A);
        let (l, r) = Self::start_cards(area);
        ui::card(c, blur, l, 22, ui::GLASS_STRONG);
        ui::card(c, blur, r, 22, ui::GLASS_STRONG);
        // Left: the welcome.
        tui::wordmark(c, l.x + 36, l.y + 40, "");
        let mut y = l.y + 92;
        for line in text::wrap(Font::Title, "Bentornato nel tuo workspace.", l.w - 72).iter().take(2) {
            text::draw(c, Font::Title, l.x + 36, y, line, TEXT);
            y += 44;
        }
        y += 14;
        let lead = "Un unico browser per la tua identità ticly: musica, chat, intelligenza artificiale e il tuo workspace, su un motore web scritto da zero per JustOS.";
        for line in text::wrap(Font::Body, lead, l.w - 72).iter().take(4) {
            text::draw(c, Font::Body, l.x + 36, y, line, TEXT_DIM);
            y += 22;
        }
        let rows = [
            (false, "Motore justweb", "HTML e CSS in Rust, nel kernel di JustOS, senza Chromium"),
            (true, "Single Sign-On Ecosistema", "Una sessione attiva su tutti i servizi ticly"),
        ];
        let mut ry = (y + 22).max(l.bottom() - 186);
        for (bolt, title, sub) in rows {
            let rr = Rect::new(l.x + 36, ry, l.w - 72, 70);
            if rr.bottom() > l.bottom() - 12 {
                break;
            }
            c.fill_round(rr, 14, 0x0DFF_FFFF);
            c.stroke_round(rr, 14, 1, ui::BORDER);
            let ib = Rect::new(rr.x + 16, rr.y + 20, 30, 30);
            c.fill_round(ib, 8, 0x14FF_FFFF);
            c.stroke_round(ib, 8, 1, ui::BORDER);
            let (cx, cy) = (ib.x + 15, ib.y + 15);
            if bolt {
                // A lightning bolt.
                c.triangle([(cx + 2, cy - 9), (cx - 6, cy + 2), (cx + 1, cy + 1)], ui::ORANGE);
                c.triangle([(cx - 1, cy - 1), (cx + 6, cy - 2), (cx - 2, cy + 9)], ui::ORANGE);
            } else {
                // A diamond.
                c.triangle([(cx, cy - 7), (cx - 7, cy), (cx + 7, cy)], TEXT_DIM);
                c.triangle([(cx - 7, cy), (cx + 7, cy), (cx, cy + 7)], TEXT_DIM);
            }
            text::draw(c, Font::Strong, rr.x + 60, rr.y + 15, title, TEXT);
            text::draw(c, Font::Small, rr.x + 60, rr.y + 39, &text::fit(Font::Small, sub, rr.w - 76), TEXT_FAINT);
            ry += 80;
        }
        // Right: search and ticly.
        let top = Self::start_top(area);
        ui::caps(c, r.x + 36, top, "Cerca nel web", TEXT_DIM);
        text::draw_right(c, Font::Small, r.right() - 36, top - 2, "DuckDuckGo", TEXT_DIM);
        let sr = Self::start_search(area);
        ui::text_field(c, sr, &text::fit(Font::Heading, &self.search, sr.w - 40), "Cerca o digita un indirizzo", focused && self.focus == Focus::Search && self.caret_on);
        let [go, login, ws, reg] = Self::start_buttons(area);
        ui::button(c, go, "Cerca →", true, self.hover == Some(go));
        let oy = top + 164;
        let ow = ui::caps_width("Oppure");
        c.fill(Rect::new(r.x + 36, oy + 6, (r.w - 72 - ow) / 2 - 12, 1), ui::SEPARATOR);
        ui::caps(c, r.x + (r.w - ow) / 2, oy, "Oppure", TEXT_FAINT);
        c.fill(Rect::new(r.x + (r.w + ow) / 2 + 12, oy + 6, (r.w - 72 - ow) / 2 - 12, 1), ui::SEPARATOR);
        ui::button(c, login, "Accedi a ticly", false, self.hover == Some(login));
        ui::button(c, ws, "Apri il workspace do.ticly.cloud", false, self.hover == Some(ws));
        text::draw(c, Font::Small, reg.x - 6 - text::width(Font::Small, SIGN_UP_Q), reg.y + 2, SIGN_UP_Q, TEXT_DIM);
        text::draw(c, Font::Strong, reg.x, reg.y, SIGN_UP, TEXT);
        c.fill(Rect::new(reg.x, reg.y + 19, reg.w, 1), if self.hover == Some(reg) { TEXT } else { 0x99FF_FFFF });
    }

    fn draw_page(&mut self, c: &mut Canvas, area: Rect, focused: bool) {
        let content = Self::content(area);
        let caret = focused && self.focus == Focus::Page && self.caret_on;
        let t = &mut self.tabs[self.cur];
        let Some(page) = t.page.clone() else { return };
        if let Some(mut p) = page.try_lock() {
            p.want_w = content.w;
            p.want_h = content.h;
            t.title = if p.title.is_empty() { host_of(&p.url) } else { p.title.clone() };
            t.url = p.url.clone();
            t.loading = p.loading;
            t.error = p.error.clone();
            t.progress = p.pictures;
            if let Some(d) = &p.doc {
                t.doc_h = d.height();
                t.scroll = t.scroll.clamp(0, (t.doc_h - content.h).max(0));
                let key = (p.version, t.scroll, content.w, content.h, caret);
                let k = crate::gui::scale();
                if t.dirty || key != t.painted || t.view.as_ref().is_none_or(|v| v.scale() != k) {
                    if let Some(f) = loaded_fonts() {
                        let v = match t.view.take() {
                            Some(v) if v.w == content.w && v.h == content.h && v.scale() == k => v,
                            _ => Canvas::new_at(content.w.max(1), content.h.max(1), 0xFFFF_FFFF, k),
                        };
                        let mut v = v;
                        d.paint(&mut v, f, Rect::new(0, 0, content.w, content.h), t.scroll, caret);
                        t.view = Some(v);
                        t.painted = key;
                        t.dirty = false;
                    }
                }
            } else if p.error.is_some() || !p.loading {
                t.view = None;
            }
        }
        if self.focus != Focus::Address {
            self.address = self.tabs[self.cur].url.clone();
        }
        let t = &self.tabs[self.cur];
        match &t.view {
            Some(v) => c.blit(v, 0, 0, content),
            None => {
                c.fill(content, 0xFF14_1011);
                if let Some(e) = &t.error {
                    let card = Rect::new(content.x + (content.w - 520) / 2, content.y + 90, 520, 220);
                    c.fill_round(card, 22, 0xFF1F_1817);
                    c.stroke_round(card, 22, 1, ui::BORDER);
                    c.glow_round(card, 22, card.x + 60, card.y, 300, ui::GLOW, 90);
                    tui::wordmark(c, card.x + 32, card.y + 28, "");
                    text::draw(c, Font::Heading, card.x + 32, card.y + 72, "Impossibile aprire la pagina", TEXT);
                    let msg = tui::net_error(e);
                    for (i, l) in text::wrap(Font::Body, &msg, card.w - 64).iter().take(3).enumerate() {
                        text::draw(c, Font::Body, card.x + 32, card.y + 106 + i as i32 * 21, l, TEXT_DIM);
                    }
                    let b = Rect::new(card.x + 32, card.bottom() - 62, 140, 40);
                    ui::button(c, b, "Riprova", true, self.hover == Some(b));
                } else {
                    let (lh, _) = text::metrics(Font::Body);
                    text::draw_centered(c, Font::Body, content.x + content.w / 2, content.y + content.h / 2 - lh, "Caricamento…", TEXT_DIM);
                }
            }
        }
        // Scroll bar.
        if t.view.is_some() && t.doc_h > content.h {
            let h = (content.h * content.h / t.doc_h).max(30);
            let y = content.y + (content.h - h) * t.scroll / (t.doc_h - content.h).max(1);
            c.fill_round(Rect::new(content.right() - 8, y + 2, 5, h - 4), 3, 0x66_80_80_80);
        }
    }

    fn retry_rect(area: Rect) -> Rect {
        let content = Self::content(area);
        let card = Rect::new(content.x + (content.w - 520) / 2, content.y + 90, 520, 220);
        Rect::new(card.x + 32, card.bottom() - 62, 140, 40)
    }

    fn draw_overlays(&mut self, c: &mut Canvas, blur: &Canvas, area: Rect, focused: bool) {
        let content = Self::content(area);
        if let Some(link) = &self.hover_link {
            let s = text::fit(Font::Small, link, content.w / 2);
            let w = text::width(Font::Small, &s) + 24;
            let r = Rect::new(content.x + 8, content.bottom() - 34, w, 26);
            c.fill_round(r, 13, 0xE614_1011);
            c.stroke_round(r, 13, 1, ui::BORDER);
            text::draw(c, Font::Small, r.x + 12, r.y + 6, &s, TEXT_DIM);
        }
        if let Some(q) = &self.find {
            let r = Self::find_rect(area);
            ui::glass(c, blur, r, 16, ui::GLASS_STRONG);
            let f = Rect::new(r.x + 8, r.y + 7, r.w - 16, 32);
            ui::search_field(c, f, q, "Trova nella pagina", focused && self.focus == Focus::Find && self.caret_on);
            if self.find_missing {
                text::draw_right(c, Font::Small, f.right() - 12, f.y + 8, "Nessun risultato", ui::RED);
            }
        }
        if let Some((_, opts, sel, r)) = &self.select {
            ui::glass(c, blur, *r, 14, ui::GLASS_STRONG);
            for (i, o) in opts.iter().take(10).enumerate() {
                let rr = Rect::new(r.x + 6, r.y + 6 + i as i32 * 34, r.w - 12, 32);
                if i == *sel || self.hover == Some(rr) {
                    c.fill_round(rr, 9, if i == *sel { ui::SELECTED } else { ui::PANEL_HOVER });
                }
                text::draw(c, Font::Body, rr.x + 12, rr.y + 7, &text::fit(Font::Body, o, rr.w - 24), TEXT);
            }
        }
        if self.menu {
            let rows = Self::menu_rows(area);
            if let (Some(f), Some(l)) = (rows.first(), rows.last()) {
                ui::glass(c, blur, Rect::new(f.x - 6, f.y - 6, f.w + 12, l.bottom() - f.y + 12), 16, ui::GLASS_STRONG);
            }
            for (r, label) in rows.iter().zip(MENU) {
                if self.hover == Some(*r) {
                    c.fill_round(*r, 10, ui::PANEL_HOVER);
                }
                text::draw(c, Font::Body, r.x + 14, r.y + 10, label, TEXT);
            }
        }
    }

    fn hover_targets(&self, area: Rect) -> Vec<Rect> {
        let mut v = Vec::new();
        for r in self.tab_rects(area) {
            v.push(Self::tab_close(r));
            v.push(r);
        }
        v.push(self.new_tab_rect(area));
        v.extend(Self::tool_buttons(area));
        v.push(Self::counter_rect(area));
        v.push(Self::menu_button(area));
        if self.menu {
            v.extend(Self::menu_rows(area));
        }
        if let Some((_, opts, _, r)) = &self.select {
            v.extend((0..opts.len().min(10) as i32).map(|i| Rect::new(r.x + 6, r.y + 6 + i * 34, r.w - 12, 32)));
        }
        let t = &self.tabs[self.cur];
        if t.page.is_none() {
            v.extend(Self::start_buttons(area));
        } else if t.view.is_none() && t.error.is_some() {
            v.push(Self::retry_rect(area));
        }
        v
    }

    fn find_now(&mut self) {
        let Some(q) = self.find.clone() else { return };
        if q.is_empty() {
            return;
        }
        let y = self.with_doc(|d| d.find_text(&q)).flatten();
        self.find_missing = y.is_none();
        if let Some(y) = y {
            let h = Self::content(self.area).h;
            let t = self.tab();
            t.scroll = (y - h / 3).max(0);
            t.dirty = true;
        }
    }

    fn menu_action(&mut self, i: usize) {
        match i {
            0 => self.new_tab(),
            1 => {
                if self.tab().page.is_some() {
                    self.find = Some(String::new());
                    self.focus = Focus::Find;
                }
            }
            2 => self.reload(),
            3 => self.navigate(String::new()),
            _ => self.close_tab(self.cur),
        }
    }

    /// A key for the focused field of the page.
    fn page_key(&mut self, ev: &KeyEvent) -> Option<Action> {
        let k: String = match ev.key {
            Key::Char(ch) if !ev.ctrl && !ch.is_control() => ch.to_string(),
            Key::Backspace => String::from("\u{8}"),
            Key::Delete => String::from("\u{7f}"),
            Key::Enter => String::from("\n"),
            Key::Left => String::from("←"),
            Key::Right => String::from("→"),
            Key::Home => String::from("home"),
            Key::End => String::from("end"),
            Key::Escape | Key::Tab => {
                self.focus = Focus::None;
                let _ = self.with_doc(|d| d.focus = None);
                return Some(Action::Redraw);
            }
            Key::Char('v') if ev.ctrl => ui::paste(),
            _ => return None,
        };
        let hit = self.with_doc(|d| d.type_key(&k))?;
        self.caret_on = true;
        Some(self.handle_hit_if_some(hit))
    }

    fn handle_hit_if_some(&mut self, hit: Option<Hit>) -> Action {
        match hit {
            Some(h) => self.handle_hit(Some(h)),
            None => {
                self.tab().dirty = true;
                Action::Redraw
            }
        }
    }
}

/// The end of `s` that fits in `max` pixels (to keep the caret in view).
fn tail_fit(font: Font, s: &str, max: i32) -> String {
    if text::width(font, s) <= max {
        return String::from(s);
    }
    let chars: Vec<char> = s.chars().collect();
    let mut start = 0;
    while start < chars.len() && text::width(font, &chars[start..].iter().collect::<String>()) > max - 12 {
        start += 1;
    }
    format!("…{}", chars[start..].iter().collect::<String>())
}

impl App for Browser {
    fn title(&self) -> String {
        String::from("ticly browser")
    }

    fn icon(&self) -> &str {
        "ticly-browser"
    }

    fn kind(&self) -> &'static str {
        "browser"
    }

    fn size(&self) -> (i32, i32) {
        (1120, 720)
    }

    fn resizable(&self) -> bool {
        true
    }

    fn draw(&mut self, c: &mut Canvas, area: Rect, blur: &Canvas, focused: bool) {
        self.area = area;
        let saved = c.clip();
        c.set_clip(saved.intersect(&area));
        if self.tabs[self.cur].page.is_none() {
            self.draw_start(c, blur, area, focused);
        } else {
            self.draw_page(c, area, focused);
        }
        self.draw_chrome(c, area, focused);
        self.draw_overlays(c, blur, area, focused);
        c.set_clip(saved);
    }

    fn press(&mut self, x: i32, y: i32, _double: bool, area: Rect) -> Action {
        self.area = area;
        let (ax, ay) = (area.x + x, area.y + y);
        self.caret_on = true;
        if self.menu {
            self.menu = false;
            if let Some(i) = Self::menu_rows(area).iter().position(|r| r.contains(ax, ay)) {
                self.menu_action(i);
            }
            return Action::Redraw;
        }
        if let Some((n, opts, _, r)) = self.select.take() {
            if r.contains(ax, ay) {
                let i = ((ay - r.y - 6) / 34).max(0) as usize;
                if i < opts.len() {
                    let _ = self.with_doc(|d| d.choose(n, i));
                    self.tab().dirty = true;
                }
            }
            return Action::Redraw;
        }
        // Tabs.
        for (i, r) in self.tab_rects(area).into_iter().enumerate() {
            if Self::tab_close(r).contains(ax, ay) {
                self.close_tab(i);
                return Action::Redraw;
            }
            if r.contains(ax, ay) {
                self.switch(i);
                return Action::Redraw;
            }
        }
        if self.new_tab_rect(area).contains(ax, ay) || Self::counter_rect(area).contains(ax, ay) {
            self.new_tab();
            return Action::Redraw;
        }
        let [back, fwd, reload, home] = Self::tool_buttons(area);
        if back.contains(ax, ay) {
            self.back();
            return Action::Redraw;
        }
        if fwd.contains(ax, ay) {
            self.forward();
            return Action::Redraw;
        }
        if reload.contains(ax, ay) {
            if self.tab().loading {
                if let Some(p) = &self.tabs[self.cur].page {
                    let mut p = p.lock();
                    p.stop = true;
                    p.loading = false;
                }
                self.tab().loading = false;
            } else {
                self.reload();
            }
            return Action::Redraw;
        }
        if home.contains(ax, ay) {
            self.navigate(String::new());
            return Action::Redraw;
        }
        if Self::address_rect(area).contains(ax, ay) {
            if self.focus != Focus::Address {
                self.focus = Focus::Address;
                // Start over, as when the whole address is selected.
                self.address.clear();
            }
            return Action::Redraw;
        }
        if Self::menu_button(area).contains(ax, ay) {
            self.menu = true;
            return Action::Redraw;
        }
        if ay < area.y + BAR_H {
            return Action::None;
        }
        if self.focus == Focus::Address {
            self.focus = Focus::None;
            self.address = self.tab().url.clone();
        }
        if self.find.is_some() && Self::find_rect(area).contains(ax, ay) {
            self.focus = Focus::Find;
            return Action::Redraw;
        }
        // The start page.
        if self.tab().page.is_none() {
            let [go, login, ws, reg] = Self::start_buttons(area);
            if Self::start_search(area).contains(ax, ay) {
                self.focus = Focus::Search;
            } else if go.contains(ax, ay) {
                if !self.search.trim().is_empty() {
                    let u = resolve(&core::mem::take(&mut self.search));
                    self.navigate(u);
                }
            } else if login.contains(ax, ay) || reg.contains(ax, ay) {
                self.navigate(String::from(LOGIN));
            } else if ws.contains(ax, ay) {
                self.navigate(String::from(WORKSPACE));
            }
            return Action::Redraw;
        }
        let t = &self.tabs[self.cur];
        if t.view.is_none() && t.error.is_some() && Self::retry_rect(area).contains(ax, ay) {
            self.reload();
            return Action::Redraw;
        }
        let content = Self::content(area);
        let (px, py) = (ax - content.x, ay - content.y + t.scroll);
        self.hover = Some(Rect::new(ax, ay, 0, 0));
        let Some(hit) = self.with_doc(|d| d.click(px, py)) else { return Action::None };
        self.handle_hit(hit)
    }

    fn hover(&mut self, at: Option<(i32, i32)>, area: Rect) -> bool {
        self.area = area;
        let p = at.map(|(x, y)| (area.x + x, area.y + y));
        let h = p.and_then(|(x, y)| self.hover_targets(area).into_iter().find(|r| r.contains(x, y)));
        let mut changed = h != self.hover;
        self.hover = h;
        // The address of the link under the pointer.
        let content = Self::content(area);
        let link = match p {
            Some((x, y)) if content.contains(x, y) && self.tab().page.is_some() && !self.menu && self.select.is_none() => {
                let scroll = self.tab().scroll;
                let page = self.tab().page.clone();
                page.and_then(|pg| pg.try_lock().and_then(|p| p.doc.as_ref().and_then(|d| d.link_at(x - content.x, y - content.y + scroll))))
            }
            _ => None,
        };
        if link != self.hover_link {
            self.hover_link = link;
            changed = true;
        }
        changed
    }

    fn key(&mut self, ev: KeyEvent) -> Action {
        self.caret_on = true;
        // Shortcuts.
        if ev.ctrl {
            match ev.key {
                Key::Char('t') => {
                    self.new_tab();
                    return Action::Redraw;
                }
                Key::Char('w') => {
                    self.close_tab(self.cur);
                    return Action::Redraw;
                }
                Key::Char('l') => {
                    self.focus = Focus::Address;
                    self.address.clear();
                    return Action::Redraw;
                }
                Key::Char('r') => {
                    self.reload();
                    return Action::Redraw;
                }
                Key::Char('f') => {
                    self.menu_action(1);
                    return Action::Redraw;
                }
                Key::Tab => {
                    let n = self.tabs.len();
                    self.switch(if ev.shift { (self.cur + n - 1) % n } else { (self.cur + 1) % n });
                    return Action::Redraw;
                }
                _ => {}
            }
        }
        if ev.alt {
            match ev.key {
                Key::Left => {
                    self.back();
                    return Action::Redraw;
                }
                Key::Right => {
                    self.forward();
                    return Action::Redraw;
                }
                _ => {}
            }
        }
        if ev.key == Key::F(5) {
            self.reload();
            return Action::Redraw;
        }
        if ev.key == Key::Escape && (self.menu || self.select.is_some()) {
            self.menu = false;
            self.select = None;
            return Action::Redraw;
        }
        let start = self.tabs[self.cur].page.is_none();
        match self.focus {
            Focus::Address => match ev.key {
                Key::Enter => {
                    let input = self.address.trim().to_string();
                    if input.is_empty() {
                        return Action::None;
                    }
                    self.navigate(resolve(&input));
                    Action::Redraw
                }
                Key::Escape => {
                    self.focus = Focus::None;
                    self.address = self.tab().url.clone();
                    Action::Redraw
                }
                Key::Char('v') if ev.ctrl => {
                    self.address.push_str(ui::paste().trim());
                    Action::Redraw
                }
                _ => {
                    if ui::edit_text(&mut self.address, &ev, 2048) {
                        Action::Redraw
                    } else {
                        Action::None
                    }
                }
            },
            Focus::Search if start => match ev.key {
                Key::Enter => {
                    if self.search.trim().is_empty() {
                        return Action::None;
                    }
                    let u = resolve(&core::mem::take(&mut self.search));
                    self.navigate(u);
                    Action::Redraw
                }
                Key::Char('v') if ev.ctrl => {
                    self.search.push_str(ui::paste().trim());
                    Action::Redraw
                }
                _ => {
                    if ui::edit_text(&mut self.search, &ev, 300) {
                        Action::Redraw
                    } else {
                        Action::None
                    }
                }
            },
            Focus::Find => match ev.key {
                Key::Enter => {
                    self.find_now();
                    Action::Redraw
                }
                Key::Escape => {
                    self.find = None;
                    self.focus = Focus::None;
                    Action::Redraw
                }
                _ => {
                    let changed = self.find.as_mut().is_some_and(|q| ui::edit_text(q, &ev, 100));
                    if changed {
                        self.find_missing = false;
                        self.find_now();
                        Action::Redraw
                    } else {
                        Action::None
                    }
                }
            },
            Focus::Page => self.page_key(&ev).unwrap_or(Action::None),
            _ => {
                let h = Self::content(self.area).h;
                let moved = match ev.key {
                    Key::Down => self.scroll_by(48),
                    Key::Up => self.scroll_by(-48),
                    Key::PageDown | Key::Char(' ') => self.scroll_by(h - 60),
                    Key::PageUp => self.scroll_by(-(h - 60)),
                    Key::Home => self.scroll_by(-1 << 28),
                    Key::End => self.scroll_by(1 << 28),
                    Key::Backspace => {
                        self.back();
                        true
                    }
                    _ => false,
                };
                if moved {
                    Action::Redraw
                } else {
                    Action::None
                }
            }
        }
    }

    fn scroll(&mut self, lines: i32) -> bool {
        if self.select.is_some() || self.menu {
            return false;
        }
        self.hover_link = None;
        self.scroll_by(lines * 60)
    }

    fn tick(&mut self) -> bool {
        self.ticks += 1;
        let mut changed = false;
        // The caret blinks.
        if matches!(self.focus, Focus::Address | Focus::Search | Focus::Find | Focus::Page) && self.ticks % 5 == 0 {
            self.caret_on = !self.caret_on;
            if self.focus == Focus::Page {
                self.tab().dirty = true;
            }
            changed = true;
        }
        let throttle = self.ticks % 4 == 0;
        let t = &mut self.tabs[self.cur];
        let mut redirect = None;
        let mut hits = Vec::new();
        if let Some(page) = t.page.clone() {
            if let Some(mut p) = page.try_lock() {
                if p.version != t.painted.0 || p.loading != t.loading {
                    changed = true;
                }
                if p.loading {
                    changed = true;
                }
                let stale = p.doc.as_ref().is_some_and(|d| d.needs_layout(p.want_w.max(320)));
                if stale && !p.busy && (throttle || !p.loading) && p.want_w > 0 {
                    p.busy = true;
                    let pg = page.clone();
                    if crate::task::spawn_big("web-layout", STACK, move || relayout(pg)).is_none() {
                        p.busy = false;
                    }
                }
                if !p.loading {
                    redirect = p.redirect.take();
                }
                hits = core::mem::take(&mut p.hits);
            }
        }
        // What the page's scripts asked for (the last navigation wins).
        if let Some(h) = hits.into_iter().rfind(|h| matches!(h, Hit::Open(_) | Hit::Submit { .. })) {
            self.handle_hit(Some(h));
            return true;
        }
        if let Some(u) = redirect {
            // Replace the entry that redirected.
            let t = self.tab();
            if t.at < t.history.len() {
                t.history[t.at] = u.clone();
            }
            self.open(u, "GET", None);
            changed = true;
        }
        changed
    }

    fn close(&mut self) {
        for t in &self.tabs {
            if let Some(p) = &t.page {
                p.lock().stop = true;
            }
        }
    }
}

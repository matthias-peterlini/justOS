//! A page: its tree, stylesheets and pictures, its layout for a width, and
//! what happens when it is clicked or typed into.

use crate::css::{self, Stylesheet, Viewport};
use crate::dom::{Data, Dom, NodeId};
use crate::fonts::WebFonts;
use crate::layout::{self, Item, Layout, Pictures, Target};
use crate::style::{self, Styles};
use crate::{html, url};
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use arcgfx::{Canvas, Rect};

enum Sheet {
    Inline(Stylesheet),
    External { url: String, sheet: Option<Stylesheet>, asked: bool },
}

pub struct Picture {
    pub url: String,
    pub canvas: Option<Canvas>,
    pub failed: bool,
    pub asked: bool,
    /// Scaled copies for the sizes the layout shows the picture at.
    scaled: Vec<Canvas>,
}

/// What a click on the page does.
#[derive(Clone, Debug, PartialEq)]
pub enum Hit {
    /// Go to an address.
    Open(String),
    /// Send a form: (method, address, body for POST).
    Submit { method: String, url: String, body: Option<String> },
    /// A text field took the focus.
    Focus(NodeId),
    /// The page changed (a checkbox, a <details>): redraw.
    Changed,
    /// A <select>: (node, options, chosen index).
    Choose(NodeId, Vec<String>, usize),
    /// A script listens: the click waits for it (see [`Document::events`]).
    Script,
}

/// Something that happened on the page, for its scripts.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub node: NodeId,
    /// "click", "input", "change".
    pub kind: &'static str,
    /// The browser waits to do what the click does until the scripts had
    /// their say (they may cancel it).
    pub deferred: bool,
}

/// A script of the page, in document order.
#[derive(Clone, Debug, PartialEq)]
pub enum Script {
    Inline(String),
    External(String),
}

pub struct Document {
    pub url: String,
    pub base: String,
    pub dom: Dom,
    pub title: String,
    sheets: Vec<Sheet>,
    ua: Stylesheet,
    pub pictures: Vec<Picture>,
    by_url: BTreeMap<String, usize>,
    styles: Option<Styles>,
    pub layout: Option<Layout>,
    laid_width: i32,
    /// The layout is shown but out of date (a picture came in).
    stale: bool,
    pub focus: Option<NodeId>,
    /// Caret position (in characters) in the focused field.
    pub caret: usize,
    /// Scripts run on the page: clicks and typing become [`Event`]s.
    pub scripted: bool,
    /// Nodes scripts listen on for clicks ([`Dom::ROOT`] for the document
    /// and the window).
    pub listened: BTreeSet<NodeId>,
    /// Events waiting for the scripts.
    pub events: Vec<Event>,
}

impl Document {
    /// Reads a page. `content_type` decides how bytes are taken (HTML,
    /// plain text, a picture).
    pub fn new(address: &str, body: &[u8], content_type: &str) -> Document {
        let ct = content_type.to_ascii_lowercase();
        let charset = ct.split("charset=").nth(1).map(|c| c.trim_matches(['"', ' ', ';']).to_string()).or_else(|| html::sniff_charset(body));
        let dom = if ct.starts_with("image/") {
            html::parse(&alloc::format!("<body style='margin:0;background:#1b1b1d;display:flex;justify-content:center;align-items:center;min-height:100vh'><img src=\"{}\" style='max-width:100%'>", attr_escape(address)))
        } else if ct.starts_with("text/plain") || ct.contains("json") || ct.contains("javascript") || ct.starts_with("text/css") || ct.contains("xml") && !ct.contains("xhtml") && !looks_html(body) {
            let text = html::decode(body, charset.as_deref());
            html::parse(&alloc::format!("<pre style='white-space:pre-wrap;margin:16px;font-size:13px'>{}</pre>", text_escape(&text)))
        } else {
            html::parse(&html::decode(body, charset.as_deref()))
        };
        let mut d = Document {
            url: address.to_string(),
            base: address.to_string(),
            dom,
            title: String::new(),
            sheets: Vec::new(),
            ua: css::parse(style::UA_CSS),
            pictures: Vec::new(),
            by_url: BTreeMap::new(),
            styles: None,
            layout: None,
            laid_width: 0,
            stale: false,
            focus: None,
            caret: 0,
            scripted: false,
            listened: BTreeSet::new(),
            events: Vec::new(),
        };
        d.read_head();
        d
    }

    fn read_head(&mut self) {
        if let Some(b) = self.dom.find("base").and_then(|b| self.dom.attr(b, "href")) {
            self.base = url::join(&self.url, b);
        }
        if let Some(t) = self.dom.find("title") {
            self.title = self.dom.text(t).split_whitespace().collect::<Vec<_>>().join(" ");
        }
        self.read_sheets();
        // The pictures of <img> elements.
        for n in self.dom.descendants(Dom::ROOT) {
            if matches!(self.dom.tag(n), "img") || (self.dom.tag(n) == "input" && self.dom.attr(n, "type").is_some_and(|t| t.eq_ignore_ascii_case("image"))) {
                if let Some(src) = layout::image_src(&self.dom, n) {
                    let u = url::join(&self.base, &src);
                    self.picture_slot(&u);
                }
            }
        }
    }

    /// Reads the <style> and <link rel=stylesheet> elements into `sheets`.
    fn read_sheets(&mut self) {
        for n in self.dom.descendants(Dom::ROOT) {
            match self.dom.tag(n) {
                "style" => {
                    let media = self.dom.attr(n, "media").unwrap_or("all");
                    if css::media_ok(media, Viewport { width: 1024.0, height: 768.0 }) {
                        self.sheets.push(Sheet::Inline(css::parse(&self.dom.text(n))));
                    }
                }
                "link" => {
                    let rel = self.dom.attr(n, "rel").unwrap_or("").to_ascii_lowercase();
                    let media = self.dom.attr(n, "media").unwrap_or("all");
                    if rel.split_whitespace().any(|r| r == "stylesheet") && !rel.contains("alternate") && media_ok_static(media) {
                        if let Some(h) = self.dom.attr(n, "href") {
                            let u = url::join(&self.base, h);
                            self.sheets.push(Sheet::External { url: u, sheet: None, asked: false });
                        }
                    }
                }
                _ => {}
            }
        }
        // Imports of inline sheets.
        let mut extra = Vec::new();
        for (i, s) in self.sheets.iter().enumerate() {
            if let Sheet::Inline(sh) = s {
                for imp in &sh.imports {
                    extra.push((i, url::join(&self.base, imp)));
                }
            }
        }
        for (i, u) in extra.into_iter().rev() {
            self.sheets.insert(i, Sheet::External { url: u, sheet: None, asked: false });
        }
    }

    fn picture_slot(&mut self, u: &str) -> usize {
        if let Some(&i) = self.by_url.get(u) {
            return i;
        }
        let i = self.pictures.len();
        self.pictures.push(Picture { url: u.to_string(), canvas: None, failed: false, asked: false, scaled: Vec::new() });
        self.by_url.insert(u.to_string(), i);
        i
    }

    /// Where <meta http-equiv=refresh> sends the page, if at once.
    pub fn refresh(&self) -> Option<String> {
        for n in self.dom.descendants(Dom::ROOT) {
            if self.dom.tag(n) == "meta" && self.dom.attr(n, "http-equiv").is_some_and(|v| v.eq_ignore_ascii_case("refresh")) {
                let c = self.dom.attr(n, "content")?;
                let (delay, rest) = c.split_once([';', ',']).unwrap_or((c, ""));
                if delay.trim().parse::<f32>().unwrap_or(99.0) <= 5.0 {
                    let u = rest.trim().trim_start_matches(|c: char| c.is_ascii_alphabetic()).trim_start_matches('=').trim().trim_matches(['\'', '"']);
                    if !u.is_empty() {
                        return Some(url::join(&self.base, u));
                    }
                }
            }
        }
        None
    }

    /// Stylesheets still to fetch (each is asked for once).
    pub fn wanted_sheets(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        for s in &mut self.sheets {
            if let Sheet::External { url, sheet: None, asked } = s {
                if !*asked {
                    *asked = true;
                    out.push(url.clone());
                }
            }
        }
        out
    }

    /// Hands in a fetched stylesheet (or None if it failed).
    pub fn add_sheet(&mut self, address: &str, text: Option<&str>) {
        let mut imports = Vec::new();
        for (i, s) in self.sheets.iter_mut().enumerate() {
            if let Sheet::External { url: u, sheet, .. } = s {
                if u == address && sheet.is_none() {
                    let parsed = css::parse(text.unwrap_or(""));
                    for imp in &parsed.imports {
                        imports.push((i, url::join(address, imp)));
                    }
                    *sheet = Some(parsed);
                }
            }
        }
        for (i, u) in imports.into_iter().rev() {
            if !self.sheets.iter().any(|s| matches!(s, Sheet::External { url, .. } if *url == u)) {
                self.sheets.insert(i, Sheet::External { url: u, sheet: None, asked: false });
            }
        }
        self.styles = None;
        self.layout = None;
    }

    /// Pictures still to fetch.
    pub fn wanted_pictures(&mut self) -> Vec<String> {
        if let Some(l) = &self.layout {
            for u in l.wanted.clone() {
                self.picture_slot(&u);
            }
        }
        let mut out = Vec::new();
        for p in &mut self.pictures {
            if !p.asked {
                p.asked = true;
                out.push(p.url.clone());
            }
        }
        out
    }

    /// Hands in a fetched picture.
    pub fn add_picture(&mut self, address: &str, canvas: Option<Canvas>) {
        if let Some(&i) = self.by_url.get(address) {
            let p = &mut self.pictures[i];
            p.failed = canvas.is_none();
            p.canvas = canvas;
            self.stale = true;
        }
    }

    pub fn needs_layout(&self, width: i32) -> bool {
        self.layout.is_none() || self.stale || self.laid_width != width
    }

    /// Lays the page out for a width (styles are computed once per set of
    /// stylesheets).
    pub fn lay_out(&mut self, fonts: &WebFonts, width: i32, height: i32) {
        style::VW.set(width as f32);
        style::VH.set(height as f32);
        let vp = Viewport { width: width as f32, height: height as f32 };
        if self.styles.is_none() || self.laid_width != width {
            let mut list: Vec<(bool, &Stylesheet)> = alloc::vec![(true, &self.ua)];
            for s in &self.sheets {
                match s {
                    Sheet::Inline(sh) => list.push((false, sh)),
                    Sheet::External { sheet: Some(sh), .. } => list.push((false, sh)),
                    _ => {}
                }
            }
            self.styles = Some(style::compute(&self.dom, &list, vp));
        }
        let pics = Pics { doc: self };
        let l = layout::layout(&self.dom, self.styles.as_ref().expect("styles were computed"), fonts, &pics, width as f32);
        self.layout = Some(l);
        self.laid_width = width;
        self.stale = false;
        self.prescale();
    }

    /// Reads the stylesheets again after a script added or removed <style>
    /// or <link> elements. Sheets already fetched are kept; new ones are
    /// asked for by [`Document::wanted_sheets`].
    pub fn reread_styles(&mut self) {
        let mut fetched = BTreeMap::new();
        for s in core::mem::take(&mut self.sheets) {
            if let Sheet::External { url, sheet, asked } = s {
                if sheet.is_some() || asked {
                    fetched.insert(url, (sheet, asked));
                }
            }
        }
        self.read_sheets();
        for s in &mut self.sheets {
            if let Sheet::External { url, sheet, asked } = s {
                if let Some((old, was)) = fetched.remove(url.as_str()) {
                    *sheet = old;
                    *asked = was;
                }
            }
        }
        self.styles = None;
        self.stale = true;
    }

    /// Restyles after the page changed (a <details> opened).
    pub fn invalidate(&mut self) {
        self.styles = None;
        self.stale = true;
    }

    pub fn height(&self) -> i32 {
        self.layout.as_ref().map_or(0, |l| l.height)
    }

    pub fn background(&self) -> u32 {
        self.layout.as_ref().map_or(0xFFFF_FFFF, |l| l.background)
    }

    /// Copies of the pictures at the sizes shown (scaling once, not on
    /// every frame).
    fn prescale(&mut self) {
        let Some(l) = &self.layout else { return };
        let mut want: Vec<(usize, i32, i32)> = Vec::new();
        for it in &l.items {
            if let Item::Image { r, img, cover, .. } = it {
                if !cover && r.w > 0 && r.h > 0 {
                    want.push((*img, r.w, r.h));
                }
            }
        }
        for (i, w, h) in want {
            let Some(p) = self.pictures.get_mut(i) else { continue };
            let Some(c) = &p.canvas else { continue };
            if (c.w == w && c.h == h) || p.scaled.iter().any(|s| s.w == w && s.h == h) {
                continue;
            }
            if p.scaled.len() > 3 {
                p.scaled.remove(0);
            }
            p.scaled.push(c.scaled(w, h));
        }
    }

    /// Where an anchor (#id) is.
    pub fn anchor(&self, frag: &str) -> Option<i32> {
        let frag = url::percent_decode(frag);
        self.layout.as_ref()?.anchors.iter().find(|(k, _)| *k == frag).map(|(_, y)| *y)
    }

    /// Paints the page with its point (0, `scroll`) at the top-left of
    /// `area` on the canvas.
    pub fn paint(&self, c: &mut Canvas, fonts: &WebFonts, area: Rect, scroll: i32, caret_on: bool) {
        let Some(l) = &self.layout else { return };
        let saved = c.clip();
        let base = saved.intersect(&area);
        c.set_clip(base);
        c.fill(area, l.background);
        let dx = area.x;
        let dy = area.y - scroll;
        let view = Rect::new(0, scroll, area.w, area.h);
        let mut clips: Vec<Rect> = alloc::vec![base];
        for it in &l.items {
            match it {
                Item::ClipPush(r) => {
                    let cur = *clips.last().unwrap_or(&base);
                    let nr = cur.intersect(&r.offset(dx, dy));
                    clips.push(nr);
                    c.set_clip(nr);
                    continue;
                }
                Item::ClipPop => {
                    if clips.len() > 1 {
                        clips.pop();
                    }
                    c.set_clip(*clips.last().unwrap_or(&base));
                    continue;
                }
                _ => {}
            }
            if let Some(b) = it.bounds() {
                if !b.overlaps(&view) {
                    continue;
                }
            }
            match it {
                Item::Fill { r, color, radius } => {
                    let rr = r.offset(dx, dy);
                    if *radius > 0 {
                        c.fill_round(rr, (*radius).min(rr.w / 2).min(rr.h / 2), *color);
                    } else {
                        c.fill(rr, *color);
                    }
                }
                Item::Border { r, w, c: col, radius } => {
                    let rr = r.offset(dx, dy);
                    if *radius > 0 && w.iter().all(|&x| x == w[0]) && col.iter().all(|&x| x == col[0]) {
                        c.stroke_round(rr, (*radius).min(rr.w / 2).min(rr.h / 2), w[0], col[0]);
                    } else {
                        if w[0] > 0 {
                            c.fill(Rect::new(rr.x, rr.y, rr.w, w[0]), col[0]);
                        }
                        if w[2] > 0 {
                            c.fill(Rect::new(rr.x, rr.bottom() - w[2], rr.w, w[2]), col[2]);
                        }
                        if w[3] > 0 {
                            c.fill(Rect::new(rr.x, rr.y, w[3], rr.h), col[3]);
                        }
                        if w[1] > 0 {
                            c.fill(Rect::new(rr.right() - w[1], rr.y, w[1], rr.h), col[1]);
                        }
                    }
                }
                Item::Text { x, y, face, color, text, deco, width } => {
                    let (lh, asc) = fonts.metrics(*face);
                    fonts.draw(c, *face, x + dx, y + dy, text, *color);
                    if deco & 1 != 0 {
                        let uy = y + dy + asc + (lh - asc) / 3;
                        c.fill(Rect::new(x + dx, uy, *width, 1), *color);
                    }
                    if deco & 2 != 0 {
                        c.fill(Rect::new(x + dx, y + dy + asc * 2 / 3, *width, 1), *color);
                    }
                }
                Item::Image { r, img, cover, radius } => {
                    let rr = r.offset(dx, dy);
                    if let Some(p) = self.pictures.get(*img) {
                        if let Some(src) = &p.canvas {
                            if *cover {
                                draw_cover(c, src, rr);
                            } else if let Some(s) = p.scaled.iter().find(|s| s.w == rr.w && s.h == rr.h) {
                                if *radius > 0 {
                                    c.blit_round_from(s, 0, 0, rr, (*radius).min(rr.w / 2), 0);
                                } else {
                                    c.draw_image(s, rr.x, rr.y, 255);
                                }
                            } else if src.w == rr.w && src.h == rr.h {
                                c.draw_image(src, rr.x, rr.y, 255);
                            } else {
                                c.draw_scaled(src, rr, 255);
                            }
                        }
                    }
                }
                Item::Field { r, node, face, color, multiline } => {
                    let rr = r.offset(dx, dy);
                    let value = self.value(*node);
                    let password = self.dom.attr(*node, "type").is_some_and(|t| t.eq_ignore_ascii_case("password"));
                    let shown: String = if password { "•".repeat(value.chars().count()) } else { value.clone() };
                    let (lh, _) = fonts.metrics(*face);
                    let focused = self.focus == Some(*node);
                    let cl = c.clip();
                    c.set_clip(cl.intersect(&rr.grow(1)));
                    if shown.is_empty() {
                        if let Some(ph) = self.dom.attr(*node, "placeholder") {
                            fonts.draw(c, *face, rr.x, rr.y + if *multiline { 0 } else { (rr.h - lh) / 2 }, ph, 0xFF8A_8A90);
                        }
                    } else if *multiline {
                        for (i, line) in shown.split('\n').enumerate() {
                            fonts.draw(c, *face, rr.x, rr.y + i as i32 * lh, line, *color);
                        }
                    } else {
                        // Keep the caret in view.
                        let before: String = shown.chars().take(self.caret).collect();
                        let cw = fonts.width(*face, &before);
                        let off = if focused && cw > rr.w - 4 { cw - rr.w + 4 } else { 0 };
                        fonts.draw(c, *face, rr.x - off, rr.y + (rr.h - lh) / 2, &shown, *color);
                    }
                    if focused && caret_on {
                        let (line_no, col) = if *multiline {
                            let before: String = shown.chars().take(self.caret).collect();
                            let ln = before.matches('\n').count() as i32;
                            (ln, fonts.width(*face, before.rsplit('\n').next().unwrap_or("")))
                        } else {
                            let before: String = shown.chars().take(self.caret).collect();
                            (0, fonts.width(*face, &before).min(rr.w - 4))
                        };
                        let cy = if *multiline { rr.y + line_no * lh } else { rr.y + (rr.h - lh) / 2 };
                        c.fill(Rect::new(rr.x + col, cy, 1, lh), 0xFF1A_56DB);
                    }
                    c.set_clip(cl);
                }
                Item::Check { r, node, radio } => {
                    let rr = r.offset(dx, dy);
                    let on = self.dom.attr(*node, "checked").is_some();
                    let rad = if *radio { rr.w / 2 } else { 3 };
                    if on {
                        c.fill_round(rr, rad, 0xFF1A_56DB);
                        if *radio {
                            c.fill_circle(rr.x + rr.w / 2, rr.y + rr.h / 2, rr.w / 4, 0xFFFF_FFFF);
                        } else {
                            c.line(rr.x + 3, rr.y + rr.h / 2, rr.x + rr.w / 2 - 1, rr.bottom() - 4, 2, 0xFFFF_FFFF);
                            c.line(rr.x + rr.w / 2 - 1, rr.bottom() - 4, rr.right() - 3, rr.y + 3, 2, 0xFFFF_FFFF);
                        }
                    } else {
                        c.fill_round(rr, rad, 0xFFFF_FFFF);
                        c.stroke_round(rr, rad, 1, 0xFF76_7680);
                    }
                }
                Item::Select { r, node, face, color } => {
                    let rr = r.offset(dx, dy);
                    let (opts, sel) = self.options(*node);
                    let (lh, _) = fonts.metrics(*face);
                    let cl = c.clip();
                    c.set_clip(cl.intersect(&rr));
                    if let Some(t) = opts.get(sel) {
                        fonts.draw(c, *face, rr.x, rr.y + (rr.h - lh) / 2, t, *color);
                    }
                    c.set_clip(cl);
                    let ax = rr.right() - 10;
                    let ay = rr.y + rr.h / 2;
                    c.triangle([(ax - 4, ay - 2), (ax + 4, ay - 2), (ax, ay + 3)], *color);
                }
                Item::ClipPush(_) | Item::ClipPop => {}
            }
        }
        c.set_clip(saved);
    }

    /// The target at a page point.
    pub fn target_at(&self, x: i32, y: i32) -> Option<Target> {
        let l = self.layout.as_ref()?;
        let mut hits = l.targets.iter().rev().filter(|(r, _)| r.contains(x, y)).map(|(_, t)| *t);
        let first = hits.next()?;
        if !matches!(first, Target::Block(_)) {
            return Some(first);
        }
        // A link or a control around a box still takes the click.
        Some(hits.find(|t| !matches!(t, Target::Block(_))).unwrap_or(first))
    }

    /// The scripts of the page, in order. Modules and data blocks are left
    /// out.
    pub fn scripts(&self) -> Vec<Script> {
        let mut out = Vec::new();
        for n in self.dom.descendants(Dom::ROOT) {
            if self.dom.tag(n) != "script" {
                continue;
            }
            let ty = self.dom.attr(n, "type").unwrap_or("").trim().to_ascii_lowercase();
            if !matches!(ty.as_str(), "" | "text/javascript" | "application/javascript" | "text/ecmascript" | "application/x-javascript") {
                continue;
            }
            match self.dom.attr(n, "src") {
                Some(src) if !src.trim().is_empty() => out.push(Script::External(url::join(&self.base, src.trim()))),
                Some(_) => {}
                None => out.push(Script::Inline(self.dom.text(n))),
            }
        }
        out
    }

    /// Whether a script listens for clicks on a node or around it.
    fn listens(&self, n: NodeId) -> bool {
        let mut cur = Some(n);
        while let Some(c) = cur {
            if self.listened.contains(&c) || self.dom.attr(c, "onclick").is_some() {
                return true;
            }
            cur = self.dom.nodes[c].parent;
        }
        false
    }

    /// What a click on a node does by itself (scripts aside).
    pub fn default_click(&mut self, n: NodeId) -> Option<Hit> {
        if matches!(self.dom.tag(n), "a" | "area" | "input" | "button" | "select" | "textarea" | "summary" | "label") {
            self.activate(n)
        } else {
            // A box inside a link.
            let link = self.dom.closest(n, "a")?;
            self.activate(link)
        }
    }

    /// The address of the link at a page point (for the status line).
    pub fn link_at(&self, x: i32, y: i32) -> Option<String> {
        match self.target_at(x, y)? {
            Target::Link(n) if matches!(self.dom.tag(n), "a" | "area") => self.dom.attr(n, "href").map(|h| url::join(&self.base, h)),
            _ => None,
        }
    }

    /// A click at a page point.
    pub fn click(&mut self, x: i32, y: i32) -> Option<Hit> {
        let t = self.target_at(x, y);
        if self.scripted {
            if let Some(n) = t.map(Target::node).filter(|&n| self.listens(n)) {
                // Following a link or sending a form waits for the scripts;
                // fields and toggles answer at once.
                let defer = match t {
                    Some(Target::Link(_) | Target::Block(_)) => true,
                    Some(Target::Control(c)) => matches!(self.dom.tag(c), "button" | "input") && matches!(self.dom.attr(c, "type").unwrap_or(if self.dom.tag(c) == "button" { "submit" } else { "text" }).to_ascii_lowercase().as_str(), "submit" | "image"),
                    _ => false,
                };
                if defer {
                    self.focus = None;
                    self.events.push(Event { node: n, kind: "click", deferred: true });
                    return Some(Hit::Script);
                }
                let hit = self.default_click(n);
                self.events.push(Event { node: n, kind: "click", deferred: false });
                if hit == Some(Hit::Changed) {
                    self.events.push(Event { node: n, kind: "change", deferred: false });
                }
                if !matches!(hit, Some(Hit::Focus(_))) {
                    self.focus = None;
                }
                return hit.or(Some(Hit::Script));
            }
        }
        let hit = match t {
            Some(Target::Link(n)) | Some(Target::Control(n)) => self.activate(n),
            Some(Target::Summary(n)) | Some(Target::Label(n)) => self.activate(n),
            Some(Target::Block(n)) => self.dom.closest(n, "a").and_then(|a| self.activate(a)),
            None => None,
        };
        if !matches!(hit, Some(Hit::Focus(_))) && hit != Some(Hit::Changed) {
            self.focus = None;
        }
        hit
    }

    /// The form a control sends.
    pub fn form_of(&self, from: NodeId) -> Option<NodeId> {
        self.dom.attr(from, "form").and_then(|id| self.dom.descendants(Dom::ROOT).into_iter().find(|&e| self.dom.attr(e, "id") == Some(id))).or_else(|| self.dom.closest(from, "form"))
    }

    fn activate(&mut self, n: NodeId) -> Option<Hit> {
        let tag = self.dom.tag(n).to_string();
        match tag.as_str() {
            "a" | "area" => {
                let href = self.dom.attr(n, "href")?;
                if href.trim_start().to_ascii_lowercase().starts_with("javascript:") {
                    return None;
                }
                Some(Hit::Open(url::join(&self.base, href)))
            }
            "summary" => {
                let d = self.dom.parent_element(n).filter(|&p| self.dom.tag(p) == "details")?;
                if self.dom.attr(d, "open").is_some() {
                    self.dom.remove_attr(d, "open");
                } else {
                    self.dom.set_attr(d, "open", "");
                }
                self.invalidate();
                Some(Hit::Changed)
            }
            "label" => {
                let target = match self.dom.attr(n, "for") {
                    Some(id) => self.dom.descendants(Dom::ROOT).into_iter().find(|&e| self.dom.attr(e, "id") == Some(id)),
                    None => self.dom.descendants(n).into_iter().find(|&e| matches!(self.dom.tag(e), "input" | "select" | "textarea")),
                }?;
                self.activate(target)
            }
            "input" => {
                let t = self.dom.attr(n, "type").unwrap_or("text").to_ascii_lowercase();
                match t.as_str() {
                    "checkbox" => {
                        if self.dom.attr(n, "checked").is_some() {
                            self.dom.remove_attr(n, "checked");
                        } else {
                            self.dom.set_attr(n, "checked", "");
                        }
                        Some(Hit::Changed)
                    }
                    "radio" => {
                        let name = self.dom.attr(n, "name").unwrap_or("").to_string();
                        for e in self.dom.descendants(Dom::ROOT) {
                            if self.dom.tag(e) == "input" && self.dom.attr(e, "type").is_some_and(|t| t.eq_ignore_ascii_case("radio")) && self.dom.attr(e, "name").unwrap_or("") == name {
                                self.dom.remove_attr(e, "checked");
                            }
                        }
                        self.dom.set_attr(n, "checked", "");
                        Some(Hit::Changed)
                    }
                    "submit" | "image" => self.submit(n),
                    "reset" | "button" | "range" | "color" | "file" | "hidden" => None,
                    _ => {
                        self.focus = Some(n);
                        self.caret = self.value(n).chars().count();
                        Some(Hit::Focus(n))
                    }
                }
            }
            "textarea" => {
                self.focus = Some(n);
                self.caret = self.value(n).chars().count();
                Some(Hit::Focus(n))
            }
            "select" => {
                let (opts, sel) = self.options(n);
                Some(Hit::Choose(n, opts, sel))
            }
            "button" => {
                let t = self.dom.attr(n, "type").unwrap_or("submit").to_ascii_lowercase();
                if t == "submit" && self.dom.closest(n, "form").is_some() {
                    self.submit(n)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// The value of a field.
    pub fn value(&self, n: NodeId) -> String {
        if self.dom.tag(n) == "textarea" {
            return match self.dom.attr(n, "data-arc-value") {
                Some(v) => v.to_string(),
                None => self.dom.text(n).trim_start_matches('\n').to_string(),
            };
        }
        self.dom.attr(n, "value").unwrap_or("").to_string()
    }

    pub fn set_value(&mut self, n: NodeId, v: &str) {
        if self.dom.tag(n) == "textarea" {
            self.dom.set_attr(n, "data-arc-value", v);
        } else {
            self.dom.set_attr(n, "value", v);
        }
    }

    /// The texts of a <select>'s options and the chosen one.
    pub fn options(&self, n: NodeId) -> (Vec<String>, usize) {
        let opts: Vec<NodeId> = self.dom.descendants(n).into_iter().filter(|&o| self.dom.tag(o) == "option").collect();
        let sel = opts.iter().position(|&o| self.dom.attr(o, "selected").is_some()).unwrap_or(0);
        (opts.iter().map(|&o| self.dom.text(o).split_whitespace().collect::<Vec<_>>().join(" ")).collect(), sel)
    }

    pub fn choose(&mut self, n: NodeId, index: usize) {
        let opts: Vec<NodeId> = self.dom.descendants(n).into_iter().filter(|&o| self.dom.tag(o) == "option").collect();
        for (i, &o) in opts.iter().enumerate() {
            if i == index {
                self.dom.set_attr(o, "selected", "");
            } else {
                self.dom.remove_attr(o, "selected");
            }
        }
        if self.scripted {
            self.events.push(Event { node: n, kind: "change", deferred: false });
        }
    }

    /// A key typed into the focused field: text, or one of "\u{8}"
    /// (backspace), "\u{7f}" (delete), "\n" (enter), "←", "→", "home",
    /// "end". Returns a submission when Enter sends the form.
    pub fn type_key(&mut self, key: &str) -> Option<Hit> {
        let n = self.focus?;
        let mut chars: Vec<char> = self.value(n).chars().collect();
        let multiline = self.dom.tag(n) == "textarea";
        self.caret = self.caret.min(chars.len());
        match key {
            "\u{8}" => {
                if self.caret > 0 {
                    chars.remove(self.caret - 1);
                    self.caret -= 1;
                }
            }
            "\u{7f}" => {
                if self.caret < chars.len() {
                    chars.remove(self.caret);
                }
            }
            "←" => self.caret = self.caret.saturating_sub(1),
            "→" => self.caret = (self.caret + 1).min(chars.len()),
            "home" => self.caret = 0,
            "end" => self.caret = chars.len(),
            "\n" if !multiline && self.scripted => {
                self.events.push(Event { node: n, kind: "submit", deferred: true });
                return Some(Hit::Script);
            }
            "\n" if !multiline => return self.submit(n),
            text => {
                let max: usize = self.dom.attr(n, "maxlength").and_then(|v| v.parse().ok()).unwrap_or(usize::MAX);
                for ch in text.chars() {
                    if chars.len() >= max {
                        break;
                    }
                    if ch == '\n' && !multiline {
                        continue;
                    }
                    chars.insert(self.caret, ch);
                    self.caret += 1;
                }
            }
        }
        let v: String = chars.into_iter().collect();
        if self.scripted && v != self.value(n) {
            self.events.push(Event { node: n, kind: "input", deferred: false });
        }
        self.set_value(n, &v);
        None
    }

    /// Sends the form an element is in.
    pub fn submit(&self, from: NodeId) -> Option<Hit> {
        let form = self.form_of(from)?;
        let mut pairs: Vec<(String, String)> = Vec::new();
        for e in self.dom.descendants(form) {
            let Some(el) = self.dom.element(e) else { continue };
            let Some(name) = el.attr("name").filter(|n| !n.is_empty()) else { continue };
            if el.attr("disabled").is_some() {
                continue;
            }
            match el.tag.as_str() {
                "input" => {
                    let t = el.attr("type").unwrap_or("text").to_ascii_lowercase();
                    match t.as_str() {
                        "checkbox" | "radio" => {
                            if el.attr("checked").is_some() {
                                pairs.push((name.into(), el.attr("value").unwrap_or("on").into()));
                            }
                        }
                        "submit" | "image" | "button" | "reset" => {
                            if e == from {
                                pairs.push((name.into(), el.attr("value").unwrap_or("").into()));
                            }
                        }
                        "file" => {}
                        _ => pairs.push((name.into(), el.attr("value").unwrap_or("").into())),
                    }
                }
                "textarea" => pairs.push((name.into(), self.value(e))),
                "select" => {
                    let opts: Vec<NodeId> = self.dom.descendants(e).into_iter().filter(|&o| self.dom.tag(o) == "option").collect();
                    let sel = opts.iter().copied().find(|&o| self.dom.attr(o, "selected").is_some()).or(opts.first().copied());
                    if let Some(o) = sel {
                        let v = self.dom.attr(o, "value").map(String::from).unwrap_or_else(|| self.dom.text(o).trim().to_string());
                        pairs.push((name.into(), v));
                    }
                }
                "button" => {
                    if e == from {
                        pairs.push((name.into(), el.attr("value").unwrap_or("").into()));
                    }
                }
                _ => {}
            }
        }
        let query = pairs.iter().map(|(k, v)| alloc::format!("{}={}", url::form_encode(k), url::form_encode(v))).collect::<Vec<_>>().join("&");
        let action = self.dom.attr(from, "formaction").or_else(|| self.dom.attr(form, "action")).unwrap_or("");
        let target = url::join(&self.base, action);
        let method = self.dom.attr(from, "formmethod").or_else(|| self.dom.attr(form, "method")).unwrap_or("get").to_ascii_uppercase();
        if method == "POST" {
            Some(Hit::Submit { method, url: target, body: Some(query) })
        } else {
            let base = target.split(['?', '#']).next().unwrap_or(&target).to_string();
            Some(Hit::Submit { method: "GET".into(), url: alloc::format!("{base}?{query}"), body: None })
        }
    }

    /// The page's text, for finding words in it.
    pub fn find_text(&self, needle: &str) -> Option<i32> {
        let l = self.layout.as_ref()?;
        let needle = needle.to_lowercase();
        l.items.iter().find_map(|it| match it {
            Item::Text { y, text, .. } if text.to_lowercase().contains(&needle) => Some(*y),
            _ => None,
        })
    }

    /// The URL of the page's icon.
    pub fn icon(&self) -> String {
        for n in self.dom.descendants(Dom::ROOT) {
            if self.dom.tag(n) == "link" && self.dom.attr(n, "rel").is_some_and(|r| r.to_ascii_lowercase().split_whitespace().any(|x| x == "icon")) {
                if let Some(h) = self.dom.attr(n, "href") {
                    return url::join(&self.base, h);
                }
            }
        }
        url::join(&self.base, "/favicon.ico")
    }

    /// Text of the whole page (for reader views and tests).
    pub fn text(&self) -> String {
        let body = self.dom.find("body").unwrap_or(Dom::ROOT);
        let mut out = String::new();
        for n in self.dom.descendants(body) {
            for &c in &self.dom.nodes[n].children {
                if let Data::Text(t) = &self.dom.nodes[c].data {
                    if !matches!(self.dom.tag(n), "script" | "style") {
                        out.push_str(t);
                    }
                }
            }
        }
        out
    }
}

struct Pics<'a> {
    doc: &'a Document,
}

impl Pictures for Pics<'_> {
    fn find(&self, u: &str) -> Option<(usize, Option<(i32, i32)>)> {
        let &i = self.doc.by_url.get(u)?;
        let p = &self.doc.pictures[i];
        Some((i, p.canvas.as_ref().map(|c| (c.w, c.h))))
    }

    fn resolve(&self, link: &str) -> String {
        url::join(&self.doc.base, link)
    }
}

fn media_ok_static(m: &str) -> bool {
    css::media_ok(m, Viewport { width: 1024.0, height: 768.0 }) || m.contains("min-width") || m.contains("max-width")
}

fn looks_html(b: &[u8]) -> bool {
    let head = String::from_utf8_lossy(&b[..b.len().min(512)]).to_ascii_lowercase();
    head.contains("<html") || head.contains("<!doctype html")
}

fn text_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn attr_escape(s: &str) -> String {
    text_escape(s).replace('"', "&quot;")
}

/// A picture drawn to cover a box, cropped in the middle.
fn draw_cover(c: &mut Canvas, src: &Canvas, r: Rect) {
    if src.w <= 0 || src.h <= 0 || r.w <= 0 || r.h <= 0 {
        return;
    }
    // Scale so the picture covers the box, then crop its middle.
    let (sw, sh) = (src.w as i64, src.h as i64);
    let (bw, bh) = (r.w as i64, r.h as i64);
    let (cw, ch) = if sw * bh > sh * bw { (sh * bw / bh, sh) } else { (sw, sw * bh / bw) };
    let crop = src.crop(Rect::new(((sw - cw) / 2) as i32, ((sh - ch) / 2) as i32, cw.max(1) as i32, ch.max(1) as i32));
    c.draw_scaled(&crop, r, 255);
}

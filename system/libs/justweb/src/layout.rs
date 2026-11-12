//! Layout: styled elements into positioned boxes and lines of text, and
//! from those a list of things to paint.
//!
//! What it does: blocks with margins, borders and padding, centring with
//! auto margins, min/max sizes; inline formatting with line breaking,
//! alignment, inline backgrounds and atomic inline boxes; floats with text
//! flowing around them; flexbox (rows, columns, wrapping, grow and shrink,
//! justify and align); grids with fixed, fractional and auto-fill tracks;
//! tables with automatic column widths; lists; images; form controls.
//! What it leaves out: absolutely positioned boxes (overlays, banners and
//! menus that need scripts are mostly that), transforms and vertical text.

use crate::Round;
use crate::dom::{Data, Dom, NodeId};
use crate::fonts::{Face, WebFonts};
use crate::style::{Align, Display, Float, Items, Justify, Len, ListStyle, Position, Style, Styles, Track, Transform, VAlign, WhiteSpace};
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use justgfx::Rect;

/// Something to paint, in page coordinates.
#[derive(Clone, Debug)]
pub enum Item {
    Fill { r: Rect, color: u32, radius: i32 },
    Border { r: Rect, w: [i32; 4], c: [u32; 4], radius: i32 },
    Text { x: i32, y: i32, face: Face, color: u32, text: String, deco: u8, width: i32 },
    Image { r: Rect, img: usize, cover: bool, radius: i32 },
    /// A text field: its value is read when painting.
    Field { r: Rect, node: NodeId, face: Face, color: u32, multiline: bool },
    Check { r: Rect, node: NodeId, radio: bool },
    Select { r: Rect, node: NodeId, face: Face, color: u32 },
    ClipPush(Rect),
    ClipPop,
}

impl Item {
    fn shift(&mut self, dx: i32, dy: i32) {
        match self {
            Item::Fill { r, .. } | Item::Border { r, .. } | Item::Image { r, .. } | Item::Field { r, .. } | Item::Check { r, .. } | Item::Select { r, .. } | Item::ClipPush(r) => {
                *r = r.offset(dx, dy)
            }
            Item::Text { x, y, .. } => {
                *x += dx;
                *y += dy;
            }
            Item::ClipPop => {}
        }
    }

    /// The area the item covers (for skipping what is off screen).
    pub fn bounds(&self) -> Option<Rect> {
        match self {
            Item::Fill { r, .. } | Item::Border { r, .. } | Item::Image { r, .. } | Item::Field { r, .. } | Item::Check { r, .. } | Item::Select { r, .. } => Some(*r),
            Item::Text { x, y, width, .. } => Some(Rect::new(*x, *y - 4, *width + 2, 80)),
            _ => None,
        }
    }
}

/// What a click on a spot of the page means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Link(NodeId),
    Control(NodeId),
    Summary(NodeId),
    Label(NodeId),
    /// Any other block box: for scripts listening for clicks.
    Block(NodeId),
}

impl Target {
    pub fn node(self) -> NodeId {
        match self {
            Target::Link(n) | Target::Control(n) | Target::Summary(n) | Target::Label(n) | Target::Block(n) => n,
        }
    }
}

/// The pictures of a page: index by address and natural size when loaded.
pub trait Pictures {
    fn find(&self, url: &str) -> Option<(usize, Option<(i32, i32)>)>;
    fn resolve(&self, link: &str) -> String;
}

pub struct Layout {
    pub items: Vec<Item>,
    pub targets: Vec<(Rect, Target)>,
    pub anchors: Vec<(String, i32)>,
    pub height: i32,
    pub width: i32,
    pub background: u32,
    /// Background pictures the page uses that are not loaded yet.
    pub wanted: Vec<String>,
}

#[derive(Clone, Copy)]
struct FRect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

#[derive(Default)]
struct Floats {
    list: Vec<(bool, FRect)>,
}

impl Floats {
    /// The free span between `left` and `right` for a line at y..y+h.
    fn edges(&self, y: f32, h: f32, left: f32, right: f32) -> (f32, f32) {
        let (mut l, mut r) = (left, right);
        for (is_left, f) in &self.list {
            if f.y < y + h.max(1.0) && f.y + f.h > y {
                if *is_left {
                    l = l.max(f.x + f.w);
                } else {
                    r = r.min(f.x);
                }
            }
        }
        (l, r.max(l))
    }

    fn clear(&self, y: f32, left: bool, right: bool) -> f32 {
        self.list.iter().filter(|(l, _)| (*l && left) || (!*l && right)).map(|(_, f)| f.y + f.h).fold(y, f32::max)
    }

    fn bottom(&self) -> f32 {
        self.list.iter().map(|(_, f)| f.y + f.h).fold(0.0, f32::max)
    }
}

#[derive(Clone, Copy, Default)]
struct Edges {
    m: [f32; 4],
    b: [f32; 4],
    p: [f32; 4],
}

impl Edges {
    fn h(&self) -> f32 {
        self.b[1] + self.b[3] + self.p[1] + self.p[3]
    }
    fn v(&self) -> f32 {
        self.b[0] + self.b[2] + self.p[0] + self.p[2]
    }
    fn mh(&self) -> f32 {
        self.m[1] + self.m[3]
    }
}

/// A piece of inline content waiting to be put on a line.
enum Inl {
    Word {
        text: String,
        w: f32,
        face: Face,
        color: u32,
        deco: u8,
        owner: Option<NodeId>,
        link: Option<NodeId>,
        space: f32,
        lh: f32,
        asc: f32,
        desc: f32,
        nowrap: bool,
        visible: bool,
    },
    Atomic {
        items: Vec<Item>,
        targets: Vec<(Rect, Target)>,
        w: f32,
        h: f32,
        valign: VAlign,
        link: Option<NodeId>,
        space: f32,
    },
    Break {
        lh: f32,
    },
    /// The start or end of an inline element with padding, borders or
    /// margins: room on the line.
    Edge {
        w: f32,
        owner: NodeId,
        open: bool,
    },
}

struct Marker {
    x: f32,
    text: String,
    face: Face,
    color: u32,
    bullet: Option<(ListStyle, f32)>,
}

pub struct Ctx<'a> {
    dom: &'a Dom,
    st: &'a Styles,
    fonts: &'a WebFonts,
    pics: &'a dyn Pictures,
    items: Vec<Item>,
    targets: Vec<(Rect, Target)>,
    anchors: Vec<(String, i32)>,
    wanted: Vec<String>,
    memo: BTreeMap<NodeId, (f32, f32)>,
    depth: u32,
    marker: Option<Marker>,
    /// Inline content gathered for the current block.
    inl: Vec<Inl>,
    last_space: bool,
    /// Boxes taken out of the flow (position: absolute, fixed) that wait for
    /// their containing block to be laid out, with their static position.
    pending: Vec<(NodeId, f32, f32)>,
    /// The lowest edge of the positioned boxes: the page scrolls to them.
    abs_bottom: f32,
    vw: f32,
}

const MAX_DEPTH: u32 = 90;

fn r(x: f32, y: f32, w: f32, h: f32) -> Rect {
    let x0 = x.rnd() as i32;
    let y0 = y.rnd() as i32;
    Rect::new(x0, y0, ((x + w).rnd() as i32 - x0).max(0), ((y + h).rnd() as i32 - y0).max(0))
}

fn fade(c: u32, o: f32) -> u32 {
    if o >= 1.0 {
        return c;
    }
    let a = ((c >> 24) as f32 * o) as u32;
    (a << 24) | (c & 0xFF_FFFF)
}

/// Lays a document out for a viewport `vw` wide.
pub fn layout(dom: &Dom, st: &Styles, fonts: &WebFonts, pics: &dyn Pictures, vw: f32) -> Layout {
    let mut c = Ctx {
        dom,
        st,
        fonts,
        pics,
        items: Vec::new(),
        targets: Vec::new(),
        anchors: Vec::new(),
        wanted: Vec::new(),
        memo: BTreeMap::new(),
        depth: 0,
        marker: None,
        inl: Vec::new(),
        last_space: true,
        pending: Vec::new(),
        abs_bottom: 0.0,
        vw,
    };
    let html = dom.nodes[Dom::ROOT].children.iter().copied().find(|&n| dom.tag(n) == "html").unwrap_or(Dom::ROOT);
    let body = dom.element_children(html).find(|&n| dom.tag(n) == "body");
    let mut floats = Floats::default();
    let mut background = 0xFFFF_FFFF;
    if let Some(s) = c.style(html) {
        if s.background >> 24 != 0 {
            background = s.background;
        } else if let Some(bs) = body.and_then(|b| c.style(b)) {
            if bs.background >> 24 != 0 {
                background = bs.background;
            }
        }
    }
    let e = c.edges(html, vw);
    let h = c.block(html, e.m[3], e.m[0], vw, &mut floats, None, None, true);
    // What nothing else holds is placed in the viewport.
    let at = c.items.len();
    c.place_positioned(0, (0.0, 0.0, vw, crate::style::VH.get()), at, true);
    let height = (h + e.m[0] + e.m[2]).max(floats.bottom()).max(c.abs_bottom).cel() as i32;
    Layout { items: c.items, targets: c.targets, anchors: c.anchors, height, width: vw as i32, background, wanted: c.wanted }
}

impl<'a> Ctx<'a> {
    fn style(&self, n: NodeId) -> Option<&'a Style> {
        self.st.styles.get(n).and_then(|s| s.as_ref())
    }

    fn edges(&self, n: NodeId, cbw: f32) -> Edges {
        let Some(s) = self.style(n) else { return Edges::default() };
        let mut e = Edges::default();
        for i in 0..4 {
            e.m[i] = s.margin[i].resolve(cbw).unwrap_or(0.0);
            e.b[i] = s.border[i].max(0.0);
            e.p[i] = s.padding[i].resolve(cbw).unwrap_or(0.0).max(0.0);
        }
        e
    }

    /// The children that take part in layout, with display: contents
    /// unwrapped.
    fn kids(&self, n: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        self.kids_into(n, &mut out);
        out
    }

    fn kids_into(&self, n: NodeId, out: &mut Vec<NodeId>) {
        let details_closed = self.dom.tag(n) == "details" && self.dom.attr(n, "open").is_none();
        let mut summary_seen = false;
        for &k in &self.dom.nodes[n].children {
            match &self.dom.nodes[k].data {
                Data::Text(_) => {
                    if !details_closed {
                        out.push(k)
                    }
                }
                Data::Element(e) => {
                    if details_closed {
                        if e.tag == "summary" && !summary_seen {
                            summary_seen = true;
                            out.push(k);
                        }
                        continue;
                    }
                    match self.style(k) {
                        Some(s) if s.display == Display::Contents => self.kids_into(k, out),
                        Some(s) if s.display == Display::None => {}
                        Some(s) if s.position == Position::Absolute || s.position == Position::Fixed => {}
                        Some(_) => out.push(k),
                        None => {}
                    }
                }
                Data::Document => {}
            }
        }
    }

    fn is_replaced(&self, n: NodeId) -> bool {
        matches!(self.dom.tag(n), "img" | "input" | "select" | "textarea" | "svg" | "video" | "canvas" | "iframe" | "embed" | "object" | "audio" | "progress" | "meter")
    }

    fn block_level(&self, n: NodeId) -> bool {
        match &self.dom.nodes[n].data {
            Data::Element(_) => match self.style(n) {
                Some(s) => (!s.display.is_inline_level() && s.display != Display::Inline) || s.float != Float::None,
                None => false,
            },
            _ => false,
        }
    }

    fn mark_items(&self) -> (usize, usize) {
        (self.items.len(), self.targets.len())
    }

    fn shift(&mut self, from: (usize, usize), dx: f32, dy: f32) {
        let (dx, dy) = (dx.rnd() as i32, dy.rnd() as i32);
        if dx == 0 && dy == 0 {
            return;
        }
        for it in &mut self.items[from.0..] {
            it.shift(dx, dy);
        }
        for t in &mut self.targets[from.1..] {
            t.0 = t.0.offset(dx, dy);
        }
    }

    /// Placeholders for a box's background and border, filled in once its
    /// size is known.
    fn deco_start(&mut self, n: NodeId) -> Vec<usize> {
        let Some(s) = self.style(n) else { return Vec::new() };
        if !s.visible {
            return Vec::new();
        }
        let mut v = Vec::new();
        if s.background >> 24 != 0 {
            v.push(self.items.len());
            self.items.push(Item::Fill { r: Rect::new(0, 0, 0, 0), color: fade(s.background, s.opacity), radius: s.radius as i32 });
        }
        if let Some(u) = &s.background_image {
            let url = self.pics.resolve(u);
            match self.pics.find(&url) {
                Some((i, Some(_))) => {
                    v.push(self.items.len());
                    self.items.push(Item::Image { r: Rect::new(0, 0, 0, 0), img: i, cover: s.background_cover, radius: s.radius as i32 });
                }
                Some((_, None)) => {}
                None => {
                    if !self.wanted.contains(&url) {
                        self.wanted.push(url);
                    }
                }
            }
        }
        if s.border.iter().any(|&b| b > 0.0) {
            v.push(self.items.len());
            let w = [s.border[0].max(0.0).rnd() as i32, s.border[1].max(0.0).rnd() as i32, s.border[2].max(0.0).rnd() as i32, s.border[3].max(0.0).rnd() as i32];
            let c = s.border_color.map(|c| fade(c, s.opacity));
            self.items.push(Item::Border { r: Rect::new(0, 0, 0, 0), w, c, radius: s.radius as i32 });
        }
        v
    }

    fn deco_end(&mut self, slots: &[usize], rect: Rect) {
        for &i in slots {
            match &mut self.items[i] {
                Item::Fill { r, .. } | Item::Border { r, .. } => *r = rect,
                Item::Image { r, .. } => {
                    // Small boxes with a background picture are mostly
                    // sprite sheets: their picture would be wrong.
                    *r = if rect.w >= 120 && rect.h >= 60 { rect } else { Rect::new(0, 0, 0, 0) };
                }
                _ => {}
            }
        }
    }

    fn anchor(&mut self, n: NodeId, y: f32) {
        if let Some(e) = self.dom.element(n) {
            for key in ["id", "name"] {
                if key == "name" && e.tag != "a" {
                    continue;
                }
                if let Some(v) = e.attr(key) {
                    self.anchors.push((v.to_string(), y as i32));
                }
            }
        }
    }

    /// The width a box takes when it fits its content (floats, inline
    /// blocks, flex items): border box.
    fn shrink_to_fit(&mut self, n: NodeId, avail: f32) -> f32 {
        let (mn, mx) = self.intrinsic(n);
        let e = self.edges(n, avail);
        (mx.min(avail.max(mn))) - e.mh()
    }

    /// The border-box width a block takes in `cbw`, honouring width,
    /// min/max and box-sizing. `None` when auto.
    fn specified_width(&self, s: &Style, e: &Edges, cbw: f32) -> Option<f32> {
        let w = match s.width {
            Len::MinContent | Len::MaxContent | Len::FitContent => return None,
            ref l => l.resolve(cbw)?,
        };
        Some(if s.border_box { w.max(e.h()) } else { w + e.h() })
    }

    fn clamp_width(&self, s: &Style, e: &Edges, cbw: f32, w: f32) -> f32 {
        let adj = |v: f32| if s.border_box { v } else { v + e.h() };
        let mut w = w;
        if let Some(mx) = s.max_width.resolve(cbw) {
            w = w.min(adj(mx));
        }
        if let Some(mn) = s.min_width.resolve(cbw) {
            w = w.max(adj(mn));
        }
        w.max(e.h())
    }

    fn content_height(&self, s: &Style, e: &Edges, cbh: Option<f32>, h: f32) -> f32 {
        let adj = |v: f32| if s.border_box { (v - e.v()).max(0.0) } else { v };
        let mut h = h;
        let res = |l: &Len| -> Option<f32> {
            if l.has_pct() {
                cbh.and_then(|b| l.resolve(b))
            } else {
                l.resolve(0.0)
            }
        };
        if let Some(v) = res(&s.height) {
            h = adj(v);
        }
        if let Some(mx) = res(&s.max_height) {
            if !s.overflow_hidden || mx < h {
                h = h.min(adj(mx).max(0.0));
            }
        }
        if let Some(mn) = res(&s.min_height) {
            h = h.max(adj(mn));
        }
        h
    }

    /// Lays out a block-level box with its border box at (x, y) — `x` is
    /// the containing block's content edge plus the left margin. Returns the
    /// border-box height. `fw` forces the border-box width (flex items,
    /// cells), `fh` the border-box height.
    #[allow(clippy::too_many_arguments)]
    fn block(&mut self, n: NodeId, x: f32, y: f32, cbw: f32, floats: &mut Floats, fw: Option<f32>, fh: Option<f32>, bfc: bool) -> f32 {
        let Some(s) = self.style(n) else { return 0.0 };
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            self.depth -= 1;
            return 0.0;
        }
        let e = self.edges(n, cbw);
        let avail = (cbw - e.mh()).max(0.0);
        let mut w = match fw {
            Some(w) => w,
            None => match self.specified_width(s, &e, cbw) {
                Some(w) => w,
                None if self.is_replaced(n) => self.replaced_size(n, s, cbw).0 + e.h(),
                None if matches!(s.display, Display::Table | Display::InlineTable) => self.table_width(n, s, &e, avail),
                None => avail,
            },
        };
        if fw.is_none() {
            w = self.clamp_width(s, &e, cbw, w);
        }
        // Auto margins centre a block narrower than its container.
        let mut x = x;
        if fw.is_none() && w < avail {
            let (ml, mr) = (s.margin[3].is_auto(), s.margin[1].is_auto());
            if ml && mr {
                x += (avail - w) / 2.0;
            } else if ml {
                x += avail - w;
            }
        }
        // Text alignment centres tables and replaced blocks in <center>.
        self.anchor(n, y);
        let slots = self.deco_start(n);
        let cx = x + e.b[3] + e.p[3];
        let cy = y + e.b[0] + e.p[0];
        let cw = (w - e.h()).max(0.0);
        let mark = self.mark_items();
        let pending = self.pending.len();
        self.note_positioned(n, cx, cy);
        // Before what is inside, so deeper targets win.
        let block_target = self.targets.len();
        self.targets.push((Rect::new(0, 0, 0, 0), Target::Block(n)));
        let clip = s.overflow_hidden && !s.height.is_auto() && self.dom.tag(n) != "html" && self.dom.tag(n) != "body";
        if clip {
            self.items.push(Item::ClipPush(Rect::new(0, 0, 0, 0)));
        }
        let list_marker = s.display == Display::ListItem && s.list_style != ListStyle::None;
        if list_marker {
            self.marker = self.make_marker(n, s, cx);
        }
        let mut own_floats = Floats::default();
        let fl: &mut Floats = if bfc || s.overflow_hidden || s.float != Float::None { &mut own_floats } else { floats };
        let fl_before = fl.list.len();
        let content_h = if s.content_visibility_hidden {
            0.0
        } else if self.is_replaced(n) {
            self.replaced(n, s, cx, cy, cw, fh.map(|h| h - e.v()))
        } else {
            match s.display {
                Display::Flex | Display::InlineFlex => self.flex(n, s, cx, cy, cw, fh.map(|h| h - e.v())),
                Display::Grid | Display::InlineGrid => self.grid(n, s, cx, cy, cw),
                Display::Table | Display::InlineTable => self.table(n, s, cx, cy, cw),
                _ => {
                    let end = self.flow(n, cx, cy, cw, fl);
                    // A block holds the floats placed inside it.
                    let fb = fl.list[fl_before.min(fl.list.len())..].iter().map(|(_, f)| f.y + f.h).fold(cy, f32::max);
                    (end - cy).max(fb - cy)
                }
            }
        };
        if list_marker && self.marker.is_some() {
            // An empty item still shows its marker.
            self.marker_line(cy + s.line() * 0.8);
        }
        let ch = match fh {
            Some(h) => (h - e.v()).max(content_h.min(h - e.v())).max(0.0),
            None => self.content_height(s, &e, None, content_h),
        };
        let bh = ch + e.v();
        let rect = r(x, y, w, bh);
        self.deco_end(&slots, rect);
        self.targets[block_target].0 = rect;
        if s.position != Position::Static {
            let cb = (x + e.b[3], y + e.b[0], w - e.b[1] - e.b[3], bh - e.b[0] - e.b[2]);
            self.place_positioned(pending, cb, mark.0 + clip as usize, false);
        }
        if clip {
            if let Item::ClipPush(cr) = &mut self.items[mark.0] {
                *cr = r(x + e.b[3], y + e.b[0], w - e.b[1] - e.b[3], bh - e.b[0] - e.b[2]);
            }
            self.items.push(Item::ClipPop);
        }
        if s.position == Position::Relative {
            let dx = s.inset[3].resolve(cbw).or_else(|| s.inset[1].resolve(cbw).map(|v| -v)).unwrap_or(0.0);
            let dy = s.inset[0].resolve(0.0).or_else(|| s.inset[2].resolve(0.0).map(|v| -v)).unwrap_or(0.0);
            if dx.abs() < 2000.0 && dy.abs() < 2000.0 {
                self.shift((slots.first().copied().unwrap_or(mark.0), mark.1), dx, dy);
            }
        }
        if s.opacity < 1.0 {
            self.fade_from(mark.0, s.opacity);
        }
        self.depth -= 1;
        bh
    }

    /// Notes the children of `n` that are out of the flow, for the box
    /// that holds them; (x, y) is where they would have been (roughly: the
    /// start of the parent's content).
    fn note_positioned(&mut self, n: NodeId, x: f32, y: f32) {
        if self.dom.tag(n) == "details" && self.dom.attr(n, "open").is_none() {
            return;
        }
        for i in 0..self.dom.nodes[n].children.len() {
            let k = self.dom.nodes[n].children[i];
            let Some(s) = self.style(k) else { continue };
            match s.display {
                Display::None => {}
                _ if s.position == Position::Absolute || s.position == Position::Fixed => self.pending.push((k, x, y)),
                // Boxes that are not laid out as blocks pass theirs on.
                Display::Contents | Display::Inline => self.note_positioned(k, x, y),
                _ => {}
            }
        }
    }

    /// Lays out the boxes out of the flow noted since `from`, in the
    /// containing block `cb` (a padding box: x, y, width, height). Their
    /// items go on top of what was drawn so far, or at `under` for a
    /// z-index below zero. Fixed boxes wait for the viewport (`last`).
    fn place_positioned(&mut self, from: usize, cb: (f32, f32, f32, f32), under: usize, last: bool) {
        if self.pending.len() <= from {
            return;
        }
        let mut noted = self.pending.split_off(from);
        if !last {
            let fixed = |c: &Self, k: NodeId| c.style(k).is_some_and(|s| s.position == Position::Fixed);
            let (wait, now): (Vec<_>, Vec<_>) = noted.into_iter().partition(|&(k, ..)| fixed(self, k));
            self.pending.extend(wait);
            noted = now;
        }
        let (cbx, cby, cbw, cbh) = cb;
        for i in 0..noted.len() {
            let (k, sx, sy) = noted[i];
            // A box measured twice is noted twice: the last note counts.
            if noted[i + 1..].iter().any(|n| n.0 == k) {
                continue;
            }
            let Some(s) = self.style(k) else { continue };
            let [top, right, bottom, left] = [0, 1, 2, 3].map(|i| s.inset[i].resolve(if i % 2 == 0 { cbh } else { cbw }));
            // What sticks to the bottom of the window (banners) would land
            // in the middle of the page: it is left out.
            if s.position == Position::Fixed && top.is_none() && bottom.is_some() {
                continue;
            }
            let e = self.edges(k, cbw);
            let replaced = self.is_replaced(k);
            let room = (cbw - left.unwrap_or(0.0) - right.unwrap_or(0.0) - e.mh()).max(0.0);
            let w = match self.specified_width(s, &e, cbw) {
                Some(w) => w,
                None if replaced => self.replaced_size(k, s, cbw).0 + e.h(),
                None if left.is_some() && right.is_some() => room,
                None => self.shrink_to_fit(k, room + e.mh()).max(e.h()),
            };
            let w = self.clamp_width(s, &e, cbw, w);
            let centred = s.margin[3].is_auto() && s.margin[1].is_auto();
            let x = match (left, right) {
                (Some(l), Some(rr)) if centred => cbx + l + ((cbw - l - rr - w) / 2.0).max(0.0),
                (Some(l), _) => cbx + l + e.m[3],
                (None, Some(rr)) => cbx + cbw - rr - e.m[1] - w,
                (None, None) => sx + e.m[3],
            };
            let fh = match (top, bottom) {
                (Some(t), Some(b)) if s.height.is_auto() && !replaced => Some((cbh - t - b - e.m[0] - e.m[2]).max(0.0)),
                _ if s.height.has_pct() => s.height.resolve(cbh).map(|h| if s.border_box { h } else { h + e.v() }),
                _ => None,
            };
            let y = match (top, bottom) {
                (Some(t), _) => cby + t + e.m[0],
                (None, Some(_)) => 0.0,
                (None, None) => sy + e.m[0],
            };
            let mark = self.mark_items();
            let saved_inl = core::mem::take(&mut self.inl);
            let saved_marker = self.marker.take();
            let bh = self.block(k, x, y, cbw, &mut Floats::default(), if replaced { None } else { Some(w) }, fh, true);
            self.inl = saved_inl;
            self.marker = saved_marker;
            let mut dx = s.translate[0].resolve(w).unwrap_or(0.0);
            let mut dy = s.translate[1].resolve(bh).unwrap_or(0.0);
            if let (None, Some(b)) = (top, bottom) {
                dy += cby + cbh - b - e.m[2] - bh;
            }
            // Far away is the way pages hide things.
            if dx.abs() > 4000.0 || dy.abs() > 20000.0 {
                dx = dx.clamp(-4000.0, 4000.0);
                dy = dy.clamp(-20000.0, 20000.0);
            }
            self.shift(mark, dx, dy);
            let (bx, by) = (x + dx, y + dy);
            if s.visible && bh > 0.0 && bx < self.vw && bx + w > 0.0 && by + bh > 0.0 {
                self.abs_bottom = self.abs_bottom.max(by + bh + e.m[2].max(0.0));
            }
            if s.z_below && under < mark.0 {
                let made = self.items.len() - mark.0;
                self.items[under..].rotate_right(made);
            }
        }
    }

    fn fade_from(&mut self, from: usize, o: f32) {
        for it in &mut self.items[from..] {
            match it {
                Item::Text { color, .. } | Item::Fill { color, .. } => *color = fade(*color, o),
                _ => {}
            }
        }
    }

    fn make_marker(&self, n: NodeId, s: &Style, cx: f32) -> Option<Marker> {
        let face = self.fonts.face(s.font_size, s.bold, s.mono);
        let ordinal = || {
            let parent = self.dom.parent_element(n)?;
            let start: i32 = self.dom.attr(parent, "start").and_then(|v| v.trim().parse().ok()).unwrap_or(1);
            let mut i = start;
            for c in self.dom.element_children(parent) {
                if c == n {
                    return Some(self.dom.attr(n, "value").and_then(|v| v.trim().parse().ok()).unwrap_or(i));
                }
                if self.style(c).is_some_and(|cs| cs.display == Display::ListItem) {
                    i += 1;
                }
            }
            Some(i)
        };
        let text = match s.list_style {
            ListStyle::Decimal => alloc::format!("{}.", ordinal()?),
            ListStyle::LowerAlpha | ListStyle::UpperAlpha => {
                let i = ordinal()?.max(1) as u32;
                let c = char::from_u32(if s.list_style == ListStyle::LowerAlpha { 'a' } else { 'A' } as u32 + (i - 1) % 26)?;
                alloc::format!("{c}.")
            }
            ListStyle::LowerRoman | ListStyle::UpperRoman => {
                let r = roman(ordinal()?.max(1) as u32);
                alloc::format!("{}.", if s.list_style == ListStyle::LowerRoman { r.to_ascii_lowercase() } else { r })
            }
            _ => String::new(),
        };
        let bullet = matches!(s.list_style, ListStyle::Disc | ListStyle::Circle | ListStyle::Square).then_some((s.list_style, s.font_size));
        Some(Marker { x: cx, text, face, color: s.color, bullet })
    }

    /// Draws a pending list marker beside the line whose baseline is `base`.
    fn marker_line(&mut self, base: f32) {
        let Some(m) = self.marker.take() else { return };
        if let Some((kind, size)) = m.bullet {
            let d = (size * 0.36).max(4.0);
            let cx = m.x - size * 0.9;
            let cy = base - size * 0.32;
            let rect = r(cx - d / 2.0, cy - d / 2.0, d, d);
            match kind {
                ListStyle::Square => self.items.push(Item::Fill { r: rect, color: m.color, radius: 0 }),
                ListStyle::Circle => self.items.push(Item::Border { r: rect, w: [1; 4], c: [m.color; 4], radius: 99 }),
                _ => self.items.push(Item::Fill { r: rect, color: m.color, radius: 99 }),
            }
        } else if !m.text.is_empty() {
            let w = self.fonts.width(m.face, &m.text);
            let (_, asc) = self.fonts.metrics(m.face);
            self.items.push(Item::Text { x: (m.x - 6.0) as i32 - w, y: (base as i32) - asc, face: m.face, color: m.color, text: m.text, deco: 0, width: w });
        }
    }

    /// The children of a block container: block boxes one under the other,
    /// inline content in lines. Returns the bottom of the content.
    fn flow(&mut self, n: NodeId, x: f32, y: f32, w: f32, floats: &mut Floats) -> f32 {
        let s = self.style(n);
        let saved_inl = core::mem::take(&mut self.inl);
        self.last_space = true;
        let mut cur = y;
        let mut prev_mb: f32 = 0.0;
        let mut started = false;
        for k in self.kids(n) {
            if self.block_level(k) {
                let ks = self.style(k).expect("an element has a style");
                if ks.float != Float::None {
                    self.flush_lines(n, x, &mut cur, &mut prev_mb, w, floats);
                    self.float(k, ks, x, cur, w, floats);
                    continue;
                }
                if self.flush_lines(n, x, &mut cur, &mut prev_mb, w, floats) {
                    started = true;
                }
                let e = self.edges(k, w);
                let gap = if !started {
                    e.m[0]
                } else if prev_mb >= 0.0 && e.m[0] >= 0.0 {
                    prev_mb.max(e.m[0])
                } else {
                    prev_mb + e.m[0]
                };
                let mut top = cur + gap;
                if ks.clear_both || ks.clear != Float::None {
                    let c = floats.clear(top, ks.clear_both || ks.clear == Float::Left, ks.clear_both || ks.clear == Float::Right);
                    top = top.max(c);
                }
                // A box that holds its own floats goes beside floats.
                let (mut bx, mut bw) = (x, w);
                let new_bfc = ks.overflow_hidden || matches!(ks.display, Display::Flex | Display::Grid | Display::Table) || self.is_replaced(k);
                if new_bfc && !floats.list.is_empty() {
                    let (l, rr) = floats.edges(top, 20.0, x, x + w);
                    bx = l;
                    bw = rr - l;
                }
                let h = self.block(k, bx + e.m[3], top, bw, floats, None, None, false);
                cur = top + h;
                prev_mb = e.m[2];
                started = true;
            } else {
                self.inline_node(k, w);
            }
        }
        self.flush_lines(n, x, &mut cur, &mut prev_mb, w, floats);
        let _ = s;
        self.inl = saved_inl;
        cur + prev_mb.max(0.0)
    }

    /// Lays out the inline content gathered so far; lines that follow a
    /// block go below its bottom margin. Whether any line was made.
    fn flush_lines(&mut self, n: NodeId, x: f32, cur: &mut f32, prev_mb: &mut f32, w: f32, floats: &mut Floats) -> bool {
        if self.inl.is_empty() {
            return false;
        }
        let top = *cur + prev_mb.max(0.0);
        let end = self.lines(n, x, top, w, floats);
        if end > top {
            *cur = end;
            *prev_mb = 0.0;
            true
        } else {
            false
        }
    }

    fn float(&mut self, k: NodeId, ks: &Style, x: f32, y: f32, w: f32, floats: &mut Floats) {
        let e = self.edges(k, w);
        let bw = match self.specified_width(ks, &e, w) {
            Some(bw) => self.clamp_width(ks, &e, w, bw),
            None if self.is_replaced(k) => self.replaced_size(k, ks, w).0 + e.h(),
            None => {
                let sw = self.shrink_to_fit(k, w);
                self.clamp_width(ks, &e, w, sw)
            }
        };
        let mark = self.mark_items();
        let mut sub = Floats::default();
        let h = self.block(k, 0.0, 0.0, bw + e.mh(), &mut sub, Some(bw), None, true);
        let ow = bw + e.mh();
        let oh = h + e.m[0] + e.m[2];
        let left = ks.float == Float::Left;
        let mut fy = if ks.clear_both { floats.clear(y, true, true) } else { y };
        // Down until there is room.
        for _ in 0..200 {
            let (l, rr) = floats.edges(fy, oh, x, x + w);
            if rr - l >= ow || (l <= x && rr >= x + w) {
                break;
            }
            let next = floats.list.iter().map(|(_, f)| f.y + f.h).filter(|&b| b > fy).fold(f32::MAX, f32::min);
            if next == f32::MAX {
                break;
            }
            fy = next;
        }
        let (l, rr) = floats.edges(fy, oh, x, x + w);
        let fx = if left { l } else { (rr - ow).max(x) };
        self.shift(mark, fx + e.m[3], fy + e.m[0]);
        floats.list.push((left, FRect { x: fx, y: fy, w: ow, h: oh }));
    }

    // Inline content.

    fn inline_node(&mut self, k: NodeId, cbw: f32) {
        match &self.dom.nodes[k].data {
            Data::Text(t) => {
                let p = self.dom.nodes[k].parent.unwrap_or(0);
                if let Some(s) = self.style(p) {
                    let link = self.link_of(p);
                    let owner = self.inline_owner(p);
                    self.text(t, s, link, owner);
                }
            }
            Data::Element(e) => {
                let Some(s) = self.style(k) else { return };
                if e.tag == "br" {
                    self.inl.push(Inl::Break { lh: s.line() });
                    self.last_space = true;
                    return;
                }
                if e.tag == "wbr" {
                    return;
                }
                if s.display == Display::Inline && !self.is_replaced(k) {
                    self.anchor(k, 0.0);
                    let ed = self.edges(k, cbw);
                    let lw = ed.m[3] + ed.b[3] + ed.p[3];
                    let rw = ed.m[1] + ed.b[1] + ed.p[1];
                    let decorated = s.background >> 24 != 0 || s.border.iter().any(|&b| b > 0.0);
                    if lw > 0.0 || decorated {
                        self.inl.push(Inl::Edge { w: lw, owner: k, open: true });
                    }
                    for c in self.kids(k) {
                        if self.block_level(c) {
                            // A block inside an inline: on its own line.
                            self.inline_atomic(c, cbw, true);
                        } else {
                            self.inline_node(c, cbw);
                        }
                    }
                    if rw > 0.0 || decorated {
                        self.inl.push(Inl::Edge { w: rw, owner: k, open: false });
                    }
                    return;
                }
                self.inline_atomic(k, cbw, false);
            }
            Data::Document => {}
        }
    }

    /// The link an inline element is in.
    fn link_of(&self, mut n: NodeId) -> Option<NodeId> {
        for _ in 0..40 {
            if let Some(e) = self.dom.element(n) {
                if (e.tag == "a" || e.tag == "area") && e.attr("href").is_some() {
                    return Some(n);
                }
                if e.tag == "summary" {
                    return Some(n);
                }
                if e.tag == "label" {
                    return Some(n);
                }
                if e.tag == "button" {
                    return Some(n);
                }
                if self.style(n).is_some_and(|s| !matches!(s.display, Display::Inline | Display::InlineBlock | Display::InlineFlex)) {
                    return None;
                }
            }
            n = self.dom.nodes[n].parent?;
        }
        None
    }

    /// The nearest inline element with a background or border.
    fn inline_owner(&self, mut n: NodeId) -> Option<NodeId> {
        for _ in 0..20 {
            let s = self.style(n)?;
            if s.display != Display::Inline {
                return None;
            }
            if s.background >> 24 != 0 || s.border.iter().any(|&b| b > 0.0) {
                return Some(n);
            }
            n = self.dom.nodes[n].parent?;
        }
        None
    }

    fn text(&mut self, t: &str, s: &Style, link: Option<NodeId>, owner: Option<NodeId>) {
        let face = self.fonts.face(s.font_size, s.bold, s.mono);
        let (flh, fasc) = self.fonts.metrics(face);
        let lh = s.line();
        let half = (lh - flh as f32) / 2.0;
        let asc = half + fasc as f32;
        let desc = lh - asc;
        let space_w = self.fonts.advance(face, ' ') as f32 + s.letter_spacing;
        let deco = (s.underline as u8) | (s.strike as u8) << 1;
        let transformed;
        let t = match s.transform {
            Transform::Upper => {
                transformed = t.to_uppercase();
                transformed.as_str()
            }
            Transform::Lower => {
                transformed = t.to_lowercase();
                transformed.as_str()
            }
            Transform::Capitalize => {
                transformed = capitalize(t);
                transformed.as_str()
            }
            Transform::None => t,
        };
        let word = |text: String, this: &mut Self, nowrap: bool| {
            let w = this.fonts.width(face, &text) as f32 + s.letter_spacing * text.chars().count() as f32;
            this.inl.push(Inl::Word { text, w, face, color: s.color, deco, owner, link, space: 0.0, lh, asc, desc, nowrap, visible: s.visible });
        };
        match s.white_space {
            WhiteSpace::Pre | WhiteSpace::PreWrap | WhiteSpace::PreLine => {
                let pre = s.white_space != WhiteSpace::PreLine;
                for (i, line) in t.split('\n').enumerate() {
                    if i > 0 {
                        self.inl.push(Inl::Break { lh });
                    }
                    let line = line.replace('\t', "    ");
                    let line = line.trim_end_matches('\r');
                    if s.white_space == WhiteSpace::Pre {
                        if !line.is_empty() {
                            word(line.to_string(), self, true);
                        }
                    } else {
                        self.words(if pre { line } else { line.trim() }, space_w, &word, pre);
                    }
                }
                self.last_space = false;
            }
            _ => {
                let nowrap = s.white_space == WhiteSpace::NoWrap;
                let starts_space = t.starts_with(|c: char| c.is_ascii_whitespace());
                if starts_space && !self.last_space {
                    self.add_space(space_w);
                }
                let mut any = false;
                for wd in t.split_ascii_whitespace() {
                    if any {
                        self.add_space(space_w);
                    }
                    word(wd.to_string(), self, nowrap);
                    any = true;
                }
                if any {
                    self.last_space = false;
                }
                if t.ends_with(|c: char| c.is_ascii_whitespace()) && (any || !starts_space) && !self.last_space {
                    self.add_space(space_w);
                }
                if nowrap && any {
                    // Keep the words of a nowrap run together.
                    let n = self.inl.len();
                    let mut joined: Option<usize> = None;
                    for i in (0..n).rev() {
                        if let Inl::Word { nowrap: true, owner: o, link: l, face: f, .. } = &self.inl[i] {
                            if *o == owner && *l == link && *f == face {
                                joined = Some(i);
                                continue;
                            }
                        }
                        break;
                    }
                    if let Some(first) = joined {
                        if n - first > 1 {
                            let mut text = String::new();
                            let mut w = 0.0;
                            let mut space = 0.0;
                            for it in self.inl.drain(first..) {
                                if let Inl::Word { text: t2, w: w2, space: sp, .. } = it {
                                    text.push_str(&t2);
                                    w += w2;
                                    if sp > 0.0 {
                                        text.push(' ');
                                        w += sp;
                                    }
                                    space = sp;
                                }
                            }
                            if space > 0.0 {
                                text.pop();
                                w -= space;
                            }
                            self.inl.push(Inl::Word { text, w, face, color: s.color, deco, owner, link, space, lh, asc, desc, nowrap: true, visible: s.visible });
                        }
                    }
                }
            }
        }
    }

    fn words(&mut self, line: &str, space_w: f32, word: &dyn Fn(String, &mut Self, bool), keep: bool) {
        let mut first = true;
        for wd in line.split(' ') {
            if !first {
                self.add_space(space_w);
            }
            first = false;
            if !wd.is_empty() || keep {
                if !wd.is_empty() {
                    word(wd.to_string(), self, false);
                }
            }
        }
    }

    fn add_space(&mut self, w: f32) {
        self.last_space = true;
        match self.inl.last_mut() {
            Some(Inl::Word { space, .. }) | Some(Inl::Atomic { space, .. }) => {
                if *space == 0.0 {
                    *space = w;
                }
            }
            Some(Inl::Edge { .. }) => {
                // A space after an element's end: on the word before it.
                let n = self.inl.len();
                for i in (0..n).rev() {
                    match &mut self.inl[i] {
                        Inl::Word { space, .. } | Inl::Atomic { space, .. } => {
                            if *space == 0.0 {
                                *space = w;
                            }
                            return;
                        }
                        Inl::Edge { .. } => continue,
                        Inl::Break { .. } => return,
                    }
                }
            }
            _ => {}
        }
    }

    /// An inline-level box laid out on its own (inline-block, image,
    /// control) and kept to be put on a line.
    fn inline_atomic(&mut self, k: NodeId, cbw: f32, own_line: bool) {
        let Some(s) = self.style(k) else { return };
        let e = self.edges(k, cbw);
        let bw = match self.specified_width(s, &e, cbw) {
            Some(w) => self.clamp_width(s, &e, cbw, w),
            None if self.is_replaced(k) => self.replaced_size(k, s, cbw).0 + e.h(),
            None if own_line => cbw - e.mh(),
            None => {
                let sw = self.shrink_to_fit(k, cbw);
                self.clamp_width(s, &e, cbw, sw)
            }
        };
        let saved_marker = self.marker.take();
        let saved_inl = core::mem::take(&mut self.inl);
        let saved_space = self.last_space;
        let mark = self.mark_items();
        let mut sub = Floats::default();
        let h = self.block(k, e.m[3], e.m[0], bw + e.mh(), &mut sub, Some(bw), None, true);
        self.inl = saved_inl;
        self.last_space = saved_space;
        self.marker = saved_marker;
        let items: Vec<Item> = self.items.drain(mark.0..).collect();
        let targets: Vec<(Rect, Target)> = self.targets.drain(mark.1..).collect();
        if own_line {
            self.inl.push(Inl::Break { lh: 0.0 });
        }
        let link = self.link_of(k).or_else(|| matches!(self.dom.tag(k), "input" | "select" | "textarea" | "button").then_some(k));
        self.inl.push(Inl::Atomic { items, targets, w: bw + e.mh(), h: h + e.m[0] + e.m[2], valign: s.vertical_align, link, space: 0.0 });
        if own_line {
            self.inl.push(Inl::Break { lh: 0.0 });
        }
        self.last_space = false;
    }

    /// Puts the gathered inline content in lines from y; returns the bottom.
    fn lines(&mut self, block: NodeId, x: f32, y: f32, w: f32, floats: &mut Floats) -> f32 {
        let items = core::mem::take(&mut self.inl);
        self.last_space = true;
        let s = self.style(block).cloned().unwrap_or_else(Style::initial);
        let align = s.text_align;
        let strut = s.line();
        let mut cur = y;
        let mut line: Vec<Inl> = Vec::new();
        let mut lw = 0.0f32;
        let mut first_line = true;
        let indent = s.text_indent;
        let (mut l, mut rr) = floats.edges(cur, strut, x, x + w);
        let mut avail = rr - l - if first_line { indent } else { 0.0 };
        let mut it = items.into_iter().peekable();
        let mut pending: Option<Inl> = None;
        loop {
            let item = match pending.take().or_else(|| it.next()) {
                Some(i) => i,
                None => break,
            };
            match item {
                Inl::Break { lh } => {
                    let only_atomic = lh == 0.0 && line.is_empty();
                    if !only_atomic || !line.is_empty() {
                        cur = self.emit(&mut line, l + if first_line { indent } else { 0.0 }, cur, avail, align, if lh > 0.0 { lh } else { 0.0 }, &s);
                        first_line = false;
                    }
                    lw = 0.0;
                    (l, rr) = floats.edges(cur, strut, x, x + w);
                    avail = rr - l;
                }
                item => {
                    let (iw, sp) = match &item {
                        Inl::Word { w, space, .. } => (*w, *space),
                        Inl::Atomic { w, space, .. } => (*w, *space),
                        Inl::Edge { w, .. } => (*w, 0.0),
                        Inl::Break { .. } => (0.0, 0.0),
                    };
                    let fits = lw + iw <= avail + 0.5;
                    if !fits && !line.is_empty() && !matches!(item, Inl::Edge { open: false, .. }) {
                        // The trailing space of the last word does not count.
                        cur = self.emit(&mut line, l + if first_line { indent } else { 0.0 }, cur, avail, align, 0.0, &s);
                        first_line = false;
                        lw = 0.0;
                        (l, rr) = floats.edges(cur, strut, x, x + w);
                        avail = rr - l;
                        // Beside floats with no room: below them.
                        if avail < iw && !floats.list.is_empty() {
                            let mut probe = cur;
                            for _ in 0..50 {
                                let next = floats.list.iter().map(|(_, f)| f.y + f.h).filter(|&b| b > probe).fold(f32::MAX, f32::min);
                                if next == f32::MAX {
                                    break;
                                }
                                probe = next;
                                let (l2, r2) = floats.edges(probe, strut, x, x + w);
                                if r2 - l2 >= iw || (l2 <= x && r2 >= x + w) {
                                    break;
                                }
                            }
                            if probe > cur {
                                cur = probe;
                                (l, rr) = floats.edges(cur, strut, x, x + w);
                                avail = rr - l;
                            }
                        }
                        pending = Some(item);
                        continue;
                    }
                    // A word longer than a whole line: cut it.
                    if !fits && line.is_empty() {
                        let cuttable = match &item {
                            Inl::Word { text, nowrap: false, face, .. } if iw > avail && text.chars().count() > 1 => Some((text.clone(), *face)),
                            _ => None,
                        };
                        if let Some((text, face)) = cuttable {
                            {
                                let (head, tail) = self.cut(&text, face, avail.max(10.0));
                                if !tail.is_empty() {
                                    let mut a = item;
                                    let mut b_item = clone_word(&a);
                                    if let (Inl::Word { text: ta, w: wa, space: sa, .. }, Inl::Word { text: tb, w: wb, .. }) = (&mut a, &mut b_item) {
                                        *ta = head;
                                        *wa = self.fonts.width(face, ta) as f32;
                                        *tb = tail;
                                        *wb = self.fonts.width(face, tb) as f32;
                                        *sa = 0.0;
                                    }
                                    line.push(a);
                                    cur = self.emit(&mut line, l + if first_line { indent } else { 0.0 }, cur, avail, align, 0.0, &s);
                                    first_line = false;
                                    lw = 0.0;
                                    (l, rr) = floats.edges(cur, strut, x, x + w);
                                    avail = rr - l;
                                    pending = Some(b_item);
                                    continue;
                                }
                            }
                        }
                    }
                    lw += iw + sp;
                    line.push(item);
                }
            }
        }
        if !line.is_empty() {
            cur = self.emit(&mut line, l + if first_line { indent } else { 0.0 }, cur, avail, align, 0.0, &s);
        }
        cur
    }

    fn cut(&self, text: &str, face: Face, max: f32) -> (String, String) {
        let mut w = 0.0;
        let mut head = String::new();
        let mut chars = text.chars();
        for c in chars.by_ref() {
            let a = self.fonts.advance(face, c) as f32;
            if w + a > max && !head.is_empty() {
                let mut tail = String::new();
                tail.push(c);
                tail.extend(chars);
                return (head, tail);
            }
            w += a;
            head.push(c);
        }
        (head, String::new())
    }

    /// Places one line at y and paints it; returns the line's bottom.
    #[allow(clippy::too_many_arguments)]
    fn emit(&mut self, line: &mut Vec<Inl>, x: f32, y: f32, avail: f32, align: Align, min_h: f32, s: &Style) -> f32 {
        if line.is_empty() {
            return y + min_h;
        }
        // Heights around the baseline.
        let (mut above, mut below) = (0.0f32, 0.0f32);
        let mut has_text = false;
        for it in line.iter() {
            match it {
                Inl::Word { asc, desc, .. } => {
                    above = above.max(*asc);
                    below = below.max(*desc);
                    has_text = true;
                }
                Inl::Atomic { h, valign, .. } => match valign {
                    VAlign::Middle => {
                        let xh = s.font_size * 0.3;
                        above = above.max(h / 2.0 + xh);
                        below = below.max(h / 2.0 - xh);
                    }
                    VAlign::Top | VAlign::TextTop => {
                        above = above.max(s.font_size);
                        below = below.max(h - s.font_size);
                    }
                    _ => above = above.max(*h),
                },
                _ => {}
            }
        }
        if has_text || min_h > 0.0 {
            // The strut of the block's own font.
            let lh = s.line();
            let face = self.fonts.face(s.font_size, s.bold, s.mono);
            let (flh, fasc) = self.fonts.metrics(face);
            let a = (lh - flh as f32) / 2.0 + fasc as f32;
            above = above.max(a);
            below = below.max(lh - a);
        }
        // Drop the trailing space of the line.
        if let Some(Inl::Word { space, .. }) | Some(Inl::Atomic { space, .. }) = line.last_mut() {
            *space = 0.0;
        }
        let total: f32 = line
            .iter()
            .map(|it| match it {
                Inl::Word { w, space, .. } | Inl::Atomic { w, space, .. } => w + space,
                Inl::Edge { w, .. } => *w,
                _ => 0.0,
            })
            .sum();
        let mut px = match align {
            Align::Center => x + ((avail - total) / 2.0).max(0.0),
            Align::Right => x + (avail - total).max(0.0),
            _ => x,
        };
        let base = y + above;
        let height = (above + below).max(min_h);
        self.marker_line(base);
        // Inline backgrounds: owner -> (start x, end x).
        let mut owners: Vec<(NodeId, f32, f32)> = Vec::new();
        let mut run: Option<(i32, i32, Face, u32, u8, String, Option<NodeId>, bool)> = None;
        let mut link_spans: Vec<(NodeId, f32, f32)> = Vec::new();
        let flush = |this: &mut Self, run: &mut Option<(i32, i32, Face, u32, u8, String, Option<NodeId>, bool)>| {
            if let Some((rx, ry, face, color, deco, text, _, visible)) = run.take() {
                if visible {
                    let width = this.fonts.width(face, &text);
                    this.items.push(Item::Text { x: rx, y: ry, face, color, text, deco, width });
                }
            }
        };
        let bg_start = self.items.len();
        let mut drained: Vec<Inl> = line.drain(..).collect();
        for it in drained.iter_mut() {
            match it {
                Inl::Word { text, w, face, color, deco, owner, link, space, visible, .. } => {
                    let (_, fasc) = self.fonts.metrics(*face);
                    let ty = (base - fasc as f32).rnd() as i32;
                    let same = run.as_ref().is_some_and(|r| r.1 == ty && r.2 == *face && r.3 == *color && r.4 == *deco && r.6 == *link && r.7 == *visible);
                    if same {
                        if let Some(r) = run.as_mut() {
                            r.5.push_str(text);
                        }
                    } else {
                        flush(self, &mut run);
                        run = Some((px.rnd() as i32, ty, *face, *color, *deco, core::mem::take(text), *link, *visible));
                    }
                    if *space > 0.0 {
                        if let Some(r) = run.as_mut() {
                            r.5.push(' ');
                        }
                    }
                    if let Some(o) = owner {
                        extend(&mut owners, *o, px, px + *w + *space);
                    }
                    if let Some(lk) = link {
                        extend(&mut link_spans, *lk, px, px + *w + *space);
                    }
                    px += *w + *space;
                }
                Inl::Atomic { items, targets, w, h, valign, link, space } => {
                    flush(self, &mut run);
                    let top = match valign {
                        VAlign::Middle => base - *h / 2.0 - s.font_size * 0.3,
                        VAlign::Top | VAlign::TextTop => base - s.font_size,
                        _ => base - *h,
                    };
                    let (dx, dy) = (px.rnd() as i32, top.rnd() as i32);
                    for mut i in items.drain(..) {
                        i.shift(dx, dy);
                        self.items.push(i);
                    }
                    for (rr, t) in targets.drain(..) {
                        self.targets.push((rr.offset(dx, dy), t));
                    }
                    if let Some(lk) = link {
                        self.targets.push((r(px, top, *w, *h), Target::Link(*lk)));
                    }
                    px += *w + *space;
                }
                Inl::Edge { w, owner, open } => {
                    flush(self, &mut run);
                    if *open {
                        extend(&mut owners, *owner, px, px + *w);
                    } else {
                        extend(&mut owners, *owner, px - 0.0, px + *w);
                    }
                    px += *w;
                }
                Inl::Break { .. } => {}
            }
        }
        flush(self, &mut run);
        // Backgrounds of inline elements, under their text.
        let mut bgs = Vec::new();
        for (o, x0, x1) in owners {
            let Some(os) = self.style(o) else { continue };
            let face = self.fonts.face(os.font_size, os.bold, os.mono);
            let (flh, fasc) = self.fonts.metrics(face);
            let e = self.edges(o, avail);
            let top = base - fasc as f32 - e.p[0] - e.b[0];
            let hh = flh as f32 + e.p[0] + e.p[2] + e.b[0] + e.b[2];
            let rect = r(x0 + e.m[3], top, (x1 - x0 - e.m[3] - e.m[1]).max(0.0), hh);
            if os.background >> 24 != 0 {
                bgs.push(Item::Fill { r: rect, color: os.background, radius: os.radius as i32 });
            }
            if os.border.iter().any(|&b| b > 0.0) {
                let w = os.border.map(|b| b.max(0.0).rnd() as i32);
                bgs.push(Item::Border { r: rect, w, c: os.border_color, radius: os.radius as i32 });
            }
        }
        if !bgs.is_empty() {
            let tail: Vec<Item> = self.items.drain(bg_start..).collect();
            self.items.extend(bgs);
            self.items.extend(tail);
        }
        for (lk, x0, x1) in link_spans {
            self.targets.push((r(x0, y, x1 - x0, height), Target::Link(lk)));
        }
        y + height
    }

    // Replaced elements.

    /// Natural size of a picture element, from the picture or its
    /// attributes.
    fn natural(&self, n: NodeId) -> Option<(f32, f32)> {
        let attr = |k: &str| self.dom.attr(n, k).and_then(|v| v.trim().trim_end_matches("px").parse::<f32>().ok());
        if self.dom.tag(n) == "img" || self.dom.tag(n) == "input" {
            if let Some(src) = self.image_src(n) {
                if let Some((_, Some((w, h)))) = self.pics.find(&src) {
                    return Some((w as f32, h as f32));
                }
            }
        }
        match (attr("width"), attr("height")) {
            (Some(w), Some(h)) => Some((w, h)),
            _ => None,
        }
    }

    /// The address of an image element's picture.
    pub fn image_src(&self, n: NodeId) -> Option<String> {
        image_src(self.dom, n).map(|s| self.pics.resolve(&s))
    }

    /// Content-box size of a replaced element.
    fn replaced_size(&self, n: NodeId, s: &Style, cbw: f32) -> (f32, f32) {
        let tag = self.dom.tag(n);
        let fsz = s.font_size;
        let face = self.fonts.face(fsz, s.bold, s.mono);
        let (flh, _) = self.fonts.metrics(face);
        let e = self.edges(n, cbw);
        let line = s.line_height.unwrap_or(flh as f32 + 2.0).max(flh as f32);
        let w_spec = s.width.resolve(cbw).map(|w| if s.border_box { (w - e.h()).max(0.0) } else { w });
        let h_spec = if s.height.has_pct() { None } else { s.height.resolve(0.0).map(|h| if s.border_box { (h - e.v()).max(0.0) } else { h }) };
        let (dw, dh) = match tag {
            "input" => {
                let t = self.dom.attr(n, "type").unwrap_or("text").to_ascii_lowercase();
                match t.as_str() {
                    "checkbox" | "radio" => (13.0, 13.0),
                    "submit" | "button" | "reset" => {
                        let label = input_label(self.dom, n);
                        (self.fonts.width(face, &label) as f32, line)
                    }
                    "image" => self.natural(n).unwrap_or((60.0, 20.0)),
                    "range" => (130.0, 16.0),
                    "color" => (40.0, 20.0),
                    "file" => (220.0, line),
                    _ => {
                        let size: f32 = self.dom.attr(n, "size").and_then(|v| v.parse().ok()).unwrap_or(20.0);
                        (size * fsz * 0.55, line)
                    }
                }
            }
            "select" => {
                let w = self
                    .dom
                    .descendants(n)
                    .into_iter()
                    .filter(|&o| self.dom.tag(o) == "option")
                    .map(|o| self.fonts.width(face, self.dom.text(o).trim()))
                    .max()
                    .unwrap_or(40) as f32;
                (w + 26.0, line)
            }
            "textarea" => {
                let cols: f32 = self.dom.attr(n, "cols").and_then(|v| v.parse().ok()).unwrap_or(20.0);
                let rows: f32 = self.dom.attr(n, "rows").and_then(|v| v.parse().ok()).unwrap_or(2.0);
                (cols * fsz * 0.6, rows * line)
            }
            "iframe" | "video" | "canvas" | "embed" | "object" => self.natural(n).unwrap_or((300.0, 150.0)),
            "audio" => (300.0, 40.0),
            "progress" | "meter" => (160.0, 12.0),
            "svg" => {
                let vb = self.dom.attr(n, "viewbox").map(|v| v.split([' ', ',']).filter_map(|x| x.parse::<f32>().ok()).collect::<Vec<_>>());
                match (self.natural(n), vb) {
                    (Some(sz), _) => sz,
                    (None, Some(v)) if v.len() == 4 && v[2] > 0.0 && v[3] > 0.0 => {
                        // An icon: as tall as the text.
                        (fsz * v[2] / v[3], fsz)
                    }
                    _ => (fsz, fsz),
                }
            }
            _ => self.natural(n).unwrap_or((0.0, 0.0)),
        };
        let ratio = s.aspect_ratio.or_else(|| (dw > 0.0 && dh > 0.0).then(|| dw / dh));
        let (mut w, mut h) = match (w_spec, h_spec) {
            (Some(w), Some(h)) => (w, h),
            (Some(w), None) => (w, ratio.map_or(dh, |r| w / r)),
            (None, Some(h)) => (ratio.map_or(dw, |r| h * r), h),
            (None, None) => (dw, dh),
        };
        if let Some(mx) = s.max_width.resolve(cbw) {
            let mx = if s.border_box { (mx - e.h()).max(0.0) } else { mx };
            if w > mx {
                if h_spec.is_none() {
                    if let Some(r) = ratio {
                        h = mx / r;
                    }
                }
                w = mx;
            }
        }
        if let Some(mh) = if s.max_height.has_pct() { None } else { s.max_height.resolve(0.0) } {
            if h > mh {
                if w_spec.is_none() {
                    if let Some(r) = ratio {
                        w = mh * r;
                    }
                }
                h = mh;
            }
        }
        if let Some(mn) = s.min_width.resolve(cbw) {
            w = w.max(mn);
        }
        (w.max(0.0), h.max(0.0))
    }

    /// Paints a replaced element's content box; returns its height.
    fn replaced(&mut self, n: NodeId, s: &Style, x: f32, y: f32, w: f32, fh: Option<f32>) -> f32 {
        let (_, h) = self.replaced_size(n, s, w + self.edges(n, w).h());
        let h = fh.unwrap_or(h);
        let face = self.fonts.face(s.font_size, s.bold, s.mono);
        let rect = r(x, y, w, h);
        let tag = self.dom.tag(n);
        if !s.visible {
            return h;
        }
        match tag {
            "img" => {
                if let Some(src) = self.image_src(n) {
                    match self.pics.find(&src) {
                        Some((i, Some(_))) => self.items.push(Item::Image { r: rect, img: i, cover: s.object_fit_cover, radius: s.radius as i32 }),
                        _ => {
                            if let Some(alt) = self.dom.attr(n, "alt").filter(|a| !a.is_empty()) {
                                if w > 20.0 && h > 14.0 {
                                    self.items.push(Item::Fill { r: rect, color: 0x10_000000, radius: 2 });
                                    let (_, asc) = self.fonts.metrics(face);
                                    let text = alt.chars().take(80).collect::<String>();
                                    let tw = self.fonts.width(face, &text);
                                    self.items.push(Item::ClipPush(rect));
                                    self.items.push(Item::Text { x: rect.x + 4, y: rect.y + 2 + asc.min(4) - asc.min(4), face, color: 0xFF70_7070, text, deco: 0, width: tw });
                                    self.items.push(Item::ClipPop);
                                }
                            }
                        }
                    }
                }
            }
            "input" => {
                let t = self.dom.attr(n, "type").unwrap_or("text").to_ascii_lowercase();
                match t.as_str() {
                    "checkbox" | "radio" => {
                        self.items.push(Item::Check { r: rect, node: n, radio: t == "radio" });
                        self.targets.push((rect, Target::Control(n)));
                    }
                    "submit" | "button" | "reset" => {
                        let label = input_label(self.dom, n);
                        let tw = self.fonts.width(face, &label);
                        let (lh, _) = self.fonts.metrics(face);
                        self.items.push(Item::Text { x: rect.x + (rect.w - tw) / 2, y: rect.y + (rect.h - lh) / 2, face, color: s.color, text: label, deco: 0, width: tw });
                        self.targets.push((rect, Target::Control(n)));
                    }
                    "image" => {
                        if let Some(src) = self.image_src(n) {
                            if let Some((i, Some(_))) = self.pics.find(&src) {
                                self.items.push(Item::Image { r: rect, img: i, cover: false, radius: 0 });
                            }
                        }
                        self.targets.push((rect, Target::Control(n)));
                    }
                    "range" => {
                        let mid = rect.y + rect.h / 2;
                        self.items.push(Item::Fill { r: Rect::new(rect.x, mid - 2, rect.w, 4), color: 0xFFC8_C8CC, radius: 2 });
                        self.items.push(Item::Fill { r: Rect::new(rect.x + rect.w / 2 - 7, mid - 7, 14, 14), color: 0xFF1A_56DB, radius: 7 });
                    }
                    _ => {
                        self.items.push(Item::Field { r: rect, node: n, face, color: s.color, multiline: false });
                        self.targets.push((rect, Target::Control(n)));
                    }
                }
            }
            "textarea" => {
                self.items.push(Item::Field { r: rect, node: n, face, color: s.color, multiline: true });
                self.targets.push((rect, Target::Control(n)));
            }
            "select" => {
                self.items.push(Item::Select { r: rect, node: n, face, color: s.color });
                self.targets.push((rect, Target::Control(n)));
            }
            "progress" | "meter" => {
                let v: f32 = self.dom.attr(n, "value").and_then(|v| v.parse().ok()).unwrap_or(0.0);
                let mx: f32 = self.dom.attr(n, "max").and_then(|v| v.parse().ok()).unwrap_or(1.0);
                self.items.push(Item::Fill { r: rect, color: 0xFFE0_E0E4, radius: 6 });
                let fw = (w * (v / mx.max(0.0001)).clamp(0.0, 1.0)) as i32;
                self.items.push(Item::Fill { r: Rect::new(rect.x, rect.y, fw, rect.h), color: 0xFF1A_56DB, radius: 6 });
            }
            "iframe" | "video" | "embed" | "object" | "canvas" | "audio" => {
                self.items.push(Item::Fill { r: rect, color: 0xFFEE_EEF0, radius: 4 });
                let label = match tag {
                    "video" => "Video",
                    "audio" => "Audio",
                    "canvas" => "",
                    _ => "Contenuto incorporato",
                };
                if !label.is_empty() {
                    let tw = self.fonts.width(face, label);
                    let (lh, _) = self.fonts.metrics(face);
                    self.items.push(Item::Text { x: rect.x + (rect.w - tw) / 2, y: rect.y + (rect.h - lh) / 2, face, color: 0xFF80_8088, text: label.into(), deco: 0, width: tw });
                }
                if tag == "iframe" {
                    if let Some(src) = self.dom.attr(n, "src") {
                        let _ = src;
                        self.targets.push((rect, Target::Link(n)));
                    }
                }
            }
            _ => {}
        }
        h
    }

    // Intrinsic widths.

    /// (min-content, max-content) margin-box widths.
    fn intrinsic(&mut self, n: NodeId) -> (f32, f32) {
        if let Some(v) = self.memo.get(&n) {
            return *v;
        }
        self.depth += 1;
        let v = if self.depth > MAX_DEPTH { (0.0, 0.0) } else { self.intrinsic_uncached(n) };
        self.depth -= 1;
        self.memo.insert(n, v);
        v
    }

    fn intrinsic_uncached(&mut self, n: NodeId) -> (f32, f32) {
        let Some(s) = self.style(n) else { return (0.0, 0.0) };
        if s.display == Display::None {
            return (0.0, 0.0);
        }
        let e = self.edges(n, 0.0);
        let extra = e.h() + e.m[1].max(0.0) + e.m[3].max(0.0);
        if !s.width.has_pct() {
            if let Some(w) = s.width.resolve(0.0) {
                let w = if s.border_box { w.max(e.h()) - e.h() } else { w };
                return (w + extra, w + extra);
            }
        }
        if self.is_replaced(n) {
            let (w, _) = self.replaced_size(n, s, 0.0);
            let (w0, _) = if s.max_width.has_pct() { (0.0, 0.0) } else { (w, 0.0) };
            return (w0 + extra, w + extra);
        }
        let (mut mn, mut mx) = (0.0f32, 0.0f32);
        match s.display {
            Display::Table | Display::InlineTable => {
                for row in self.table_rows(n) {
                    let (mut a, mut b) = (0.0, 0.0);
                    for c in self.kids(row) {
                        if matches!(self.dom.nodes[c].data, Data::Element(_)) {
                            let (cmn, cmx) = self.intrinsic(c);
                            a += cmn + s.border_spacing;
                            b += cmx + s.border_spacing;
                        }
                    }
                    mn = mn.max(a);
                    mx = mx.max(b);
                }
            }
            Display::Flex | Display::InlineFlex if s.flex_row => {
                let kids = self.kids(n);
                let count = kids.len() as f32;
                for c in kids {
                    let (cmn, cmx) = self.child_intrinsic(c);
                    mn = if s.flex_wrap { mn.max(cmn) } else { mn + cmn };
                    mx += cmx;
                }
                mx += s.column_gap * (count - 1.0).max(0.0);
            }
            _ => {
                let (a, b) = self.flow_intrinsic(n);
                mn = a;
                mx = b;
                if matches!(s.display, Display::Grid | Display::InlineGrid) && s.columns.len() > 1 {
                    mx = mx * s.columns.len() as f32 + s.column_gap * (s.columns.len() - 1) as f32;
                }
            }
        }
        if let Some(m) = s.max_width.resolve(0.0).filter(|_| !s.max_width.has_pct()) {
            mx = mx.min(m);
            mn = mn.min(m);
        }
        if let Some(m) = s.min_width.resolve(0.0).filter(|_| !s.min_width.has_pct()) {
            mx = mx.max(m);
            mn = mn.max(m);
        }
        if s.white_space == WhiteSpace::NoWrap || s.white_space == WhiteSpace::Pre {
            mn = mx;
        }
        (mn + extra, mx + extra)
    }

    fn child_intrinsic(&mut self, c: NodeId) -> (f32, f32) {
        match &self.dom.nodes[c].data {
            Data::Text(t) => {
                let p = self.dom.nodes[c].parent.unwrap_or(0);
                let s = self.style(p).cloned().unwrap_or_else(Style::initial);
                self.text_intrinsic(t, &s)
            }
            _ => self.intrinsic(c),
        }
    }

    fn text_intrinsic(&self, t: &str, s: &Style) -> (f32, f32) {
        let face = self.fonts.face(s.font_size, s.bold, s.mono);
        let space = self.fonts.advance(face, ' ') as f32;
        let up;
        let t = if s.transform == Transform::Upper {
            up = t.to_uppercase();
            up.as_str()
        } else {
            t
        };
        if matches!(s.white_space, WhiteSpace::Pre | WhiteSpace::PreWrap) {
            let w = t.split('\n').map(|l| self.fonts.width(face, l)).max().unwrap_or(0) as f32;
            return (if s.white_space == WhiteSpace::Pre { w } else { 0.0 }, w);
        }
        let mut mn = 0.0f32;
        let mut mx = 0.0f32;
        for (i, w) in t.split_ascii_whitespace().enumerate() {
            let ww = self.fonts.width(face, w) as f32;
            mn = mn.max(ww);
            mx += ww + if i > 0 { space } else { 0.0 };
        }
        if t.starts_with(|c: char| c.is_ascii_whitespace()) && !t.trim().is_empty() {
            mx += space;
        }
        if s.white_space == WhiteSpace::NoWrap {
            mn = mx;
        }
        (mn, mx)
    }

    /// Intrinsic widths of a block's mixed content.
    fn flow_intrinsic(&mut self, n: NodeId) -> (f32, f32) {
        let (mut mn, mut mx) = (0.0f32, 0.0f32);
        let mut line = 0.0f32;
        let mut line_min = 0.0f32;
        let mut nowrap_line = false;
        self.inline_intrinsic(n, &mut mn, &mut mx, &mut line, &mut line_min, &mut nowrap_line);
        (mn.max(line_min), mx.max(line))
    }

    fn inline_intrinsic(&mut self, n: NodeId, mn: &mut f32, mx: &mut f32, line: &mut f32, line_min: &mut f32, nowrap: &mut bool) {
        for c in self.kids(n) {
            match &self.dom.nodes[c].data {
                Data::Text(t) => {
                    let p = self.dom.nodes[c].parent.unwrap_or(0);
                    let s = self.style(p).cloned().unwrap_or_else(Style::initial);
                    let (a, b) = self.text_intrinsic(t, &s);
                    *line += b;
                    if s.white_space == WhiteSpace::NoWrap {
                        *line_min += a;
                    } else {
                        *mn = mn.max(*line_min);
                        *line_min = a;
                        *mn = mn.max(a);
                    }
                    let _ = nowrap;
                }
                Data::Element(e) => {
                    let Some(s) = self.style(c) else { continue };
                    if e.tag == "br" {
                        *mx = mx.max(*line);
                        *mn = mn.max(*line_min);
                        *line = 0.0;
                        *line_min = 0.0;
                        continue;
                    }
                    if s.float != Float::None {
                        let (a, b) = self.intrinsic(c);
                        *mn = mn.max(a);
                        *line += b;
                        continue;
                    }
                    if s.display == Display::Inline && !self.is_replaced(c) {
                        let ed = self.edges(c, 0.0);
                        *line += ed.h() + ed.mh();
                        self.inline_intrinsic(c, mn, mx, line, line_min, nowrap);
                        continue;
                    }
                    let (a, b) = self.intrinsic(c);
                    if s.display.is_inline_level() || self.is_replaced(c) {
                        *line += b;
                        *mn = mn.max(a);
                    } else {
                        *mx = mx.max(*line).max(b);
                        *mn = mn.max(*line_min).max(a);
                        *line = 0.0;
                        *line_min = 0.0;
                    }
                }
                Data::Document => {}
            }
        }
    }

    // Flexbox.

    fn flex(&mut self, n: NodeId, s: &Style, x: f32, y: f32, w: f32, fh: Option<f32>) -> f32 {
        let mut kids: Vec<NodeId> = self.kids(n).into_iter().filter(|&k| !matches!(&self.dom.nodes[k].data, Data::Text(t) if t.trim().is_empty())).collect();
        kids.sort_by_key(|&k| self.style(k).map_or(0, |ks| ks.order));
        if s.flex_reverse {
            kids.reverse();
        }
        if !s.flex_row {
            return self.flex_column(n, s, &kids, x, y, w, fh);
        }
        let gap = s.column_gap;
        // (node, base outer width, min outer, grow, shrink)
        let mut items: Vec<(NodeId, f32, f32, f32, f32, Edges)> = Vec::new();
        for &k in &kids {
            if let Data::Text(_) = &self.dom.nodes[k].data {
                let (a, b) = self.child_intrinsic(k);
                items.push((k, b.min(w), a.min(w), 0.0, 1.0, Edges::default()));
                continue;
            }
            let Some(ks) = self.style(k) else { continue };
            let e = self.edges(k, w);
            let (imin, imax) = self.intrinsic(k);
            let base = match &ks.basis {
                Len::Auto | Len::MaxContent | Len::FitContent => match self.specified_width(ks, &e, w) {
                    Some(bw) => bw + e.mh(),
                    None => imax,
                },
                Len::MinContent => imin,
                b => b.resolve(w).map(|v| if ks.border_box { v.max(e.h()) } else { v + e.h() } + e.mh()).unwrap_or(imax),
            };
            let min = match ks.min_width.resolve(w) {
                Some(m) => m + e.mh() + if ks.border_box { 0.0 } else { e.h() },
                None if ks.overflow_hidden => e.h() + e.mh(),
                None => imin.min(base.max(imin.min(w))),
            };
            let base = self.clamp_width(ks, &e, w, base - e.mh()) + e.mh();
            items.push((k, base, min, ks.grow, ks.shrink, e));
        }
        // Lines.
        let mut lines: Vec<Vec<usize>> = Vec::new();
        if s.flex_wrap {
            let mut cur: Vec<usize> = Vec::new();
            let mut used = 0.0;
            for (i, it) in items.iter().enumerate() {
                let add = it.1 + if cur.is_empty() { 0.0 } else { gap };
                if !cur.is_empty() && used + add > w + 0.5 {
                    lines.push(core::mem::take(&mut cur));
                    used = 0.0;
                }
                used += it.1 + if cur.is_empty() { 0.0 } else { gap };
                cur.push(i);
            }
            if !cur.is_empty() {
                lines.push(cur);
            }
        } else {
            lines.push((0..items.len()).collect());
        }
        let mut cy = y;
        let row_gap = s.row_gap;
        for (li, line) in lines.iter().enumerate() {
            if li > 0 {
                cy += row_gap;
            }
            let total: f32 = line.iter().map(|&i| items[i].1).sum::<f32>() + gap * (line.len() as f32 - 1.0).max(0.0);
            let free = w - total;
            let mut sizes: Vec<f32> = line.iter().map(|&i| items[i].1).collect();
            let grow: f32 = line.iter().map(|&i| items[i].3).sum();
            if free > 0.0 && grow > 0.0 {
                for (j, &i) in line.iter().enumerate() {
                    sizes[j] += free * items[i].3 / grow;
                }
            } else if free < 0.0 {
                // Shrink, never below the minimum, in a few passes.
                let mut deficit = -free;
                for _ in 0..4 {
                    let weight: f32 = line.iter().enumerate().filter(|(j, &i)| sizes[*j] > items[i].2 + 0.1).map(|(j, &i)| items[i].4 * sizes[j]).sum();
                    if weight <= 0.0 || deficit <= 0.1 {
                        break;
                    }
                    let mut taken = 0.0;
                    for (j, &i) in line.iter().enumerate() {
                        if sizes[j] > items[i].2 + 0.1 {
                            let cut = (deficit * items[i].4 * sizes[j] / weight).min(sizes[j] - items[i].2);
                            sizes[j] -= cut;
                            taken += cut;
                        }
                    }
                    deficit -= taken;
                }
            }
            let used: f32 = sizes.iter().sum::<f32>() + gap * (line.len() as f32 - 1.0).max(0.0);
            let left = (w - used).max(0.0);
            let k = line.len() as f32;
            let (mut px, extra) = match s.justify {
                Justify::Center => (x + left / 2.0, 0.0),
                Justify::End => (x + left, 0.0),
                Justify::Between if k > 1.0 => (x, left / (k - 1.0)),
                Justify::Around => (x + left / (2.0 * k), left / k),
                Justify::Evenly => (x + left / (k + 1.0), left / (k + 1.0)),
                _ => (x, 0.0),
            };
            // Lay out each item at the top of the line, then align.
            let mut placed: Vec<((usize, usize), f32, Items, bool)> = Vec::new();
            let mut line_h = 0.0f32;
            for (j, &i) in line.iter().enumerate() {
                let (k_node, _, _, _, _, e) = items[i];
                let mark = self.mark_items();
                let ow = sizes[j];
                let h = if let Data::Text(t) = &self.dom.nodes[k_node].data {
                    let t = t.clone();
                    self.anon_text(k_node, &t, px, cy, ow)
                } else {
                    let mut sub = Floats::default();
                    self.block(k_node, px + e.m[3], cy + e.m[0], ow, &mut sub, Some((ow - e.mh()).max(0.0)), None, true) + e.m[0] + e.m[2]
                };
                let ks = self.style(k_node);
                let al = ks.and_then(|ks| ks.align_self).unwrap_or(s.align_items);
                let auto_h = ks.is_none_or(|ks| ks.height.is_auto());
                line_h = line_h.max(h);
                placed.push((mark, h, al, auto_h));
                px += ow + gap + extra;
            }
            if let Some(fh) = fh {
                if lines.len() == 1 {
                    line_h = line_h.max(fh);
                }
            }
            // Stretch: re-lay items with backgrounds to the line's height.
            for (j, &i) in line.iter().enumerate() {
                let (mark, h, al, auto_h) = placed[j];
                let dy = match al {
                    Items::Center => (line_h - h) / 2.0,
                    Items::End => line_h - h,
                    _ => 0.0,
                };
                if dy > 0.0 {
                    let end = placed.get(j + 1).map(|p| p.0).unwrap_or((self.items.len(), self.targets.len()));
                    for it in &mut self.items[mark.0..end.0] {
                        it.shift(0, dy.rnd() as i32);
                    }
                    for t in &mut self.targets[mark.1..end.1] {
                        t.0 = t.0.offset(0, dy.rnd() as i32);
                    }
                }
                if al == Items::Stretch && auto_h && h < line_h - 0.5 {
                    // Grow the item's background to the line's height.
                    let k_node = items[i].0;
                    if let Some(ks) = self.style(k_node) {
                        if ks.background >> 24 != 0 || ks.border.iter().any(|&b| b > 0.0) {
                            let e = items[i].5;
                            let end = placed.get(j + 1).map(|p| p.0.0).unwrap_or(self.items.len());
                            for it in &mut self.items[mark.0..end] {
                                if let Item::Fill { r: rr, .. } | Item::Border { r: rr, .. } = it {
                                    if (rr.h as f32 - (h - e.m[0] - e.m[2])).abs() < 1.5 && rr.y == (cy + e.m[0]).rnd() as i32 {
                                        rr.h = (line_h - e.m[0] - e.m[2]).rnd() as i32;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            cy += line_h;
        }
        cy - y
    }

    fn flex_column(&mut self, _n: NodeId, s: &Style, kids: &[NodeId], x: f32, y: f32, w: f32, fh: Option<f32>) -> f32 {
        let mut cy = y;
        let start = self.mark_items();
        for (i, &k) in kids.iter().enumerate() {
            if i > 0 {
                cy += s.row_gap;
            }
            if let Data::Text(t) = &self.dom.nodes[k].data {
                let t = t.clone();
                cy += self.anon_text(k, &t, x, cy, w);
                continue;
            }
            let Some(ks) = self.style(k) else { continue };
            let e = self.edges(k, w);
            let al = ks.align_self.unwrap_or(s.align_items);
            let mut sub = Floats::default();
            let (bw, bx) = match (al, self.specified_width(ks, &e, w)) {
                (_, Some(bw)) => {
                    let bw = self.clamp_width(ks, &e, w, bw);
                    let free = w - bw - e.mh();
                    let auto_center = ks.margin[1].is_auto() && ks.margin[3].is_auto();
                    (bw, x + e.m[3] + if al == Items::Center || auto_center { free / 2.0 } else if al == Items::End { free } else { 0.0 })
                }
                (Items::Stretch, None) => (self.clamp_width(ks, &e, w, w - e.mh()), x + e.m[3]),
                (al, None) => {
                    let sw = self.shrink_to_fit(k, w);
                    let bw = self.clamp_width(ks, &e, w, sw);
                    let free = w - bw - e.mh();
                    (bw, x + e.m[3] + if al == Items::Center { free / 2.0 } else if al == Items::End { free } else { 0.0 })
                }
            };
            let h = self.block(k, bx, cy + e.m[0], w, &mut sub, Some(bw), None, true);
            cy += h + e.m[0] + e.m[2];
        }
        let content = cy - y;
        if let Some(fh) = fh {
            let free = fh - content;
            if free > 0.0 {
                let dy = match s.justify {
                    Justify::Center => free / 2.0,
                    Justify::End => free,
                    _ => 0.0,
                };
                self.shift(start, 0.0, dy);
            }
            return content.max(fh);
        }
        content
    }

    /// Text directly in a flex or grid container: an anonymous block.
    fn anon_text(&mut self, k: NodeId, t: &str, x: f32, y: f32, w: f32) -> f32 {
        let p = self.dom.nodes[k].parent.unwrap_or(0);
        let Some(s) = self.style(p) else { return 0.0 };
        let saved = core::mem::take(&mut self.inl);
        self.last_space = true;
        let link = self.link_of(p);
        self.text(t, s, link, None);
        let mut fl = Floats::default();
        let end = self.lines(p, x, y, w, &mut fl);
        self.inl = saved;
        end - y
    }

    // Grid.

    fn grid(&mut self, n: NodeId, s: &Style, x: f32, y: f32, w: f32) -> f32 {
        let kids: Vec<NodeId> = self.kids(n).into_iter().filter(|&k| !matches!(&self.dom.nodes[k].data, Data::Text(t) if t.trim().is_empty())).collect();
        let gap = s.column_gap;
        // Column widths.
        let mut tracks = s.columns.clone();
        if tracks.is_empty() {
            tracks.push(Track::Fr(1.0));
        }
        if let Some(Track::Fill(min)) = tracks.iter().find(|t| matches!(t, Track::Fill(_))).cloned() {
            let count = (((w + gap) / (min + gap)).flr() as usize).max(1);
            tracks = alloc::vec![Track::Fr(1.0); count];
        }
        let fixed: f32 = tracks
            .iter()
            .map(|t| match t {
                Track::Px(p) => *p,
                Track::Pct(p) => w * p / 100.0,
                _ => 0.0,
            })
            .sum();
        let frs: f32 = tracks
            .iter()
            .map(|t| match t {
                Track::Fr(f) => *f,
                Track::Auto => 1.0,
                _ => 0.0,
            })
            .sum();
        let cols = tracks.len();
        let free = (w - fixed - gap * (cols as f32 - 1.0)).max(0.0);
        let widths: Vec<f32> = tracks
            .iter()
            .map(|t| match t {
                Track::Px(p) => *p,
                Track::Pct(p) => w * p / 100.0,
                Track::Fr(f) => free * f / frs.max(0.0001),
                Track::Auto => free / frs.max(0.0001),
                Track::Fill(_) => free / cols as f32,
            })
            .collect();
        let mut cy = y;
        let mut col = 0usize;
        let mut row: Vec<((usize, usize), f32, NodeId)> = Vec::new();
        let mut row_h = 0.0f32;
        let finish_row = |this: &mut Self, row: &mut Vec<((usize, usize), f32, NodeId)>, row_h: f32| {
            for (mark, h, k) in row.drain(..) {
                if let Some(ks) = this.style(k) {
                    let al = ks.align_self.unwrap_or(s.align_items);
                    let dy = match al {
                        Items::Center => (row_h - h) / 2.0,
                        Items::End => row_h - h,
                        _ => 0.0,
                    };
                    let _ = (mark, dy);
                }
            }
        };
        for &k in kids.iter() {
            let span = match self.style(k) {
                Some(ks) if ks.span_all => cols,
                Some(ks) => (ks.span as usize).clamp(1, cols),
                None => 1,
            };
            if col + span > cols && col > 0 {
                finish_row(self, &mut row, row_h);
                cy += row_h + s.row_gap;
                row_h = 0.0;
                col = 0;
            }
            let cx = x + widths[..col].iter().sum::<f32>() + gap * col as f32;
            let cw = widths[col..col + span].iter().sum::<f32>() + gap * (span as f32 - 1.0);
            let mark = self.mark_items();
            let h = if let Data::Text(t) = &self.dom.nodes[k].data {
                let t = t.clone();
                self.anon_text(k, &t, cx, cy, cw)
            } else {
                let e = self.edges(k, cw);
                let ks = self.style(k);
                let mut sub = Floats::default();
                let bw = match ks.and_then(|ks| self.specified_width(ks, &e, cw)) {
                    Some(bw) => bw.min(cw - e.mh()),
                    None => cw - e.mh(),
                };
                self.block(k, cx + e.m[3], cy + e.m[0], cw, &mut sub, Some(bw.max(0.0)), None, true) + e.m[0] + e.m[2]
            };
            row.push((mark, h, k));
            row_h = row_h.max(h);
            col += span;
            if col >= cols {
                finish_row(self, &mut row, row_h);
                cy += row_h + s.row_gap;
                row_h = 0.0;
                col = 0;
            }
        }
        if col > 0 {
            finish_row(self, &mut row, row_h);
            cy += row_h;
        } else if cy > y {
            cy -= s.row_gap;
        }
        cy - y
    }

    // Tables.

    fn table_rows(&self, n: NodeId) -> Vec<NodeId> {
        let mut rows = Vec::new();
        let mut groups: Vec<(u8, Vec<NodeId>)> = Vec::new();
        for k in self.kids(n) {
            let Some(ks) = self.style(k) else { continue };
            match ks.display {
                Display::TableRow => groups.push((1, alloc::vec![k])),
                Display::TableRowGroup => {
                    let order = match self.dom.tag(k) {
                        "thead" => 0,
                        "tfoot" => 2,
                        _ => 1,
                    };
                    let rs: Vec<NodeId> = self.kids(k).into_iter().filter(|&r| self.style(r).is_some_and(|rs| rs.display == Display::TableRow)).collect();
                    groups.push((order, rs));
                }
                _ => {}
            }
        }
        groups.sort_by_key(|g| g.0);
        for (_, g) in groups {
            rows.extend(g);
        }
        rows
    }

    fn cells(&self, row: NodeId) -> Vec<NodeId> {
        self.kids(row).into_iter().filter(|&c| matches!(self.dom.nodes[c].data, Data::Element(_))).collect()
    }

    /// Column count and each column's (min, max).
    fn columns(&mut self, n: NodeId) -> Vec<(f32, f32)> {
        let rows = self.table_rows(n);
        let mut cols: Vec<(f32, f32)> = Vec::new();
        let mut spanning: Vec<(usize, usize, f32, f32)> = Vec::new();
        for row in rows {
            let mut ci = 0;
            for c in self.cells(row) {
                let span = self.style(c).map_or(1, |cs| cs.span as usize).max(1);
                let (mn, mx) = self.intrinsic(c);
                if cols.len() < ci + span {
                    cols.resize(ci + span, (0.0, 0.0));
                }
                if span == 1 {
                    cols[ci].0 = cols[ci].0.max(mn);
                    cols[ci].1 = cols[ci].1.max(mx);
                } else {
                    spanning.push((ci, span, mn, mx));
                }
                ci += span;
            }
        }
        for (ci, span, mn, mx) in spanning {
            let (have_mn, have_mx): (f32, f32) = cols[ci..ci + span].iter().fold((0.0, 0.0), |a, c| (a.0 + c.0, a.1 + c.1));
            if mn > have_mn {
                for c in &mut cols[ci..ci + span] {
                    c.0 += (mn - have_mn) / span as f32;
                }
            }
            if mx > have_mx {
                for c in &mut cols[ci..ci + span] {
                    c.1 += (mx - have_mx) / span as f32;
                }
            }
        }
        cols
    }

    fn table_width(&mut self, n: NodeId, s: &Style, e: &Edges, avail: f32) -> f32 {
        let cols = self.columns(n);
        let sp = s.border_spacing;
        let mx: f32 = cols.iter().map(|c| c.1).sum::<f32>() + sp * (cols.len() + 1) as f32;
        let mn: f32 = cols.iter().map(|c| c.0).sum::<f32>() + sp * (cols.len() + 1) as f32;
        (mx.min(avail - e.h()).max(mn) + e.h()).max(0.0)
    }

    fn table(&mut self, n: NodeId, s: &Style, x: f32, y: f32, w: f32) -> f32 {
        let sp = if s.collapse { 0.0 } else { s.border_spacing };
        let cols = self.columns(n);
        let ncols = cols.len().max(1);
        let inner = (w - sp * (ncols + 1) as f32).max(0.0);
        let smin: f32 = cols.iter().map(|c| c.0).sum();
        let smax: f32 = cols.iter().map(|c| c.1).sum();
        let widths: Vec<f32> = if cols.is_empty() {
            alloc::vec![inner]
        } else if smax <= inner {
            // Extra room: shared in proportion to the widest contents.
            let extra = inner - smax;
            cols.iter().map(|c| c.1 + if smax > 0.0 { extra * c.1 / smax } else { extra / ncols as f32 }).collect()
        } else if smin >= inner {
            cols.iter().map(|c| c.0).collect()
        } else {
            let k = (inner - smin) / (smax - smin).max(0.0001);
            cols.iter().map(|c| c.0 + (c.1 - c.0) * k).collect()
        };
        let mut cy = y + sp;
        // A caption on top.
        for k in self.kids(n) {
            if self.style(k).is_some_and(|ks| ks.display == Display::TableCaption) {
                let mut fl = Floats::default();
                let e = self.edges(k, w);
                let h = self.block(k, x + e.m[3], cy + e.m[0], w, &mut fl, None, None, true);
                cy += h + e.m[0] + e.m[2];
            }
        }
        for row in self.table_rows(n) {
            let rs = self.style(row);
            let row_slots = self.deco_start(row);
            let mut ci = 0;
            let mut cx = x + sp;
            let mut row_h = 0.0f32;
            let mut placed: Vec<((usize, usize), f32, NodeId, Vec<usize>, Rect)> = Vec::new();
            for c in self.cells(row) {
                let span = self.style(c).map_or(1, |cs| cs.span as usize).max(1);
                let end = (ci + span).min(widths.len());
                let cw = widths[ci.min(end)..end].iter().sum::<f32>() + sp * (end.saturating_sub(ci) as f32 - 1.0).max(0.0);
                let mark = self.mark_items();
                let mut fl = Floats::default();
                let h = self.block(c, cx, cy, cw, &mut fl, Some(cw), None, true);
                let slots: Vec<usize> = Vec::new();
                placed.push((mark, h, c, slots, r(cx, cy, cw, h)));
                row_h = row_h.max(h);
                cx += cw + sp;
                ci = end;
            }
            if let Some(rs) = rs {
                if let Some(hh) = rs.height.resolve(0.0) {
                    row_h = row_h.max(hh);
                }
            }
            // Cells as tall as the row, contents aligned.
            let ends: Vec<(usize, usize)> = placed.iter().skip(1).map(|p| p.0).chain(core::iter::once((self.items.len(), self.targets.len()))).collect();
            for (j, (mark, h, c, _, rect)) in placed.iter().enumerate() {
                let Some(cs) = self.style(*c) else { continue };
                let free = row_h - h;
                // The cell's own background and border: stretch them.
                for it in &mut self.items[mark.0..ends[j].0] {
                    if let Item::Fill { r: rr, .. } | Item::Border { r: rr, .. } = it {
                        if rr.x == rect.x && rr.y == rect.y && rr.w == rect.w && rr.h == rect.h {
                            rr.h = row_h.rnd() as i32;
                        }
                    }
                }
                if free > 0.5 {
                    let dy = match cs.vertical_align {
                        VAlign::Middle => free / 2.0,
                        VAlign::Bottom => free,
                        _ => 0.0,
                    };
                    if dy > 0.0 {
                        let dy = dy.rnd() as i32;
                        for it in &mut self.items[mark.0..ends[j].0] {
                            let is_cell_box = matches!(it, Item::Fill { r: rr, .. } | Item::Border { r: rr, .. } if rr.x == rect.x && rr.y == rect.y && rr.w == rect.w);
                            if !is_cell_box {
                                it.shift(0, dy);
                            }
                        }
                        for t in &mut self.targets[mark.1..ends[j].1] {
                            t.0 = t.0.offset(0, dy);
                        }
                    }
                }
            }
            self.deco_end(&row_slots, r(x, cy, w, row_h));
            cy += row_h + sp;
        }
        cy - y
    }
}

fn extend(v: &mut Vec<(NodeId, f32, f32)>, n: NodeId, x0: f32, x1: f32) {
    match v.iter_mut().find(|e| e.0 == n) {
        Some(e) => {
            e.1 = e.1.min(x0);
            e.2 = e.2.max(x1);
        }
        None => v.push((n, x0, x1)),
    }
}

fn clone_word(i: &Inl) -> Inl {
    match i {
        Inl::Word { text, w, face, color, deco, owner, link, space, lh, asc, desc, nowrap, visible } => Inl::Word {
            text: text.clone(),
            w: *w,
            face: *face,
            color: *color,
            deco: *deco,
            owner: *owner,
            link: *link,
            space: *space,
            lh: *lh,
            asc: *asc,
            desc: *desc,
            nowrap: *nowrap,
            visible: *visible,
        },
        _ => Inl::Break { lh: 0.0 },
    }
}

fn capitalize(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    let mut start = true;
    for c in t.chars() {
        if start && c.is_alphabetic() {
            out.extend(c.to_uppercase());
            start = false;
        } else {
            out.push(c);
            if c.is_whitespace() {
                start = true;
            }
        }
    }
    out
}

fn roman(mut n: u32) -> String {
    let t = [(1000, "M"), (900, "CM"), (500, "D"), (400, "CD"), (100, "C"), (90, "XC"), (50, "L"), (40, "XL"), (10, "X"), (9, "IX"), (5, "V"), (4, "IV"), (1, "I")];
    let mut s = String::new();
    for (v, r) in t {
        while n >= v {
            s.push_str(r);
            n -= v;
        }
    }
    s
}

/// The label of an input button.
pub fn input_label(dom: &Dom, n: NodeId) -> String {
    match dom.attr(n, "value") {
        Some(v) if !v.is_empty() => v.to_string(),
        _ => match dom.attr(n, "type").unwrap_or("").to_ascii_lowercase().as_str() {
            "reset" => "Reimposta".into(),
            "button" => String::new(),
            _ => "Invia".into(),
        },
    }
}

/// The picture of an <img>: its src, or the lazy-loading attributes pages
/// use instead, or the first candidate of srcset.
pub fn image_src(dom: &Dom, n: NodeId) -> Option<String> {
    let attr = |k: &str| dom.attr(n, k).map(str::trim).filter(|v| !v.is_empty());
    let src = attr("src");
    let placeholder = src.is_none_or(|s| s.starts_with("data:") || s.ends_with("blank.gif") || s.contains("placeholder") || s.contains("spacer"));
    if placeholder {
        for k in ["data-src", "data-lazy-src", "data-original", "data-url", "data-lazy"] {
            if let Some(v) = attr(k) {
                return Some(v.to_string());
            }
        }
        for k in ["srcset", "data-srcset"] {
            if let Some(set) = attr(k) {
                return srcset_pick(set);
            }
        }
        // <picture>: a <source> before the image.
        if let Some(p) = dom.parent_element(n).filter(|&p| dom.tag(p) == "picture") {
            for s in dom.element_children(p) {
                if dom.tag(s) == "source" {
                    let t = dom.attr(s, "type").unwrap_or("");
                    if t.contains("webp") || t.contains("avif") {
                        continue;
                    }
                    if let Some(set) = dom.attr(s, "srcset") {
                        return srcset_pick(set);
                    }
                }
            }
        }
    }
    src.filter(|s| !s.starts_with("data:")).map(String::from)
}

/// A candidate of a srcset near 1x or 800px wide.
fn srcset_pick(set: &str) -> Option<String> {
    let mut best: Option<(f32, String)> = None;
    for cand in set.split(',') {
        let mut parts = cand.split_whitespace();
        let Some(url) = parts.next() else { continue };
        let d = parts.next().unwrap_or("1x");
        let score = if let Some(w) = d.strip_suffix('w') {
            (w.parse::<f32>().unwrap_or(800.0) - 800.0).abs()
        } else {
            (d.trim_end_matches('x').parse::<f32>().unwrap_or(1.0) - 1.0).abs() * 1000.0
        };
        if best.as_ref().is_none_or(|b| score < b.0) {
            best = Some((score, url.to_string()));
        }
    }
    best.map(|b| b.1)
}

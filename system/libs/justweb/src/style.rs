//! The cascade: which declarations apply to each element, and the values
//! they compute to (lengths in pixels, colours as ARGB).

use crate::css::{self, Decl, Stylesheet, Viewport};
use crate::dom::{Data, Dom, NodeId};
use alloc::collections::BTreeMap;
use alloc::sync::Arc as Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Display {
    None,
    Block,
    Inline,
    InlineBlock,
    ListItem,
    Table,
    InlineTable,
    TableRow,
    TableRowGroup,
    TableCell,
    TableCaption,
    Flex,
    InlineFlex,
    Grid,
    InlineGrid,
    Contents,
}

impl Display {
    pub fn is_inline_level(self) -> bool {
        matches!(self, Display::Inline | Display::InlineBlock | Display::InlineFlex | Display::InlineGrid | Display::InlineTable)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Position {
    Static,
    Relative,
    Absolute,
    Fixed,
    Sticky,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Float {
    None,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
    Justify,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WhiteSpace {
    Normal,
    NoWrap,
    Pre,
    PreWrap,
    PreLine,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListStyle {
    None,
    Disc,
    Circle,
    Square,
    Decimal,
    LowerAlpha,
    UpperAlpha,
    LowerRoman,
    UpperRoman,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transform {
    None,
    Upper,
    Lower,
    Capitalize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Justify {
    Start,
    Center,
    End,
    Between,
    Around,
    Evenly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Items {
    Stretch,
    Start,
    Center,
    End,
    Baseline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VAlign {
    Baseline,
    Middle,
    Top,
    Bottom,
    TextTop,
    Sub,
    Super,
}

/// A length before layout: pixels, a percentage of the containing block,
/// or a mix of both (calc), and the keywords.
#[derive(Clone, Debug, PartialEq)]
pub enum Len {
    Auto,
    None,
    Px(f32),
    Pct(f32),
    Calc(f32, f32),
    Min(Vec<Len>),
    Max(Vec<Len>),
    MinContent,
    MaxContent,
    FitContent,
}

impl Len {
    /// Pixels against a base for percentages; `None` for auto and the like.
    pub fn resolve(&self, base: f32) -> Option<f32> {
        match self {
            Len::Px(p) => Some(*p),
            Len::Pct(p) => Some(base * p / 100.0),
            Len::Calc(px, pct) => Some(px + base * pct / 100.0),
            Len::Min(v) => v.iter().filter_map(|l| l.resolve(base)).reduce(f32::min),
            Len::Max(v) => v.iter().filter_map(|l| l.resolve(base)).reduce(f32::max),
            _ => None,
        }
    }

    pub fn or0(&self, base: f32) -> f32 {
        self.resolve(base).unwrap_or(0.0)
    }

    pub fn is_auto(&self) -> bool {
        matches!(self, Len::Auto)
    }

    /// Depends on the containing block's size.
    pub fn has_pct(&self) -> bool {
        match self {
            Len::Pct(_) => true,
            Len::Calc(_, p) => *p != 0.0,
            Len::Min(v) | Len::Max(v) => v.iter().any(Len::has_pct),
            _ => false,
        }
    }
}

/// A grid track.
#[derive(Clone, Debug, PartialEq)]
pub enum Track {
    Px(f32),
    Pct(f32),
    Fr(f32),
    Auto,
    /// repeat(auto-fill / auto-fit, minmax(min, …)): as many as fit.
    Fill(f32),
}

#[derive(Clone, Debug)]
pub struct Style {
    pub display: Display,
    pub position: Position,
    pub float: Float,
    pub clear: Float,
    pub clear_both: bool,
    pub color: u32,
    pub background: u32,
    pub background_image: Option<String>,
    /// background-size: cover (true) or contain/auto.
    pub background_cover: bool,
    pub font_size: f32,
    pub bold: bool,
    pub italic: bool,
    pub mono: bool,
    pub line_height: Option<f32>,
    pub text_align: Align,
    pub underline: bool,
    pub strike: bool,
    pub transform: Transform,
    pub white_space: WhiteSpace,
    pub margin: [Len; 4],
    pub padding: [Len; 4],
    pub border: [f32; 4],
    pub border_color: [u32; 4],
    pub radius: f32,
    pub width: Len,
    pub height: Len,
    pub min_width: Len,
    pub max_width: Len,
    pub min_height: Len,
    pub max_height: Len,
    pub border_box: bool,
    pub list_style: ListStyle,
    pub list_inside: bool,
    pub visible: bool,
    pub opacity: f32,
    pub flex_row: bool,
    pub flex_reverse: bool,
    pub flex_wrap: bool,
    pub justify: Justify,
    pub align_items: Items,
    pub align_self: Option<Items>,
    pub row_gap: f32,
    pub column_gap: f32,
    pub grow: f32,
    pub shrink: f32,
    pub basis: Len,
    pub order: i32,
    pub columns: Vec<Track>,
    /// grid-column: spans every column ("1 / -1").
    pub span_all: bool,
    pub span: u32,
    pub vertical_align: VAlign,
    pub text_indent: f32,
    pub overflow_hidden: bool,
    pub inset: [Len; 4],
    pub letter_spacing: f32,
    pub border_spacing: f32,
    pub collapse: bool,
    pub content_visibility_hidden: bool,
    /// z-index below zero: behind the content of its containing block.
    pub z_below: bool,
    /// transform: translate(x, y); percentages are of the box itself.
    pub translate: [Len; 2],
    pub object_fit_cover: bool,
    pub aspect_ratio: Option<f32>,
    pub cursor_pointer: bool,
    vars: Rc<BTreeMap<String, String>>,
}

pub const ROOT_FONT: f32 = 16.0;

impl Style {
    pub fn initial() -> Style {
        Style {
            display: Display::Inline,
            position: Position::Static,
            float: Float::None,
            clear: Float::None,
            clear_both: false,
            color: 0xFF20_2124,
            background: 0,
            background_image: None,
            background_cover: false,
            font_size: ROOT_FONT,
            bold: false,
            italic: false,
            mono: false,
            line_height: None,
            text_align: Align::Left,
            underline: false,
            strike: false,
            transform: Transform::None,
            white_space: WhiteSpace::Normal,
            margin: [Len::Px(0.0), Len::Px(0.0), Len::Px(0.0), Len::Px(0.0)],
            padding: [Len::Px(0.0), Len::Px(0.0), Len::Px(0.0), Len::Px(0.0)],
            border: [0.0; 4],
            border_color: [0xFF00_0000; 4],
            radius: 0.0,
            width: Len::Auto,
            height: Len::Auto,
            min_width: Len::Auto,
            max_width: Len::None,
            min_height: Len::Auto,
            max_height: Len::None,
            border_box: false,
            list_style: ListStyle::Disc,
            list_inside: false,
            visible: true,
            opacity: 1.0,
            flex_row: true,
            flex_reverse: false,
            flex_wrap: false,
            justify: Justify::Start,
            align_items: Items::Stretch,
            align_self: None,
            row_gap: 0.0,
            column_gap: 0.0,
            grow: 0.0,
            shrink: 1.0,
            basis: Len::Auto,
            order: 0,
            columns: Vec::new(),
            span_all: false,
            span: 1,
            vertical_align: VAlign::Baseline,
            text_indent: 0.0,
            overflow_hidden: false,
            inset: [Len::Auto, Len::Auto, Len::Auto, Len::Auto],
            letter_spacing: 0.0,
            border_spacing: 0.0,
            collapse: false,
            content_visibility_hidden: false,
            z_below: false,
            translate: [Len::Auto, Len::Auto],
            object_fit_cover: false,
            aspect_ratio: None,
            cursor_pointer: false,
            vars: Rc::new(BTreeMap::new()),
        }
    }

    /// The style a child starts from: the inherited properties.
    fn inherit(&self) -> Style {
        let mut s = Style::initial();
        s.color = self.color;
        s.font_size = self.font_size;
        s.bold = self.bold;
        s.italic = self.italic;
        s.mono = self.mono;
        s.line_height = self.line_height;
        s.text_align = self.text_align;
        s.transform = self.transform;
        s.white_space = self.white_space;
        s.list_style = self.list_style;
        s.list_inside = self.list_inside;
        s.visible = self.visible;
        s.letter_spacing = self.letter_spacing;
        s.border_spacing = self.border_spacing;
        s.collapse = self.collapse;
        s.cursor_pointer = self.cursor_pointer;
        s.vars = self.vars.clone();
        s
    }

    /// Line height in pixels.
    pub fn line(&self) -> f32 {
        self.line_height.unwrap_or(self.font_size * 1.3)
    }
}

/// The browser's own stylesheet.
pub const UA_CSS: &str = r#"
html, address, blockquote, body, center, dd, details, dialog, dir, div, dl, dt, fieldset, figcaption, figure, footer, form, h1, h2, h3, h4, h5, h6, header, hgroup, hr, legend, listing, main, menu, nav, ol, p, plaintext, pre, search, section, summary, ul, xmp, article, aside, optgroup { display: block }
head, link, meta, script, style, title, template, base, datalist, param, noembed, noframes, area, [hidden], dialog:not([open]), input[type=hidden], rp, source, track, map, colgroup, col { display: none }
body { margin: 8px; line-height: 1.35 }
p, dl, multicol, blockquote, figure, menu, ul, ol { margin-top: 1em; margin-bottom: 1em }
pre, listing, xmp, plaintext { margin-top: 1em; margin-bottom: 1em; white-space: pre; font-family: monospace }
ul ul, ul ol, ol ul, ol ol, ul menu, menu ul { margin-top: 0; margin-bottom: 0 }
h1 { font-size: 2em; margin: .67em 0; font-weight: bold }
h2 { font-size: 1.5em; margin: .83em 0; font-weight: bold }
h3 { font-size: 1.17em; margin: 1em 0; font-weight: bold }
h4 { margin: 1.33em 0; font-weight: bold }
h5 { font-size: .83em; margin: 1.67em 0; font-weight: bold }
h6 { font-size: .67em; margin: 2.33em 0; font-weight: bold }
ul, ol, menu, dir { padding-left: 40px }
li { display: list-item }
ol { list-style-type: decimal }
ul ul, ol ul { list-style-type: circle }
ul ul ul, ol ul ul { list-style-type: square }
dd { margin-left: 40px }
blockquote, figure { margin-left: 40px; margin-right: 40px }
b, strong, th, dt { font-weight: bold }
i, em, cite, var, dfn, address { font-style: italic }
code, kbd, samp, tt { font-family: monospace }
a:link { color: #1a56db; text-decoration: underline; cursor: pointer }
u, ins { text-decoration: underline }
s, strike, del { text-decoration: line-through }
small, sub, sup { font-size: smaller }
big { font-size: larger }
sub { vertical-align: sub }
sup { vertical-align: super }
mark { background-color: #fde68a; color: black }
center { text-align: center }
hr { border: 1px solid #c8c8cc; margin: .6em 0; height: 0 }
table { display: table; border-spacing: 2px }
caption { display: table-caption; text-align: center }
thead, tbody, tfoot { display: table-row-group }
tr { display: table-row }
td, th { display: table-cell; padding: 1px; vertical-align: middle }
th { text-align: center }
img, svg, video, canvas, iframe, object, embed { display: inline-block }
iframe { border: 2px inset #ccc }
input, select, textarea, button { display: inline-block; font-size: 13.333px; color: #202124 }
input, textarea, select { border: 1px solid #8f8f9d; border-radius: 4px; padding: 3px 6px; background-color: white }
button, input[type=submit], input[type=button], input[type=reset] { border: 1px solid #8f8f9d; border-radius: 4px; padding: 3px 10px; background-color: #e9e9ed; text-align: center; cursor: pointer }
textarea { white-space: pre-wrap; font-family: monospace }
fieldset { border: 1px solid #c0c0c0; margin: 0 2px; padding: .35em .75em .6em }
legend { padding: 0 2px }
summary { font-weight: bold }
abbr[title] { text-decoration: underline }
ruby { display: inline }
rt { font-size: 50% }
"#;

/// The computed styles of a document.
pub struct Styles {
    pub styles: Vec<Option<Style>>,
}

struct Index<'a> {
    by_id: BTreeMap<&'a str, Vec<(usize, usize)>>,
    by_class: BTreeMap<&'a str, Vec<(usize, usize)>>,
    by_tag: BTreeMap<&'a str, Vec<(usize, usize)>>,
    rest: Vec<(usize, usize)>,
}

/// Computes every element's style. `sheets` are in cascade order
/// (the UA sheet first).
pub fn compute(dom: &Dom, sheets: &[(bool, &Stylesheet)], vp: Viewport) -> Styles {
    // All rules that hold for this screen, indexed by their subject.
    let mut rules: Vec<(bool, &css::Rule)> = Vec::new();
    for (ua, sheet) in sheets {
        for r in &sheet.rules {
            if r.media.iter().all(|m| css::media_ok(m, vp)) {
                rules.push((*ua, r));
            }
        }
    }
    let mut ix = Index { by_id: BTreeMap::new(), by_class: BTreeMap::new(), by_tag: BTreeMap::new(), rest: Vec::new() };
    for (ri, (_, r)) in rules.iter().enumerate() {
        for (si, s) in r.selectors.iter().enumerate() {
            let c = s.subject();
            let key = (ri, si);
            if let Some(id) = &c.id {
                ix.by_id.entry(id.as_str()).or_default().push(key);
            } else if let Some(cl) = c.classes.first() {
                ix.by_class.entry(cl.as_str()).or_default().push(key);
            } else if let Some(t) = &c.tag {
                ix.by_tag.entry(t.as_str()).or_default().push(key);
            } else {
                ix.rest.push(key);
            }
        }
    }
    let mut out: Vec<Option<Style>> = alloc::vec![None; dom.nodes.len()];
    let root = Style::initial();
    // Parents come before their children in the node vector.
    for id in 0..dom.nodes.len() {
        let Data::Element(e) = &dom.nodes[id].data else { continue };
        let parent = dom.nodes[id].parent.and_then(|p| out[p].as_ref()).unwrap_or(&root);
        let mut cands: Vec<(usize, usize)> = Vec::new();
        if let Some(i) = e.attr("id") {
            if let Some(v) = ix.by_id.get(i) {
                cands.extend_from_slice(v);
            }
        }
        if let Some(cl) = e.attr("class") {
            for c in cl.split_ascii_whitespace() {
                if let Some(v) = ix.by_class.get(c) {
                    cands.extend_from_slice(v);
                }
            }
        }
        if let Some(v) = ix.by_tag.get(e.tag.as_str()) {
            cands.extend_from_slice(v);
        }
        cands.extend_from_slice(&ix.rest);
        // (rank, specificity, order, decl)
        let mut matched: Vec<(u8, u32, usize, &Decl)> = Vec::new();
        cands.sort_unstable();
        cands.dedup_by_key(|k| k.0);
        for (ri, _) in cands {
            let (ua, r) = rules[ri];
            let spec = r.selectors.iter().filter(|s| css::matches(dom, id, s)).map(|s| s.specificity).max();
            let Some(spec) = spec else { continue };
            for d in &r.decls {
                let rank = match (ua, d.important) {
                    (true, false) => 0,
                    (false, false) => 1,
                    (false, true) => 3,
                    (true, true) => 4,
                };
                matched.push((rank, spec, ri, d));
            }
        }
        let inline: Vec<Decl> = e.attr("style").map(|s| css::parse_block(s).0).unwrap_or_default();
        for d in &inline {
            matched.push((if d.important { 3 } else { 2 }, u32::MAX, usize::MAX, d));
        }
        matched.sort_by(|a, b| (a.0, a.1, a.2).cmp(&(b.0, b.1, b.2)));
        let mut s = parent.inherit();
        presentational(dom, id, &e.tag, &mut s, parent);
        // Custom properties first, then the rest with var() substituted.
        let mut vars: Option<BTreeMap<String, String>> = None;
        for (_, _, _, d) in &matched {
            if d.name.starts_with("--") {
                vars.get_or_insert_with(|| (*s.vars).clone()).insert(d.name.clone(), d.value.clone());
            }
        }
        if let Some(v) = vars {
            s.vars = Rc::new(v);
        }
        let mut decls: BTreeMap<String, String> = BTreeMap::new();
        let mut order: Vec<String> = Vec::new();
        for (_, _, _, d) in &matched {
            if d.name.starts_with("--") {
                continue;
            }
            let v = if d.value.contains("var(") { substitute(&d.value, &s.vars, 0) } else { d.value.clone() };
            for (k, v) in expand(&d.name, &v) {
                if !decls.contains_key(&k) {
                    order.push(k.clone());
                }
                decls.insert(k, v);
            }
        }
        // font-size first: em units depend on it.
        if let Some(v) = decls.get("font-size") {
            if let Some(px) = font_size(v, parent.font_size) {
                s.font_size = px;
            }
        }
        for k in &order {
            if k == "font-size" {
                continue;
            }
            apply(&mut s, k, &decls[k], parent);
        }
        if s.position == Position::Absolute || s.position == Position::Fixed {
            s.float = Float::None;
        }
        out[id] = Some(s);
    }
    Styles { styles: out }
}

/// HTML attributes that act as style (width, bgcolor, align…).
fn presentational(dom: &Dom, id: NodeId, tag: &str, s: &mut Style, _parent: &Style) {
    let attr = |n: &str| dom.attr(id, n);
    let len = |v: &str| -> Option<Len> {
        let v = v.trim();
        if let Some(p) = v.strip_suffix('%') {
            return p.trim().parse().ok().map(Len::Pct);
        }
        v.trim_end_matches("px").trim().parse().ok().map(Len::Px)
    };
    if matches!(tag, "img" | "table" | "td" | "th" | "col" | "iframe" | "video" | "canvas" | "svg" | "input" | "embed" | "object" | "hr") {
        if let Some(w) = attr("width").and_then(len) {
            s.width = w;
        }
        if let Some(h) = attr("height").and_then(len) {
            if tag != "td" && tag != "th" {
                s.height = h;
            }
        }
    }
    if let Some(c) = attr("bgcolor").and_then(|c| parse_color(c, s.color)) {
        s.background = c;
    }
    if tag == "font" {
        if let Some(c) = attr("color").and_then(|c| parse_color(c, s.color)) {
            s.color = c;
        }
        if let Some(sz) = attr("size").and_then(|v| v.trim().parse::<i32>().ok()) {
            s.font_size = [10.0, 13.0, 16.0, 18.0, 24.0, 32.0, 48.0][(sz.clamp(1, 7) - 1) as usize];
        }
    }
    if let Some(a) = attr("align") {
        let a = a.to_ascii_lowercase();
        if matches!(tag, "img" | "table") {
            match a.as_str() {
                "left" if tag == "img" => s.float = Float::Left,
                "right" => s.float = Float::Right,
                "center" if tag == "table" => {
                    s.margin[1] = Len::Auto;
                    s.margin[3] = Len::Auto;
                }
                _ => {}
            }
        } else {
            s.text_align = match a.as_str() {
                "center" | "middle" => Align::Center,
                "right" => Align::Right,
                "justify" => Align::Justify,
                _ => Align::Left,
            };
        }
    }
    if matches!(tag, "td" | "th") {
        if let Some(v) = attr("valign") {
            s.vertical_align = match v.to_ascii_lowercase().as_str() {
                "top" => VAlign::Top,
                "bottom" => VAlign::Bottom,
                _ => VAlign::Middle,
            };
        }
        if attr("nowrap").is_some() {
            s.white_space = WhiteSpace::NoWrap;
        }
        if let Some(n) = attr("colspan").and_then(|v| v.trim().parse::<u32>().ok()) {
            s.span = n.clamp(1, 100);
        }
        // Padding and borders come from the table.
        if let Some(table) = dom.closest(id, "table") {
            if let Some(p) = dom.attr(table, "cellpadding").and_then(|v| v.trim().parse::<f32>().ok()) {
                s.padding = [Len::Px(p), Len::Px(p), Len::Px(p), Len::Px(p)];
            }
            if dom.attr(table, "border").and_then(|v| v.trim().parse::<f32>().ok()).is_some_and(|b| b > 0.0) {
                s.border = [1.0; 4];
                s.border_color = [0xFF80_8080; 4];
            }
        }
    }
    if tag == "table" {
        if let Some(b) = attr("border").and_then(|v| v.trim().parse::<f32>().ok()) {
            s.border = [b; 4];
            s.border_color = [0xFF80_8080; 4];
        }
        if let Some(sp) = attr("cellspacing").and_then(|v| v.trim().parse::<f32>().ok()) {
            s.border_spacing = sp;
        }
    }
    if tag == "ol" {
        if let Some(t) = attr("type") {
            s.list_style = match t {
                "a" => ListStyle::LowerAlpha,
                "A" => ListStyle::UpperAlpha,
                "i" => ListStyle::LowerRoman,
                "I" => ListStyle::UpperRoman,
                _ => ListStyle::Decimal,
            };
        }
    }
    if tag == "body" {
        if let Some(c) = attr("text").and_then(|c| parse_color(c, s.color)) {
            s.color = c;
        }
    }
}

/// Replaces var(--x, fallback).
fn substitute(v: &str, vars: &BTreeMap<String, String>, depth: u32) -> String {
    if depth > 8 {
        return String::new();
    }
    let mut out = String::new();
    let mut rest = v;
    while let Some(p) = rest.find("var(") {
        out.push_str(&rest[..p]);
        let inner_start = p + 4;
        let b = rest.as_bytes();
        let mut d = 1;
        let mut i = inner_start;
        while i < b.len() {
            match b[i] {
                b'(' => d += 1,
                b')' => {
                    d -= 1;
                    if d == 0 {
                        break;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let inner = &rest[inner_start..i.min(rest.len())];
        let (name, fallback) = match inner.find(',') {
            Some(c) => (inner[..c].trim(), Some(inner[c + 1..].trim())),
            None => (inner.trim(), None),
        };
        match vars.get(name) {
            Some(val) if !val.trim().is_empty() => out.push_str(&substitute(val, vars, depth + 1)),
            _ => {
                if let Some(f) = fallback {
                    out.push_str(&substitute(f, vars, depth + 1));
                }
            }
        }
        rest = &rest[(i + 1).min(rest.len())..];
    }
    out.push_str(rest);
    out
}

/// Splits on spaces at the top level (not inside parentheses).
pub fn tokens(v: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut start = None;
    for (i, c) in v.char_indices() {
        match c {
            '(' => {
                depth += 1;
                start.get_or_insert(i);
            }
            ')' => depth -= 1,
            c if c.is_whitespace() && depth == 0 => {
                if let Some(s) = start.take() {
                    out.push(&v[s..i]);
                }
            }
            _ => {
                start.get_or_insert(i);
            }
        }
    }
    if let Some(s) = start {
        out.push(&v[s..]);
    }
    out
}

fn four(v: &str) -> [String; 4] {
    let t = tokens(v);
    let g = |i: usize| t.get(i).map(|s| s.to_string());
    match t.len() {
        0 => Default::default(),
        1 => [g(0).unwrap(), g(0).unwrap(), g(0).unwrap(), g(0).unwrap()],
        2 => [g(0).unwrap(), g(1).unwrap(), g(0).unwrap(), g(1).unwrap()],
        3 => [g(0).unwrap(), g(1).unwrap(), g(2).unwrap(), g(1).unwrap()],
        _ => [g(0).unwrap(), g(1).unwrap(), g(2).unwrap(), g(3).unwrap()],
    }
}

const SIDES: [&str; 4] = ["top", "right", "bottom", "left"];

/// Shorthands into longhands.
/// `light-dark(<light>, <dark>)` → `<light>` (pages are shown light).
fn light_only(v: &str) -> String {
    let mut s = v.to_string();
    while let Some(at) = s.to_ascii_lowercase().find("light-dark(") {
        let open = at + "light-dark(".len();
        let (mut depth, mut comma, mut close) = (0, None, None);
        for (i, c) in s[open..].char_indices() {
            match c {
                '(' => depth += 1,
                ')' if depth == 0 => {
                    close = Some(open + i);
                    break;
                }
                ')' => depth -= 1,
                ',' if depth == 0 && comma.is_none() => comma = Some(open + i),
                _ => {}
            }
        }
        let Some(close) = close else { break };
        let light = s[open..comma.unwrap_or(close)].trim().to_string();
        s.replace_range(at..=close, &light);
    }
    s
}

fn expand(name: &str, v: &str) -> Vec<(String, String)> {
    let light = light_only(v);
    let v = light.as_str();
    let mut out = Vec::new();
    let mut push = |k: &str, v: &str| out.push((k.to_string(), v.to_string()));
    match name {
        "margin" | "padding" => {
            for (s, x) in SIDES.iter().zip(four(v)) {
                push(&alloc::format!("{name}-{s}"), &x);
            }
        }
        "inset" => {
            for (s, x) in SIDES.iter().zip(four(v)) {
                push(s, &x);
            }
        }
        "margin-inline" | "padding-inline" => {
            let t = tokens(v);
            let base = name.trim_end_matches("-inline");
            push(&alloc::format!("{base}-left"), t.first().copied().unwrap_or("0"));
            push(&alloc::format!("{base}-right"), t.get(1).or(t.first()).copied().unwrap_or("0"));
        }
        "margin-block" | "padding-block" => {
            let t = tokens(v);
            let base = name.trim_end_matches("-block");
            push(&alloc::format!("{base}-top"), t.first().copied().unwrap_or("0"));
            push(&alloc::format!("{base}-bottom"), t.get(1).or(t.first()).copied().unwrap_or("0"));
        }
        "margin-inline-start" => push("margin-left", v),
        "margin-inline-end" => push("margin-right", v),
        "margin-block-start" => push("margin-top", v),
        "margin-block-end" => push("margin-bottom", v),
        "padding-inline-start" => push("padding-left", v),
        "padding-inline-end" => push("padding-right", v),
        "padding-block-start" => push("padding-top", v),
        "padding-block-end" => push("padding-bottom", v),
        "inline-size" => push("width", v),
        "block-size" => push("height", v),
        "max-inline-size" => push("max-width", v),
        "min-block-size" => push("min-height", v),
        "border" | "border-top" | "border-right" | "border-bottom" | "border-left" | "border-block" | "border-inline" | "border-block-end"
        | "border-block-start" | "border-inline-start" | "border-inline-end" => {
            let sides: &[&str] = match name {
                "border" => &SIDES,
                "border-block" => &["top", "bottom"],
                "border-inline" => &["left", "right"],
                "border-block-end" => &["bottom"],
                "border-block-start" => &["top"],
                "border-inline-start" => &["left"],
                "border-inline-end" => &["right"],
                n => core::slice::from_ref(SIDES.iter().find(|s| n.ends_with(*s)).unwrap()),
            };
            let (mut width, mut style, mut color) = ("medium".to_string(), "none".to_string(), "currentcolor".to_string());
            for t in tokens(v) {
                let tl = t.to_ascii_lowercase();
                if matches!(tl.as_str(), "none" | "hidden" | "solid" | "dashed" | "dotted" | "double" | "groove" | "ridge" | "inset" | "outset") {
                    style = tl;
                } else if tl.starts_with(|c: char| c.is_ascii_digit() || c == '.') || matches!(tl.as_str(), "thin" | "medium" | "thick") {
                    width = tl;
                } else {
                    color = t.to_string();
                }
            }
            if v.trim() == "0" || v.trim() == "none" {
                style = "none".into();
            }
            for s in sides {
                push(&alloc::format!("border-{s}-width"), &width);
                push(&alloc::format!("border-{s}-style"), &style);
                push(&alloc::format!("border-{s}-color"), &color);
            }
        }
        "border-width" | "border-style" | "border-color" => {
            let what = &name[7..];
            for (s, x) in SIDES.iter().zip(four(v)) {
                push(&alloc::format!("border-{s}-{what}"), &x);
            }
        }
        "background" => {
            // The colour is the last layer's; an image is the first url().
            let last = v.rsplit(',').next().unwrap_or(v);
            let mut color = "transparent".to_string();
            for t in tokens(last) {
                if parse_color(t, 0).is_some() {
                    color = t.to_string();
                }
            }
            if let Some(p) = v.find("url(") {
                push("background-image", &v[p..]);
            } else if v.contains("gradient(") {
                // A gradient: its first colour stands for it.
                if let Some(c) = gradient_color(v) {
                    color = c;
                }
                push("background-image", "none");
            } else {
                push("background-image", "none");
            }
            if v.contains("cover") {
                push("background-size", "cover");
            }
            push("background-color", &color);
        }
        "font" => {
            let t = tokens(v);
            for (i, x) in t.iter().enumerate() {
                let xl = x.to_ascii_lowercase();
                if matches!(xl.as_str(), "bold" | "bolder" | "600" | "700" | "800" | "900") {
                    push("font-weight", "bold");
                } else if xl == "italic" || xl == "oblique" {
                    push("font-style", "italic");
                } else if xl.starts_with(|c: char| c.is_ascii_digit() || c == '.') || matches!(xl.as_str(), "small" | "medium" | "large" | "x-large" | "xx-large" | "x-small" | "smaller" | "larger") {
                    let (size, lh) = x.split_once('/').unwrap_or((x, ""));
                    if !(xl.len() == 3 && xl.ends_with("00")) {
                        push("font-size", size);
                        if !lh.is_empty() {
                            push("line-height", lh);
                        }
                        let fam = t[i + 1..].join(" ");
                        push("font-family", &fam);
                        break;
                    }
                }
            }
        }
        "flex" => {
            let t = tokens(v);
            match t.as_slice() {
                ["none"] => {
                    push("flex-grow", "0");
                    push("flex-shrink", "0");
                    push("flex-basis", "auto");
                }
                ["auto"] => {
                    push("flex-grow", "1");
                    push("flex-shrink", "1");
                    push("flex-basis", "auto");
                }
                [g] if g.parse::<f32>().is_ok() => {
                    push("flex-grow", g);
                    push("flex-shrink", "1");
                    push("flex-basis", "0");
                }
                [b] => push("flex-basis", b),
                [g, s] if s.parse::<f32>().is_ok() => {
                    push("flex-grow", g);
                    push("flex-shrink", s);
                    push("flex-basis", "0");
                }
                [g, b] => {
                    push("flex-grow", g);
                    push("flex-basis", b);
                }
                [g, s, b, ..] => {
                    push("flex-grow", g);
                    push("flex-shrink", s);
                    push("flex-basis", b);
                }
                _ => {}
            }
        }
        "flex-flow" => {
            for t in tokens(v) {
                if t.contains("wrap") {
                    push("flex-wrap", t);
                } else {
                    push("flex-direction", t);
                }
            }
        }
        "gap" | "grid-gap" => {
            let t = tokens(v);
            push("row-gap", t.first().copied().unwrap_or("0"));
            push("column-gap", t.get(1).or(t.first()).copied().unwrap_or("0"));
        }
        "grid-row-gap" => push("row-gap", v),
        "grid-column-gap" => push("column-gap", v),
        "place-items" => push("align-items", tokens(v).first().copied().unwrap_or("stretch")),
        "place-content" => push("justify-content", tokens(v).last().copied().unwrap_or("start")),
        "list-style" => {
            for t in tokens(v) {
                if t == "inside" || t == "outside" {
                    push("list-style-position", t);
                } else if !t.starts_with("url(") {
                    push("list-style-type", t);
                }
            }
        }
        "text-decoration" => push("text-decoration-line", v),
        "overflow" => push("overflow-x", tokens(v).first().copied().unwrap_or("visible")),
        "border-radius" => push("border-radius", v.split('/').next().unwrap_or(v)),
        "grid-template" => push("grid-template-columns", v.split('/').nth(1).unwrap_or("none")),
        "grid-area" | "grid-column" => push("grid-column", v),
        "white-space-collapse" => push("white-space", if v.contains("preserve") { "pre-wrap" } else { "normal" }),
        _ => push(name, v),
    }
    out
}

fn gradient_color(v: &str) -> Option<String> {
    let start = v.find("gradient(")? + 9;
    let inner = &v[start..];
    for part in inner.split(',') {
        let p = part.trim();
        let first = tokens(p).first().copied().unwrap_or("");
        if parse_color(first, 0).is_some_and(|c| c >> 24 != 0) {
            return Some(first.to_string());
        }
        if let Some(i) = p.find("rgb").or_else(|| p.find('#')) {
            let c = &p[i..];
            let c = if c.starts_with("rgb") { &v[start + inner.find(c)?..] } else { c };
            let c = c.split(')').next().map(|x| if x.starts_with("rgb") { alloc::format!("{x})") } else { x.split_whitespace().next().unwrap_or("").to_string() });
            if let Some(c) = c {
                if parse_color(&c, 0).is_some() {
                    return Some(c);
                }
            }
        }
    }
    None
}

/// A length; `em` is relative to `font`.
pub fn length(v: &str, font: f32) -> Option<Len> {
    let v = v.trim();
    let vl = v.to_ascii_lowercase();
    match vl.as_str() {
        "auto" => return Some(Len::Auto),
        "none" => return Some(Len::None),
        "0" => return Some(Len::Px(0.0)),
        "min-content" => return Some(Len::MinContent),
        "max-content" => return Some(Len::MaxContent),
        "fit-content" | "-webkit-fill-available" | "-moz-available" | "stretch" => return Some(if vl == "fit-content" { Len::FitContent } else { Len::Auto }),
        "thin" => return Some(Len::Px(1.0)),
        "medium" => return Some(Len::Px(3.0)),
        "thick" => return Some(Len::Px(5.0)),
        _ => {}
    }
    for (f, kind) in [("calc(", 0), ("-webkit-calc(", 0), ("min(", 1), ("max(", 2), ("clamp(", 3)] {
        if let Some(inner) = vl.strip_prefix(f).and_then(|r| r.strip_suffix(')')) {
            return match kind {
                0 => calc(inner, font),
                1 | 2 => {
                    let args: Vec<Len> = split_args(inner).iter().filter_map(|a| length(a, font)).collect();
                    Some(if kind == 1 { Len::Min(args) } else { Len::Max(args) })
                }
                _ => {
                    let a: Vec<Len> = split_args(inner).iter().filter_map(|a| length(a, font)).collect();
                    if a.len() != 3 {
                        return None;
                    }
                    Some(Len::Max(alloc::vec![a[0].clone(), Len::Min(alloc::vec![a[1].clone(), a[2].clone()])]))
                }
            };
        }
    }
    let num_end = vl.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+' || c == 'e')).unwrap_or(vl.len());
    // 'e' only as an exponent: "1em" must not take the e.
    let num_end = if vl[..num_end].ends_with('e') { num_end - 1 } else { num_end };
    let n: f32 = vl[..num_end].parse().ok()?;
    let unit = &vl[num_end..];
    Some(match unit {
        "px" | "" => Len::Px(n),
        "%" => Len::Pct(n),
        "em" => Len::Px(n * font),
        "rem" => Len::Px(n * ROOT_FONT),
        "ex" | "ch" => Len::Px(n * font * 0.55),
        "pt" => Len::Px(n * 4.0 / 3.0),
        "pc" => Len::Px(n * 16.0),
        "in" => Len::Px(n * 96.0),
        "cm" => Len::Px(n * 96.0 / 2.54),
        "mm" => Len::Px(n * 96.0 / 25.4),
        "q" => Len::Px(n * 96.0 / 101.6),
        "vw" | "vi" | "svw" | "lvw" | "dvw" => Len::Px(n * VW.with(|w| w) / 100.0),
        "vh" | "vb" | "svh" | "lvh" | "dvh" => Len::Px(n * VH.with(|h| h) / 100.0),
        "vmin" => Len::Px(n * VW.with(|w| w).min(VH.with(|h| h)) / 100.0),
        "vmax" => Len::Px(n * VW.with(|w| w).max(VH.with(|h| h)) / 100.0),
        "lh" => Len::Px(n * font * 1.3),
        _ => return None,
    })
}

/// The viewport size for vw/vh, set before computing styles.
pub struct Cell(core::cell::Cell<f32>);
// SAFETY: styles are computed on one thread at a time (the engine is not
// shared between threads while it works).
unsafe impl Sync for Cell {}
impl Cell {
    fn with<T>(&self, f: impl FnOnce(f32) -> T) -> T {
        f(self.0.get())
    }
    pub fn get(&self) -> f32 {
        self.0.get()
    }
    pub fn set(&self, v: f32) {
        self.0.set(v)
    }
}
pub static VW: Cell = Cell(core::cell::Cell::new(1024.0));
pub static VH: Cell = Cell(core::cell::Cell::new(768.0));

fn split_args(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                out.push(s[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(s[start..].trim());
    out
}

/// calc(): sums and differences of lengths, scaled by numbers.
fn calc(expr: &str, font: f32) -> Option<Len> {
    // Terms separated by + and - (with spaces around them).
    let mut px = 0.0;
    let mut pct = 0.0;
    let toks = tokens(expr);
    let mut sign = 1.0;
    let mut i = 0;
    while i < toks.len() {
        let t = toks[i];
        match t {
            "+" => sign = 1.0,
            "-" => sign = -1.0,
            _ => {
                // A product: a * b or a / b.
                let mut val = term(t, font)?;
                while i + 2 < toks.len() && (toks[i + 1] == "*" || toks[i + 1] == "/") {
                    let other = term(toks[i + 2], font)?;
                    val = if toks[i + 1] == "*" {
                        match (val, other) {
                            ((p, q, true), (n, _, false)) | ((n, _, false), (p, q, true)) => (p * n, q * n, true),
                            ((a, _, false), (b, _, false)) => (a * b, 0.0, false),
                            _ => return None,
                        }
                    } else {
                        let (n, _, _) = other;
                        if n == 0.0 {
                            return None;
                        }
                        (val.0 / n, val.1 / n, val.2)
                    };
                    i += 2;
                }
                px += sign * val.0;
                pct += sign * val.1;
                sign = 1.0;
            }
        }
        i += 1;
    }
    Some(if pct == 0.0 { Len::Px(px) } else { Len::Calc(px, pct) })
}

/// One term of calc(): (px, pct, is a length).
fn term(t: &str, font: f32) -> Option<(f32, f32, bool)> {
    let t = t.trim();
    if let Some(inner) = t.strip_prefix('(').and_then(|x| x.strip_suffix(')')) {
        return match calc(inner, font)? {
            Len::Px(p) => Some((p, 0.0, true)),
            Len::Calc(p, q) => Some((p, q, true)),
            _ => None,
        };
    }
    if let Ok(n) = t.parse::<f32>() {
        return Some((n, 0.0, false));
    }
    match length(t, font)? {
        Len::Px(p) => Some((p, 0.0, true)),
        Len::Pct(p) => Some((0.0, p, true)),
        Len::Calc(p, q) => Some((p, q, true)),
        _ => None,
    }
}

fn font_size(v: &str, parent: f32) -> Option<f32> {
    let v = v.trim().to_ascii_lowercase();
    Some(match v.as_str() {
        "xx-small" => 9.0,
        "x-small" => 10.0,
        "small" => 13.0,
        "medium" => 16.0,
        "large" => 18.0,
        "x-large" => 24.0,
        "xx-large" => 32.0,
        "xxx-large" => 48.0,
        "smaller" => parent * 0.83,
        "larger" => parent * 1.2,
        "inherit" | "unset" => parent,
        "initial" => 16.0,
        _ => match length(&v, parent)? {
            Len::Pct(p) => parent * p / 100.0,
            l => l.resolve(parent)?,
        },
    })
}

fn apply(s: &mut Style, k: &str, v: &str, parent: &Style) {
    let vl = v.trim().to_ascii_lowercase();
    let font = s.font_size;
    if vl == "inherit" {
        inherit_one(s, k, parent);
        return;
    }
    let len = |v: &str| length(v, font);
    let side = |k: &str| SIDES.iter().position(|s| k.ends_with(s));
    match k {
        "display" => {
            let first = vl.split_whitespace().collect::<Vec<_>>();
            let d = match first.as_slice() {
                ["none"] => Some(Display::None),
                ["block"] | ["flow-root"] | ["block", "flow-root"] | ["block", "flow"] | ["-webkit-box"] if vl != "-webkit-box" => Some(Display::Block),
                ["-webkit-box"] | ["-webkit-flex"] | ["-ms-flexbox"] => Some(Display::Flex),
                ["inline"] | ["inline", "flow"] => Some(Display::Inline),
                ["inline-block"] | ["inline", "flow-root"] => Some(Display::InlineBlock),
                ["list-item"] | ["block", "list-item"] => Some(Display::ListItem),
                ["table"] => Some(Display::Table),
                ["inline-table"] => Some(Display::InlineTable),
                ["table-row"] => Some(Display::TableRow),
                ["table-row-group"] | ["table-header-group"] | ["table-footer-group"] => Some(Display::TableRowGroup),
                ["table-cell"] => Some(Display::TableCell),
                ["table-caption"] => Some(Display::TableCaption),
                ["table-column"] | ["table-column-group"] => Some(Display::None),
                ["flex"] | ["block", "flex"] => Some(Display::Flex),
                ["inline-flex"] | ["inline", "flex"] | ["-webkit-inline-box"] => Some(Display::InlineFlex),
                ["grid"] | ["block", "grid"] => Some(Display::Grid),
                ["inline-grid"] | ["inline", "grid"] => Some(Display::InlineGrid),
                ["contents"] => Some(Display::Contents),
                ["ruby"] | ["ruby-text"] => Some(Display::Inline),
                _ => None,
            };
            if let Some(d) = d {
                s.display = d;
            }
        }
        "position" => {
            s.position = match vl.as_str() {
                "relative" => Position::Relative,
                "absolute" => Position::Absolute,
                "fixed" => Position::Fixed,
                "sticky" | "-webkit-sticky" => Position::Sticky,
                _ => Position::Static,
            }
        }
        "float" => {
            s.float = match vl.as_str() {
                "left" | "inline-start" => Float::Left,
                "right" | "inline-end" => Float::Right,
                _ => Float::None,
            }
        }
        "clear" => {
            s.clear_both = vl == "both";
            s.clear = match vl.as_str() {
                "left" => Float::Left,
                "right" => Float::Right,
                _ => Float::None,
            }
        }
        "color" => {
            if let Some(c) = parse_color(v, parent.color) {
                s.color = c;
            }
        }
        "background-color" => {
            if let Some(c) = parse_color(v, s.color) {
                s.background = c;
            }
        }
        "background-image" => {
            s.background_image = if vl.contains("url(") { css::url_of(&v[vl.find("url(").unwrap_or(0)..]) } else { None };
            if s.background_image.is_none() && vl.contains("gradient(") && s.background == 0 {
                if let Some(c) = gradient_color(v).and_then(|c| parse_color(&c, s.color)) {
                    s.background = c;
                }
            }
        }
        "background-size" => s.background_cover = vl.contains("cover"),
        "font-weight" => {
            s.bold = match vl.as_str() {
                "bold" | "bolder" => true,
                "normal" | "lighter" => false,
                n => n.parse::<u32>().map_or(s.bold, |w| w >= 600),
            }
        }
        "font-style" => s.italic = vl.contains("italic") || vl.contains("oblique"),
        "font-family" => {
            let first = vl.split(',').next().unwrap_or("");
            s.mono = ["mono", "courier", "consolas", "menlo", "monaco", "code", "fixed"].iter().any(|m| first.contains(m));
        }
        "line-height" => {
            s.line_height = match vl.as_str() {
                "normal" => None,
                _ => {
                    if let Ok(n) = vl.parse::<f32>() {
                        Some(n * font)
                    } else {
                        match len(&vl) {
                            Some(Len::Pct(p)) => Some(font * p / 100.0),
                            Some(l) => l.resolve(font),
                            None => s.line_height,
                        }
                    }
                }
            }
            .map(|l| l.max(font * 0.8));
        }
        "text-align" => {
            s.text_align = match vl.as_str() {
                "center" | "-webkit-center" | "-moz-center" => Align::Center,
                "right" | "end" | "-webkit-right" => Align::Right,
                "justify" => Align::Justify,
                _ => Align::Left,
            }
        }
        "text-decoration-line" => {
            s.underline = vl.contains("underline");
            s.strike = vl.contains("line-through");
        }
        "text-transform" => {
            s.transform = match vl.as_str() {
                "uppercase" => Transform::Upper,
                "lowercase" => Transform::Lower,
                "capitalize" => Transform::Capitalize,
                _ => Transform::None,
            }
        }
        "white-space" => {
            s.white_space = match vl.as_str() {
                "nowrap" => WhiteSpace::NoWrap,
                "pre" => WhiteSpace::Pre,
                "pre-wrap" | "break-spaces" => WhiteSpace::PreWrap,
                "pre-line" => WhiteSpace::PreLine,
                _ => WhiteSpace::Normal,
            }
        }
        "text-wrap" | "text-wrap-mode" => {
            if vl == "nowrap" {
                s.white_space = WhiteSpace::NoWrap;
            }
        }
        k if k.starts_with("margin-") => {
            if let (Some(i), Some(l)) = (side(k), len(&vl)) {
                s.margin[i] = l;
            }
        }
        k if k.starts_with("padding-") => {
            if let (Some(i), Some(l)) = (side(k), len(&vl)) {
                if !matches!(l, Len::Auto) {
                    s.padding[i] = l;
                }
            }
        }
        "top" | "right" | "bottom" | "left" => {
            if let (Some(i), Some(l)) = (side(k), len(&vl)) {
                s.inset[i] = l;
            }
        }
        k if k.starts_with("border-") && k.ends_with("-width") => {
            if let (Some(i), Some(l)) = (side(&k[..k.len() - 6]), len(&vl)) {
                if s.border[i] >= 0.0 {
                    s.border[i] = l.or0(0.0);
                }
            }
        }
        k if k.starts_with("border-") && k.ends_with("-style") => {
            if let Some(i) = side(&k[..k.len() - 6]) {
                if vl == "none" || vl == "hidden" {
                    // Marks the side as having no border, whatever its width.
                    s.border[i] = -1.0;
                } else if s.border[i] < 0.0 {
                    s.border[i] = 3.0;
                }
            }
        }
        k if k.starts_with("border-") && k.ends_with("-color") => {
            if let Some(i) = side(&k[..k.len() - 6]) {
                if let Some(c) = parse_color(v, s.color) {
                    s.border_color[i] = c;
                }
            }
        }
        "border-radius" => {
            if let Some(l) = tokens(&vl).first().and_then(|t| len(t)) {
                s.radius = match l {
                    Len::Pct(p) if p >= 50.0 => 9999.0,
                    l => l.or0(0.0),
                };
            }
        }
        "border-collapse" => s.collapse = vl == "collapse",
        "border-spacing" => {
            if let Some(l) = tokens(&vl).first().and_then(|t| len(t)) {
                s.border_spacing = l.or0(0.0);
            }
        }
        "width" => set_len(&mut s.width, len(&vl)),
        "height" => set_len(&mut s.height, len(&vl)),
        "min-width" => set_len(&mut s.min_width, len(&vl)),
        "max-width" => set_len(&mut s.max_width, len(&vl)),
        "min-height" => set_len(&mut s.min_height, len(&vl)),
        "max-height" => set_len(&mut s.max_height, len(&vl)),
        "box-sizing" => s.border_box = vl == "border-box",
        "list-style-type" => {
            s.list_style = match vl.as_str() {
                "none" => ListStyle::None,
                "circle" => ListStyle::Circle,
                "square" => ListStyle::Square,
                "decimal" | "decimal-leading-zero" => ListStyle::Decimal,
                "lower-alpha" | "lower-latin" => ListStyle::LowerAlpha,
                "upper-alpha" | "upper-latin" => ListStyle::UpperAlpha,
                "lower-roman" => ListStyle::LowerRoman,
                "upper-roman" => ListStyle::UpperRoman,
                "disc" => ListStyle::Disc,
                _ => s.list_style,
            }
        }
        "list-style-position" => s.list_inside = vl == "inside",
        "visibility" => s.visible = vl != "hidden" && vl != "collapse",
        "z-index" => s.z_below = vl.parse::<i32>().is_ok_and(|z| z < 0),
        "translate" => {
            let mut it = vl.split_whitespace().map(len);
            s.translate = [it.next().flatten().unwrap_or(Len::Auto), it.next().flatten().unwrap_or(Len::Auto)];
        }
        "transform" | "-webkit-transform" => {
            s.translate = [Len::Auto, Len::Auto];
            let mut rest = vl.as_str();
            while let Some(at) = rest.find("translate") {
                rest = &rest[at + 9..];
                let Some(open) = rest.find('(') else { break };
                let Some(close) = rest.find(')') else { break };
                if close < open {
                    break;
                }
                let kind = rest[..open].trim();
                let args: Vec<Option<Len>> = split_args(&rest[open + 1..close]).iter().map(|a| len(a.trim())).collect();
                let arg = |i: usize| args.get(i).cloned().flatten().unwrap_or(Len::Auto);
                match kind {
                    "" | "3d" => s.translate = [arg(0), arg(1)],
                    "x" => s.translate[0] = arg(0),
                    "y" => s.translate[1] = arg(0),
                    _ => {}
                }
                rest = &rest[close + 1..];
            }
        }
        "opacity" => {
            if let Ok(o) = vl.trim_end_matches('%').parse::<f32>() {
                s.opacity = if vl.ends_with('%') { o / 100.0 } else { o }.clamp(0.0, 1.0);
            }
        }
        "flex-direction" | "-webkit-box-orient" => {
            s.flex_row = !vl.starts_with("column") && vl != "vertical";
            s.flex_reverse = vl.ends_with("reverse");
        }
        "flex-wrap" => s.flex_wrap = vl.starts_with("wrap"),
        "justify-content" | "-webkit-box-pack" => {
            s.justify = match vl.trim_start_matches("safe ") {
                "center" => Justify::Center,
                "flex-end" | "end" | "right" => Justify::End,
                "space-between" | "justify" => Justify::Between,
                "space-around" => Justify::Around,
                "space-evenly" => Justify::Evenly,
                _ => Justify::Start,
            }
        }
        "align-items" | "-webkit-box-align" => s.align_items = items(&vl),
        "align-self" => s.align_self = if vl == "auto" { None } else { Some(items(&vl)) },
        "row-gap" => s.row_gap = len(&vl).map_or(0.0, |l| l.or0(0.0)),
        "column-gap" => s.column_gap = len(&vl).map_or(0.0, |l| l.or0(0.0)),
        "flex-grow" | "-webkit-box-flex" => s.grow = vl.parse().unwrap_or(0.0),
        "flex-shrink" => s.shrink = vl.parse().unwrap_or(1.0),
        "flex-basis" => {
            if vl == "content" {
                s.basis = Len::Auto;
            } else if let Some(l) = len(&vl) {
                s.basis = l;
            }
        }
        "order" => s.order = vl.parse().unwrap_or(0),
        "grid-template-columns" => s.columns = tracks(&vl, font),
        "grid-column" | "grid-column-end" => {
            s.span_all = vl.replace(' ', "").contains("1/-1") || vl.contains("-1");
            if let Some(n) = vl.split("span").nth(1).and_then(|n| n.trim().split(|c: char| !c.is_ascii_digit()).next()).and_then(|n| n.parse().ok()) {
                s.span = n;
            }
        }
        "vertical-align" => {
            s.vertical_align = match vl.as_str() {
                "middle" => VAlign::Middle,
                "top" => VAlign::Top,
                "bottom" | "text-bottom" => VAlign::Bottom,
                "text-top" => VAlign::TextTop,
                "sub" => VAlign::Sub,
                "super" => VAlign::Super,
                _ => VAlign::Baseline,
            }
        }
        "text-indent" => s.text_indent = len(&vl).and_then(|l| l.resolve(0.0)).unwrap_or(0.0),
        "overflow-x" | "overflow-y" => s.overflow_hidden = vl != "visible",
        "letter-spacing" => s.letter_spacing = len(&vl).and_then(|l| l.resolve(0.0)).unwrap_or(0.0),
        "content-visibility" => s.content_visibility_hidden = vl == "hidden",
        "object-fit" => s.object_fit_cover = vl == "cover",
        "aspect-ratio" => {
            let t = vl.replace("auto", "");
            let parts: Vec<f32> = t.split('/').filter_map(|x| x.trim().parse().ok()).collect();
            s.aspect_ratio = match parts.as_slice() {
                [a, b] if *b > 0.0 => Some(a / b),
                [a] => Some(*a),
                _ => None,
            };
        }
        "cursor" => s.cursor_pointer = vl == "pointer",
        "clip" => {
            // The "visually hidden" idiom.
            if vl.replace(' ', "").starts_with("rect(0") && s.position == Position::Absolute {
                s.visible = false;
            }
        }
        "clip-path" => {
            if vl.contains("inset(50%") || vl.contains("inset(100%") {
                s.visible = false;
            }
        }
        _ => {}
    }
}

fn set_len(target: &mut Len, l: Option<Len>) {
    if let Some(l) = l {
        *target = l;
    }
}

fn items(v: &str) -> Items {
    match v.trim_start_matches("safe ") {
        "center" => Items::Center,
        "flex-start" | "start" | "self-start" | "baseline" | "first baseline" => {
            if v.contains("baseline") {
                Items::Baseline
            } else {
                Items::Start
            }
        }
        "flex-end" | "end" | "self-end" => Items::End,
        _ => Items::Stretch,
    }
}

fn tracks(v: &str, font: f32) -> Vec<Track> {
    let mut out = Vec::new();
    for t in tokens(v) {
        if let Some(inner) = t.strip_prefix("repeat(").and_then(|x| x.strip_suffix(')')) {
            let (count, what) = inner.split_once(',').unwrap_or(("1", inner));
            let count = count.trim();
            if count == "auto-fill" || count == "auto-fit" {
                let what = what.trim();
                let min = what.strip_prefix("minmax(").and_then(|x| x.split(',').next()).unwrap_or(what);
                let min = length(min.trim(), font).and_then(|l| l.resolve(0.0)).unwrap_or(200.0);
                out.push(Track::Fill(min.max(20.0)));
                continue;
            }
            let n: usize = count.parse().unwrap_or(1);
            let inner_tracks = tracks(what, font);
            for _ in 0..n.min(24) {
                out.extend(inner_tracks.iter().cloned());
            }
            continue;
        }
        if t.starts_with('[') {
            continue;
        }
        let t2 = if let Some(inner) = t.strip_prefix("minmax(").and_then(|x| x.strip_suffix(')')) {
            inner.split(',').nth(1).unwrap_or("1fr").trim().to_string()
        } else {
            t.to_string()
        };
        if let Some(fr) = t2.strip_suffix("fr") {
            out.push(Track::Fr(fr.parse().unwrap_or(1.0)));
        } else {
            out.push(match length(&t2, font) {
                Some(Len::Px(p)) => Track::Px(p),
                Some(Len::Pct(p)) => Track::Pct(p),
                _ => Track::Auto,
            });
        }
    }
    out
}

fn inherit_one(s: &mut Style, k: &str, p: &Style) {
    match k {
        "color" => s.color = p.color,
        "background-color" => s.background = p.background,
        "display" => s.display = p.display,
        "width" => s.width = p.width.clone(),
        "height" => s.height = p.height.clone(),
        "font-weight" => s.bold = p.bold,
        "text-align" => s.text_align = p.text_align,
        _ => {}
    }
}

const NAMED: &[(&str, u32)] = &[
    ("black", 0x000000), ("white", 0xFFFFFF), ("red", 0xFF0000), ("green", 0x008000), ("blue", 0x0000FF), ("yellow", 0xFFFF00), ("gray", 0x808080),
    ("grey", 0x808080), ("silver", 0xC0C0C0), ("maroon", 0x800000), ("purple", 0x800080), ("fuchsia", 0xFF00FF), ("magenta", 0xFF00FF), ("lime", 0x00FF00),
    ("olive", 0x808000), ("navy", 0x000080), ("teal", 0x008080), ("aqua", 0x00FFFF), ("cyan", 0x00FFFF), ("orange", 0xFFA500), ("pink", 0xFFC0CB),
    ("brown", 0xA52A2A), ("gold", 0xFFD700), ("indigo", 0x4B0082), ("violet", 0xEE82EE), ("coral", 0xFF7F50), ("salmon", 0xFA8072), ("tomato", 0xFF6347),
    ("crimson", 0xDC143C), ("orangered", 0xFF4500), ("darkred", 0x8B0000), ("darkgreen", 0x006400), ("darkblue", 0x00008B), ("lightgray", 0xD3D3D3),
    ("lightgrey", 0xD3D3D3), ("darkgray", 0xA9A9A9), ("darkgrey", 0xA9A9A9), ("dimgray", 0x696969), ("dimgrey", 0x696969), ("gainsboro", 0xDCDCDC),
    ("whitesmoke", 0xF5F5F5), ("snow", 0xFFFAFA), ("ivory", 0xFFFFF0), ("beige", 0xF5F5DC), ("linen", 0xFAF0E6), ("lavender", 0xE6E6FA),
    ("lightblue", 0xADD8E6), ("skyblue", 0x87CEEB), ("steelblue", 0x4682B4), ("royalblue", 0x4169E1), ("dodgerblue", 0x1E90FF), ("deepskyblue", 0x00BFFF),
    ("cornflowerblue", 0x6495ED), ("midnightblue", 0x191970), ("slategray", 0x708090), ("slategrey", 0x708090), ("lightslategray", 0x778899),
    ("darkslategray", 0x2F4F4F), ("lightgreen", 0x90EE90), ("seagreen", 0x2E8B57), ("forestgreen", 0x228B22), ("limegreen", 0x32CD32),
    ("darkolivegreen", 0x556B2F), ("yellowgreen", 0x9ACD32), ("khaki", 0xF0E68C), ("darkorange", 0xFF8C00), ("chocolate", 0xD2691E), ("tan", 0xD2B48C),
    ("wheat", 0xF5DEB3), ("sienna", 0xA0522D), ("firebrick", 0xB22222), ("hotpink", 0xFF69B4), ("deeppink", 0xFF1493), ("orchid", 0xDA70D6),
    ("plum", 0xDDA0DD), ("thistle", 0xD8BFD8), ("turquoise", 0x40E0D0), ("aquamarine", 0x7FFFD4), ("mintcream", 0xF5FFFA), ("honeydew", 0xF0FFF0),
    ("aliceblue", 0xF0F8FF), ("ghostwhite", 0xF8F8FF), ("azure", 0xF0FFFF), ("seashell", 0xFFF5EE), ("oldlace", 0xFDF5E6), ("floralwhite", 0xFFFAF0),
    ("antiquewhite", 0xFAEBD7), ("lightyellow", 0xFFFFE0), ("lightcyan", 0xE0FFFF), ("lightpink", 0xFFB6C1), ("mistyrose", 0xFFE4E1),
    ("lemonchiffon", 0xFFFACD), ("papayawhip", 0xFFEFD5), ("peachpuff", 0xFFDAB9), ("darkviolet", 0x9400D3), ("darkmagenta", 0x8B008B),
    ("mediumpurple", 0x9370DB), ("rebeccapurple", 0x663399), ("slateblue", 0x6A5ACD), ("darkslateblue", 0x483D8B), ("cadetblue", 0x5F9EA0),
    ("darkcyan", 0x008B8B), ("lightseagreen", 0x20B2AA), ("mediumseagreen", 0x3CB371), ("springgreen", 0x00FF7F), ("lawngreen", 0x7CFC00),
    ("chartreuse", 0x7FFF00), ("goldenrod", 0xDAA520), ("darkgoldenrod", 0xB8860B), ("peru", 0xCD853F), ("rosybrown", 0xBC8F8F), ("indianred", 0xCD5C5C),
    ("lightcoral", 0xF08080), ("darksalmon", 0xE9967A), ("lightsalmon", 0xFFA07A), ("palegreen", 0x98FB98), ("paleturquoise", 0xAFEEEE),
    ("powderblue", 0xB0E0E6), ("lightsteelblue", 0xB0C4DE), ("mediumblue", 0x0000CD), ("burlywood", 0xDEB887), ("bisque", 0xFFE4C4),
    ("navajowhite", 0xFFDEAD), ("moccasin", 0xFFE4B5), ("cornsilk", 0xFFF8DC), ("blanchedalmond", 0xFFEBCD), ("lavenderblush", 0xFFF0F5),
    ("mediumvioletred", 0xC71585), ("palevioletred", 0xDB7093), ("darkkhaki", 0xBDB76B), ("palegoldenrod", 0xEEE8AA), ("darkseagreen", 0x8FBC8F),
    ("olivedrab", 0x6B8E23), ("mediumaquamarine", 0x66CDAA), ("darkturquoise", 0x00CED1), ("mediumturquoise", 0x48D1CC), ("mediumslateblue", 0x7B68EE),
    ("blueviolet", 0x8A2BE2), ("darkorchid", 0x9932CC), ("mediumorchid", 0xBA55D3), ("saddlebrown", 0x8B4513), ("sandybrown", 0xF4A460),
    ("lightgoldenrodyellow", 0xFAFAD2), ("greenyellow", 0xADFF2F), ("mediumspringgreen", 0x00FA9A), ("lightskyblue", 0x87CEFA), ("darkslategrey", 0x2F4F4F),
    ("lightslategrey", 0x778899), ("slategray", 0x708090),
];

/// A CSS colour as ARGB; `current` is currentColor.
pub fn parse_color(v: &str, current: u32) -> Option<u32> {
    let v = v.trim().to_ascii_lowercase();
    if v == "transparent" || v == "none" {
        return Some(0);
    }
    if v == "currentcolor" || v == "inherit" {
        return Some(current);
    }
    // Pages are shown in the light scheme: light-dark(<light>, <dark>).
    if let Some(inner) = v.strip_prefix("light-dark(").and_then(|r| r.strip_suffix(')')) {
        let mut depth = 0;
        let split = inner.char_indices().find(|&(_, c)| {
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {}
            }
            c == ',' && depth == 0
        })?;
        return parse_color(&inner[..split.0], current);
    }
    if let Some(h) = v.strip_prefix('#') {
        let hex = |s: &str| u32::from_str_radix(s, 16).ok();
        let d = |c: u8| hex(core::str::from_utf8(&[c, c]).ok()?);
        let b = h.as_bytes();
        return match h.len() {
            3 => Some(0xFF00_0000 | d(b[0])? << 16 | d(b[1])? << 8 | d(b[2])?),
            4 => Some(d(b[3])? << 24 | d(b[0])? << 16 | d(b[1])? << 8 | d(b[2])?),
            6 => Some(0xFF00_0000 | hex(h)?),
            8 => {
                let rgba = hex(h)?;
                Some((rgba & 0xFF) << 24 | rgba >> 8)
            }
            _ => None,
        };
    }
    if let Some(inner) = v.strip_prefix("rgba(").or_else(|| v.strip_prefix("rgb(")).and_then(|r| r.strip_suffix(')')) {
        let parts: Vec<&str> = inner.split([',', ' ', '/']).map(str::trim).filter(|s| !s.is_empty()).collect();
        if parts.len() < 3 {
            return None;
        }
        let ch = |s: &str| -> Option<u32> {
            Some(if let Some(p) = s.strip_suffix('%') { (p.parse::<f32>().ok()? * 2.55) as u32 } else { s.parse::<f32>().ok()?.clamp(0.0, 255.0) as u32 }.min(255))
        };
        let a = match parts.get(3) {
            Some(a) => {
                let f = if let Some(p) = a.strip_suffix('%') { p.parse::<f32>().ok()? / 100.0 } else { a.parse::<f32>().ok()? };
                (f.clamp(0.0, 1.0) * 255.0) as u32
            }
            None => 255,
        };
        return Some(a << 24 | ch(parts[0])? << 16 | ch(parts[1])? << 8 | ch(parts[2])?);
    }
    if let Some(inner) = v.strip_prefix("hsla(").or_else(|| v.strip_prefix("hsl(")).and_then(|r| r.strip_suffix(')')) {
        let parts: Vec<&str> = inner.split([',', ' ', '/']).map(str::trim).filter(|s| !s.is_empty()).collect();
        if parts.len() < 3 {
            return None;
        }
        let h: f32 = parts[0].trim_end_matches("deg").parse().ok()?;
        let s: f32 = parts[1].trim_end_matches('%').parse::<f32>().ok()? / 100.0;
        let l: f32 = parts[2].trim_end_matches('%').parse::<f32>().ok()? / 100.0;
        let a = match parts.get(3) {
            Some(a) => {
                let f = if let Some(p) = a.strip_suffix('%') { p.parse::<f32>().ok()? / 100.0 } else { a.parse::<f32>().ok()? };
                (f.clamp(0.0, 1.0) * 255.0) as u32
            }
            None => 255,
        };
        let (r, g, b) = hsl(h, s, l);
        return Some(a << 24 | r << 16 | g << 8 | b);
    }
    NAMED.iter().find(|(n, _)| *n == v).map(|(_, c)| 0xFF00_0000 | c)
}

fn hsl(h: f32, s: f32, l: f32) -> (u32, u32, u32) {
    let h = ((h % 360.0) + 360.0) % 360.0 / 360.0;
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let f = |t: f32| {
        let t = if t < 0.0 {
            t + 1.0
        } else if t > 1.0 {
            t - 1.0
        } else {
            t
        };
        let v = if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        };
        (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u32
    };
    (f(h + 1.0 / 3.0), f(h), f(h - 1.0 / 3.0))
}

/// Element ids that are not shown: those with display none, or under one.
pub fn hidden(styles: &Styles, dom: &Dom, id: NodeId) -> bool {
    let mut n = Some(id);
    while let Some(i) = n {
        if styles.styles[i].as_ref().is_some_and(|s| s.display == Display::None) {
            return true;
        }
        n = dom.nodes[i].parent;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::html;

    #[test]
    fn values() {
        assert_eq!(parse_color("#fff", 0), Some(0xFFFF_FFFF));
        assert_eq!(parse_color("rgba(255, 0, 0, .5)", 0), Some(0x7FFF_0000));
        assert_eq!(parse_color("light-dark(#eee,#222)", 0), Some(0xFFEE_EEEE));
        assert_eq!(light_only("light-dark(#eee,#222) url(a.png)"), "#eee url(a.png)");
        assert_eq!(parse_color("light-dark(rgb(1, 2, 3), #222)", 0), Some(0xFF01_0203));
        assert_eq!(parse_color("rgb(0 128 255 / 100%)", 0), Some(0xFF00_80FF));
        assert_eq!(parse_color("hsl(120, 100%, 50%)", 0), Some(0xFF00_FF00));
        assert_eq!(length("2em", 10.0), Some(Len::Px(20.0)));
        assert_eq!(length("calc(100% - 20px)", 16.0), Some(Len::Calc(-20.0, 100.0)));
        assert_eq!(length("calc(2 * 8px + 1rem)", 16.0), Some(Len::Px(32.0)));
        assert_eq!(length("min(100%, 600px)", 16.0).unwrap().resolve(1000.0), Some(600.0));
        assert_eq!(length("1e2px", 16.0), Some(Len::Px(100.0)));
    }

    #[test]
    fn cascade() {
        let d = html::parse("<div id=x class=a style='color: blue'><p class=b>t</p><span>s</span></div>");
        let ua = css::parse(UA_CSS);
        let sheet = css::parse(":root { --main: #00ff00; --m: 10px } #x { color: red; padding: 1px 2px } .a p { color: var(--main); margin: var(--m) 0 } p.b { font-size: 2em; font-weight: 700 } span { display: block }");
        let st = compute(&d, &[(true, &ua), (false, &sheet)], Viewport { width: 800.0, height: 600.0 });
        let div = d.find("div").unwrap();
        let p = d.find("p").unwrap();
        let s = st.styles[div].as_ref().unwrap();
        assert_eq!(s.color, 0xFF00_00FF);
        assert_eq!(s.padding[1], Len::Px(2.0));
        let ps = st.styles[p].as_ref().unwrap();
        assert_eq!(ps.color, 0xFF00_FF00);
        assert_eq!(ps.font_size, 32.0);
        assert!(ps.bold);
        assert_eq!(ps.margin[0], Len::Px(10.0));
        assert_eq!(st.styles[d.find("span").unwrap()].as_ref().unwrap().display, Display::Block);
    }
}

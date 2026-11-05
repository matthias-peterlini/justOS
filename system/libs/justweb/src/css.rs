//! CSS: stylesheets into rules, selectors and their matching, and the
//! media queries a rule depends on.

use crate::dom::{Data, Dom, NodeId};
use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Clone, Debug)]
pub struct Decl {
    /// Lower case (custom properties keep their case).
    pub name: String,
    pub value: String,
    pub important: bool,
}

#[derive(Clone, Debug)]
pub struct Rule {
    pub selectors: Vec<Selector>,
    pub decls: Vec<Decl>,
    /// The media queries the rule sits in (all must hold).
    pub media: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Stylesheet {
    pub rules: Vec<Rule>,
    /// Addresses of `@import`ed sheets.
    pub imports: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Combinator {
    Descendant,
    Child,
    Adjacent,
    Sibling,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AttrOp {
    Exists,
    Equals,
    Word,
    Prefix,
    Suffix,
    Contains,
    Dash,
}

#[derive(Clone, Debug)]
pub struct AttrSel {
    pub name: String,
    pub op: AttrOp,
    pub value: String,
    pub ci: bool,
}

#[derive(Clone, Debug)]
pub enum Pseudo {
    FirstChild,
    LastChild,
    OnlyChild,
    FirstOfType,
    LastOfType,
    NthChild(i32, i32),
    NthLastChild(i32, i32),
    NthOfType(i32, i32),
    Not(Vec<Compound>),
    Is(Vec<Compound>),
    Link,
    Root,
    Empty,
    Checked,
    Disabled,
    Enabled,
    /// Something we never match (:hover, ::before, :has…).
    Never,
}

#[derive(Clone, Debug, Default)]
pub struct Compound {
    pub tag: Option<String>,
    pub id: Option<String>,
    pub classes: Vec<String>,
    pub attrs: Vec<AttrSel>,
    pub pseudo: Vec<Pseudo>,
}

#[derive(Clone, Debug)]
pub struct Selector {
    /// Left to right; the combinator is the one before each compound.
    pub parts: Vec<(Combinator, Compound)>,
    pub specificity: u32,
}

impl Selector {
    /// The rightmost compound, which an element must match first.
    pub fn subject(&self) -> &Compound {
        &self.parts.last().expect("a selector has a compound").1
    }
}

/// Skips comments and returns the source without them.
fn strip_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        match rest.find("/*") {
            Some(p) => {
                out.push_str(&rest[..p]);
                match rest[p + 2..].find("*/") {
                    Some(e) => rest = &rest[p + 2 + e + 2..],
                    None => break,
                }
            }
            None => {
                out.push_str(rest);
                break;
            }
        }
    }
    out
}

/// The index just past the block that starts at `open` (a '{'), honouring
/// nesting and strings.
fn block_end(s: &[u8], open: usize) -> usize {
    let mut depth = 0;
    let mut i = open;
    while i < s.len() {
        match s[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            b'"' | b'\'' => {
                let q = s[i];
                i += 1;
                while i < s.len() && s[i] != q {
                    if s[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    s.len()
}

/// The next top-level occurrence of one of `stops` from `i`, outside
/// strings, brackets and parentheses.
fn find_top(s: &[u8], mut i: usize, stops: &[u8]) -> usize {
    let mut depth = 0i32;
    while i < s.len() {
        let c = s[i];
        if depth == 0 && stops.contains(&c) {
            return i;
        }
        match c {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            b'"' | b'\'' => {
                i += 1;
                while i < s.len() && s[i] != c {
                    if s[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    s.len()
}

fn char_boundary(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i.min(s.len())
}

/// Parses a stylesheet.
pub fn parse(src: &str) -> Stylesheet {
    let src = strip_comments(src);
    let mut sheet = Stylesheet::default();
    parse_rules(&src, &mut sheet, &Vec::new());
    sheet
}

fn parse_rules(src: &str, sheet: &mut Stylesheet, media: &Vec<String>) {
    let b = src.as_bytes();
    let mut i = 0;
    while i < b.len() {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b';') {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        if b[i] == b'@' {
            let stop = find_top(b, i, b";{");
            let stop = char_boundary(src, stop);
            let prelude = src[i + 1..stop].trim();
            let (name, rest) = prelude.split_once(|c: char| c.is_whitespace() || c == '(' || c == '"' || c == '\'').map_or((prelude, ""), |(n, _)| (n, &prelude[n.len()..]));
            let name = name.to_ascii_lowercase();
            if stop >= b.len() || b[stop] == b';' {
                if name == "import" {
                    if let Some(u) = url_of(rest.trim()) {
                        let cond = rest.trim().split_once([')', '"', '\'']).map(|(_, c)| c.trim().trim_start_matches(['"', '\'', ')']).trim()).unwrap_or("");
                        if cond.is_empty() || media_text_ok(cond) {
                            sheet.imports.push(u);
                        }
                    }
                }
                i = stop + 1;
                continue;
            }
            let end = block_end(b, stop);
            let inner = &src[stop + 1..char_boundary(src, end.saturating_sub(1)).max(stop + 1)];
            match name.as_str() {
                "media" => {
                    let mut m = media.clone();
                    m.push(rest.trim().to_string());
                    parse_rules(inner, sheet, &m);
                }
                "supports" => {
                    if supports(rest.trim()) {
                        parse_rules(inner, sheet, media);
                    }
                }
                "layer" | "container" | "document" | "scope" | "starting-style" => {
                    if name != "starting-style" {
                        parse_rules(inner, sheet, media);
                    }
                }
                _ => {}
            }
            i = end;
            continue;
        }
        let open = find_top(b, i, b"{");
        if open >= b.len() {
            break;
        }
        let end = block_end(b, open);
        let prelude = &src[i..open];
        let body = &src[open + 1..char_boundary(src, end.saturating_sub(1)).max(open + 1)];
        let selectors = parse_selector_list(prelude);
        let (decls, nested) = parse_block(body);
        if !selectors.is_empty() && !decls.is_empty() {
            sheet.rules.push(Rule { selectors: selectors.clone(), decls, media: media.clone() });
        }
        // Nested rules ("& .x { }", ".y & { }"): flattened onto the parent.
        for (sel, inner) in nested {
            let nested_sel: String = prelude
                .split(',')
                .flat_map(|p| {
                    let p = p.trim();
                    sel.split(',').map(move |s| {
                        let s = s.trim();
                        if s.contains('&') {
                            s.replace('&', p)
                        } else {
                            alloc::format!("{p} {s}")
                        }
                    })
                })
                .collect::<Vec<_>>()
                .join(",");
            parse_rules(&alloc::format!("{nested_sel}{{{inner}}}"), sheet, media);
        }
        i = end;
    }
}

/// Declarations of a block, and the rules nested in it.
pub fn parse_block(body: &str) -> (Vec<Decl>, Vec<(String, String)>) {
    let b = body.as_bytes();
    let mut decls = Vec::new();
    let mut nested = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let stop = find_top(b, i, b";{");
        if stop < b.len() && b[stop] == b'{' {
            let end = block_end(b, stop);
            let sel = body[i..stop].trim().to_string();
            let inner = body[stop + 1..char_boundary(body, end.saturating_sub(1)).max(stop + 1)].to_string();
            if !sel.starts_with('@') {
                nested.push((sel, inner));
            }
            i = end;
            continue;
        }
        let stop = char_boundary(body, stop);
        let d = &body[i..stop];
        if let Some((k, v)) = d.split_once(':') {
            let k = k.trim();
            let mut v = v.trim();
            let mut important = false;
            if let Some(p) = v.to_ascii_lowercase().rfind("!important") {
                v = v[..p].trim();
                important = true;
            }
            if !k.is_empty() {
                let name = if k.starts_with("--") { k.to_string() } else { k.to_ascii_lowercase() };
                decls.push(Decl { name, value: v.to_string(), important });
            }
        }
        i = stop + 1;
    }
    (decls, nested)
}

/// The address in `url(...)` or a quoted string.
pub fn url_of(v: &str) -> Option<String> {
    let v = v.trim();
    let inner = if let Some(r) = v.strip_prefix("url(").or_else(|| v.strip_prefix("URL(")) {
        r.split(')').next()?.trim()
    } else {
        v
    };
    let inner = inner.trim_matches(['"', '\'']);
    let q = if v.starts_with('"') || v.starts_with('\'') {
        let q = v.as_bytes()[0] as char;
        v[1..].split(q).next()?
    } else {
        inner
    };
    (!q.is_empty()).then(|| q.to_string())
}

fn supports(cond: &str) -> bool {
    let c = cond.to_ascii_lowercase();
    if c.starts_with("not") {
        return false;
    }
    // Things we do not do.
    !["backdrop-filter", "container", "subgrid", ":has", "anchor", "view-transition", "selector("].iter().any(|x| c.contains(x))
}

/// The viewport media queries are tested against.
#[derive(Clone, Copy)]
pub struct Viewport {
    pub width: f32,
    pub height: f32,
}

/// Whether a media query list holds for the screen.
pub fn media_ok(query: &str, vp: Viewport) -> bool {
    query.split(',').any(|q| one_query(q.trim(), vp))
}

fn media_text_ok(q: &str) -> bool {
    media_ok(q, Viewport { width: 1024.0, height: 768.0 })
}

fn one_query(q: &str, vp: Viewport) -> bool {
    let q = q.to_ascii_lowercase();
    let (negate, q) = match q.strip_prefix("not ") {
        Some(r) => (true, r.trim().to_string()),
        None => (false, q.trim_start_matches("only ").to_string()),
    };
    let mut ok = true;
    for part in q.split(" and ") {
        let p = part.trim();
        let r = if p.is_empty() || p == "all" || p == "screen" {
            true
        } else if p == "print" || p == "speech" || p == "tv" {
            false
        } else if let Some(f) = p.strip_prefix('(').and_then(|x| x.strip_suffix(')')) {
            feature(f.trim(), vp)
        } else {
            false
        };
        ok &= r;
    }
    ok != negate
}

fn feature(f: &str, vp: Viewport) -> bool {
    // Range syntax: (width >= 600px), (400px <= width <= 800px).
    for op in [">=", "<=", ">", "<"] {
        if f.contains(op) {
            let parts: Vec<&str> = f.split(op).map(str::trim).collect();
            if parts.len() == 2 {
                let (name, v, op) = if parts[0].chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
                    (parts[0], parts[1], op)
                } else {
                    (parts[1], parts[0], flip(op))
                };
                let x = if name.contains("height") { vp.height } else { vp.width };
                let v = media_len(v);
                return match op {
                    ">=" => x >= v,
                    "<=" => x <= v,
                    ">" => x > v,
                    _ => x < v,
                };
            }
            if parts.len() == 3 {
                let x = if parts[1].contains("height") { vp.height } else { vp.width };
                let lo = media_len(parts[0]);
                let hi = media_len(parts[2]);
                return x >= lo && x <= hi;
            }
        }
    }
    let (name, value) = f.split_once(':').map_or((f, ""), |(a, b)| (a.trim(), b.trim()));
    match name {
        "min-width" => vp.width >= media_len(value),
        "max-width" => vp.width <= media_len(value),
        "min-height" => vp.height >= media_len(value),
        "max-height" => vp.height <= media_len(value),
        "width" => (vp.width - media_len(value)).abs() < 0.5,
        "orientation" => (value == "landscape") == (vp.width >= vp.height),
        "prefers-color-scheme" => value == "light",
        "prefers-reduced-motion" => value == "reduce",
        "prefers-contrast" | "forced-colors" | "inverted-colors" => value == "no-preference" || value == "none",
        "hover" | "any-hover" => value == "hover" || value.is_empty(),
        "pointer" | "any-pointer" => value == "fine" || value.is_empty(),
        "color" | "min-color" => true,
        "min-resolution" | "-webkit-min-device-pixel-ratio" | "min-device-pixel-ratio" => {
            let v: f32 = value.trim_end_matches(|c: char| c.is_ascii_alphabetic()).parse().unwrap_or(2.0);
            v <= 1.0
        }
        "max-resolution" | "-webkit-max-device-pixel-ratio" => true,
        "display-mode" => value == "browser",
        "scripting" => value == "none",
        "update" => value == "fast",
        _ => false,
    }
}

fn flip(op: &str) -> &str {
    match op {
        ">=" => "<=",
        "<=" => ">=",
        ">" => "<",
        _ => ">",
    }
}

fn media_len(v: &str) -> f32 {
    let v = v.trim();
    let num: String = v.chars().take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-').collect();
    let n: f32 = num.parse().unwrap_or(0.0);
    let unit = &v[num.len()..];
    match unit {
        "em" | "rem" => n * 16.0,
        "pt" => n * 4.0 / 3.0,
        _ => n,
    }
}

// Selectors.

/// A comma-separated list; selectors we cannot read are left out.
pub fn parse_selector_list(s: &str) -> Vec<Selector> {
    split_top(s, ',').into_iter().filter_map(|p| parse_selector(p.trim())).collect()
}

fn split_top(s: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            c if c == sep && depth == 0 => {
                out.push(&s[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

fn parse_selector(s: &str) -> Option<Selector> {
    if s.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    let mut comb = Combinator::Descendant;
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut spec = 0u32;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '>' || c == '+' || c == '~' {
            comb = match c {
                '>' => Combinator::Child,
                '+' => Combinator::Adjacent,
                _ => Combinator::Sibling,
            };
            i += 1;
            continue;
        }
        let (compound, used, sp) = parse_compound(&chars[i..])?;
        if used == 0 {
            return None;
        }
        spec += sp;
        parts.push((comb, compound));
        comb = Combinator::Descendant;
        i += used;
    }
    if parts.is_empty() {
        return None;
    }
    Some(Selector { parts, specificity: spec })
}

fn ident(chars: &[char], mut i: usize) -> (String, usize) {
    let mut s = String::new();
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() {
            // An escape: hex digits or the next character.
            let mut hex = String::new();
            let mut j = i + 1;
            while j < chars.len() && hex.len() < 6 && chars[j].is_ascii_hexdigit() {
                hex.push(chars[j]);
                j += 1;
            }
            if !hex.is_empty() {
                if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    s.push(ch);
                }
                if j < chars.len() && chars[j] == ' ' {
                    j += 1;
                }
                i = j;
            } else {
                s.push(chars[i + 1]);
                i += 2;
            }
            continue;
        }
        if c.is_alphanumeric() || c == '-' || c == '_' || (c as u32) > 127 {
            s.push(c);
            i += 1;
        } else {
            break;
        }
    }
    (s, i)
}

/// One compound selector: returns it, the characters used and its
/// specificity (ids * 10000 + classes * 100 + types).
fn parse_compound(chars: &[char]) -> Option<(Compound, usize, u32)> {
    let mut c = Compound::default();
    let mut i = 0;
    let mut spec = 0;
    if i < chars.len() && chars[i] == '*' {
        i += 1;
    } else if i < chars.len() && (chars[i].is_alphabetic() || chars[i] == '\\' || chars[i] == '-' || chars[i] == '_') {
        let (name, j) = ident(chars, i);
        c.tag = Some(name.to_ascii_lowercase());
        spec += 1;
        i = j;
    }
    // A namespace prefix ("svg|a") is not supported.
    if i < chars.len() && chars[i] == '|' {
        return None;
    }
    while i < chars.len() {
        match chars[i] {
            '#' => {
                let (name, j) = ident(chars, i + 1);
                c.id = Some(name);
                spec += 10000;
                i = j;
            }
            '.' => {
                let (name, j) = ident(chars, i + 1);
                if name.is_empty() {
                    return None;
                }
                c.classes.push(name);
                spec += 100;
                i = j;
            }
            '[' => {
                let end = i + chars[i..].iter().position(|&x| x == ']')?;
                let inner: String = chars[i + 1..end].iter().collect();
                c.attrs.push(parse_attr(&inner)?);
                spec += 100;
                i = end + 1;
            }
            ':' => {
                let element = chars.get(i + 1) == Some(&':');
                let start = if element { i + 2 } else { i + 1 };
                let (name, mut j) = ident(chars, start);
                let name = name.to_ascii_lowercase();
                let mut arg = String::new();
                if j < chars.len() && chars[j] == '(' {
                    let mut depth = 0;
                    let mut k = j;
                    while k < chars.len() {
                        if chars[k] == '(' {
                            depth += 1;
                        } else if chars[k] == ')' {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        k += 1;
                    }
                    arg = chars[j + 1..k.min(chars.len())].iter().collect();
                    j = (k + 1).min(chars.len());
                }
                let legacy_element = matches!(name.as_str(), "before" | "after" | "first-line" | "first-letter");
                let p = if element || legacy_element {
                    Pseudo::Never
                } else {
                    spec += 100;
                    match name.as_str() {
                        "first-child" => Pseudo::FirstChild,
                        "last-child" => Pseudo::LastChild,
                        "only-child" => Pseudo::OnlyChild,
                        "first-of-type" => Pseudo::FirstOfType,
                        "last-of-type" => Pseudo::LastOfType,
                        "nth-child" => nth(&arg).map_or(Pseudo::Never, |(a, b)| Pseudo::NthChild(a, b)),
                        "nth-last-child" => nth(&arg).map_or(Pseudo::Never, |(a, b)| Pseudo::NthLastChild(a, b)),
                        "nth-of-type" => nth(&arg).map_or(Pseudo::Never, |(a, b)| Pseudo::NthOfType(a, b)),
                        "not" => Pseudo::Not(compounds(&arg)?),
                        "is" | "where" | "matches" | "-webkit-any" | "-moz-any" => match compounds(&arg) {
                            Some(v) => Pseudo::Is(v),
                            None => Pseudo::Never,
                        },
                        "link" | "any-link" => Pseudo::Link,
                        "root" => Pseudo::Root,
                        "empty" => Pseudo::Empty,
                        "checked" => Pseudo::Checked,
                        "disabled" => Pseudo::Disabled,
                        "enabled" => Pseudo::Enabled,
                        "lang" | "dir" | "defined" | "scope" => Pseudo::Is(Vec::new()),
                        _ => Pseudo::Never,
                    }
                };
                c.pseudo.push(p);
                i = j;
            }
            _ => break,
        }
    }
    Some((c, i, spec))
}

/// The compounds of a simple selector list (inside :not() and :is()).
fn compounds(arg: &str) -> Option<Vec<Compound>> {
    let mut out = Vec::new();
    for p in split_top(arg, ',') {
        let chars: Vec<char> = p.trim().chars().collect();
        let (c, used, _) = parse_compound(&chars)?;
        if used != chars.len() {
            return None;
        }
        out.push(c);
    }
    Some(out)
}

fn nth(arg: &str) -> Option<(i32, i32)> {
    let a = arg.trim().to_ascii_lowercase().replace(' ', "");
    let a = a.split("of").next().unwrap_or("");
    match a {
        "odd" => return Some((2, 1)),
        "even" => return Some((2, 0)),
        _ => {}
    }
    if let Some(p) = a.find('n') {
        let k = match &a[..p] {
            "" | "+" => 1,
            "-" => -1,
            s => s.parse().ok()?,
        };
        let b = if a[p + 1..].is_empty() { 0 } else { a[p + 1..].parse().ok()? };
        Some((k, b))
    } else {
        Some((0, a.parse().ok()?))
    }
}

fn parse_attr(s: &str) -> Option<AttrSel> {
    let s = s.trim();
    let ops = [("~=", AttrOp::Word), ("^=", AttrOp::Prefix), ("$=", AttrOp::Suffix), ("*=", AttrOp::Contains), ("|=", AttrOp::Dash), ("=", AttrOp::Equals)];
    for (tok, op) in ops {
        if let Some(p) = s.find(tok) {
            let name = s[..p].trim().to_ascii_lowercase();
            let mut v = s[p + tok.len()..].trim();
            let mut ci = false;
            if v.ends_with(" i") || v.ends_with(" I") {
                ci = true;
                v = v[..v.len() - 2].trim();
            } else if v.ends_with(" s") {
                v = v[..v.len() - 2].trim();
            }
            let v = v.trim_matches(['"', '\'']);
            return Some(AttrSel { name, op, value: v.into(), ci });
        }
    }
    Some(AttrSel { name: s.to_ascii_lowercase(), op: AttrOp::Exists, value: String::new(), ci: false })
}

// Matching.

pub fn matches(dom: &Dom, node: NodeId, sel: &Selector) -> bool {
    match_from(dom, node, &sel.parts, sel.parts.len() - 1)
}

fn match_from(dom: &Dom, node: NodeId, parts: &[(Combinator, Compound)], i: usize) -> bool {
    if !compound_matches(dom, node, &parts[i].1) {
        return false;
    }
    if i == 0 {
        return true;
    }
    match parts[i].0 {
        Combinator::Child => dom.parent_element(node).is_some_and(|p| match_from(dom, p, parts, i - 1)),
        Combinator::Descendant => {
            let mut p = dom.parent_element(node);
            while let Some(n) = p {
                if match_from(dom, n, parts, i - 1) {
                    return true;
                }
                p = dom.parent_element(n);
            }
            false
        }
        Combinator::Adjacent => prev_element(dom, node).is_some_and(|p| match_from(dom, p, parts, i - 1)),
        Combinator::Sibling => {
            let mut p = prev_element(dom, node);
            while let Some(n) = p {
                if match_from(dom, n, parts, i - 1) {
                    return true;
                }
                p = prev_element(dom, n);
            }
            false
        }
    }
}

fn prev_element(dom: &Dom, node: NodeId) -> Option<NodeId> {
    let parent = dom.nodes[node].parent?;
    let sib = &dom.nodes[parent].children;
    let pos = sib.iter().position(|&c| c == node)?;
    sib[..pos].iter().rev().copied().find(|&c| matches!(dom.nodes[c].data, Data::Element(_)))
}

/// (position among element siblings from 1, count, position among the
/// same tag, count of the same tag).
fn position(dom: &Dom, node: NodeId) -> (i32, i32, i32, i32) {
    let Some(parent) = dom.nodes[node].parent else { return (1, 1, 1, 1) };
    let tag = dom.tag(node);
    let (mut idx, mut n, mut tidx, mut tn) = (0, 0, 0, 0);
    for c in dom.element_children(parent) {
        n += 1;
        let same = dom.tag(c) == tag;
        if same {
            tn += 1;
        }
        if c == node {
            idx = n;
            tidx = tn;
        }
    }
    (idx, n, tidx, tn)
}

fn nth_ok(a: i32, b: i32, pos: i32) -> bool {
    if a == 0 {
        return pos == b;
    }
    let d = pos - b;
    d % a == 0 && d / a >= 0
}

pub fn compound_matches(dom: &Dom, node: NodeId, c: &Compound) -> bool {
    let Some(e) = dom.element(node) else { return false };
    if let Some(t) = &c.tag {
        if *t != e.tag {
            return false;
        }
    }
    if let Some(id) = &c.id {
        if e.attr("id") != Some(id.as_str()) {
            return false;
        }
    }
    for cl in &c.classes {
        if !e.has_class(cl) {
            return false;
        }
    }
    for a in &c.attrs {
        let Some(v) = e.attr(&a.name) else { return false };
        let (v, want) = if a.ci { (v.to_ascii_lowercase(), a.value.to_ascii_lowercase()) } else { (v.to_string(), a.value.clone()) };
        let ok = match a.op {
            AttrOp::Exists => true,
            AttrOp::Equals => v == want,
            AttrOp::Word => v.split_ascii_whitespace().any(|w| w == want),
            AttrOp::Prefix => !want.is_empty() && v.starts_with(&want),
            AttrOp::Suffix => !want.is_empty() && v.ends_with(&want),
            AttrOp::Contains => !want.is_empty() && v.contains(&want),
            AttrOp::Dash => v == want || v.starts_with(&alloc::format!("{want}-")),
        };
        if !ok {
            return false;
        }
    }
    for p in &c.pseudo {
        let ok = match p {
            Pseudo::Never => false,
            Pseudo::FirstChild => position(dom, node).0 == 1,
            Pseudo::LastChild => {
                let p = position(dom, node);
                p.0 == p.1
            }
            Pseudo::OnlyChild => position(dom, node).1 == 1,
            Pseudo::FirstOfType => position(dom, node).2 == 1,
            Pseudo::LastOfType => {
                let p = position(dom, node);
                p.2 == p.3
            }
            Pseudo::NthChild(a, b) => nth_ok(*a, *b, position(dom, node).0),
            Pseudo::NthLastChild(a, b) => {
                let p = position(dom, node);
                nth_ok(*a, *b, p.1 - p.0 + 1)
            }
            Pseudo::NthOfType(a, b) => nth_ok(*a, *b, position(dom, node).2),
            Pseudo::Not(list) => !list.iter().any(|c| compound_matches(dom, node, c)),
            Pseudo::Is(list) => list.is_empty() || list.iter().any(|c| compound_matches(dom, node, c)),
            Pseudo::Link => matches!(e.tag.as_str(), "a" | "area") && e.attr("href").is_some(),
            Pseudo::Root => e.tag == "html",
            Pseudo::Empty => dom.nodes[node].children.iter().all(|&c| matches!(&dom.nodes[c].data, Data::Text(t) if t.is_empty())),
            Pseudo::Checked => e.attr("checked").is_some() || e.attr("selected").is_some(),
            Pseudo::Disabled => e.attr("disabled").is_some(),
            Pseudo::Enabled => e.attr("disabled").is_none() && matches!(e.tag.as_str(), "input" | "button" | "select" | "textarea"),
        };
        if !ok {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::html;

    #[test]
    fn rules() {
        let s = parse("/* c */ a, p > b.x { color: red !important; margin: 0 } @media (max-width: 600px) { p { color: blue } } @import url(x.css); .n { & .m { color: green } }");
        assert_eq!(s.rules.len(), 3);
        assert_eq!(s.rules[2].selectors.len(), 1);
        assert_eq!(s.rules[0].selectors.len(), 2);
        assert!(s.rules[0].decls[0].important);
        assert_eq!(s.rules[1].media, ["(max-width: 600px)"]);
        assert!(!media_ok("(max-width: 600px)", Viewport { width: 1000.0, height: 700.0 }));
        assert!(media_ok("screen and (min-width: 40em)", Viewport { width: 1000.0, height: 700.0 }));
        assert!(media_ok("(width >= 600px)", Viewport { width: 1000.0, height: 700.0 }));
        assert_eq!(s.imports, ["x.css"]);
    }

    #[test]
    fn matching() {
        let d = html::parse("<div id=a class='x y'><p>1</p><p class=z>2</p><span data-k='v-1'>3</span></div>");
        let ps: Vec<_> = d.descendants(0).into_iter().filter(|&n| d.tag(n) == "p").collect();
        let span = d.find("span").unwrap();
        let t = |s: &str, n| matches(&d, n, &parse_selector_list(s)[0]);
        assert!(t("#a > p", ps[0]));
        assert!(t("div.x.y p:first-child", ps[0]));
        assert!(!t("p:first-child", ps[1]));
        assert!(t("p + p.z", ps[1]));
        assert!(t("p ~ span[data-k|=v]", span));
        assert!(t("p:not(.z)", ps[0]));
        assert!(!t("p:not(.z)", ps[1]));
        assert!(t("div :nth-child(2n+1)", span));
        assert!(!t("a:hover", ps[0]));
    }
}

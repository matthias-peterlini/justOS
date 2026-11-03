//! HTML into a [`Dom`]: a forgiving tokenizer and a tree builder that
//! follows the common rules of the HTML standard (implied `html`, `head`
//! and `body`, paragraphs and list items closed by what follows them, void
//! elements, raw text in `script` and `style`), without its full state
//! machine. Broken markup still gives a reasonable tree.

use crate::dom::{Data, Dom, Element, NodeId};
use alloc::string::{String, ToString};
use alloc::vec::Vec;

const VOID: &[&str] = &["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr", "keygen"];
const RAW: &[&str] = &["script", "style", "xmp", "iframe", "noembed", "noframes", "template"];
const RCDATA: &[&str] = &["textarea", "title"];
const HEAD: &[&str] = &["meta", "link", "title", "style", "script", "base", "noscript"];
/// Elements that end an open paragraph.
const CLOSES_P: &[&str] = &[
    "address", "article", "aside", "blockquote", "center", "details", "dialog", "dir", "div", "dl", "fieldset", "figcaption", "figure", "footer", "form", "h1", "h2", "h3",
    "h4", "h5", "h6", "header", "hgroup", "hr", "main", "menu", "nav", "ol", "p", "pre", "section", "summary", "table", "ul", "li", "dd", "dt", "listing", "plaintext",
];
/// Where the search for an element to close stops.
const SCOPE: &[&str] = &["html", "table", "td", "th", "caption", "marquee", "object", "template", "button"];
const MAX_DEPTH: usize = 256;

/// Parses a document.
pub fn parse(src: &str) -> Dom {
    let mut b = Builder::new();
    let mut t = Tokenizer { s: src, i: 0 };
    while let Some(tok) = t.next(b.raw.as_deref()) {
        b.token(tok);
    }
    b.dom
}

/// Parses a fragment of HTML into the children of `parent`.
pub fn parse_into(dom: &mut Dom, parent: NodeId, src: &str) {
    let mut b = Builder { dom: core::mem::take(dom), stack: alloc::vec![parent], head: parent, body: Some(parent), raw: None };
    let mut t = Tokenizer { s: src, i: 0 };
    while let Some(tok) = t.next(b.raw.as_deref()) {
        b.token(tok);
    }
    *dom = b.dom;
}

enum Token {
    Start(String, Vec<(String, String)>, bool),
    End(String),
    Text(String),
}

struct Tokenizer<'a> {
    s: &'a str,
    i: usize,
}

impl<'a> Tokenizer<'a> {
    fn rest(&self) -> &'a str {
        &self.s[self.i..]
    }

    fn next(&mut self, raw: Option<&str>) -> Option<Token> {
        if self.i >= self.s.len() {
            return None;
        }
        if let Some(tag) = raw {
            // Raw text up to the closing tag.
            let rest = self.rest();
            let end = find_ci(rest, &alloc::format!("</{tag}")).unwrap_or(rest.len());
            if end > 0 {
                let text = &rest[..end];
                self.i += end;
                let text = if RCDATA.contains(&tag) { decode_entities(text) } else { text.to_string() };
                return Some(Token::Text(text));
            }
        }
        let rest = self.rest();
        if rest.starts_with('<') {
            let b = rest.as_bytes();
            if rest.starts_with("<!--") {
                let end = rest[4..].find("-->").map_or(rest.len(), |e| e + 7);
                self.i += end;
                return self.next(raw);
            }
            if rest.starts_with("<!") || rest.starts_with("<?") {
                let end = rest.find('>').map_or(rest.len(), |e| e + 1);
                self.i += end;
                return self.next(raw);
            }
            if b.len() > 2 && b[1] == b'/' && b[2].is_ascii_alphabetic() {
                let end = rest.find('>').map_or(rest.len(), |e| e + 1);
                let name: String = rest[2..end].chars().take_while(|c| !c.is_whitespace() && *c != '>' && *c != '/').collect();
                self.i += end;
                return Some(Token::End(name.to_ascii_lowercase()));
            }
            if b.len() > 1 && b[1].is_ascii_alphabetic() {
                return Some(self.start_tag());
            }
        }
        // Text up to the next tag.
        let rest = self.rest();
        let mut end = rest.len();
        let mut from = if rest.starts_with('<') { 1 } else { 0 };
        while let Some(p) = rest[from..].find('<') {
            let at = from + p;
            let nb = rest.as_bytes().get(at + 1).copied().unwrap_or(0);
            if nb.is_ascii_alphabetic() || nb == b'/' || nb == b'!' || nb == b'?' {
                end = at;
                break;
            }
            from = at + 1;
        }
        let text = &rest[..end];
        self.i += end;
        Some(Token::Text(decode_entities(text)))
    }

    fn start_tag(&mut self) -> Token {
        let s = self.s.as_bytes();
        let mut i = self.i + 1;
        let start = i;
        while i < s.len() && !s[i].is_ascii_whitespace() && s[i] != b'>' && s[i] != b'/' {
            i += 1;
        }
        let name = self.s[start..i].to_ascii_lowercase();
        let mut attrs: Vec<(String, String)> = Vec::new();
        let mut self_closing = false;
        loop {
            while i < s.len() && (s[i].is_ascii_whitespace() || (s[i] == b'/' && s.get(i + 1) != Some(&b'>'))) {
                i += 1;
            }
            if i >= s.len() {
                break;
            }
            if s[i] == b'>' {
                i += 1;
                break;
            }
            if s[i] == b'/' {
                self_closing = true;
                i += 2;
                break;
            }
            let ks = i;
            while i < s.len() && !s[i].is_ascii_whitespace() && s[i] != b'=' && s[i] != b'>' && !(s[i] == b'/' && s.get(i + 1) == Some(&b'>')) {
                i += 1;
            }
            let key = self.s[ks..i].to_ascii_lowercase();
            while i < s.len() && s[i].is_ascii_whitespace() {
                i += 1;
            }
            let mut value = String::new();
            if i < s.len() && s[i] == b'=' {
                i += 1;
                while i < s.len() && s[i].is_ascii_whitespace() {
                    i += 1;
                }
                if i < s.len() && (s[i] == b'"' || s[i] == b'\'') {
                    let q = s[i];
                    i += 1;
                    let vs = i;
                    while i < s.len() && s[i] != q {
                        i += 1;
                    }
                    value = decode_entities(&self.s[vs..i]);
                    i = (i + 1).min(s.len());
                } else {
                    let vs = i;
                    while i < s.len() && !s[i].is_ascii_whitespace() && s[i] != b'>' {
                        i += 1;
                    }
                    value = decode_entities(&self.s[vs..i]);
                }
            }
            if !key.is_empty() && !attrs.iter().any(|(k, _)| *k == key) {
                attrs.push((key, value));
            }
        }
        // Keep the index on a character boundary.
        while i < s.len() && !self.s.is_char_boundary(i) {
            i += 1;
        }
        self.i = i.min(s.len());
        Token::Start(name, attrs, self_closing)
    }
}

fn find_ci(hay: &str, needle: &str) -> Option<usize> {
    let h = hay.as_bytes();
    let n = needle.as_bytes();
    if n.len() > h.len() {
        return None;
    }
    (0..=h.len() - n.len()).find(|&i| h[i..i + n.len()].eq_ignore_ascii_case(n))
}

struct Builder {
    dom: Dom,
    stack: Vec<NodeId>,
    head: NodeId,
    body: Option<NodeId>,
    /// Inside a raw text element: its tag.
    raw: Option<String>,
}

impl Builder {
    fn new() -> Builder {
        let mut dom = Dom::new();
        let html = dom.add(Dom::ROOT, Data::Element(Element { tag: "html".into(), attrs: Vec::new() }));
        let head = dom.add(html, Data::Element(Element { tag: "head".into(), attrs: Vec::new() }));
        Builder { dom, stack: alloc::vec![html], head, body: None, raw: None }
    }

    fn html(&self) -> NodeId {
        self.dom.nodes[Dom::ROOT].children[0]
    }

    fn body(&mut self) -> NodeId {
        if let Some(b) = self.body {
            return b;
        }
        let html = self.html();
        let b = self.dom.add(html, Data::Element(Element { tag: "body".into(), attrs: Vec::new() }));
        self.body = Some(b);
        self.stack = alloc::vec![html, b];
        b
    }

    fn current(&mut self) -> NodeId {
        if self.body.is_none() {
            return self.body();
        }
        *self.stack.last().unwrap_or(&self.html())
    }

    fn open(&self, tag: &str) -> Option<usize> {
        for (i, &n) in self.stack.iter().enumerate().rev() {
            let t = self.dom.tag(n);
            if t == tag {
                return Some(i);
            }
            if SCOPE.contains(&t) || t == "body" {
                return None;
            }
        }
        None
    }

    fn close_to(&mut self, index: usize) {
        self.stack.truncate(index.max(1));
    }

    fn close(&mut self, tag: &str) {
        if let Some(i) = self.open(tag) {
            self.close_to(i);
        }
    }

    /// Closes the nearest of these elements, if open before `limit`.
    fn close_any(&mut self, tags: &[&str], limits: &[&str]) {
        for (i, &n) in self.stack.iter().enumerate().rev() {
            let t = self.dom.tag(n);
            if tags.contains(&t) {
                self.close_to(i);
                return;
            }
            if limits.contains(&t) || SCOPE.contains(&t) || t == "body" {
                return;
            }
        }
    }

    fn token(&mut self, tok: Token) {
        match tok {
            Token::Text(t) => {
                if self.raw.is_some() {
                    let cur = *self.stack.last().unwrap_or(&self.head);
                    // Templates and frames are not kept (scripts are, for
                    // the script engine).
                    if !matches!(self.dom.tag(cur), "template" | "iframe" | "noembed" | "noframes") {
                        self.text(cur, &t);
                    }
                    return;
                }
                if self.body.is_none() && t.trim().is_empty() {
                    return;
                }
                let cur = self.current();
                let tag = self.dom.tag(cur);
                if matches!(tag, "table" | "tbody" | "thead" | "tfoot" | "tr") && t.trim().is_empty() {
                    return;
                }
                self.text(cur, &t);
            }
            Token::Start(name, attrs, self_closing) => self.start(name, attrs, self_closing),
            Token::End(name) => self.end(&name),
        }
    }

    fn text(&mut self, parent: NodeId, t: &str) {
        if let Some(&last) = self.dom.nodes[parent].children.last() {
            if let Data::Text(prev) = &mut self.dom.nodes[last].data {
                prev.push_str(t);
                return;
            }
        }
        self.dom.add(parent, Data::Text(t.into()));
    }

    fn start(&mut self, name: String, attrs: Vec<(String, String)>, self_closing: bool) {
        match name.as_str() {
            "html" => {
                let html = self.html();
                merge_attrs(&mut self.dom, html, attrs);
                return;
            }
            "head" => return,
            "body" => {
                let b = self.body();
                merge_attrs(&mut self.dom, b, attrs);
                return;
            }
            _ => {}
        }
        if self.body.is_none() && HEAD.contains(&name.as_str()) {
            let id = self.dom.add(self.head, Data::Element(Element { tag: name.clone(), attrs }));
            if RAW.contains(&name.as_str()) || RCDATA.contains(&name.as_str()) {
                self.stack.push(id);
                self.raw = Some(name);
            }
            return;
        }
        self.body();
        let n = name.as_str();
        if CLOSES_P.contains(&n) {
            self.close("p");
        }
        match n {
            "li" => self.close_any(&["li"], &["ul", "ol", "menu"]),
            "dt" | "dd" => self.close_any(&["dt", "dd"], &["dl"]),
            "option" => self.close_any(&["option"], &["select", "datalist", "optgroup"]),
            "optgroup" => self.close_any(&["optgroup", "option"], &["select"]),
            "tr" => {
                self.close_any(&["td", "th"], &["table"]);
                self.close_any(&["tr"], &["table"]);
            }
            "td" | "th" => self.close_any(&["td", "th"], &["tr", "table"]),
            "thead" | "tbody" | "tfoot" => {
                self.close_any(&["td", "th"], &["table"]);
                self.close_any(&["tr"], &["table"]);
                self.close_any(&["thead", "tbody", "tfoot"], &["table"]);
            }
            "a" => self.close("a"),
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => self.close_any(&["h1", "h2", "h3", "h4", "h5", "h6"], &[]),
            "button" => self.close("button"),
            _ => {}
        }
        let parent = self.current();
        let id = self.dom.add(parent, Data::Element(Element { tag: name.clone(), attrs }));
        if VOID.contains(&n) || (self_closing && !matches!(n, "div" | "span" | "a" | "p")) || self.stack.len() >= MAX_DEPTH {
            return;
        }
        self.stack.push(id);
        if RAW.contains(&n) || RCDATA.contains(&n) {
            self.raw = Some(name);
        }
    }

    fn end(&mut self, name: &str) {
        if let Some(raw) = &self.raw {
            if raw == name {
                self.raw = None;
                self.stack.pop();
            }
            return;
        }
        match name {
            "html" | "body" | "head" => {}
            "br" => self.start("br".into(), Vec::new(), true),
            "p" => {
                if self.open("p").is_none() {
                    self.body();
                    let parent = self.current();
                    self.dom.add(parent, Data::Element(Element { tag: "p".into(), attrs: Vec::new() }));
                } else {
                    self.close("p");
                }
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => self.close_any(&["h1", "h2", "h3", "h4", "h5", "h6"], &[]),
            _ => {
                // Close the element if open (formatting elements anywhere
                // up the stack, the rest only within scope).
                for (i, &n) in self.stack.iter().enumerate().rev() {
                    let t = self.dom.tag(n);
                    if t == name {
                        self.close_to(i);
                        return;
                    }
                    if (SCOPE.contains(&t) || t == "body") && t != "button" {
                        return;
                    }
                }
            }
        }
    }
}

fn merge_attrs(dom: &mut Dom, id: NodeId, attrs: Vec<(String, String)>) {
    for (k, v) in attrs {
        if dom.attr(id, &k).is_none() {
            dom.set_attr(id, &k, &v);
        }
    }
}

const ENTITIES: &[(&str, &str)] = &[
    ("amp", "&"), ("lt", "<"), ("gt", ">"), ("quot", "\""), ("apos", "'"), ("nbsp", "\u{a0}"), ("copy", "©"), ("reg", "®"), ("trade", "™"),
    ("hellip", "…"), ("mdash", "—"), ("ndash", "–"), ("lsquo", "‘"), ("rsquo", "’"), ("ldquo", "“"), ("rdquo", "”"), ("sbquo", "‚"), ("bdquo", "„"),
    ("laquo", "«"), ("raquo", "»"), ("lsaquo", "‹"), ("rsaquo", "›"), ("bull", "•"), ("middot", "·"), ("deg", "°"), ("plusmn", "±"), ("times", "×"),
    ("divide", "÷"), ("euro", "€"), ("pound", "£"), ("yen", "¥"), ("cent", "¢"), ("sect", "§"), ("para", "¶"), ("dagger", "†"), ("Dagger", "‡"),
    ("larr", "←"), ("rarr", "→"), ("uarr", "↑"), ("darr", "↓"), ("harr", "↔"), ("minus", "−"), ("le", "≤"), ("ge", "≥"), ("ne", "≠"), ("asymp", "≈"),
    ("infin", "∞"), ("frac12", "½"), ("frac14", "¼"), ("frac34", "¾"), ("sup1", "¹"), ("sup2", "²"), ("sup3", "³"), ("micro", "µ"), ("iexcl", "¡"),
    ("iquest", "¿"), ("shy", "\u{ad}"), ("zwj", ""), ("zwnj", ""), ("lrm", ""), ("rlm", ""), ("thinsp", " "), ("ensp", " "), ("emsp", " "),
    ("agrave", "à"), ("aacute", "á"), ("acirc", "â"), ("atilde", "ã"), ("auml", "ä"), ("aring", "å"), ("aelig", "æ"), ("ccedil", "ç"), ("egrave", "è"),
    ("eacute", "é"), ("ecirc", "ê"), ("euml", "ë"), ("igrave", "ì"), ("iacute", "í"), ("icirc", "î"), ("iuml", "ï"), ("ntilde", "ñ"), ("ograve", "ò"),
    ("oacute", "ó"), ("ocirc", "ô"), ("otilde", "õ"), ("ouml", "ö"), ("oslash", "ø"), ("ugrave", "ù"), ("uacute", "ú"), ("ucirc", "û"), ("uuml", "ü"),
    ("yacute", "ý"), ("yuml", "ÿ"), ("szlig", "ß"), ("Agrave", "À"), ("Aacute", "Á"), ("Acirc", "Â"), ("Auml", "Ä"), ("Ccedil", "Ç"), ("Egrave", "È"),
    ("Eacute", "É"), ("Igrave", "Ì"), ("Ograve", "Ò"), ("Oacute", "Ó"), ("Ouml", "Ö"), ("Ugrave", "Ù"), ("Uuml", "Ü"), ("Ntilde", "Ñ"), ("ordf", "ª"),
    ("ordm", "º"), ("not", "¬"), ("check", "✓"), ("star", "☆"), ("starf", "★"), ("hearts", "♥"), ("prime", "′"), ("Prime", "″"), ("permil", "‰"),
    ("acute", "´"), ("cedil", "¸"), ("uml", "¨"), ("macr", "¯"), ("curren", "¤"), ("brvbar", "¦"), ("colon", ":"), ("comma", ","), ("period", "."),
    ("excl", "!"), ("quest", "?"), ("num", "#"), ("dollar", "$"), ("percnt", "%"), ("lpar", "("), ("rpar", ")"), ("ast", "*"), ("plus", "+"),
    ("sol", "/"), ("semi", ";"), ("equals", "="), ("commat", "@"), ("lsqb", "["), ("rsqb", "]"), ("lowbar", "_"), ("grave", "`"), ("lcub", "{"),
    ("rcub", "}"), ("verbar", "|"), ("Tab", "\t"), ("NewLine", "\n"),
];

/// Replaces character references (`&amp;`, `&#233;`, `&#x2014;`).
pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.into();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(p) = rest.find('&') {
        out.push_str(&rest[..p]);
        rest = &rest[p..];
        let body = &rest[1..];
        if let Some(num) = body.strip_prefix('#') {
            let (hex, digits) = match num.strip_prefix(['x', 'X']) {
                Some(h) => (true, h),
                None => (false, num),
            };
            let len = digits.chars().take_while(|c| if hex { c.is_ascii_hexdigit() } else { c.is_ascii_digit() }).count();
            if len > 0 {
                let v = u32::from_str_radix(&digits[..len], if hex { 16 } else { 10 }).unwrap_or(0xFFFD);
                let v = match v {
                    0x80..=0x9F => windows_1252(v),
                    0 => 0xFFFD,
                    v => v,
                };
                out.push(char::from_u32(v).unwrap_or('\u{FFFD}'));
                let used = 1 + (if hex { 2 } else { 1 }) + len;
                rest = &rest[used..];
                if rest.starts_with(';') {
                    rest = &rest[1..];
                }
                continue;
            }
        } else {
            let len = body.chars().take_while(|c| c.is_ascii_alphanumeric()).count();
            let name = &body[..len];
            if let Some((_, v)) = ENTITIES.iter().find(|(k, _)| *k == name) {
                out.push_str(v);
                rest = &rest[1 + len..];
                if rest.starts_with(';') {
                    rest = &rest[1..];
                }
                continue;
            }
        }
        out.push('&');
        rest = &rest[1..];
    }
    out.push_str(rest);
    out
}

fn windows_1252(v: u32) -> u32 {
    const T: [u16; 32] = [
        0x20AC, 0x81, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039, 0x0152, 0x8D, 0x017D, 0x8F, 0x90, 0x2018, 0x2019, 0x201C,
        0x201D, 0x2022, 0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x9D, 0x017E, 0x0178,
    ];
    T[(v - 0x80) as usize] as u32
}

/// Decodes the bytes of a page: UTF-8, or Windows-1252 (Latin-1) when the
/// page says so or is not valid UTF-8.
pub fn decode(bytes: &[u8], charset: Option<&str>) -> String {
    let latin = charset.is_some_and(|c| {
        let c = c.to_ascii_lowercase();
        c.contains("8859") || c.contains("1252") || c.contains("latin")
    });
    if !latin {
        if let Ok(s) = core::str::from_utf8(bytes) {
            return s.strip_prefix('\u{feff}').unwrap_or(s).into();
        }
        if charset.is_some_and(|c| c.eq_ignore_ascii_case("utf-8")) {
            return String::from_utf8_lossy(bytes).into_owned();
        }
    }
    bytes.iter().map(|&b| if (0x80..0xA0).contains(&b) { char::from_u32(windows_1252(b as u32)).unwrap_or('?') } else { b as char }).collect()
}

/// The charset a page declares in a `<meta>` near its start.
pub fn sniff_charset(bytes: &[u8]) -> Option<String> {
    let head = &bytes[..bytes.len().min(2048)];
    let s = String::from_utf8_lossy(head).to_ascii_lowercase();
    let p = s.find("charset=")?;
    let v: String = s[p + 8..].trim_start_matches(['"', '\'']).chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    (!v.is_empty()).then_some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree() {
        let d = parse("<!doctype html><title>T &amp; U</title><p>One<p>Two <b>bold</b><ul><li>a<li>b</ul><img src=x.png alt='A'>");
        let body = d.find("body").unwrap();
        let tags: Vec<&str> = d.element_children(body).map(|n| d.tag(n)).collect();
        assert_eq!(tags, ["p", "p", "ul", "img"]);
        assert_eq!(d.text(d.find("title").unwrap()), "T & U");
        let ul = d.find("ul").unwrap();
        assert_eq!(d.element_children(ul).count(), 2);
        assert_eq!(d.attr(d.find("img").unwrap(), "alt"), Some("A"));
    }

    #[test]
    fn raw_and_entities() {
        let d = parse("<script>if (a < b) { x = '</p>'; }</script><style>p > a { color: red }</style><p>&lt;a&gt; &#233;&#x2014;&eacute;&unknown;");
        let style = d.find("style").unwrap();
        assert_eq!(d.text(style), "p > a { color: red }");
        assert_eq!(d.text(d.find("p").unwrap()), "<a> é—é&unknown;");
    }

    #[test]
    fn tables() {
        let d = parse("<table><tr><td>1<td>2<tr><td>3</table>");
        let trs: Vec<_> = d.descendants(0).into_iter().filter(|&n| d.tag(n) == "tr").collect();
        assert_eq!(trs.len(), 2);
        assert_eq!(d.element_children(trs[0]).count(), 2);
    }
}

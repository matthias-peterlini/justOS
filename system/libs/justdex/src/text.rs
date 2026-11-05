//! Text as Java sees it: UTF-16 positions, numbers written the way Java
//! writes them, `String.format` and the regular expressions of `split`,
//! `matches` and `replaceAll`.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// The length in UTF-16 units.
pub fn len(s: &str) -> usize {
    if s.is_ascii() {
        s.len()
    } else {
        s.encode_utf16().count()
    }
}

pub fn units(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

pub fn from_units(u: &[u16]) -> String {
    String::from_utf16_lossy(u)
}

/// The UTF-16 unit at `i`.
pub fn char_at(s: &str, i: usize) -> Option<u16> {
    if s.is_ascii() {
        s.as_bytes().get(i).map(|&b| b as u16)
    } else {
        s.encode_utf16().nth(i)
    }
}

/// Units `from..to`.
pub fn sub(s: &str, from: usize, to: usize) -> Option<String> {
    if from > to {
        return None;
    }
    if s.is_ascii() {
        return s.get(from..to).map(String::from);
    }
    let u = units(s);
    u.get(from..to).map(from_units)
}

/// A byte offset as a UTF-16 position.
pub fn pos(s: &str, byte: usize) -> usize {
    if s.is_ascii() {
        byte
    } else {
        s[..byte].encode_utf16().count()
    }
}

/// A UTF-16 position as a byte offset.
pub fn byte_at(s: &str, unit: usize) -> usize {
    if s.is_ascii() {
        return unit.min(s.len());
    }
    let mut n = 0;
    for (b, c) in s.char_indices() {
        if n >= unit {
            return b;
        }
        n += c.len_utf16();
    }
    s.len()
}

pub fn hash(s: &str) -> i32 {
    s.encode_utf16().fold(0i32, |h, c| h.wrapping_mul(31).wrapping_add(c as i32))
}

pub fn char_string(c: u16) -> String {
    from_units(&[c])
}

/// `Double.toString`.
pub fn double(d: f64) -> String {
    if d.is_nan() {
        return "NaN".into();
    }
    if d.is_infinite() {
        return if d > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    if d == 0.0 {
        return if d.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    let a = d.abs();
    if (1e-3..1e7).contains(&a) {
        let s = format!("{d}");
        if s.contains('.') {
            s
        } else {
            s + ".0"
        }
    } else {
        sci(&format!("{d:e}"))
    }
}

/// `Float.toString`.
pub fn float(f: f32) -> String {
    if f.is_nan() || f.is_infinite() || f == 0.0 {
        return double(f as f64);
    }
    let a = f.abs();
    if (1e-3..1e7).contains(&a) {
        let s = format!("{f}");
        if s.contains('.') {
            s
        } else {
            s + ".0"
        }
    } else {
        sci(&format!("{f:e}"))
    }
}

/// "1.5e-5" → "1.5E-5", "1e10" → "1.0E10".
fn sci(s: &str) -> String {
    let (m, e) = s.split_once('e').unwrap_or((s, "0"));
    if m.contains('.') {
        format!("{m}E{e}")
    } else {
        format!("{m}.0E{e}")
    }
}

pub fn radix(mut v: u64, radix: u32, neg: bool) -> String {
    if v == 0 {
        return "0".into();
    }
    let mut out = Vec::new();
    while v > 0 {
        out.push(core::char::from_digit((v % radix as u64) as u32, radix).unwrap_or('?'));
        v /= radix as u64;
    }
    if neg {
        out.push('-');
    }
    out.iter().rev().collect()
}

/// `Long.parseLong` (None: a NumberFormatException).
pub fn parse_long(s: &str, radix: u32, min: i64, max: i64) -> Option<i64> {
    let (neg, digits) = match s.as_bytes().first()? {
        b'-' => (true, &s[1..]),
        b'+' => (false, &s[1..]),
        _ => (false, s),
    };
    if digits.is_empty() || !(2..=36).contains(&radix) {
        return None;
    }
    let mut v: i128 = 0;
    for c in digits.chars() {
        v = v * radix as i128 + c.to_digit(radix)? as i128;
        if v > (max as i128) + 1 {
            return None;
        }
    }
    let v = if neg { -v } else { v };
    (min as i128..=max as i128).contains(&v).then_some(v as i64)
}

/// `Double.parseDouble`.
pub fn parse_double(s: &str) -> Option<f64> {
    let t = s.trim();
    let t = t.strip_suffix(['d', 'D', 'f', 'F']).unwrap_or(t);
    match t {
        "NaN" => return Some(f64::NAN),
        "Infinity" | "+Infinity" => return Some(f64::INFINITY),
        "-Infinity" => return Some(f64::NEG_INFINITY),
        _ => {}
    }
    if t.chars().any(|c| c.is_ascii_alphabetic() && c != 'e' && c != 'E') {
        return None;
    }
    t.parse().ok()
}

/// One argument of `String.format`, already read from its box.
pub enum Arg {
    Int(i64),
    Float(f64),
    Bool(bool),
    Char(u16),
    /// Anything else, as its `toString`, and whether it was null.
    Text(String, bool),
}

/// `String.format` (None: a format error, with its message).
pub fn format(fmt: &str, args: &[Arg]) -> Result<String, String> {
    let mut out = String::new();
    let mut next = 0;
    let mut it = fmt.chars().peekable();
    while let Some(c) = it.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let mut spec = String::new();
        let conv = loop {
            match it.next() {
                Some(c) if c.is_ascii_alphabetic() || c == '%' => break c,
                Some(c) => spec.push(c),
                None => return Err(format!("Format specifier '%{spec}'")),
            }
        };
        if conv == 'n' {
            out.push('\n');
            continue;
        }
        if conv == '%' {
            out.push('%');
            continue;
        }
        // %[index$][flags][width][.precision]
        let mut index = None;
        if let Some((n, rest)) = spec.split_once('$') {
            index = n.parse::<usize>().ok().map(|n| n.saturating_sub(1));
            spec = rest.into();
        }
        let flags: String = spec.chars().take_while(|c| "-+0, #(".contains(*c)).collect();
        let rest = &spec[flags.len()..];
        let (width, prec) = match rest.split_once('.') {
            Some((w, p)) => (w.parse::<usize>().ok(), p.parse::<usize>().ok()),
            None => (rest.parse::<usize>().ok(), None),
        };
        let i = index.unwrap_or_else(|| {
            next += 1;
            next - 1
        });
        let arg = args.get(i).ok_or_else(|| format!("Format specifier '%{spec}{conv}'"))?;
        let plus = flags.contains('+');
        let group = flags.contains(',');
        let mut body = match (conv.to_ascii_lowercase(), arg) {
            ('d', Arg::Int(v)) => sign(group_digits(&v.unsigned_abs().to_string(), group), *v < 0, plus, flags.contains(' ')),
            ('x', Arg::Int(v)) => format!("{:x}", if *v < 0 { *v as u64 & if *v >= i32::MIN as i64 { 0xFFFF_FFFF } else { u64::MAX } } else { *v as u64 }),
            ('o', Arg::Int(v)) => format!("{:o}", *v as u64 & 0xFFFF_FFFF),
            ('f', Arg::Float(v)) | ('e', Arg::Float(v)) | ('g', Arg::Float(v)) if v.is_nan() || v.is_infinite() => double(*v),
            ('f', Arg::Float(v)) => {
                let s = format!("{:.*}", prec.unwrap_or(6), v.abs());
                let (int, frac) = s.split_once('.').map(|(a, b)| (a.to_string(), format!(".{b}"))).unwrap_or((s.clone(), String::new()));
                sign(group_digits(&int, group) + &frac, *v < 0.0 && s.chars().any(|c| c != '0' && c != '.'), plus, flags.contains(' '))
            }
            ('e', Arg::Float(v)) => {
                let s = format!("{:.*e}", prec.unwrap_or(6), v.abs());
                let (m, e) = s.split_once('e').unwrap_or((&s, "0"));
                let e: i32 = e.parse().unwrap_or(0);
                sign(format!("{m}e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs()), *v < 0.0, plus, false)
            }
            ('g', Arg::Float(v)) => format!("{:.*}", prec.unwrap_or(6), v),
            ('f' | 'e' | 'g', Arg::Int(v)) => return Err(format!("f != java.lang.{}", if *v > i32::MAX as i64 { "Long" } else { "Integer" })),
            ('d' | 'x' | 'o', Arg::Float(_)) => return Err("d != java.lang.Double".into()),
            ('c', Arg::Char(c)) => char_string(*c),
            ('c', Arg::Int(c)) => char::from_u32(*c as u32).map(String::from).unwrap_or_default(),
            ('b', Arg::Bool(b)) => b.to_string(),
            ('b', Arg::Text(_, null)) => (!null).to_string(),
            ('b', _) => "true".into(),
            ('s', Arg::Int(v)) => v.to_string(),
            ('s', Arg::Float(v)) => double(*v),
            ('s', Arg::Bool(b)) => b.to_string(),
            ('s', Arg::Char(c)) => char_string(*c),
            ('s', Arg::Text(t, _)) => t.clone(),
            (c, _) => return Err(format!("Conversion = '{c}'")),
        };
        if conv.is_ascii_uppercase() {
            body = body.to_uppercase();
        }
        if conv == 's' || conv == 'S' {
            if let Some(p) = prec {
                body = body.chars().take(p).collect();
            }
        }
        let w = width.unwrap_or(0);
        let n = len(&body);
        if n < w {
            let pad = w - n;
            if flags.contains('-') {
                body.extend(core::iter::repeat_n(' ', pad));
            } else if flags.contains('0') && conv != 's' {
                let at = body.find(|c: char| c.is_ascii_digit()).unwrap_or(0);
                body.insert_str(at, &"0".repeat(pad));
            } else {
                body.insert_str(0, &" ".repeat(pad));
            }
        }
        out.push_str(&body);
    }
    Ok(out)
}

fn sign(s: String, neg: bool, plus: bool, space: bool) -> String {
    if neg {
        format!("-{s}")
    } else if plus {
        format!("+{s}")
    } else if space {
        format!(" {s}")
    } else {
        s
    }
}

fn group_digits(s: &str, group: bool) -> String {
    if !group || s.len() <= 3 {
        return s.into();
    }
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

// ---- Regular expressions ------------------------------------------------

enum Node {
    Char(char),
    Any,
    /// Ranges, negated.
    Class(Vec<(char, char)>, bool),
    Start,
    End,
    Group(Vec<Vec<Item>>),
}

struct Item {
    node: Node,
    min: u32,
    max: u32,
    lazy: bool,
}

/// A small backtracking regex: literals, `.`, classes (`[a-z]`, `\d`, `\w`,
/// `\s` and their negations), groups, `|`, `^`, `$` and the quantifiers
/// `* + ? {n} {n,} {n,m}` (lazy with `?`).
pub struct Regex {
    alts: Vec<Vec<Item>>,
}

struct Parser<'a> {
    s: &'a [char],
    i: usize,
}

const DIGIT: &[(char, char)] = &[('0', '9')];
const WORD: &[(char, char)] = &[('a', 'z'), ('A', 'Z'), ('0', '9'), ('_', '_')];
const SPACE: &[(char, char)] = &[(' ', ' '), ('\t', '\r')];

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }

    fn alts(&mut self) -> Result<Vec<Vec<Item>>, String> {
        let mut alts = alloc::vec![self.seq()?];
        while self.peek() == Some('|') {
            self.i += 1;
            alts.push(self.seq()?);
        }
        Ok(alts)
    }

    fn seq(&mut self) -> Result<Vec<Item>, String> {
        let mut items = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            self.i += 1;
            let node = match c {
                '.' => Node::Any,
                '^' => Node::Start,
                '$' => Node::End,
                '(' => {
                    // (?:...) is the same group to us.
                    if self.s.get(self.i..self.i + 2) == Some(&['?', ':']) {
                        self.i += 2;
                    }
                    let alts = self.alts()?;
                    if self.peek() != Some(')') {
                        return Err("Unclosed group".into());
                    }
                    self.i += 1;
                    Node::Group(alts)
                }
                '[' => self.class()?,
                '\\' => self.escape()?,
                c => Node::Char(c),
            };
            let (mut min, mut max) = (1, 1);
            match self.peek() {
                Some('*') => (min, max) = (0, u32::MAX),
                Some('+') => (min, max) = (1, u32::MAX),
                Some('?') => (min, max) = (0, 1),
                Some('{') => {
                    let close = self.s[self.i..].iter().position(|&c| c == '}').ok_or("Unclosed counted closure")?;
                    let body: String = self.s[self.i + 1..self.i + close].iter().collect();
                    let (a, b) = match body.split_once(',') {
                        Some((a, "")) => (a.parse().map_err(|_| "bad count")?, u32::MAX),
                        Some((a, b)) => (a.parse().map_err(|_| "bad count")?, b.parse().map_err(|_| "bad count")?),
                        None => {
                            let n = body.parse().map_err(|_| "bad count")?;
                            (n, n)
                        }
                    };
                    (min, max) = (a, b);
                    self.i += close;
                }
                _ => {
                    items.push(Item { node, min, max, lazy: false });
                    continue;
                }
            }
            self.i += 1;
            let lazy = self.peek() == Some('?');
            if lazy {
                self.i += 1;
            }
            items.push(Item { node, min, max, lazy });
        }
        Ok(items)
    }

    fn escape_ranges(c: char) -> Option<(&'static [(char, char)], bool)> {
        Some(match c {
            'd' => (DIGIT, false),
            'D' => (DIGIT, true),
            'w' => (WORD, false),
            'W' => (WORD, true),
            's' => (SPACE, false),
            'S' => (SPACE, true),
            _ => return None,
        })
    }

    fn escaped_char(c: char) -> char {
        match c {
            'n' => '\n',
            't' => '\t',
            'r' => '\r',
            'f' => '\x0C',
            c => c,
        }
    }

    fn escape(&mut self) -> Result<Node, String> {
        let c = self.peek().ok_or("Unexpected internal error")?;
        self.i += 1;
        Ok(match Self::escape_ranges(c) {
            Some((r, neg)) => Node::Class(r.to_vec(), neg),
            None => Node::Char(Self::escaped_char(c)),
        })
    }

    fn class(&mut self) -> Result<Node, String> {
        let neg = self.peek() == Some('^');
        if neg {
            self.i += 1;
        }
        let mut ranges = Vec::new();
        let mut first = true;
        loop {
            let c = self.peek().ok_or("Unclosed character class")?;
            self.i += 1;
            if c == ']' && !first {
                break;
            }
            first = false;
            let lo = if c == '\\' {
                let e = self.peek().ok_or("Unclosed character class")?;
                self.i += 1;
                if let Some((r, false)) = Self::escape_ranges(e) {
                    ranges.extend_from_slice(r);
                    continue;
                }
                Self::escaped_char(e)
            } else {
                c
            };
            if self.peek() == Some('-') && self.s.get(self.i + 1).is_some_and(|&c| c != ']') {
                let mut hi = self.s[self.i + 1];
                self.i += 2;
                if hi == '\\' {
                    hi = Self::escaped_char(self.peek().unwrap_or('\\'));
                    self.i += 1;
                }
                ranges.push((lo, hi));
            } else {
                ranges.push((lo, lo));
            }
        }
        Ok(Node::Class(ranges, neg))
    }
}

type K<'k> = &'k mut dyn FnMut(usize) -> bool;

impl Regex {
    pub fn new(pattern: &str) -> Result<Regex, String> {
        let chars: Vec<char> = pattern.chars().collect();
        let mut p = Parser { s: &chars, i: 0 };
        let alts = p.alts()?;
        if p.i != chars.len() {
            return Err(format!("Unmatched closing ')' near index {}", p.i));
        }
        Ok(Regex { alts })
    }

    fn alts(&self, alts: &[Vec<Item>], s: &[char], i: usize, k: K<'_>) -> bool {
        alts.iter().any(|seq| self.seq(seq, 0, s, i, 0, k))
    }

    fn seq(&self, items: &[Item], idx: usize, s: &[char], i: usize, count: u32, k: K<'_>) -> bool {
        let Some(it) = items.get(idx) else { return k(i) };
        let more = |this: &Self, k: K<'_>| count < it.max && this.node(&it.node, s, i, &mut |j| (j > i || count < it.min) && this.seq(items, idx, s, j, count + 1, k));
        let done = |this: &Self, k: K<'_>| count >= it.min && this.seq(items, idx + 1, s, i, 0, k);
        // Lazy: as few repetitions as will do; greedy: as many.
        #[allow(clippy::if_same_then_else)]
        if it.lazy {
            done(self, k) || more(self, k)
        } else {
            more(self, k) || done(self, k)
        }
    }

    fn node(&self, n: &Node, s: &[char], i: usize, k: K<'_>) -> bool {
        match n {
            Node::Char(c) => s.get(i) == Some(c) && k(i + 1),
            Node::Any => s.get(i).is_some_and(|&c| c != '\n') && k(i + 1),
            Node::Class(r, neg) => s.get(i).is_some_and(|&c| r.iter().any(|&(lo, hi)| (lo..=hi).contains(&c)) != *neg) && k(i + 1),
            Node::Start => i == 0 && k(i),
            Node::End => i == s.len() && k(i),
            Node::Group(alts) => self.alts(alts, s, i, k),
        }
    }

    /// The longest (greedy) match starting at `i`: its end.
    fn match_at(&self, s: &[char], i: usize) -> Option<usize> {
        let mut end = None;
        self.alts(&self.alts, s, i, &mut |j| {
            end = Some(j);
            true
        });
        end
    }

    pub fn matches(&self, text: &str) -> bool {
        let s: Vec<char> = text.chars().collect();
        self.alts(&self.alts, &s, 0, &mut |j| j == s.len())
    }

    /// The matches in `text`, as char ranges.
    fn find_all(&self, s: &[char]) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut i = 0;
        while i <= s.len() {
            match self.match_at(s, i) {
                Some(j) => {
                    out.push((i, j));
                    i = if j > i { j } else { i + 1 };
                }
                None => i += 1,
            }
        }
        out
    }

    /// `String.split(regex, limit)`.
    pub fn split(&self, text: &str, limit: i32) -> Vec<String> {
        let s: Vec<char> = text.chars().collect();
        let mut parts = Vec::new();
        let mut last = 0;
        for (a, b) in self.find_all(&s) {
            if limit > 0 && parts.len() as i32 == limit - 1 {
                break;
            }
            // A zero-width match at the start gives no empty first part.
            if b == 0 || (a == b && a >= s.len()) {
                continue;
            }
            parts.push(s[last..a].iter().collect::<String>());
            last = b;
        }
        parts.push(s[last..].iter().collect());
        if limit == 0 {
            while parts.len() > 1 && parts.last().is_some_and(|p| p.is_empty()) {
                parts.pop();
            }
            if parts.len() == 1 && parts[0].is_empty() && !text.is_empty() {
                parts.pop();
            }
        }
        parts
    }

    /// `replaceAll` / `replaceFirst` (the replacement is taken literally,
    /// apart from `\` escapes).
    pub fn replace(&self, text: &str, with: &str, all: bool) -> String {
        let s: Vec<char> = text.chars().collect();
        let with: String = {
            let mut out = String::new();
            let mut it = with.chars();
            while let Some(c) = it.next() {
                if c == '\\' {
                    out.extend(it.next());
                } else {
                    out.push(c);
                }
            }
            out
        };
        let mut out = String::new();
        let mut last = 0;
        for (a, b) in self.find_all(&s) {
            out.extend(&s[last..a]);
            out.push_str(&with);
            last = b;
            if !all {
                break;
            }
        }
        out.extend(&s[last.min(s.len())..]);
        out
    }
}

/// A plain-text pattern needs no regex.
pub fn literal(pattern: &str) -> Option<String> {
    if !pattern.chars().any(|c| ".$|()[{^?*+\\".contains(c)) {
        return Some(pattern.into());
    }
    let mut it = pattern.chars();
    if let (Some('\\'), Some(c), None) = (it.next(), it.next(), it.next()) {
        if !c.is_ascii_alphanumeric() {
            return Some(c.to_string());
        }
    }
    None
}

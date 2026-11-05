//! Regular expressions: a backtracking matcher over characters.
//!
//! Classes, groups (named too), alternation, greedy and lazy repetition,
//! backreferences, lookahead and lookbehind, anchors and word boundaries;
//! flags `g i m s y` (`u` and `d` are accepted). Matching gives up after a
//! fixed amount of work rather than hang on a pathological pattern.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Debug)]
enum Item {
    Range(char, char),
    Digit(bool),
    Word(bool),
    Space(bool),
}

#[derive(Debug)]
enum Node {
    Char(char),
    Any,
    Class(bool, Vec<Item>),
    Start,
    End,
    WordB(bool),
    Group(Box<Node>, Option<usize>),
    /// Inner, positive, behind.
    Look(Box<Node>, bool, bool),
    Back(usize),
    Seq(Vec<Node>),
    Alt(Vec<Node>),
    Rep(Box<Node>, u32, u32, bool),
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Flags {
    pub global: bool,
    pub icase: bool,
    pub multiline: bool,
    pub dotall: bool,
    pub sticky: bool,
    pub unicode: bool,
}

#[derive(Debug)]
pub struct Regex {
    pub source: Rc<str>,
    pub flags_text: Rc<str>,
    pub flags: Flags,
    prog: Node,
    /// Capture groups, not counting the whole match.
    pub groups: usize,
    pub names: Vec<(String, usize)>,
}

/// Start and end of each group (0 is the whole match), in characters.
pub type Caps = Vec<Option<(usize, usize)>>;

struct P<'a> {
    s: &'a [char],
    i: usize,
    groups: usize,
    names: Vec<(String, usize)>,
    depth: u32,
}

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn is_space(c: char) -> bool {
    c.is_whitespace() || c == '\u{feff}'
}

fn is_line_end(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

impl P<'_> {
    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn alt(&mut self) -> Result<Node, String> {
        self.depth += 1;
        if self.depth > 200 {
            return Err(String::from("espressione regolare troppo annidata"));
        }
        let mut alts = Vec::new();
        loop {
            alts.push(self.seq()?);
            if !self.eat('|') {
                break;
            }
        }
        self.depth -= 1;
        Ok(if alts.len() == 1 { alts.pop().expect("one alternative") } else { Node::Alt(alts) })
    }

    fn seq(&mut self) -> Result<Node, String> {
        let mut items = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            let atom = self.atom()?;
            let atom = self.quantifier(atom)?;
            items.push(atom);
        }
        Ok(if items.len() == 1 { items.pop().expect("one item") } else { Node::Seq(items) })
    }

    fn number(&mut self) -> Option<u32> {
        let start = self.i;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
        }
        if start == self.i {
            return None;
        }
        self.s[start..self.i].iter().collect::<String>().parse().ok()
    }

    fn quantifier(&mut self, atom: Node) -> Result<Node, String> {
        let (min, max) = match self.peek() {
            Some('*') => (0, u32::MAX),
            Some('+') => (1, u32::MAX),
            Some('?') => (0, 1),
            Some('{') => {
                let save = self.i;
                self.i += 1;
                let Some(min) = self.number() else {
                    self.i = save;
                    return Ok(atom);
                };
                let max = if self.eat(',') { self.number().unwrap_or(u32::MAX) } else { min };
                if !self.eat('}') {
                    self.i = save;
                    return Ok(atom);
                }
                self.i -= 1;
                (min, max)
            }
            _ => return Ok(atom),
        };
        self.i += 1;
        let greedy = !self.eat('?');
        if matches!(atom, Node::Start | Node::End | Node::WordB(_)) {
            return Ok(atom);
        }
        Ok(Node::Rep(Box::new(atom), min, max, greedy))
    }

    fn atom(&mut self) -> Result<Node, String> {
        let c = self.peek().ok_or_else(|| String::from("fine inattesa"))?;
        self.i += 1;
        Ok(match c {
            '^' => Node::Start,
            '$' => Node::End,
            '.' => Node::Any,
            '[' => self.class()?,
            '\\' => self.escape(false)?,
            '(' => {
                let mut cap = None;
                let node = if self.eat('?') {
                    if self.eat(':') {
                        self.alt()?
                    } else if self.eat('=') {
                        Node::Look(Box::new(self.alt()?), true, false)
                    } else if self.eat('!') {
                        Node::Look(Box::new(self.alt()?), false, false)
                    } else if self.eat('<') {
                        if self.eat('=') {
                            Node::Look(Box::new(self.alt()?), true, true)
                        } else if self.eat('!') {
                            Node::Look(Box::new(self.alt()?), false, true)
                        } else {
                            let mut name = String::new();
                            while let Some(c) = self.peek() {
                                self.i += 1;
                                if c == '>' {
                                    break;
                                }
                                name.push(c);
                            }
                            self.groups += 1;
                            cap = Some(self.groups);
                            self.names.push((name, self.groups));
                            self.alt()?
                        }
                    } else {
                        return Err(String::from("gruppo non valido"));
                    }
                } else {
                    self.groups += 1;
                    cap = Some(self.groups);
                    self.alt()?
                };
                if !self.eat(')') {
                    return Err(String::from("manca ')'"));
                }
                match node {
                    n @ Node::Look(..) => n,
                    n => Node::Group(Box::new(n), cap),
                }
            }
            ')' => return Err(String::from("')' di troppo")),
            c => Node::Char(c),
        })
    }

    /// After a backslash.
    fn escape(&mut self, in_class: bool) -> Result<Node, String> {
        let c = self.peek().ok_or_else(|| String::from("'\\' alla fine"))?;
        self.i += 1;
        Ok(match c {
            'd' => Node::Class(false, alloc::vec![Item::Digit(false)]),
            'D' => Node::Class(false, alloc::vec![Item::Digit(true)]),
            'w' => Node::Class(false, alloc::vec![Item::Word(false)]),
            'W' => Node::Class(false, alloc::vec![Item::Word(true)]),
            's' => Node::Class(false, alloc::vec![Item::Space(false)]),
            'S' => Node::Class(false, alloc::vec![Item::Space(true)]),
            'b' if !in_class => Node::WordB(true),
            'B' if !in_class => Node::WordB(false),
            'b' => Node::Char('\u{8}'),
            'n' => Node::Char('\n'),
            'r' => Node::Char('\r'),
            't' => Node::Char('\t'),
            'f' => Node::Char('\u{c}'),
            'v' => Node::Char('\u{b}'),
            '0' => Node::Char('\0'),
            'c' => {
                let l = self.peek().unwrap_or('@');
                self.i += 1;
                Node::Char(char::from_u32(l as u32 % 32).unwrap_or('\0'))
            }
            'x' => Node::Char(self.hex(2)?),
            'u' => {
                if self.eat('{') {
                    let mut v = 0u32;
                    while let Some(d) = self.peek().and_then(|c| c.to_digit(16)) {
                        v = v.saturating_mul(16).saturating_add(d);
                        self.i += 1;
                    }
                    self.eat('}');
                    Node::Char(char::from_u32(v).unwrap_or('\u{fffd}'))
                } else {
                    Node::Char(self.hex(4)?)
                }
            }
            'k' if !in_class && self.peek() == Some('<') => {
                self.i += 1;
                let mut name = String::new();
                while let Some(c) = self.peek() {
                    self.i += 1;
                    if c == '>' {
                        break;
                    }
                    name.push(c);
                }
                // Resolved once the whole pattern is read.
                Node::Back(usize::MAX - self.names.iter().position(|n| n.0 == name).unwrap_or(0))
            }
            '1'..='9' if !in_class => {
                self.i -= 1;
                let n = self.number().unwrap_or(0) as usize;
                Node::Back(n)
            }
            c => Node::Char(c),
        })
    }

    fn hex(&mut self, n: usize) -> Result<char, String> {
        let mut v = 0;
        for _ in 0..n {
            let d = self.peek().and_then(|c| c.to_digit(16)).ok_or_else(|| String::from("sequenza esadecimale non valida"))?;
            v = v * 16 + d;
            self.i += 1;
        }
        Ok(char::from_u32(v).unwrap_or('\u{fffd}'))
    }

    fn class(&mut self) -> Result<Node, String> {
        let neg = self.eat('^');
        let mut items = Vec::new();
        let mut first = true;
        loop {
            let Some(c) = self.peek() else { return Err(String::from("manca ']'")) };
            if c == ']' && !first {
                self.i += 1;
                break;
            }
            if c == ']' && first {
                // `[]` matches nothing, `[^]` anything.
                self.i += 1;
                break;
            }
            first = false;
            self.i += 1;
            let lo = if c == '\\' {
                match self.escape(true)? {
                    Node::Char(ch) => ch,
                    Node::Class(_, mut it) => {
                        items.append(&mut it);
                        continue;
                    }
                    _ => continue,
                }
            } else {
                c
            };
            if self.peek() == Some('-') && self.s.get(self.i + 1).is_some_and(|c| *c != ']') {
                self.i += 1;
                let h = self.peek().unwrap_or(lo);
                self.i += 1;
                let hi = if h == '\\' {
                    match self.escape(true)? {
                        Node::Char(ch) => ch,
                        Node::Class(_, mut it) => {
                            items.push(Item::Range(lo, lo));
                            items.push(Item::Range('-', '-'));
                            items.append(&mut it);
                            continue;
                        }
                        _ => lo,
                    }
                } else {
                    h
                };
                items.push(Item::Range(lo, hi));
            } else {
                items.push(Item::Range(lo, lo));
            }
        }
        Ok(Node::Class(neg, items))
    }
}

fn resolve_names(n: &mut Node, names: &[(String, usize)]) {
    match n {
        Node::Back(b) if *b > usize::MAX / 2 => {
            let i = usize::MAX - *b;
            *b = names.get(i).map_or(0, |x| x.1);
        }
        Node::Group(x, _) | Node::Look(x, _, _) | Node::Rep(x, _, _, _) => resolve_names(x, names),
        Node::Seq(v) | Node::Alt(v) => v.iter_mut().for_each(|x| resolve_names(x, names)),
        _ => {}
    }
}

impl Regex {
    pub fn new(source: &str, flags: &str) -> Result<Regex, String> {
        let mut f = Flags::default();
        for c in flags.chars() {
            match c {
                'g' => f.global = true,
                'i' => f.icase = true,
                'm' => f.multiline = true,
                's' => f.dotall = true,
                'y' => f.sticky = true,
                'u' | 'v' => f.unicode = true,
                'd' => {}
                c => return Err(alloc::format!("flag non valido '{c}'")),
            }
        }
        let chars: Vec<char> = source.chars().collect();
        let mut p = P { s: &chars, i: 0, groups: 0, names: Vec::new(), depth: 0 };
        let mut prog = p.alt()?;
        if p.i < chars.len() {
            return Err(String::from("')' di troppo"));
        }
        resolve_names(&mut prog, &p.names);
        Ok(Regex { source: Rc::from(source), flags_text: Rc::from(flags), flags: f, prog, groups: p.groups, names: p.names })
    }

    /// The first match at or after `start` (only at `start` if sticky).
    pub fn exec(&self, s: &[char], start: usize) -> Option<Caps> {
        let mut m = M { re: self, s, caps: alloc::vec![None; self.groups + 1], steps: 0, depth: 0 };
        let last = if self.flags.sticky { start } else { s.len() };
        let first = match &self.prog {
            Node::Char(c) if !self.flags.icase => Some(*c),
            Node::Seq(v) => match v.first() {
                Some(Node::Char(c)) if !self.flags.icase => Some(*c),
                _ => None,
            },
            _ => None,
        };
        let mut pos = start;
        while pos <= last {
            if let Some(c) = first {
                match s[pos.min(s.len())..].iter().position(|x| *x == c) {
                    Some(d) if pos + d <= last => pos += d,
                    _ => return None,
                }
            }
            m.caps.iter_mut().for_each(|c| *c = None);
            let p0 = pos;
            if m.node(&self.prog, pos, &mut |me, j| {
                me.caps[0] = Some((p0, j));
                true
            }) {
                return Some(m.caps);
            }
            if m.steps > STEP_LIMIT {
                return None;
            }
            pos += 1;
        }
        None
    }
}

const STEP_LIMIT: u64 = 2_000_000;
const DEPTH_LIMIT: u32 = 1500;

type K<'k, 'a> = &'k mut dyn FnMut(&mut M<'a>, usize) -> bool;

struct M<'a> {
    re: &'a Regex,
    s: &'a [char],
    caps: Caps,
    steps: u64,
    depth: u32,
}

fn fold(c: char) -> char {
    if c.is_ascii() {
        c.to_ascii_lowercase()
    } else {
        c.to_lowercase().next().unwrap_or(c)
    }
}

impl<'a> M<'a> {
    fn class_has(&self, items: &[Item], c: char) -> bool {
        items.iter().any(|it| match it {
            Item::Range(lo, hi) => {
                (*lo..=*hi).contains(&c) || (self.re.flags.icase && {
                    let (l, u) = (fold(c), c.to_uppercase().next().unwrap_or(c));
                    (*lo..=*hi).contains(&l) || (*lo..=*hi).contains(&u)
                })
            }
            Item::Digit(neg) => c.is_ascii_digit() != *neg,
            Item::Word(neg) => is_word(c) != *neg,
            Item::Space(neg) => is_space(c) != *neg,
        })
    }

    /// Whether a one-character node matches at `i`.
    fn one(&self, n: &Node, i: usize) -> Option<bool> {
        let c = self.s.get(i).copied();
        Some(match n {
            Node::Char(x) => c.is_some_and(|c| c == *x || (self.re.flags.icase && fold(c) == fold(*x))),
            Node::Any => c.is_some_and(|c| self.re.flags.dotall || !is_line_end(c)),
            Node::Class(neg, items) => c.is_some_and(|c| self.class_has(items, c) != *neg),
            _ => return None,
        })
    }

    fn node(&mut self, n: &'a Node, i: usize, k: K<'_, 'a>) -> bool {
        self.steps += 1;
        if self.steps > STEP_LIMIT || self.depth > DEPTH_LIMIT {
            return false;
        }
        if let Some(ok) = self.one(n, i) {
            return ok && k(self, i + 1);
        }
        self.depth += 1;
        let r = self.node_inner(n, i, k);
        self.depth -= 1;
        r
    }

    fn node_inner(&mut self, n: &'a Node, i: usize, k: K<'_, 'a>) -> bool {
        let s = self.s;
        let ml = self.re.flags.multiline;
        match n {
            Node::Start => (i == 0 || (ml && is_line_end(s[i - 1]))) && k(self, i),
            Node::End => (i == s.len() || (ml && is_line_end(s[i]))) && k(self, i),
            Node::WordB(want) => {
                let a = i > 0 && is_word(s[i - 1]);
                let b = i < s.len() && is_word(s[i]);
                ((a != b) == *want) && k(self, i)
            }
            Node::Seq(items) => self.seq(items, i, k),
            Node::Alt(alts) => {
                for a in alts {
                    if self.node(a, i, k) {
                        return true;
                    }
                }
                false
            }
            Node::Group(inner, None) => self.node(inner, i, k),
            Node::Group(inner, Some(g)) => {
                let g = *g;
                let old = self.caps[g];
                if self.node(inner, i, &mut |me, j| {
                    let save = me.caps[g];
                    me.caps[g] = Some((i, j));
                    if k(me, j) {
                        true
                    } else {
                        me.caps[g] = save;
                        false
                    }
                }) {
                    return true;
                }
                self.caps[g] = old;
                false
            }
            Node::Look(inner, positive, behind) => {
                let saved = self.caps.clone();
                let found = if *behind {
                    (0..=i).rev().any(|start| self.node(inner, start, &mut |_, j| j == i))
                } else {
                    self.node(inner, i, &mut |_, _| true)
                };
                if found != *positive {
                    self.caps = saved;
                    return false;
                }
                if !*positive {
                    self.caps.clone_from(&saved);
                }
                if k(self, i) {
                    true
                } else {
                    self.caps = saved;
                    false
                }
            }
            Node::Back(g) => {
                let Some(Some((a, b))) = self.caps.get(*g).copied() else { return k(self, i) };
                let len = b - a;
                if i + len > s.len() {
                    return false;
                }
                let icase = self.re.flags.icase;
                let same = (0..len).all(|d| s[a + d] == s[i + d] || (icase && fold(s[a + d]) == fold(s[i + d])));
                same && k(self, i + len)
            }
            Node::Rep(inner, min, max, greedy) => {
                if self.one(inner, i).is_some() {
                    // One character at a time: count, then try each length.
                    let mut n = 0u32;
                    while n < *max && self.one(inner, i + n as usize) == Some(true) {
                        n += 1;
                    }
                    if n < *min {
                        return false;
                    }
                    if *greedy {
                        let mut c = n;
                        loop {
                            if k(self, i + c as usize) {
                                return true;
                            }
                            if c == *min {
                                return false;
                            }
                            c -= 1;
                        }
                    }
                    for c in *min..=n {
                        if k(self, i + c as usize) {
                            return true;
                        }
                    }
                    false
                } else {
                    self.rep(inner, *min, *max, *greedy, 0, i, k)
                }
            }
            Node::Char(_) | Node::Any | Node::Class(..) => false,
        }
    }

    fn seq(&mut self, items: &'a [Node], i: usize, k: K<'_, 'a>) -> bool {
        let mut i = i;
        let mut items = items;
        // Plain characters in a row need no backtracking.
        while let Some((first, rest)) = items.split_first() {
            match self.one(first, i) {
                Some(true) => {
                    i += 1;
                    items = rest;
                }
                Some(false) => return false,
                None => break,
            }
        }
        match items.split_first() {
            None => k(self, i),
            Some((first, rest)) => self.node(first, i, &mut |me, j| me.seq(rest, j, k)),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn rep(&mut self, n: &'a Node, min: u32, max: u32, greedy: bool, count: u32, i: usize, k: K<'_, 'a>) -> bool {
        if count < min {
            return self.node(n, i, &mut |me, j| me.rep(n, min, max, greedy, count + 1, j, k));
        }
        if greedy {
            if count < max && self.node(n, i, &mut |me, j| j != i && me.rep(n, min, max, greedy, count + 1, j, k)) {
                return true;
            }
            k(self, i)
        } else {
            if k(self, i) {
                return true;
            }
            count < max && self.node(n, i, &mut |me, j| j != i && me.rep(n, min, max, greedy, count + 1, j, k))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(p: &str, f: &str, s: &str) -> Option<(usize, usize)> {
        let re = Regex::new(p, f).unwrap();
        let chars: Vec<char> = s.chars().collect();
        re.exec(&chars, 0).and_then(|c| c[0])
    }

    #[test]
    fn basics() {
        assert_eq!(find("b+", "", "aabbbc"), Some((2, 5)));
        assert_eq!(find("^a.c$", "", "abc"), Some((0, 3)));
        assert_eq!(find("[a-c]{2,}", "", "xxabcabz"), Some((2, 7)));
        assert_eq!(find("colou?r", "", "my color"), Some((3, 8)));
        assert_eq!(find("\\d+\\.\\d*", "", "pi 3.14!"), Some((3, 7)));
        assert_eq!(find("HELLO", "i", "say hello"), Some((4, 9)));
        assert_eq!(find("a|bc|d", "", "xxbc"), Some((2, 4)));
        assert_eq!(find("(a+)b\\1", "", "aaabaa"), Some((1, 6)));
        assert_eq!(find("\\bfoo\\b", "", "afoo foo"), Some((5, 8)));
        assert_eq!(find("x(?=y)", "", "xa xy"), Some((3, 4)));
        assert_eq!(find("(?<!\\$)\\d", "", "$1 2"), Some((3, 4)));
        assert_eq!(find("a.*?b", "", "a1b2b"), Some((0, 3)));
        assert_eq!(find("[^\"]*", "", "ab\"c"), Some((0, 2)));
        assert_eq!(find("^b", "m", "a\nb"), Some((2, 3)));
    }

    #[test]
    fn groups() {
        let re = Regex::new("(?<y>\\d{4})-(\\d\\d)", "").unwrap();
        let s: Vec<char> = "on 2026-10".chars().collect();
        let c = re.exec(&s, 0).unwrap();
        assert_eq!(c[1], Some((3, 7)));
        assert_eq!(c[2], Some((8, 10)));
        assert_eq!(re.names[0].0, "y");
    }

    #[test]
    fn no_hang() {
        let s: String = core::iter::repeat_n('a', 40).collect();
        assert_eq!(find("(a*)*b", "", &s), None);
        let long: String = core::iter::repeat_n('x', 100_000).collect();
        assert_eq!(find(".*", "", &long), Some((0, 100_000)));
    }
}

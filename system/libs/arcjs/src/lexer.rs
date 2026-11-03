//! Source text to tokens.
//!
//! The whole script is read up front. Whether a `/` starts a regular
//! expression or divides is decided by the token before it, and every token
//! remembers whether a line break came before it (for automatic semicolons
//! and `a \n ++b`).

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    /// An identifier or a keyword (`#x` for private names).
    Name(Rc<str>),
    Num(f64),
    Str(Rc<str>),
    /// A template: its cooked pieces and the source of each `${…}`.
    Tmpl(Vec<Rc<str>>, Vec<String>),
    /// A regular expression: pattern and flags.
    Regex(Rc<str>, Rc<str>),
    Punct(&'static str),
    Eof,
}

#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    /// A line break comes before the token.
    pub nl: bool,
    pub line: u32,
}

/// Longest first, so the first match is the right one.
const PUNCTS: &[&str] = &[
    ">>>=", "...", "===", "!==", "**=", "<<=", ">>=", ">>>", "&&=", "||=", "??=", "=>", "==", "!=", "<=", ">=", "&&", "||", "??", "?.", "++", "--", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "**", "<<", ">>", "{", "}", "(", ")", "[", "]", ";", ",", "<", ">", "+", "-", "*", "/", "%", "&", "|", "^", "!", "~", "?", ":", "=", ".", "@",
];

/// Keywords after which a `/` starts a regular expression.
const BEFORE_REGEX: &[&str] = &["return", "typeof", "instanceof", "in", "of", "new", "delete", "void", "throw", "case", "do", "else", "yield", "await"];

pub struct Lexer<'a> {
    src: &'a str,
    b: &'a [u8],
    i: usize,
    line: u32,
    names: BTreeMap<String, Rc<str>>,
}

fn is_id_start(c: char) -> bool {
    c == '$' || c == '_' || c.is_alphabetic()
}

fn is_id_part(c: char) -> bool {
    c == '$' || c == '_' || c.is_alphanumeric() || c == '\u{200c}' || c == '\u{200d}'
}

fn hex(c: u8) -> Option<u32> {
    (c as char).to_digit(16)
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Self {
        Lexer { src, b: src.as_bytes(), i: 0, line: 1, names: BTreeMap::new() }
    }

    fn err(&self, what: &str) -> String {
        alloc::format!("riga {}: {what}", self.line)
    }

    fn peek(&self) -> Option<char> {
        let c = *self.b.get(self.i)?;
        if c < 0x80 {
            Some(c as char)
        } else {
            self.src[self.i..].chars().next()
        }
    }

    fn at(&self, k: usize) -> u8 {
        self.b.get(self.i + k).copied().unwrap_or(0)
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.i += c.len_utf8();
        if c == '\n' {
            self.line += 1;
        }
        Some(c)
    }

    pub fn intern(&mut self, s: &str) -> Rc<str> {
        if let Some(r) = self.names.get(s) {
            return r.clone();
        }
        let r: Rc<str> = Rc::from(s);
        self.names.insert(String::from(s), r.clone());
        r
    }

    /// Skips blanks and comments; true if a line break was among them.
    fn skip(&mut self) -> Result<bool, String> {
        let mut nl = false;
        loop {
            match self.peek() {
                Some('\n' | '\r' | '\u{2028}' | '\u{2029}') => {
                    nl = true;
                    self.bump();
                }
                Some(c) if c.is_whitespace() || c == '\u{feff}' => {
                    self.bump();
                }
                Some('/') if self.at(1) == b'/' => {
                    while let Some(c) = self.peek() {
                        if c == '\n' || c == '\r' {
                            break;
                        }
                        self.bump();
                    }
                }
                Some('/') if self.at(1) == b'*' => {
                    self.i += 2;
                    loop {
                        match self.bump() {
                            None => return Err(self.err("commento non chiuso")),
                            Some('*') if self.at(0) == b'/' => {
                                self.i += 1;
                                break;
                            }
                            Some('\n') => nl = true,
                            _ => {}
                        }
                    }
                }
                Some('<') if self.b[self.i..].starts_with(b"<!--") => {
                    while let Some(c) = self.peek() {
                        if c == '\n' {
                            break;
                        }
                        self.bump();
                    }
                }
                Some('-') if (nl || self.i == 0) && self.b[self.i..].starts_with(b"-->") => {
                    while let Some(c) = self.peek() {
                        if c == '\n' {
                            break;
                        }
                        self.bump();
                    }
                }
                _ => return Ok(nl),
            }
        }
    }

    /// The whole script as tokens, ending with [`Tok::Eof`].
    pub fn tokens(mut self) -> Result<Vec<Token>, String> {
        let mut out: Vec<Token> = Vec::new();
        if self.b.starts_with(b"#!") {
            while self.peek().is_some_and(|c| c != '\n') {
                self.bump();
            }
        }
        loop {
            let nl = self.skip()?;
            let line = self.line;
            let Some(c) = self.peek() else {
                out.push(Token { tok: Tok::Eof, nl: true, line });
                return Ok(out);
            };
            let tok = if is_id_start(c) || c == '\\' || (c == '#' && self.src[self.i + 1..].chars().next().is_some_and(is_id_start)) {
                self.name()?
            } else if c.is_ascii_digit() || (c == '.' && self.at(1).is_ascii_digit()) {
                self.number()?
            } else if c == '"' || c == '\'' {
                self.bump();
                Tok::Str(Rc::from(self.string(c)?))
            } else if c == '`' {
                self.bump();
                self.template()?
            } else if c == '/' && regex_allowed(out.last().map(|t| &t.tok)) {
                self.regex()?
            } else {
                let rest = &self.b[self.i..];
                let Some(p) = PUNCTS.iter().find(|p| rest.starts_with(p.as_bytes())) else {
                    return Err(self.err(&alloc::format!("carattere inatteso '{c}'")));
                };
                // `a?.5:b` is a conditional, not optional chaining.
                if *p == "?." && self.at(2).is_ascii_digit() {
                    self.i += 1;
                    Tok::Punct("?")
                } else {
                    self.i += p.len();
                    Tok::Punct(p)
                }
            };
            out.push(Token { tok, nl, line });
        }
    }

    fn name(&mut self) -> Result<Tok, String> {
        let mut s = String::new();
        if self.peek() == Some('#') {
            self.bump();
            s.push('#');
        }
        while let Some(c) = self.peek() {
            if c == '\\' && self.at(1) == b'u' {
                self.i += 2;
                s.push(self.unicode_escape()?);
            } else if is_id_part(c) {
                s.push(c);
                self.bump();
            } else {
                break;
            }
        }
        Ok(Tok::Name(self.intern(&s)))
    }

    fn number(&mut self) -> Result<Tok, String> {
        let start = self.i;
        let radix = if self.at(0) == b'0' {
            match self.at(1) | 0x20 {
                b'x' => 16,
                b'o' => 8,
                b'b' => 2,
                _ => 10,
            }
        } else {
            10
        };
        if radix != 10 {
            self.i += 2;
            let mut v = 0f64;
            while let Some(d) = self.peek().and_then(|c| if c == '_' { Some(u32::MAX) } else { c.to_digit(radix) }) {
                if d != u32::MAX {
                    v = v * f64::from(radix) + f64::from(d);
                }
                self.i += 1;
            }
            if self.at(0) == b'n' {
                self.i += 1;
            }
            return Ok(Tok::Num(v));
        }
        let mut text = String::new();
        let mut legacy_octal = self.at(0) == b'0' && self.at(1).is_ascii_digit();
        while let Some(c) = self.peek() {
            match c {
                '0'..='9' => text.push(c),
                '_' => {}
                '.' if !text.contains(['.', 'e']) && !legacy_octal => text.push(c),
                'e' | 'E' if !text.contains('e') && !legacy_octal => {
                    text.push('e');
                    if matches!(self.at(1), b'+' | b'-') {
                        self.i += 1;
                        text.push(self.at(0) as char);
                    }
                }
                _ => break,
            }
            self.i += 1;
        }
        if legacy_octal && text.bytes().any(|d| d >= b'8') {
            legacy_octal = false;
        }
        if self.at(0) == b'n' {
            self.i += 1;
        }
        if self.peek().is_some_and(is_id_start) {
            return Err(self.err("numero non valido"));
        }
        let v = if legacy_octal {
            text.bytes().fold(0f64, |v, d| v * 8.0 + f64::from(d - b'0'))
        } else {
            text.parse::<f64>().map_err(|_| self.err(&alloc::format!("numero non valido '{}'", &self.src[start..self.i])))?
        };
        Ok(Tok::Num(v))
    }

    fn unicode_escape(&mut self) -> Result<char, String> {
        let mut v = 0u32;
        if self.at(0) == b'{' {
            self.i += 1;
            while let Some(d) = hex(self.at(0)) {
                v = v.saturating_mul(16).saturating_add(d);
                self.i += 1;
            }
            if self.at(0) != b'}' {
                return Err(self.err("sequenza \\u non valida"));
            }
            self.i += 1;
            return Ok(char::from_u32(v).unwrap_or('\u{fffd}'));
        }
        for _ in 0..4 {
            let d = hex(self.at(0)).ok_or_else(|| self.err("sequenza \\u non valida"))?;
            v = v * 16 + d;
            self.i += 1;
        }
        // A surrogate pair written as two escapes.
        if (0xd800..0xdc00).contains(&v) && self.at(0) == b'\\' && self.at(1) == b'u' {
            let save = self.i;
            self.i += 2;
            let mut lo = 0u32;
            let mut ok = true;
            for _ in 0..4 {
                match hex(self.at(0)) {
                    Some(d) => {
                        lo = lo * 16 + d;
                        self.i += 1;
                    }
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok && (0xdc00..0xe000).contains(&lo) {
                return Ok(char::from_u32(0x10000 + ((v - 0xd800) << 10) + (lo - 0xdc00)).unwrap_or('\u{fffd}'));
            }
            self.i = save;
        }
        Ok(char::from_u32(v).unwrap_or('\u{fffd}'))
    }

    /// One escape after a backslash, pushed onto `s`.
    fn escape(&mut self, s: &mut String) -> Result<(), String> {
        let Some(c) = self.bump() else { return Err(self.err("stringa non chiusa")) };
        match c {
            'n' => s.push('\n'),
            't' => s.push('\t'),
            'r' => s.push('\r'),
            'b' => s.push('\u{8}'),
            'f' => s.push('\u{c}'),
            'v' => s.push('\u{b}'),
            '0' if !self.at(0).is_ascii_digit() => s.push('\0'),
            'x' => {
                let (Some(a), Some(b)) = (hex(self.at(0)), hex(self.at(1))) else { return Err(self.err("sequenza \\x non valida")) };
                self.i += 2;
                s.push(char::from_u32(a * 16 + b).unwrap_or('\u{fffd}'));
            }
            'u' => s.push(self.unicode_escape()?),
            '\r' => {
                if self.at(0) == b'\n' {
                    self.i += 1;
                }
            }
            '\n' | '\u{2028}' | '\u{2029}' => {}
            c => s.push(c),
        }
        Ok(())
    }

    fn string(&mut self, q: char) -> Result<String, String> {
        let mut s = String::new();
        loop {
            match self.bump() {
                None | Some('\n') => return Err(self.err("stringa non chiusa")),
                Some('\\') => self.escape(&mut s)?,
                Some(c) if c == q => return Ok(s),
                Some(c) => s.push(c),
            }
        }
    }

    fn template(&mut self) -> Result<Tok, String> {
        let mut parts = Vec::new();
        let mut exprs = Vec::new();
        let mut s = String::new();
        loop {
            match self.bump() {
                None => return Err(self.err("template non chiuso")),
                Some('`') => {
                    parts.push(Rc::from(s.as_str()));
                    return Ok(Tok::Tmpl(parts, exprs));
                }
                Some('\\') => self.escape(&mut s)?,
                Some('$') if self.at(0) == b'{' => {
                    self.i += 1;
                    parts.push(Rc::from(core::mem::take(&mut s).as_str()));
                    let start = self.i;
                    self.skip_braced()?;
                    exprs.push(String::from(&self.src[start..self.i - 1]));
                }
                Some('\r') => {
                    if self.at(0) == b'\n' {
                        self.i += 1;
                    }
                    s.push('\n');
                }
                Some(c) => s.push(c),
            }
        }
    }

    /// Skips to just past the `}` closing a `${`.
    fn skip_braced(&mut self) -> Result<(), String> {
        let mut depth = 0usize;
        loop {
            match self.bump() {
                None => return Err(self.err("template non chiuso")),
                Some('{') => depth += 1,
                Some('}') => {
                    if depth == 0 {
                        return Ok(());
                    }
                    depth -= 1;
                }
                Some(q @ ('"' | '\'')) => {
                    self.string(q)?;
                }
                Some('`') => {
                    self.template()?;
                }
                Some('/') if self.at(0) == b'/' => {
                    while self.peek().is_some_and(|c| c != '\n') {
                        self.bump();
                    }
                }
                Some('/') if self.at(0) == b'*' => {
                    self.i += 1;
                    while !(self.at(0) == b'*' && self.at(1) == b'/') {
                        if self.bump().is_none() {
                            return Err(self.err("commento non chiuso"));
                        }
                    }
                    self.i += 2;
                }
                _ => {}
            }
        }
    }

    fn regex(&mut self) -> Result<Tok, String> {
        self.i += 1;
        let start = self.i;
        let mut class = false;
        loop {
            match self.bump() {
                None | Some('\n') => return Err(self.err("espressione regolare non chiusa")),
                Some('\\') => {
                    self.bump();
                }
                Some('[') => class = true,
                Some(']') => class = false,
                Some('/') if !class => break,
                _ => {}
            }
        }
        let pattern = &self.src[start..self.i - 1];
        let fstart = self.i;
        while self.peek().is_some_and(is_id_part) {
            self.bump();
        }
        Ok(Tok::Regex(Rc::from(pattern), Rc::from(&self.src[fstart..self.i])))
    }
}

fn regex_allowed(prev: Option<&Tok>) -> bool {
    match prev {
        None => true,
        Some(Tok::Punct(p)) => !matches!(*p, ")" | "]" | "++" | "--"),
        Some(Tok::Name(n)) => BEFORE_REGEX.contains(&&**n),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(s: &str) -> Vec<Tok> {
        Lexer::new(s).tokens().unwrap().into_iter().map(|t| t.tok).collect()
    }

    #[test]
    fn regex_or_division() {
        assert!(matches!(toks("a / b / c")[1], Tok::Punct("/")));
        assert!(matches!(toks("x = /ab+c/gi")[2], Tok::Regex(..)));
        assert!(matches!(toks("(a) / 2")[3], Tok::Punct("/")));
    }

    #[test]
    fn numbers_and_strings() {
        assert_eq!(toks("0x1F 1_000 .5 1e3 0b101")[..5], [Tok::Num(31.0), Tok::Num(1000.0), Tok::Num(0.5), Tok::Num(1000.0), Tok::Num(5.0)]);
        assert_eq!(toks(r#"'a\nb' "\u{41}\x42""#)[..2], [Tok::Str(Rc::from("a\nb")), Tok::Str(Rc::from("AB"))]);
    }

    #[test]
    fn templates() {
        let t = toks("`a${x + `in${y}`}b`");
        let Tok::Tmpl(parts, exprs) = &t[0] else { panic!() };
        assert_eq!(parts.len(), 2);
        assert_eq!(exprs[0], "x + `in${y}`");
    }
}

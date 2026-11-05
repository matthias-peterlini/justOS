//! JSON (RFC 8259): [`parse`] into a [`Value`], and [`Value`]'s `Display`
//! back to text. Numbers are `f64`, as in JavaScript.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(test)]
extern crate std;

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Value>),
    /// Members in their order.
    Obj(Vec<(String, Value)>),
}

static NULL: Value = Value::Null;

impl Value {
    /// The member `key` of an object (Null if absent).
    pub fn get(&self, key: &str) -> &Value {
        match self {
            Value::Obj(m) => m.iter().find(|(k, _)| k == key).map(|(_, v)| v).unwrap_or(&NULL),
            _ => &NULL,
        }
    }

    /// Element `i` of an array (Null if absent).
    pub fn at(&self, i: usize) -> &Value {
        match self {
            Value::Arr(a) => a.get(i).unwrap_or(&NULL),
            _ => &NULL,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    /// The string, or "" for anything else.
    pub fn str(&self) -> &str {
        self.as_str().unwrap_or("")
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Num(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        self.as_f64().map(|n| n as i64)
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_array(&self) -> &[Value] {
        match self {
            Value::Arr(a) => a,
            _ => &[],
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// An object from pairs (for building requests).
    pub fn object(pairs: Vec<(&str, Value)>) -> Value {
        Value::Obj(pairs.into_iter().map(|(k, v)| (String::from(k), v)).collect())
    }

    pub fn string(s: &str) -> Value {
        Value::Str(String::from(s))
    }
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
    depth: u32,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> Option<()> {
        self.ws();
        (self.b.get(self.i) == Some(&c)).then(|| self.i += 1)
    }

    fn lit(&mut self, s: &[u8], v: Value) -> Option<Value> {
        if self.b.get(self.i..self.i + s.len()) == Some(s) {
            self.i += s.len();
            Some(v)
        } else {
            None
        }
    }

    fn value(&mut self) -> Option<Value> {
        self.ws();
        self.depth += 1;
        if self.depth > 256 {
            return None;
        }
        let v = match *self.b.get(self.i)? {
            b'n' => self.lit(b"null", Value::Null),
            b't' => self.lit(b"true", Value::Bool(true)),
            b'f' => self.lit(b"false", Value::Bool(false)),
            b'"' => self.string().map(Value::Str),
            b'[' => {
                self.i += 1;
                let mut a = Vec::new();
                if self.eat(b']').is_none() {
                    loop {
                        a.push(self.value()?);
                        if self.eat(b',').is_some() {
                            continue;
                        }
                        self.eat(b']')?;
                        break;
                    }
                }
                Some(Value::Arr(a))
            }
            b'{' => {
                self.i += 1;
                let mut m = Vec::new();
                if self.eat(b'}').is_none() {
                    loop {
                        self.ws();
                        let k = self.string()?;
                        self.eat(b':')?;
                        let v = self.value()?;
                        m.push((k, v));
                        if self.eat(b',').is_some() {
                            continue;
                        }
                        self.eat(b'}')?;
                        break;
                    }
                }
                Some(Value::Obj(m))
            }
            _ => self.number(),
        };
        self.depth -= 1;
        v
    }

    fn number(&mut self) -> Option<Value> {
        let start = self.i;
        while self.i < self.b.len() && matches!(self.b[self.i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
            self.i += 1;
        }
        let s = core::str::from_utf8(&self.b[start..self.i]).ok()?;
        s.parse::<f64>().ok().map(Value::Num)
    }

    fn hex4(&mut self) -> Option<u32> {
        let s = core::str::from_utf8(self.b.get(self.i..self.i + 4)?).ok()?;
        self.i += 4;
        u32::from_str_radix(s, 16).ok()
    }

    fn string(&mut self) -> Option<String> {
        if self.b.get(self.i) != Some(&b'"') {
            return None;
        }
        self.i += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let c = *self.b.get(self.i)?;
            self.i += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let e = *self.b.get(self.i)?;
                    self.i += 1;
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let mut cp = self.hex4()?;
                            if (0xD800..0xDC00).contains(&cp) && self.b.get(self.i..self.i + 2) == Some(b"\\u") {
                                self.i += 2;
                                let lo = self.hex4()?;
                                cp = 0x10000 + ((cp - 0xD800) << 10) + (lo.wrapping_sub(0xDC00) & 0x3FF);
                            }
                            char::from_u32(cp).unwrap_or('\u{FFFD}')
                        }
                        _ => return None,
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                _ => out.push(c),
            }
        }
        Some(String::from_utf8_lossy(&out).into_owned())
    }
}

/// Parses a JSON document.
pub fn parse(text: &str) -> Option<Value> {
    parse_bytes(text.as_bytes())
}

pub fn parse_bytes(b: &[u8]) -> Option<Value> {
    let b = b.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(b);
    let mut p = Parser { b, i: 0, depth: 0 };
    let v = p.value()?;
    p.ws();
    (p.i == b.len()).then_some(v)
}

fn write_str(f: &mut fmt::Formatter<'_>, s: &str) -> fmt::Result {
    f.write_str("\"")?;
    for c in s.chars() {
        match c {
            '"' => f.write_str("\\\"")?,
            '\\' => f.write_str("\\\\")?,
            '\n' => f.write_str("\\n")?,
            '\r' => f.write_str("\\r")?,
            '\t' => f.write_str("\\t")?,
            c if (c as u32) < 0x20 => write!(f, "\\u{:04x}", c as u32)?,
            c => write!(f, "{c}")?,
        }
    }
    f.write_str("\"")
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => f.write_str("null"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Num(n) => {
                if (*n as i64) as f64 == *n && *n > -1e15 && *n < 1e15 {
                    write!(f, "{}", *n as i64)
                } else {
                    write!(f, "{n}")
                }
            }
            Value::Str(s) => write_str(f, s),
            Value::Arr(a) => {
                f.write_str("[")?;
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        f.write_str(",")?;
                    }
                    write!(f, "{v}")?;
                }
                f.write_str("]")
            }
            Value::Obj(m) => {
                f.write_str("{")?;
                for (i, (k, v)) in m.iter().enumerate() {
                    if i > 0 {
                        f.write_str(",")?;
                    }
                    write_str(f, k)?;
                    write!(f, ":{v}")?;
                }
                f.write_str("}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn roundtrip() {
        let t = r#"{"a": [1, 2.5, -3e2, true, null], "b": {"c": "x\"y\\z\nè😀"}, "e": []}"#;
        let v = parse(t).unwrap();
        assert_eq!(v.get("a").at(1).as_f64(), Some(2.5));
        assert_eq!(v.get("a").at(2).as_f64(), Some(-300.0));
        assert_eq!(v.get("b").get("c").str(), "x\"y\\z\nè😀");
        assert!(v.get("missing").is_null());
        let back = parse(&v.to_string()).unwrap();
        assert_eq!(back, v);
        assert!(parse("{\"a\":1,}").is_none());
        assert!(parse("[1 2]").is_none());
    }
}

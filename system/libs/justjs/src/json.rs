//! `JSON.parse` and `JSON.stringify`.

use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use crate::builtins::func;
use crate::interp::*;
use crate::value::*;

struct P<'a> {
    b: &'a [u8],
    s: &'a str,
    i: usize,
    depth: u32,
}

impl P<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn err(&self) -> String {
        match self.s[self.i.min(self.s.len())..].chars().next() {
            Some(c) => format!("Unexpected token '{c}' in JSON at position {}", self.i),
            None => String::from("Unexpected end of JSON input"),
        }
    }

    fn value(&mut self, it: &mut Interp) -> Result<Value, String> {
        self.ws();
        self.depth += 1;
        if self.depth > 500 {
            return Err(String::from("JSON nested too deeply"));
        }
        let v = match self.b.get(self.i) {
            Some(b'{') => {
                self.i += 1;
                let o = it.new_obj();
                self.ws();
                if self.b.get(self.i) == Some(&b'}') {
                    self.i += 1;
                } else {
                    loop {
                        self.ws();
                        if self.b.get(self.i) != Some(&b'"') {
                            return Err(self.err());
                        }
                        let k = self.string()?;
                        self.ws();
                        if self.b.get(self.i) != Some(&b':') {
                            return Err(self.err());
                        }
                        self.i += 1;
                        let v = self.value(it)?;
                        o.borrow_mut().props.insert(Key::Str(Rc::from(k.as_str())), Prop::data(v));
                        self.ws();
                        match self.b.get(self.i) {
                            Some(b',') => self.i += 1,
                            Some(b'}') => {
                                self.i += 1;
                                break;
                            }
                            _ => return Err(self.err()),
                        }
                    }
                }
                Value::Obj(o)
            }
            Some(b'[') => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                } else {
                    loop {
                        items.push(self.value(it)?);
                        self.ws();
                        match self.b.get(self.i) {
                            Some(b',') => self.i += 1,
                            Some(b']') => {
                                self.i += 1;
                                break;
                            }
                            _ => return Err(self.err()),
                        }
                    }
                }
                it.new_array(items)
            }
            Some(b'"') => Value::str(&self.string()?),
            Some(b't') if self.b[self.i..].starts_with(b"true") => {
                self.i += 4;
                Value::Bool(true)
            }
            Some(b'f') if self.b[self.i..].starts_with(b"false") => {
                self.i += 5;
                Value::Bool(false)
            }
            Some(b'n') if self.b[self.i..].starts_with(b"null") => {
                self.i += 4;
                Value::Null
            }
            Some(c) if *c == b'-' || c.is_ascii_digit() => {
                let start = self.i;
                self.i += 1;
                while self.i < self.b.len() && matches!(self.b[self.i], b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-') {
                    self.i += 1;
                }
                Value::Num(self.s[start..self.i].parse().map_err(|_| self.err())?)
            }
            _ => return Err(self.err()),
        };
        self.depth -= 1;
        Ok(v)
    }

    fn string(&mut self) -> Result<String, String> {
        self.i += 1;
        let mut out = String::new();
        loop {
            let start = self.i;
            while self.i < self.b.len() && self.b[self.i] != b'"' && self.b[self.i] != b'\\' && self.b[self.i] >= 0x20 {
                self.i += 1;
            }
            out.push_str(&self.s[start..self.i]);
            match self.b.get(self.i) {
                Some(b'"') => {
                    self.i += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.i += 1;
                    let c = *self.b.get(self.i).ok_or_else(|| self.err())?;
                    self.i += 1;
                    match c {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let unit =|p: &mut Self| -> Result<u32, String> {
                                let h = p.s.get(p.i..p.i + 4).ok_or_else(|| p.err())?;
                                let v = u32::from_str_radix(h, 16).map_err(|_| p.err())?;
                                p.i += 4;
                                Ok(v)
                            };
                            let hi = unit(self)?;
                            let ch = if (0xd800..0xdc00).contains(&hi) && self.b[self.i..].starts_with(b"\\u") {
                                self.i += 2;
                                let lo = unit(self)?;
                                char::from_u32(0x10000 + ((hi - 0xd800) << 10) + (lo.wrapping_sub(0xdc00) & 0x3ff))
                            } else {
                                char::from_u32(hi)
                            };
                            out.push(ch.unwrap_or('\u{fffd}'));
                        }
                        _ => return Err(self.err()),
                    }
                }
                _ => return Err(self.err()),
            }
        }
    }
}

pub fn parse(it: &mut Interp, text: &str) -> R<Value> {
    let mut p = P { b: text.as_bytes(), s: text, i: 0, depth: 0 };
    let r = p.value(it).and_then(|v| {
        p.ws();
        if p.i < p.b.len() {
            Err(p.err())
        } else {
            Ok(v)
        }
    });
    r.map_err(|e| it.syntax_error(&e))
}

fn revive(it: &mut Interp, holder: &Value, key: Key, f: &Value) -> R<Value> {
    let v = it.get(holder, &key)?;
    if let Value::Obj(_) = v {
        for k in it.own_keys(&v, false) {
            let nv = revive(it, &v, k.clone(), f)?;
            if matches!(nv, Value::Undef) {
                it.delete(&v, &k);
            } else {
                it.put(&v, k, nv)?;
            }
        }
    }
    let kv = Value::Str(match key {
        Key::Str(s) => s,
        Key::Sym(_) => Rc::from(""),
    });
    it.call(f, holder.clone(), &[kv, v])
}

pub fn quote(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

struct Ser {
    replacer: Value,
    allow: Option<Vec<Key>>,
    gap: String,
    stack: Vec<ObjRef>,
}

impl Ser {
    fn value(&mut self, it: &mut Interp, holder: &Value, key: &Key, indent: &str) -> R<Option<String>> {
        let mut v = it.get(holder, key)?;
        let kv = || match key {
            Key::Str(s) => Value::Str(s.clone()),
            Key::Sym(_) => Value::str(""),
        };
        if let Value::Obj(_) = v {
            let f = it.get(&v, &Key::str("toJSON"))?;
            if f.callable() {
                v = it.call(&f, v.clone(), &[kv()])?;
            }
        }
        if self.replacer.callable() {
            v = it.call(&self.replacer, holder.clone(), &[kv(), v])?;
        }
        if let Value::Obj(o) = &v {
            let prim = match &o.borrow().kind {
                Kind::Prim(p) => Some(p.clone()),
                _ => None,
            };
            if let Some(p) = prim {
                v = p;
            }
        }
        Ok(Some(match &v {
            Value::Null => String::from("null"),
            Value::Bool(b) => String::from(if *b { "true" } else { "false" }),
            Value::Num(n) => {
                if n.is_finite() {
                    num_to_string(*n)
                } else {
                    String::from("null")
                }
            }
            Value::Str(s) => {
                let mut out = String::new();
                quote(s, &mut out);
                out
            }
            Value::Undef | Value::Sym(_) => return Ok(None),
            Value::Obj(o) => {
                if o.borrow().callable() {
                    return Ok(None);
                }
                if self.stack.iter().any(|x| x.same(o)) || self.stack.len() > 200 {
                    return Err(it.type_error("Converting circular structure to JSON"));
                }
                self.stack.push(o.clone());
                let inner = format!("{indent}{}", self.gap);
                let is_array = matches!(o.borrow().kind, Kind::Array(_));
                let mut parts = Vec::new();
                if is_array {
                    let n = match &o.borrow().kind {
                        Kind::Array(a) => a.len(),
                        _ => 0,
                    };
                    for i in 0..n {
                        let k = Key::Str(Rc::from(num_to_string(i as f64).as_str()));
                        parts.push(self.value(it, &v, &k, &inner)?.unwrap_or_else(|| String::from("null")));
                    }
                } else {
                    let keys = match &self.allow {
                        Some(a) => a.clone(),
                        None => it.own_keys(&v, false),
                    };
                    for k in keys {
                        if let Some(s) = self.value(it, &v, &k, &inner)? {
                            let mut p = String::new();
                            quote(&it.key_text(&k), &mut p);
                            p.push(':');
                            if !self.gap.is_empty() {
                                p.push(' ');
                            }
                            p.push_str(&s);
                            parts.push(p);
                        }
                    }
                }
                self.stack.pop();
                let (open, close) = if is_array { ('[', ']') } else { ('{', '}') };
                if parts.is_empty() {
                    format!("{open}{close}")
                } else if self.gap.is_empty() {
                    format!("{open}{}{close}", parts.join(","))
                } else {
                    format!("{open}\n{inner}{}\n{indent}{close}", parts.join(&format!(",\n{inner}")))
                }
            }
        }))
    }
}

pub fn stringify(it: &mut Interp, v: &Value, replacer: &Value, space: &Value) -> R<Option<String>> {
    let gap = match space {
        Value::Num(n) => " ".repeat(n.clamp(0.0, 10.0) as usize),
        Value::Str(s) => s.chars().take(10).collect(),
        _ => String::new(),
    };
    let allow = match replacer {
        Value::Obj(o) if matches!(o.borrow().kind, Kind::Array(_)) => {
            let items = crate::array::list_of(it, replacer)?;
            let mut keys = Vec::new();
            for i in items {
                keys.push(it.to_key(&i)?);
            }
            Some(keys)
        }
        _ => None,
    };
    let holder = it.new_obj();
    holder.borrow_mut().props.insert(Key::str(""), Prop::data(v.clone()));
    let mut s = Ser { replacer: replacer.clone(), allow, gap, stack: Vec::new() };
    s.value(it, &Value::Obj(holder), &Key::str(""), "")
}

pub fn install(it: &mut Interp) {
    let j = it.new_obj();
    it.set_global("JSON", Value::Obj(j.clone()));
    j.borrow_mut().props.insert(Key::Sym(SYM_TO_STRING_TAG), Prop::hidden(Value::str("JSON")));
    func(it, &j, "parse", 2, |it, _, args, _| {
        let text = crate::builtins::str_arg(it, args, 0)?;
        let v = parse(it, &text)?;
        let f = arg(args, 1);
        if f.callable() {
            let root = it.new_obj();
            root.borrow_mut().props.insert(Key::str(""), Prop::data(v));
            return revive(it, &Value::Obj(root), Key::str(""), &f);
        }
        Ok(v)
    });
    func(it, &j, "stringify", 3, |it, _, args, _| {
        Ok(match stringify(it, &arg(args, 0), &arg(args, 1), &arg(args, 2))? {
            Some(s) => Value::str(&s),
            None => Value::Undef,
        })
    });
}

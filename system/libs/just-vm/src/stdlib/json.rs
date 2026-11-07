//! `import json`: JSON text ↔ JustScript values.
//!
//! Objects become maps (keys sorted), arrays lists, `null` nil. Integers
//! without fraction or exponent stay ints when they fit.

use super::text;
use crate::prelude::*;
use crate::value::{MapKey, Value};
use crate::vm::Vm;
use alloc::collections::BTreeMap;
use justc_core::bytecode::{OPTION_TYPE, SOME};
use core::fmt::Write;

/// Nesting deeper than this is rejected instead of overflowing the stack.
const MAX_DEPTH: usize = 256;

pub(super) fn call(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Value, String> {
    match name {
        "json.parse" => {
            let src = text(&args[0], name)?;
            let parsed = Parser { src: src.as_bytes(), pos: 0 }.document();
            Ok(match parsed {
                Ok(v) => vm.ok(v),
                Err(e) => vm.err(e),
            })
        }
        "json.stringify" => {
            let indent = match args.get(1) {
                None | Some(Value::Nil) => None,
                Some(Value::Int(n)) => Some(" ".repeat((*n).clamp(0, 16) as usize)),
                Some(Value::Str(s)) => Some(s.to_string()),
                Some(other) => return Err(format!("`json.stringify`: indent must be an int or string, found {}", other.type_name())),
            };
            Ok(Value::str(&stringify(&args[0], indent.as_deref())?))
        }
        _ => Err(format!("builtin `{name}` is not implemented by this runtime")),
    }
}

// ------------------------------------------------------------------ write

pub(crate) fn stringify(value: &Value, indent: Option<&str>) -> Result<String, String> {
    let mut out = String::new();
    write_value(&mut out, value, indent, 0)?;
    Ok(out)
}

fn write_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn newline(out: &mut String, indent: Option<&str>, depth: usize) {
    if let Some(ind) = indent {
        out.push('\n');
        for _ in 0..depth {
            out.push_str(ind);
        }
    }
}

/// `[a, b]` or `{"k": v}` with optional indentation.
fn write_seq<T>(
    out: &mut String,
    (open, close): (char, char),
    items: impl ExactSizeIterator<Item = T>,
    indent: Option<&str>,
    depth: usize,
    mut item: impl FnMut(&mut String, T) -> Result<(), String>,
) -> Result<(), String> {
    out.push(open);
    let empty = items.len() == 0;
    for (i, it) in items.enumerate() {
        if i > 0 {
            out.push(',');
        }
        newline(out, indent, depth + 1);
        item(out, it)?;
    }
    if !empty {
        newline(out, indent, depth);
    }
    out.push(close);
    Ok(())
}

fn write_value(out: &mut String, value: &Value, indent: Option<&str>, depth: usize) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err("`json.stringify`: value nested too deeply (or contains itself)".into());
    }
    let colon = if indent.is_some() { ": " } else { ":" };
    match value {
        Value::Nil => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Int(i) => {
            let _ = write!(out, "{i}");
        }
        Value::Float(f) if f.is_finite() => {
            let _ = write!(out, "{value}");
        }
        Value::Float(_) => out.push_str("null"),
        Value::Str(s) => write_str(out, s),
        Value::Range(a, b) => {
            let items: Vec<Value> = (*a..*b).take(10_000_000).map(Value::Int).collect();
            write_value(out, &Value::list(items), indent, depth)?;
        }
        Value::List(l) => {
            let l = l.borrow();
            write_seq(out, ('[', ']'), l.iter(), indent, depth, |out, v| write_value(out, v, indent, depth + 1))?;
        }
        Value::Map(m) => {
            let m = m.borrow();
            write_seq(out, ('{', '}'), m.iter(), indent, depth, |out, (k, v)| {
                write_str(out, &k.to_value().to_string());
                out.push_str(colon);
                write_value(out, v, indent, depth + 1)
            })?;
        }
        Value::Struct(s) => {
            let fields = s.fields.borrow();
            write_seq(out, ('{', '}'), s.ty.fields().iter().zip(fields.iter()), indent, depth, |out, (k, v)| {
                write_str(out, k);
                out.push_str(colon);
                write_value(out, v, indent, depth + 1)
            })?;
        }
        // Some(x) is x and None is null; other variants are "Name" or {"Name": value(s)}.
        Value::Variant(v) if v.type_index == OPTION_TYPE => match v.values.first() {
            Some(inner) if v.tag == SOME => write_value(out, inner, indent, depth)?,
            _ => out.push_str("null"),
        },
        Value::Variant(v) => match v.values.as_slice() {
            [] => write_str(out, v.name()),
            values => {
                let inner = if let [one] = values { one.clone() } else { Value::list(values.to_vec()) };
                write_seq(out, ('{', '}'), core::iter::once(&inner), indent, depth, |out, inner| {
                    write_str(out, v.name());
                    out.push_str(colon);
                    write_value(out, inner, indent, depth + 1)
                })?;
            }
        },
        other => return Err(format!("`json.stringify`: cannot convert {} to JSON", other.type_name())),
    }
    Ok(())
}

// ------------------------------------------------------------------- read

struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn document(mut self) -> Result<Value, String> {
        let v = self.value(0)?;
        self.ws();
        if self.pos < self.src.len() {
            return Err(self.error("unexpected text after the JSON value"));
        }
        Ok(v)
    }

    /// Line and column of the current position, for messages.
    fn error(&self, msg: &str) -> String {
        let before = &self.src[..self.pos.min(self.src.len())];
        let line = before.iter().filter(|&&b| b == b'\n').count() + 1;
        let col = before.iter().rev().take_while(|&&b| b != b'\n').count() + 1;
        format!("invalid JSON at line {line}, column {col}: {msg}")
    }

    fn ws(&mut self) {
        while self.pos < self.src.len() && matches!(self.src[self.pos], b' ' | b'\t' | b'\n' | b'\r') {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn expect(&mut self, b: u8) -> Result<(), String> {
        self.ws();
        if self.peek() == Some(b) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.error(&format!("expected `{}`", b as char)))
        }
    }

    fn keyword(&mut self, word: &str, v: Value) -> Result<Value, String> {
        if self.src[self.pos..].starts_with(word.as_bytes()) {
            self.pos += word.len();
            Ok(v)
        } else {
            Err(self.error("unexpected character"))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, String> {
        if depth > MAX_DEPTH {
            return Err(self.error("nested too deeply"));
        }
        self.ws();
        match self.peek() {
            None => Err(self.error("unexpected end of input")),
            Some(b'n') => self.keyword("null", Value::Nil),
            Some(b't') => self.keyword("true", Value::Bool(true)),
            Some(b'f') => self.keyword("false", Value::Bool(false)),
            Some(b'"') => Ok(Value::str(&self.string()?)),
            Some(b'[') => {
                self.pos += 1;
                let mut items = Vec::new();
                self.ws();
                if self.peek() == Some(b']') {
                    self.pos += 1;
                    return Ok(Value::list(items));
                }
                loop {
                    items.push(self.value(depth + 1)?);
                    self.ws();
                    match self.peek() {
                        Some(b',') => self.pos += 1,
                        Some(b']') => {
                            self.pos += 1;
                            return Ok(Value::list(items));
                        }
                        _ => return Err(self.error("expected `,` or `]`")),
                    }
                }
            }
            Some(b'{') => {
                self.pos += 1;
                let mut map = BTreeMap::new();
                self.ws();
                if self.peek() == Some(b'}') {
                    self.pos += 1;
                    return Ok(Value::map(map));
                }
                loop {
                    self.ws();
                    if self.peek() != Some(b'"') {
                        return Err(self.error("expected a string key"));
                    }
                    let key = self.string()?;
                    self.expect(b':')?;
                    let v = self.value(depth + 1)?;
                    map.insert(MapKey::Str(key.into()), v);
                    self.ws();
                    match self.peek() {
                        Some(b',') => self.pos += 1,
                        Some(b'}') => {
                            self.pos += 1;
                            return Ok(Value::map(map));
                        }
                        _ => return Err(self.error("expected `,` or `}`")),
                    }
                }
            }
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.error("unexpected character")),
        }
    }

    fn number(&mut self) -> Result<Value, String> {
        let start = self.pos;
        let mut float = false;
        while let Some(b) = self.peek() {
            match b {
                b'0'..=b'9' | b'-' | b'+' => {}
                b'.' | b'e' | b'E' => float = true,
                _ => break,
            }
            self.pos += 1;
        }
        let s = core::str::from_utf8(&self.src[start..self.pos]).unwrap_or("");
        if !float {
            if let Ok(i) = s.parse::<i64>() {
                return Ok(Value::Int(i));
            }
        }
        s.parse::<f64>().map(Value::Float).map_err(|_| {
            self.pos = start;
            self.error("invalid number")
        })
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let digits = self.src.get(self.pos..self.pos + 4).and_then(|d| core::str::from_utf8(d).ok());
        let v = digits.and_then(|d| u32::from_str_radix(d, 16).ok()).ok_or_else(|| self.error("invalid \\u escape"))?;
        self.pos += 4;
        Ok(v)
    }

    fn string(&mut self) -> Result<String, String> {
        self.pos += 1; // opening quote
        let mut out = String::new();
        loop {
            let start = self.pos;
            while self.pos < self.src.len() && !matches!(self.src[self.pos], b'"' | b'\\') && self.src[self.pos] >= 0x20 {
                self.pos += 1;
            }
            // The input is a &str, so runs between ASCII delimiters are valid UTF-8.
            out.push_str(core::str::from_utf8(&self.src[start..self.pos]).map_err(|_| self.error("invalid UTF-8"))?);
            match self.peek() {
                None => return Err(self.error("unterminated string")),
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.pos += 1;
                    let esc = self.peek().ok_or_else(|| self.error("unterminated string"))?;
                    self.pos += 1;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let mut code = self.hex4()?;
                            if (0xD800..0xDC00).contains(&code) && self.src[self.pos..].starts_with(b"\\u") {
                                self.pos += 2;
                                let low = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&low) {
                                    return Err(self.error("invalid surrogate pair"));
                                }
                                code = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                            }
                            out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                        }
                        _ => {
                            self.pos -= 1;
                            return Err(self.error("invalid escape"));
                        }
                    }
                }
                Some(_) => return Err(self.error("control character in string")),
            }
        }
    }
}

//! `String` and `RegExp`.
//!
//! Strings are kept as UTF-8; indices count characters. For speed the
//! interpreter remembers where the characters of the last string it indexed
//! start, so `s[i]` in a loop doesn't rescan `s`.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::builtins::{ctor, func, make_iter, num_arg, str_arg};
use crate::interp::*;
use crate::regex::{Caps, Regex};
use crate::value::*;

/// Character count (uncached).
pub fn len(s: &str) -> usize {
    if s.is_ascii() {
        s.len()
    } else {
        s.chars().count()
    }
}

/// The character at a character index (uncached).
pub fn char_at(s: &str, i: usize) -> Option<char> {
    if s.is_ascii() {
        s.as_bytes().get(i).map(|b| *b as char)
    } else {
        s.chars().nth(i)
    }
}

/// Where the characters of one string start.
#[derive(Default)]
pub struct StrCache {
    s: Option<Rc<str>>,
    /// `None` when the string is ASCII.
    offs: Option<Vec<u32>>,
}

fn prepare(it: &mut Interp, s: &Rc<str>) {
    if it.strcache.s.as_ref().is_some_and(|c| Rc::ptr_eq(c, s)) {
        return;
    }
    let offs = if s.is_ascii() { None } else { Some(s.char_indices().map(|(b, _)| b as u32).collect()) };
    it.strcache = StrCache { s: Some(s.clone()), offs };
}

pub fn clen(it: &mut Interp, s: &Rc<str>) -> usize {
    if s.len() < 64 {
        return len(s);
    }
    prepare(it, s);
    match &it.strcache.offs {
        None => s.len(),
        Some(o) => o.len(),
    }
}

/// The byte offset of a character index.
pub fn byte_at(it: &mut Interp, s: &Rc<str>, ci: usize) -> usize {
    if s.len() < 64 {
        return s.char_indices().nth(ci).map_or(s.len(), |x| x.0);
    }
    prepare(it, s);
    match &it.strcache.offs {
        None => ci.min(s.len()),
        Some(o) => o.get(ci).map_or(s.len(), |b| *b as usize),
    }
}

/// The character index of a byte offset.
pub fn char_index(it: &mut Interp, s: &Rc<str>, byte: usize) -> usize {
    if s.len() < 64 {
        return s[..byte.min(s.len())].chars().count();
    }
    prepare(it, s);
    match &it.strcache.offs {
        None => byte,
        Some(o) => o.partition_point(|x| (*x as usize) < byte),
    }
}

pub fn cchar(it: &mut Interp, s: &Rc<str>, ci: usize) -> Option<char> {
    let b = byte_at(it, s, ci);
    s.get(b..).and_then(|r| r.chars().next())
}

fn cslice(it: &mut Interp, s: &Rc<str>, from: usize, to: usize) -> Value {
    if from == 0 && to >= clen(it, s) {
        return Value::Str(s.clone());
    }
    let a = byte_at(it, s, from);
    let b = byte_at(it, s, to.max(from));
    Value::str(&s[a..b])
}

fn this_str(it: &mut Interp, this: &Value) -> R<Rc<str>> {
    match this {
        Value::Str(s) => Ok(s.clone()),
        Value::Undef | Value::Null => Err(it.type_error("String.prototype method called on null or undefined")),
        v => it.to_str(v),
    }
}

fn opt_num(it: &mut Interp, args: &[Value], i: usize, default: f64) -> R<f64> {
    match arg(args, i) {
        Value::Undef => Ok(default),
        v => it.to_number(&v),
    }
}

fn regex_of(v: &Value) -> Option<Rc<Regex>> {
    match v {
        Value::Obj(o) => match &o.borrow().kind {
            Kind::RegExp(r) => Some(r.clone()),
            _ => None,
        },
        _ => None,
    }
}

pub fn new_regexp(it: &mut Interp, pattern: &str, flags: &str) -> R<Value> {
    let re = Regex::new(pattern, flags).map_err(|e| it.syntax_error(&alloc::format!("Invalid regular expression: /{pattern}/: {e}")))?;
    let p = it.p.regexp.clone();
    let o = it.alloc(Some(p), Kind::RegExp(Rc::new(re)));
    o.borrow_mut().props.insert(Key::str("lastIndex"), Prop::hidden(Value::Num(0.0)));
    Ok(Value::Obj(o))
}

/// The array `exec` returns.
fn match_array(it: &mut Interp, re: &Regex, chars: &[char], caps: &Caps, input: &Rc<str>) -> R<Value> {
    let items: Vec<Value> = caps.iter().map(|c| c.map_or(Value::Undef, |(a, b)| Value::str(&chars[a..b].iter().collect::<String>()))).collect();
    let a = it.new_array(items.clone());
    let Value::Obj(o) = &a else { return Ok(a) };
    let index = caps[0].map_or(0, |c| c.0);
    o.borrow_mut().props.insert(Key::str("index"), Prop::data(Value::Num(index as f64)));
    o.borrow_mut().props.insert(Key::str("input"), Prop::data(Value::Str(input.clone())));
    let groups = if re.names.is_empty() {
        Value::Undef
    } else {
        let g = it.new_obj();
        for (name, i) in &re.names {
            g.borrow_mut().props.insert(Key::str(name), Prop::data(items.get(*i).cloned().unwrap_or_default()));
        }
        Value::Obj(g)
    };
    o.borrow_mut().props.insert(Key::str("groups"), Prop::data(groups));
    Ok(a)
}

fn last_index(it: &mut Interp, this: &Value) -> R<usize> {
    let v = it.get(this, &Key::str("lastIndex"))?;
    Ok(to_int(it.to_number(&v)?).max(0.0) as usize)
}

fn regexp_exec(it: &mut Interp, this: &Value, s: &Rc<str>) -> R<Value> {
    let Some(re) = regex_of(this) else { return Err(it.type_error("not a RegExp")) };
    let chars: Vec<char> = s.chars().collect();
    let keep = re.flags.global || re.flags.sticky;
    let start = if keep { last_index(it, this)? } else { 0 };
    if start > chars.len() {
        it.put(this, Key::str("lastIndex"), Value::Num(0.0))?;
        return Ok(Value::Null);
    }
    match re.exec(&chars, start) {
        None => {
            if keep {
                it.put(this, Key::str("lastIndex"), Value::Num(0.0))?;
            }
            Ok(Value::Null)
        }
        Some(caps) => {
            if keep {
                let end = caps[0].map_or(0, |c| c.1);
                it.put(this, Key::str("lastIndex"), Value::Num(end as f64))?;
            }
            match_array(it, &re, &chars, &caps, s)
        }
    }
}

/// `$&`, `$1`, `$<name>`… in a replacement.
fn expand(tpl: &str, chars: &[char], caps: &Caps, names: &[(String, usize)]) -> String {
    let piece = |c: Option<(usize, usize)>| c.map(|(a, b)| chars[a..b].iter().collect::<String>()).unwrap_or_default();
    let (ms, me) = caps[0].unwrap_or((0, 0));
    let mut out = String::new();
    let t: Vec<char> = tpl.chars().collect();
    let mut i = 0;
    while i < t.len() {
        if t[i] != '$' || i + 1 >= t.len() {
            out.push(t[i]);
            i += 1;
            continue;
        }
        match t[i + 1] {
            '$' => {
                out.push('$');
                i += 2;
            }
            '&' => {
                out.push_str(&piece(caps[0]));
                i += 2;
            }
            '`' => {
                out.extend(&chars[..ms]);
                i += 2;
            }
            '\'' => {
                out.extend(&chars[me..]);
                i += 2;
            }
            '<' => match t[i + 2..].iter().position(|c| *c == '>') {
                Some(e) => {
                    let name: String = t[i + 2..i + 2 + e].iter().collect();
                    if let Some((_, g)) = names.iter().find(|n| n.0 == name) {
                        out.push_str(&piece(caps.get(*g).copied().flatten()));
                    }
                    i += 3 + e;
                }
                None => {
                    out.push('$');
                    i += 1;
                }
            },
            d if d.is_ascii_digit() => {
                let one = d as usize - '0' as usize;
                let two = t.get(i + 2).filter(|c| c.is_ascii_digit()).map(|c| one * 10 + (*c as usize - '0' as usize));
                match two {
                    Some(n) if n >= 1 && n < caps.len() => {
                        out.push_str(&piece(caps[n]));
                        i += 3;
                    }
                    _ if one >= 1 && one < caps.len() => {
                        out.push_str(&piece(caps[one]));
                        i += 2;
                    }
                    _ => {
                        out.push('$');
                        i += 1;
                    }
                }
            }
            _ => {
                out.push('$');
                i += 1;
            }
        }
    }
    out
}

fn replace(it: &mut Interp, this: &Value, args: &[Value], all: bool) -> R<Value> {
    let s = this_str(it, this)?;
    let pat = arg(args, 0);
    let rep = arg(args, 1);
    let rep_str = if rep.callable() { None } else { Some(it.to_str(&rep)?) };
    let chars: Vec<char> = s.chars().collect();
    // Matches as character ranges with their groups.
    let mut found: Vec<Caps> = Vec::new();
    let names;
    if let Some(re) = regex_of(&pat) {
        if all && !re.flags.global {
            return Err(it.type_error("replaceAll must be called with a global RegExp"));
        }
        names = re.names.clone();
        let global = re.flags.global;
        let mut pos = if global { 0 } else { last_index(it, &pat).unwrap_or(0) * usize::from(re.flags.sticky) };
        while pos <= chars.len() {
            let Some(c) = re.exec(&chars, pos) else { break };
            let (a, b) = c[0].unwrap_or((pos, pos));
            found.push(c);
            if !global {
                break;
            }
            pos = if b == a { b + 1 } else { b };
        }
        if global || re.flags.sticky {
            it.put(&pat, Key::str("lastIndex"), Value::Num(0.0))?;
        }
    } else {
        names = Vec::new();
        let p: Vec<char> = it.to_str(&pat)?.chars().collect();
        let mut pos = 0;
        while pos <= chars.len() {
            let at = if p.is_empty() { Some(pos) } else { chars[pos..].windows(p.len()).position(|w| w == p.as_slice()).map(|d| d + pos) };
            let Some(a) = at else { break };
            found.push(vec![Some((a, a + p.len()))]);
            if !all {
                break;
            }
            pos = if p.is_empty() { a + 1 } else { a + p.len() };
        }
    }
    if found.is_empty() {
        return Ok(Value::Str(s));
    }
    let mut out = String::new();
    let mut last = 0;
    for caps in &found {
        let (a, b) = caps[0].unwrap_or((last, last));
        out.extend(&chars[last..a]);
        match &rep_str {
            Some(t) => out.push_str(&expand(t, &chars, caps, &names)),
            None => {
                let mut cargs: Vec<Value> = caps.iter().map(|c| c.map_or(Value::Undef, |(x, y)| Value::str(&chars[x..y].iter().collect::<String>()))).collect();
                cargs.push(Value::Num(a as f64));
                cargs.push(Value::Str(s.clone()));
                if !names.is_empty() {
                    let g = it.new_obj();
                    for (n, i) in &names {
                        g.borrow_mut().props.insert(Key::str(n), Prop::data(cargs.get(*i).cloned().unwrap_or_default()));
                    }
                    cargs.push(Value::Obj(g));
                }
                let r = it.call(&rep, Value::Undef, &cargs)?;
                out.push_str(&it.to_str(&r)?);
            }
        }
        last = b;
    }
    out.extend(&chars[last.min(chars.len())..]);
    Ok(Value::str(&out))
}

fn split(it: &mut Interp, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let s = this_str(it, this)?;
    let sep = arg(args, 0);
    let limit = match arg(args, 1) {
        Value::Undef => usize::MAX,
        v => to_u32(it.to_number(&v)?) as usize,
    };
    let mut out: Vec<Value> = Vec::new();
    if limit == 0 {
        return Ok(it.new_array(out));
    }
    if matches!(sep, Value::Undef) {
        return Ok(it.new_array(vec![Value::Str(s)]));
    }
    if let Some(re) = regex_of(&sep) {
        let chars: Vec<char> = s.chars().collect();
        if chars.is_empty() {
            let m = re.exec(&chars, 0);
            return Ok(it.new_array(if m.is_some() { Vec::new() } else { vec![Value::Str(s)] }));
        }
        let mut last = 0;
        let mut pos = 0;
        while pos < chars.len() {
            let Some(c) = re.exec(&chars, pos) else { break };
            let (a, b) = c[0].unwrap_or((pos, pos));
            if a >= chars.len() {
                break;
            }
            if b == a && a == last {
                pos = a + 1;
                continue;
            }
            out.push(Value::str(&chars[last..a].iter().collect::<String>()));
            if out.len() >= limit {
                return Ok(it.new_array(out));
            }
            for g in c.iter().skip(1) {
                out.push(g.map_or(Value::Undef, |(x, y)| Value::str(&chars[x..y].iter().collect::<String>())));
            }
            last = b;
            pos = if b == a { b + 1 } else { b };
        }
        out.push(Value::str(&chars[last..].iter().collect::<String>()));
        out.truncate(limit);
        return Ok(it.new_array(out));
    }
    let sep = it.to_str(&sep)?;
    if sep.is_empty() {
        out = s.chars().take(limit).map(|c| Value::str(c.encode_utf8(&mut [0; 4]))).collect();
    } else {
        out = s.split(&*sep).take(limit).map(Value::str).collect();
    }
    Ok(it.new_array(out))
}

fn trim_ws(c: char) -> bool {
    c.is_whitespace() || c == '\u{feff}'
}

fn pad(it: &mut Interp, this: &Value, args: &[Value], start: bool) -> R<Value> {
    let s = this_str(it, this)?;
    let target = to_int(num_arg(it, args, 0)?).max(0.0) as usize;
    let fill = match arg(args, 1) {
        Value::Undef => Rc::from(" "),
        v => it.to_str(&v)?,
    };
    let n = clen(it, &s);
    if target <= n || fill.is_empty() {
        return Ok(Value::Str(s));
    }
    let padding: String = fill.chars().cycle().take(target - n).collect();
    Ok(Value::str(&if start { padding + &s } else { String::from(&*s) + &padding }))
}

pub fn install(it: &mut Interp) {
    let proto = it.p.string.clone();
    let c = ctor(it, "String", 1, |it, this, args, _| {
        let s = match args.first() {
            None => Rc::from(""),
            Some(Value::Sym(n)) if it.new_target.nullish() => Rc::from(alloc::format!("Symbol({})", it.symbols[*n as usize]).as_str()),
            Some(v) => it.to_str(v)?,
        };
        if !it.new_target.nullish() {
            if let Value::Obj(o) = this {
                o.borrow_mut().kind = Kind::Prim(Value::Str(s));
            }
            return Ok(this.clone());
        }
        Ok(Value::Str(s))
    }, &proto);
    func(it, &c, "fromCharCode", 1, |it, _, args, _| {
        let mut units = Vec::new();
        for i in 0..args.len() {
            units.push(to_u32(num_arg(it, args, i)?) as u16);
        }
        Ok(Value::str(&String::from_utf16_lossy(&units)))
    });
    func(it, &c, "fromCodePoint", 1, |it, _, args, _| {
        let mut s = String::new();
        for i in 0..args.len() {
            let n = num_arg(it, args, i)?;
            match char::from_u32(n as u32).filter(|_| n >= 0.0 && n == to_int(n)) {
                Some(ch) => s.push(ch),
                None => return Err(it.range_error("Invalid code point")),
            }
        }
        Ok(Value::str(&s))
    });
    func(it, &c, "raw", 1, |it, _, args, _| {
        let strings = it.get(&arg(args, 0), &Key::str("raw"))?;
        let parts = crate::array::list_of(it, &strings)?;
        let mut s = String::new();
        for (i, p) in parts.iter().enumerate() {
            s.push_str(&it.to_str(p)?);
            if i + 1 < parts.len() {
                if let Some(v) = args.get(i + 1) {
                    s.push_str(&it.to_str(v)?);
                }
            }
        }
        Ok(Value::str(&s))
    });

    func(it, &proto, "toString", 0, |it, this, _, _| this_str(it, this).map(Value::Str));
    func(it, &proto, "valueOf", 0, |it, this, _, _| this_str(it, this).map(Value::Str));
    func(it, &proto, "charAt", 1, |it, this, args, _| {
        let s = this_str(it, this)?;
        let i = to_int(num_arg(it, args, 0)?);
        if i < 0.0 {
            return Ok(Value::str(""));
        }
        Ok(cchar(it, &s, i as usize).map_or(Value::str(""), |c| Value::str(c.encode_utf8(&mut [0; 4]))))
    });
    func(it, &proto, "at", 1, |it, this, args, _| {
        let s = this_str(it, this)?;
        let n = clen(it, &s) as f64;
        let i = to_int(num_arg(it, args, 0)?);
        let i = if i < 0.0 { n + i } else { i };
        if i < 0.0 || i >= n {
            return Ok(Value::Undef);
        }
        Ok(cchar(it, &s, i as usize).map_or(Value::Undef, |c| Value::str(c.encode_utf8(&mut [0; 4]))))
    });
    fn code(it: &mut Interp, this: &Value, args: &[Value]) -> R<Option<char>> {
        let s = this_str(it, this)?;
        let i = to_int(num_arg(it, args, 0)?);
        if i < 0.0 {
            return Ok(None);
        }
        Ok(cchar(it, &s, i as usize))
    }
    func(it, &proto, "charCodeAt", 1, |it, this, args, _| Ok(Value::Num(code(it, this, args)?.map_or(f64::NAN, |c| {
        let mut b = [0u16; 2];
        f64::from(c.encode_utf16(&mut b)[0])
    }))));
    func(it, &proto, "codePointAt", 1, |it, this, args, _| Ok(code(it, this, args)?.map_or(Value::Undef, |c| Value::Num(f64::from(c as u32)))));
    func(it, &proto, "indexOf", 1, |it, this, args, _| {
        let s = this_str(it, this)?;
        let p = str_arg(it, args, 0)?;
        let from = to_int(num_arg(it, args, 1)?).max(0.0) as usize;
        let b = byte_at(it, &s, from);
        Ok(Value::Num(match s[b..].find(&*p) {
            Some(x) => char_index(it, &s, b + x) as f64,
            None => -1.0,
        }))
    });
    func(it, &proto, "lastIndexOf", 1, |it, this, args, _| {
        let s = this_str(it, this)?;
        let p = str_arg(it, args, 0)?;
        let end = match arg(args, 1) {
            Value::Undef => s.len(),
            v => {
                let n = it.to_number(&v)?;
                let n = if n.is_nan() { f64::INFINITY } else { n };
                let ci = to_int(n).max(0.0) as usize;
                (byte_at(it, &s, ci) + p.len()).min(s.len())
            }
        };
        Ok(Value::Num(match s[..end].rfind(&*p) {
            Some(x) => char_index(it, &s, x) as f64,
            None => -1.0,
        }))
    });
    func(it, &proto, "includes", 1, |it, this, args, _| {
        let s = this_str(it, this)?;
        if regex_of(&arg(args, 0)).is_some() {
            return Err(it.type_error("First argument to String.prototype.includes must not be a regular expression"));
        }
        let p = str_arg(it, args, 0)?;
        let from = to_int(num_arg(it, args, 1)?).max(0.0) as usize;
        let b = byte_at(it, &s, from);
        Ok(Value::Bool(s[b..].contains(&*p)))
    });
    func(it, &proto, "startsWith", 1, |it, this, args, _| {
        let s = this_str(it, this)?;
        let p = str_arg(it, args, 0)?;
        let from = to_int(num_arg(it, args, 1)?).max(0.0) as usize;
        let b = byte_at(it, &s, from);
        Ok(Value::Bool(s[b..].starts_with(&*p)))
    });
    func(it, &proto, "endsWith", 1, |it, this, args, _| {
        let s = this_str(it, this)?;
        let p = str_arg(it, args, 0)?;
        let end = match arg(args, 1) {
            Value::Undef => s.len(),
            v => {
                let n = to_int(it.to_number(&v)?).max(0.0) as usize;
                byte_at(it, &s, n)
            }
        };
        Ok(Value::Bool(s[..end].ends_with(&*p)))
    });
    func(it, &proto, "slice", 2, |it, this, args, _| {
        let s = this_str(it, this)?;
        let n = clen(it, &s);
        let a = rel_index(opt_num(it, args, 0, 0.0)?, n);
        let b = rel_index(opt_num(it, args, 1, n as f64)?, n);
        Ok(cslice(it, &s, a, b))
    });
    func(it, &proto, "substring", 2, |it, this, args, _| {
        let s = this_str(it, this)?;
        let n = clen(it, &s);
        let a = to_int(opt_num(it, args, 0, 0.0)?).clamp(0.0, n as f64) as usize;
        let b = to_int(opt_num(it, args, 1, n as f64)?).clamp(0.0, n as f64) as usize;
        Ok(cslice(it, &s, a.min(b), a.max(b)))
    });
    func(it, &proto, "substr", 2, |it, this, args, _| {
        let s = this_str(it, this)?;
        let n = clen(it, &s);
        let a = rel_index(opt_num(it, args, 0, 0.0)?, n);
        let l = to_int(opt_num(it, args, 1, n as f64)?).clamp(0.0, (n - a) as f64) as usize;
        Ok(cslice(it, &s, a, a + l))
    });
    func(it, &proto, "toUpperCase", 0, |it, this, _, _| Ok(Value::str(&this_str(it, this)?.to_uppercase())));
    func(it, &proto, "toLowerCase", 0, |it, this, _, _| Ok(Value::str(&this_str(it, this)?.to_lowercase())));
    func(it, &proto, "toLocaleUpperCase", 0, |it, this, _, _| Ok(Value::str(&this_str(it, this)?.to_uppercase())));
    func(it, &proto, "toLocaleLowerCase", 0, |it, this, _, _| Ok(Value::str(&this_str(it, this)?.to_lowercase())));
    func(it, &proto, "trim", 0, |it, this, _, _| Ok(Value::str(this_str(it, this)?.trim_matches(trim_ws))));
    for name in ["trimStart", "trimLeft"] {
        func(it, &proto, name, 0, |it, this, _, _| Ok(Value::str(this_str(it, this)?.trim_start_matches(trim_ws))));
    }
    for name in ["trimEnd", "trimRight"] {
        func(it, &proto, name, 0, |it, this, _, _| Ok(Value::str(this_str(it, this)?.trim_end_matches(trim_ws))));
    }
    func(it, &proto, "padStart", 2, |it, this, args, _| pad(it, this, args, true));
    func(it, &proto, "padEnd", 2, |it, this, args, _| pad(it, this, args, false));
    func(it, &proto, "repeat", 1, |it, this, args, _| {
        let s = this_str(it, this)?;
        let n = to_int(num_arg(it, args, 0)?);
        if n < 0.0 || n.is_infinite() || n * s.len() as f64 > 1e8 {
            return Err(it.range_error("Invalid count value"));
        }
        Ok(Value::str(&s.repeat(n as usize)))
    });
    func(it, &proto, "concat", 1, |it, this, args, _| {
        let mut s = String::from(&*this_str(it, this)?);
        for a in args {
            s.push_str(&it.to_str(a)?);
        }
        Ok(Value::str(&s))
    });
    func(it, &proto, "localeCompare", 1, |it, this, args, _| {
        let a = this_str(it, this)?;
        let b = str_arg(it, args, 0)?;
        let key = |s: &str| s.chars().map(fold_accent).collect::<String>();
        let o = key(&a).cmp(&key(&b)).then_with(|| a.cmp(&b));
        Ok(Value::Num(match o {
            core::cmp::Ordering::Less => -1.0,
            core::cmp::Ordering::Equal => 0.0,
            core::cmp::Ordering::Greater => 1.0,
        }))
    });
    func(it, &proto, "normalize", 0, |it, this, _, _| this_str(it, this).map(Value::Str));
    func(it, &proto, "split", 2, split);
    func(it, &proto, "replace", 2, |it, this, args, _| replace(it, this, args, false));
    func(it, &proto, "replaceAll", 2, |it, this, args, _| replace(it, this, args, true));
    func(it, &proto, "match", 1, |it, this, args, _| {
        let s = this_str(it, this)?;
        let re = match arg(args, 0) {
            v if regex_of(&v).is_some() => v,
            Value::Undef => new_regexp(it, "(?:)", "")?,
            v => {
                let p = it.to_str(&v)?;
                new_regexp(it, &p, "")?
            }
        };
        let r = regex_of(&re).expect("a RegExp");
        if !r.flags.global {
            return regexp_exec(it, &re, &s);
        }
        let chars: Vec<char> = s.chars().collect();
        let mut out = Vec::new();
        let mut pos = 0;
        while pos <= chars.len() {
            let Some(c) = r.exec(&chars, pos) else { break };
            let (a, b) = c[0].unwrap_or((pos, pos));
            out.push(Value::str(&chars[a..b].iter().collect::<String>()));
            pos = if b == a { b + 1 } else { b };
        }
        it.put(&re, Key::str("lastIndex"), Value::Num(0.0))?;
        Ok(if out.is_empty() { Value::Null } else { it.new_array(out) })
    });
    func(it, &proto, "matchAll", 1, |it, this, args, _| {
        let s = this_str(it, this)?;
        let re = match arg(args, 0) {
            v if regex_of(&v).is_some() => v,
            v => {
                let p = it.to_str(&v)?;
                new_regexp(it, &p, "g")?
            }
        };
        let r = regex_of(&re).expect("a RegExp");
        if !r.flags.global {
            return Err(it.type_error("String.prototype.matchAll called with a non-global RegExp argument"));
        }
        let chars: Vec<char> = s.chars().collect();
        let mut out = Vec::new();
        let mut pos = 0;
        while pos <= chars.len() {
            let Some(c) = r.exec(&chars, pos) else { break };
            let (a, b) = c[0].unwrap_or((pos, pos));
            out.push(match_array(it, &r, &chars, &c, &s)?);
            pos = if b == a { b + 1 } else { b };
        }
        Ok(make_iter(it, out))
    });
    func(it, &proto, "search", 1, |it, this, args, _| {
        let s = this_str(it, this)?;
        let re = match regex_of(&arg(args, 0)) {
            Some(r) => r,
            None => {
                let p = str_arg(it, args, 0)?;
                Rc::new(Regex::new(&p, "").map_err(|e| it.syntax_error(&e))?)
            }
        };
        let chars: Vec<char> = s.chars().collect();
        let found = re.exec(&chars, 0);
        Ok(Value::Num(found.and_then(|c| c[0]).map_or(-1.0, |c| c.0 as f64)))
    });
    let iter = it.native("[Symbol.iterator]", 0, |it, this, _, _| {
        let s = this_str(it, this)?;
        let items = s.chars().map(|c| Value::str(c.encode_utf8(&mut [0; 4]))).collect();
        Ok(make_iter(it, items))
    }, Value::Undef, false);
    proto.borrow_mut().props.insert(Key::Sym(SYM_ITERATOR), Prop::hidden(Value::Obj(iter)));
    for (name, tag) in [("bold", "b"), ("italics", "i"), ("big", "big"), ("small", "small")] {
        let f = it.native(name, 0, |it, this, _, data| {
            let s = this_str(it, this)?;
            let t = it.to_str(data)?;
            Ok(Value::str(&alloc::format!("<{t}>{s}</{t}>")))
        }, Value::str(tag), false);
        it.set_hidden(&proto, name, Value::Obj(f));
    }

    regexp(it);
}

/// For sorting: letters without their accents, in lower case.
fn fold_accent(c: char) -> char {
    let c = c.to_lowercase().next().unwrap_or(c);
    match c {
        'à' | 'á' | 'â' | 'ä' | 'ã' | 'å' => 'a',
        'è' | 'é' | 'ê' | 'ë' => 'e',
        'ì' | 'í' | 'î' | 'ï' => 'i',
        'ò' | 'ó' | 'ô' | 'ö' | 'õ' => 'o',
        'ù' | 'ú' | 'û' | 'ü' => 'u',
        'ç' => 'c',
        'ñ' => 'n',
        c => c,
    }
}

fn regexp(it: &mut Interp) {
    let proto = it.p.regexp.clone();
    ctor(it, "RegExp", 2, |it, _, args, _| {
        let p = arg(args, 0);
        let (src, flags) = match regex_of(&p) {
            Some(r) => (String::from(&*r.source), String::from(&*r.flags_text)),
            None => (if matches!(p, Value::Undef) { String::from("(?:)") } else { String::from(&*it.to_str(&p)?) }, String::new()),
        };
        let flags = match arg(args, 1) {
            Value::Undef => flags,
            v => String::from(&*it.to_str(&v)?),
        };
        new_regexp(it, &src, &flags)
    }, &proto);
    func(it, &proto, "exec", 1, |it, this, args, _| {
        let s = str_arg(it, args, 0)?;
        regexp_exec(it, this, &s)
    });
    func(it, &proto, "test", 1, |it, this, args, _| {
        let s = str_arg(it, args, 0)?;
        Ok(Value::Bool(!matches!(regexp_exec(it, this, &s)?, Value::Null)))
    });
    func(it, &proto, "toString", 0, |it, this, _, _| match regex_of(this) {
        Some(r) => Ok(Value::str(&alloc::format!("/{}/{}", r.source, r.flags_text))),
        None => Err(it.type_error("not a RegExp")),
    });
    fn flag(it: &mut Interp, this: &Value, _: &[Value], data: &Value) -> R<Value> {
        let Some(r) = regex_of(this) else { return Ok(Value::Undef) };
        let name = it.to_str(data)?;
        Ok(match &*name {
            "source" => Value::Str(r.source.clone()),
            "flags" => Value::Str(r.flags_text.clone()),
            "global" => Value::Bool(r.flags.global),
            "ignoreCase" => Value::Bool(r.flags.icase),
            "multiline" => Value::Bool(r.flags.multiline),
            "dotAll" => Value::Bool(r.flags.dotall),
            "sticky" => Value::Bool(r.flags.sticky),
            "unicode" => Value::Bool(r.flags.unicode),
            _ => Value::Bool(false),
        })
    }
    for name in ["source", "flags", "global", "ignoreCase", "multiline", "dotAll", "sticky", "unicode", "hasIndices"] {
        it.accessor(&proto, name, flag, None, Value::str(name));
    }
}

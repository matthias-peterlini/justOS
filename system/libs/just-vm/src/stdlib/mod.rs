//! The standard library modules (`import math`, `import fs`, ...). Their
//! functions are the builtins named `module.function`.

use crate::prelude::*;
mod http;
pub(crate) mod json;
mod time;

use crate::value::{MapKey, Value};
use crate::vm::Vm;
use alloc::collections::BTreeMap;
use justc_core::bytecode::builtin_index;

pub(crate) fn call(vm: &mut Vm, name: &str, args: Vec<Value>) -> Result<Value, String> {
    let (module, _) = name.split_once('.').unwrap_or((name, ""));
    match module {
        "math" => math(vm, name, &args),
        "random" => random(vm, name, &args),
        "time" => time::call(vm, name, &args),
        "fs" => Ok(fs(vm, name, &args)?),
        "path" => path(name, &args),
        "json" => json::call(vm, name, &args),
        "os" => os(vm, name, &args),
        "http" => http::call(vm, name, &args),
        _ => Err(format!("builtin `{name}` is not implemented by this runtime")),
    }
}

/// A map with string keys.
pub(crate) fn record(fields: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    Value::map(fields.into_iter().map(|(k, v)| (MapKey::Str(k.into()), v)).collect::<BTreeMap<_, _>>())
}

fn num(v: &Value, name: &str) -> Result<f64, String> {
    match v {
        Value::Int(i) => Ok(*i as f64),
        Value::Float(f) => Ok(*f),
        other => Err(format!("`{name}` expects a number, found {}", other.type_name())),
    }
}

fn int(v: &Value, name: &str) -> Result<i64, String> {
    match v {
        Value::Int(i) => Ok(*i),
        other => Err(format!("`{name}` expects an int, found {}", other.type_name())),
    }
}

pub(crate) fn text<'a>(v: &'a Value, name: &str) -> Result<&'a str, String> {
    match v {
        Value::Str(s) => Ok(s),
        other => Err(format!("`{name}` expects a string, found {}", other.type_name())),
    }
}

/// A float with no fractional part as an int.
fn to_int(x: f64, name: &str) -> Result<Value, String> {
    if x.is_finite() && x.abs() < 9.2e18 {
        Ok(Value::Int(x as i64))
    } else {
        Err(format!("`{name}`: {x} does not fit in an int"))
    }
}

// ------------------------------------------------------------------- math

fn math(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Value, String> {
    let x = || num(&args[0], name);
    let float = |f: fn(f64) -> f64| Ok(Value::Float(f(num(&args[0], name)?)));
    match name {
        "math.sin" => float(f64::sin),
        "math.cos" => float(f64::cos),
        "math.tan" => float(f64::tan),
        "math.asin" => float(f64::asin),
        "math.acos" => float(f64::acos),
        "math.atan" => float(f64::atan),
        "math.atan2" => Ok(Value::Float(x()?.atan2(num(&args[1], name)?))),
        "math.exp" => float(f64::exp),
        "math.log" => Ok(Value::Float(match args.get(1) {
            Some(base) => x()?.log(num(base, name)?),
            None => x()?.ln(),
        })),
        "math.log10" => float(f64::log10),
        "math.log2" => float(f64::log2),
        "math.pow" => match (&args[0], &args[1]) {
            (Value::Int(b), Value::Int(e)) if *e >= 0 => {
                let e = u32::try_from(*e).map_err(|_| "integer overflow in `math.pow`")?;
                Ok(Value::Int(b.checked_pow(e).ok_or("integer overflow in `math.pow` (use floats for large results)")?))
            }
            (b, e) => Ok(Value::Float(num(b, name)?.powf(num(e, name)?))),
        },
        "math.floor" | "math.ceil" | "math.trunc" | "math.round" if matches!(args[0], Value::Int(_)) && args.len() == 1 => {
            Ok(args[0].clone())
        }
        "math.floor" => to_int(x()?.floor(), name),
        "math.ceil" => to_int(x()?.ceil(), name),
        "math.trunc" => to_int(x()?.trunc(), name),
        "math.round" => match args.get(1) {
            None => to_int(x()?.round(), name),
            Some(d) => {
                let scale = 10f64.powi(int(d, name)?.clamp(-300, 300) as i32);
                Ok(Value::Float((x()? * scale).round() / scale))
            }
        },
        "math.sign" => Ok(match &args[0] {
            Value::Int(i) => Value::Int(i.signum()),
            _ => {
                let v = x()?;
                if v.is_nan() {
                    Value::Float(v)
                } else {
                    Value::Int(if v > 0.0 {
                        1
                    } else if v < 0.0 {
                        -1
                    } else {
                        0
                    })
                }
            }
        }),
        "math.clamp" => match (&args[0], &args[1], &args[2]) {
            (Value::Int(v), Value::Int(lo), Value::Int(hi)) if lo <= hi => Ok(Value::Int(*v.max(lo).min(hi))),
            (v, lo, hi) => {
                let (v, lo, hi) = (num(v, name)?, num(lo, name)?, num(hi, name)?);
                if lo > hi || lo.is_nan() || hi.is_nan() {
                    return Err(format!("`math.clamp`: lo ({lo}) is greater than hi ({hi})"));
                }
                Ok(Value::Float(v.clamp(lo, hi)))
            }
        },
        "math.hypot" => Ok(Value::Float(x()?.hypot(num(&args[1], name)?))),
        "math.is_nan" => Ok(Value::Bool(x()?.is_nan())),
        "math.is_finite" => Ok(Value::Bool(x()?.is_finite())),
        // Aliases of the global builtins.
        "math.sqrt" | "math.abs" | "math.min" | "math.max" => {
            let global = builtin_index(&name[5..]).ok_or("missing builtin")?;
            crate::builtins::call(vm, global, args.to_vec())
        }
        _ => Err(format!("builtin `{name}` is not implemented by this runtime")),
    }
}

// ----------------------------------------------------------------- random

/// SplitMix64: small, fast and statistically solid (not for cryptography).
fn next_u64(vm: &mut Vm) -> u64 {
    vm.rng = vm.rng.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = vm.rng;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Uniform in `0..n` (`n > 0`), without modulo bias.
fn below(vm: &mut Vm, n: u64) -> u64 {
    let zone = u64::MAX - u64::MAX % n;
    loop {
        let r = next_u64(vm);
        if r < zone {
            return r % n;
        }
    }
}

fn random(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Value, String> {
    match name {
        "random.seed" => {
            vm.rng = int(&args[0], name)? as u64;
            Ok(Value::Nil)
        }
        "random.int" => {
            let (lo, hi) = (int(&args[0], name)?, int(&args[1], name)?);
            if lo >= hi {
                return Err(format!("`random.int({lo}, {hi})`: lo must be less than hi"));
            }
            let span = hi.wrapping_sub(lo) as u64;
            Ok(Value::Int(lo.wrapping_add(below(vm, span) as i64)))
        }
        "random.float" => Ok(Value::Float((next_u64(vm) >> 11) as f64 / (1u64 << 53) as f64)),
        "random.choice" => match &args[0] {
            Value::List(l) => {
                let len = l.borrow().len();
                if len == 0 {
                    return Err("`random.choice` of an empty list".into());
                }
                let i = below(vm, len as u64) as usize;
                let item = l.borrow()[i].clone();
                Ok(item)
            }
            other => Err(format!("`random.choice` expects a list, found {}", other.type_name())),
        },
        "random.shuffle" => match &args[0] {
            Value::List(l) => {
                let len = l.borrow().len();
                for i in (1..len).rev() {
                    let j = below(vm, i as u64 + 1) as usize;
                    l.borrow_mut().swap(i, j);
                }
                Ok(Value::Nil)
            }
            other => Err(format!("`random.shuffle` expects a list, found {}", other.type_name())),
        },
        _ => Err(format!("builtin `{name}` is not implemented by this runtime")),
    }
}

// --------------------------------------------------------------------- fs

/// File system errors are values (`Err(message)`); wrong argument types are runtime errors.
fn fs(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Value, String> {
    let path = text(&args[0], name)?;
    let done = |vm: &Vm, r: Result<(), String>| match r {
        Ok(()) => vm.ok(Value::Nil),
        Err(e) => vm.err(e),
    };
    Ok(match name {
        "fs.read" => match vm.host.read_file(path) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(s) => vm.ok(Value::str(&s)),
                Err(_) => vm.err(format!("{path}: not UTF-8 text")),
            },
            Err(e) => vm.err(e),
        },
        "fs.write" => {
            let r = vm.host.write_file(path, text(&args[1], name)?.as_bytes(), false);
            done(vm, r)
        }
        "fs.append" => {
            let r = vm.host.write_file(path, text(&args[1], name)?.as_bytes(), true);
            done(vm, r)
        }
        "fs.exists" => Value::Bool(vm.host.file_kind(path).is_some()),
        "fs.is_file" => Value::Bool(vm.host.file_kind(path) == Some(false)),
        "fs.is_dir" => Value::Bool(vm.host.file_kind(path) == Some(true)),
        "fs.list" => match vm.host.list_dir(path) {
            Ok(mut names) => {
                names.sort();
                vm.ok(Value::list(names.iter().map(|n| Value::str(n)).collect()))
            }
            Err(e) => vm.err(e),
        },
        "fs.mkdir" => {
            let r = vm.host.make_dir(path);
            done(vm, r)
        }
        "fs.remove" => {
            let r = vm.host.remove(path);
            done(vm, r)
        }
        "fs.rename" => {
            let r = vm.host.rename(path, text(&args[1], name)?);
            done(vm, r)
        }
        "fs.copy" => {
            let r = vm.host.copy_file(path, text(&args[1], name)?);
            done(vm, r)
        }
        _ => return Err(format!("builtin `{name}` is not implemented by this runtime")),
    })
}

// ------------------------------------------------------------------- path

/// Paths are text with `/` between names; a name ending in `:` (a justOS
/// volume, a Windows drive) or a leading `/` makes a path absolute.
fn is_absolute(p: &str) -> bool {
    p.starts_with('/') || p.starts_with('\\') || p.split(['/', '\\']).next().is_some_and(|first| first.ends_with(':'))
}

/// The last name of a path and what comes before it (trailing separators
/// ignored); `None` for a root or an empty path.
fn split_last(p: &str) -> Option<(&str, &str)> {
    let trimmed = p.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() || trimmed.ends_with(':') {
        return None;
    }
    match trimmed.rfind(['/', '\\']) {
        Some(i) => {
            let parent = &trimmed[..i];
            // Keep the separator of a root: "/a" -> "/", "sys:/a" -> "sys:/".
            let parent = if parent.is_empty() || parent.ends_with(':') { &trimmed[..=i] } else { parent };
            Some((parent, &trimmed[i + 1..]))
        }
        None => Some(("", trimmed)),
    }
}

fn path(name: &str, args: &[Value]) -> Result<Value, String> {
    if name == "path.join" {
        let mut joined = String::new();
        for part in args {
            let part = text(part, name)?;
            if joined.is_empty() || is_absolute(part) {
                joined = part.to_owned();
            } else if !part.is_empty() {
                if !joined.ends_with(['/', '\\']) {
                    joined.push('/');
                }
                joined.push_str(part);
            }
        }
        return Ok(Value::str(&joined));
    }
    let p = text(&args[0], name)?;
    let file = split_last(p).map_or("", |(_, n)| n);
    // ".profile" has no extension.
    let dot = file.rfind('.').filter(|&i| i > 0);
    Ok(Value::str(match name {
        "path.name" => file,
        "path.stem" => dot.map_or(file, |i| &file[..i]),
        "path.ext" => dot.map_or("", |i| &file[i + 1..]),
        "path.parent" => split_last(p).map_or("", |(parent, _)| parent),
        _ => return Err(format!("builtin `{name}` is not implemented by this runtime")),
    }))
}

// --------------------------------------------------------------------- os

fn os(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Value, String> {
    Ok(match name {
        "os.args" => Value::list(vm.args.iter().map(|a| Value::str(a)).collect()),
        "os.env" => match vm.host.env(text(&args[0], name)?) {
            Some(v) => vm.some(Value::str(&v)),
            None => vm.none(),
        },
        "os.cwd" => {
            let cwd = vm.host.cwd();
            Value::str(&cwd)
        }
        "os.exit" => {
            let code = match args.first() {
                Some(c) => int(c, name)?.clamp(i32::MIN as i64, i32::MAX as i64) as i32,
                None => 0,
            };
            vm.exit_code = Some(code);
            return Err(format!("the program exited with code {code}"));
        }
        "os.run" => {
            let program = text(&args[0], name)?;
            let mut list = Vec::new();
            match args.get(1) {
                Some(Value::List(l)) => list.extend(l.borrow().iter().map(Value::to_string)),
                Some(other) => return Err(format!("`os.run` expects a list of arguments, found {}", other.type_name())),
                None => {}
            }
            match vm.host.run(program, &list) {
                Ok(out) => vm.ok(record([
                    ("code", Value::Int(out.code)),
                    ("stdout", Value::str(&out.stdout)),
                    ("stderr", Value::str(&out.stderr)),
                ])),
                Err(e) => vm.err(e),
            }
        }
        "os.notify" => {
            let summary = args[0].to_string();
            let body = args.get(1).map(Value::to_string).unwrap_or_default();
            let (title, id) = (vm.app_title.clone(), vm.app_id.clone());
            match vm.host.notify(&title, &id, &summary, &body) {
                Ok(id) => vm.ok(Value::Int(id.into())),
                Err(e) => vm.err(e),
            }
        }
        "os.name" => Value::str(vm.host.os_name()),
        "os.sep" => Value::str("/"),
        _ => return Err(format!("builtin `{name}` is not implemented by this runtime")),
    })
}

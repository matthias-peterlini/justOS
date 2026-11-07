//! Native functions; indices match `justc_core::bytecode::BUILTINS`.

use crate::prelude::*;
use crate::value::{MapKey, StructObj, Value};
use crate::vm::Vm;
use alloc::rc::Rc;
use justc_core::bytecode::{BUILTINS, NONE, OK, OPTION_TYPE, RESULT_TYPE, SOME};
use core::cell::RefCell;

/// The value inside `Some`/`Ok`, or a description of `None`/`Err(e)`.
fn unwrapped(v: &Value, name: &str) -> Result<Result<Value, String>, String> {
    match v {
        Value::Variant(o) if o.type_index == OPTION_TYPE || o.type_index == RESULT_TYPE => Ok(match o.values.first() {
            Some(inner) if o.tag == 0 => Ok(inner.clone()),
            _ => Err(v.to_string()),
        }),
        other => Err(format!("`{name}` expects an Option or Result, found {}", other.type_name())),
    }
}

fn is_variant(v: &Value, ty: u16, tag: u16, name: &str) -> Result<bool, String> {
    match v {
        Value::Variant(o) if o.type_index == ty => Ok(o.tag == tag),
        other => {
            let want = if ty == OPTION_TYPE { "an Option" } else { "a Result" };
            Err(format!("`{name}` expects {want}, found {}", other.type_name()))
        }
    }
}

/// Call an JustScript function value from a builtin.
pub(crate) fn call_value(vm: &mut Vm, f: &Value, args: Vec<Value>) -> Result<Value, String> {
    vm.call(f.clone(), args).map_err(|e| e.message)
}

fn want_bool(v: Value, name: &str) -> Result<bool, String> {
    match v {
        Value::Bool(b) => Ok(b),
        other => Err(format!("the function given to `{name}` must return a bool, found {}", other.type_name())),
    }
}

fn want_str<'a>(v: &'a Value, name: &str) -> Result<&'a str, String> {
    match v {
        Value::Str(s) => Ok(s),
        other => Err(format!("`{name}` expects a string, found {}", other.type_name())),
    }
}

fn want_int(v: &Value, name: &str) -> Result<i64, String> {
    match v {
        Value::Int(i) => Ok(*i),
        other => Err(format!("`{name}` expects an int, found {}", other.type_name())),
    }
}

/// The items of a list (copied, so callbacks may change the list), or of a range.
fn items(v: &Value, name: &str) -> Result<Vec<Value>, String> {
    match v {
        Value::List(l) => Ok(l.borrow().clone()),
        Value::Range(a, b) => {
            if b.saturating_sub(*a) > 10_000_000 {
                return Err(format!("`{name}`: range too large"));
            }
            Ok((*a..*b).map(Value::Int).collect())
        }
        other => Err(format!("`{name}` expects a list, found {}", other.type_name())),
    }
}

/// `start..end` of a sequence of `len` items, negative positions counting from the end.
fn bounds(len: usize, start: i64, end: Option<i64>) -> (usize, usize) {
    let at = |i: i64| if i < 0 { (len as i64 + i).max(0) as usize } else { (i as usize).min(len) };
    let (s, e) = (at(start), end.map_or(len, at));
    (s, e.max(s))
}

fn pad(text: &str, width: &Value, fill: Option<&Value>, start: bool, name: &str) -> Result<Value, String> {
    let width = want_int(width, name)?.clamp(0, 1 << 20) as usize;
    let fill = match fill {
        Some(f) => want_str(f, name)?,
        None => " ",
    };
    let mut fill = fill.chars();
    let (Some(c), None) = (fill.next(), fill.next()) else { return Err(format!("`{name}`: fill must be one character")) };
    let n = width.saturating_sub(text.chars().count());
    let padding: String = core::iter::repeat_n(c, n).collect();
    Ok(Value::str(&if start { padding + text } else { text.to_owned() + &padding }))
}

/// Sort `list` by `key(item)` (or the items themselves).
fn sort_values(vm: &mut Vm, list: &mut [Value], key: Option<&Value>, name: &str) -> Result<(), String> {
    let keys = match key {
        Some(f) => list.iter().map(|v| call_value(vm, f, vec![v.clone()])).collect::<Result<Vec<_>, _>>()?,
        None => list.to_vec(),
    };
    let mut order: Vec<usize> = (0..list.len()).collect();
    let mut error = None;
    order.sort_by(|&a, &b| {
        keys[a].order(&keys[b]).unwrap_or_else(|e| {
            error.get_or_insert(format!("`{name}`: {e}"));
            core::cmp::Ordering::Equal
        })
    });
    if let Some(e) = error {
        return Err(e);
    }
    let sorted: Vec<Value> = order.into_iter().map(|i| list[i].clone()).collect();
    list.clone_from_slice(&sorted);
    Ok(())
}

/// `min`/`max` of two values or of a list.
fn extreme(args: &[Value], min: bool, name: &str) -> Result<Value, String> {
    let candidates = match args {
        [one] => items(one, name)?,
        _ => args.to_vec(),
    };
    let mut best: Option<Value> = None;
    for v in candidates {
        best = Some(match best {
            None => v,
            Some(b) => {
                let less = v.order(&b).map_err(|e| format!("`{name}`: {e}"))? == core::cmp::Ordering::Less;
                if less == min && !v.equals(&b) {
                    v
                } else {
                    b
                }
            }
        });
    }
    best.ok_or_else(|| format!("`{name}` of an empty list"))
}

pub(crate) fn call(vm: &mut Vm, id: u8, args: Vec<Value>) -> Result<Value, String> {
    let spec = BUILTINS.get(id as usize).ok_or("unknown builtin")?;
    let n = args.len();
    if n < spec.min_args as usize || spec.max_args.is_some_and(|m| n > m as usize) {
        return Err(format!("`{}` called with {n} argument(s)", spec.name));
    }
    let arg = |i: usize| &args[i];
    let num = |v: &Value| match v {
        Value::Int(i) => Ok(*i as f64),
        Value::Float(f) => Ok(*f),
        other => Err(format!("`{}` expects a number, found {}", spec.name, other.type_name())),
    };
    Ok(match spec.name {
        "print" => {
            let text = args.iter().map(ToString::to_string).collect::<Vec<_>>().join(" ");
            vm.host.print(&text);
            Value::Nil
        }
        "len" => Value::Int(match arg(0) {
            Value::Str(s) => s.chars().count() as i64,
            Value::List(l) => l.borrow().len() as i64,
            Value::Range(a, b) => (b - a).max(0),
            Value::Map(m) => m.borrow().len() as i64,
            other => return Err(format!("`len` expects a string, list or map, found {}", other.type_name())),
        }),
        "str" => Value::Str(Rc::from(arg(0).to_string())),
        "int" => Value::Int(match arg(0) {
            Value::Int(i) => *i,
            Value::Float(f) if f.is_finite() && f.abs() < 9.2e18 => f.trunc() as i64,
            Value::Float(f) => return Err(format!("cannot convert {f} to int")),
            Value::Bool(b) => *b as i64,
            Value::Str(s) => s.trim().parse().map_err(|_| format!("cannot parse {:?} as int", &**s))?,
            other => return Err(format!("cannot convert {} to int", other.type_name())),
        }),
        "float" => Value::Float(match arg(0) {
            Value::Str(s) => s.trim().parse().map_err(|_| format!("cannot parse {:?} as float", &**s))?,
            other => num(other)?,
        }),
        "push" => match arg(0) {
            Value::List(l) => {
                l.borrow_mut().push(arg(1).clone());
                Value::Nil
            }
            other => return Err(format!("`push` expects a list, found {}", other.type_name())),
        },
        "pop" => match arg(0) {
            Value::List(l) => l.borrow_mut().pop().ok_or("pop from empty list")?,
            other => return Err(format!("`pop` expects a list, found {}", other.type_name())),
        },
        "type_of" => Value::str(arg(0).type_name()),
        "clock" => Value::Float(vm.host.clock_ms().saturating_sub(vm.started) as f64 / 1000.0),
        "abs" => match arg(0) {
            Value::Int(i) => Value::Int(i.checked_abs().ok_or("integer overflow")?),
            other => Value::Float(num(other)?.abs()),
        },
        "min" | "max" if n == 2 => {
            let (a, b) = (arg(0), arg(1));
            let a_less = match (a, b) {
                (Value::Str(x), Value::Str(y)) => x < y,
                _ => num(a)? < num(b)?,
            };
            if a_less == (spec.name == "min") {
                a.clone()
            } else {
                b.clone()
            }
        }
        "min" | "max" => extreme(&args, spec.name == "min", spec.name)?,
        "sqrt" => Value::Float(num(arg(0))?.sqrt()),
        "join" => match (arg(0), arg(1)) {
            (Value::List(l), Value::Str(sep)) => {
                Value::Str(Rc::from(l.borrow().iter().map(ToString::to_string).collect::<Vec<_>>().join(sep)))
            }
            _ => return Err("`join` expects (list, string)".into()),
        },
        "contains" => Value::Bool(match (arg(0), arg(1)) {
            (Value::Str(h), Value::Str(n)) => h.contains(&**n),
            (Value::List(l), needle) => l.borrow().iter().any(|v| v.equals(needle)),
            (Value::Range(a, b), Value::Int(i)) => a <= i && i < b,
            (Value::Map(m), key) => MapKey::from_value(key).is_ok_and(|k| m.borrow().contains_key(&k)),
            (h, _) => return Err(format!("`contains` is not supported on {}", h.type_name())),
        }),
        "get" => match (arg(0), arg(1)) {
            (Value::Map(m), key) => {
                let found = m.borrow().get(&MapKey::from_value(key)?).cloned();
                found.map_or_else(|| vm.none(), |v| vm.some(v))
            }
            (Value::List(l), Value::Int(i)) => {
                let l = l.borrow();
                let at = if *i < 0 { l.len() as i64 + i } else { *i };
                match usize::try_from(at).ok().and_then(|at| l.get(at)) {
                    Some(v) => vm.some(v.clone()),
                    None => vm.none(),
                }
            }
            (c, _) => return Err(format!("`get` expects a map or a list and an index, found {}", c.type_name())),
        },
        "keys" | "values" => match arg(0) {
            Value::Map(m) => {
                let m = m.borrow();
                let items = if spec.name == "keys" { m.keys().map(MapKey::to_value).collect() } else { m.values().cloned().collect() };
                Value::list(items)
            }
            other => return Err(format!("`{}` expects a map, found {}", spec.name, other.type_name())),
        },
        "remove" => match (arg(0), arg(1)) {
            (Value::Map(m), key) => {
                let removed = m.borrow_mut().remove(&MapKey::from_value(key)?);
                removed.map_or_else(|| vm.none(), |v| vm.some(v))
            }
            (Value::List(l), Value::Int(i)) => {
                let mut l = l.borrow_mut();
                let at = if *i < 0 { l.len() as i64 + i } else { *i };
                match usize::try_from(at).ok().filter(|&at| at < l.len()) {
                    Some(at) => vm.some(l.remove(at)),
                    None => vm.none(),
                }
            }
            (c, _) => return Err(format!("`remove` expects a map or a list and an index, found {}", c.type_name())),
        },
        "insert" => match (arg(0), arg(1)) {
            (Value::List(l), Value::Int(i)) => {
                let mut l = l.borrow_mut();
                let at = if *i < 0 { l.len() as i64 + i } else { *i };
                match usize::try_from(at).ok().filter(|&at| at <= l.len()) {
                    Some(at) => l.insert(at, arg(2).clone()),
                    None => return Err(format!("insert index {i} out of bounds (length {})", l.len())),
                }
                Value::Nil
            }
            (c, _) => return Err(format!("`insert` expects a list and an index, found {}", c.type_name())),
        },
        "unwrap" => unwrapped(arg(0), "unwrap")?.map_err(|e| format!("called `unwrap` on {e}"))?,
        "expect" => unwrapped(arg(0), "expect")?.map_err(|e| format!("{}: {e}", arg(1)))?,
        "unwrap_or" => unwrapped(arg(0), "unwrap_or")?.unwrap_or_else(|_| arg(1).clone()),
        "ok_or" => match arg(0) {
            Value::Variant(o) if o.type_index == OPTION_TYPE => match o.values.first() {
                Some(v) if o.tag == SOME => vm.ok(v.clone()),
                _ => vm.err_value(arg(1).clone()),
            },
            other => return Err(format!("`ok_or` expects an Option, found {}", other.type_name())),
        },
        "is_some" => Value::Bool(!is_variant(arg(0), OPTION_TYPE, NONE, "is_some")?),
        "is_none" => Value::Bool(is_variant(arg(0), OPTION_TYPE, NONE, "is_none")?),
        "is_ok" => Value::Bool(is_variant(arg(0), RESULT_TYPE, OK, "is_ok")?),
        "is_err" => Value::Bool(!is_variant(arg(0), RESULT_TYPE, OK, "is_err")?),
        "panic" => return Err(arg(0).to_string()),
        "assert" => match arg(0) {
            Value::Bool(true) => Value::Nil,
            Value::Bool(false) => {
                return Err(args.get(1).map_or_else(|| "assertion failed".to_owned(), |m| format!("assertion failed: {m}")))
            }
            other => return Err(format!("`assert` expects a bool, found {}", other.type_name())),
        },
        "copy" => match arg(0) {
            Value::List(l) => Value::list(l.borrow().clone()),
            Value::Map(m) => Value::map(m.borrow().clone()),
            Value::Struct(s) => Value::Struct(Rc::new(StructObj {
                ty: Rc::clone(&s.ty),
                type_index: s.type_index,
                fields: RefCell::new(s.fields.borrow().clone()),
            })),
            other => other.clone(),
        },

        // ---------------------------------------------------------- strings
        "split" => {
            let text = want_str(arg(0), "split")?;
            let parts: Vec<Value> = match args.get(1) {
                None => text.split_whitespace().map(Value::str).collect(),
                Some(sep) => match want_str(sep, "split")? {
                    "" => return Err("`split`: the separator is empty (use `chars` to split into characters)".into()),
                    sep => text.split(sep).map(Value::str).collect(),
                },
            };
            Value::list(parts)
        }
        "trim" => Value::str(want_str(arg(0), "trim")?.trim()),
        "trim_start" => Value::str(want_str(arg(0), "trim_start")?.trim_start()),
        "trim_end" => Value::str(want_str(arg(0), "trim_end")?.trim_end()),
        "upper" => Value::str(&want_str(arg(0), "upper")?.to_uppercase()),
        "lower" => Value::str(&want_str(arg(0), "lower")?.to_lowercase()),
        "replace" => {
            let (text, from, to) = (want_str(arg(0), "replace")?, want_str(arg(1), "replace")?, want_str(arg(2), "replace")?);
            if from.is_empty() {
                return Err("`replace`: the text to replace is empty".into());
            }
            Value::str(&text.replace(from, to))
        }
        "starts_with" => Value::Bool(want_str(arg(0), "starts_with")?.starts_with(want_str(arg(1), "starts_with")?)),
        "ends_with" => Value::Bool(want_str(arg(0), "ends_with")?.ends_with(want_str(arg(1), "ends_with")?)),
        "index_of" => {
            let found = match (arg(0), arg(1)) {
                (Value::Str(h), Value::Str(needle)) => h.find(&**needle).map(|at| h[..at].chars().count()),
                (Value::Str(_), other) => return Err(format!("`index_of` on a string expects a string, found {}", other.type_name())),
                (Value::List(l), needle) => l.borrow().iter().position(|v| v.equals(needle)),
                (other, _) => return Err(format!("`index_of` expects a string or list, found {}", other.type_name())),
            };
            match found {
                Some(i) => vm.some(Value::Int(i as i64)),
                None => vm.none(),
            }
        }
        "chars" => Value::list(want_str(arg(0), "chars")?.chars().map(|c| Value::str(c.encode_utf8(&mut [0; 4]))).collect()),
        "repeat" => {
            let times = want_int(arg(1), "repeat")?.max(0) as usize;
            match arg(0) {
                Value::Str(s) => {
                    if s.len().saturating_mul(times) > 1 << 28 {
                        return Err("`repeat`: result too large".into());
                    }
                    Value::str(&s.repeat(times))
                }
                Value::List(l) => {
                    let l = l.borrow();
                    if l.len().saturating_mul(times) > 1 << 24 {
                        return Err("`repeat`: result too large".into());
                    }
                    Value::list(l.iter().cloned().cycle().take(l.len() * times).collect())
                }
                other => return Err(format!("`repeat` expects a string or list, found {}", other.type_name())),
            }
        }
        "slice" => {
            let start = want_int(arg(1), "slice")?;
            let end = args.get(2).map(|e| want_int(e, "slice")).transpose()?;
            match arg(0) {
                Value::Str(s) => {
                    let (a, b) = bounds(s.chars().count(), start, end);
                    Value::str(&s.chars().skip(a).take(b - a).collect::<String>())
                }
                Value::List(l) => {
                    let l = l.borrow();
                    let (a, b) = bounds(l.len(), start, end);
                    Value::list(l[a..b].to_vec())
                }
                other => return Err(format!("`slice` expects a string or list, found {}", other.type_name())),
            }
        }
        "lines" => Value::list(want_str(arg(0), "lines")?.lines().map(Value::str).collect()),
        "pad_start" | "pad_end" => pad(want_str(arg(0), spec.name)?, arg(1), args.get(2), spec.name == "pad_start", spec.name)?,
        "ord" => {
            let mut chars = want_str(arg(0), "ord")?.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Value::Int(c as i64),
                _ => return Err("`ord` expects a string of exactly one character".into()),
            }
        }
        "chr" => {
            let code = want_int(arg(0), "chr")?;
            let c = u32::try_from(code).ok().and_then(char::from_u32).ok_or_else(|| format!("`chr`: {code} is not a Unicode character"))?;
            Value::str(c.encode_utf8(&mut [0; 4]))
        }

        // ------------------------------------------------------------ lists
        "map" => {
            let mut out = Vec::new();
            for v in items(arg(0), "map")? {
                out.push(call_value(vm, arg(1), vec![v])?);
            }
            Value::list(out)
        }
        "filter" => {
            let mut out = Vec::new();
            for v in items(arg(0), "filter")? {
                if want_bool(call_value(vm, arg(1), vec![v.clone()])?, "filter")? {
                    out.push(v);
                }
            }
            Value::list(out)
        }
        "reduce" => {
            let mut acc = arg(1).clone();
            for v in items(arg(0), "reduce")? {
                acc = call_value(vm, arg(2), vec![acc, v])?;
            }
            acc
        }
        "sort" => match arg(0) {
            Value::List(l) => {
                let mut sorted = l.borrow().clone();
                sort_values(vm, &mut sorted, args.get(1), "sort")?;
                *l.borrow_mut() = sorted;
                Value::Nil
            }
            other => return Err(format!("`sort` expects a list, found {}", other.type_name())),
        },
        "sorted" => {
            let mut sorted = items(arg(0), "sorted")?;
            sort_values(vm, &mut sorted, args.get(1), "sorted")?;
            Value::list(sorted)
        }
        "reverse" => match arg(0) {
            Value::List(l) => {
                l.borrow_mut().reverse();
                Value::Nil
            }
            other => return Err(format!("`reverse` expects a list, found {}", other.type_name())),
        },
        "reversed" => match arg(0) {
            Value::Str(s) => Value::str(&s.chars().rev().collect::<String>()),
            other => {
                let mut l = items(other, "reversed")?;
                l.reverse();
                Value::list(l)
            }
        },
        "sum" => {
            let mut total = Value::Int(0);
            for v in items(arg(0), "sum")? {
                total = match (&total, &v) {
                    (Value::Int(a), Value::Int(b)) => Value::Int(a.checked_add(*b).ok_or("integer overflow in `sum`")?),
                    (a, b) => Value::Float(num(a)? + num(b)?),
                };
            }
            total
        }
        "any" | "all" => {
            let all = spec.name == "all";
            let mut result = all;
            for v in items(arg(0), spec.name)? {
                let b = match args.get(1) {
                    Some(f) => want_bool(call_value(vm, f, vec![v])?, spec.name)?,
                    None => match v {
                        Value::Bool(b) => b,
                        other => return Err(format!("`{}` expects a list of bools, found {}", spec.name, other.type_name())),
                    },
                };
                if b != all {
                    result = !all;
                    break;
                }
            }
            Value::Bool(result)
        }
        "first" => {
            let mut found = None;
            for v in items(arg(0), "first")? {
                let hit = match args.get(1) {
                    Some(f) => want_bool(call_value(vm, f, vec![v.clone()])?, "first")?,
                    None => true,
                };
                if hit {
                    found = Some(v);
                    break;
                }
            }
            found.map_or_else(|| vm.none(), |v| vm.some(v))
        }
        "last" => items(arg(0), "last")?.pop().map_or_else(|| vm.none(), |v| vm.some(v)),
        "range" => {
            let (start, end) = (want_int(arg(0), "range")?, want_int(arg(1), "range")?);
            let step = args.get(2).map(|s| want_int(s, "range")).transpose()?.unwrap_or(1);
            if step == 0 {
                return Err("`range`: step must not be 0".into());
            }
            let count = if step > 0 { (end - start).max(0) / step } else { (start - end).max(0) / -step };
            if count > 10_000_000 {
                return Err("`range`: too many items".into());
            }
            let mut out = Vec::new();
            let mut i = start;
            while (step > 0 && i < end) || (step < 0 && i > end) {
                out.push(Value::Int(i));
                i += step;
            }
            Value::list(out)
        }
        "zip" => {
            let (a, b) = (items(arg(0), "zip")?, items(arg(1), "zip")?);
            Value::list(a.into_iter().zip(b).map(|(x, y)| Value::list(vec![x, y])).collect())
        }
        "items" => match arg(0) {
            Value::Map(m) => Value::list(m.borrow().iter().map(|(k, v)| Value::list(vec![k.to_value(), v.clone()])).collect()),
            other => return Err(format!("`items` expects a map, found {}", other.type_name())),
        },
        "extend" => match arg(0) {
            Value::List(l) => {
                let more = items(arg(1), "extend")?;
                l.borrow_mut().extend(more);
                Value::Nil
            }
            other => return Err(format!("`extend` expects a list, found {}", other.type_name())),
        },
        "clear" => match arg(0) {
            Value::List(l) => {
                l.borrow_mut().clear();
                Value::Nil
            }
            Value::Map(m) => {
                m.borrow_mut().clear();
                Value::Nil
            }
            other => return Err(format!("`clear` expects a list or map, found {}", other.type_name())),
        },

        // ---------------------------------------------------- input, parsing
        "read_line" => {
            let prompt = args.first().map(ToString::to_string);
            match vm.host.read_line(prompt.as_deref()) {
                Some(line) => vm.some(Value::str(&line)),
                None => vm.none(),
            }
        }
        "parse_int" => match want_str(arg(0), "parse_int")?.trim().parse::<i64>() {
            Ok(i) => vm.some(Value::Int(i)),
            Err(_) => vm.none(),
        },
        "parse_float" => match want_str(arg(0), "parse_float")?.trim().parse::<f64>() {
            Ok(f) => vm.some(Value::Float(f)),
            Err(_) => vm.none(),
        },
        name if name.contains('.') => return crate::stdlib::call(vm, name, args),
        other => return Err(format!("builtin `{other}` is not implemented by this runtime")),
    })
}

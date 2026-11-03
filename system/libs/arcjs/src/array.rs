//! `Array` and its methods.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::builtins::{ctor, func, make_iter, num_arg};
use crate::interp::*;
use crate::value::*;

/// An array, or anything with a `length`, as a list.
pub fn list_of(it: &mut Interp, v: &Value) -> R<Vec<Value>> {
    if let Value::Obj(o) = v {
        if let Kind::Array(a) = &o.borrow().kind {
            return Ok(a.clone());
        }
    }
    if let Value::Str(s) = v {
        return Ok(s.chars().map(|c| Value::str(c.encode_utf8(&mut [0; 4]))).collect());
    }
    if !matches!(v, Value::Obj(_)) {
        return Ok(Vec::new());
    }
    let n = it.get(v, &Key::str("length"))?;
    let n = to_int(it.to_number(&n)?).clamp(0.0, 1e7) as usize;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        out.push(it.get_v(v, &Value::Num(i as f64))?);
    }
    Ok(out)
}

fn len(it: &mut Interp, this: &Value) -> R<usize> {
    if let Value::Obj(o) = this {
        if let Kind::Array(a) = &o.borrow().kind {
            return Ok(a.len());
        }
    }
    if this.nullish() {
        return Err(it.type_error("Array.prototype method called on null or undefined"));
    }
    let n = it.get(this, &Key::str("length"))?;
    Ok(to_int(it.to_number(&n)?).clamp(0.0, 1e7) as usize)
}

fn at(it: &mut Interp, this: &Value, i: usize) -> R<Value> {
    it.get_v(this, &Value::Num(i as f64))
}

fn with_arr<T>(it: &mut Interp, this: &Value, f: impl FnOnce(&mut Vec<Value>) -> T) -> R<T> {
    if let Value::Obj(o) = this {
        let mut b = o.borrow_mut();
        if b.frozen {
            drop(b);
            return Err(it.type_error("Cannot modify a frozen array"));
        }
        if let Kind::Array(a) = &mut b.kind {
            return Ok(f(a));
        }
    }
    Err(it.type_error("not an array"))
}

fn callback(it: &mut Interp, args: &[Value]) -> R<Value> {
    let f = arg(args, 0);
    if !f.callable() {
        let d = it.display(&f);
        return Err(it.type_error(&alloc::format!("{d} is not a function")));
    }
    Ok(f)
}

/// Runs `f(item, index, array)` over the array until `stop` says so;
/// returns the index it stopped at.
fn scan(it: &mut Interp, this: &Value, args: &[Value], rev: bool, stop: impl Fn(&Value) -> bool) -> R<Option<(usize, Value)>> {
    let f = callback(it, args)?;
    let t = arg(args, 1);
    let n = len(it, this)?;
    for k in 0..n {
        let i = if rev { n - 1 - k } else { k };
        let v = at(it, this, i)?;
        let r = it.call(&f, t.clone(), &[v.clone(), Value::Num(i as f64), this.clone()])?;
        if stop(&r) {
            return Ok(Some((i, v)));
        }
    }
    Ok(None)
}

fn compare(it: &mut Interp, a: &Value, b: &Value, cmp: &Value) -> R<bool> {
    if cmp.callable() {
        let r = it.call(cmp, Value::Undef, &[a.clone(), b.clone()])?;
        let r = it.to_number(&r)?;
        // NaN counts as "in order", like a stable sort with a broken comparator.
        return Ok(r.partial_cmp(&0.0) != Some(core::cmp::Ordering::Greater));
    }
    let x = it.to_str(a)?;
    let y = it.to_str(b)?;
    Ok(x <= y)
}

/// A stable merge sort that can call back into JavaScript.
pub fn sort(it: &mut Interp, v: Vec<Value>, cmp: &Value) -> R<Vec<Value>> {
    let (mut items, undefs): (Vec<Value>, Vec<Value>) = v.into_iter().partition(|x| !matches!(x, Value::Undef));
    let n = items.len();
    let mut buf = items.clone();
    let mut width = 1;
    while width < n {
        let mut lo = 0;
        while lo < n {
            let mid = (lo + width).min(n);
            let hi = (lo + 2 * width).min(n);
            let (mut i, mut j, mut k) = (lo, mid, lo);
            while i < mid && j < hi {
                if compare(it, &items[i], &items[j], cmp)? {
                    buf[k] = items[i].clone();
                    i += 1;
                } else {
                    buf[k] = items[j].clone();
                    j += 1;
                }
                k += 1;
            }
            while i < mid {
                buf[k] = items[i].clone();
                i += 1;
                k += 1;
            }
            while j < hi {
                buf[k] = items[j].clone();
                j += 1;
                k += 1;
            }
            lo = hi;
        }
        core::mem::swap(&mut items, &mut buf);
        width *= 2;
    }
    items.extend(undefs);
    Ok(items)
}

fn join(it: &mut Interp, this: &Value, sep: &str) -> R<Value> {
    let items = list_of(it, this)?;
    let mut s = String::new();
    for (i, v) in items.iter().enumerate() {
        if i > 0 {
            s.push_str(sep);
        }
        if !v.nullish() {
            s.push_str(&it.to_str(v)?);
        }
    }
    Ok(Value::Str(Rc::from(s.as_str())))
}

fn flat_into(it: &mut Interp, out: &mut Vec<Value>, items: Vec<Value>, depth: f64) -> R<()> {
    for v in items {
        let is_arr = matches!(&v, Value::Obj(o) if matches!(o.borrow().kind, Kind::Array(_)));
        if is_arr && depth >= 1.0 {
            let inner = list_of(it, &v)?;
            flat_into(it, out, inner, depth - 1.0)?;
        } else {
            out.push(v);
        }
    }
    Ok(())
}

fn range(it: &mut Interp, args: &[Value], from: usize, n: usize) -> R<(usize, usize)> {
    let s = arg(args, from);
    let e = arg(args, from + 1);
    let start = if matches!(s, Value::Undef) { 0 } else { rel_index(it.to_number(&s)?, n) };
    let end = if matches!(e, Value::Undef) { n } else { rel_index(it.to_number(&e)?, n) };
    Ok((start, end.max(start)))
}

pub fn install(it: &mut Interp) {
    let proto = it.p.array.clone();
    let c = ctor(it, "Array", 1, |it, _, args, _| {
        let items = if let [Value::Num(n)] = args {
            if *n < 0.0 || *n != to_int(*n) || *n > 1e8 {
                return Err(it.range_error("Invalid array length"));
            }
            vec![Value::Undef; *n as usize]
        } else {
            args.to_vec()
        };
        let a = it.new_array(items);
        let nt = it.new_target.clone();
        if !nt.nullish() {
            if let (Value::Obj(p), Value::Obj(o)) = (it.get(&nt, &Key::str("prototype"))?, &a) {
                o.borrow_mut().proto = Some(p);
            }
        }
        Ok(a)
    }, &proto);
    func(it, &c, "isArray", 1, |_, _, args, _| Ok(Value::Bool(matches!(arg(args, 0), Value::Obj(o) if matches!(o.borrow().kind, Kind::Array(_))))));
    func(it, &c, "of", 0, |it, _, args, _| Ok(it.new_array(args.to_vec())));
    func(it, &c, "from", 1, |it, _, args, _| {
        let src = arg(args, 0);
        if src.nullish() {
            return Err(it.type_error("Array.from requires an array-like object"));
        }
        let iterable = matches!(src, Value::Str(_)) || matches!(&src, Value::Obj(o) if !matches!(o.borrow().kind, Kind::Plain | Kind::Host(..)));
        let has_iter = !iterable && matches!(src, Value::Obj(_)) && it.get(&src, &Key::Sym(SYM_ITERATOR))?.callable();
        let mut items = if iterable || has_iter { it.iterate(&src)? } else { list_of(it, &src)? };
        let f = arg(args, 1);
        if f.callable() {
            for (i, v) in items.iter_mut().enumerate() {
                *v = it.call(&f, arg(args, 2), &[v.clone(), Value::Num(i as f64)])?;
            }
        }
        Ok(it.new_array(items))
    });

    func(it, &proto, "push", 1, |it, this, args, _| Ok(Value::Num(with_arr(it, this, |a| {
        a.extend_from_slice(args);
        a.len()
    })? as f64)));
    func(it, &proto, "pop", 0, |it, this, _, _| Ok(with_arr(it, this, Vec::pop)?.unwrap_or_default()));
    func(it, &proto, "shift", 0, |it, this, _, _| with_arr(it, this, |a| if a.is_empty() { Value::Undef } else { a.remove(0) }));
    func(it, &proto, "unshift", 1, |it, this, args, _| Ok(Value::Num(with_arr(it, this, |a| {
        a.splice(0..0, args.iter().cloned());
        a.len()
    })? as f64)));
    func(it, &proto, "slice", 2, |it, this, args, _| {
        let items = list_of(it, this)?;
        let (s, e) = range(it, args, 0, items.len())?;
        Ok(it.new_array(items[s..e].to_vec()))
    });
    func(it, &proto, "splice", 2, |it, this, args, _| {
        let n = len(it, this)?;
        let start = rel_index(num_arg(it, args, 0)?, n);
        let count = match args.len() {
            0 => 0,
            1 => n - start,
            _ => to_int(num_arg(it, args, 1)?).clamp(0.0, (n - start) as f64) as usize,
        };
        let insert = args.get(2..).unwrap_or(&[]).to_vec();
        let removed = with_arr(it, this, |a| a.splice(start..start + count, insert).collect::<Vec<_>>())?;
        Ok(it.new_array(removed))
    });
    func(it, &proto, "concat", 1, |it, this, args, _| {
        let mut out = list_of(it, this)?;
        for a in args {
            let spread = matches!(a, Value::Obj(o) if matches!(o.borrow().kind, Kind::Array(_)));
            if spread {
                out.extend(list_of(it, a)?);
            } else {
                out.push(a.clone());
            }
        }
        Ok(it.new_array(out))
    });
    func(it, &proto, "join", 1, |it, this, args, _| {
        let sep = match arg(args, 0) {
            Value::Undef => Rc::from(","),
            v => it.to_str(&v)?,
        };
        join(it, this, &sep)
    });
    func(it, &proto, "toString", 0, |it, this, _, _| join(it, this, ","));
    func(it, &proto, "toLocaleString", 0, |it, this, _, _| join(it, this, ","));
    func(it, &proto, "reverse", 0, |it, this, _, _| {
        with_arr(it, this, |a| a.reverse())?;
        Ok(this.clone())
    });
    func(it, &proto, "toReversed", 0, |it, this, _, _| {
        let mut v = list_of(it, this)?;
        v.reverse();
        Ok(it.new_array(v))
    });
    func(it, &proto, "indexOf", 1, |it, this, args, _| {
        let items = list_of(it, this)?;
        let x = arg(args, 0);
        let from = rel_index(num_arg(it, args, 1)?, items.len());
        Ok(Value::Num(items.iter().skip(from).position(|v| v.strict_eq(&x)).map_or(-1.0, |i| (i + from) as f64)))
    });
    func(it, &proto, "lastIndexOf", 1, |it, this, args, _| {
        let items = list_of(it, this)?;
        let x = arg(args, 0);
        Ok(Value::Num(items.iter().rposition(|v| v.strict_eq(&x)).map_or(-1.0, |i| i as f64)))
    });
    func(it, &proto, "includes", 1, |it, this, args, _| {
        let items = list_of(it, this)?;
        let x = arg(args, 0);
        let from = rel_index(num_arg(it, args, 1)?, items.len());
        Ok(Value::Bool(items.iter().skip(from).any(|v| v.same_zero(&x))))
    });
    func(it, &proto, "find", 1, |it, this, args, _| Ok(scan(it, this, args, false, Value::truthy)?.map(|x| x.1).unwrap_or_default()));
    func(it, &proto, "findLast", 1, |it, this, args, _| Ok(scan(it, this, args, true, Value::truthy)?.map(|x| x.1).unwrap_or_default()));
    func(it, &proto, "findIndex", 1, |it, this, args, _| Ok(Value::Num(scan(it, this, args, false, Value::truthy)?.map_or(-1.0, |x| x.0 as f64))));
    func(it, &proto, "findLastIndex", 1, |it, this, args, _| Ok(Value::Num(scan(it, this, args, true, Value::truthy)?.map_or(-1.0, |x| x.0 as f64))));
    func(it, &proto, "some", 1, |it, this, args, _| Ok(Value::Bool(scan(it, this, args, false, Value::truthy)?.is_some())));
    func(it, &proto, "every", 1, |it, this, args, _| Ok(Value::Bool(scan(it, this, args, false, |v| !v.truthy())?.is_none())));
    func(it, &proto, "forEach", 1, |it, this, args, _| {
        scan(it, this, args, false, |_| false)?;
        Ok(Value::Undef)
    });
    func(it, &proto, "map", 1, |it, this, args, _| {
        let f = callback(it, args)?;
        let n = len(it, this)?;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let v = at(it, this, i)?;
            out.push(it.call(&f, arg(args, 1), &[v, Value::Num(i as f64), this.clone()])?);
        }
        Ok(it.new_array(out))
    });
    func(it, &proto, "filter", 1, |it, this, args, _| {
        let f = callback(it, args)?;
        let n = len(it, this)?;
        let mut out = Vec::new();
        for i in 0..n {
            let v = at(it, this, i)?;
            if it.call(&f, arg(args, 1), &[v.clone(), Value::Num(i as f64), this.clone()])?.truthy() {
                out.push(v);
            }
        }
        Ok(it.new_array(out))
    });
    fn reduce(it: &mut Interp, this: &Value, args: &[Value], rev: bool) -> R<Value> {
        let f = callback(it, args)?;
        let n = len(it, this)?;
        let mut order: Vec<usize> = (0..n).collect();
        if rev {
            order.reverse();
        }
        let mut iter = order.into_iter();
        let mut acc = if args.len() >= 2 {
            arg(args, 1)
        } else {
            match iter.next() {
                Some(i) => at(it, this, i)?,
                None => return Err(it.type_error("Reduce of empty array with no initial value")),
            }
        };
        for i in iter {
            let v = at(it, this, i)?;
            acc = it.call(&f, Value::Undef, &[acc, v, Value::Num(i as f64), this.clone()])?;
        }
        Ok(acc)
    }
    func(it, &proto, "reduce", 1, |it, this, args, _| reduce(it, this, args, false));
    func(it, &proto, "reduceRight", 1, |it, this, args, _| reduce(it, this, args, true));
    func(it, &proto, "sort", 1, |it, this, args, _| {
        let items = list_of(it, this)?;
        let sorted = sort(it, items, &arg(args, 0))?;
        with_arr(it, this, |a| *a = sorted)?;
        Ok(this.clone())
    });
    func(it, &proto, "toSorted", 1, |it, this, args, _| {
        let items = list_of(it, this)?;
        let sorted = sort(it, items, &arg(args, 0))?;
        Ok(it.new_array(sorted))
    });
    func(it, &proto, "fill", 1, |it, this, args, _| {
        let n = len(it, this)?;
        let (s, e) = range(it, args, 1, n)?;
        let v = arg(args, 0);
        with_arr(it, this, |a| a[s..e].iter_mut().for_each(|x| *x = v.clone()))?;
        Ok(this.clone())
    });
    func(it, &proto, "flat", 0, |it, this, args, _| {
        let d = arg(args, 0);
        let depth = if matches!(d, Value::Undef) { 1.0 } else { it.to_number(&d)? };
        let items = list_of(it, this)?;
        let mut out = Vec::new();
        flat_into(it, &mut out, items, depth)?;
        Ok(it.new_array(out))
    });
    func(it, &proto, "flatMap", 1, |it, this, args, _| {
        let f = callback(it, args)?;
        let n = len(it, this)?;
        let mut mapped = Vec::with_capacity(n);
        for i in 0..n {
            let v = at(it, this, i)?;
            mapped.push(it.call(&f, arg(args, 1), &[v, Value::Num(i as f64), this.clone()])?);
        }
        let mut out = Vec::new();
        flat_into(it, &mut out, mapped, 1.0)?;
        Ok(it.new_array(out))
    });
    func(it, &proto, "at", 1, |it, this, args, _| {
        let n = len(it, this)? as f64;
        let i = to_int(num_arg(it, args, 0)?);
        let i = if i < 0.0 { n + i } else { i };
        if i < 0.0 || i >= n {
            return Ok(Value::Undef);
        }
        at(it, this, i as usize)
    });
    func(it, &proto, "with", 2, |it, this, args, _| {
        let mut items = list_of(it, this)?;
        let n = items.len() as f64;
        let i = to_int(num_arg(it, args, 0)?);
        let i = if i < 0.0 { n + i } else { i };
        if i < 0.0 || i >= n {
            return Err(it.range_error("Invalid index"));
        }
        items[i as usize] = arg(args, 1);
        Ok(it.new_array(items))
    });
    func(it, &proto, "keys", 0, |it, this, _, _| {
        let n = len(it, this)?;
        let keys = (0..n).map(|i| Value::Num(i as f64)).collect();
        Ok(make_iter(it, keys))
    });
    func(it, &proto, "entries", 0, |it, this, _, _| {
        let items = list_of(it, this)?;
        let pairs = items.into_iter().enumerate().map(|(i, v)| it.new_array(vec![Value::Num(i as f64), v])).collect();
        Ok(make_iter(it, pairs))
    });
    let values = it.native("values", 0, |it, this, _, _| {
        let items = list_of(it, this)?;
        Ok(make_iter(it, items))
    }, Value::Undef, false);
    proto.borrow_mut().props.insert(Key::str("values"), Prop::hidden(Value::Obj(values.clone())));
    proto.borrow_mut().props.insert(Key::Sym(SYM_ITERATOR), Prop::hidden(Value::Obj(values)));
}

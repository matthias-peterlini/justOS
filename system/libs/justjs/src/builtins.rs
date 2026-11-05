//! The global object: Object, Function, Error, Symbol, Boolean, Number,
//! Math, Promise, Map, Set, console, timers and the global functions.
//! Arrays, strings, JSON and dates have their own modules.

use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt::Write;

use arcc_core::float as fm;

use crate::interp::*;
use crate::value::*;

type N = Value;

pub fn install(it: &mut Interp) {
    let g = it.global.clone();
    let gv = Value::Obj(g.clone());
    it.set_hidden(&g, "globalThis", gv);
    g.borrow_mut().props.insert(Key::str("undefined"), Prop { slot: Slot::Data(Value::Undef), enumerable: false, writable: false });
    g.borrow_mut().props.insert(Key::str("NaN"), Prop { slot: Slot::Data(Value::Num(f64::NAN)), enumerable: false, writable: false });
    g.borrow_mut().props.insert(Key::str("Infinity"), Prop { slot: Slot::Data(Value::Num(f64::INFINITY)), enumerable: false, writable: false });
    object(it);
    function(it);
    errors(it);
    symbol(it);
    boolean(it);
    number(it);
    math(it);
    globals(it);
    console(it);
    timers(it);
    promise(it);
    collections(it);
    iterators(it);
    crate::array::install(it);
    crate::string::install(it);
    crate::json::install(it);
    crate::date::install(it);
}

/// A constructor on the global object, tied to its prototype.
pub fn ctor(it: &mut Interp, name: &str, arity: u32, f: NativeFn, proto: &ObjRef) -> ObjRef {
    let c = it.native(name, arity, f, Value::Undef, true);
    c.borrow_mut().props.insert(Key::str("prototype"), Prop { slot: Slot::Data(Value::Obj(proto.clone())), enumerable: false, writable: false });
    proto.borrow_mut().props.insert(Key::str("constructor"), Prop::hidden(Value::Obj(c.clone())));
    it.set_global(name, Value::Obj(c.clone()));
    c
}

pub fn func(it: &mut Interp, o: &ObjRef, name: &str, arity: u32, f: NativeFn) {
    it.method(o, name, arity, f);
}

fn undef(_: &mut Interp, _: &N, _: &[N], _: &N) -> R<N> {
    Ok(Value::Undef)
}

pub fn num_arg(it: &mut Interp, args: &[Value], i: usize) -> R<f64> {
    let v = arg(args, i);
    it.to_number(&v)
}

pub fn str_arg(it: &mut Interp, args: &[Value], i: usize) -> R<Rc<str>> {
    let v = arg(args, i);
    it.to_str(&v)
}

// ---------------------------------------------------------------------------
// Object

fn has_own(it: &mut Interp, v: &Value, key: &Key) -> R<bool> {
    let o = it.to_object(v)?;
    let b = o.borrow();
    Ok(match &b.kind {
        Kind::Array(a) if key.index().is_some_and(|i| i < a.len()) || key.is("length") => true,
        Kind::Prim(Value::Str(s)) if key.index().is_some_and(|i| i < crate::string::len(s)) => true,
        k => b.props.find(key).is_some() || (matches!(k, Kind::Func(_) | Kind::Native(_)) && (key.is("name") || key.is("length"))),
    })
}

fn object(it: &mut Interp) {
    let proto = it.p.object.clone();
    let c = ctor(it, "Object", 1, object_ctor, &proto);
    func(it, &proto, "hasOwnProperty", 1, |it, this, args, _| {
        let k = it.to_key(&arg(args, 0))?;
        Ok(Value::Bool(has_own(it, this, &k)?))
    });
    func(it, &proto, "isPrototypeOf", 1, |_, this, args, _| {
        let (Value::Obj(p), Value::Obj(o)) = (this, &arg(args, 0)) else { return Ok(Value::Bool(false)) };
        let mut cur = o.borrow().proto.clone();
        while let Some(x) = cur {
            if x.same(p) {
                return Ok(Value::Bool(true));
            }
            cur = x.borrow().proto.clone();
        }
        Ok(Value::Bool(false))
    });
    func(it, &proto, "propertyIsEnumerable", 1, |it, this, args, _| {
        let k = it.to_key(&arg(args, 0))?;
        Ok(Value::Bool(it.own_keys(this, false).contains(&k)))
    });
    func(it, &proto, "toString", 0, object_to_string);
    func(it, &proto, "toLocaleString", 0, |it, this, _, _| {
        let f = it.get(this, &Key::str("toString"))?;
        it.call(&f, this.clone(), &[])
    });
    func(it, &proto, "valueOf", 0, |it, this, _, _| Ok(Value::Obj(it.to_object(this)?)));
    it.accessor(
        &proto,
        "__proto__",
        |it, this, _, _| {
            Ok(match this {
                Value::Obj(o) => o.borrow().proto.clone().map(Value::Obj).unwrap_or(Value::Null),
                v => Value::Obj(it.to_object(v)?.borrow().proto.clone().expect("primitive prototype")),
            })
        },
        Some(|_, this, args, _| {
            if let Value::Obj(o) = this {
                match arg(args, 0) {
                    Value::Obj(p) => o.borrow_mut().proto = Some(p),
                    Value::Null => o.borrow_mut().proto = None,
                    _ => {}
                }
            }
            Ok(Value::Undef)
        }),
        Value::Undef,
    );
    func(it, &c, "keys", 1, |it, _, args, _| {
        let v = arg(args, 0);
        if v.nullish() {
            return Err(it.type_error("Cannot convert undefined or null to object"));
        }
        let keys = it.own_keys(&v, false).into_iter().filter_map(|k| if let Key::Str(s) = k { Some(Value::Str(s)) } else { None }).collect();
        Ok(it.new_array(keys))
    });
    func(it, &c, "values", 1, |it, _, args, _| {
        let v = arg(args, 0);
        let mut out = Vec::new();
        for k in it.own_keys(&v, false) {
            out.push(it.get(&v, &k)?);
        }
        Ok(it.new_array(out))
    });
    func(it, &c, "entries", 1, |it, _, args, _| {
        let v = arg(args, 0);
        let mut out = Vec::new();
        for k in it.own_keys(&v, false) {
            let x = it.get(&v, &k)?;
            let Key::Str(s) = k else { continue };
            out.push(it.new_array(vec![Value::Str(s), x]));
        }
        Ok(it.new_array(out))
    });
    func(it, &c, "assign", 2, |it, _, args, _| {
        let t = arg(args, 0);
        let t = Value::Obj(it.to_object(&t)?);
        for s in args.iter().skip(1) {
            if s.nullish() {
                continue;
            }
            for k in it.own_keys(s, false) {
                let v = it.get(s, &k)?;
                it.put(&t, k, v)?;
            }
        }
        Ok(t)
    });
    func(it, &c, "create", 2, |it, _, args, _| {
        let proto = match arg(args, 0) {
            Value::Obj(p) => Some(p),
            Value::Null => None,
            _ => return Err(it.type_error("Object prototype may only be an Object or null")),
        };
        let o = it.alloc(proto, Kind::Plain);
        let props = arg(args, 1);
        if !props.nullish() {
            define_properties(it, &o, &props)?;
        }
        Ok(Value::Obj(o))
    });
    func(it, &c, "defineProperty", 3, |it, _, args, _| {
        let Value::Obj(o) = arg(args, 0) else { return Err(it.type_error("Object.defineProperty called on non-object")) };
        let k = it.to_key(&arg(args, 1))?;
        define_property(it, &o, k, &arg(args, 2))?;
        Ok(Value::Obj(o))
    });
    func(it, &c, "defineProperties", 2, |it, _, args, _| {
        let Value::Obj(o) = arg(args, 0) else { return Err(it.type_error("Object.defineProperties called on non-object")) };
        define_properties(it, &o, &arg(args, 1))?;
        Ok(Value::Obj(o))
    });
    func(it, &c, "getOwnPropertyNames", 1, |it, _, args, _| {
        let v = arg(args, 0);
        let mut keys: Vec<Value> = it.own_keys(&v, true).into_iter().filter_map(|k| if let Key::Str(s) = k { Some(Value::Str(s)) } else { None }).collect();
        if v.callable() {
            keys.push(Value::str("name"));
            keys.push(Value::str("length"));
        }
        Ok(it.new_array(keys))
    });
    func(it, &c, "getOwnPropertySymbols", 1, |it, _, _, _| Ok(it.new_array(Vec::new())));
    func(it, &c, "getOwnPropertyDescriptor", 2, |it, _, args, _| {
        let v = arg(args, 0);
        let k = it.to_key(&arg(args, 1))?;
        let Value::Obj(o) = &v else { return Ok(Value::Undef) };
        let prop = o.borrow().props.get(&k).cloned();
        let prop = match prop {
            Some(p) => p,
            None if has_own(it, &v, &k)? => Prop::data(it.get(&v, &k)?),
            None => return Ok(Value::Undef),
        };
        let d = it.new_obj();
        let dv = Value::Obj(d.clone());
        match prop.slot {
            Slot::Data(x) => {
                it.put(&dv, Key::str("value"), x)?;
                it.put(&dv, Key::str("writable"), Value::Bool(prop.writable))?;
            }
            Slot::Acc(g, s) => {
                it.put(&dv, Key::str("get"), g)?;
                it.put(&dv, Key::str("set"), s)?;
            }
        }
        it.put(&dv, Key::str("enumerable"), Value::Bool(prop.enumerable))?;
        it.put(&dv, Key::str("configurable"), Value::Bool(true))?;
        Ok(dv)
    });
    func(it, &c, "getOwnPropertyDescriptors", 1, |it, _, args, _| {
        let v = arg(args, 0);
        let out = it.new_obj();
        let gopd = it.get(&Value::Obj(it.global.clone()), &Key::str("Object"))?;
        let f = it.get(&gopd, &Key::str("getOwnPropertyDescriptor"))?;
        for k in it.own_keys(&v, true) {
            let kv = match &k {
                Key::Str(s) => Value::Str(s.clone()),
                Key::Sym(s) => Value::Sym(*s),
            };
            let d = it.call(&f, Value::Undef, &[v.clone(), kv])?;
            out.borrow_mut().props.insert(k, Prop::data(d));
        }
        Ok(Value::Obj(out))
    });
    func(it, &c, "getPrototypeOf", 1, |it, _, args, _| {
        let o = it.to_object(&arg(args, 0))?;
        let p = o.borrow().proto.clone();
        Ok(p.map(Value::Obj).unwrap_or(Value::Null))
    });
    func(it, &c, "setPrototypeOf", 2, |_, _, args, _| {
        let v = arg(args, 0);
        if let Value::Obj(o) = &v {
            match arg(args, 1) {
                Value::Obj(p) => o.borrow_mut().proto = Some(p),
                Value::Null => o.borrow_mut().proto = None,
                _ => {}
            }
        }
        Ok(v)
    });
    func(it, &c, "freeze", 1, |_, _, args, _| {
        let v = arg(args, 0);
        if let Value::Obj(o) = &v {
            let mut b = o.borrow_mut();
            b.frozen = true;
            let keys: Vec<Key> = b.props.keys().to_vec();
            for k in keys {
                if let Some(p) = b.props.get_mut(&k) {
                    p.writable = false;
                }
            }
        }
        Ok(v)
    });
    func(it, &c, "isFrozen", 1, |_, _, args, _| Ok(Value::Bool(match arg(args, 0) {
        Value::Obj(o) => o.borrow().frozen,
        _ => true,
    })));
    func(it, &c, "seal", 1, |_, _, args, _| Ok(arg(args, 0)));
    func(it, &c, "preventExtensions", 1, |_, _, args, _| Ok(arg(args, 0)));
    func(it, &c, "isSealed", 1, |_, _, args, _| Ok(Value::Bool(!matches!(arg(args, 0), Value::Obj(_)))));
    func(it, &c, "isExtensible", 1, |_, _, args, _| Ok(Value::Bool(matches!(arg(args, 0), Value::Obj(o) if !o.borrow().frozen))));
    func(it, &c, "fromEntries", 1, |it, _, args, _| {
        let items = it.iterate(&arg(args, 0))?;
        let o = it.new_obj();
        for e in items {
            let k = it.get_v(&e, &Value::Num(0.0))?;
            let v = it.get_v(&e, &Value::Num(1.0))?;
            let k = it.to_key(&k)?;
            o.borrow_mut().props.insert(k, Prop::data(v));
        }
        Ok(Value::Obj(o))
    });
    func(it, &c, "is", 2, |_, _, args, _| {
        let (a, b) = (arg(args, 0), arg(args, 1));
        Ok(Value::Bool(match (&a, &b) {
            (Value::Num(x), Value::Num(y)) => (x.is_nan() && y.is_nan()) || (x == y && x.is_sign_negative() == y.is_sign_negative()),
            _ => a.strict_eq(&b),
        }))
    });
    func(it, &c, "hasOwn", 2, |it, _, args, _| {
        let k = it.to_key(&arg(args, 1))?;
        Ok(Value::Bool(has_own(it, &arg(args, 0), &k)?))
    });
}

fn object_ctor(it: &mut Interp, _: &N, args: &[N], _: &N) -> R<N> {
    let v = arg(args, 0);
    if v.nullish() {
        return Ok(Value::Obj(it.new_obj()));
    }
    Ok(Value::Obj(it.to_object(&v)?))
}

pub fn object_to_string(it: &mut Interp, this: &N, _: &[N], _: &N) -> R<N> {
    let tag = match this {
        Value::Undef => "Undefined",
        Value::Null => "Null",
        Value::Obj(o) => {
            let t = it.get(this, &Key::Sym(SYM_TO_STRING_TAG))?;
            if let Value::Str(s) = t {
                return Ok(Value::str(&format!("[object {s}]")));
            }
            match o.borrow().kind {
                Kind::Array(_) => "Array",
                Kind::Func(_) | Kind::Native(_) | Kind::Bound(_) => "Function",
                Kind::Error => "Error",
                Kind::Date(_) => "Date",
                Kind::RegExp(_) => "RegExp",
                Kind::Prim(Value::Str(_)) => "String",
                Kind::Prim(Value::Num(_)) => "Number",
                Kind::Prim(Value::Bool(_)) => "Boolean",
                _ => "Object",
            }
        }
        Value::Str(_) => "String",
        Value::Num(_) => "Number",
        Value::Bool(_) => "Boolean",
        Value::Sym(_) => "Symbol",
    };
    Ok(Value::str(&format!("[object {tag}]")))
}

fn define_properties(it: &mut Interp, o: &ObjRef, props: &Value) -> R<()> {
    for k in it.own_keys(props, false) {
        let d = it.get(props, &k)?;
        define_property(it, o, k, &d)?;
    }
    Ok(())
}

pub fn define_property(it: &mut Interp, o: &ObjRef, k: Key, desc: &Value) -> R<()> {
    if !matches!(desc, Value::Obj(_)) {
        return Err(it.type_error("Property description must be an object"));
    }
    let field = |it: &mut Interp, n: &str| -> R<Option<Value>> {
        let Value::Obj(d) = desc else { return Ok(None) };
        let key = Key::str(n);
        if it.has_property(d, &key)? {
            Ok(Some(it.get(desc, &key)?))
        } else {
            Ok(None)
        }
    };
    let get = field(it, "get")?;
    let set = field(it, "set")?;
    let value = field(it, "value")?;
    let enumerable = field(it, "enumerable")?.map(|v| v.truthy());
    let writable = field(it, "writable")?.map(|v| v.truthy());
    let old = o.borrow().props.get(&k).cloned();
    if get.is_some() || set.is_some() {
        let (g0, s0) = match &old {
            Some(Prop { slot: Slot::Acc(g, s), .. }) => (g.clone(), s.clone()),
            _ => (Value::Undef, Value::Undef),
        };
        let p = Prop { slot: Slot::Acc(get.unwrap_or(g0), set.unwrap_or(s0)), enumerable: enumerable.unwrap_or(old.as_ref().is_some_and(|p| p.enumerable)), writable: true };
        o.borrow_mut().props.insert(k, p);
        return Ok(());
    }
    let is_array_slot = matches!(o.borrow().kind, Kind::Array(_)) && (k.index().is_some() || k.is("length"));
    if is_array_slot {
        if let Some(v) = value {
            it.put_obj(o, k, v, &Value::Obj(o.clone()))?;
        }
        return Ok(());
    }
    let p = match old {
        Some(mut p) => {
            if let Some(v) = value {
                p.slot = Slot::Data(v);
            }
            if let Some(e) = enumerable {
                p.enumerable = e;
            }
            if let Some(w) = writable {
                p.writable = w;
            }
            p
        }
        None => Prop { slot: Slot::Data(value.unwrap_or_default()), enumerable: enumerable.unwrap_or(false), writable: writable.unwrap_or(false) },
    };
    o.borrow_mut().props.insert(k, p);
    Ok(())
}

// ---------------------------------------------------------------------------
// Function

fn function(it: &mut Interp) {
    let proto = it.p.function.clone();
    // Function.prototype is itself callable and does nothing.
    proto.borrow_mut().kind = Kind::Native(Rc::new(Native { f: undef, data: Value::Undef, name: Rc::from(""), arity: 0, ctor: false }));
    ctor(it, "Function", 1, |it, _, args, _| {
        let mut params = String::new();
        for (i, a) in args.iter().take(args.len().saturating_sub(1)).enumerate() {
            if i > 0 {
                params.push(',');
            }
            params.push_str(&it.to_str(a)?);
        }
        let body = match args.last() {
            Some(b) => it.to_str(b)?,
            None => Rc::from(""),
        };
        it.eval_src(&format!("(function anonymous({params}\n) {{\n{body}\n}})"))
    }, &proto);
    func(it, &proto, "call", 1, |it, this, args, _| {
        let t = arg(args, 0);
        it.call(this, t, args.get(1..).unwrap_or(&[]))
    });
    func(it, &proto, "apply", 2, |it, this, args, _| {
        let t = arg(args, 0);
        let list = arg(args, 1);
        let list = if list.nullish() { Vec::new() } else { crate::array::list_of(it, &list)? };
        it.call(this, t, &list)
    });
    func(it, &proto, "bind", 1, |it, this, args, _| {
        if !this.callable() {
            return Err(it.type_error("Bind must be called on a function"));
        }
        let b = Bound { target: this.clone(), this: arg(args, 0), args: args.get(1..).unwrap_or(&[]).to_vec() };
        let p = it.p.function.clone();
        Ok(Value::Obj(it.alloc(Some(p), Kind::Bound(Rc::new(b)))))
    });
    func(it, &proto, "toString", 0, |it, this, _, _| {
        let name = match this {
            Value::Obj(_) => {
                let n = it.get(this, &Key::str("name"))?;
                it.to_str(&n)?
            }
            _ => return Err(it.type_error("Function.prototype.toString requires that 'this' be a Function")),
        };
        Ok(Value::str(&format!("function {name}() {{ [native code] }}")))
    });
    let has_instance = it.native("[Symbol.hasInstance]", 1, |it, this, args, _| Ok(Value::Bool(it.instance_of(&arg(args, 0), this)?)), Value::Undef, false);
    proto.borrow_mut().props.insert(Key::Sym(SYM_HAS_INSTANCE), Prop::hidden(Value::Obj(has_instance)));
}

// ---------------------------------------------------------------------------
// Errors

fn error_ctor(it: &mut Interp, this: &N, args: &[N], data: &N) -> R<N> {
    let o = match (this, &it.new_target) {
        (Value::Obj(o), nt) if !nt.nullish() => o.clone(),
        _ => {
            let proto = data.obj().cloned().unwrap_or_else(|| it.p.error.clone());
            it.alloc(Some(proto), Kind::Plain)
        }
    };
    o.borrow_mut().kind = Kind::Error;
    let msg = arg(args, 0);
    if !matches!(msg, Value::Undef) {
        let m = it.to_str(&msg)?;
        o.borrow_mut().props.insert(Key::str("message"), Prop::hidden(Value::Str(m)));
    }
    let opts = arg(args, 1);
    if let Value::Obj(op) = &opts {
        if it.has_property(op, &Key::str("cause"))? {
            let c = it.get(&opts, &Key::str("cause"))?;
            o.borrow_mut().props.insert(Key::str("cause"), Prop::hidden(c));
        }
    }
    let v = Value::Obj(o.clone());
    let text = it.describe_error(&v);
    o.borrow_mut().props.insert(Key::str("stack"), Prop::hidden(Value::str(&format!("{text}\n    at <anonymous>"))));
    Ok(v)
}

fn errors(it: &mut Interp) {
    let base = it.p.error.clone();
    let protos = [
        ("Error", base.clone()),
        ("TypeError", it.p.type_error.clone()),
        ("RangeError", it.p.range_error.clone()),
        ("SyntaxError", it.p.syntax_error.clone()),
        ("ReferenceError", it.p.reference_error.clone()),
        ("EvalError", it.alloc(Some(base.clone()), Kind::Plain)),
        ("URIError", it.alloc(Some(base.clone()), Kind::Plain)),
        ("AggregateError", it.alloc(Some(base.clone()), Kind::Plain)),
    ];
    let mut base_ctor = None;
    for (name, proto) in protos {
        let c = it.native(name, 1, error_ctor, Value::Obj(proto.clone()), true);
        c.borrow_mut().props.insert(Key::str("prototype"), Prop { slot: Slot::Data(Value::Obj(proto.clone())), enumerable: false, writable: false });
        proto.borrow_mut().props.insert(Key::str("constructor"), Prop::hidden(Value::Obj(c.clone())));
        it.set_hidden(&proto, "name", Value::str(name));
        it.set_hidden(&proto, "message", Value::str(""));
        it.set_global(name, Value::Obj(c.clone()));
        match &base_ctor {
            None => base_ctor = Some(c),
            Some(b) => c.borrow_mut().proto = Some(b.clone()),
        }
    }
    if let Some(c) = base_ctor {
        func(it, &c, "captureStackTrace", 1, undef);
    }
    func(it, &base, "toString", 0, |it, this, _, _| {
        if !matches!(this, Value::Obj(_)) {
            return Err(it.type_error("Error.prototype.toString called on non-object"));
        }
        let n = it.get(this, &Key::str("name"))?;
        let n = if matches!(n, Value::Undef) { Rc::from("Error") } else { it.to_str(&n)? };
        let m = it.get(this, &Key::str("message"))?;
        let m = if matches!(m, Value::Undef) { Rc::from("") } else { it.to_str(&m)? };
        Ok(Value::str(&match (n.is_empty(), m.is_empty()) {
            (_, true) => String::from(&*n),
            (true, false) => String::from(&*m),
            _ => format!("{n}: {m}"),
        }))
    });
}

// ---------------------------------------------------------------------------
// Symbol, Boolean, Number

fn symbol(it: &mut Interp) {
    let proto = it.p.symbol.clone();
    let c = it.native("Symbol", 0, |it, _, args, _| {
        let d = arg(args, 0);
        let d = if matches!(d, Value::Undef) { Rc::from("") } else { it.to_str(&d)? };
        Ok(it.symbol(&d))
    }, Value::Undef, false);
    c.borrow_mut().props.insert(Key::str("prototype"), Prop::hidden(Value::Obj(proto.clone())));
    proto.borrow_mut().props.insert(Key::str("constructor"), Prop::hidden(Value::Obj(c.clone())));
    it.set_global("Symbol", Value::Obj(c.clone()));
    for (name, id) in [("iterator", SYM_ITERATOR), ("asyncIterator", SYM_ASYNC_ITERATOR), ("hasInstance", SYM_HAS_INSTANCE), ("toPrimitive", SYM_TO_PRIMITIVE), ("toStringTag", SYM_TO_STRING_TAG)] {
        it.set_hidden(&c, name, Value::Sym(id));
    }
    func(it, &c, "for", 1, |it, _, args, _| {
        let k = str_arg(it, args, 0)?;
        if let Some((_, id)) = it.registry.iter().find(|(n, _)| *n == k) {
            return Ok(Value::Sym(*id));
        }
        let s = it.symbol(&k);
        if let Value::Sym(id) = s {
            it.registry.push((k, id));
        }
        Ok(s)
    });
    func(it, &proto, "toString", 0, |it, this, _, _| {
        let s = this_sym(it, this)?;
        Ok(Value::str(&format!("Symbol({})", it.symbols[s as usize])))
    });
    func(it, &proto, "valueOf", 0, |it, this, _, _| Ok(Value::Sym(this_sym(it, this)?)));
    it.accessor(&proto, "description", |it, this, _, _| {
        let s = this_sym(it, this)?;
        Ok(Value::Str(it.symbols[s as usize].clone()))
    }, None, Value::Undef);
}

fn this_sym(it: &mut Interp, this: &Value) -> R<u32> {
    match this {
        Value::Sym(s) => Ok(*s),
        Value::Obj(o) => match &o.borrow().kind {
            Kind::Prim(Value::Sym(s)) => Ok(*s),
            _ => Err(it.type_error("not a symbol")),
        },
        _ => Err(it.type_error("not a symbol")),
    }
}

fn boolean(it: &mut Interp) {
    let proto = it.p.boolean.clone();
    ctor(it, "Boolean", 1, |it, this, args, _| {
        let b = arg(args, 0).truthy();
        if !it.new_target.nullish() {
            if let Value::Obj(o) = this {
                o.borrow_mut().kind = Kind::Prim(Value::Bool(b));
            }
            return Ok(this.clone());
        }
        Ok(Value::Bool(b))
    }, &proto);
    fn this_bool(it: &mut Interp, this: &Value) -> R<bool> {
        match this {
            Value::Bool(b) => Ok(*b),
            Value::Obj(o) => match &o.borrow().kind {
                Kind::Prim(Value::Bool(b)) => Ok(*b),
                _ => Err(it.type_error("not a boolean")),
            },
            _ => Err(it.type_error("not a boolean")),
        }
    }
    func(it, &proto, "toString", 0, |it, this, _, _| Ok(Value::str(if this_bool(it, this)? { "true" } else { "false" })));
    func(it, &proto, "valueOf", 0, |it, this, _, _| Ok(Value::Bool(this_bool(it, this)?)));
}

pub fn this_num(it: &mut Interp, this: &Value) -> R<f64> {
    match this {
        Value::Num(n) => Ok(*n),
        Value::Obj(o) => match &o.borrow().kind {
            Kind::Prim(Value::Num(n)) => Ok(*n),
            _ => Err(it.type_error("not a number")),
        },
        _ => Err(it.type_error("not a number")),
    }
}

/// `n.toString(radix)`.
fn radix_string(n: f64, radix: u32) -> String {
    if n.is_nan() || n.is_infinite() || n == 0.0 {
        return num_to_string(n);
    }
    let digits = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let neg = n < 0.0;
    let n = n.abs();
    let mut int = fm::floor(n);
    let mut frac = n - int;
    let mut out = Vec::new();
    if int == 0.0 {
        out.push(b'0');
    }
    while int >= 1.0 {
        let d = (int % f64::from(radix)) as usize;
        out.push(digits[d]);
        int = fm::floor(int / f64::from(radix));
    }
    out.reverse();
    if frac > 0.0 {
        out.push(b'.');
        for _ in 0..20 {
            frac *= f64::from(radix);
            let d = fm::floor(frac);
            out.push(digits[d as usize]);
            frac -= d;
            if frac <= 0.0 {
                break;
            }
        }
    }
    let mut s = String::from_utf8(out).unwrap_or_default();
    if neg {
        s.insert(0, '-');
    }
    s
}

/// `1.5e3` as JavaScript writes it: `1.5e+3`.
fn js_exp(s: &str) -> String {
    match s.split_once('e') {
        Some((m, e)) if !e.starts_with('-') => format!("{m}e+{e}"),
        _ => String::from(s),
    }
}

pub fn parse_int(s: &str, radix: i32) -> f64 {
    let s = s.trim_start();
    let (neg, mut s) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let mut radix = radix;
    if (radix == 0 || radix == 16) && (s.starts_with("0x") || s.starts_with("0X")) {
        s = &s[2..];
        radix = 16;
    }
    if radix == 0 {
        radix = 10;
    }
    if !(2..=36).contains(&radix) {
        return f64::NAN;
    }
    let mut v = 0f64;
    let mut any = false;
    for c in s.chars() {
        let Some(d) = c.to_digit(radix as u32) else { break };
        v = v * f64::from(radix) + f64::from(d);
        any = true;
    }
    if !any {
        return f64::NAN;
    }
    if neg {
        -v
    } else {
        v
    }
}

pub fn parse_float(s: &str) -> f64 {
    let s = s.trim_start();
    let b = s.as_bytes();
    let mut i = 0;
    if matches!(b.first(), Some(b'+' | b'-')) {
        i += 1;
    }
    if s[i..].starts_with("Infinity") {
        return if b.first() == Some(&b'-') { f64::NEG_INFINITY } else { f64::INFINITY };
    }
    let start_digits = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
    }
    if i == start_digits || (i == start_digits + 1 && b[start_digits] == b'.') {
        return f64::NAN;
    }
    if i < b.len() && (b[i] | 0x20) == b'e' {
        let mut j = i + 1;
        if matches!(b.get(j), Some(b'+' | b'-')) {
            j += 1;
        }
        if b.get(j).is_some_and(u8::is_ascii_digit) {
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            i = j;
        }
    }
    s[..i].parse().unwrap_or(f64::NAN)
}

/// Digits grouped the Italian way: `1.234.567,89`.
/// Just enough `Intl` for pages that format numbers and dates: always in
/// Italian, whatever locale they ask for.
fn intl(it: &mut Interp) {
    let intl = it.new_obj();
    it.set_global("Intl", Value::Obj(intl.clone()));
    let nf = it.native(
        "NumberFormat",
        2,
        |it, _, args, _| {
            let mut max = 3.0;
            if let Value::Obj(_) = arg(args, 1) {
                if let Value::Num(m) = it.get(&arg(args, 1), &Key::str("maximumFractionDigits"))? {
                    max = m.clamp(0.0, 20.0);
                }
            }
            let o = it.new_obj();
            let f = it.native(
                "format",
                1,
                |it, _, args, data| {
                    let n = num_arg(it, args, 0)?;
                    let Value::Num(max) = data else { return Ok(Value::Undef) };
                    Ok(Value::str(&locale_number(n, *max as usize)))
                },
                Value::Num(max),
                false,
            );
            it.set_hidden(&o, "format", Value::Obj(f));
            Ok(Value::Obj(o))
        },
        Value::Undef,
        true,
    );
    it.set_hidden(&intl, "NumberFormat", Value::Obj(nf));
    let df = it.native(
        "DateTimeFormat",
        2,
        |it, _, args, _| {
            let o = it.new_obj();
            let with_time = match arg(args, 1) {
                v @ Value::Obj(_) => !it.get(&v, &Key::str("hour"))?.nullish(),
                _ => false,
            };
            let f = it.native(
                "format",
                1,
                |it, _, args, data| {
                    let d = match arg(args, 0) {
                        Value::Undef => {
                            let g = Value::Obj(it.global.clone());
                            let date = it.get(&g, &Key::str("Date"))?;
                            it.construct(&date, &[], None)?
                        }
                        d => d,
                    };
                    let m = if data.truthy() { "toLocaleString" } else { "toLocaleDateString" };
                    let f = it.get(&d, &Key::str(m))?;
                    it.call(&f, d, &[])
                },
                Value::Bool(with_time),
                false,
            );
            it.set_hidden(&o, "format", Value::Obj(f));
            Ok(Value::Obj(o))
        },
        Value::Undef,
        true,
    );
    it.set_hidden(&intl, "DateTimeFormat", Value::Obj(df));
}

pub fn locale_number(n: f64, max_frac: usize) -> String {
    if !n.is_finite() {
        return num_to_string(n);
    }
    let s = format!("{:.*}", max_frac, n.abs());
    let (int, frac) = s.split_once('.').unwrap_or((&s, ""));
    let frac = frac.trim_end_matches('0');
    let mut out = String::new();
    if n < 0.0 && (int != "0" || !frac.is_empty()) {
        out.push('-');
    }
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            out.push('.');
        }
        out.push(c);
    }
    if !frac.is_empty() {
        out.push(',');
        out.push_str(frac);
    }
    out
}

fn number(it: &mut Interp) {
    let proto = it.p.number.clone();
    let c = ctor(it, "Number", 1, |it, this, args, _| {
        let n = if args.is_empty() { 0.0 } else { num_arg(it, args, 0)? };
        if !it.new_target.nullish() {
            if let Value::Obj(o) = this {
                o.borrow_mut().kind = Kind::Prim(Value::Num(n));
            }
            return Ok(this.clone());
        }
        Ok(Value::Num(n))
    }, &proto);
    for (name, v) in [
        ("MAX_SAFE_INTEGER", 9007199254740991.0),
        ("MIN_SAFE_INTEGER", -9007199254740991.0),
        ("EPSILON", f64::EPSILON),
        ("MAX_VALUE", f64::MAX),
        ("MIN_VALUE", 5e-324),
        ("POSITIVE_INFINITY", f64::INFINITY),
        ("NEGATIVE_INFINITY", f64::NEG_INFINITY),
        ("NaN", f64::NAN),
    ] {
        it.set_hidden(&c, name, Value::Num(v));
    }
    func(it, &c, "isInteger", 1, |_, _, args, _| Ok(Value::Bool(matches!(arg(args, 0), Value::Num(n) if n.is_finite() && fm::trunc(n) == n))));
    func(it, &c, "isSafeInteger", 1, |_, _, args, _| Ok(Value::Bool(matches!(arg(args, 0), Value::Num(n) if n.is_finite() && fm::trunc(n) == n && n.abs() <= 9007199254740991.0))));
    func(it, &c, "isFinite", 1, |_, _, args, _| Ok(Value::Bool(matches!(arg(args, 0), Value::Num(n) if n.is_finite()))));
    func(it, &c, "isNaN", 1, |_, _, args, _| Ok(Value::Bool(matches!(arg(args, 0), Value::Num(n) if n.is_nan()))));
    func(it, &proto, "toString", 1, |it, this, args, _| {
        let n = this_num(it, this)?;
        let r = arg(args, 0);
        let radix = if matches!(r, Value::Undef) { 10.0 } else { it.to_number(&r)? };
        if !(2.0..=36.0).contains(&radix) {
            return Err(it.range_error("toString() radix must be between 2 and 36"));
        }
        Ok(Value::str(&if radix == 10.0 { num_to_string(n) } else { radix_string(n, radix as u32) }))
    });
    func(it, &proto, "toLocaleString", 0, |it, this, args, _| {
        let n = this_num(it, this)?;
        let mut max = 3;
        if let Value::Obj(_) = arg(args, 1) {
            let m = it.get(&arg(args, 1), &Key::str("maximumFractionDigits"))?;
            if let Value::Num(m) = m {
                max = m.clamp(0.0, 20.0) as usize;
            }
        }
        Ok(Value::str(&locale_number(n, max)))
    });
    func(it, &proto, "valueOf", 0, |it, this, _, _| Ok(Value::Num(this_num(it, this)?)));
    intl(it);
    func(it, &proto, "toFixed", 1,|it, this, args, _| {
        let n = this_num(it, this)?;
        let d = num_arg(it, args, 0)?;
        let d = if d.is_nan() { 0.0 } else { d };
        if !(0.0..=100.0).contains(&d) {
            return Err(it.range_error("toFixed() digits argument must be between 0 and 100"));
        }
        if !n.is_finite() || n.abs() >= 1e21 {
            return Ok(Value::str(&num_to_string(n)));
        }
        let s = format!("{:.*}", d as usize, n);
        Ok(Value::str(if s.starts_with("-") && s[1..].bytes().all(|b| b == b'0' || b == b'.') { &s[1..] } else { &s }))
    });
    func(it, &proto, "toPrecision", 1, |it, this, args, _| {
        let n = this_num(it, this)?;
        if matches!(arg(args, 0), Value::Undef) || !n.is_finite() {
            return Ok(Value::str(&num_to_string(n)));
        }
        let p = num_arg(it, args, 0)?;
        if !(1.0..=100.0).contains(&p) {
            return Err(it.range_error("toPrecision() argument must be between 1 and 100"));
        }
        let p = p as usize;
        if n == 0.0 {
            return Ok(Value::str(&format!("{:.*}", p - 1, 0.0)));
        }
        let e = format!("{:.*e}", p - 1, n);
        let exp: i32 = e.split_once('e').and_then(|x| x.1.parse().ok()).unwrap_or(0);
        if exp < -6 || exp >= p as i32 {
            return Ok(Value::str(&js_exp(&e)));
        }
        Ok(Value::str(&format!("{:.*}", (p as i32 - 1 - exp).max(0) as usize, n)))
    });
    func(it, &proto, "toExponential", 1, |it, this, args, _| {
        let n = this_num(it, this)?;
        if !n.is_finite() {
            return Ok(Value::str(&num_to_string(n)));
        }
        let s = if matches!(arg(args, 0), Value::Undef) { format!("{n:e}") } else { format!("{:.*e}", num_arg(it, args, 0)?.clamp(0.0, 100.0) as usize, n) };
        Ok(Value::str(&js_exp(&s)))
    });
}

// ---------------------------------------------------------------------------
// Math

fn math(it: &mut Interp) {
    let m = it.new_obj();
    it.set_global("Math", Value::Obj(m.clone()));
    for (name, v) in [
        ("PI", core::f64::consts::PI),
        ("E", core::f64::consts::E),
        ("LN2", core::f64::consts::LN_2),
        ("LN10", core::f64::consts::LN_10),
        ("LOG2E", core::f64::consts::LOG2_E),
        ("LOG10E", core::f64::consts::LOG10_E),
        ("SQRT2", core::f64::consts::SQRT_2),
        ("SQRT1_2", core::f64::consts::FRAC_1_SQRT_2),
    ] {
        it.set_hidden(&m, name, Value::Num(v));
    }
    m.borrow_mut().props.insert(Key::Sym(SYM_TO_STRING_TAG), Prop::hidden(Value::str("Math")));
    macro_rules! unary {
        ($name:expr, $f:expr) => {
            func(it, &m, $name, 1, |it, _, args, _| {
                let x = num_arg(it, args, 0)?;
                let f: fn(f64) -> f64 = $f;
                Ok(Value::Num(f(x)))
            });
        };
    }
    unary!("abs", f64::abs);
    unary!("floor", fm::floor);
    unary!("ceil", fm::ceil);
    unary!("trunc", fm::trunc);
    unary!("sqrt", fm::sqrt);
    unary!("cbrt", |x| if x < 0.0 { -fm::powf(-x, 1.0 / 3.0) } else { fm::powf(x, 1.0 / 3.0) });
    unary!("exp", fm::exp);
    unary!("expm1", |x| fm::exp(x) - 1.0);
    unary!("log", fm::ln);
    unary!("log1p", |x| fm::ln(1.0 + x));
    unary!("log2", fm::log2);
    unary!("log10", fm::log10);
    unary!("sin", fm::sin);
    unary!("cos", fm::cos);
    unary!("tan", fm::tan);
    unary!("asin", fm::asin);
    unary!("acos", fm::acos);
    unary!("atan", fm::atan);
    unary!("sinh", |x| (fm::exp(x) - fm::exp(-x)) / 2.0);
    unary!("cosh", |x| (fm::exp(x) + fm::exp(-x)) / 2.0);
    unary!("tanh", |x| {
        if x > 20.0 {
            1.0
        } else if x < -20.0 {
            -1.0
        } else {
            let (a, b) = (fm::exp(x), fm::exp(-x));
            (a - b) / (a + b)
        }
    });
    unary!("sign", |x| if x.is_nan() || x == 0.0 { x } else if x > 0.0 { 1.0 } else { -1.0 });
    unary!("fround", |x| f64::from(x as f32));
    unary!("clz32", |x| f64::from(to_u32(x).leading_zeros()));
    unary!("round", |x| {
        if !x.is_finite() || fm::trunc(x) == x {
            x
        } else {
            let r = fm::floor(x + 0.5);
            if r == 0.0 && x < 0.0 {
                -0.0
            } else {
                r
            }
        }
    });
    func(it, &m, "atan2", 2, |it, _, args, _| {
        let y = num_arg(it, args, 0)?;
        let x = num_arg(it, args, 1)?;
        Ok(Value::Num(fm::atan2(y, x)))
    });
    func(it, &m, "pow", 2, |it, _, args, _| {
        let a = arg(args, 0);
        let b = arg(args, 1);
        it.binop(crate::ast::Op::Exp, &a, &b)
    });
    func(it, &m, "imul", 2, |it, _, args, _| {
        let a = to_i32(num_arg(it, args, 0)?);
        let b = to_i32(num_arg(it, args, 1)?);
        Ok(Value::Num(f64::from(a.wrapping_mul(b))))
    });
    func(it, &m, "hypot", 2, |it, _, args, _| {
        let mut s = 0.0;
        for i in 0..args.len() {
            let x = num_arg(it, args, i)?;
            s += x * x;
        }
        Ok(Value::Num(fm::sqrt(s)))
    });
    func(it, &m, "max", 2, |it, _, args, _| {
        let mut r = f64::NEG_INFINITY;
        for i in 0..args.len() {
            let x = num_arg(it, args, i)?;
            if x.is_nan() {
                return Ok(Value::Num(f64::NAN));
            }
            if x > r || (x == 0.0 && r == 0.0 && !x.is_sign_negative()) {
                r = x;
            }
        }
        Ok(Value::Num(r))
    });
    func(it, &m, "min", 2, |it, _, args, _| {
        let mut r = f64::INFINITY;
        for i in 0..args.len() {
            let x = num_arg(it, args, i)?;
            if x.is_nan() {
                return Ok(Value::Num(f64::NAN));
            }
            if x < r || (x == 0.0 && r == 0.0 && x.is_sign_negative()) {
                r = x;
            }
        }
        Ok(Value::Num(r))
    });
    func(it, &m, "random", 0, |it, _, _, _| Ok(Value::Num(it.random())));
}

// ---------------------------------------------------------------------------
// Global functions

const URI_KEEP: &str = "-_.!~*'()";
const URI_RESERVED: &str = ";,/?:@&=+$#";

fn uri_encode(s: &str, keep: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || URI_KEEP.contains(c) || keep.contains(c) {
            out.push(c);
        } else {
            for b in c.encode_utf8(&mut [0; 4]).bytes() {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

fn uri_decode(s: &str, keep: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = s.get(i + 1..i + 3)?;
            let v = u8::from_str_radix(h, 16).ok()?;
            if v < 0x80 && keep.contains(v as char) {
                out.extend_from_slice(&b[i..i + 3]);
            } else {
                out.push(v);
            }
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn globals(it: &mut Interp) {
    let g = it.global.clone();
    func(it, &g, "parseInt", 2, |it, _, args, _| {
        let s = str_arg(it, args, 0)?;
        let r = num_arg(it, args, 1)?;
        Ok(Value::Num(parse_int(&s, if r.is_nan() { 0 } else { to_i32(r) })))
    });
    func(it, &g, "parseFloat", 1, |it, _, args, _| {
        let s = str_arg(it, args, 0)?;
        Ok(Value::Num(parse_float(&s)))
    });
    func(it, &g, "isNaN", 1, |it, _, args, _| Ok(Value::Bool(num_arg(it, args, 0)?.is_nan())));
    func(it, &g, "isFinite", 1, |it, _, args, _| Ok(Value::Bool(num_arg(it, args, 0)?.is_finite())));
    func(it, &g, "encodeURIComponent", 1, |it, _, args, _| Ok(Value::str(&uri_encode(&str_arg(it, args, 0)?, ""))));
    func(it, &g, "encodeURI", 1, |it, _, args, _| Ok(Value::str(&uri_encode(&str_arg(it, args, 0)?, URI_RESERVED))));
    func(it, &g, "decodeURIComponent", 1, |it, _, args, _| {
        let s = str_arg(it, args, 0)?;
        match uri_decode(&s, "") {
            Some(d) => Ok(Value::str(&d)),
            None => Err(it.type_error("URI malformed")),
        }
    });
    func(it, &g, "decodeURI", 1, |it, _, args, _| {
        let s = str_arg(it, args, 0)?;
        match uri_decode(&s, URI_RESERVED) {
            Some(d) => Ok(Value::str(&d)),
            None => Err(it.type_error("URI malformed")),
        }
    });
    func(it, &g, "escape", 1, |it, _, args, _| Ok(Value::str(&uri_encode(&str_arg(it, args, 0)?, "@*_+-./"))));
    func(it, &g, "unescape", 1, |it, _, args, _| {
        let s = str_arg(it, args, 0)?;
        Ok(Value::str(&uri_decode(&s, "").unwrap_or_else(|| String::from(&*s))))
    });
    func(it, &g, "eval", 1, |it, _, args, _| match arg(args, 0) {
        Value::Str(s) => it.eval_src(&s),
        v => Ok(v),
    });
    let parse_int_fn = it.get(&Value::Obj(g.clone()), &Key::str("parseInt")).unwrap_or_default();
    let parse_float_fn = it.get(&Value::Obj(g.clone()), &Key::str("parseFloat")).unwrap_or_default();
    if let Ok(Value::Obj(n)) = it.get(&Value::Obj(g), &Key::str("Number")) {
        it.set_hidden(&n, "parseInt", parse_int_fn);
        it.set_hidden(&n, "parseFloat", parse_float_fn);
    }
}

// ---------------------------------------------------------------------------
// console

/// A value the way the console shows it.
pub fn inspect(it: &mut Interp, v: &Value, depth: u32) -> String {
    match v {
        Value::Str(s) if depth > 0 => format!("'{s}'"),
        Value::Obj(o) if depth < 3 => {
            enum S {
                Arr(Vec<Value>),
                Err,
                Func,
                Obj,
                Other,
            }
            let shape = match &o.borrow().kind {
                Kind::Array(a) => S::Arr(a.clone()),
                Kind::Error => S::Err,
                Kind::Func(_) | Kind::Native(_) | Kind::Bound(_) => S::Func,
                Kind::Plain | Kind::Host(..) => S::Obj,
                _ => S::Other,
            };
            match shape {
                S::Arr(a) => {
                    let parts: Vec<String> = a.iter().take(50).map(|x| inspect(it, x, depth + 1)).collect();
                    format!("[{}{}]", parts.join(", "), if a.len() > 50 { ", …" } else { "" })
                }
                S::Err => it.describe_error(v),
                S::Func => format!("[{}]", it.display(v)),
                S::Obj => {
                    let keys = it.own_keys(v, false);
                    let mut parts = Vec::new();
                    for k in keys.iter().take(30) {
                        let x = it.get(v, k).unwrap_or_default();
                        parts.push(format!("{}: {}", it.key_text(k), inspect(it, &x, depth + 1)));
                    }
                    if parts.is_empty() {
                        String::from("{}")
                    } else {
                        format!("{{ {} }}", parts.join(", "))
                    }
                }
                S::Other => it.display(v),
            }
        }
        Value::Obj(_) => String::from("[Object]"),
        v => it.display(v),
    }
}

fn console(it: &mut Interp) {
    let c = it.new_obj();
    it.set_global("console", Value::Obj(c.clone()));
    fn print(it: &mut Interp, args: &[Value], data: &Value) -> R<Value> {
        let mut line = match data {
            Value::Str(p) => String::from(&**p),
            _ => String::new(),
        };
        for (i, a) in args.iter().enumerate() {
            if i > 0 {
                line.push(' ');
            }
            line.push_str(&inspect(it, a, 0));
        }
        it.log(line);
        Ok(Value::Undef)
    }
    for (name, prefix) in [("log", ""), ("info", ""), ("debug", ""), ("dir", ""), ("table", ""), ("trace", ""), ("warn", "⚠ "), ("error", "✖ ")] {
        let f = it.native(name, 0, |it, _, args, data| print(it, args, data), Value::str(prefix), false);
        it.set_hidden(&c, name, Value::Obj(f));
    }
    for name in ["group", "groupCollapsed", "groupEnd", "time", "timeEnd", "timeLog", "count", "countReset", "clear", "profile", "profileEnd"] {
        func(it, &c, name, 0, undef);
    }
    func(it, &c, "assert", 0, |it, _, args, _| {
        if !arg(args, 0).truthy() {
            print(it, args.get(1..).unwrap_or(&[]), &Value::str("✖ Assertion failed: "))?;
        }
        Ok(Value::Undef)
    });
}

// ---------------------------------------------------------------------------
// Timers

fn timers(it: &mut Interp) {
    let g = it.global.clone();
    fn add(it: &mut Interp, args: &[Value], repeat: bool) -> R<Value> {
        let mut f = arg(args, 0);
        if let Value::Str(s) = &f {
            f = it.eval_src(&format!("(function(){{{s}\n}})"))?;
        }
        if !f.callable() {
            return Ok(Value::Num(0.0));
        }
        let delay = num_arg(it, args, 1)?;
        let rest = args.get(2..).unwrap_or(&[]).to_vec();
        Ok(Value::Num(f64::from(it.add_timer(f, delay, rest, repeat))))
    }
    func(it, &g, "setTimeout", 2, |it, _, args, _| add(it, args, false));
    func(it, &g, "setInterval", 2, |it, _, args, _| add(it, args, true));
    fn clear(it: &mut Interp, _: &Value, args: &[Value], _: &Value) -> R<Value> {
        if let Value::Num(n) = arg(args, 0) {
            it.clear_timer(n as u32);
        }
        Ok(Value::Undef)
    }
    func(it, &g, "clearTimeout", 1, clear);
    func(it, &g, "clearInterval", 1, clear);
    func(it, &g, "queueMicrotask", 1, |it, _, args, _| {
        let f = arg(args, 0);
        if f.callable() {
            it.jobs.push_back(Job::Call(f, Value::Undef, Vec::new()));
        }
        Ok(Value::Undef)
    });
}

// ---------------------------------------------------------------------------
// Promise

fn this_promise(it: &mut Interp, this: &Value) -> R<ObjRef> {
    match this {
        Value::Obj(o) if matches!(o.borrow().kind, Kind::Promise(_)) => Ok(o.clone()),
        _ => Err(it.type_error("not a promise")),
    }
}

/// A function that hands its data to a helper: `[f, …]`.
fn closure(it: &mut Interp, f: NativeFn, data: Vec<Value>) -> Value {
    let d = it.new_array(data);
    Value::Obj(it.native("", 1, f, d, false))
}

fn data_at(data: &Value, i: usize) -> Value {
    match data {
        Value::Obj(o) => match &o.borrow().kind {
            Kind::Array(a) => a.get(i).cloned().unwrap_or_default(),
            _ => Value::Undef,
        },
        _ => Value::Undef,
    }
}

fn promise(it: &mut Interp) {
    let proto = it.p.promise.clone();
    proto.borrow_mut().props.insert(Key::Sym(SYM_TO_STRING_TAG), Prop::hidden(Value::str("Promise")));
    let c = ctor(it, "Promise", 1, |it, this, args, _| {
        if it.new_target.nullish() {
            return Err(it.type_error("Promise constructor cannot be invoked without 'new'"));
        }
        let ex = arg(args, 0);
        if !ex.callable() {
            return Err(it.type_error("Promise resolver is not a function"));
        }
        let Value::Obj(o) = this else { return Ok(Value::Undef) };
        o.borrow_mut().kind = Kind::Promise(Box::new(Promise { state: PromiseState::Pending, reactions: Vec::new(), handled: false }));
        let (res, rej) = it.resolving_functions(o);
        match it.call(&ex, Value::Undef, &[res, rej.clone()]) {
            Ok(_) => {}
            Err(Ctl::Throw(e)) => {
                it.call(&rej, Value::Undef, &[e])?;
            }
            Err(e) => return Err(e),
        }
        Ok(this.clone())
    }, &proto);
    func(it, &proto, "then", 2, |it, this, args, _| {
        let p = this_promise(it, this)?;
        let t = it.new_promise();
        it.promise_then(&p, arg(args, 0), arg(args, 1), Some(t.clone()));
        Ok(Value::Obj(t))
    });
    func(it, &proto, "catch", 1, |it, this, args, _| {
        let f = it.get(this, &Key::str("then"))?;
        it.call(&f, this.clone(), &[Value::Undef, arg(args, 0)])
    });
    func(it, &proto, "finally", 1, |it, this, args, _| {
        let f = arg(args, 0);
        let ok = closure(it, |it, _, args, data| {
            let f = data_at(data, 0);
            if f.callable() {
                it.call(&f, Value::Undef, &[])?;
            }
            Ok(arg(args, 0))
        }, vec![f.clone()]);
        let err = closure(it, |it, _, args, data| {
            let f = data_at(data, 0);
            if f.callable() {
                it.call(&f, Value::Undef, &[])?;
            }
            Err(Ctl::Throw(arg(args, 0)))
        }, vec![f]);
        let then = it.get(this, &Key::str("then"))?;
        it.call(&then, this.clone(), &[ok, err])
    });
    func(it, &c, "resolve", 1, |it, _, args, _| Ok(Value::Obj(it.to_promise(arg(args, 0))?)));
    func(it, &c, "reject", 1, |it, _, args, _| {
        let p = it.new_promise();
        it.reject_promise(&p, arg(args, 0));
        Ok(Value::Obj(p))
    });
    func(it, &c, "withResolvers", 0, |it, _, _, _| {
        let p = it.new_promise();
        let (res, rej) = it.resolving_functions(&p);
        let o = it.new_obj();
        let ov = Value::Obj(o);
        it.put(&ov, Key::str("promise"), Value::Obj(p))?;
        it.put(&ov, Key::str("resolve"), res)?;
        it.put(&ov, Key::str("reject"), rej)?;
        Ok(ov)
    });
    // all, allSettled, any: data = [result, values, index, counter].
    fn combine(it: &mut Interp, args: &[Value], mode: u8) -> R<Value> {
        let items = it.iterate(&arg(args, 0))?;
        let result = it.new_promise();
        let values = it.new_array(vec![Value::Undef; items.len()]);
        let counter = it.new_array(vec![Value::Num(items.len() as f64)]);
        if items.is_empty() {
            if mode == 2 {
                let p = it.p.error.clone();
                let e = it.new_error(&p, "All promises were rejected");
                it.reject_promise(&result, e);
            } else {
                it.resolve_promise(&result, values)?;
            }
            return Ok(Value::Obj(result));
        }
        fn done_one(it: &mut Interp, data: &Value, v: Value, settle_ok: bool) -> R<()> {
            let result = data_at(data, 0);
            let values = data_at(data, 1);
            let i = data_at(data, 2);
            let counter = data_at(data, 3);
            it.put_v(&values, &i, v)?;
            let left = it.get_v(&counter, &Value::Num(0.0))?;
            let left = it.to_number(&left)? - 1.0;
            it.put_v(&counter, &Value::Num(0.0), Value::Num(left))?;
            if left <= 0.0 {
                if let Value::Obj(r) = result {
                    if settle_ok {
                        it.resolve_promise(&r, values)?;
                    } else {
                        let p = it.p.error.clone();
                        let e = it.new_error(&p, "All promises were rejected");
                        if let Value::Obj(eo) = &e {
                            eo.borrow_mut().props.insert(Key::str("errors"), Prop::hidden(values));
                        }
                        it.reject_promise(&r, e);
                    }
                }
            }
            Ok(())
        }
        fn settled(it: &mut Interp, status: &str, key: &str, v: Value) -> R<Value> {
            let o = it.new_obj();
            let ov = Value::Obj(o);
            it.put(&ov, Key::str("status"), Value::str(status))?;
            it.put(&ov, Key::str(key), v)?;
            Ok(ov)
        }
        for (i, item) in items.into_iter().enumerate() {
            let p = it.to_promise(item)?;
            let data = vec![Value::Obj(result.clone()), values.clone(), Value::Num(i as f64), counter.clone()];
            let (ok, err): (NativeFn, NativeFn) = match mode {
                0 => (
                    |it, _, args, data| {
                        done_one(it, data, arg(args, 0), true)?;
                        Ok(Value::Undef)
                    },
                    |it, _, args, data| {
                        if let Value::Obj(r) = data_at(data, 0) {
                            it.reject_promise(&r, arg(args, 0));
                        }
                        Ok(Value::Undef)
                    },
                ),
                1 => (
                    |it, _, args, data| {
                        let s = settled(it, "fulfilled", "value", arg(args, 0))?;
                        done_one(it, data, s, true)?;
                        Ok(Value::Undef)
                    },
                    |it, _, args, data| {
                        let s = settled(it, "rejected", "reason", arg(args, 0))?;
                        done_one(it, data, s, true)?;
                        Ok(Value::Undef)
                    },
                ),
                _ => (
                    |it, _, args, data| {
                        if let Value::Obj(r) = data_at(data, 0) {
                            it.resolve_promise(&r, arg(args, 0))?;
                        }
                        Ok(Value::Undef)
                    },
                    |it, _, args, data| {
                        done_one(it, data, arg(args, 0), false)?;
                        Ok(Value::Undef)
                    },
                ),
            };
            let okf = closure(it, ok, data.clone());
            let errf = closure(it, err, data);
            it.promise_then(&p, okf, errf, None);
        }
        Ok(Value::Obj(result))
    }
    func(it, &c, "all", 1, |it, _, args, _| combine(it, args, 0));
    func(it, &c, "allSettled", 1, |it, _, args, _| combine(it, args, 1));
    func(it, &c, "any", 1, |it, _, args, _| combine(it, args, 2));
    func(it, &c, "race", 1, |it, _, args, _| {
        let items = it.iterate(&arg(args, 0))?;
        let result = it.new_promise();
        let (res, rej) = it.resolving_functions(&result);
        for item in items {
            let p = it.to_promise(item)?;
            it.promise_then(&p, res.clone(), rej.clone(), None);
        }
        Ok(Value::Obj(result))
    });
}

// ---------------------------------------------------------------------------
// Map and Set

fn this_map<T>(it: &mut Interp, this: &Value, f: impl FnOnce(&mut Vec<(Value, Value)>) -> T) -> R<T> {
    if let Value::Obj(o) = this {
        if let Kind::Map(m) = &mut o.borrow_mut().kind {
            return Ok(f(m));
        }
    }
    Err(it.type_error("not a Map"))
}

fn this_set<T>(it: &mut Interp, this: &Value, f: impl FnOnce(&mut Vec<Value>) -> T) -> R<T> {
    if let Value::Obj(o) = this {
        if let Kind::Set(s) = &mut o.borrow_mut().kind {
            return Ok(f(s));
        }
    }
    Err(it.type_error("not a Set"))
}

/// `-0` keys are stored as `+0`.
fn norm(v: Value) -> Value {
    match v {
        Value::Num(n) => Value::Num(n + 0.0),
        v => v,
    }
}

pub fn make_iter(it: &mut Interp, items: Vec<Value>) -> Value {
    let p = it.p.iter.clone();
    Value::Obj(it.alloc(Some(p), Kind::Iter(items, 0)))
}

fn collections(it: &mut Interp) {
    let mp = it.p.map.clone();
    let mc = ctor(it, "Map", 0, |it, this, args, _| {
        if it.new_target.nullish() {
            return Err(it.type_error("Constructor Map requires 'new'"));
        }
        let Value::Obj(o) = this else { return Ok(Value::Undef) };
        o.borrow_mut().kind = Kind::Map(Vec::new());
        let init = arg(args, 0);
        if !init.nullish() {
            for e in it.iterate(&init)? {
                let k = norm(it.get_v(&e, &Value::Num(0.0))?);
                let v = it.get_v(&e, &Value::Num(1.0))?;
                this_map(it, this, |m| match m.iter_mut().find(|x| x.0.same_zero(&k)) {
                    Some(x) => x.1 = v,
                    None => m.push((k, v)),
                })?;
            }
        }
        Ok(this.clone())
    }, &mp);
    it.set_global("WeakMap", Value::Obj(mc));
    mp.borrow_mut().props.insert(Key::Sym(SYM_TO_STRING_TAG), Prop::hidden(Value::str("Map")));
    func(it, &mp, "get", 1, |it, this, args, _| {
        let k = arg(args, 0);
        this_map(it, this, |m| m.iter().find(|x| x.0.same_zero(&k)).map(|x| x.1.clone()).unwrap_or_default())
    });
    func(it, &mp, "set", 2, |it, this, args, _| {
        let k = norm(arg(args, 0));
        let v = arg(args, 1);
        this_map(it, this, |m| match m.iter_mut().find(|x| x.0.same_zero(&k)) {
            Some(x) => x.1 = v,
            None => m.push((k, v)),
        })?;
        Ok(this.clone())
    });
    func(it, &mp, "has", 1, |it, this, args, _| {
        let k = arg(args, 0);
        Ok(Value::Bool(this_map(it, this, |m| m.iter().any(|x| x.0.same_zero(&k)))?))
    });
    func(it, &mp, "delete", 1, |it, this, args, _| {
        let k = arg(args, 0);
        Ok(Value::Bool(this_map(it, this, |m| match m.iter().position(|x| x.0.same_zero(&k)) {
            Some(i) => {
                m.remove(i);
                true
            }
            None => false,
        })?))
    });
    func(it, &mp, "clear", 0, |it, this, _, _| {
        this_map(it, this, Vec::clear)?;
        Ok(Value::Undef)
    });
    it.accessor(&mp, "size", |it, this, _, _| Ok(Value::Num(this_map(it, this, |m| m.len())? as f64)), None, Value::Undef);
    func(it, &mp, "forEach", 1, |it, this, args, _| {
        let f = arg(args, 0);
        let entries = this_map(it, this, |m| m.clone())?;
        for (k, v) in entries {
            it.call(&f, arg(args, 1), &[v, k, this.clone()])?;
        }
        Ok(Value::Undef)
    });
    func(it, &mp, "keys", 0, |it, this, _, _| {
        let k = this_map(it, this, |m| m.iter().map(|x| x.0.clone()).collect())?;
        Ok(make_iter(it, k))
    });
    func(it, &mp, "values", 0, |it, this, _, _| {
        let v = this_map(it, this, |m| m.iter().map(|x| x.1.clone()).collect())?;
        Ok(make_iter(it, v))
    });
    let entries = it.native("entries", 0, |it, this, _, _| {
        let e = this_map(it, this, |m| m.clone())?;
        let pairs = e.into_iter().map(|(k, v)| it.new_array(vec![k, v])).collect();
        Ok(make_iter(it, pairs))
    }, Value::Undef, false);
    mp.borrow_mut().props.insert(Key::str("entries"), Prop::hidden(Value::Obj(entries.clone())));
    mp.borrow_mut().props.insert(Key::Sym(SYM_ITERATOR), Prop::hidden(Value::Obj(entries)));

    let sp = it.p.set.clone();
    let sc = ctor(it, "Set", 0, |it, this, args, _| {
        if it.new_target.nullish() {
            return Err(it.type_error("Constructor Set requires 'new'"));
        }
        let Value::Obj(o) = this else { return Ok(Value::Undef) };
        o.borrow_mut().kind = Kind::Set(Vec::new());
        let init = arg(args, 0);
        if !init.nullish() {
            for v in it.iterate(&init)? {
                let v = norm(v);
                this_set(it, this, |s| {
                    if !s.iter().any(|x| x.same_zero(&v)) {
                        s.push(v);
                    }
                })?;
            }
        }
        Ok(this.clone())
    }, &sp);
    it.set_global("WeakSet", Value::Obj(sc));
    sp.borrow_mut().props.insert(Key::Sym(SYM_TO_STRING_TAG), Prop::hidden(Value::str("Set")));
    func(it, &sp, "add", 1, |it, this, args, _| {
        let v = norm(arg(args, 0));
        this_set(it, this, |s| {
            if !s.iter().any(|x| x.same_zero(&v)) {
                s.push(v);
            }
        })?;
        Ok(this.clone())
    });
    func(it, &sp, "has", 1, |it, this, args, _| {
        let v = arg(args, 0);
        Ok(Value::Bool(this_set(it, this, |s| s.iter().any(|x| x.same_zero(&v)))?))
    });
    func(it, &sp, "delete", 1, |it, this, args, _| {
        let v = arg(args, 0);
        Ok(Value::Bool(this_set(it, this, |s| match s.iter().position(|x| x.same_zero(&v)) {
            Some(i) => {
                s.remove(i);
                true
            }
            None => false,
        })?))
    });
    func(it, &sp, "clear", 0, |it, this, _, _| {
        this_set(it, this, Vec::clear)?;
        Ok(Value::Undef)
    });
    it.accessor(&sp, "size", |it, this, _, _| Ok(Value::Num(this_set(it, this, |s| s.len())? as f64)), None, Value::Undef);
    func(it, &sp, "forEach", 1, |it, this, args, _| {
        let f = arg(args, 0);
        let items = this_set(it, this, |s| s.clone())?;
        for v in items {
            it.call(&f, arg(args, 1), &[v.clone(), v, this.clone()])?;
        }
        Ok(Value::Undef)
    });
    func(it, &sp, "entries", 0, |it, this, _, _| {
        let items = this_set(it, this, |s| s.clone())?;
        let pairs = items.into_iter().map(|v| it.new_array(vec![v.clone(), v])).collect();
        Ok(make_iter(it, pairs))
    });
    let values = it.native("values", 0, |it, this, _, _| {
        let items = this_set(it, this, |s| s.clone())?;
        Ok(make_iter(it, items))
    }, Value::Undef, false);
    for k in [Key::str("values"), Key::str("keys"), Key::Sym(SYM_ITERATOR)] {
        sp.borrow_mut().props.insert(k, Prop::hidden(Value::Obj(values.clone())));
    }
}

fn iterators(it: &mut Interp) {
    let ip = it.p.iter.clone();
    func(it, &ip, "next", 0, |it, this, _, _| {
        let next = match this {
            Value::Obj(o) => match &mut o.borrow_mut().kind {
                Kind::Iter(items, at) => {
                    let v = items.get(*at).cloned();
                    if v.is_some() {
                        *at += 1;
                    }
                    v
                }
                _ => return Err(it.type_error("not an iterator")),
            },
            _ => return Err(it.type_error("not an iterator")),
        };
        let r = it.new_obj();
        let rv = Value::Obj(r);
        it.put(&rv, Key::str("done"), Value::Bool(next.is_none()))?;
        it.put(&rv, Key::str("value"), next.unwrap_or_default())?;
        Ok(rv)
    });
    let me = it.native("[Symbol.iterator]", 0, |_, this, _, _| Ok(this.clone()), Value::Undef, false);
    ip.borrow_mut().props.insert(Key::Sym(SYM_ITERATOR), Prop::hidden(Value::Obj(me)));
    func(it, &ip, "toArray", 0, |it, this, _, _| {
        let v = it.iterate(this)?;
        Ok(it.new_array(v))
    });
}

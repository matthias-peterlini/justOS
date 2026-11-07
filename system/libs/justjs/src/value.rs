//! Values, objects and their properties.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Ref, RefCell, RefMut};
use core::fmt::Write;

use crate::ast::{FuncDef, Name};
use crate::interp::{Interp, ScopeRef, R};

#[derive(Clone, Default)]
pub enum Value {
    #[default]
    Undef,
    Null,
    Bool(bool),
    Num(f64),
    Str(Rc<str>),
    Sym(u32),
    Obj(ObjRef),
}

#[derive(Clone)]
pub struct ObjRef(pub Rc<RefCell<Object>>);

impl ObjRef {
    pub fn borrow(&self) -> Ref<'_, Object> {
        self.0.borrow()
    }

    pub fn borrow_mut(&self) -> RefMut<'_, Object> {
        self.0.borrow_mut()
    }

    pub fn same(&self, o: &ObjRef) -> bool {
        Rc::ptr_eq(&self.0, &o.0)
    }

    pub fn id(&self) -> usize {
        Rc::as_ptr(&self.0) as *const u8 as usize
    }
}

/// A property key.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Key {
    Str(Rc<str>),
    Sym(u32),
}

impl Key {
    pub fn str(s: &str) -> Key {
        Key::Str(Rc::from(s))
    }

    /// The key as an array index.
    pub fn index(&self) -> Option<usize> {
        match self {
            Key::Str(s) => str_index(s),
            Key::Sym(_) => None,
        }
    }

    pub fn is(&self, s: &str) -> bool {
        matches!(self, Key::Str(k) if &**k == s)
    }
}

/// `"12"` → 12, but not `"012"` or `"1.5"`.
pub fn str_index(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    if b.is_empty() || b.len() > 9 || (b.len() > 1 && b[0] == b'0') || !b.iter().all(u8::is_ascii_digit) {
        return None;
    }
    s.parse().ok()
}

#[derive(Clone)]
pub enum Slot {
    Data(Value),
    /// Getter and setter (`Undef` when missing).
    Acc(Value, Value),
}

#[derive(Clone)]
pub struct Prop {
    pub slot: Slot,
    pub enumerable: bool,
    pub writable: bool,
}

impl Prop {
    pub fn data(v: Value) -> Prop {
        Prop { slot: Slot::Data(v), enumerable: true, writable: true }
    }

    /// What builtins and methods are: not listed by `for…in`.
    pub fn hidden(v: Value) -> Prop {
        Prop { slot: Slot::Data(v), enumerable: false, writable: true }
    }
}

/// Properties in the order they were added, with an index once there are
/// many.
#[derive(Default)]
pub struct Props {
    keys: Vec<Key>,
    vals: Vec<Prop>,
    index: Option<BTreeMap<Key, usize>>,
}

const INDEX_FROM: usize = 12;

impl Props {
    pub fn find(&self, k: &Key) -> Option<usize> {
        match &self.index {
            Some(ix) => ix.get(k).copied(),
            None => self.keys.iter().position(|x| match (x, k) {
                (Key::Str(a), Key::Str(b)) => Rc::ptr_eq(a, b) || a == b,
                _ => x == k,
            }),
        }
    }

    pub fn get(&self, k: &Key) -> Option<&Prop> {
        self.find(k).map(|i| &self.vals[i])
    }

    pub fn get_mut(&mut self, k: &Key) -> Option<&mut Prop> {
        self.find(k).map(|i| &mut self.vals[i])
    }

    pub fn insert(&mut self, k: Key, p: Prop) {
        if let Some(i) = self.find(&k) {
            self.vals[i] = p;
            return;
        }
        if let Some(ix) = &mut self.index {
            ix.insert(k.clone(), self.keys.len());
        }
        self.keys.push(k);
        self.vals.push(p);
        if self.index.is_none() && self.keys.len() > INDEX_FROM {
            self.index = Some(self.keys.iter().cloned().enumerate().map(|(i, k)| (k, i)).collect());
        }
    }

    pub fn remove(&mut self, k: &Key) -> Option<Prop> {
        let i = self.find(k)?;
        self.keys.remove(i);
        let p = self.vals.remove(i);
        if self.index.is_some() {
            self.index = Some(self.keys.iter().cloned().enumerate().map(|(i, k)| (k, i)).collect());
        }
        Some(p)
    }

    pub fn keys(&self) -> &[Key] {
        &self.keys
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Key, &Prop)> {
        self.keys.iter().zip(self.vals.iter())
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub fn clear(&mut self) {
        self.keys.clear();
        self.vals.clear();
        self.index = None;
    }
}

pub type NativeFn = fn(&mut Interp, &Value, &[Value], &Value) -> R<Value>;

pub struct Native {
    pub f: NativeFn,
    /// Handed to `f` on every call.
    pub data: Value,
    pub name: Rc<str>,
    pub arity: u32,
    /// Whether `new` works on it.
    pub ctor: bool,
}

/// A function written in JavaScript, with what it closed over.
pub struct Closure {
    pub def: Rc<FuncDef>,
    pub scope: ScopeRef,
    /// The object `super` starts from (methods only).
    pub home: Option<ObjRef>,
    /// For class constructors.
    pub class: Option<Rc<ClassRt>>,
}

/// What a class constructor needs when it makes an instance.
pub struct ClassRt {
    pub def: Rc<crate::ast::ClassDef>,
    pub derived: bool,
    /// Instance fields: key and member index.
    pub fields: Vec<(Key, usize)>,
    pub scope: ScopeRef,
    pub proto: ObjRef,
}

pub struct Bound {
    pub target: Value,
    pub this: Value,
    pub args: Vec<Value>,
}

#[derive(Clone)]
pub enum PromiseState {
    Pending,
    Fulfilled(Value),
    Rejected(Value),
}

pub struct Reaction {
    pub on_ok: Value,
    pub on_err: Value,
    /// The promise `then` returned.
    pub target: Option<ObjRef>,
}

pub struct Promise {
    pub state: PromiseState,
    pub reactions: Vec<Reaction>,
    pub handled: bool,
}

pub enum Kind {
    Plain,
    Array(Vec<Value>),
    Func(Rc<Closure>),
    Native(Rc<Native>),
    Bound(Rc<Bound>),
    Error,
    Date(f64),
    RegExp(Rc<crate::regex::Regex>),
    Map(Vec<(Value, Value)>),
    Set(Vec<Value>),
    Promise(Box<Promise>),
    /// A boxed primitive (`new String("a")`).
    Prim(Value),
    /// What `keys()`, `values()` and `entries()` return.
    Iter(Vec<Value>, usize),
    /// An object the embedder answers for.
    Host(u32, usize),
}

pub struct Object {
    pub props: Props,
    pub proto: Option<ObjRef>,
    pub kind: Kind,
    pub frozen: bool,
}

impl Object {
    pub fn new(proto: Option<ObjRef>, kind: Kind) -> Object {
        Object { props: Props::default(), proto, kind, frozen: false }
    }

    pub fn callable(&self) -> bool {
        matches!(self.kind, Kind::Func(_) | Kind::Native(_) | Kind::Bound(_))
    }
}

impl Value {
    pub fn str(s: &str) -> Value {
        Value::Str(Rc::from(s))
    }

    pub fn truthy(&self) -> bool {
        match self {
            Value::Undef | Value::Null => false,
            Value::Bool(b) => *b,
            Value::Num(n) => *n != 0.0 && !n.is_nan(),
            Value::Str(s) => !s.is_empty(),
            Value::Sym(_) | Value::Obj(_) => true,
        }
    }

    pub fn nullish(&self) -> bool {
        matches!(self, Value::Undef | Value::Null)
    }

    pub fn obj(&self) -> Option<&ObjRef> {
        match self {
            Value::Obj(o) => Some(o),
            _ => None,
        }
    }

    pub fn callable(&self) -> bool {
        matches!(self, Value::Obj(o) if o.borrow().callable())
    }

    pub fn type_of(&self) -> &'static str {
        match self {
            Value::Undef => "undefined",
            Value::Null => "object",
            Value::Bool(_) => "boolean",
            Value::Num(_) => "number",
            Value::Str(_) => "string",
            Value::Sym(_) => "symbol",
            Value::Obj(o) => {
                if o.borrow().callable() {
                    "function"
                } else {
                    "object"
                }
            }
        }
    }

    /// `===`.
    pub fn strict_eq(&self, o: &Value) -> bool {
        match (self, o) {
            (Value::Undef, Value::Undef) | (Value::Null, Value::Null) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Num(a), Value::Num(b)) => a == b,
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::Sym(a), Value::Sym(b)) => a == b,
            (Value::Obj(a), Value::Obj(b)) => a.same(b),
            _ => false,
        }
    }

    /// SameValueZero, what `includes`, `Map` and `Set` use.
    pub fn same_zero(&self, o: &Value) -> bool {
        match (self, o) {
            (Value::Num(a), Value::Num(b)) if a.is_nan() && b.is_nan() => true,
            _ => self.strict_eq(o),
        }
    }
}

/// A number the way JavaScript writes it.
pub fn num_to_string(n: f64) -> String {
    if n.is_nan() {
        return String::from("NaN");
    }
    if n == 0.0 {
        return String::from("0");
    }
    if n.is_infinite() {
        return String::from(if n > 0.0 { "Infinity" } else { "-Infinity" });
    }
    if n.abs() < 1e15 && n == (n as i64) as f64 {
        return alloc::format!("{}", n as i64);
    }
    let mut out = String::new();
    if n < 0.0 {
        out.push('-');
    }
    // The shortest digits that read back the same, and the exponent.
    let e = alloc::format!("{:e}", n.abs());
    let (mant, exp) = e.split_once('e').unwrap_or((&e, "0"));
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let point = exp.parse::<i32>().unwrap_or(0) + 1;
    if k <= point && point <= 21 {
        out.push_str(&digits);
        for _ in 0..point - k {
            out.push('0');
        }
    } else if 0 < point && point <= 21 {
        out.push_str(&digits[..point as usize]);
        out.push('.');
        out.push_str(&digits[point as usize..]);
    } else if -6 < point && point <= 0 {
        out.push_str("0.");
        for _ in 0..-point {
            out.push('0');
        }
        out.push_str(&digits);
    } else {
        out.push_str(&digits[..1]);
        if k > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        let _ = write!(out, "e{}{}", if point > 0 { "+" } else { "-" }, (point - 1).abs());
    }
    out
}

/// A string read as a number (`Number("…")`).
pub fn str_to_num(s: &str) -> f64 {
    let t = s.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
    if t.is_empty() {
        return 0.0;
    }
    let (neg, body) = match t.as_bytes()[0] {
        b'-' => (true, &t[1..]),
        b'+' => (false, &t[1..]),
        _ => (false, t),
    };
    let radix = match body.get(..2) {
        Some("0x" | "0X") => 16,
        Some("0o" | "0O") => 8,
        Some("0b" | "0B") => 2,
        _ => 10,
    };
    if radix != 10 {
        if neg || body.len() == 2 {
            return f64::NAN;
        }
        let mut v = 0f64;
        for c in body[2..].chars() {
            let Some(d) = c.to_digit(radix) else { return f64::NAN };
            v = v * f64::from(radix) + f64::from(d);
        }
        return v;
    }
    let v = if body == "Infinity" {
        f64::INFINITY
    } else if body.bytes().all(|c| c.is_ascii_digit() || matches!(c, b'.' | b'e' | b'E' | b'+' | b'-')) && body.bytes().any(|c| c.is_ascii_digit()) {
        body.parse::<f64>().unwrap_or(f64::NAN)
    } else {
        f64::NAN
    };
    if neg {
        -v
    } else {
        v
    }
}

/// ToInt32.
pub fn to_i32(n: f64) -> i32 {
    if !n.is_finite() {
        return 0;
    }
    let t = justc_core::float::trunc(n);
    let m = t - 4294967296.0 * justc_core::float::floor(t / 4294967296.0);
    m as u32 as i32
}

pub fn to_u32(n: f64) -> u32 {
    to_i32(n) as u32
}

/// ToIntegerOrInfinity.
pub fn to_int(n: f64) -> f64 {
    if n.is_nan() {
        0.0
    } else {
        justc_core::float::trunc(n)
    }
}

/// A relative index (`slice(-2)`) clamped to `0..=len`.
pub fn rel_index(n: f64, len: usize) -> usize {
    let n = to_int(n);
    let len_f = len as f64;
    if n < 0.0 {
        (len_f + n).max(0.0) as usize
    } else {
        n.min(len_f) as usize
    }
}

pub fn name_key(n: &Name) -> Key {
    Key::Str(n.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        assert_eq!(num_to_string(1.0), "1");
        assert_eq!(num_to_string(-0.5), "-0.5");
        assert_eq!(num_to_string(0.1 + 0.2), "0.30000000000000004");
        assert_eq!(num_to_string(1e21), "1e+21");
        assert_eq!(num_to_string(1e-7), "1e-7");
        assert_eq!(num_to_string(123456789012345680000.0), "123456789012345680000");
        assert_eq!(num_to_string(0.000001), "0.000001");
        assert_eq!(num_to_string(1.5e300), "1.5e+300");
        assert_eq!(str_to_num(" 42 "), 42.0);
        assert_eq!(str_to_num("0x10"), 16.0);
        assert!(str_to_num("12px").is_nan());
        assert!(str_to_num("inf").is_nan());
        assert_eq!(to_i32(4294967297.0), 1);
        assert_eq!(to_i32(-1.0), -1);
    }
}

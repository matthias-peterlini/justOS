//! Runtime values. Heap objects are reference counted (`Rc`), giving
//! deterministic, pause-free memory management. Reference cycles (a list
//! containing itself) are not collected; closures capture values, not
//! variables, so such cycles can only be created explicitly.

use crate::prelude::*;
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use justc_core::bytecode::TypeDef;
use core::cell::RefCell;
use core::fmt;

pub type ListRef = Rc<RefCell<Vec<Value>>>;
pub type MapRef = Rc<RefCell<BTreeMap<MapKey, Value>>>;

/// Map keys: bools, ints and strings, ordered by type then value.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MapKey {
    Bool(bool),
    Int(i64),
    Str(Rc<str>),
}

impl MapKey {
    pub fn from_value(v: &Value) -> Result<MapKey, String> {
        match v {
            Value::Bool(b) => Ok(MapKey::Bool(*b)),
            Value::Int(i) => Ok(MapKey::Int(*i)),
            Value::Str(s) => Ok(MapKey::Str(Rc::clone(s))),
            other => Err(format!("map keys must be strings, ints or bools, found {}", other.type_name())),
        }
    }

    pub fn to_value(&self) -> Value {
        match self {
            MapKey::Bool(b) => Value::Bool(*b),
            MapKey::Int(i) => Value::Int(*i),
            MapKey::Str(s) => Value::Str(Rc::clone(s)),
        }
    }
}

/// An instance of a `struct`. Fields are mutable and shared by reference.
#[derive(Debug)]
pub struct StructObj {
    pub ty: Rc<TypeDef>,
    pub type_index: u16,
    pub fields: RefCell<Vec<Value>>,
}

/// An enum variant with its values (immutable).
#[derive(Debug)]
pub struct VariantObj {
    pub ty: Rc<TypeDef>,
    pub type_index: u16,
    pub tag: u16,
    pub values: Vec<Value>,
}

impl VariantObj {
    pub fn name(&self) -> &str {
        self.ty.variants().get(self.tag as usize).map_or("?", |(n, _)| n)
    }
}

/// A nested function or lambda with the values it captured.
#[derive(Debug)]
pub struct Closure {
    pub func: u16,
    pub captures: Vec<Value>,
}

#[derive(Clone, Debug, Default)]
pub enum Value {
    #[default]
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Rc<str>),
    List(ListRef),
    Func(u16),
    Builtin(u8),
    /// A callable with pre-bound arguments (event handlers).
    Bound(Rc<Bound>),
    /// Half-open integer range `start..end`.
    Range(i64, i64),
    Iter(Rc<RefCell<Iter>>),
    Map(MapRef),
    Struct(Rc<StructObj>),
    Variant(Rc<VariantObj>),
    Closure(Rc<Closure>),
}

#[derive(Debug)]
pub struct Bound {
    pub callee: Value,
    pub args: Vec<Value>,
}

#[derive(Debug)]
pub enum Iter {
    Range { next: i64, end: i64 },
    List { list: ListRef, index: usize },
    Str { text: Rc<str>, offset: usize },
}

impl Iterator for Iter {
    type Item = Value;

    fn next(&mut self) -> Option<Value> {
        match self {
            Iter::Range { next, end } => (*next < *end).then(|| {
                *next += 1;
                Value::Int(*next - 1)
            }),
            Iter::List { list, index } => {
                let item = list.borrow().get(*index).cloned();
                *index += 1;
                item
            }
            Iter::Str { text, offset } => {
                let c = text[*offset..].chars().next()?;
                *offset += c.len_utf8();
                Some(Value::Str(Rc::from(c.encode_utf8(&mut [0; 4]) as &str)))
            }
        }
    }
}

impl Value {
    pub fn str(s: &str) -> Value {
        Value::Str(Rc::from(s))
    }

    pub fn list(items: Vec<Value>) -> Value {
        Value::List(Rc::new(RefCell::new(items)))
    }

    pub fn map(entries: BTreeMap<MapKey, Value>) -> Value {
        Value::Map(Rc::new(RefCell::new(entries)))
    }

    /// The type's name: `int`, `list`, ... or a struct/enum name.
    pub fn type_name(&self) -> &str {
        match self {
            Value::Struct(s) => &s.ty.name,
            Value::Variant(v) => &v.ty.name,
            other => other.kind_name(),
        }
    }

    /// Like [`type_name`](Self::type_name), with `struct` / `enum` for user types.
    pub fn kind_name(&self) -> &'static str {
        match self {
            Value::Nil => "nil",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Str(_) => "string",
            Value::List(_) => "list",
            Value::Func(_) | Value::Builtin(_) | Value::Bound(_) | Value::Closure(_) => "function",
            Value::Range(..) => "range",
            Value::Iter(_) => "iterator",
            Value::Map(_) => "map",
            Value::Struct(_) => "struct",
            Value::Variant(_) => "enum",
        }
    }

    pub fn is_callable(&self) -> bool {
        matches!(self, Value::Func(_) | Value::Builtin(_) | Value::Bound(_) | Value::Closure(_))
    }

    /// Structural equality; ints and floats compare numerically.
    pub fn equals(&self, other: &Value) -> bool {
        self.equals_at(other, 0)
    }

    fn equals_at(&self, other: &Value, depth: usize) -> bool {
        // Deeply nested (or cyclic) values compare unequal instead of overflowing.
        if depth > 256 {
            return false;
        }
        let eq = |a: &Value, b: &Value| a.equals_at(b, depth + 1);
        match (self, other) {
            (Value::Nil, Value::Nil) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Int(a), Value::Float(b)) | (Value::Float(b), Value::Int(a)) => (*a as f64) == *b,
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::List(a), Value::List(b)) => {
                Rc::ptr_eq(a, b) || {
                    let (a, b) = (a.borrow(), b.borrow());
                    a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| eq(x, y))
                }
            }
            (Value::Map(a), Value::Map(b)) => {
                Rc::ptr_eq(a, b) || {
                    let (a, b) = (a.borrow(), b.borrow());
                    a.len() == b.len() && a.iter().zip(b.iter()).all(|((ka, va), (kb, vb))| ka == kb && eq(va, vb))
                }
            }
            (Value::Struct(a), Value::Struct(b)) => {
                Rc::ptr_eq(a, b) || {
                    a.type_index == b.type_index && {
                        let (a, b) = (a.fields.borrow(), b.fields.borrow());
                        a.iter().zip(b.iter()).all(|(x, y)| eq(x, y))
                    }
                }
            }
            (Value::Variant(a), Value::Variant(b)) => {
                Rc::ptr_eq(a, b)
                    || (a.type_index == b.type_index && a.tag == b.tag && a.values.iter().zip(&b.values).all(|(x, y)| eq(x, y)))
            }
            (Value::Closure(a), Value::Closure(b)) => Rc::ptr_eq(a, b),
            (Value::Func(a), Value::Func(b)) => a == b,
            (Value::Builtin(a), Value::Builtin(b)) => a == b,
            (Value::Range(a, b), Value::Range(c, d)) => a == c && b == d,
            _ => false,
        }
    }

    /// Ordering for sorting: numbers, strings, bools, and lists of them
    /// (element by element). NaN sorts after every other number.
    pub fn order(&self, other: &Value) -> Result<core::cmp::Ordering, String> {
        use core::cmp::Ordering;
        let float = |a: f64, b: f64| a.partial_cmp(&b).unwrap_or_else(|| a.is_nan().cmp(&b.is_nan()));
        Ok(match (self, other) {
            (Value::Int(a), Value::Int(b)) => a.cmp(b),
            (Value::Int(a), Value::Float(b)) => float(*a as f64, *b),
            (Value::Float(a), Value::Int(b)) => float(*a, *b as f64),
            (Value::Float(a), Value::Float(b)) => float(*a, *b),
            (Value::Str(a), Value::Str(b)) => a.cmp(b),
            (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
            (Value::List(a), Value::List(b)) => {
                if Rc::ptr_eq(a, b) {
                    return Ok(Ordering::Equal);
                }
                let (a, b) = (a.borrow(), b.borrow());
                for (x, y) in a.iter().zip(b.iter()) {
                    match x.order(y)? {
                        Ordering::Equal => {}
                        o => return Ok(o),
                    }
                }
                a.len().cmp(&b.len())
            }
            (a, b) => return Err(format!("cannot compare {} with {}", a.type_name(), b.type_name())),
        })
    }

    fn fmt_repr(&self, f: &mut fmt::Formatter<'_>, depth: usize) -> fmt::Result {
        let compound = matches!(self, Value::List(_) | Value::Map(_) | Value::Struct(_) | Value::Variant(_));
        if compound && depth > 32 {
            return f.write_str("...");
        }
        let list = |f: &mut fmt::Formatter<'_>, items: &[Value]| {
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    f.write_str(", ")?;
                }
                item.fmt_repr(f, depth + 1)?;
            }
            Ok(())
        };
        match self {
            Value::Str(s) if depth > 0 => write!(f, "{:?}", &**s),
            Value::List(items) => {
                f.write_str("[")?;
                list(f, &items.borrow())?;
                f.write_str("]")
            }
            Value::Map(entries) => {
                f.write_str("{")?;
                for (i, (k, v)) in entries.borrow().iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    k.to_value().fmt_repr(f, depth + 1)?;
                    f.write_str(": ")?;
                    v.fmt_repr(f, depth + 1)?;
                }
                f.write_str("}")
            }
            Value::Struct(s) => {
                write!(f, "{}(", s.ty.name)?;
                for (i, (name, v)) in s.ty.fields().iter().zip(s.fields.borrow().iter()).enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{name}: ")?;
                    v.fmt_repr(f, depth + 1)?;
                }
                f.write_str(")")
            }
            Value::Variant(v) => {
                f.write_str(v.name())?;
                if !v.values.is_empty() {
                    f.write_str("(")?;
                    list(f, &v.values)?;
                    f.write_str(")")?;
                }
                Ok(())
            }
            other => fmt::Display::fmt(other, f),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Nil => f.write_str("nil"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Int(v) => write!(f, "{v}"),
            Value::Float(v) => {
                if v.is_finite() && v.fract() == 0.0 && v.abs() < 1e16 {
                    write!(f, "{v:.1}")
                } else {
                    write!(f, "{v}")
                }
            }
            Value::Str(s) => f.write_str(s),
            Value::List(_) | Value::Map(_) | Value::Struct(_) | Value::Variant(_) => self.fmt_repr(f, 0),
            Value::Closure(c) => write!(f, "<fn #{}>", c.func),
            Value::Func(i) => write!(f, "<fn #{i}>"),
            Value::Builtin(i) => write!(f, "<builtin {}>", justc_core::bytecode::BUILTINS.get(*i as usize).map_or("?", |b| b.name)),
            Value::Bound(_) => f.write_str("<bound fn>"),
            Value::Range(a, b) => write!(f, "{a}..{b}"),
            Value::Iter(_) => f.write_str("<iterator>"),
        }
    }
}

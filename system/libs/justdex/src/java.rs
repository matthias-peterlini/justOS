//! The Java library, written in Rust: `java.lang`, the collections of
//! `java.util`, `Scanner`, `Random`, `BufferedReader`.
//!
//! A call reaches here when the program has no method of that name: by the
//! class the call names (static methods and constructors) or by the state
//! the object carries ([`Native`]).

use crate::text::{self, Arg, Regex};
use crate::{java_name, new_ref, Coll, Inst, JResult, Map, Native, Obj, Ref, Throw, Val, Vm};
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use arcc_core::float;
use core::cmp::Ordering;

const OBJ: &str = "Ljava/lang/Object;";

/// The direct supertypes of a library class.
pub(crate) fn supertypes(class: &str) -> &'static [&'static str] {
    match class {
        "Ljava/lang/String;" => &["Ljava/lang/CharSequence;", "Ljava/lang/Comparable;"],
        "Ljava/lang/StringBuilder;" | "Ljava/lang/StringBuffer;" => &["Ljava/lang/CharSequence;"],
        "Ljava/lang/Integer;" | "Ljava/lang/Long;" | "Ljava/lang/Double;" | "Ljava/lang/Float;" | "Ljava/lang/Short;" | "Ljava/lang/Byte;" => {
            &["Ljava/lang/Number;", "Ljava/lang/Comparable;"]
        }
        "Ljava/lang/Boolean;" | "Ljava/lang/Character;" | "Ljava/lang/Enum;" => &["Ljava/lang/Comparable;"],
        "Ljava/util/ArrayList;" | "Ljava/util/Vector;" => &["Ljava/util/List;", "Ljava/util/RandomAccess;"],
        "Ljava/util/LinkedList;" => &["Ljava/util/List;", "Ljava/util/Deque;"],
        "Ljava/util/Stack;" => &["Ljava/util/Vector;"],
        "Ljava/util/ArrayDeque;" | "Ljava/util/concurrent/LinkedBlockingDeque;" => &["Ljava/util/Deque;"],
        "Ljava/util/PriorityQueue;" | "Ljava/util/Deque;" => &["Ljava/util/Queue;"],
        "Ljava/util/Queue;" | "Ljava/util/List;" | "Ljava/util/Set;" => &["Ljava/util/Collection;"],
        "Ljava/util/Collection;" => &["Ljava/lang/Iterable;"],
        "Ljava/util/HashSet;" | "Ljava/util/LinkedHashSet;" => &["Ljava/util/Set;"],
        "Ljava/util/TreeSet;" => &["Ljava/util/SortedSet;"],
        "Ljava/util/SortedSet;" => &["Ljava/util/Set;"],
        "Ljava/util/HashMap;" | "Ljava/util/LinkedHashMap;" | "Ljava/util/Hashtable;" => &["Ljava/util/Map;"],
        "Ljava/util/TreeMap;" => &["Ljava/util/SortedMap;"],
        "Ljava/util/SortedMap;" => &["Ljava/util/Map;"],
        "Ljava/lang/Thread;" => &["Ljava/lang/Runnable;"],
        "Ljava/lang/Exception;" | "Ljava/lang/Error;" => &["Ljava/lang/Throwable;"],
        "Ljava/lang/RuntimeException;"
        | "Ljava/io/IOException;"
        | "Ljava/lang/InterruptedException;"
        | "Ljava/lang/CloneNotSupportedException;"
        | "Ljava/lang/ReflectiveOperationException;" => &["Ljava/lang/Exception;"],
        "Ljava/lang/ClassNotFoundException;" => &["Ljava/lang/ReflectiveOperationException;"],
        "Ljava/lang/IllegalArgumentException;"
        | "Ljava/lang/IllegalStateException;"
        | "Ljava/lang/ArithmeticException;"
        | "Ljava/lang/NullPointerException;"
        | "Ljava/lang/ClassCastException;"
        | "Ljava/lang/IndexOutOfBoundsException;"
        | "Ljava/lang/UnsupportedOperationException;"
        | "Ljava/lang/NegativeArraySizeException;"
        | "Ljava/lang/ArrayStoreException;"
        | "Ljava/lang/SecurityException;"
        | "Ljava/util/NoSuchElementException;"
        | "Ljava/util/ConcurrentModificationException;" => &["Ljava/lang/RuntimeException;"],
        "Ljava/lang/NumberFormatException;" => &["Ljava/lang/IllegalArgumentException;"],
        "Ljava/util/InputMismatchException;" => &["Ljava/util/NoSuchElementException;"],
        "Ljava/lang/ArrayIndexOutOfBoundsException;" | "Ljava/lang/StringIndexOutOfBoundsException;" => &["Ljava/lang/IndexOutOfBoundsException;"],
        "Ljava/io/FileNotFoundException;" | "Ljava/io/EOFException;" => &["Ljava/io/IOException;"],
        "Ljava/lang/StackOverflowError;" | "Ljava/lang/OutOfMemoryError;" => &["Ljava/lang/VirtualMachineError;"],
        "Ljava/lang/VirtualMachineError;" | "Ljava/lang/AssertionError;" | "Ljava/lang/LinkageError;" => &["Ljava/lang/Error;"],
        "Ljava/lang/NoSuchMethodError;" | "Ljava/lang/NoSuchFieldError;" | "Ljava/lang/AbstractMethodError;" => {
            &["Ljava/lang/IncompatibleClassChangeError;"]
        }
        "Ljava/lang/IncompatibleClassChangeError;" | "Ljava/lang/VerifyError;" | "Ljava/lang/ExceptionInInitializerError;" => &["Ljava/lang/LinkageError;"],
        _ => crate::android::supertypes(class),
    }
}

fn box_class(kind: u8) -> &'static str {
    match kind {
        b'I' => "Ljava/lang/Integer;",
        b'J' => "Ljava/lang/Long;",
        b'D' => "Ljava/lang/Double;",
        b'F' => "Ljava/lang/Float;",
        b'Z' => "Ljava/lang/Boolean;",
        b'C' => "Ljava/lang/Character;",
        b'S' => "Ljava/lang/Short;",
        _ => "Ljava/lang/Byte;",
    }
}

fn box_kind(class: &str) -> Option<u8> {
    Some(match class {
        "Ljava/lang/Integer;" => b'I',
        "Ljava/lang/Long;" => b'J',
        "Ljava/lang/Double;" => b'D',
        "Ljava/lang/Float;" => b'F',
        "Ljava/lang/Boolean;" => b'Z',
        "Ljava/lang/Character;" => b'C',
        "Ljava/lang/Short;" => b'S',
        "Ljava/lang/Byte;" => b'B',
        _ => return None,
    })
}

/// A boxed or primitive value as a number.
fn num(kind: u8, bits: u64) -> (i64, f64) {
    match kind {
        b'J' => (bits as i64, bits as i64 as f64),
        b'D' => (f64::from_bits(bits) as i64, f64::from_bits(bits)),
        b'F' => {
            let f = f32::from_bits(bits as u32) as f64;
            (f as i64, f)
        }
        b'C' => (bits as u16 as i64, bits as u16 as f64),
        _ => (bits as u32 as i32 as i64, bits as u32 as i32 as f64),
    }
}

/// A primitive as Java writes it.
pub(crate) fn prim_string(kind: u8, bits: u64) -> String {
    match kind {
        b'J' => (bits as i64).to_string(),
        b'D' => text::double(f64::from_bits(bits)),
        b'F' => text::float(f32::from_bits(bits as u32)),
        b'Z' => (bits as u32 != 0).to_string(),
        b'C' => text::char_string(bits as u16),
        _ => (bits as u32 as i32).to_string(),
    }
}

fn identity(o: &Ref) -> i32 {
    ((Rc::as_ptr(o) as *const u8 as usize >> 4) as u32 & 0x7FFF_FFFF) as i32
}

fn is_list(class: &str) -> bool {
    matches!(
        class,
        "Ljava/util/ArrayList;"
            | "Ljava/util/LinkedList;"
            | "Ljava/util/Vector;"
            | "Ljava/util/Stack;"
            | "Ljava/util/ArrayDeque;"
            | "Ljava/util/PriorityQueue;"
            | "Ljava/util/AbstractList;"
            | "Ljava/util/concurrent/CopyOnWriteArrayList;"
    )
}

/// What kind of object a library call is about.
enum Kind {
    Str,
    Arr,
    Plain,
}

impl Vm {
    pub(crate) fn new_native(&mut self, base: &str) -> Native {
        match base {
            "Ljava/lang/StringBuilder;" | "Ljava/lang/StringBuffer;" => Native::Sb(String::new()),
            c if is_list(c) => Native::Coll(Coll { sorted: c == "Ljava/util/PriorityQueue;", ..Coll::default() }),
            "Ljava/util/HashSet;" | "Ljava/util/LinkedHashSet;" => Native::Coll(Coll { set: true, ..Coll::default() }),
            "Ljava/util/TreeSet;" => Native::Coll(Coll { set: true, sorted: true, ..Coll::default() }),
            "Ljava/util/HashMap;" | "Ljava/util/LinkedHashMap;" | "Ljava/util/Hashtable;" | "Ljava/util/concurrent/ConcurrentHashMap;" => Native::Map(Map::default()),
            "Ljava/util/TreeMap;" => Native::Map(Map { sorted: true, ..Map::default() }),
            "Ljava/util/Random;" => Native::Random(0),
            "Ljava/util/Scanner;" => Native::Scanner { line: None, text: None },
            "Ljava/io/BufferedReader;" | "Ljava/io/InputStreamReader;" => Native::Reader,
            "Ljava/lang/Thread;" => Native::Thread(Val::NULL),
            c => match box_kind(c) {
                Some(k) => Native::Box(k, 0),
                None => crate::android::new_native(c).unwrap_or(Native::None),
            },
        }
    }

    pub(crate) fn boxed(&mut self, kind: u8, bits: u64) -> Val {
        Val::R(new_ref(Obj::Inst(Inst { class: Rc::from(box_class(kind)), fields: BTreeMap::new(), native: Native::Box(kind, bits) })))
    }

    fn new_coll(&mut self, class: &str, items: Vec<Val>) -> Val {
        let set = class.ends_with("Set;");
        Val::R(new_ref(Obj::Inst(Inst {
            class: Rc::from(class),
            fields: BTreeMap::new(),
            native: Native::Coll(Coll { items, set, sorted: class == "Ljava/util/TreeSet;" }),
        })))
    }

    fn std_stream(&mut self, which: u8) -> Val {
        let slot = match which {
            0 => &mut self.inp,
            1 => &mut self.out,
            _ => &mut self.err,
        };
        let o = slot.get_or_insert_with(|| {
            let (class, native) = match which {
                0 => ("Ljava/io/InputStream;", Native::In),
                1 => ("Ljava/io/PrintStream;", Native::Out(false)),
                _ => ("Ljava/io/PrintStream;", Native::Out(true)),
            };
            new_ref(Obj::Inst(Inst { class: Rc::from(class), fields: BTreeMap::new(), native }))
        });
        Val::R(o.clone())
    }

    /// A static field of a library class.
    pub(crate) fn static_field(&mut self, class: &str, name: &str) -> JResult<Val> {
        Ok(match (class, name) {
            ("Ljava/lang/System;", "out") => self.std_stream(1),
            ("Ljava/lang/System;", "err") => self.std_stream(2),
            ("Ljava/lang/System;", "in") => self.std_stream(0),
            (c, "TYPE") if box_kind(c).is_some() => {
                let k = [box_kind(c).unwrap_or(b'I')];
                self.class_object(core::str::from_utf8(&k).unwrap_or("I"))
            }
            ("Ljava/lang/Void;", "TYPE") => self.class_object("V"),
            ("Ljava/lang/Boolean;", "TRUE") => self.boxed(b'Z', 1),
            ("Ljava/lang/Boolean;", "FALSE") => self.boxed(b'Z', 0),
            ("Ljava/util/Locale;", _) | ("Ljava/lang/String;", "CASE_INSENSITIVE_ORDER") => {
                Val::R(new_ref(Obj::Inst(Inst { class: Rc::from(class), fields: BTreeMap::new(), native: Native::None })))
            }
            _ => match crate::android::static_field(self, class, name) {
                Some(v) => v,
                None => return self.throw("Ljava/lang/NoSuchFieldError;", &format!("{}.{name}", java_name(class))),
            },
        })
    }

    /// The text of a `String`, `StringBuilder` or other `CharSequence`.
    pub fn str_of(v: &Val) -> Option<String> {
        let Val::R(o) = v else { return None };
        match &*o.borrow() {
            Obj::Str(s) => Some(s.clone()),
            Obj::Inst(Inst { native: Native::Sb(s), .. }) => Some(s.clone()),
            _ => None,
        }
    }

    /// `String.valueOf(Object)`: the object's `toString()`.
    pub fn to_jstring(&mut self, v: &Val) -> JResult<String> {
        match v {
            Val::P(0) => Ok("null".into()),
            Val::P(x) => Ok((*x as u32 as i32).to_string()),
            Val::R(o) => {
                if let Obj::Str(s) = &*o.borrow() {
                    return Ok(s.clone());
                }
                let s = self.call_virtual(v, "toString", "()Ljava/lang/String;", Vec::new())?;
                Ok(Self::str_of(&s).unwrap_or_else(|| "null".into()))
            }
        }
    }

    /// A value of type `ty` as `String.valueOf` writes it.
    fn typed_string(&mut self, v: &Val, ty: &str) -> JResult<String> {
        match ty.as_bytes() {
            [k] => Ok(prim_string(*k, match v {
                Val::P(x) => *x,
                Val::R(_) => 1,
            })),
            b"[C" => Ok(self.char_array(v).map(|u| text::from_units(&u)).unwrap_or_else(|| "null".into())),
            _ => self.to_jstring(v),
        }
    }

    fn char_array(&self, v: &Val) -> Option<Vec<u16>> {
        let Val::R(o) = v else { return None };
        match &*o.borrow() {
            Obj::Arr { data, .. } => Some(data.iter().map(|c| c.i() as u16).collect()),
            _ => None,
        }
    }

    fn array(v: &Val) -> Option<(Rc<str>, Vec<Val>)> {
        let Val::R(o) = v else { return None };
        match &*o.borrow() {
            Obj::Arr { ty, data } => Some((ty.clone(), data.clone())),
            _ => None,
        }
    }

    fn new_array_of(ty: &str, data: Vec<Val>) -> Val {
        Val::R(new_ref(Obj::Arr { ty: Rc::from(ty), data }))
    }

    pub(crate) fn jstring(&mut self, s: String) -> Val {
        Val::R(new_ref(Obj::Str(s)))
    }

    /// `a.equals(b)`.
    pub fn jequals(&mut self, a: &Val, b: &Val) -> JResult<bool> {
        if a.same(b) {
            return Ok(true);
        }
        let (Val::R(x), Val::R(y)) = (a, b) else { return Ok(false) };
        {
            let (x, y) = (x.borrow(), y.borrow());
            match (&*x, &*y) {
                (Obj::Str(p), Obj::Str(q)) => return Ok(p == q),
                (Obj::Str(_), _) | (_, Obj::Str(_)) => return Ok(false),
                (Obj::Inst(Inst { native: Native::Box(k1, b1), .. }), Obj::Inst(Inst { native: Native::Box(k2, b2), .. })) => {
                    return Ok(k1 == k2 && b1 == b2)
                }
                _ => {}
            }
        }
        Ok(self.call_virtual(a, "equals", "(Ljava/lang/Object;)Z", vec![b.clone()])?.z())
    }

    pub fn jhash(&mut self, v: &Val) -> JResult<i32> {
        let Val::R(o) = v else { return Ok(0) };
        if let Obj::Str(s) = &*o.borrow() {
            return Ok(text::hash(s));
        }
        Ok(self.call_virtual(v, "hashCode", "()I", Vec::new())?.i())
    }

    /// Compares with `cmp` (a `Comparator`) or by natural order.
    pub fn compare(&mut self, a: &Val, b: &Val, cmp: &Val) -> JResult<i32> {
        if !cmp.is_null() {
            // The library's own comparators are plain objects of a marker class.
            let marker = match cmp {
                Val::R(o) => match &*o.borrow() {
                    Obj::Inst(Inst { class, native: Native::None, .. }) => Some(class.clone()),
                    _ => None,
                },
                Val::P(_) => None,
            };
            return match marker.as_deref() {
                // String.CASE_INSENSITIVE_ORDER.
                Some("Ljava/lang/String;") => {
                    let (x, y) = (Self::str_of(a).unwrap_or_default().to_lowercase(), Self::str_of(b).unwrap_or_default().to_lowercase());
                    Ok(compare_str(&x, &y))
                }
                Some("Ljava/util/Collections$ReverseComparator;") => Ok(-self.compare(a, b, &Val::NULL)?),
                _ => Ok(self.call_virtual(cmp, "compare", "(Ljava/lang/Object;Ljava/lang/Object;)I", vec![a.clone(), b.clone()])?.i()),
            };
        }
        if let (Val::R(x), Val::R(y)) = (a, b) {
            let (x, y) = (x.borrow(), y.borrow());
            match (&*x, &*y) {
                (Obj::Str(p), Obj::Str(q)) => return Ok(compare_str(p, q)),
                (Obj::Inst(Inst { native: Native::Box(k1, b1), .. }), Obj::Inst(Inst { native: Native::Box(k2, b2), .. })) => {
                    let (p, q) = (num(*k1, *b1), num(*k2, *b2));
                    let o = if matches!(k1, b'D' | b'F') { p.1.partial_cmp(&q.1).unwrap_or(Ordering::Equal) } else { p.0.cmp(&q.0) };
                    return Ok(o as i32);
                }
                _ => {}
            }
        }
        if a.is_null() || b.is_null() {
            return self.npe("comparing null");
        }
        Ok(self.call_virtual(a, "compareTo", "(Ljava/lang/Object;)I", vec![b.clone()])?.i())
    }

    /// A stable merge sort with a comparator that may throw.
    pub fn sort_vals(&mut self, v: Vec<Val>, cmp: &Val) -> JResult<Vec<Val>> {
        if v.len() <= 1 {
            return Ok(v);
        }
        let mut left = v;
        let right = left.split_off(left.len() / 2);
        let left = self.sort_vals(left, cmp)?;
        let right = self.sort_vals(right, cmp)?;
        let mut out = Vec::with_capacity(left.len() + right.len());
        let (mut l, mut r) = (left.into_iter().peekable(), right.into_iter().peekable());
        while let (Some(a), Some(b)) = (l.peek(), r.peek()) {
            if self.compare(a, b, cmp)? <= 0 {
                out.extend(l.next());
            } else {
                out.extend(r.next());
            }
        }
        out.extend(l);
        out.extend(r);
        Ok(out)
    }

    fn format_args(&mut self, fmt: &str, args: &Val) -> JResult<String> {
        let list = Self::array(args).map(|a| a.1).unwrap_or_default();
        let mut out = Vec::new();
        for a in &list {
            let arg = match a {
                Val::P(0) => Arg::Text("null".into(), true),
                Val::R(o) => {
                    let b = match &*o.borrow() {
                        Obj::Inst(Inst { native: Native::Box(k, bits), .. }) => Some((*k, *bits)),
                        _ => None,
                    };
                    match b {
                        Some((k @ (b'D' | b'F'), bits)) => Arg::Float(num(k, bits).1),
                        Some((b'Z', bits)) => Arg::Bool(bits != 0),
                        Some((b'C', bits)) => Arg::Char(bits as u16),
                        Some((k, bits)) => Arg::Int(num(k, bits).0),
                        None => Arg::Text(self.to_jstring(a)?, false),
                    }
                }
                Val::P(x) => Arg::Int(*x as i64),
            };
            out.push(arg);
        }
        match text::format(fmt, &out) {
            Ok(s) => Ok(s),
            Err(e) => self.throw("Ljava/util/IllegalFormatException;", &e),
        }
    }

    fn write(&mut self, s: &str, err: bool) {
        self.host.write(s, err);
    }

    fn next_random(seed: &mut u64, bits: u32) -> i32 {
        *seed = (seed.wrapping_mul(0x5_DEEC_E66D).wrapping_add(0xB)) & ((1 << 48) - 1);
        (*seed >> (48 - bits)) as i64 as i32
    }

    fn random_int(seed: &mut u64, bound: i32) -> i32 {
        if bound.isolate_lowest_one() == bound {
            return ((bound as i64 * Self::next_random(seed, 31) as i64) >> 31) as i32;
        }
        loop {
            let bits = Self::next_random(seed, 31);
            let val = bits % bound;
            if bits.wrapping_sub(val).wrapping_add(bound - 1) >= 0 {
                return val;
            }
        }
    }

    fn random_double(seed: &mut u64) -> f64 {
        let hi = Self::next_random(seed, 26) as i64;
        let lo = Self::next_random(seed, 27) as i64;
        ((hi << 27) + lo) as f64 * (1.0 / (1u64 << 53) as f64)
    }

    fn seed_from(s: u64) -> u64 {
        (s ^ 0x5_DEEC_E66D) & ((1 << 48) - 1)
    }

    /// A call to the library.
    pub(crate) fn native(&mut self, class: &str, name: &str, sig: &str, this: Option<Val>, args: Vec<Val>) -> JResult<Val> {
        let found = match this {
            Some(this) if name == "<init>" => self.construct_native(class, sig, this, args).map(|_| Some(Val::NULL)),
            Some(this) => self.virtual_native(class, name, sig, this, args),
            None => self.static_native(class, name, sig, args),
        }?;
        match found {
            Some(v) => Ok(v),
            None => self.throw("Ljava/lang/NoSuchMethodError;", &format!("{}.{name}{sig} is not available on JustOS yet", java_name(class))),
        }
    }

    fn construct_native(&mut self, class: &str, sig: &str, this: Val, args: Vec<Val>) -> JResult<()> {
        let Val::R(o) = &this else { return self.npe("constructor of null") };
        let a0 = args.first().cloned().unwrap_or(Val::NULL);
        if class == "Ljava/lang/String;" {
            let s = match sig {
                "()V" => String::new(),
                "([C)V" => self.char_array(&a0).map(|u| text::from_units(&u)).unwrap_or_default(),
                "([CII)V" => {
                    let u = self.char_array(&a0).unwrap_or_default();
                    let (from, n) = (args[1].i().max(0) as usize, args[2].i().max(0) as usize);
                    text::from_units(u.get(from..from + n).unwrap_or(&[]))
                }
                "([B)V" => {
                    let b: Vec<u8> = Self::array(&a0).map(|a| a.1.iter().map(|v| v.i() as u8).collect()).unwrap_or_default();
                    String::from_utf8_lossy(&b).into_owned()
                }
                _ => Self::str_of(&a0).unwrap_or_default(),
            };
            *o.borrow_mut() = Obj::Str(s);
            return Ok(());
        }
        if class == "Ljava/lang/Enum;" {
            if let Obj::Inst(i) = &mut *o.borrow_mut() {
                i.fields.insert(Rc::from("$name"), a0.clone());
                i.fields.insert(Rc::from("$ordinal"), args.get(1).cloned().unwrap_or(Val::NULL));
            }
            return Ok(());
        }
        if self.is_subclass(class, "Ljava/lang/Throwable;") {
            let (msg, cause) = match sig {
                "(Ljava/lang/String;)V" => (a0, Val::NULL),
                "(Ljava/lang/String;Ljava/lang/Throwable;)V" => (a0, args[1].clone()),
                "(Ljava/lang/Throwable;)V" => {
                    let m = if a0.is_null() { Val::NULL } else { let s = self.to_jstring(&a0)?; self.jstring(s) };
                    (m, a0)
                }
                "(Ljava/lang/Object;)V" => {
                    let s = self.to_jstring(&a0)?;
                    (self.jstring(s), Val::NULL)
                }
                _ => (Val::NULL, Val::NULL),
            };
            if let Obj::Inst(i) = &mut *o.borrow_mut() {
                if !msg.is_null() {
                    i.fields.insert(Rc::from("message"), msg);
                }
                if !cause.is_null() {
                    i.fields.insert(Rc::from("cause"), cause);
                }
            }
            return Ok(());
        }
        // Read the arguments before borrowing the object (it may be one).
        let text_arg = Self::str_of(&a0);
        let items_arg: Option<Vec<Val>> = match &a0 {
            Val::R(src) if !Rc::ptr_eq(src, o) => match &*src.borrow() {
                Obj::Inst(Inst { native: Native::Coll(c), .. }) => Some(c.items.clone()),
                _ => None,
            },
            _ => None,
        };
        let map_arg: Option<(Vec<Val>, Vec<Val>)> = match &a0 {
            Val::R(src) if !Rc::ptr_eq(src, o) => match &*src.borrow() {
                Obj::Inst(Inst { native: Native::Map(m), .. }) => Some((m.keys.clone(), m.vals.clone())),
                _ => None,
            },
            _ => None,
        };
        let now = self.host.time_ms();
        let mut sort_after = None;
        {
            let mut ob = o.borrow_mut();
            let Obj::Inst(inst) = &mut *ob else { return Ok(()) };
            match &mut inst.native {
                Native::Sb(s) => {
                    if let Some(t) = text_arg {
                        *s = t;
                    }
                }
                Native::Coll(c) => {
                    if let Some(items) = items_arg {
                        if c.set || c.sorted {
                            sort_after = Some(items);
                        } else {
                            c.items = items;
                        }
                    }
                }
                Native::Map(m) => {
                    if let Some((k, v)) = map_arg {
                        m.keys = k;
                        m.vals = v;
                    }
                }
                Native::Random(seed) => {
                    *seed = Self::seed_from(if sig == "(J)V" { a0.j() as u64 } else { now.wrapping_mul(0x2545_F491_4F6C_DD1D) ^ 0x8BAD_F00D });
                }
                Native::Scanner { text, .. } => {
                    if let Some(t) = text_arg {
                        let mut lines: Vec<String> = t.lines().map(String::from).collect();
                        lines.reverse();
                        *text = Some(lines);
                    }
                }
                Native::Thread(target) => {
                    if !a0.is_null() && Self::str_of(&a0).is_none() {
                        *target = a0.clone();
                    }
                }
                Native::Box(k, bits) => {
                    *bits = match (sig, text_arg) {
                        ("(Ljava/lang/String;)V", Some(t)) => {
                            let k = *k;
                            drop(ob);
                            return match self.parse_box(k, &t)? {
                                Val::R(b) => {
                                    let nb = match &*b.borrow() {
                                        Obj::Inst(Inst { native: Native::Box(_, x), .. }) => *x,
                                        _ => 0,
                                    };
                                    if let Obj::Inst(i) = &mut *o.borrow_mut() {
                                        i.native = Native::Box(k, nb);
                                    }
                                    Ok(())
                                }
                                _ => Ok(()),
                            };
                        }
                        _ => match &a0 {
                            Val::P(x) => *x,
                            Val::R(_) => 0,
                        },
                    };
                }
                Native::View(_) | Native::Activity(_) | Native::Toast(_) | Native::Handler => {
                    drop(ob);
                    return crate::android::construct(self, class, sig, &this, &args);
                }
                _ => {}
            }
        }
        if let Some(items) = sort_after {
            for it in items {
                self.coll_add(o, it)?;
            }
        }
        Ok(())
    }

    fn parse_box(&mut self, kind: u8, s: &str) -> JResult<Val> {
        let bits = match kind {
            b'D' | b'F' => match text::parse_double(s) {
                Some(d) if kind == b'D' => d.to_bits(),
                Some(d) => (d as f32).to_bits() as u64,
                None => return self.throw("Ljava/lang/NumberFormatException;", &format!("For input string: \"{s}\"")),
            },
            b'Z' => s.eq_ignore_ascii_case("true") as u64,
            _ => {
                let (min, max) = match kind {
                    b'J' => (i64::MIN, i64::MAX),
                    b'S' => (i16::MIN as i64, i16::MAX as i64),
                    b'B' => (i8::MIN as i64, i8::MAX as i64),
                    _ => (i32::MIN as i64, i32::MAX as i64),
                };
                match text::parse_long(s, 10, min, max) {
                    Some(v) => v as u64,
                    None => return self.throw("Ljava/lang/NumberFormatException;", &format!("For input string: \"{s}\"")),
                }
            }
        };
        Ok(self.boxed(kind, bits))
    }

    /// Adds to a collection (a set keeps one of each; sorted ones stay so).
    fn coll_add(&mut self, o: &Ref, v: Val) -> JResult<bool> {
        let (items, set, sorted) = match &*o.borrow() {
            Obj::Inst(Inst { native: Native::Coll(c), .. }) => (c.items.clone(), c.set, c.sorted),
            _ => return Ok(false),
        };
        let mut at = items.len();
        if set || sorted {
            if set {
                for it in &items {
                    if self.jequals(it, &v)? {
                        return Ok(false);
                    }
                }
            }
            if sorted {
                at = 0;
                while at < items.len() && self.compare(&items[at], &v, &Val::NULL)? <= 0 {
                    at += 1;
                }
            }
        }
        if let Obj::Inst(Inst { native: Native::Coll(c), .. }) = &mut *o.borrow_mut() {
            let at = at.min(c.items.len());
            c.items.insert(at, v);
        }
        Ok(true)
    }

    fn index_of(&mut self, items: &[Val], v: &Val) -> JResult<Option<usize>> {
        for (i, it) in items.iter().enumerate() {
            if self.jequals(it, v)? {
                return Ok(Some(i));
            }
        }
        Ok(None)
    }

    fn list_string(&mut self, items: &[Val], open: char, close: char) -> JResult<String> {
        let mut s = String::from(open);
        for (i, it) in items.iter().enumerate() {
            if i > 0 {
                s.push_str(", ");
            }
            s.push_str(&self.to_jstring(it)?);
        }
        s.push(close);
        Ok(s)
    }

    fn virtual_native(&mut self, class: &str, name: &str, sig: &str, this: Val, args: Vec<Val>) -> JResult<Option<Val>> {
        let Val::R(o) = this.clone() else { return self.npe(&format!("{name}() on null")) };
        let kind = match &*o.borrow() {
            Obj::Str(_) => Kind::Str,
            Obj::Arr { .. } => Kind::Arr,
            Obj::Inst(_) => Kind::Plain,
        };
        let got = match kind {
            Kind::Str => self.string_method(&o, name, sig, &args)?,
            Kind::Arr => match name {
                "clone" => {
                    let (ty, data) = Self::array(&this).unwrap_or((Rc::from("[Ljava/lang/Object;"), Vec::new()));
                    Some(Self::new_array_of(&ty, data))
                }
                _ => None,
            },
            Kind::Plain => self.inst_method(&o, class, name, sig, &args)?,
        };
        if got.is_some() {
            return Ok(got);
        }
        if let Some(v) = crate::android::method(self, &o, class, name, sig, &args)? {
            return Ok(Some(v));
        }
        let cls = Self::class_of(&o);
        if self.is_subclass(&cls, "Ljava/lang/Throwable;") {
            if let Some(v) = self.throwable_method(&o, name)? {
                return Ok(Some(v));
            }
        }
        if self.is_subclass(&cls, "Ljava/lang/Enum;") {
            let field = |n: &str| match &*o.borrow() {
                Obj::Inst(i) => i.fields.get(n).cloned().unwrap_or(Val::NULL),
                _ => Val::NULL,
            };
            match name {
                "name" | "toString" => return Ok(Some(field("$name"))),
                "ordinal" => return Ok(Some(field("$ordinal"))),
                "compareTo" => {
                    let other = match &args[0] {
                        Val::R(x) => match &*x.borrow() {
                            Obj::Inst(i) => i.fields.get("$ordinal").map(|v| v.i()).unwrap_or(0),
                            _ => 0,
                        },
                        _ => return self.npe("compareTo(null)"),
                    };
                    return Ok(Some(Val::int(field("$ordinal").i() - other)));
                }
                "getDeclaringClass" => return Ok(Some(self.class_object(&cls))),
                _ => {}
            }
        }
        // java.lang.Object.
        Ok(match name {
            "toString" => {
                let s = match &*o.borrow() {
                    Obj::Arr { ty, .. } => format!("{}@{:x}", ty.replace('/', "."), identity(&o)),
                    _ => format!("{}@{:x}", java_name(&cls), identity(&o)),
                };
                Some(self.jstring(s))
            }
            "hashCode" => Some(Val::int(identity(&o))),
            "equals" => Some(Val::bool(this.same(&args[0]))),
            "getClass" => Some(self.class_object(&cls)),
            "notify" | "notifyAll" | "wait" | "finalize" => Some(Val::NULL),
            _ => None,
        })
    }

    fn throwable_method(&mut self, o: &Ref, name: &str) -> JResult<Option<Val>> {
        let field = |n: &str| match &*o.borrow() {
            Obj::Inst(i) => i.fields.get(n).cloned().unwrap_or(Val::NULL),
            _ => Val::NULL,
        };
        Ok(Some(match name {
            "getMessage" | "getLocalizedMessage" => field("message"),
            "getCause" => field("cause"),
            "toString" => {
                let mut s = java_name(&Self::class_of(o));
                let m = field("message");
                if !m.is_null() {
                    s.push_str(": ");
                    s.push_str(&self.to_jstring(&m)?);
                }
                self.jstring(s)
            }
            "printStackTrace" => {
                let s = self.describe(o);
                self.write(&format!("{s}\n\tat (JustOS: no stack trace)\n"), true);
                Val::NULL
            }
            "initCause" => {
                if let Obj::Inst(i) = &mut *o.borrow_mut() {
                    i.fields.insert(Rc::from("cause"), field("cause"));
                }
                Val::R(o.clone())
            }
            "fillInStackTrace" => Val::R(o.clone()),
            "getStackTrace" => Self::new_array_of("[Ljava/lang/StackTraceElement;", Vec::new()),
            "addSuppressed" | "setStackTrace" => Val::NULL,
            _ => return Ok(None),
        }))
    }

    fn string_method(&mut self, o: &Ref, name: &str, sig: &str, args: &[Val]) -> JResult<Option<Val>> {
        let s = match &*o.borrow() {
            Obj::Str(s) => s.clone(),
            _ => String::new(),
        };
        let arg_s = |k: usize| args.get(k).and_then(Self::str_of);
        let int = |k: usize| args.get(k).map(|v| v.i()).unwrap_or(0);
        let n = text::len(&s);
        let v = match (name, sig) {
            ("length", _) => Val::int(n as i32),
            ("isEmpty", _) => Val::bool(s.is_empty()),
            ("isBlank", _) => Val::bool(s.trim().is_empty()),
            ("charAt", _) => {
                let i = int(0);
                match text::char_at(&s, i.max(0) as usize).filter(|_| i >= 0) {
                    Some(c) => Val::int(c as i32),
                    None => return self.throw("Ljava/lang/StringIndexOutOfBoundsException;", &format!("index {i}, length {n}")),
                }
            }
            ("substring" | "subSequence", _) => {
                let from = int(0);
                let to = if args.len() > 1 { int(1) } else { n as i32 };
                if from < 0 || to > n as i32 || from > to {
                    return self.throw("Ljava/lang/StringIndexOutOfBoundsException;", &format!("begin {from}, end {to}, length {n}"));
                }
                let t = text::sub(&s, from as usize, to as usize).unwrap_or_default();
                self.jstring(t)
            }
            ("indexOf" | "lastIndexOf", _) => {
                let needle = match arg_s(0) {
                    Some(t) => t,
                    None => text::char_string(int(0) as u16),
                };
                let last = name == "lastIndexOf";
                let found = if args.len() > 1 {
                    let from = int(1).max(0) as usize;
                    if last {
                        let end = text::byte_at(&s, from.saturating_add(text::len(&needle)));
                        s[..end].rfind(&needle)
                    } else {
                        let b = text::byte_at(&s, from);
                        s[b..].find(&needle).map(|i| i + b)
                    }
                } else if last {
                    s.rfind(&needle)
                } else {
                    s.find(&needle)
                };
                Val::int(found.map(|b| text::pos(&s, b) as i32).unwrap_or(-1))
            }
            ("contains", _) => Val::bool(s.contains(&arg_s(0).unwrap_or_default())),
            ("startsWith", "(Ljava/lang/String;I)Z") => {
                let b = text::byte_at(&s, int(1).max(0) as usize);
                Val::bool(s[b..].starts_with(&arg_s(0).unwrap_or_default()))
            }
            ("startsWith", _) => Val::bool(s.starts_with(&arg_s(0).unwrap_or_default())),
            ("endsWith", _) => Val::bool(s.ends_with(&arg_s(0).unwrap_or_default())),
            ("equals" | "contentEquals", _) => Val::bool(matches!(&args[0], Val::R(x) if matches!(&*x.borrow(), Obj::Str(t) if *t == s)) || (name == "contentEquals" && arg_s(0).as_deref() == Some(&s))),
            ("equalsIgnoreCase", _) => Val::bool(arg_s(0).is_some_and(|t| t.to_lowercase() == s.to_lowercase())),
            ("compareTo", _) => match arg_s(0) {
                Some(t) => Val::int(compare_str(&s, &t)),
                None => return self.npe("compareTo(null)"),
            },
            ("compareToIgnoreCase", _) => Val::int(compare_str(&s.to_lowercase(), &arg_s(0).unwrap_or_default().to_lowercase())),
            ("hashCode", _) => Val::int(text::hash(&s)),
            ("toUpperCase", _) => self.jstring(s.to_uppercase()),
            ("toLowerCase", _) => self.jstring(s.to_lowercase()),
            ("trim", _) => self.jstring(s.trim_matches(|c: char| c <= ' ').into()),
            ("strip", _) => self.jstring(s.trim().into()),
            ("stripLeading", _) => self.jstring(s.trim_start().into()),
            ("stripTrailing", _) => self.jstring(s.trim_end().into()),
            ("concat", _) => self.jstring(s + &arg_s(0).unwrap_or_default()),
            ("repeat", _) => self.jstring(s.repeat(int(0).max(0) as usize)),
            ("intern" | "toString", _) => Val::R(o.clone()),
            ("replace", "(CC)Ljava/lang/String;") => {
                let (a, b) = (int(0) as u16, int(1) as u16);
                let u: Vec<u16> = text::units(&s).into_iter().map(|c| if c == a { b } else { c }).collect();
                self.jstring(text::from_units(&u))
            }
            ("replace", _) => {
                let (a, b) = (arg_s(0).unwrap_or_default(), arg_s(1).unwrap_or_default());
                let t = if a.is_empty() {
                    let mut t = b.clone();
                    for c in s.chars() {
                        t.push(c);
                        t.push_str(&b);
                    }
                    t
                } else {
                    s.replace(&a, &b)
                };
                self.jstring(t)
            }
            ("replaceAll" | "replaceFirst", _) => {
                let re = self.regex(&arg_s(0).unwrap_or_default())?;
                let t = re.replace(&s, &arg_s(1).unwrap_or_default(), name == "replaceAll");
                self.jstring(t)
            }
            ("matches", _) => {
                let re = self.regex(&arg_s(0).unwrap_or_default())?;
                Val::bool(re.matches(&s))
            }
            ("split", _) => {
                let pat = arg_s(0).unwrap_or_default();
                let limit = if args.len() > 1 { int(1) } else { 0 };
                let parts = match text::literal(&pat).filter(|p| !p.is_empty()) {
                    Some(p) => {
                        let mut parts: Vec<String> = if limit > 0 { s.splitn(limit as usize, p.as_str()).map(String::from).collect() } else { s.split(p.as_str()).map(String::from).collect() };
                        if limit == 0 {
                            while parts.len() > 1 && parts.last().is_some_and(|p| p.is_empty()) {
                                parts.pop();
                            }
                            if parts.len() == 1 && parts[0].is_empty() && !s.is_empty() {
                                parts.pop();
                            }
                        }
                        parts
                    }
                    None => self.regex(&pat)?.split(&s, limit),
                };
                let data = parts.into_iter().map(|p| self.jstring(p)).collect();
                Self::new_array_of("[Ljava/lang/String;", data)
            }
            ("toCharArray", _) => Self::new_array_of("[C", text::units(&s).into_iter().map(|c| Val::int(c as i32)).collect()),
            ("getBytes", _) => Self::new_array_of("[B", s.bytes().map(|b| Val::int(b as i8 as i32)).collect()),
            ("chars", _) => return Ok(None),
            _ => return Ok(None),
        };
        Ok(Some(v))
    }

    fn regex(&mut self, pat: &str) -> JResult<Regex> {
        match Regex::new(pat) {
            Ok(r) => Ok(r),
            Err(e) => self.throw("Ljava/util/regex/PatternSyntaxException;", &format!("{e} {pat}")),
        }
    }

    /// Methods of objects with library state.
    fn inst_method(&mut self, o: &Ref, class: &str, name: &str, sig: &str, args: &[Val]) -> JResult<Option<Val>> {
        enum K {
            Out(bool),
            In,
            Sb,
            Box(u8, u64),
            Coll,
            Map,
            Entry(Val, Val),
            Iter,
            Random,
            Scanner,
            Reader,
            Thread(Val),
            Class(Rc<str>),
            Other,
        }
        let k = match &*o.borrow() {
            Obj::Inst(i) => match &i.native {
                Native::Out(e) => K::Out(*e),
                Native::In => K::In,
                Native::Sb(_) => K::Sb,
                Native::Box(k, b) => K::Box(*k, *b),
                Native::Coll(_) => K::Coll,
                Native::Map(_) => K::Map,
                Native::Entry(a, b) => K::Entry(a.clone(), b.clone()),
                Native::Iter { .. } => K::Iter,
                Native::Random(_) => K::Random,
                Native::Scanner { .. } => K::Scanner,
                Native::Reader => K::Reader,
                Native::Thread(t) => K::Thread(t.clone()),
                Native::Class(c) => K::Class(c.clone()),
                _ => K::Other,
            },
            _ => K::Other,
        };
        let a0 = args.first().cloned().unwrap_or(Val::NULL);
        Ok(match k {
            K::Out(err) => {
                let s = match name {
                    "println" | "print" => {
                        let mut s = match crate::params(sig).first() {
                            Some(ty) => self.typed_string(&a0, ty)?,
                            None => String::new(),
                        };
                        if name == "println" {
                            s.push('\n');
                        }
                        s
                    }
                    "printf" | "format" => {
                        let (f, a) = if sig.starts_with("(Ljava/util/Locale;") { (1, 2) } else { (0, 1) };
                        let fmt = Self::str_of(&args[f]).unwrap_or_default();
                        self.format_args(&fmt, &args[a])?
                    }
                    "write" => text::char_string(a0.i() as u8 as u16),
                    "flush" | "close" => String::new(),
                    _ => return Ok(None),
                };
                self.write(&s, err);
                Some(if name == "printf" || name == "format" { Val::R(o.clone()) } else { Val::NULL })
            }
            K::In => match name {
                "read" => {
                    // A byte at a time from the line the user typed.
                    let c = self.host.read_line().map(|l| l.bytes().next().unwrap_or(b'\n') as i32).unwrap_or(-1);
                    Some(Val::int(c))
                }
                "available" => Some(Val::int(0)),
                "close" => Some(Val::NULL),
                _ => None,
            },
            K::Sb => self.sb_method(o, name, sig, args)?,
            K::Box(kind, bits) => {
                let (i, d) = num(kind, bits);
                Some(match name {
                    "intValue" => Val::int(i as i32),
                    "longValue" => Val::long(i),
                    "shortValue" => Val::int(i as i16 as i32),
                    "byteValue" => Val::int(i as i8 as i32),
                    "doubleValue" => Val::double(d),
                    "floatValue" => Val::float(d as f32),
                    "booleanValue" | "charValue" => Val::P(bits),
                    "toString" => self.jstring(prim_string(kind, bits)),
                    "hashCode" => Val::int(match kind {
                        b'J' => (bits ^ (bits >> 32)) as i32,
                        b'D' => (bits ^ (bits >> 32)) as i32,
                        b'Z' => if bits != 0 { 1231 } else { 1237 },
                        _ => bits as u32 as i32,
                    }),
                    "equals" => Val::bool(matches!(&a0, Val::R(x) if matches!(&*x.borrow(), Obj::Inst(Inst { native: Native::Box(k2, b2), .. }) if *k2 == kind && *b2 == bits))),
                    "compareTo" => Val::int(self.compare(&Val::R(o.clone()), &a0, &Val::NULL)?),
                    "isNaN" => Val::bool(d.is_nan()),
                    "isInfinite" => Val::bool(d.is_infinite()),
                    _ => return Ok(None),
                })
            }
            K::Coll => self.coll_method(o, class, name, sig, args)?,
            K::Map => self.map_method(o, name, args)?,
            K::Entry(k, v) => match name {
                "getKey" => Some(k),
                "getValue" => Some(v),
                "setValue" => {
                    if let Obj::Inst(Inst { native: Native::Entry(_, val), .. }) = &mut *o.borrow_mut() {
                        *val = a0;
                    }
                    Some(v)
                }
                "toString" => {
                    let s = format!("{}={}", self.to_jstring(&k)?, self.to_jstring(&v)?);
                    Some(self.jstring(s))
                }
                "hashCode" => Some(Val::int(self.jhash(&k)? ^ self.jhash(&v)?)),
                _ => None,
            },
            K::Iter => {
                let (src, pos) = match &*o.borrow() {
                    Obj::Inst(Inst { native: Native::Iter { src, pos }, .. }) => (src.clone(), *pos),
                    _ => return Ok(None),
                };
                let items = match &*src.borrow() {
                    Obj::Inst(Inst { native: Native::Coll(c), .. }) => c.items.clone(),
                    Obj::Arr { data, .. } => data.clone(),
                    _ => Vec::new(),
                };
                let set_pos = |p: usize| {
                    if let Obj::Inst(Inst { native: Native::Iter { pos, .. }, .. }) = &mut *o.borrow_mut() {
                        *pos = p;
                    }
                };
                match name {
                    "hasNext" => Some(Val::bool(pos < items.len())),
                    "next" => match items.get(pos) {
                        Some(v) => {
                            set_pos(pos + 1);
                            Some(v.clone())
                        }
                        None => return self.throw("Ljava/util/NoSuchElementException;", ""),
                    },
                    "remove" if pos > 0 => {
                        if let Obj::Inst(Inst { native: Native::Coll(c), .. }) = &mut *src.borrow_mut() {
                            c.items.remove(pos - 1);
                        }
                        set_pos(pos - 1);
                        Some(Val::NULL)
                    }
                    "remove" => return self.throw("Ljava/lang/IllegalStateException;", ""),
                    _ => None,
                }
            }
            K::Random => {
                let mut seed = match &*o.borrow() {
                    Obj::Inst(Inst { native: Native::Random(s), .. }) => *s,
                    _ => 0,
                };
                let v = match (name, sig) {
                    ("nextInt", "()I") => Val::int(Self::next_random(&mut seed, 32)),
                    ("nextInt", "(I)I") => {
                        if a0.i() <= 0 {
                            return self.throw("Ljava/lang/IllegalArgumentException;", "bound must be positive");
                        }
                        Val::int(Self::random_int(&mut seed, a0.i()))
                    }
                    ("nextInt", "(II)I") => {
                        let (lo, hi) = (a0.i(), args[1].i());
                        if lo >= hi {
                            return self.throw("Ljava/lang/IllegalArgumentException;", "bound must be greater than origin");
                        }
                        Val::int(lo.wrapping_add(Self::random_int(&mut seed, hi.wrapping_sub(lo))))
                    }
                    ("nextLong", _) => {
                        let hi = Self::next_random(&mut seed, 32) as i64;
                        let lo = Self::next_random(&mut seed, 32) as i64;
                        Val::long((hi << 32).wrapping_add(lo))
                    }
                    ("nextDouble", _) => Val::double(Self::random_double(&mut seed)),
                    ("nextFloat", _) => Val::float(Self::next_random(&mut seed, 24) as f32 / (1 << 24) as f32),
                    ("nextBoolean", _) => Val::bool(Self::next_random(&mut seed, 1) != 0),
                    ("nextGaussian", _) => {
                        // Box-Muller, as Java (without the cached second value).
                        loop {
                            let v1 = 2.0 * Self::random_double(&mut seed) - 1.0;
                            let v2 = 2.0 * Self::random_double(&mut seed) - 1.0;
                            let s = v1 * v1 + v2 * v2;
                            if s < 1.0 && s != 0.0 {
                                break Val::double(v1 * float::sqrt(-2.0 * float::ln(s) / s));
                            }
                        }
                    }
                    ("setSeed", _) => {
                        seed = Self::seed_from(a0.j() as u64);
                        Val::NULL
                    }
                    _ => return Ok(None),
                };
                if let Obj::Inst(Inst { native: Native::Random(s), .. }) = &mut *o.borrow_mut() {
                    *s = seed;
                }
                Some(v)
            }
            K::Scanner => self.scanner_method(o, name)?,
            K::Reader => match name {
                "readLine" => Some(match self.host.read_line() {
                    Some(l) => self.jstring(l),
                    None => Val::NULL,
                }),
                "ready" => Some(Val::bool(true)),
                "close" => Some(Val::NULL),
                _ => None,
            },
            K::Thread(target) => match name {
                "start" => {
                    // One thread at a time: the new one runs to its end now.
                    self.call_virtual(&Val::R(o.clone()), "run", "()V", Vec::new())?;
                    Some(Val::NULL)
                }
                "run" => {
                    if !target.is_null() {
                        self.call_virtual(&target, "run", "()V", Vec::new())?;
                    }
                    Some(Val::NULL)
                }
                "join" | "interrupt" | "setDaemon" | "setName" | "setPriority" => Some(Val::NULL),
                "isAlive" | "isInterrupted" => Some(Val::bool(false)),
                "getName" => Some(self.jstring("main".into())),
                "getId" => Some(Val::long(1)),
                _ => None,
            },
            K::Class(c) => match name {
                "getName" | "getTypeName" | "getCanonicalName" => Some(self.jstring(java_name(&c))),
                "getSimpleName" => {
                    let n = java_name(&c);
                    let n = n.rsplit(['.', '$']).next().unwrap_or("").to_string();
                    Some(self.jstring(n))
                }
                "toString" => Some(self.jstring(format!("class {}", java_name(&c)))),
                "isInstance" => Some(Val::bool(self.instance_of(&a0, &c))),
                "hashCode" => Some(Val::int(text::hash(&c))),
                "equals" => Some(Val::bool(matches!(&a0, Val::R(x) if matches!(&*x.borrow(), Obj::Inst(Inst { native: Native::Class(d), .. }) if *d == c)))),
                _ => None,
            },
            K::Other => None,
        })
    }

    fn sb_method(&mut self, o: &Ref, name: &str, sig: &str, args: &[Val]) -> JResult<Option<Val>> {
        let a0 = args.first().cloned().unwrap_or(Val::NULL);
        let cur = Self::str_of(&Val::R(o.clone())).unwrap_or_default();
        let int = |k: usize| args.get(k).map(|v| v.i()).unwrap_or(0);
        let this = Val::R(o.clone());
        let new = match name {
            "append" => {
                let ps = crate::params(sig);
                let mut s = cur;
                if ps.len() == 3 && ps[0] == "[C" {
                    let u = self.char_array(&a0).unwrap_or_default();
                    let (from, n) = (int(1).max(0) as usize, int(2).max(0) as usize);
                    s.push_str(&text::from_units(u.get(from..from + n).unwrap_or(&[])));
                } else if ps.len() == 3 {
                    let t = self.to_jstring(&a0)?;
                    s.push_str(&text::sub(&t, int(1).max(0) as usize, int(2).max(0) as usize).unwrap_or_default());
                } else {
                    let t = self.typed_string(&a0, ps.first().copied().unwrap_or(OBJ))?;
                    s.push_str(&t);
                }
                s
            }
            "insert" => {
                let ps = crate::params(sig);
                let t = self.typed_string(&args[1], ps.get(1).copied().unwrap_or(OBJ))?;
                let at = text::byte_at(&cur, int(0).max(0) as usize);
                let mut s = cur;
                s.insert_str(at, &t);
                s
            }
            "reverse" => {
                let mut u = text::units(&cur);
                u.reverse();
                text::from_units(&u)
            }
            "deleteCharAt" | "delete" => {
                let n = text::len(&cur) as i32;
                let from = int(0);
                let to = if name == "delete" { int(1).min(n) } else { from + 1 };
                if from < 0 || from > to || (name == "deleteCharAt" && from >= n) {
                    return self.throw("Ljava/lang/StringIndexOutOfBoundsException;", &format!("index {from},length {n}"));
                }
                let mut u = text::units(&cur);
                u.drain(from as usize..to as usize);
                text::from_units(&u)
            }
            "replace" => {
                let mut u = text::units(&cur);
                let (from, to) = (int(0).max(0) as usize, (int(1).max(0) as usize).min(u.len()));
                let with = text::units(&Self::str_of(&args[2]).unwrap_or_default());
                u.splice(from.min(to)..to, with);
                text::from_units(&u)
            }
            "setCharAt" => {
                let mut u = text::units(&cur);
                match u.get_mut(int(0).max(0) as usize) {
                    Some(c) => *c = int(1) as u16,
                    None => return self.throw("Ljava/lang/StringIndexOutOfBoundsException;", ""),
                }
                text::from_units(&u)
            }
            "setLength" => {
                let mut u = text::units(&cur);
                u.resize(int(0).max(0) as usize, 0);
                text::from_units(&u)
            }
            _ => {
                // Reading methods are the String ones.
                let s = new_ref(Obj::Str(cur));
                return match name {
                    "toString" => Ok(Some(Val::R(s))),
                    "capacity" => Ok(Some(Val::int(text::len(&Self::str_of(&Val::R(s)).unwrap_or_default()) as i32 + 16))),
                    "equals" => Ok(Some(Val::bool(this.same(&a0)))),
                    "hashCode" => Ok(Some(Val::int(identity(o)))),
                    "compareTo" => {
                        let t = Self::str_of(&a0).unwrap_or_default();
                        Ok(Some(Val::int(compare_str(&Self::str_of(&Val::R(s)).unwrap_or_default(), &t))))
                    }
                    _ => self.string_method(&s, name, sig, args),
                };
            }
        };
        if let Obj::Inst(Inst { native: Native::Sb(s), .. }) = &mut *o.borrow_mut() {
            *s = new;
        }
        Ok(Some(if name == "setCharAt" || name == "setLength" { Val::NULL } else { this }))
    }

    fn coll_items(o: &Ref) -> Vec<Val> {
        match &*o.borrow() {
            Obj::Inst(Inst { native: Native::Coll(c), .. }) => c.items.clone(),
            Obj::Arr { data, .. } => data.clone(),
            _ => Vec::new(),
        }
    }

    fn set_items(o: &Ref, items: Vec<Val>) {
        if let Obj::Inst(Inst { native: Native::Coll(c), .. }) = &mut *o.borrow_mut() {
            c.items = items;
        }
    }

    /// The elements of a collection, an array or a map's keys.
    fn elements(&mut self, v: &Val) -> JResult<Vec<Val>> {
        let Val::R(o) = v else { return self.npe("null collection") };
        let map = match &*o.borrow() {
            Obj::Inst(Inst { native: Native::Map(m), .. }) => Some(m.keys.clone()),
            _ => None,
        };
        match map {
            Some(k) => Ok(k),
            None => {
                let items = Self::coll_items(o);
                if items.is_empty() && matches!(&*o.borrow(), Obj::Inst(Inst { native: Native::None, .. })) {
                    // A collection of the program's own: walk its iterator.
                    let it = self.call_virtual(v, "iterator", "()Ljava/util/Iterator;", Vec::new())?;
                    let mut out = Vec::new();
                    while self.call_virtual(&it, "hasNext", "()Z", Vec::new())?.z() {
                        out.push(self.call_virtual(&it, "next", "()Ljava/lang/Object;", Vec::new())?);
                    }
                    return Ok(out);
                }
                Ok(items)
            }
        }
    }

    fn iterator(&mut self, src: &Ref) -> Val {
        Val::R(new_ref(Obj::Inst(Inst { class: Rc::from("Ljava/util/Iterator;"), fields: BTreeMap::new(), native: Native::Iter { src: src.clone(), pos: 0 } })))
    }

    fn coll_method(&mut self, o: &Ref, class: &str, name: &str, sig: &str, args: &[Val]) -> JResult<Option<Val>> {
        let items = Self::coll_items(o);
        let a0 = args.first().cloned().unwrap_or(Val::NULL);
        let n = items.len();
        let stack = class == "Ljava/util/Stack;";
        let bad = |vm: &mut Vm, i: i32| vm.throw::<Option<Val>>("Ljava/lang/IndexOutOfBoundsException;", &format!("Index {i} out of bounds for length {n}"));
        let empty = |vm: &mut Vm| vm.throw::<Option<Val>>(if stack { "Ljava/util/EmptyStackException;" } else { "Ljava/util/NoSuchElementException;" }, "");
        let v = match (name, sig) {
            ("add" | "offer" | "addLast" | "offerLast", "(Ljava/lang/Object;)Z" | "(Ljava/lang/Object;)V") => {
                let added = self.coll_add(o, a0)?;
                if sig.ends_with('Z') {
                    Val::bool(added)
                } else {
                    Val::NULL
                }
            }
            ("push", _) if stack => {
                self.coll_add(o, a0.clone())?;
                a0
            }
            ("add", "(ILjava/lang/Object;)V") => {
                let i = a0.i();
                if i < 0 || i as usize > n {
                    return bad(self, i);
                }
                let mut items = items;
                items.insert(i as usize, args[1].clone());
                Self::set_items(o, items);
                Val::NULL
            }
            ("addFirst" | "offerFirst" | "push", _) => {
                let mut items = items;
                items.insert(0, a0);
                Self::set_items(o, items);
                Val::bool(true)
            }
            ("get" | "elementAt", _) => match items.get(a0.i().max(0) as usize).filter(|_| a0.i() >= 0) {
                Some(v) => v.clone(),
                None => return bad(self, a0.i()),
            },
            ("set", _) => {
                let i = a0.i();
                if i < 0 || i as usize >= n {
                    return bad(self, i);
                }
                let mut items = items;
                let old = core::mem::replace(&mut items[i as usize], args[1].clone());
                Self::set_items(o, items);
                old
            }
            ("remove", "(I)Ljava/lang/Object;") => {
                let i = a0.i();
                if i < 0 || i as usize >= n {
                    return bad(self, i);
                }
                let mut items = items;
                let old = items.remove(i as usize);
                Self::set_items(o, items);
                old
            }
            ("remove", "(Ljava/lang/Object;)Z") => match self.index_of(&items, &a0)? {
                Some(i) => {
                    let mut items = items;
                    items.remove(i);
                    Self::set_items(o, items);
                    Val::bool(true)
                }
                None => Val::bool(false),
            },
            ("pop", _) if stack => match items.last().cloned() {
                Some(v) => {
                    let mut items = items;
                    items.pop();
                    Self::set_items(o, items);
                    v
                }
                None => return empty(self),
            },
            ("peek", _) if stack => match items.last() {
                Some(v) => v.clone(),
                None => return empty(self),
            },
            ("remove" | "pop" | "removeFirst" | "poll" | "pollFirst" | "element" | "getFirst" | "peek" | "peekFirst" | "first" | "firstElement", _) => {
                let take = matches!(name, "remove" | "pop" | "removeFirst" | "poll" | "pollFirst");
                match items.first().cloned() {
                    Some(v) => {
                        if take {
                            let mut items = items;
                            items.remove(0);
                            Self::set_items(o, items);
                        }
                        v
                    }
                    None if name.starts_with("p") && name != "pop" => Val::NULL,
                    None => return empty(self),
                }
            }
            ("removeLast" | "pollLast" | "getLast" | "peekLast" | "last" | "lastElement", _) => match items.last().cloned() {
                Some(v) => {
                    if name == "removeLast" || name == "pollLast" {
                        let mut items = items;
                        items.pop();
                        Self::set_items(o, items);
                    }
                    v
                }
                None if name.starts_with("p") => Val::NULL,
                None => return empty(self),
            },
            ("size", _) => Val::int(n as i32),
            ("isEmpty" | "empty", _) => Val::bool(n == 0),
            ("contains", _) => Val::bool(self.index_of(&items, &a0)?.is_some()),
            ("containsAll", _) => {
                let other = self.elements(&a0)?;
                let mut all = true;
                for v in &other {
                    if self.index_of(&items, v)?.is_none() {
                        all = false;
                        break;
                    }
                }
                Val::bool(all)
            }
            ("indexOf", _) => Val::int(self.index_of(&items, &a0)?.map(|i| i as i32).unwrap_or(-1)),
            ("lastIndexOf", _) => {
                let mut found = -1;
                for (i, it) in items.iter().enumerate() {
                    if self.jequals(it, &a0)? {
                        found = i as i32;
                    }
                }
                Val::int(found)
            }
            ("search", _) if stack => {
                let mut found = -1;
                for (i, it) in items.iter().rev().enumerate() {
                    if self.jequals(it, &a0)? {
                        found = i as i32 + 1;
                        break;
                    }
                }
                Val::int(found)
            }
            ("clear" | "removeAllElements", _) => {
                Self::set_items(o, Vec::new());
                Val::NULL
            }
            ("addAll", "(Ljava/util/Collection;)Z") => {
                let other = self.elements(&a0)?;
                let any = !other.is_empty();
                for v in other {
                    self.coll_add(o, v)?;
                }
                Val::bool(any)
            }
            ("addAll", "(ILjava/util/Collection;)Z") => {
                let other = self.elements(&args[1])?;
                let mut items = items;
                let at = (a0.i().max(0) as usize).min(n);
                items.splice(at..at, other);
                Self::set_items(o, items);
                Val::bool(true)
            }
            ("removeAll" | "retainAll", _) => {
                let other = self.elements(&a0)?;
                let mut keep = Vec::new();
                for it in &items {
                    let inside = self.index_of(&other, it)?.is_some();
                    if inside == (name == "retainAll") {
                        keep.push(it.clone());
                    }
                }
                let changed = keep.len() != n;
                Self::set_items(o, keep);
                Val::bool(changed)
            }
            ("removeIf", _) => {
                let mut keep = Vec::new();
                for it in &items {
                    if !self.call_virtual(&a0, "test", "(Ljava/lang/Object;)Z", vec![it.clone()])?.z() {
                        keep.push(it.clone());
                    }
                }
                let changed = keep.len() != n;
                Self::set_items(o, keep);
                Val::bool(changed)
            }
            ("forEach", _) => {
                for it in &items {
                    self.call_virtual(&a0, "accept", "(Ljava/lang/Object;)V", vec![it.clone()])?;
                }
                Val::NULL
            }
            ("iterator" | "listIterator" | "descendingIterator", _) => {
                if name == "descendingIterator" {
                    let mut rev = items;
                    rev.reverse();
                    let c = self.new_coll("Ljava/util/ArrayList;", rev);
                    let Val::R(c) = c else { unreachable!() };
                    self.iterator(&c)
                } else {
                    self.iterator(o)
                }
            }
            ("sort", _) => {
                let sorted = self.sort_vals(items, &a0)?;
                Self::set_items(o, sorted);
                Val::NULL
            }
            ("subList", _) => {
                let (from, to) = (a0.i(), args[1].i());
                if from < 0 || to as usize > n || from > to {
                    return bad(self, to);
                }
                self.new_coll("Ljava/util/ArrayList;", items[from as usize..to as usize].to_vec())
            }
            ("toArray", _) => {
                let ty = match Self::array(&a0) {
                    Some((ty, _)) => ty,
                    None => Rc::from("[Ljava/lang/Object;"),
                };
                Self::new_array_of(&ty, items)
            }
            ("toString", _) => {
                let s = self.list_string(&items, '[', ']')?;
                self.jstring(s)
            }
            ("equals", _) => {
                let Val::R(x) = &a0 else { return Ok(Some(Val::bool(false))) };
                let other = match &*x.borrow() {
                    Obj::Inst(Inst { native: Native::Coll(c), .. }) => Some(c.items.clone()),
                    _ => None,
                };
                let Some(other) = other else { return Ok(Some(Val::bool(false))) };
                let mut eq = other.len() == n;
                if eq {
                    let set = matches!(&*o.borrow(), Obj::Inst(Inst { native: Native::Coll(c), .. }) if c.set);
                    for (i, it) in items.iter().enumerate() {
                        let ok = if set { self.index_of(&other, it)?.is_some() } else { self.jequals(it, &other[i])? };
                        if !ok {
                            eq = false;
                            break;
                        }
                    }
                }
                Val::bool(eq)
            }
            ("hashCode", _) => {
                let mut h = 1i32;
                for it in &items {
                    h = h.wrapping_mul(31).wrapping_add(self.jhash(it)?);
                }
                Val::int(h)
            }
            ("ensureCapacity" | "trimToSize", _) => Val::NULL,
            _ => return Ok(None),
        };
        Ok(Some(v))
    }

    fn map_parts(o: &Ref) -> (Vec<Val>, Vec<Val>, bool) {
        match &*o.borrow() {
            Obj::Inst(Inst { native: Native::Map(m), .. }) => (m.keys.clone(), m.vals.clone(), m.sorted),
            _ => (Vec::new(), Vec::new(), false),
        }
    }

    fn set_map(o: &Ref, keys: Vec<Val>, vals: Vec<Val>) {
        if let Obj::Inst(Inst { native: Native::Map(m), .. }) = &mut *o.borrow_mut() {
            m.keys = keys;
            m.vals = vals;
        }
    }

    /// Puts a key; gives the old value.
    fn map_put(&mut self, o: &Ref, k: Val, v: Val) -> JResult<Val> {
        let (mut keys, mut vals, sorted) = Self::map_parts(o);
        if let Some(i) = self.index_of(&keys, &k)? {
            let old = core::mem::replace(&mut vals[i], v);
            Self::set_map(o, keys, vals);
            return Ok(old);
        }
        let mut at = keys.len();
        if sorted {
            at = 0;
            while at < keys.len() && self.compare(&keys[at], &k, &Val::NULL)? < 0 {
                at += 1;
            }
        }
        keys.insert(at, k);
        vals.insert(at, v);
        Self::set_map(o, keys, vals);
        Ok(Val::NULL)
    }

    fn map_method(&mut self, o: &Ref, name: &str, args: &[Val]) -> JResult<Option<Val>> {
        let (keys, vals, _) = Self::map_parts(o);
        let a0 = args.first().cloned().unwrap_or(Val::NULL);
        let find = |vm: &mut Vm, k: &Val| vm.index_of(&keys, k);
        let v = match name {
            "put" => self.map_put(o, a0, args[1].clone())?,
            "get" => find(self, &a0)?.map(|i| vals[i].clone()).unwrap_or(Val::NULL),
            "getOrDefault" => find(self, &a0)?.map(|i| vals[i].clone()).unwrap_or_else(|| args[1].clone()),
            "containsKey" => Val::bool(find(self, &a0)?.is_some()),
            "containsValue" => Val::bool(self.index_of(&vals, &a0)?.is_some()),
            "putIfAbsent" => match find(self, &a0)? {
                Some(i) if !vals[i].is_null() => vals[i].clone(),
                _ => self.map_put(o, a0, args[1].clone())?,
            },
            "remove" => match find(self, &a0)? {
                Some(i) => {
                    let (mut keys, mut vals) = (keys, vals);
                    keys.remove(i);
                    let old = vals.remove(i);
                    Self::set_map(o, keys, vals);
                    old
                }
                None => Val::NULL,
            },
            "size" => Val::int(keys.len() as i32),
            "isEmpty" => Val::bool(keys.is_empty()),
            "clear" => {
                Self::set_map(o, Vec::new(), Vec::new());
                Val::NULL
            }
            "putAll" => {
                let Val::R(src) = &a0 else { return self.npe("putAll(null)") };
                let (k2, v2, _) = Self::map_parts(src);
                for (k, v) in k2.into_iter().zip(v2) {
                    self.map_put(o, k, v)?;
                }
                Val::NULL
            }
            "keySet" | "navigableKeySet" => self.new_coll("Ljava/util/LinkedHashSet;", keys),
            "values" => self.new_coll("Ljava/util/ArrayList;", vals),
            "entrySet" => {
                let entries = keys
                    .into_iter()
                    .zip(vals)
                    .map(|(k, v)| Val::R(new_ref(Obj::Inst(Inst { class: Rc::from("Ljava/util/Map$Entry;"), fields: BTreeMap::new(), native: Native::Entry(k, v) }))))
                    .collect();
                self.new_coll("Ljava/util/LinkedHashSet;", entries)
            }
            "firstKey" | "lastKey" => match if name == "firstKey" { keys.first() } else { keys.last() } {
                Some(k) => k.clone(),
                None => return self.throw("Ljava/util/NoSuchElementException;", ""),
            },
            "toString" => {
                let mut s = String::from("{");
                for (i, (k, v)) in keys.iter().zip(&vals).enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    s.push_str(&self.to_jstring(k)?);
                    s.push('=');
                    s.push_str(&self.to_jstring(v)?);
                }
                s.push('}');
                self.jstring(s)
            }
            "equals" => {
                let Val::R(x) = &a0 else { return Ok(Some(Val::bool(false))) };
                let (k2, v2, _) = Self::map_parts(x);
                let mut eq = k2.len() == keys.len();
                for (k, v) in keys.iter().zip(&vals) {
                    if !eq {
                        break;
                    }
                    eq = match self.index_of(&k2, k)? {
                        Some(i) => self.jequals(v, &v2[i])?,
                        None => false,
                    };
                }
                Val::bool(eq)
            }
            "hashCode" => {
                let mut h = 0i32;
                for (k, v) in keys.iter().zip(&vals) {
                    h = h.wrapping_add(self.jhash(k)? ^ self.jhash(v)?);
                }
                Val::int(h)
            }
            _ => return Ok(None),
        };
        Ok(Some(v))
    }

    /// The next word of a `Scanner` (`peek`: leave it there).
    fn scan_token(&mut self, o: &Ref, peek: bool) -> JResult<Option<String>> {
        loop {
            let (line, more) = match &mut *o.borrow_mut() {
                Obj::Inst(Inst { native: Native::Scanner { line, text }, .. }) => (line.take(), text.as_mut().map(|t| t.pop())),
                _ => return Ok(None),
            };
            let line = match (line, more) {
                (Some(l), _) => Some(l),
                (None, Some(next)) => next,
                (None, None) => self.host.read_line(),
            };
            let Some(line) = line else { return Ok(None) };
            let rest = line.trim_start();
            if rest.is_empty() {
                continue;
            }
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            let token = rest[..end].to_string();
            let left = if peek { line.clone() } else { rest[end..].to_string() };
            if let Obj::Inst(Inst { native: Native::Scanner { line, .. }, .. }) = &mut *o.borrow_mut() {
                *line = Some(left);
            }
            return Ok(Some(token));
        }
    }

    fn scanner_method(&mut self, o: &Ref, name: &str) -> JResult<Option<Val>> {
        let missing = |vm: &mut Vm| vm.throw::<Option<Val>>("Ljava/util/NoSuchElementException;", "");
        let mismatch = |vm: &mut Vm, t: &str| vm.throw::<Option<Val>>("Ljava/util/InputMismatchException;", &format!("For input string: \"{t}\""));
        Ok(Some(match name {
            "nextLine" => {
                let (line, more) = match &mut *o.borrow_mut() {
                    Obj::Inst(Inst { native: Native::Scanner { line, text }, .. }) => (line.take(), text.as_mut().map(|t| t.pop())),
                    _ => (None, None),
                };
                let line = match (line, more) {
                    (Some(l), _) => Some(l),
                    (None, Some(next)) => next,
                    (None, None) => self.host.read_line(),
                };
                match line {
                    Some(l) => self.jstring(l),
                    None => return self.throw("Ljava/util/NoSuchElementException;", "No line found"),
                }
            }
            "hasNextLine" => {
                let has = match &mut *o.borrow_mut() {
                    Obj::Inst(Inst { native: Native::Scanner { line, text }, .. }) => line.is_some() || text.as_ref().is_some_and(|t| !t.is_empty()),
                    _ => false,
                };
                if has {
                    Val::bool(true)
                } else if matches!(&*o.borrow(), Obj::Inst(Inst { native: Native::Scanner { text: Some(_), .. }, .. })) {
                    Val::bool(false)
                } else {
                    let l = self.host.read_line();
                    let got = l.is_some();
                    if let Obj::Inst(Inst { native: Native::Scanner { line, .. }, .. }) = &mut *o.borrow_mut() {
                        *line = l;
                    }
                    Val::bool(got)
                }
            }
            "next" => match self.scan_token(o, false)? {
                Some(t) => self.jstring(t),
                None => return missing(self),
            },
            "hasNext" => Val::bool(self.scan_token(o, true)?.is_some()),
            "hasNextInt" | "hasNextLong" => Val::bool(self.scan_token(o, true)?.is_some_and(|t| text::parse_long(&t, 10, i64::MIN, i64::MAX).is_some())),
            "hasNextDouble" => Val::bool(self.scan_token(o, true)?.is_some_and(|t| text::parse_double(&t).is_some())),
            "nextInt" | "nextLong" | "nextShort" | "nextByte" => {
                let Some(t) = self.scan_token(o, true)? else { return missing(self) };
                let (min, max) = if name == "nextLong" { (i64::MIN, i64::MAX) } else { (i32::MIN as i64, i32::MAX as i64) };
                match text::parse_long(&t, 10, min, max) {
                    Some(v) => {
                        self.scan_token(o, false)?;
                        if name == "nextLong" {
                            Val::long(v)
                        } else {
                            Val::int(v as i32)
                        }
                    }
                    None => return mismatch(self, &t),
                }
            }
            "nextDouble" | "nextFloat" => {
                let Some(t) = self.scan_token(o, true)? else { return missing(self) };
                match text::parse_double(&t) {
                    Some(v) => {
                        self.scan_token(o, false)?;
                        if name == "nextDouble" {
                            Val::double(v)
                        } else {
                            Val::float(v as f32)
                        }
                    }
                    None => return mismatch(self, &t),
                }
            }
            "nextBoolean" => {
                let Some(t) = self.scan_token(o, false)? else { return missing(self) };
                Val::bool(t.eq_ignore_ascii_case("true"))
            }
            "close" => Val::NULL,
            _ => return Ok(None),
        }))
    }

    fn static_native(&mut self, class: &str, name: &str, sig: &str, args: Vec<Val>) -> JResult<Option<Val>> {
        let a0 = args.first().cloned().unwrap_or(Val::NULL);
        let arg_s = |k: usize| args.get(k).and_then(Self::str_of);
        let p0 = crate::params(sig).first().map(|s| s.to_string()).unwrap_or_default();
        let v = match class {
            "Ljava/lang/System;" => match name {
                "currentTimeMillis" => Val::long(self.host.time_ms() as i64),
                "nanoTime" => Val::long(self.host.uptime_ms() as i64 * 1_000_000),
                "exit" => return Err(Throw::Exit(a0.i())),
                "arraycopy" => {
                    let (Val::R(src), Val::R(dst)) = (&a0, &args[2]) else { return self.npe("arraycopy of null") };
                    let (sp, dp, n) = (args[1].i(), args[3].i(), args[4].i());
                    let data = Self::array(&a0).map(|a| a.1).unwrap_or_default();
                    let dlen = Self::array(&args[2]).map(|a| a.1.len()).unwrap_or(0);
                    if sp < 0 || dp < 0 || n < 0 || (sp + n) as usize > data.len() || (dp + n) as usize > dlen {
                        return self.throw("Ljava/lang/ArrayIndexOutOfBoundsException;", &format!("arraycopy: last source index {} out of bounds for length {}", sp + n, data.len()));
                    }
                    let _ = src;
                    if let Obj::Arr { data: d, .. } = &mut *dst.borrow_mut() {
                        d[dp as usize..(dp + n) as usize].clone_from_slice(&data[sp as usize..(sp + n) as usize]);
                    }
                    Val::NULL
                }
                "identityHashCode" => Val::int(a0.as_ref().map(identity).unwrap_or(0)),
                "lineSeparator" => self.jstring("\n".into()),
                "getProperty" | "getenv" => match arg_s(0).as_deref() {
                    Some("line.separator") => self.jstring("\n".into()),
                    Some("os.name") => self.jstring("JustOS".into()),
                    Some("java.version") => self.jstring("1.8".into()),
                    _ => args.get(1).cloned().unwrap_or(Val::NULL),
                },
                "gc" | "runFinalization" | "setOut" | "setErr" => Val::NULL,
                _ => return Ok(None),
            },
            "Ljava/lang/Math;" | "Ljava/lang/StrictMath;" => return self.math(name, sig, &args),
            "Ljava/lang/Integer;" | "Ljava/lang/Long;" | "Ljava/lang/Short;" | "Ljava/lang/Byte;" => {
                let kind = box_kind(class).unwrap_or(b'I');
                let long = kind == b'J';
                let mk = |v: i64| if long { Val::long(v) } else { Val::int(v as i32) };
                let x = if long { a0.j() } else { a0.i() as i64 };
                let unsigned = if long { x as u64 } else { x as u32 as u64 };
                match name {
                    "parseInt" | "parseLong" | "parseShort" | "parseByte" | "valueOf" | "decode" if p0 == "Ljava/lang/String;" => {
                        let s = arg_s(0).unwrap_or_else(|| "null".into());
                        let radix = if args.len() > 1 { args[1].i() as u32 } else { 10 };
                        let (min, max) = match kind {
                            b'J' => (i64::MIN, i64::MAX),
                            b'S' => (i16::MIN as i64, i16::MAX as i64),
                            b'B' => (i8::MIN as i64, i8::MAX as i64),
                            _ => (i32::MIN as i64, i32::MAX as i64),
                        };
                        let Some(v) = text::parse_long(&s, radix, min, max) else {
                            let extra = if radix != 10 { format!(" under radix {radix}") } else { String::new() };
                            return self.throw("Ljava/lang/NumberFormatException;", &format!("For input string: \"{s}\"{extra}"));
                        };
                        if name.starts_with("parse") {
                            mk(v)
                        } else {
                            self.boxed(kind, if long { v as u64 } else { v as i32 as u32 as u64 })
                        }
                    }
                    "valueOf" => self.boxed(kind, if long { x as u64 } else { x as i32 as u32 as u64 }),
                    "toString" if args.len() == 2 => self.jstring(text::radix(x.unsigned_abs(), args[1].i() as u32, x < 0)),
                    "toString" => self.jstring(x.to_string()),
                    "toBinaryString" => self.jstring(text::radix(unsigned, 2, false)),
                    "toHexString" => self.jstring(text::radix(unsigned, 16, false)),
                    "toOctalString" => self.jstring(text::radix(unsigned, 8, false)),
                    "compare" => {
                        let y = if long { args[1].j() } else { args[1].i() as i64 };
                        Val::int(x.cmp(&y) as i32)
                    }
                    "max" | "min" | "sum" => {
                        let y = if long { args[1].j() } else { args[1].i() as i64 };
                        mk(match name {
                            "max" => x.max(y),
                            "min" => x.min(y),
                            _ => x.wrapping_add(y),
                        })
                    }
                    "signum" => Val::int(x.signum() as i32),
                    "abs" => mk(x.wrapping_abs()),
                    "bitCount" => Val::int(unsigned.count_ones() as i32),
                    "hashCode" => Val::int(if long { (x ^ (x >> 32)) as i32 } else { x as i32 }),
                    "reverse" => mk(if long { (x as u64).reverse_bits() as i64 } else { (x as u32).reverse_bits() as i32 as i64 }),
                    "numberOfLeadingZeros" => Val::int(if long { unsigned.leading_zeros() } else { (unsigned as u32).leading_zeros() } as i32),
                    "numberOfTrailingZeros" => Val::int(if long { unsigned.trailing_zeros() } else { (unsigned as u32).trailing_zeros() } as i32),
                    "highestOneBit" => mk(if unsigned == 0 { 0 } else { 1i64 << (63 - unsigned.leading_zeros()) }),
                    _ => return Ok(None),
                }
            }
            "Ljava/lang/Double;" | "Ljava/lang/Float;" => {
                let dbl = class == "Ljava/lang/Double;";
                let x = if p0 == "D" { a0.d() } else { a0.f() as f64 };
                let mk = |v: f64| if dbl { Val::double(v) } else { Val::float(v as f32) };
                match name {
                    "parseDouble" | "parseFloat" | "valueOf" if p0 == "Ljava/lang/String;" => {
                        let s = arg_s(0).unwrap_or_default();
                        let Some(v) = text::parse_double(&s) else {
                            return self.throw("Ljava/lang/NumberFormatException;", &format!("For input string: \"{s}\""));
                        };
                        if name.starts_with("parse") {
                            mk(v)
                        } else if dbl {
                            self.boxed(b'D', v.to_bits())
                        } else {
                            self.boxed(b'F', (v as f32).to_bits() as u64)
                        }
                    }
                    "valueOf" => {
                        if dbl {
                            self.boxed(b'D', x.to_bits())
                        } else {
                            self.boxed(b'F', (x as f32).to_bits() as u64)
                        }
                    }
                    "toString" => self.jstring(if dbl { text::double(x) } else { text::float(x as f32) }),
                    "isNaN" => Val::bool(x.is_nan()),
                    "isInfinite" => Val::bool(x.is_infinite()),
                    "isFinite" => Val::bool(x.is_finite()),
                    "compare" => {
                        let y = if dbl { args[1].d() } else { args[1].f() as f64 };
                        Val::int(x.partial_cmp(&y).map(|o| o as i32).unwrap_or(if x.is_nan() { 1 } else { -1 }))
                    }
                    "max" | "min" | "sum" => {
                        let y = if dbl { args[1].d() } else { args[1].f() as f64 };
                        mk(match name {
                            "max" => x.max(y),
                            "min" => x.min(y),
                            _ => x + y,
                        })
                    }
                    "doubleToLongBits" | "doubleToRawLongBits" => Val::long(x.to_bits() as i64),
                    "longBitsToDouble" => Val::double(f64::from_bits(a0.j() as u64)),
                    "floatToIntBits" | "floatToRawIntBits" => Val::int((x as f32).to_bits() as i32),
                    "intBitsToFloat" => Val::float(f32::from_bits(a0.i() as u32)),
                    "hashCode" => {
                        let b = x.to_bits();
                        Val::int((b ^ (b >> 32)) as i32)
                    }
                    _ => return Ok(None),
                }
            }
            "Ljava/lang/Boolean;" => match name {
                "valueOf" if p0 == "Z" => self.boxed(b'Z', a0.z() as u64),
                "valueOf" | "parseBoolean" => {
                    let b = arg_s(0).is_some_and(|s| s.eq_ignore_ascii_case("true"));
                    if name == "valueOf" {
                        self.boxed(b'Z', b as u64)
                    } else {
                        Val::bool(b)
                    }
                }
                "toString" => self.jstring(a0.z().to_string()),
                "compare" => Val::int(a0.z() as i32 - args[1].z() as i32),
                "logicalAnd" => Val::bool(a0.z() && args[1].z()),
                "logicalOr" => Val::bool(a0.z() || args[1].z()),
                "logicalXor" => Val::bool(a0.z() ^ args[1].z()),
                _ => return Ok(None),
            },
            "Ljava/lang/Character;" => {
                let c = a0.i() as u32;
                let ch = char::from_u32(c).unwrap_or('\u{FFFD}');
                match name {
                    "valueOf" => self.boxed(b'C', c as u16 as u64),
                    "toString" => self.jstring(text::char_string(c as u16)),
                    "isDigit" => Val::bool(ch.is_numeric()),
                    "isLetter" | "isAlphabetic" => Val::bool(ch.is_alphabetic()),
                    "isLetterOrDigit" => Val::bool(ch.is_alphanumeric()),
                    "isWhitespace" | "isSpaceChar" => Val::bool(ch.is_whitespace()),
                    "isUpperCase" => Val::bool(ch.is_uppercase()),
                    "isLowerCase" => Val::bool(ch.is_lowercase()),
                    "toUpperCase" | "toLowerCase" | "toTitleCase" => {
                        let mut it = if name == "toLowerCase" { ch.to_lowercase().collect::<Vec<char>>() } else { ch.to_uppercase().collect() };
                        let out = if it.len() == 1 { it.pop().unwrap() as u32 } else { c };
                        Val::int(out as i32)
                    }
                    "getNumericValue" => Val::int(ch.to_digit(36).map(|d| d as i32).unwrap_or(-1)),
                    "digit" => Val::int(ch.to_digit(args[1].i() as u32).map(|d| d as i32).unwrap_or(-1)),
                    "forDigit" => Val::int(core::char::from_digit(c, args[1].i() as u32).map(|d| d as i32).unwrap_or(0)),
                    "compare" => Val::int(c as i32 - args[1].i()),
                    "isSurrogate" => Val::bool((0xD800..0xE000).contains(&c)),
                    "hashCode" => Val::int(c as i32),
                    _ => return Ok(None),
                }
            }
            "Ljava/lang/String;" => match name {
                "valueOf" | "copyValueOf" => {
                    let s = if sig.starts_with("([CII)") {
                        let u = self.char_array(&a0).unwrap_or_default();
                        let (from, n) = (args[1].i().max(0) as usize, args[2].i().max(0) as usize);
                        text::from_units(u.get(from..from + n).unwrap_or(&[]))
                    } else {
                        self.typed_string(&a0, &p0)?
                    };
                    self.jstring(s)
                }
                "format" => {
                    let (f, a) = if p0 == "Ljava/util/Locale;" { (1, 2) } else { (0, 1) };
                    let fmt = arg_s(f).unwrap_or_default();
                    let s = self.format_args(&fmt, &args[a])?;
                    self.jstring(s)
                }
                "join" => {
                    let sep = arg_s(0).unwrap_or_default();
                    let parts = match Self::array(&args[1]) {
                        Some((_, d)) => d,
                        None => self.elements(&args[1])?,
                    };
                    let mut s = String::new();
                    for (i, p) in parts.iter().enumerate() {
                        if i > 0 {
                            s.push_str(&sep);
                        }
                        s.push_str(&self.to_jstring(p)?);
                    }
                    self.jstring(s)
                }
                _ => return Ok(None),
            },
            "Ljava/lang/reflect/Array;" if name == "newInstance" => {
                let elem = match &a0 {
                    Val::R(c) => match &*c.borrow() {
                        Obj::Inst(Inst { native: Native::Class(n), .. }) => n.clone(),
                        _ => Rc::from(OBJ),
                    },
                    _ => return self.npe("Array.newInstance(null)"),
                };
                let dims: Vec<i32> = match Self::array(&args[1]) {
                    Some((_, d)) => d.iter().map(|v| v.i()).collect(),
                    None => vec![args[1].i()],
                };
                self.multi_array(&elem, &dims)?
            }
            "Ljava/util/Arrays;" => return self.arrays(name, sig, &args),
            "Ljava/util/Collections;" => return self.collections(name, &args),
            "Ljava/util/Objects;" => match name {
                "equals" => Val::bool(self.jequals(&a0, &args[1])? || (a0.is_null() && args[1].is_null())),
                "hashCode" => Val::int(if a0.is_null() { 0 } else { self.jhash(&a0)? }),
                "hash" => {
                    let mut h = 1i32;
                    for it in Self::array(&a0).map(|a| a.1).unwrap_or_default() {
                        h = h.wrapping_mul(31).wrapping_add(if it.is_null() { 0 } else { self.jhash(&it)? });
                    }
                    Val::int(h)
                }
                "toString" if a0.is_null() && args.len() > 1 => args[1].clone(),
                "toString" => {
                    let s = self.to_jstring(&a0)?;
                    self.jstring(s)
                }
                "requireNonNull" => {
                    if a0.is_null() {
                        let m = arg_s(1).unwrap_or_default();
                        return self.npe(&m);
                    }
                    a0
                }
                "isNull" => Val::bool(a0.is_null()),
                "nonNull" => Val::bool(!a0.is_null()),
                "requireNonNullElse" => {
                    if a0.is_null() {
                        args[1].clone()
                    } else {
                        a0
                    }
                }
                _ => return Ok(None),
            },
            "Ljava/lang/Thread;" => match name {
                "sleep" => {
                    let ms = a0.j().max(0) as u64;
                    self.host.sleep(ms);
                    Val::NULL
                }
                "currentThread" => {
                    let t = self.new_object("Ljava/lang/Thread;")?;
                    Val::R(t)
                }
                "yield" | "onSpinWait" => Val::NULL,
                _ => return Ok(None),
            },
            "Ljava/lang/Enum;" if name == "valueOf" => {
                let cls = match &a0 {
                    Val::R(c) => match &*c.borrow() {
                        Obj::Inst(Inst { native: Native::Class(n), .. }) => n.clone(),
                        _ => Rc::from(""),
                    },
                    _ => Rc::from(""),
                };
                let want = arg_s(1).unwrap_or_default();
                self.init_class(&cls)?;
                let values = self.class(&cls).and_then(|c| c.statics.borrow().get("$VALUES").cloned()).unwrap_or(Val::NULL);
                for v in Self::array(&values).map(|a| a.1).unwrap_or_default() {
                    let name = match &v {
                        Val::R(x) => match &*x.borrow() {
                            Obj::Inst(i) => i.fields.get("$name").and_then(Self::str_of),
                            _ => None,
                        },
                        _ => None,
                    };
                    if name.as_deref() == Some(want.as_str()) {
                        return Ok(Some(v));
                    }
                }
                return self.throw("Ljava/lang/IllegalArgumentException;", &format!("No enum constant {}.{want}", java_name(&cls)));
            }
            _ => return crate::android::static_method(self, class, name, sig, &args),
        };
        Ok(Some(v))
    }

    /// `new elem[d0][d1]...`.
    fn multi_array(&mut self, elem: &str, dims: &[i32]) -> JResult<Val> {
        let ty = format!("{}{elem}", "[".repeat(dims.len()));
        let Some((&n, rest)) = dims.split_first() else { return Ok(Val::NULL) };
        let arr = self.new_array(&ty, n)?;
        if !rest.is_empty() {
            let mut data = Vec::with_capacity(n.max(0) as usize);
            for _ in 0..n {
                data.push(self.multi_array(elem, rest)?);
            }
            if let Val::R(o) = &arr {
                if let Obj::Arr { data: d, .. } = &mut *o.borrow_mut() {
                    *d = data;
                }
            }
        }
        Ok(arr)
    }

    fn math(&mut self, name: &str, sig: &str, args: &[Val]) -> JResult<Option<Val>> {
        let p = crate::params(sig);
        let ty = p.first().copied().unwrap_or("D");
        let x = |k: usize| match ty {
            "I" => args[k].i() as f64,
            "J" => args[k].j() as f64,
            "F" => args[k].f() as f64,
            _ => args[k].d(),
        };
        let int = |k: usize| if ty == "J" { args[k].j() } else { args[k].i() as i64 };
        let typed = |v: f64| match ty {
            "F" => Val::float(v as f32),
            _ => Val::double(v),
        };
        let whole = |v: i64| if ty == "J" { Val::long(v) } else { Val::int(v as i32) };
        let overflow = |vm: &mut Vm| vm.throw::<Option<Val>>("Ljava/lang/ArithmeticException;", if ty == "J" { "long overflow" } else { "integer overflow" });
        Ok(Some(match name {
            "abs" if ty == "I" || ty == "J" => whole(int(0).wrapping_abs()),
            "abs" => typed(x(0).abs()),
            "max" | "min" if ty == "I" || ty == "J" => whole(if name == "max" { int(0).max(int(1)) } else { int(0).min(int(1)) }),
            "max" | "min" => {
                let (a, b) = (x(0), x(1));
                typed(if a.is_nan() || b.is_nan() {
                    f64::NAN
                } else if name == "max" {
                    a.max(b)
                } else {
                    a.min(b)
                })
            }
            "sqrt" => Val::double(float::sqrt(x(0))),
            "cbrt" => {
                let v = x(0);
                let r = float::powf(v.abs(), 1.0 / 3.0);
                let r = float::round(r * 1e12) / 1e12;
                Val::double(if v < 0.0 { -r } else { r })
            }
            "pow" => Val::double(float::powf(x(0), x(1))),
            "exp" => Val::double(float::exp(x(0))),
            "log" => Val::double(float::ln(x(0))),
            "log10" => Val::double(float::log10(x(0))),
            "log1p" => Val::double(float::ln(1.0 + x(0))),
            "expm1" => Val::double(float::exp(x(0)) - 1.0),
            "sin" => Val::double(float::sin(x(0))),
            "cos" => Val::double(float::cos(x(0))),
            "tan" => Val::double(float::tan(x(0))),
            "asin" => Val::double(float::asin(x(0))),
            "acos" => Val::double(float::acos(x(0))),
            "atan" => Val::double(float::atan(x(0))),
            "atan2" => Val::double(float::atan2(x(0), x(1))),
            "sinh" | "cosh" | "tanh" => {
                let (a, b) = (float::exp(x(0)), float::exp(-x(0)));
                Val::double(match name {
                    "sinh" => (a - b) / 2.0,
                    "cosh" => (a + b) / 2.0,
                    _ => (a - b) / (a + b),
                })
            }
            "hypot" => Val::double(float::hypot(x(0), x(1))),
            "floor" => Val::double(float::floor(x(0))),
            "ceil" => Val::double(float::ceil(x(0))),
            "rint" => {
                let v = x(0);
                let f = float::floor(v);
                let d = v - f;
                Val::double(if d > 0.5 || (d == 0.5 && f % 2.0 != 0.0) { f + 1.0 } else { f })
            }
            "round" if ty == "F" => Val::int(float::floor(x(0) + 0.5) as i32),
            "round" => Val::long(float::floor(x(0) + 0.5) as i64),
            "signum" => typed(if x(0) > 0.0 {
                1.0
            } else if x(0) < 0.0 {
                -1.0
            } else {
                x(0)
            }),
            "toRadians" => Val::double(x(0) / 180.0 * core::f64::consts::PI),
            "toDegrees" => Val::double(x(0) * 180.0 / core::f64::consts::PI),
            "random" => {
                if self.seed == 0 {
                    let t = self.host.time_ms() ^ self.host.uptime_ms().rotate_left(32);
                    self.seed = Self::seed_from(t);
                }
                let mut s = self.seed;
                let v = Self::random_double(&mut s);
                self.seed = s;
                Val::double(v)
            }
            "floorDiv" | "floorMod" => {
                let (a, b) = (int(0), int(1));
                if b == 0 {
                    return self.throw("Ljava/lang/ArithmeticException;", "/ by zero");
                }
                let q = a.wrapping_div(b) - if (a % b != 0) && ((a < 0) != (b < 0)) { 1 } else { 0 };
                whole(if name == "floorDiv" { q } else { a - q * b })
            }
            "addExact" | "subtractExact" | "multiplyExact" => {
                let (a, b) = (int(0), int(1));
                let r = match name {
                    "addExact" => a.checked_add(b),
                    "subtractExact" => a.checked_sub(b),
                    _ => a.checked_mul(b),
                };
                match r {
                    Some(v) if ty == "J" || (i32::MIN as i64..=i32::MAX as i64).contains(&v) => whole(v),
                    _ => return overflow(self),
                }
            }
            "negateExact" => whole(int(0).wrapping_neg()),
            "toIntExact" => {
                let v = args[0].j();
                if (i32::MIN as i64..=i32::MAX as i64).contains(&v) {
                    Val::int(v as i32)
                } else {
                    return overflow(self);
                }
            }
            _ => return Ok(None),
        }))
    }

    fn arrays(&mut self, name: &str, sig: &str, args: &[Val]) -> JResult<Option<Val>> {
        let a0 = args.first().cloned().unwrap_or(Val::NULL);
        let arr = Self::array(&a0);
        let (ty, data) = match (&arr, name) {
            (Some(a), _) => a.clone(),
            (None, "asList") => (Rc::from("[Ljava/lang/Object;"), Vec::new()),
            (None, "toString" | "deepToString") => return Ok(Some(self.jstring("null".into()))),
            (None, _) => return self.npe("null array"),
        };
        let elem = ty[1..].to_string();
        let prim = elem.len() == 1;
        Ok(Some(match name {
            "toString" | "deepToString" => {
                let mut s = String::from("[");
                for (i, v) in data.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    let part = if name == "deepToString" && Self::array(v).is_some() {
                        let Some(Val::R(r)) = self.arrays("deepToString", sig, core::slice::from_ref(v))? else { return Ok(None) };
                        Self::str_of(&Val::R(r)).unwrap_or_default()
                    } else {
                        self.typed_string(v, &elem)?
                    };
                    s.push_str(&part);
                }
                s.push(']');
                self.jstring(s)
            }
            "sort" | "parallelSort" => {
                let ps = crate::params(sig);
                let (from, to) = if ps.len() >= 3 && ps[1] == "I" { (args[1].i().max(0) as usize, (args[2].i().max(0) as usize).min(data.len())) } else { (0, data.len()) };
                let cmp = if ps.last().is_some_and(|p| *p == "Ljava/util/Comparator;") { args[args.len() - 1].clone() } else { Val::NULL };
                let mut data = data;
                let part: Vec<Val> = data[from.min(to)..to].to_vec();
                let sorted = if prim {
                    let mut part = part;
                    match elem.as_str() {
                        "J" => part.sort_by_key(|v| v.j()),
                        "D" => part.sort_by(|a, b| a.d().total_cmp(&b.d())),
                        "F" => part.sort_by(|a, b| a.f().total_cmp(&b.f())),
                        "C" => part.sort_by_key(|v| v.i() as u16),
                        _ => part.sort_by_key(|v| v.i()),
                    }
                    part
                } else {
                    self.sort_vals(part, &cmp)?
                };
                data.splice(from.min(to)..to, sorted);
                if let Some(Val::R(o)) = Some(a0) {
                    if let Obj::Arr { data: d, .. } = &mut *o.borrow_mut() {
                        *d = data;
                    }
                }
                Val::NULL
            }
            "fill" => {
                let (from, to, v) = if args.len() == 4 { (args[1].i().max(0) as usize, args[2].i().max(0) as usize, args[3].clone()) } else { (0, data.len(), args[1].clone()) };
                if let Val::R(o) = &a0 {
                    if let Obj::Arr { data: d, .. } = &mut *o.borrow_mut() {
                        for slot in d.iter_mut().take(to).skip(from) {
                            *slot = v.clone();
                        }
                    }
                }
                Val::NULL
            }
            "asList" => self.new_coll("Ljava/util/ArrayList;", data),
            "copyOf" => {
                let n = args[1].i();
                if n < 0 {
                    return self.throw("Ljava/lang/NegativeArraySizeException;", &format!("{n}"));
                }
                let mut d = data;
                d.resize(n as usize, Val::NULL);
                Self::new_array_of(&ty, d)
            }
            "copyOfRange" => {
                let (from, to) = (args[1].i(), args[2].i());
                if from < 0 || from as usize > data.len() || from > to {
                    return self.throw("Ljava/lang/ArrayIndexOutOfBoundsException;", &format!("{from}"));
                }
                let mut d: Vec<Val> = data[from as usize..(to as usize).min(data.len())].to_vec();
                d.resize((to - from) as usize, Val::NULL);
                Self::new_array_of(&ty, d)
            }
            "equals" => {
                let other = Self::array(&args[1]).map(|a| a.1);
                let eq = match other {
                    Some(o) if o.len() == data.len() => {
                        let mut eq = true;
                        for (a, b) in data.iter().zip(&o) {
                            let same = if prim { a.same(b) } else { self.jequals(a, b)? || (a.is_null() && b.is_null()) };
                            if !same {
                                eq = false;
                                break;
                            }
                        }
                        eq
                    }
                    _ => false,
                };
                Val::bool(eq)
            }
            "hashCode" => {
                let mut h = 1i32;
                for it in &data {
                    let e = if prim { it.i() } else if it.is_null() { 0 } else { self.jhash(it)? };
                    h = h.wrapping_mul(31).wrapping_add(e);
                }
                Val::int(h)
            }
            "binarySearch" => {
                let key = args[args.len() - 1].clone();
                let (mut lo, mut hi) = (0i32, data.len() as i32 - 1);
                let mut found = None;
                while lo <= hi {
                    let mid = (lo + hi) / 2;
                    let c = if prim {
                        match elem.as_str() {
                            "D" | "F" => data[mid as usize].d().partial_cmp(&key.d()).map(|o| o as i32).unwrap_or(0),
                            "J" => data[mid as usize].j().cmp(&key.j()) as i32,
                            _ => data[mid as usize].i().cmp(&key.i()) as i32,
                        }
                    } else {
                        self.compare(&data[mid as usize], &key, &Val::NULL)?
                    };
                    match c.cmp(&0) {
                        Ordering::Less => lo = mid + 1,
                        Ordering::Greater => hi = mid - 1,
                        Ordering::Equal => {
                            found = Some(mid);
                            break;
                        }
                    }
                }
                Val::int(found.unwrap_or(-(lo + 1)))
            }
            _ => return Ok(None),
        }))
    }

    fn collections(&mut self, name: &str, args: &[Val]) -> JResult<Option<Val>> {
        let a0 = args.first().cloned().unwrap_or(Val::NULL);
        let list = || match &a0 {
            Val::R(o) => Some(o.clone()),
            _ => None,
        };
        Ok(Some(match name {
            "sort" => {
                let Some(o) = list() else { return self.npe("sort(null)") };
                let items = Self::coll_items(&o);
                let cmp = args.get(1).cloned().unwrap_or(Val::NULL);
                let sorted = self.sort_vals(items, &cmp)?;
                Self::set_items(&o, sorted);
                Val::NULL
            }
            "reverse" => {
                let Some(o) = list() else { return self.npe("reverse(null)") };
                let mut items = Self::coll_items(&o);
                items.reverse();
                Self::set_items(&o, items);
                Val::NULL
            }
            "shuffle" => {
                let Some(o) = list() else { return self.npe("shuffle(null)") };
                let mut items = Self::coll_items(&o);
                let mut seed = match args.get(1) {
                    Some(Val::R(r)) => match &*r.borrow() {
                        Obj::Inst(Inst { native: Native::Random(s), .. }) => *s,
                        _ => 1,
                    },
                    _ => Self::seed_from(self.host.time_ms() ^ self.host.uptime_ms() << 20),
                };
                for i in (1..items.len()).rev() {
                    let j = Self::random_int(&mut seed, i as i32 + 1) as usize;
                    items.swap(i, j);
                }
                if let Some(Val::R(r)) = args.get(1) {
                    if let Obj::Inst(Inst { native: Native::Random(s), .. }) = &mut *r.borrow_mut() {
                        *s = seed;
                    }
                }
                Self::set_items(&o, items);
                Val::NULL
            }
            "swap" => {
                let Some(o) = list() else { return self.npe("swap(null)") };
                let mut items = Self::coll_items(&o);
                let (i, j) = (args[1].i() as usize, args[2].i() as usize);
                if i >= items.len() || j >= items.len() {
                    return self.throw("Ljava/lang/IndexOutOfBoundsException;", "");
                }
                items.swap(i, j);
                Self::set_items(&o, items);
                Val::NULL
            }
            "max" | "min" => {
                let items = self.elements(&a0)?;
                let cmp = args.get(1).cloned().unwrap_or(Val::NULL);
                let mut best: Option<Val> = None;
                for it in items {
                    best = Some(match best {
                        None => it,
                        Some(b) => {
                            let c = self.compare(&it, &b, &cmp)?;
                            if (name == "max" && c > 0) || (name == "min" && c < 0) {
                                it
                            } else {
                                b
                            }
                        }
                    });
                }
                match best {
                    Some(b) => b,
                    None => return self.throw("Ljava/util/NoSuchElementException;", ""),
                }
            }
            "frequency" => {
                let items = self.elements(&a0)?;
                let mut n = 0;
                for it in &items {
                    if self.jequals(it, &args[1])? || (it.is_null() && args[1].is_null()) {
                        n += 1;
                    }
                }
                Val::int(n)
            }
            "addAll" => {
                let Some(o) = list() else { return self.npe("addAll(null)") };
                for v in Self::array(&args[1]).map(|a| a.1).unwrap_or_default() {
                    self.coll_add(&o, v)?;
                }
                Val::bool(true)
            }
            "unmodifiableList" | "unmodifiableSet" | "unmodifiableMap" | "unmodifiableCollection" | "synchronizedList" | "synchronizedMap" => a0,
            "emptyList" => self.new_coll("Ljava/util/ArrayList;", Vec::new()),
            "emptySet" => self.new_coll("Ljava/util/HashSet;", Vec::new()),
            "emptyMap" => Val::R(self.new_object("Ljava/util/HashMap;")?),
            "singletonList" | "singleton" => self.new_coll(if name == "singleton" { "Ljava/util/HashSet;" } else { "Ljava/util/ArrayList;" }, vec![a0]),
            "nCopies" => self.new_coll("Ljava/util/ArrayList;", vec![args[1].clone(); a0.i().max(0) as usize]),
            "reverseOrder" if args.is_empty() => Val::R(new_ref(Obj::Inst(Inst { class: Rc::from("Ljava/util/Collections$ReverseComparator;"), fields: BTreeMap::new(), native: Native::None }))),
            _ => return Ok(None),
        }))
    }
}

/// `String.compareTo`: by UTF-16 units, then by length.
fn compare_str(a: &str, b: &str) -> i32 {
    let (x, y) = (text::units(a), text::units(b));
    for (p, q) in x.iter().zip(&y) {
        if p != q {
            return *p as i32 - *q as i32;
        }
    }
    x.len() as i32 - y.len() as i32
}

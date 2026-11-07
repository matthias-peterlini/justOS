//! JustVM instruction set, compiled program representation, binary
//! (de)serialization, a load-time verifier and a disassembler.
//!
//! Instructions are a one-byte opcode followed by little-endian operands.
//! Jump targets are absolute byte offsets inside the owning function.

use crate::prelude::*;
use core::fmt::{self, Write as _};

pub use crate::ui::{Attr, UiKind};

macro_rules! opcodes {
    ($( $(#[$doc:meta])* $name:ident = $val:literal, $len:literal; )*) => {
        /// A single VM opcode.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        #[repr(u8)]
        pub enum Op { $( $(#[$doc])* $name = $val, )* }

        impl Op {
            #[inline]
            pub fn from_u8(byte: u8) -> Option<Op> {
                match byte { $( $val => Some(Op::$name), )* _ => None }
            }

            /// Number of operand bytes following the opcode.
            #[inline]
            pub fn operand_len(self) -> usize {
                match self { $( Op::$name => $len, )* }
            }

            pub fn mnemonic(self) -> &'static str {
                match self { $( Op::$name => stringify!($name), )* }
            }
        }
    };
}

opcodes! {
    /// `u16` constant index → push constant.
    Const = 0, 2;
    Nil = 1, 0;
    True = 2, 0;
    False = 3, 0;
    Pop = 4, 0;
    /// Duplicate the two topmost values (`a b` → `a b a b`).
    Dup2 = 5, 0;
    GetLocal = 6, 2;
    SetLocal = 7, 2;
    GetGlobal = 8, 2;
    SetGlobal = 9, 2;
    /// `u16` function index → push function value.
    Func = 10, 2;
    /// `u8` builtin index → push builtin function value.
    Builtin = 11, 1;
    Add = 12, 0;
    Sub = 13, 0;
    Mul = 14, 0;
    Div = 15, 0;
    Rem = 16, 0;
    Neg = 17, 0;
    Not = 18, 0;
    Eq = 19, 0;
    Ne = 20, 0;
    Lt = 21, 0;
    Le = 22, 0;
    Gt = 23, 0;
    Ge = 24, 0;
    Range = 25, 0;
    /// `u32` absolute target.
    Jump = 26, 4;
    /// Pop a bool; jump to `u32` target when false.
    JumpIfFalse = 27, 4;
    /// Short-circuit `and`: if top is false jump (keeping it), else pop.
    JumpIfFalseKeep = 28, 4;
    /// Short-circuit `or`: if top is true jump (keeping it), else pop.
    JumpIfTrueKeep = 29, 4;
    /// `u8` argument count; callee sits below the arguments.
    Call = 30, 1;
    Return = 31, 0;
    /// `u16` element count.
    List = 32, 2;
    Index = 33, 0;
    /// Stack: container, index, value.
    SetIndex = 34, 0;
    /// `u8` argument count: bind callee + args into a callable (event handlers).
    Bind = 35, 1;
    /// `u16` part count: stringify and concatenate (string interpolation).
    Concat = 36, 2;
    /// Replace the iterable on top of the stack with an iterator.
    IterInit = 37, 0;
    /// `u16` local slot holding the iterator, `u32` exit target.
    IterNext = 38, 6;
    /// Format 1 container: `u8` UiKind (window, column or row); pops the
    /// optional argument. Superseded by `UiBegin`, still executed.
    UiOpen = 39, 1;
    /// Closes a `UiOpen` or `UiBegin` element.
    UiClose = 40, 0;
    /// Format 1 label: pops the text. Superseded by `UiBegin`.
    UiLabel = 41, 0;
    /// Format 1 button: pops the action (or nil) then the text. Superseded by `UiBegin`.
    UiButton = 42, 0;
    /// `u8` UiKind; pops the element argument (nil when absent) and opens the
    /// element. Attributes, the handler and children follow until `UiClose`.
    UiBegin = 43, 1;
    /// `u8` Attr; pops the value and sets it on the innermost open element.
    UiAttr = 44, 1;
    /// Pops the event handler of the innermost open element.
    UiOn = 45, 0;
    /// Duplicate the topmost value.
    Dup = 46, 0;
    /// `u16` entry count: pops that many key/value pairs into a new map.
    Map = 47, 2;
    /// `u16` constant (field name): pops a struct, pushes the field.
    GetField = 48, 2;
    /// `u16` constant (field name). Stack: struct, value.
    SetField = 49, 2;
    /// `u16` type: pops the struct's fields (declaration order).
    NewStruct = 50, 2;
    /// `u16` type, `u16` variant: pops the variant's values.
    NewVariant = 51, 4;
    /// `u16` type, `u16` variant ([`ANY_VARIANT`] for structs): pops a
    /// value, pushes whether it has that type (and variant).
    IsType = 52, 4;
    /// `u8` position: pops a struct or variant, pushes its value at that position.
    GetItem = 53, 1;
    /// Stack: value, lo, hi → whether `value` is a number with `lo <= value < hi`.
    InRange = 54, 0;
    /// `u16` constant (method name), `u8` argument count. Stack: fallback
    /// callable (or nil), receiver, arguments. Calls the method of the
    /// receiver's type, else the fallback with the receiver first.
    Method = 55, 3;
    /// `u16` function, `u8` capture count: pops the captured values.
    Closure = 56, 3;
    /// `u8` capture index of the running closure.
    GetCapture = 57, 1;
    /// Push the running function itself (recursion in nested functions).
    Current = 58, 0;
    /// Pops an Option or Result: pushes the value of `Some`/`Ok`, or returns
    /// `None`/`Err` from the current function.
    Try = 59, 0;
}

/// `IsType` operand for "any variant" (and for structs).
pub const ANY_VARIANT: u16 = u16::MAX;

/// Built-in types, always present at these indices.
pub const OPTION_TYPE: u16 = 0;
pub const RESULT_TYPE: u16 = 1;
pub const SOME: u16 = 0;
pub const NONE: u16 = 1;
pub const OK: u16 = 0;
pub const ERR: u16 = 1;

#[derive(Clone, Debug, PartialEq)]
pub enum TypeShape {
    /// Field names, in declaration order.
    Struct(Vec<String>),
    /// Variant names and their number of values.
    Enum(Vec<(String, u8)>),
}

/// A `struct` or `enum` declared by the program (or built in).
#[derive(Clone, Debug, PartialEq)]
pub struct TypeDef {
    pub name: String,
    pub shape: TypeShape,
    /// Methods that take `self`, callable as `value.name(...)`.
    pub methods: Vec<(String, u16)>,
}

impl TypeDef {
    /// `Option` and `Result`, the first two types of every program.
    pub fn builtins() -> Vec<TypeDef> {
        let enum_def = |name: &str, variants: [(&str, u8); 2]| TypeDef {
            name: name.into(),
            shape: TypeShape::Enum(variants.iter().map(|&(n, a)| (n.to_owned(), a)).collect()),
            methods: Vec::new(),
        };
        vec![enum_def("Option", [("Some", 1), ("None", 0)]), enum_def("Result", [("Ok", 1), ("Err", 1)])]
    }

    pub fn fields(&self) -> &[String] {
        match &self.shape {
            TypeShape::Struct(fields) => fields,
            TypeShape::Enum(_) => &[],
        }
    }

    pub fn variants(&self) -> &[(String, u8)] {
        match &self.shape {
            TypeShape::Struct(_) => &[],
            TypeShape::Enum(variants) => variants,
        }
    }

    pub fn method(&self, name: &str) -> Option<u16> {
        self.methods.iter().find(|(n, _)| n == name).map(|&(_, f)| f)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuiltinSpec {
    pub name: &'static str,
    pub min_args: u8,
    /// `None` = variadic.
    pub max_args: Option<u8>,
    pub doc: &'static str,
}

const fn builtin(name: &'static str, min_args: u8, max_args: Option<u8>, doc: &'static str) -> BuiltinSpec {
    BuiltinSpec { name, min_args, max_args, doc }
}

/// Native functions. The index in this table is the `Builtin` operand, so
/// entries are only ever appended; the VM implements them in the same order.
pub const BUILTINS: &[BuiltinSpec] = &[
    builtin("print", 0, None, "print(values...) — write values separated by spaces"),
    builtin("len", 1, Some(1), "len(x) — length of a string or list"),
    builtin("str", 1, Some(1), "str(x) — convert to string"),
    builtin("int", 1, Some(1), "int(x) — convert to integer"),
    builtin("float", 1, Some(1), "float(x) — convert to float"),
    builtin("push", 2, Some(2), "push(list, value) — append to a list"),
    builtin("pop", 1, Some(1), "pop(list) — remove and return the last element"),
    builtin("type_of", 1, Some(1), "type_of(x) — name of the value's type"),
    builtin("clock", 0, Some(0), "clock() — seconds since program start"),
    builtin("abs", 1, Some(1), "abs(n) — absolute value"),
    builtin("min", 1, Some(2), "min(a, b) or min(list) — smaller value"),
    builtin("max", 1, Some(2), "max(a, b) or max(list) — larger value"),
    builtin("sqrt", 1, Some(1), "sqrt(n) — square root"),
    builtin("join", 2, Some(2), "join(list, sep) — concatenate list items"),
    builtin("contains", 2, Some(2), "contains(haystack, needle) — substring, element or map key test"),
    builtin("get", 2, Some(2), "get(collection, key) — Some(value) for a map key or list index, else None"),
    builtin("keys", 1, Some(1), "keys(map) — list of the keys, sorted"),
    builtin("values", 1, Some(1), "values(map) — list of the values, in key order"),
    builtin("remove", 2, Some(2), "remove(collection, key) — remove a map key or list index; returns Some(value) or None"),
    builtin("insert", 3, Some(3), "insert(list, index, value) — insert into a list at index"),
    builtin("unwrap", 1, Some(1), "unwrap(x) — the value of Some/Ok; error on None/Err"),
    builtin("expect", 2, Some(2), "expect(x, message) — like unwrap, with an error message"),
    builtin("unwrap_or", 2, Some(2), "unwrap_or(x, default) — the value of Some/Ok, else default"),
    builtin("is_some", 1, Some(1), "is_some(option) — whether it is Some"),
    builtin("is_none", 1, Some(1), "is_none(option) — whether it is None"),
    builtin("is_ok", 1, Some(1), "is_ok(result) — whether it is Ok"),
    builtin("is_err", 1, Some(1), "is_err(result) — whether it is Err"),
    builtin("panic", 1, Some(1), "panic(message) — stop the program with an error"),
    builtin("assert", 1, Some(2), "assert(condition, message?) — panic when the condition is false"),
    builtin("copy", 1, Some(1), "copy(x) — shallow copy of a list, map or struct"),
    // ---- strings
    builtin("split", 1, Some(2), "split(text, sep?) — list of the parts between `sep`; without `sep`, the words"),
    builtin("trim", 1, Some(1), "trim(text) — without leading and trailing whitespace"),
    builtin("trim_start", 1, Some(1), "trim_start(text) — without leading whitespace"),
    builtin("trim_end", 1, Some(1), "trim_end(text) — without trailing whitespace"),
    builtin("upper", 1, Some(1), "upper(text) — in upper case"),
    builtin("lower", 1, Some(1), "lower(text) — in lower case"),
    builtin("replace", 3, Some(3), "replace(text, from, to) — every `from` replaced by `to`"),
    builtin("starts_with", 2, Some(2), "starts_with(text, prefix) — whether text begins with prefix"),
    builtin("ends_with", 2, Some(2), "ends_with(text, suffix) — whether text ends with suffix"),
    builtin("index_of", 2, Some(2), "index_of(text or list, needle) — Some(first position) or None"),
    builtin("chars", 1, Some(1), "chars(text) — list of the characters"),
    builtin("repeat", 2, Some(2), "repeat(text or list, n) — n copies joined together"),
    builtin("slice", 2, Some(3), "slice(text or list, start, end?) — the part from start to end (exclusive); negative counts from the end"),
    builtin("lines", 1, Some(1), "lines(text) — list of the lines"),
    builtin("pad_start", 2, Some(3), "pad_start(text, width, fill?) — padded on the left to width"),
    builtin("pad_end", 2, Some(3), "pad_end(text, width, fill?) — padded on the right to width"),
    builtin("ord", 1, Some(1), "ord(char) — the Unicode code of a one-character string"),
    builtin("chr", 1, Some(1), "chr(code) — the one-character string for a Unicode code"),
    // ---- lists
    builtin("map", 2, Some(2), "map(list, f) — new list of f(item) for each item"),
    builtin("filter", 2, Some(2), "filter(list, f) — new list of the items where f(item) is true"),
    builtin("reduce", 3, Some(3), "reduce(list, initial, f) — fold: acc = f(acc, item) for each item"),
    builtin("sort", 1, Some(2), "sort(list, key?) — sort in place, by key(item) when given"),
    builtin("sorted", 1, Some(2), "sorted(list, key?) — sorted copy"),
    builtin("reverse", 1, Some(1), "reverse(list) — reverse in place"),
    builtin("reversed", 1, Some(1), "reversed(list or text) — reversed copy"),
    builtin("sum", 1, Some(1), "sum(list) — sum of the numbers"),
    builtin("any", 1, Some(2), "any(list, f?) — whether some item (or f(item)) is true"),
    builtin("all", 1, Some(2), "all(list, f?) — whether every item (or f(item)) is true"),
    builtin("first", 1, Some(2), "first(list, f?) — Some(first item, or first where f(item) is true) or None"),
    builtin("last", 1, Some(1), "last(list) — Some(last item) or None"),
    builtin("range", 2, Some(3), "range(start, end, step?) — list of ints from start up to end (exclusive)"),
    builtin("zip", 2, Some(2), "zip(a, b) — list of [a[i], b[i]] pairs"),
    builtin("items", 1, Some(1), "items(map) — list of [key, value] pairs, in key order"),
    builtin("extend", 2, Some(2), "extend(list, other) — append every item of other"),
    builtin("clear", 1, Some(1), "clear(list or map) — remove every item"),
    // ---- input and parsing
    builtin("read_line", 0, Some(1), "read_line(prompt?) — Some(line typed on standard input) or None at end of input"),
    builtin("parse_int", 1, Some(1), "parse_int(text) — Some(int) or None"),
    builtin("parse_float", 1, Some(1), "parse_float(text) — Some(float) or None"),
    builtin("ok_or", 2, Some(2), "ok_or(option, error) — Ok(value) for Some(value), Err(error) for None"),
    // ---- import math
    builtin("math.sin", 1, Some(1), "math.sin(x) — sine (radians)"),
    builtin("math.cos", 1, Some(1), "math.cos(x) — cosine (radians)"),
    builtin("math.tan", 1, Some(1), "math.tan(x) — tangent (radians)"),
    builtin("math.asin", 1, Some(1), "math.asin(x) — arc sine"),
    builtin("math.acos", 1, Some(1), "math.acos(x) — arc cosine"),
    builtin("math.atan", 1, Some(1), "math.atan(x) — arc tangent"),
    builtin("math.atan2", 2, Some(2), "math.atan2(y, x) — angle of the point (x, y)"),
    builtin("math.exp", 1, Some(1), "math.exp(x) — e to the power x"),
    builtin("math.log", 1, Some(2), "math.log(x, base?) — logarithm, natural by default"),
    builtin("math.log10", 1, Some(1), "math.log10(x) — base-10 logarithm"),
    builtin("math.log2", 1, Some(1), "math.log2(x) — base-2 logarithm"),
    builtin("math.pow", 2, Some(2), "math.pow(x, y) — x to the power y (int when both are ints and y >= 0)"),
    builtin("math.floor", 1, Some(1), "math.floor(x) — largest int <= x"),
    builtin("math.ceil", 1, Some(1), "math.ceil(x) — smallest int >= x"),
    builtin("math.round", 1, Some(2), "math.round(x, digits?) — nearest int, or float rounded to digits decimals"),
    builtin("math.trunc", 1, Some(1), "math.trunc(x) — x without its fractional part, as int"),
    builtin("math.sign", 1, Some(1), "math.sign(x) — -1, 0 or 1"),
    builtin("math.clamp", 3, Some(3), "math.clamp(x, lo, hi) — x limited to lo..=hi"),
    builtin("math.hypot", 2, Some(2), "math.hypot(x, y) — length of the vector (x, y)"),
    builtin("math.is_nan", 1, Some(1), "math.is_nan(x) — whether x is NaN"),
    builtin("math.is_finite", 1, Some(1), "math.is_finite(x) — whether x is neither infinite nor NaN"),
    builtin("math.sqrt", 1, Some(1), "math.sqrt(x) — square root"),
    builtin("math.abs", 1, Some(1), "math.abs(x) — absolute value"),
    builtin("math.min", 1, Some(2), "math.min(a, b) or math.min(list) — smaller value"),
    builtin("math.max", 1, Some(2), "math.max(a, b) or math.max(list) — larger value"),
    // ---- import random
    builtin("random.seed", 1, Some(1), "random.seed(n) — make the following numbers repeatable"),
    builtin("random.int", 2, Some(2), "random.int(lo, hi) — random int with lo <= n < hi"),
    builtin("random.float", 0, Some(0), "random.float() — random float in [0, 1)"),
    builtin("random.choice", 1, Some(1), "random.choice(list) — a random item"),
    builtin("random.shuffle", 1, Some(1), "random.shuffle(list) — shuffle in place"),
    // ---- import time (UTC)
    builtin("time.now", 0, Some(0), "time.now() — seconds since 1970-01-01 UTC, as float"),
    builtin("time.sleep", 1, Some(1), "time.sleep(seconds) — pause the program"),
    builtin("time.date", 0, Some(1), "time.date(timestamp?) — map of year, month, day, hour, minute, second, weekday (UTC)"),
    builtin(
        "time.format",
        1,
        Some(2),
        "time.format(timestamp, pattern?) — text such as \"2026-10-21 09:30:00\" (%Y %m %d %H %M %S %j %a %b, UTC)",
    ),
    builtin("time.timestamp", 3, Some(6), "time.timestamp(year, month, day, hour?, minute?, second?) — seconds since 1970 (UTC)"),
    // ---- import fs
    builtin("fs.read", 1, Some(1), "fs.read(path) — Ok(text) or Err(message)"),
    builtin("fs.write", 2, Some(2), "fs.write(path, text) — replace the file's content; Ok(nil) or Err(message)"),
    builtin("fs.append", 2, Some(2), "fs.append(path, text) — add to the end of the file; Ok(nil) or Err(message)"),
    builtin("fs.exists", 1, Some(1), "fs.exists(path) — whether a file or directory exists"),
    builtin("fs.is_file", 1, Some(1), "fs.is_file(path) — whether path is a file"),
    builtin("fs.is_dir", 1, Some(1), "fs.is_dir(path) — whether path is a directory"),
    builtin("fs.list", 1, Some(1), "fs.list(dir) — Ok(sorted list of entry names) or Err(message)"),
    builtin("fs.mkdir", 1, Some(1), "fs.mkdir(path) — create a directory and its parents; Ok(nil) or Err(message)"),
    builtin("fs.remove", 1, Some(1), "fs.remove(path) — delete a file or an empty directory; Ok(nil) or Err(message)"),
    builtin("fs.rename", 2, Some(2), "fs.rename(from, to) — move or rename; Ok(nil) or Err(message)"),
    builtin("fs.copy", 2, Some(2), "fs.copy(from, to) — copy a file; Ok(nil) or Err(message)"),
    // ---- import path
    builtin("path.join", 1, None, "path.join(parts...) — parts joined with the system separator"),
    builtin("path.name", 1, Some(1), "path.name(path) — the last component (\"a/b.txt\" → \"b.txt\")"),
    builtin("path.stem", 1, Some(1), "path.stem(path) — the file name without extension"),
    builtin("path.ext", 1, Some(1), "path.ext(path) — the extension without the dot, or \"\""),
    builtin("path.parent", 1, Some(1), "path.parent(path) — the directory containing path"),
    // ---- import json
    builtin("json.parse", 1, Some(1), "json.parse(text) — Ok(value) or Err(message); objects become maps, null becomes nil"),
    builtin("json.stringify", 1, Some(2), "json.stringify(value, indent?) — JSON text; structs become objects"),
    // ---- import os
    builtin("os.args", 0, Some(0), "os.args() — list of the command-line arguments"),
    builtin("os.env", 1, Some(1), "os.env(name) — Some(value of the environment variable) or None"),
    builtin("os.cwd", 0, Some(0), "os.cwd() — the current directory"),
    builtin("os.exit", 0, Some(1), "os.exit(code?) — stop the program with an exit code (default 0)"),
    builtin("os.run", 1, Some(2), "os.run(program, args?) — run a program; Ok({code, stdout, stderr}) or Err(message)"),
    builtin("os.name", 0, Some(0), "os.name — the operating system: \"windows\", \"linux\" or \"macos\""),
    builtin("os.sep", 0, Some(0), "os.sep — the path separator: \"\\\\\" on Windows, \"/\" elsewhere"),
    // ---- import http
    builtin("http.get", 1, Some(2), "http.get(url, headers?) — Ok({status, body, headers}) or Err(message)"),
    builtin(
        "http.post",
        2,
        Some(3),
        "http.post(url, body, headers?) — Ok({status, body, headers}) or Err(message); a map or list body is sent as JSON",
    ),
    builtin("http.request", 2, Some(4), "http.request(method, url, body?, headers?) — Ok({status, body, headers}) or Err(message)"),
    // ---- 0.5 (the index is the bytecode encoding: new builtins go last)
    builtin("os.notify", 1, Some(2), "os.notify(title, body?) — show a desktop notification; Ok(id) or Err(message)"),
];

/// Standard library modules, used after `import name`. Their functions are
/// the builtins named `name.function`.
pub const STD_MODULES: &[&str] = &["math", "random", "time", "fs", "path", "json", "os", "http"];

/// Builtins without arguments read like values (`os.name`): they are
/// evaluated at runtime, on the system running the program.
pub const STD_PROPERTIES: &[&str] = &["os.name", "os.sep"];

/// Constant values of standard library modules (`math.pi`).
pub const STD_CONSTANTS: &[(&str, f64)] = &[
    ("math.pi", core::f64::consts::PI),
    ("math.tau", core::f64::consts::TAU),
    ("math.e", core::f64::consts::E),
    ("math.inf", f64::INFINITY),
    ("math.nan", f64::NAN),
];

/// The names of `module`'s functions and values.
pub fn std_members(module: &str) -> Vec<&'static str> {
    let prefix = format!("{module}.");
    BUILTINS.iter().map(|b| b.name).chain(STD_CONSTANTS.iter().map(|c| c.0)).filter_map(|n| n.strip_prefix(prefix.as_str())).collect()
}

pub fn builtin_index(name: &str) -> Option<u8> {
    BUILTINS.iter().position(|b| b.name == name).map(|i| i as u8)
}

#[derive(Clone, Debug, PartialEq)]
pub enum Constant {
    Int(i64),
    Float(f64),
    Str(String),
}

impl fmt::Display for Constant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Constant::Int(v) => write!(f, "{v}"),
            Constant::Float(v) => write!(f, "{v:?}"),
            Constant::Str(s) => write!(f, "{s:?}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Function {
    pub name: String,
    pub arity: u8,
    /// Total local slots including parameters.
    pub n_locals: u16,
    pub code: Vec<u8>,
    /// `(code offset, source line)` pairs, sorted by offset. Empty when debug
    /// info is stripped.
    pub lines: Vec<(u32, u32)>,
}

impl Function {
    /// Source line of the instruction at `ip`, if debug info is present.
    pub fn line_at(&self, ip: usize) -> Option<u32> {
        let idx = self.lines.partition_point(|&(off, _)| off as usize <= ip);
        idx.checked_sub(1).map(|i| self.lines[i].1)
    }
}

pub const NO_VIEW: u16 = u16::MAX;

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Program {
    pub constants: Vec<Constant>,
    pub functions: Vec<Function>,
    /// Struct and enum types; starts with [`TypeDef::builtins`] in programs
    /// that use types (packages from older compilers have none).
    pub types: Vec<TypeDef>,
    pub globals: Vec<String>,
    /// Index of the top-level `<main>` function.
    pub entry: u16,
    /// Index of the `view` function, if declared.
    pub view: Option<u16>,
}

// --------------------------------------------------------------------------
// Serialization
// --------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeError(pub String);

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "malformed bytecode: {}", self.0)
    }
}

impl core::error::Error for DecodeError {}

/// Bounds-checked little-endian reader.
pub(crate) struct ByteReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> ByteReader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        ByteReader { data, pos: 0 }
    }

    pub(crate) fn bytes(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.data.len()).ok_or_else(|| DecodeError("unexpected end of data".into()))?;
        let out = &self.data[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    pub(crate) fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.bytes(1)?[0])
    }

    pub(crate) fn u16(&mut self) -> Result<u16, DecodeError> {
        Ok(u16::from_le_bytes(self.bytes(2)?.try_into().unwrap()))
    }

    pub(crate) fn u32(&mut self) -> Result<u32, DecodeError> {
        Ok(u32::from_le_bytes(self.bytes(4)?.try_into().unwrap()))
    }

    pub(crate) fn u64(&mut self) -> Result<u64, DecodeError> {
        Ok(u64::from_le_bytes(self.bytes(8)?.try_into().unwrap()))
    }

    pub(crate) fn string(&mut self) -> Result<String, DecodeError> {
        let len = self.u32()? as usize;
        let bytes = self.bytes(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| DecodeError("invalid UTF-8 in string".into()))
    }

    /// Reject absurd element counts before allocating.
    pub(crate) fn count(&mut self, min_elem_size: usize) -> Result<usize, DecodeError> {
        let n = self.u32()? as usize;
        if n.saturating_mul(min_elem_size) > self.data.len() - self.pos {
            return Err(DecodeError(format!("element count {n} exceeds available data")));
        }
        Ok(n)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.pos == self.data.len()
    }
}

pub(crate) fn put_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

impl Program {
    /// Serialize the constant pool (`CPOL` section).
    pub fn encode_constants(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(self.constants.len() as u32).to_le_bytes());
        for c in &self.constants {
            match c {
                Constant::Int(v) => {
                    out.push(0);
                    out.extend_from_slice(&v.to_le_bytes());
                }
                Constant::Float(v) => {
                    out.push(1);
                    out.extend_from_slice(&v.to_bits().to_le_bytes());
                }
                Constant::Str(s) => {
                    out.push(2);
                    put_str(&mut out, s);
                }
            }
        }
        out
    }

    /// Serialize globals, functions and entry points (`CODE` section).
    pub fn encode_code(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&self.entry.to_le_bytes());
        out.extend_from_slice(&self.view.unwrap_or(NO_VIEW).to_le_bytes());
        out.extend_from_slice(&(self.globals.len() as u32).to_le_bytes());
        for g in &self.globals {
            put_str(&mut out, g);
        }
        out.extend_from_slice(&(self.functions.len() as u32).to_le_bytes());
        for f in &self.functions {
            put_str(&mut out, &f.name);
            out.push(f.arity);
            out.extend_from_slice(&f.n_locals.to_le_bytes());
            out.extend_from_slice(&(f.code.len() as u32).to_le_bytes());
            out.extend_from_slice(&f.code);
            out.extend_from_slice(&(f.lines.len() as u32).to_le_bytes());
            for &(off, line) in &f.lines {
                out.extend_from_slice(&off.to_le_bytes());
                out.extend_from_slice(&line.to_le_bytes());
            }
        }
        out
    }

    /// Serialize the type table (`TYPE` section).
    pub fn encode_types(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(self.types.len() as u32).to_le_bytes());
        for t in &self.types {
            put_str(&mut out, &t.name);
            match &t.shape {
                TypeShape::Struct(fields) => {
                    out.push(0);
                    out.extend_from_slice(&(fields.len() as u32).to_le_bytes());
                    for f in fields {
                        put_str(&mut out, f);
                    }
                }
                TypeShape::Enum(variants) => {
                    out.push(1);
                    out.extend_from_slice(&(variants.len() as u32).to_le_bytes());
                    for (name, arity) in variants {
                        put_str(&mut out, name);
                        out.push(*arity);
                    }
                }
            }
            out.extend_from_slice(&(t.methods.len() as u32).to_le_bytes());
            for (name, func) in &t.methods {
                put_str(&mut out, name);
                out.extend_from_slice(&func.to_le_bytes());
            }
        }
        out
    }

    fn decode_types(data: &[u8]) -> Result<Vec<TypeDef>, DecodeError> {
        let mut r = ByteReader::new(data);
        let n = r.count(13)?;
        let mut types = Vec::with_capacity(n);
        for _ in 0..n {
            let name = r.string()?;
            let shape = match r.u8()? {
                0 => {
                    let n = r.count(4)?;
                    TypeShape::Struct((0..n).map(|_| r.string()).collect::<Result<_, _>>()?)
                }
                1 => {
                    let n = r.count(5)?;
                    TypeShape::Enum((0..n).map(|_| Ok((r.string()?, r.u8()?))).collect::<Result<_, DecodeError>>()?)
                }
                k => return Err(DecodeError(format!("unknown type kind {k}"))),
            };
            let n = r.count(6)?;
            let methods = (0..n).map(|_| Ok((r.string()?, r.u16()?))).collect::<Result<_, DecodeError>>()?;
            types.push(TypeDef { name, shape, methods });
        }
        if !r.is_empty() {
            return Err(DecodeError("trailing bytes after type table".into()));
        }
        Ok(types)
    }

    /// Decode and verify a program from its `CPOL`, `CODE` and (optional,
    /// empty when absent) `TYPE` sections.
    pub fn decode(constants: &[u8], code: &[u8], types: &[u8]) -> Result<Program, DecodeError> {
        let mut r = ByteReader::new(constants);
        let n = r.count(5)?;
        let mut consts = Vec::with_capacity(n);
        for _ in 0..n {
            consts.push(match r.u8()? {
                0 => Constant::Int(r.u64()? as i64),
                1 => Constant::Float(f64::from_bits(r.u64()?)),
                2 => Constant::Str(r.string()?),
                t => return Err(DecodeError(format!("unknown constant tag {t}"))),
            });
        }
        if !r.is_empty() {
            return Err(DecodeError("trailing bytes after constant pool".into()));
        }

        let mut r = ByteReader::new(code);
        let entry = r.u16()?;
        let view = r.u16()?;
        let n = r.count(4)?;
        let globals = (0..n).map(|_| r.string()).collect::<Result<Vec<_>, _>>()?;
        let n = r.count(15)?;
        let mut functions = Vec::with_capacity(n);
        for _ in 0..n {
            let name = r.string()?;
            let arity = r.u8()?;
            let n_locals = r.u16()?;
            let len = r.u32()? as usize;
            let code = r.bytes(len)?.to_vec();
            let n_lines = r.count(8)?;
            let lines = (0..n_lines).map(|_| Ok((r.u32()?, r.u32()?))).collect::<Result<Vec<_>, DecodeError>>()?;
            functions.push(Function { name, arity, n_locals, code, lines });
        }
        if !r.is_empty() {
            return Err(DecodeError("trailing bytes after code section".into()));
        }
        let types = if types.is_empty() { Vec::new() } else { Self::decode_types(types)? };
        let program = Program { constants: consts, functions, types, globals, entry, view: (view != NO_VIEW).then_some(view) };
        program.verify()?;
        Ok(program)
    }

    /// Static verification: every instruction decodes, every operand refers
    /// to an existing constant/local/global/function, every jump lands on an
    /// instruction boundary and every function ends in `Return`.
    pub fn verify(&self) -> Result<(), DecodeError> {
        let err = |f: &Function, msg: String| DecodeError(format!("in function `{}`: {msg}", f.name));
        if self.functions.get(self.entry as usize).is_none() {
            return Err(DecodeError("entry function index out of range".into()));
        }
        if let Some(v) = self.view {
            if self.functions.get(v as usize).is_none_or(|f| f.arity != 0) {
                return Err(DecodeError("invalid view function".into()));
            }
        }
        for t in &self.types {
            if t.methods.iter().any(|&(_, f)| self.functions.get(f as usize).is_none_or(|f| f.arity == 0)) {
                return Err(DecodeError(format!("invalid method in type `{}`", t.name)));
            }
            if t.fields().len() > u8::MAX as usize || t.variants().len() >= ANY_VARIANT as usize {
                return Err(DecodeError(format!("type `{}` is too large", t.name)));
            }
        }
        let is_str = |i: usize| matches!(self.constants.get(i), Some(Constant::Str(_)));
        let struct_ty = |t: usize| matches!(self.types.get(t), Some(TypeDef { shape: TypeShape::Struct(_), .. }));
        let variant = |t: usize, v: usize| self.types.get(t).is_some_and(|t| v < t.variants().len());
        for f in &self.functions {
            if f.n_locals < f.arity as u16 {
                return Err(err(f, "fewer locals than parameters".into()));
            }
            let mut starts = vec![false; f.code.len() + 1];
            let mut jumps = Vec::new();
            let mut ip = 0;
            let mut last = None;
            while ip < f.code.len() {
                starts[ip] = true;
                let op = Op::from_u8(f.code[ip]).ok_or_else(|| err(f, format!("invalid opcode 0x{:02x} at {ip}", f.code[ip])))?;
                let operands = f.code.get(ip + 1..ip + 1 + op.operand_len()).ok_or_else(|| err(f, format!("truncated {op:?} at {ip}")))?;
                let u16_at = |i: usize| u16::from_le_bytes([operands[i], operands[i + 1]]) as usize;
                let u32_at = |i: usize| u32::from_le_bytes(operands[i..i + 4].try_into().unwrap()) as usize;
                let check = |ok: bool, what: &str| if ok { Ok(()) } else { Err(err(f, format!("{what} out of range at {ip}"))) };
                match op {
                    Op::Const => check(u16_at(0) < self.constants.len(), "constant")?,
                    Op::GetLocal | Op::SetLocal => check(u16_at(0) < f.n_locals as usize, "local slot")?,
                    Op::GetGlobal | Op::SetGlobal => check(u16_at(0) < self.globals.len(), "global")?,
                    Op::Func => check(u16_at(0) < self.functions.len(), "function")?,
                    Op::Builtin => check((operands[0] as usize) < BUILTINS.len(), "builtin")?,
                    Op::UiOpen => check(UiKind::from_u8(operands[0]).is_some_and(UiKind::is_legacy_container), "ui kind")?,
                    Op::UiBegin => check(UiKind::from_u8(operands[0]).is_some(), "ui kind")?,
                    Op::UiAttr => check(Attr::from_u8(operands[0]).is_some(), "ui attribute")?,
                    Op::GetField | Op::SetField | Op::Method => check(is_str(u16_at(0)), "name constant")?,
                    Op::NewStruct => check(struct_ty(u16_at(0)), "struct type")?,
                    Op::NewVariant => check(variant(u16_at(0), u16_at(2)), "enum variant")?,
                    Op::IsType => {
                        let (t, v) = (u16_at(0), u16_at(2));
                        check(if v == ANY_VARIANT as usize { t < self.types.len() } else { variant(t, v) }, "type")?
                    }
                    Op::Closure => check(u16_at(0) < self.functions.len(), "function")?,
                    Op::Jump | Op::JumpIfFalse | Op::JumpIfFalseKeep | Op::JumpIfTrueKeep => jumps.push((ip, u32_at(0))),
                    Op::IterNext => {
                        check(u16_at(0) < f.n_locals as usize, "local slot")?;
                        jumps.push((ip, u32_at(2)));
                    }
                    _ => {}
                }
                last = Some(op);
                ip += 1 + op.operand_len();
            }
            if last != Some(Op::Return) {
                return Err(err(f, "does not end with Return".into()));
            }
            for (at, target) in jumps {
                if target >= f.code.len() || !starts[target] {
                    return Err(err(f, format!("jump at {at} to invalid target {target}")));
                }
            }
        }
        Ok(())
    }

    /// Human-readable listing of the whole program.
    pub fn disassemble(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "; constants: {}", self.constants.len());
        for (i, c) in self.constants.iter().enumerate() {
            let _ = writeln!(out, ";   #{i:<4} {c}");
        }
        let _ = writeln!(out, "; globals: {}", self.globals.join(", "));
        for (fi, f) in self.functions.iter().enumerate() {
            let tag = if fi == self.entry as usize {
                " (entry)"
            } else if Some(fi as u16) == self.view {
                " (view)"
            } else {
                ""
            };
            let _ = writeln!(out, "\nfn #{fi} {}{tag}  arity={} locals={} size={}B", f.name, f.arity, f.n_locals, f.code.len());
            let mut ip = 0;
            let mut last_line = None;
            while ip < f.code.len() {
                let Some(op) = Op::from_u8(f.code[ip]) else {
                    let _ = writeln!(out, "  {ip:05}  <invalid 0x{:02x}>", f.code[ip]);
                    break;
                };
                let line = f.line_at(ip);
                let line_col = match line {
                    Some(l) if line != last_line => format!("{l:>4}"),
                    _ => "   |".into(),
                };
                last_line = line;
                let ops = &f.code[(ip + 1).min(f.code.len())..(ip + 1 + op.operand_len()).min(f.code.len())];
                let operand = self.describe_operands(op, ops);
                let _ = writeln!(out, "  {ip:05} {line_col}  {:<16}{operand}", op.mnemonic());
                ip += 1 + op.operand_len();
            }
        }
        out
    }

    fn describe_operands(&self, op: Op, ops: &[u8]) -> String {
        if ops.len() < op.operand_len() {
            return "<truncated>".into();
        }
        let u16_at = |i: usize| u16::from_le_bytes([ops[i], ops[i + 1]]);
        let u32_at = |i: usize| u32::from_le_bytes(ops[i..i + 4].try_into().unwrap());
        match op {
            Op::Const => {
                let i = u16_at(0);
                format!("#{i} ({})", self.constants.get(i as usize).map_or("?".into(), |c| c.to_string()))
            }
            Op::GetGlobal | Op::SetGlobal => {
                let i = u16_at(0);
                format!("{i} ({})", self.globals.get(i as usize).map_or("?", |s| s))
            }
            Op::Func => {
                let i = u16_at(0);
                format!("{i} ({})", self.functions.get(i as usize).map_or("?", |f| &f.name))
            }
            Op::Builtin => format!("{} ({})", ops[0], BUILTINS.get(ops[0] as usize).map_or("?", |b| b.name)),
            Op::GetLocal | Op::SetLocal | Op::List | Op::Concat => u16_at(0).to_string(),
            Op::Jump | Op::JumpIfFalse | Op::JumpIfFalseKeep | Op::JumpIfTrueKeep => format!("-> {:05}", u32_at(0)),
            Op::IterNext => format!("slot {} else -> {:05}", u16_at(0), u32_at(2)),
            Op::Call | Op::Bind => format!("argc={}", ops[0]),
            Op::GetField | Op::SetField => {
                let i = u16_at(0);
                format!(".{}", self.constants.get(i as usize).map_or("?".into(), |c| c.to_string().trim_matches('"').to_owned()))
            }
            Op::Method => {
                let i = u16_at(0);
                format!(
                    ".{}() argc={}",
                    self.constants.get(i as usize).map_or("?".into(), |c| c.to_string().trim_matches('"').to_owned()),
                    ops[2]
                )
            }
            Op::NewStruct => self.types.get(u16_at(0) as usize).map_or("?".into(), |t| t.name.clone()),
            Op::NewVariant | Op::IsType => {
                let (t, v) = (u16_at(0), u16_at(2));
                let ty = self.types.get(t as usize);
                let name = ty.map_or("?", |t| &t.name);
                match ty.and_then(|t| t.variants().get(v as usize)) {
                    Some((variant, _)) => format!("{name}.{variant}"),
                    None => name.to_string(),
                }
            }
            Op::Closure => {
                let i = u16_at(0);
                format!("{i} ({}) captures={}", self.functions.get(i as usize).map_or("?", |f| &f.name), ops[2])
            }
            Op::GetItem | Op::GetCapture => ops[0].to_string(),
            Op::UiOpen | Op::UiBegin => UiKind::from_u8(ops[0]).map_or("?", UiKind::name).to_string(),
            Op::UiAttr => Attr::from_u8(ops[0]).map_or("?", Attr::name).to_string(),
            _ => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opcode_roundtrip() {
        for b in 0..=255u8 {
            if let Some(op) = Op::from_u8(b) {
                assert_eq!(op as u8, b);
            }
        }
        assert_eq!(Op::IterNext.operand_len(), 6);
    }

    #[test]
    fn verifier_rejects_bad_code() {
        let mut p = Program {
            functions: vec![Function { name: "<main>".into(), code: vec![Op::Const as u8, 0, 0, Op::Return as u8], ..Default::default() }],
            ..Default::default()
        };
        assert!(p.verify().is_err(), "constant index out of range");
        p.constants.push(Constant::Int(1));
        assert!(p.verify().is_ok());
        p.functions[0].code = vec![Op::Jump as u8, 2, 0, 0, 0, Op::Return as u8];
        assert!(p.verify().is_err(), "jump into the middle of an instruction");
        p.functions[0].code = vec![Op::Nil as u8];
        assert!(p.verify().is_err(), "missing return");
    }
}

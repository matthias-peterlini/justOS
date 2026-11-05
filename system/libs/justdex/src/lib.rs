//! Android and Java programs: a reader for DEX files (the code inside an
//! `.apk`), an interpreter for the Dalvik bytecode in them and the part of
//! the Java and Android libraries those programs use most.
//!
//! The Java library is written here, in Rust ([`java`]): strings and
//! `StringBuilder`, the boxed numbers, `Math`, `System.out` and `System.in`,
//! `Scanner`, the collections (`ArrayList`, `HashMap`, `HashSet`, `TreeMap`
//! and friends), `Random`, exceptions, threads (run one after the other).
//! The Android part ([`android`]) is the core of a screen: an `Activity` with
//! `TextView`, `Button`, `EditText`, `ImageView`-less `LinearLayout`s built
//! in code, click listeners, `Toast`, `Log` and `Handler` timers. A program
//! that needs something else gets a `NoSuchMethodError` naming it.
//!
//! The interpreter talks to the world through a [`Host`].

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod android;
pub mod dex;
mod interp;
mod java;
mod text;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

pub use dex::Dex;

/// What the interpreter needs from the system.
pub trait Host {
    /// Writes to standard output (or, with `err`, to standard error).
    fn write(&mut self, s: &str, err: bool);
    /// A line typed by the user, without its newline (None: no more input).
    fn read_line(&mut self) -> Option<String>;
    /// Milliseconds since 1970.
    fn time_ms(&mut self) -> u64;
    /// Milliseconds since the computer started.
    fn uptime_ms(&mut self) -> u64;
    fn sleep(&mut self, ms: u64);
}

pub type Ref = Rc<RefCell<Obj>>;

/// A register: a primitive (narrow values in the low 32 bits, `long` and
/// `double` whole in the first register of their pair) or an object.
/// `null` is `P(0)`.
#[derive(Clone, Debug)]
pub enum Val {
    P(u64),
    R(Ref),
}

impl Val {
    pub const NULL: Val = Val::P(0);

    pub fn int(v: i32) -> Val {
        Val::P(v as u32 as u64)
    }
    pub fn long(v: i64) -> Val {
        Val::P(v as u64)
    }
    pub fn float(v: f32) -> Val {
        Val::P(v.to_bits() as u64)
    }
    pub fn double(v: f64) -> Val {
        Val::P(v.to_bits())
    }
    pub fn bool(v: bool) -> Val {
        Val::P(v as u64)
    }
    pub fn obj(r: Ref) -> Val {
        Val::R(r)
    }

    pub fn i(&self) -> i32 {
        match self {
            Val::P(v) => *v as u32 as i32,
            Val::R(_) => 1,
        }
    }
    pub fn j(&self) -> i64 {
        match self {
            Val::P(v) => *v as i64,
            Val::R(_) => 1,
        }
    }
    pub fn f(&self) -> f32 {
        f32::from_bits(self.i() as u32)
    }
    pub fn d(&self) -> f64 {
        f64::from_bits(self.j() as u64)
    }
    pub fn z(&self) -> bool {
        self.i() != 0
    }
    pub fn as_ref(&self) -> Option<&Ref> {
        match self {
            Val::R(r) => Some(r),
            Val::P(_) => None,
        }
    }
    pub fn is_null(&self) -> bool {
        matches!(self, Val::P(0))
    }
    /// The same register or the same object.
    pub fn same(&self, o: &Val) -> bool {
        match (self, o) {
            (Val::P(a), Val::P(b)) => *a as u32 == *b as u32,
            (Val::R(a), Val::R(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }
}

#[derive(Debug)]
pub enum Obj {
    Str(String),
    /// `ty` is the array's descriptor ("[I", "[Ljava/lang/String;").
    Arr { ty: Rc<str>, data: Vec<Val> },
    Inst(Inst),
}

#[derive(Debug)]
pub struct Inst {
    /// The class's descriptor ("Lcom/example/MainActivity;").
    pub class: Rc<str>,
    pub fields: BTreeMap<Rc<str>, Val>,
    /// The state of a library class (this object's or its ancestor's).
    pub native: Native,
}

/// The state of an object of a class written in Rust.
#[derive(Debug)]
pub enum Native {
    None,
    /// `System.out` / `System.err`.
    Out(bool),
    In,
    Sb(String),
    /// A boxed primitive: its descriptor letter and bits.
    Box(u8, u64),
    Coll(Coll),
    Map(Map),
    Entry(Val, Val),
    Iter { src: Ref, pos: usize },
    Random(u64),
    /// What is left of the line `Scanner` is reading and, for a scanner of
    /// a string, its next lines (last first).
    Scanner { line: Option<String>, text: Option<Vec<String>> },
    Reader,
    Thread(Val),
    Class(Rc<str>),
    View(android::View),
    Activity(android::Activity),
    Toast(String),
    Handler,
}

#[derive(Debug, Default)]
pub struct Coll {
    pub items: Vec<Val>,
    pub set: bool,
    pub sorted: bool,
}

#[derive(Debug, Default)]
pub struct Map {
    pub keys: Vec<Val>,
    pub vals: Vec<Val>,
    pub sorted: bool,
}

/// Why a method stopped early.
#[derive(Debug)]
pub enum Throw {
    /// A Java exception.
    Ex(Ref),
    /// `System.exit`.
    Exit(i32),
}

pub type JResult<T> = Result<T, Throw>;

pub struct Method {
    pub class: Rc<str>,
    pub name: Rc<str>,
    pub sig: Rc<str>,
    pub access: u32,
    pub dex: usize,
    pub code: Option<Rc<dex::Code>>,
}

pub struct Class {
    pub name: Rc<str>,
    pub superclass: Option<Rc<str>>,
    pub interfaces: Vec<Rc<str>>,
    pub access: u32,
    pub dex: usize,
    def: usize,
    /// By name; a name may have several signatures.
    pub methods: BTreeMap<Rc<str>, Vec<Rc<Method>>>,
    pub statics: RefCell<BTreeMap<Rc<str>, Val>>,
    ready: Cell<bool>,
}

impl Class {
    pub fn method(&self, name: &str, sig: &str) -> Option<&Rc<Method>> {
        self.methods.get(name)?.iter().find(|m| &*m.sig == sig)
    }
}

pub struct Vm {
    pub dexes: Vec<Dex>,
    index: BTreeMap<Rc<str>, (usize, usize)>,
    classes: BTreeMap<Rc<str>, Rc<Class>>,
    literals: Vec<Vec<Option<Ref>>>,
    pub host: Box<dyn Host>,
    depth: usize,
    out: Option<Ref>,
    err: Option<Ref>,
    inp: Option<Ref>,
    seed: u64,
    /// The Android side: the screen, toasts and timers.
    pub ui: android::Ui,
}

const MAX_DEPTH: usize = 400;

/// The DEX files in `data`: a `.dex` file, or the `classes*.dex` of an
/// `.apk` or `.jar` (a zip archive).
pub fn load(data: &[u8]) -> Result<Vec<Dex>, String> {
    if data.starts_with(b"dex\n") {
        return Ok(alloc::vec![Dex::parse(data.to_vec()).map_err(|e| e.0.to_string())?]);
    }
    if !data.starts_with(b"PK") {
        return Err("not a DEX file, an APK or a JAR".into());
    }
    let mut files = justzip::unzip(data).ok_or("a damaged archive")?;
    files.retain(|f| f.name.starts_with("classes") && f.name.ends_with(".dex"));
    // classes.dex, classes2.dex, classes3.dex... in that order.
    files.sort_by_key(|f| (f.name.len(), f.name.clone()));
    if files.is_empty() {
        return Err("the archive has no classes.dex: no code to run".into());
    }
    files.into_iter().map(|f| Dex::parse(f.data).map_err(|e| format!("{}: {}", f.name, e.0))).collect()
}

/// "Ljava/lang/String;" → "java.lang.String"; "[I" → "int[]".
pub fn java_name(desc: &str) -> String {
    if let Some(rest) = desc.strip_prefix('[') {
        return format!("{}[]", java_name(rest));
    }
    match desc {
        "I" => "int".into(),
        "J" => "long".into(),
        "Z" => "boolean".into(),
        "B" => "byte".into(),
        "C" => "char".into(),
        "S" => "short".into(),
        "F" => "float".into(),
        "D" => "double".into(),
        "V" => "void".into(),
        _ => desc.trim_start_matches('L').trim_end_matches(';').replace('/', "."),
    }
}

/// The parameter descriptors of a signature "(I[JLjava/lang/String;)V".
pub fn params(sig: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let b = sig.as_bytes();
    let end = sig.find(')').unwrap_or(sig.len());
    let mut i = 1;
    while i < end {
        let start = i;
        while i < end && b[i] == b'[' {
            i += 1;
        }
        if i < end && b[i] == b'L' {
            while i < end && b[i] != b';' {
                i += 1;
            }
        }
        i += 1;
        out.push(&sig[start..i.min(end)]);
    }
    out
}

pub fn ret(sig: &str) -> &str {
    sig.rsplit(')').next().unwrap_or("V")
}

fn wide(ty: &str) -> bool {
    ty == "J" || ty == "D"
}

impl Vm {
    pub fn new(dexes: Vec<Dex>, host: Box<dyn Host>) -> Vm {
        let mut index = BTreeMap::new();
        for (d, dex) in dexes.iter().enumerate() {
            for (i, c) in dex.classes.iter().enumerate() {
                // The first definition of a class wins, as on Android.
                index.entry(dex.types[c.class as usize].clone()).or_insert((d, i));
            }
        }
        let literals = dexes.iter().map(|d| alloc::vec![None; d.strings.len()]).collect();
        Vm { dexes, index, classes: BTreeMap::new(), literals, host, depth: 0, out: None, err: None, inp: None, seed: 0, ui: android::Ui::default() }
    }

    /// The program's classes (descriptors).
    pub fn class_names(&self) -> Vec<Rc<str>> {
        self.index.keys().cloned().collect()
    }

    /// A class of the program (None for the library's and unknown ones).
    pub fn class(&mut self, name: &str) -> Option<Rc<Class>> {
        if let Some(c) = self.classes.get(name) {
            return Some(c.clone());
        }
        // AndroidX's activities and widgets, even when the program carries
        // their code, are the library's own.
        if android::alias(name).is_some() {
            return None;
        }
        let (d, i) = *self.index.get(name)?;
        let dex = &self.dexes[d];
        let def = &dex.classes[i];
        let ty = |t: u32| dex.types[t as usize].clone();
        let mut methods: BTreeMap<Rc<str>, Vec<Rc<Method>>> = BTreeMap::new();
        for m in def.direct.iter().chain(&def.virtual_) {
            let id = &dex.methods[m.idx as usize];
            let code = if m.code_off != 0 { dex.code(m.code_off).ok().map(Rc::new) } else { None };
            let method = Method {
                class: ty(id.class),
                name: dex.strings[id.name as usize].clone(),
                sig: dex.protos[id.proto as usize].sig.clone(),
                access: m.access,
                dex: d,
                code,
            };
            methods.entry(method.name.clone()).or_default().push(Rc::new(method));
        }
        let class = Rc::new(Class {
            name: ty(def.class),
            superclass: (def.superclass != dex::NO_INDEX).then(|| ty(def.superclass)),
            interfaces: def.interfaces.iter().map(|&t| ty(t)).collect(),
            access: def.access,
            dex: d,
            def: i,
            methods,
            statics: RefCell::new(BTreeMap::new()),
            ready: Cell::new(false),
        });
        self.classes.insert(class.name.clone(), class.clone());
        Some(class)
    }

    /// Sets a class's static fields and runs its `<clinit>` (once).
    pub fn init_class(&mut self, name: &str) -> JResult<()> {
        let Some(c) = self.class(name) else { return Ok(()) };
        if c.ready.get() {
            return Ok(());
        }
        c.ready.set(true);
        if let Some(s) = c.superclass.clone() {
            self.init_class(&s)?;
        }
        let (fields, values) = {
            let dex = &self.dexes[c.dex];
            let def = &dex.classes[c.def];
            let fields: Vec<(Rc<str>, Rc<str>)> = def
                .static_fields
                .iter()
                .map(|&f| {
                    let f = &dex.fields[f as usize];
                    (dex.strings[f.name as usize].clone(), dex.types[f.ty as usize].clone())
                })
                .collect();
            (fields, dex.static_values(def.static_values_off))
        };
        for (k, (name, ty)) in fields.into_iter().enumerate() {
            let v = match values.get(k) {
                Some(dex::Const::Int(v)) => Val::int(*v),
                Some(dex::Const::Long(v)) => Val::long(*v),
                Some(dex::Const::Float(v)) => Val::float(*v),
                Some(dex::Const::Double(v)) => Val::double(*v),
                Some(dex::Const::Bool(v)) => Val::bool(*v),
                Some(dex::Const::String(i)) => Val::R(self.literal(c.dex, *i)),
                _ => Val::NULL,
            };
            let _ = ty;
            c.statics.borrow_mut().insert(name, v);
        }
        if let Some(m) = c.method("<clinit>", "()V").cloned() {
            self.exec(&m, Vec::new())?;
        }
        Ok(())
    }

    /// A string constant of the program (the same object every time).
    pub fn literal(&mut self, dex: usize, idx: u32) -> Ref {
        if let Some(Some(r)) = self.literals[dex].get(idx as usize) {
            return r.clone();
        }
        let s = self.dexes[dex].strings.get(idx as usize).map(|s| s.to_string()).unwrap_or_default();
        let r = new_ref(Obj::Str(s));
        if let Some(slot) = self.literals[dex].get_mut(idx as usize) {
            *slot = Some(r.clone());
        }
        r
    }

    /// The first ancestor of `name` (or `name` itself) that is not a class
    /// of the program: the library class whose state the object carries.
    pub fn library_base(&mut self, name: &str) -> Rc<str> {
        let mut cur: Rc<str> = Rc::from(name);
        for _ in 0..64 {
            match self.class(&cur) {
                Some(c) => match &c.superclass {
                    Some(s) => cur = s.clone(),
                    None => return Rc::from("Ljava/lang/Object;"),
                },
                None => return android::alias(&cur).map(Rc::from).unwrap_or(cur),
            }
        }
        cur
    }

    /// Is `class` (a descriptor) `target` or one of its subtypes?
    pub fn is_subclass(&mut self, class: &str, target: &str) -> bool {
        if class == target || target == "Ljava/lang/Object;" {
            return true;
        }
        if let (Some(a), Some(b)) = (class.strip_prefix('['), target.strip_prefix('[')) {
            return if a.starts_with('L') || a.starts_with('[') { self.is_subclass(a, b) } else { a == b };
        }
        if class.starts_with('[') {
            return false;
        }
        if let Some(c) = self.class(class) {
            let mut up: Vec<Rc<str>> = c.interfaces.clone();
            up.extend(c.superclass.clone());
            return up.iter().any(|s| self.is_subclass(s, target));
        }
        let class = android::alias(class).unwrap_or(class);
        if class == target {
            return true;
        }
        java::supertypes(class).iter().any(|s| self.is_subclass(s, target))
    }

    /// The class of an object.
    pub fn class_of(o: &Ref) -> Rc<str> {
        match &*o.borrow() {
            Obj::Str(_) => Rc::from("Ljava/lang/String;"),
            Obj::Arr { ty, .. } => ty.clone(),
            Obj::Inst(i) => i.class.clone(),
        }
    }

    pub fn instance_of(&mut self, v: &Val, ty: &str) -> bool {
        match v {
            Val::R(o) => {
                let c = Self::class_of(o);
                self.is_subclass(&c, ty)
            }
            Val::P(_) => false,
        }
    }

    /// The method a virtual call on an object of `class` reaches, if the
    /// program has one.
    pub fn find_virtual(&mut self, class: &str, name: &str, sig: &str) -> Option<Rc<Method>> {
        let mut cur: Rc<str> = Rc::from(class);
        let mut seen = Vec::new();
        for _ in 0..64 {
            let c = self.class(&cur)?;
            if let Some(m) = c.method(name, sig) {
                if m.code.is_some() && m.access & dex::ACC_STATIC == 0 {
                    return Some(m.clone());
                }
            }
            seen.extend(c.interfaces.iter().cloned());
            match &c.superclass {
                Some(s) => cur = s.clone(),
                None => break,
            }
        }
        // Default methods of interfaces.
        while let Some(i) = seen.pop() {
            if let Some(c) = self.class(&i) {
                if let Some(m) = c.method(name, sig) {
                    if m.code.is_some() {
                        return Some(m.clone());
                    }
                }
                seen.extend(c.interfaces.iter().cloned());
            }
        }
        None
    }

    /// A method `class` declares or inherits (static or direct calls).
    pub fn find_method(&mut self, class: &str, name: &str, sig: &str) -> Option<Rc<Method>> {
        let mut cur: Rc<str> = Rc::from(class);
        for _ in 0..64 {
            let c = self.class(&cur)?;
            if let Some(m) = c.method(name, sig) {
                return Some(m.clone());
            }
            cur = c.superclass.clone()?;
        }
        None
    }

    /// A Java exception of a library class, with a message.
    pub fn exception(&mut self, class: &str, msg: &str) -> Ref {
        let mut fields = BTreeMap::new();
        if !msg.is_empty() {
            fields.insert(Rc::from("message"), self.string(msg));
        }
        new_ref(Obj::Inst(Inst { class: Rc::from(class), fields, native: Native::None }))
    }

    pub fn throw<T>(&mut self, class: &str, msg: &str) -> JResult<T> {
        Err(Throw::Ex(self.exception(class, msg)))
    }

    pub fn npe<T>(&mut self, what: &str) -> JResult<T> {
        self.throw("Ljava/lang/NullPointerException;", what)
    }

    /// A new Java string.
    pub fn string(&mut self, s: &str) -> Val {
        Val::R(new_ref(Obj::Str(s.into())))
    }

    /// "java.lang.ArithmeticException: / by zero".
    pub fn describe(&mut self, ex: &Ref) -> String {
        match self.to_jstring(&Val::R(ex.clone())) {
            Ok(s) => s,
            Err(_) => java_name(&Self::class_of(ex)),
        }
    }

    /// Prints an exception nobody caught, as Java does, and gives the
    /// program's exit status.
    pub fn uncaught(&mut self, t: Throw) -> i32 {
        match t {
            Throw::Exit(code) => code,
            Throw::Ex(ex) => {
                let s = self.describe(&ex);
                self.host.write(&format!("Exception in thread \"main\" {s}\n"), true);
                1
            }
        }
    }

    /// The class with `public static void main(String[])` (the first one).
    pub fn main_class(&mut self) -> Option<Rc<str>> {
        for name in self.class_names() {
            if let Some(c) = self.class(&name) {
                if c.method("main", "([Ljava/lang/String;)V").is_some_and(|m| m.access & dex::ACC_STATIC != 0) {
                    return Some(name);
                }
            }
        }
        None
    }

    /// Runs `main` of `class` with `args`; gives the exit status.
    pub fn run_main(&mut self, class: &str, args: &[String]) -> i32 {
        let arr: Vec<Val> = args.iter().map(|a| self.string(a)).collect();
        let arr = Val::R(new_ref(Obj::Arr { ty: Rc::from("[Ljava/lang/String;"), data: arr }));
        let r = self.init_class(class).and_then(|_| match self.find_method(class, "main", "([Ljava/lang/String;)V") {
            Some(m) => self.exec(&m, alloc::vec![arr]).map(|_| ()),
            None => self.throw("Ljava/lang/NoSuchMethodError;", "main"),
        });
        match r {
            Ok(()) => 0,
            Err(t) => self.uncaught(t),
        }
    }

    /// Calls `name` on an object: the program's method if its class has
    /// one, the library's otherwise. `args` are one value per parameter.
    pub fn call_virtual(&mut self, this: &Val, name: &str, sig: &str, args: Vec<Val>) -> JResult<Val> {
        let Val::R(o) = this else { return self.npe(&format!("calling {name}() on null")) };
        let class = Self::class_of(o);
        if let Some(m) = self.find_virtual(&class, name, sig) {
            let mut regs = alloc::vec![this.clone()];
            regs.extend(expand(sig, args));
            return self.exec(&m, regs);
        }
        let base = self.library_base(&class);
        self.native(&base, name, sig, Some(this.clone()), args)
    }

    /// Calls a static method by class, name and signature.
    pub fn call_static(&mut self, class: &str, name: &str, sig: &str, args: Vec<Val>) -> JResult<Val> {
        self.init_class(class)?;
        if let Some(m) = self.find_method(class, name, sig) {
            return self.exec(&m, expand(sig, args));
        }
        self.native(class, name, sig, None, args)
    }

    /// Creates an object of `class` (without running a constructor).
    pub fn new_object(&mut self, class: &str) -> JResult<Ref> {
        if class == "Ljava/lang/String;" {
            return Ok(new_ref(Obj::Str(String::new())));
        }
        self.init_class(class)?;
        let base = self.library_base(class);
        let native = self.new_native(&base);
        Ok(new_ref(Obj::Inst(Inst { class: Rc::from(class), fields: BTreeMap::new(), native })))
    }

    /// `new class(args)` with the constructor of signature `sig`.
    pub fn construct(&mut self, class: &str, sig: &str, args: Vec<Val>) -> JResult<Ref> {
        let o = self.new_object(class)?;
        let this = Val::R(o.clone());
        match self.find_method(class, "<init>", sig) {
            Some(m) => {
                let mut regs = alloc::vec![this];
                regs.extend(expand(sig, args));
                self.exec(&m, regs)?;
            }
            None => {
                let base = self.library_base(class);
                self.native(&base, "<init>", sig, Some(this), args)?;
            }
        }
        Ok(o)
    }
}

pub fn new_ref(o: Obj) -> Ref {
    Rc::new(RefCell::new(o))
}

/// One value per parameter → registers (wide values take two).
pub fn expand(sig: &str, args: Vec<Val>) -> Vec<Val> {
    let mut out = Vec::with_capacity(args.len() + 2);
    let ps = params(sig);
    for (k, a) in args.into_iter().enumerate() {
        let w = ps.get(k).is_some_and(|p| wide(p));
        out.push(a);
        if w {
            out.push(Val::NULL);
        }
    }
    out
}

/// Registers → one value per parameter.
pub fn compress(sig: &str, this: bool, regs: Vec<Val>) -> (Option<Val>, Vec<Val>) {
    let mut it = regs.into_iter();
    let this = if this { it.next() } else { None };
    let mut out = Vec::new();
    for p in params(sig) {
        out.push(it.next().unwrap_or(Val::NULL));
        if wide(p) {
            it.next();
        }
    }
    (this, out)
}

#[cfg(test)]
mod tests;

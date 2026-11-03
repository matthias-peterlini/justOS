//! The interpreter: it walks the syntax tree.
//!
//! Async functions run to the end when called: an `await` runs the queued
//! jobs (and, if it must, the timers) until its promise settles. Every run
//! has a step budget, so a script that never ends is stopped.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::rc::{Rc, Weak};
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::any::Any;
use core::cell::RefCell;

use crate::ast::*;
use crate::value::*;

/// How a statement or expression ends other than normally.
pub enum Ctl {
    Throw(Value),
    Return(Value),
    Break(Option<Name>),
    Continue(Option<Name>),
    /// `a?.b` met a null or undefined `a`.
    Short,
    /// Out of steps: nothing catches it.
    Halt,
}

pub type R<T> = Result<T, Ctl>;

pub struct Binding {
    pub name: Name,
    pub value: Value,
    pub konst: bool,
}

/// What a function call adds to its scope.
pub struct Frame {
    /// `None` in a derived constructor before `super()`.
    pub this: Option<Value>,
    pub callee: Option<ObjRef>,
    pub home: Option<ObjRef>,
    pub new_target: Value,
}

pub struct Scope {
    pub vars: Vec<Binding>,
    pub parent: Option<ScopeRef>,
    pub frame: Option<Box<Frame>>,
    /// The outermost scope: names not found end up on the global object.
    pub global: bool,
}

pub type ScopeRef = Rc<RefCell<Scope>>;

pub type HostGet = fn(&mut Interp, &ObjRef, u32, usize, &Key) -> R<Option<Value>>;
pub type HostSet = fn(&mut Interp, &ObjRef, u32, usize, &Key, &Value) -> R<bool>;

pub enum Job {
    Call(Value, Value, Vec<Value>),
    /// A reaction, whether the promise was fulfilled, and its value.
    React(Reaction, bool, Value),
}

pub struct Timer {
    pub id: u32,
    pub due: f64,
    pub f: Value,
    pub args: Vec<Value>,
    pub every: Option<f64>,
}

pub struct Protos {
    pub object: ObjRef,
    pub function: ObjRef,
    pub array: ObjRef,
    pub string: ObjRef,
    pub number: ObjRef,
    pub boolean: ObjRef,
    pub symbol: ObjRef,
    pub error: ObjRef,
    pub type_error: ObjRef,
    pub range_error: ObjRef,
    pub syntax_error: ObjRef,
    pub reference_error: ObjRef,
    pub date: ObjRef,
    pub regexp: ObjRef,
    pub map: ObjRef,
    pub set: ObjRef,
    pub promise: ObjRef,
    pub iter: ObjRef,
}

pub const SYM_ITERATOR: u32 = 0;
pub const SYM_ASYNC_ITERATOR: u32 = 1;
pub const SYM_HAS_INSTANCE: u32 = 2;
pub const SYM_TO_PRIMITIVE: u32 = 3;
pub const SYM_TO_STRING_TAG: u32 = 4;

/// Calls inside calls.
pub const MAX_DEPTH: u32 = 72;
/// Steps one run may take.
pub const BUDGET: u64 = 4_000_000;
/// Lines `console` keeps.
const CONSOLE_MAX: usize = 300;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Bind {
    Var,
    Let,
    Const,
    Assign,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Hint {
    Default,
    Num,
    Str,
}

/// Something that can be assigned to.
enum Ref {
    Name(Name),
    Prop(Value, Key),
}

pub struct Interp {
    pub global: ObjRef,
    pub scope: ScopeRef,
    pub p: Protos,
    pub steps: u64,
    pub budget: u64,
    depth: u32,
    pub jobs: VecDeque<Job>,
    pub timers: Vec<Timer>,
    next_timer: u32,
    /// Milliseconds on a steady clock, kept up to date by the embedder.
    pub now: f64,
    /// Wall-clock milliseconds since 1970 when `now` was 0.
    pub epoch: f64,
    pub symbols: Vec<Rc<str>>,
    pub registry: Vec<(Rc<str>, u32)>,
    pub host: Option<Box<dyn Any>>,
    pub host_get: Option<HostGet>,
    pub host_set: Option<HostSet>,
    /// Asked for a global name nothing defines (a page's element ids).
    pub global_miss: Option<fn(&mut Interp, &str) -> Option<Value>>,
    /// What `console` printed.
    pub console: Vec<String>,
    objects: Vec<Weak<RefCell<Object>>>,
    scopes: Vec<Weak<RefCell<Scope>>>,
    prune_at: usize,
    pub new_target: Value,
    pending_labels: Vec<Name>,
    pub seed: u64,
    pub strcache: crate::string::StrCache,
}

fn same_name(a: &Name, b: &Name) -> bool {
    Rc::ptr_eq(a, b) || a == b
}

pub fn arg(args: &[Value], i: usize) -> Value {
    args.get(i).cloned().unwrap_or_default()
}

/// A short name for an expression, for error messages.
fn label(e: &Expr) -> String {
    match e {
        Expr::Ident(n) => String::from(&**n),
        Expr::This => String::from("this"),
        Expr::Dot(o, n, _) => format!("{}.{n}", label(o)),
        Expr::Index(o, _, _) => format!("{}[…]", label(o)),
        Expr::Call(c, _, _) => format!("{}(…)", label(c)),
        Expr::SuperDot(n) => format!("super.{n}"),
        _ => String::from("l'espressione"),
    }
}

impl Default for Interp {
    fn default() -> Self {
        Self::new()
    }
}

impl Interp {
    pub fn new() -> Interp {
        let mk = |proto: Option<&ObjRef>, kind: Kind| ObjRef(Rc::new(RefCell::new(Object::new(proto.cloned(), kind))));
        let object = mk(None, Kind::Plain);
        let function = mk(Some(&object), Kind::Plain);
        let error = mk(Some(&object), Kind::Plain);
        let p = Protos {
            function: function.clone(),
            array: mk(Some(&object), Kind::Array(Vec::new())),
            string: mk(Some(&object), Kind::Prim(Value::str(""))),
            number: mk(Some(&object), Kind::Prim(Value::Num(0.0))),
            boolean: mk(Some(&object), Kind::Prim(Value::Bool(false))),
            symbol: mk(Some(&object), Kind::Plain),
            type_error: mk(Some(&error), Kind::Plain),
            range_error: mk(Some(&error), Kind::Plain),
            syntax_error: mk(Some(&error), Kind::Plain),
            reference_error: mk(Some(&error), Kind::Plain),
            error,
            date: mk(Some(&object), Kind::Plain),
            regexp: mk(Some(&object), Kind::Plain),
            map: mk(Some(&object), Kind::Plain),
            set: mk(Some(&object), Kind::Plain),
            promise: mk(Some(&object), Kind::Plain),
            iter: mk(Some(&object), Kind::Plain),
            object,
        };
        let global = mk(Some(&p.object), Kind::Plain);
        let scope = Rc::new(RefCell::new(Scope {
            vars: Vec::new(),
            parent: None,
            frame: Some(Box::new(Frame { this: Some(Value::Obj(global.clone())), callee: None, home: None, new_target: Value::Undef })),
            global: true,
        }));
        let mut it = Interp {
            global,
            scope,
            p,
            steps: 0,
            budget: BUDGET,
            depth: 0,
            jobs: VecDeque::new(),
            timers: Vec::new(),
            next_timer: 1,
            now: 0.0,
            epoch: 0.0,
            symbols: ["Symbol.iterator", "Symbol.asyncIterator", "Symbol.hasInstance", "Symbol.toPrimitive", "Symbol.toStringTag"].iter().map(|s| Rc::from(*s)).collect(),
            registry: Vec::new(),
            host: None,
            host_get: None,
            host_set: None,
            global_miss: None,
            console: Vec::new(),
            objects: Vec::new(),
            scopes: Vec::new(),
            prune_at: 4096,
            new_target: Value::Undef,
            pending_labels: Vec::new(),
            seed: 0x9e37_79b9_7f4a_7c15,
            strcache: Default::default(),
        };
        let protos = [&it.p.object, &it.p.function, &it.p.array, &it.p.string, &it.p.number, &it.p.boolean, &it.p.symbol, &it.p.error, &it.p.type_error, &it.p.range_error, &it.p.syntax_error, &it.p.reference_error, &it.p.date, &it.p.regexp, &it.p.map, &it.p.set, &it.p.promise, &it.p.iter, &it.global].map(|o| Rc::downgrade(&o.0));
        it.objects.extend(protos);
        crate::builtins::install(&mut it);
        it
    }

    // -- Objects ----------------------------------------------------------

    pub fn alloc(&mut self, proto: Option<ObjRef>, kind: Kind) -> ObjRef {
        let o = ObjRef(Rc::new(RefCell::new(Object::new(proto, kind))));
        self.objects.push(Rc::downgrade(&o.0));
        if self.objects.len() >= self.prune_at {
            self.objects.retain(|w| w.strong_count() > 0);
            self.scopes.retain(|w| w.strong_count() > 0);
            self.prune_at = (self.objects.len() * 2).max(4096);
        }
        o
    }

    pub fn new_obj(&mut self) -> ObjRef {
        let p = self.p.object.clone();
        self.alloc(Some(p), Kind::Plain)
    }

    pub fn new_array(&mut self, v: Vec<Value>) -> Value {
        let p = self.p.array.clone();
        Value::Obj(self.alloc(Some(p), Kind::Array(v)))
    }

    pub fn new_error(&mut self, proto: &ObjRef, msg: &str) -> Value {
        let o = self.alloc(Some(proto.clone()), Kind::Error);
        o.borrow_mut().props.insert(Key::str("message"), Prop::hidden(Value::str(msg)));
        Value::Obj(o)
    }

    pub fn type_error(&mut self, msg: &str) -> Ctl {
        let p = self.p.type_error.clone();
        Ctl::Throw(self.new_error(&p, msg))
    }

    pub fn range_error(&mut self, msg: &str) -> Ctl {
        let p = self.p.range_error.clone();
        Ctl::Throw(self.new_error(&p, msg))
    }

    pub fn syntax_error(&mut self, msg: &str) -> Ctl {
        let p = self.p.syntax_error.clone();
        Ctl::Throw(self.new_error(&p, msg))
    }

    pub fn reference_error(&mut self, msg: &str) -> Ctl {
        let p = self.p.reference_error.clone();
        Ctl::Throw(self.new_error(&p, msg))
    }

    /// A function written in Rust.
    pub fn native(&mut self, name: &str, arity: u32, f: NativeFn, data: Value, ctor: bool) -> ObjRef {
        let p = self.p.function.clone();
        self.alloc(Some(p), Kind::Native(Rc::new(Native { f, data, name: Rc::from(name), arity, ctor })))
    }

    /// Puts a builtin method on `o`.
    pub fn method(&mut self, o: &ObjRef, name: &str, arity: u32, f: NativeFn) {
        let nf = self.native(name, arity, f, Value::Undef, false);
        o.borrow_mut().props.insert(Key::str(name), Prop::hidden(Value::Obj(nf)));
    }

    /// Puts a getter (and maybe a setter) on `o`.
    pub fn accessor(&mut self, o: &ObjRef, name: &str, get: NativeFn, set: Option<NativeFn>, data: Value) {
        let g = Value::Obj(self.native(name, 0, get, data.clone(), false));
        let s = match set {
            Some(f) => Value::Obj(self.native(name, 1, f, data, false)),
            None => Value::Undef,
        };
        o.borrow_mut().props.insert(Key::str(name), Prop { slot: Slot::Acc(g, s), enumerable: false, writable: true });
    }

    pub fn set_hidden(&mut self, o: &ObjRef, name: &str, v: Value) {
        o.borrow_mut().props.insert(Key::str(name), Prop::hidden(v));
    }

    pub fn set_global(&mut self, name: &str, v: Value) {
        let g = self.global.clone();
        self.set_hidden(&g, name, v);
    }

    pub fn symbol(&mut self, desc: &str) -> Value {
        self.symbols.push(Rc::from(desc));
        Value::Sym(self.symbols.len() as u32 - 1)
    }

    fn new_scope(&mut self, parent: &ScopeRef) -> ScopeRef {
        let s = Rc::new(RefCell::new(Scope { vars: Vec::new(), parent: Some(parent.clone()), frame: None, global: false }));
        self.scopes.push(Rc::downgrade(&s));
        s
    }

    fn tick(&mut self) -> R<()> {
        self.steps += 1;
        if self.steps > self.budget {
            return Err(Ctl::Halt);
        }
        Ok(())
    }

    pub fn random(&mut self) -> f64 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 7;
        self.seed ^= self.seed << 17;
        (self.seed >> 11) as f64 / (1u64 << 53) as f64
    }

    // -- Running ----------------------------------------------------------

    /// Runs a script. Errors come back as text (and go to the console).
    pub fn run(&mut self, src: &str) -> Result<Value, String> {
        self.steps = 0;
        let prog = match crate::parser::parse(src) {
            Ok(p) => p,
            Err(e) => {
                let msg = format!("SyntaxError: {e}");
                self.log(msg.clone());
                return Err(msg);
            }
        };
        let r = self.run_program(&prog);
        let r = self.settle(r);
        self.drain();
        r
    }

    /// Calls a function from outside (an event, a timer).
    pub fn call_entry(&mut self, f: &Value, this: Value, args: &[Value]) -> Result<Value, String> {
        self.steps = 0;
        let r = self.call(f, this, args);
        let r = self.settle(r);
        self.drain();
        r
    }

    fn settle(&mut self, r: R<Value>) -> Result<Value, String> {
        match r {
            Ok(v) => Ok(v),
            Err(c) => {
                let msg = self.ctl_message(c);
                self.log(msg.clone());
                Err(msg)
            }
        }
    }

    fn ctl_message(&mut self, c: Ctl) -> String {
        match c {
            Ctl::Throw(v) => format!("Uncaught {}", self.describe_error(&v)),
            Ctl::Halt => String::from("script interrotto: ci mette troppo"),
            _ => String::from("istruzione fuori posto"),
        }
    }

    /// Runs the queued jobs; errors go to the console.
    pub fn drain(&mut self) {
        if let Err(c) = self.run_jobs() {
            let msg = self.ctl_message(c);
            self.log(msg);
        }
    }

    pub fn log(&mut self, line: String) {
        if self.console.len() >= CONSOLE_MAX {
            self.console.remove(0);
        }
        self.console.push(line);
    }

    /// `Name: message` for an error, the text otherwise.
    pub fn describe_error(&mut self, v: &Value) -> String {
        if let Value::Obj(o) = v {
            if matches!(o.borrow().kind, Kind::Error) {
                let name = self.get(v, &Key::str("name")).ok().map(|n| self.display(&n)).unwrap_or_default();
                let msg = self.get(v, &Key::str("message")).ok().map(|n| self.display(&n)).unwrap_or_default();
                return if msg.is_empty() { name } else { format!("{name}: {msg}") };
            }
        }
        self.display(v)
    }

    /// A value as text without running any script (for messages).
    pub fn display(&mut self, v: &Value) -> String {
        match v {
            Value::Str(s) => String::from(&**s),
            Value::Obj(o) => {
                let b = o.borrow();
                match &b.kind {
                    Kind::Array(a) => format!("Array({})", a.len()),
                    Kind::Func(c) => format!("function {}", c.def.name.as_deref().unwrap_or("")),
                    Kind::Native(n) => format!("function {}", n.name),
                    Kind::Error | Kind::Plain => {
                        drop(b);
                        match self.to_str(v) {
                            Ok(s) => String::from(&*s),
                            Err(_) => String::from("[object]"),
                        }
                    }
                    _ => String::from("[object]"),
                }
            }
            Value::Sym(s) => format!("Symbol({})", self.symbols.get(*s as usize).map(|d| &**d).unwrap_or("")),
            _ => {
                let s = self.to_str(v).ok();
                s.map(|s| String::from(&*s)).unwrap_or_default()
            }
        }
    }

    pub fn run_jobs(&mut self) -> R<()> {
        let mut n = 0u32;
        while let Some(job) = self.jobs.pop_front() {
            n += 1;
            if n > 100_000 {
                self.jobs.clear();
                return Err(Ctl::Halt);
            }
            match job {
                Job::Call(f, this, args) => {
                    if let Err(c) = self.call(&f, this, &args) {
                        if matches!(c, Ctl::Halt) {
                            return Err(c);
                        }
                        let msg = self.ctl_message(c);
                        self.log(msg);
                    }
                }
                Job::React(r, ok, v) => self.react(r, ok, v)?,
            }
        }
        Ok(())
    }

    /// The earliest moment a timer is due.
    pub fn next_timer(&self) -> Option<f64> {
        self.timers.iter().map(|t| t.due).reduce(f64::min)
    }

    pub fn add_timer(&mut self, f: Value, delay: f64, args: Vec<Value>, repeat: bool) -> u32 {
        let id = self.next_timer;
        self.next_timer += 1;
        let delay = if delay.is_nan() { 0.0 } else { delay.max(0.0) };
        self.timers.push(Timer { id, due: self.now + delay, f, args, every: repeat.then_some(delay.max(4.0)) });
        id
    }

    pub fn clear_timer(&mut self, id: u32) {
        self.timers.retain(|t| t.id != id);
    }

    /// Takes the timer to run next: the earliest due by `until`.
    fn take_timer(&mut self, until: f64) -> Option<Timer> {
        let (i, _) = self.timers.iter().enumerate().filter(|(_, t)| t.due <= until).min_by(|a, b| a.1.due.total_cmp(&b.1.due).then(a.1.id.cmp(&b.1.id)))?;
        let t = &mut self.timers[i];
        if let Some(every) = t.every {
            let next = Timer { id: t.id, due: t.due.max(self.now) + every, f: t.f.clone(), args: t.args.clone(), every: t.every };
            Some(core::mem::replace(t, next))
        } else {
            Some(self.timers.remove(i))
        }
    }

    /// Runs the timers due by `now`; true if any ran.
    pub fn run_timers(&mut self, now: f64) -> bool {
        self.now = now;
        let mut ran = false;
        for _ in 0..200 {
            let Some(t) = self.take_timer(now) else { break };
            ran = true;
            let _ = self.call_entry(&t.f, Value::Undef, &t.args);
        }
        ran
    }

    /// Runs the earliest timer even if it isn't due (an `await` waiting).
    fn run_next_timer(&mut self) -> R<bool> {
        let Some(t) = self.take_timer(f64::INFINITY) else { return Ok(false) };
        if t.due > self.now {
            self.now = t.due;
        }
        if let Err(c) = self.call(&t.f, Value::Undef, &t.args) {
            if matches!(c, Ctl::Halt) {
                return Err(c);
            }
            let msg = self.ctl_message(c);
            self.log(msg);
        }
        Ok(true)
    }

    /// Runs source in the global scope: `eval`, `new Function`, handlers.
    pub fn eval_src(&mut self, src: &str) -> R<Value> {
        let prog = crate::parser::parse(src).map_err(|e| self.syntax_error(&e))?;
        self.run_program(&prog)
    }

    fn run_program(&mut self, prog: &Program) -> R<Value> {
        let g = self.global.clone();
        for v in &prog.vars {
            let k = Key::Str(v.clone());
            let exists = self.scope.borrow().vars.iter().any(|b| same_name(&b.name, v)) || g.borrow().props.find(&k).is_some();
            if !exists {
                g.borrow_mut().props.insert(k, Prop::data(Value::Undef));
            }
        }
        let sc = self.scope.clone();
        for s in &prog.body {
            if let Stmt::Func(def) = s {
                let f = self.make_function(def, &sc, false);
                if let Some(n) = &def.name {
                    g.borrow_mut().props.insert(Key::Str(n.clone()), Prop::data(f));
                }
            }
        }
        let mut last = Value::Undef;
        for s in &prog.body {
            match s {
                Stmt::Func(_) => {}
                Stmt::Expr(e) => last = self.eval(e, &sc)?,
                s => self.exec(s, &sc)?,
            }
        }
        Ok(last)
    }

    // -- Scopes -----------------------------------------------------------

    pub fn declare(&mut self, sc: &ScopeRef, name: &Name, v: Value, konst: bool) {
        let mut s = sc.borrow_mut();
        if let Some(b) = s.vars.iter_mut().find(|b| same_name(&b.name, name)) {
            b.value = v;
            b.konst = konst;
        } else {
            s.vars.push(Binding { name: name.clone(), value: v, konst });
        }
    }

    pub fn lookup(&mut self, sc: &ScopeRef, name: &Name) -> R<Value> {
        let mut cur = sc.clone();
        loop {
            let next = {
                let s = cur.borrow();
                if let Some(b) = s.vars.iter().find(|b| same_name(&b.name, name)) {
                    return Ok(b.value.clone());
                }
                s.parent.clone()
            };
            match next {
                Some(p) => cur = p,
                None => break,
            }
        }
        let g = self.global.clone();
        let gv = Value::Obj(g.clone());
        match self.get_opt(&g, &Key::Str(name.clone()), &gv)? {
            Some(v) => Ok(v),
            None => match self.global_miss.and_then(|f| f(self, name)) {
                Some(v) => Ok(v),
                None => Err(self.reference_error(&format!("{name} is not defined"))),
            },
        }
    }

    fn assign_name(&mut self, sc: &ScopeRef, name: &Name, v: Value) -> R<()> {
        let mut cur = sc.clone();
        loop {
            let next = {
                let mut s = cur.borrow_mut();
                if let Some(b) = s.vars.iter_mut().find(|b| same_name(&b.name, name)) {
                    if b.konst {
                        drop(s);
                        return Err(self.type_error("Assignment to constant variable."));
                    }
                    b.value = v;
                    return Ok(());
                }
                s.parent.clone()
            };
            match next {
                Some(p) => cur = p,
                None => break,
            }
        }
        let g = Value::Obj(self.global.clone());
        self.put(&g, Key::Str(name.clone()), v)
    }

    /// The scope of the function (not arrow) code runs in.
    fn frame_scope(&self, sc: &ScopeRef) -> ScopeRef {
        let mut cur = sc.clone();
        loop {
            let next = {
                let s = cur.borrow();
                if s.frame.is_some() {
                    break;
                }
                s.parent.clone()
            };
            match next {
                Some(p) => cur = p,
                None => break,
            }
        }
        cur
    }

    fn this_val(&mut self, sc: &ScopeRef) -> R<Value> {
        let fs = self.frame_scope(sc);
        let t = fs.borrow().frame.as_ref().map(|f| f.this.clone());
        match t {
            Some(Some(v)) => Ok(v),
            Some(None) => Err(self.reference_error("Must call super constructor in derived class before accessing 'this'")),
            None => Ok(Value::Undef),
        }
    }

    // -- Properties -------------------------------------------------------

    fn proto_of(&self, v: &Value) -> Option<ObjRef> {
        match v {
            Value::Str(_) => Some(self.p.string.clone()),
            Value::Num(_) => Some(self.p.number.clone()),
            Value::Bool(_) => Some(self.p.boolean.clone()),
            Value::Sym(_) => Some(self.p.symbol.clone()),
            _ => None,
        }
    }

    /// Makes a function's `prototype` the first time it is asked for.
    fn ensure_prototype(&mut self, o: &ObjRef) {
        let need = {
            let b = o.borrow();
            matches!(&b.kind, Kind::Func(c) if !c.def.arrow && !c.def.method && !c.def.is_async) && b.props.find(&Key::str("prototype")).is_none()
        };
        if need {
            let proto = self.new_obj();
            proto.borrow_mut().props.insert(Key::str("constructor"), Prop::hidden(Value::Obj(o.clone())));
            o.borrow_mut().props.insert(Key::str("prototype"), Prop { slot: Slot::Data(Value::Obj(proto)), enumerable: false, writable: true });
        }
    }

    pub fn get(&mut self, base: &Value, key: &Key) -> R<Value> {
        match base {
            Value::Obj(o) => Ok(self.get_opt(o, key, base)?.unwrap_or_default()),
            Value::Str(s) => {
                if key.is("length") {
                    return Ok(Value::Num(crate::string::clen(self, s) as f64));
                }
                if let Some(i) = key.index() {
                    return Ok(crate::string::cchar(self, s, i).map(|c| Value::str(c.encode_utf8(&mut [0; 4]))).unwrap_or_default());
                }
                let p = self.p.string.clone();
                Ok(self.get_opt(&p, key, base)?.unwrap_or_default())
            }
            Value::Undef | Value::Null => {
                let k = self.key_text(key);
                Err(self.type_error(&format!("Cannot read properties of {} (reading '{k}')", if base.nullish() && matches!(base, Value::Null) { "null" } else { "undefined" })))
            }
            _ => {
                let p = self.proto_of(base).expect("primitive prototype");
                Ok(self.get_opt(&p, key, base)?.unwrap_or_default())
            }
        }
    }

    pub fn key_text(&self, k: &Key) -> String {
        match k {
            Key::Str(s) => String::from(&**s),
            Key::Sym(n) => format!("Symbol({})", self.symbols.get(*n as usize).map(|d| &**d).unwrap_or("")),
        }
    }

    pub fn get_opt(&mut self, o: &ObjRef, key: &Key, receiver: &Value) -> R<Option<Value>> {
        enum Found {
            Val(Value),
            Getter(Value),
            Next(Option<ObjRef>),
        }
        if key.is("prototype") {
            self.ensure_prototype(o);
        }
        let mut cur = o.clone();
        loop {
            let host = match cur.borrow().kind {
                Kind::Host(t, id) => Some((t, id)),
                _ => None,
            };
            if let (Some((t, id)), Some(hook)) = (host, self.host_get) {
                if let Some(v) = hook(self, &cur, t, id, key)? {
                    return Ok(Some(v));
                }
            }
            let f = {
                let b = cur.borrow();
                let special = match &b.kind {
                    Kind::Array(a) => match key.index() {
                        Some(i) if i < a.len() => Some(a[i].clone()),
                        _ if key.is("length") => Some(Value::Num(a.len() as f64)),
                        _ => None,
                    },
                    Kind::Prim(Value::Str(s)) => match key.index() {
                        Some(i) => crate::string::char_at(s, i).map(|c| Value::str(c.encode_utf8(&mut [0; 4]))),
                        None if key.is("length") => Some(Value::Num(crate::string::len(s) as f64)),
                        None => None,
                    },
                    _ => None,
                };
                if let Some(v) = special {
                    Found::Val(v)
                } else if let Some(p) = b.props.get(key) {
                    match &p.slot {
                        Slot::Data(v) => Found::Val(v.clone()),
                        Slot::Acc(g, _) => Found::Getter(g.clone()),
                    }
                } else {
                    let virt = match (&b.kind, key) {
                        (Kind::Func(c), k) if k.is("name") => Some(Value::str(c.def.name.as_deref().unwrap_or(""))),
                        (Kind::Func(c), k) if k.is("length") => Some(Value::Num(c.def.params.iter().take_while(|p| !matches!(p, Pat::Def(..))).count() as f64)),
                        (Kind::Native(n), k) if k.is("name") => Some(Value::Str(n.name.clone())),
                        (Kind::Native(n), k) if k.is("length") => Some(Value::Num(f64::from(n.arity))),
                        (Kind::Bound(_), k) if k.is("name") => Some(Value::str("bound")),
                        _ => None,
                    };
                    match virt {
                        Some(v) => Found::Val(v),
                        None => Found::Next(b.proto.clone()),
                    }
                }
            };
            match f {
                Found::Val(v) => return Ok(Some(v)),
                Found::Getter(g) => {
                    if !g.callable() {
                        return Ok(Some(Value::Undef));
                    }
                    return self.call(&g, receiver.clone(), &[]).map(Some);
                }
                Found::Next(Some(p)) => cur = p,
                Found::Next(None) => return Ok(None),
            }
        }
    }

    /// `base[k]` where `k` is any value.
    pub fn get_v(&mut self, base: &Value, k: &Value) -> R<Value> {
        if let (Value::Obj(o), Value::Num(n)) = (base, k) {
            if let Kind::Array(a) = &o.borrow().kind {
                let i = *n as usize;
                if i as f64 == *n && i < a.len() {
                    return Ok(a[i].clone());
                }
            }
        }
        if let (Value::Str(s), Value::Num(n)) = (base, k) {
            let i = *n as usize;
            if i as f64 == *n {
                return Ok(crate::string::cchar(self, s, i).map(|c| Value::str(c.encode_utf8(&mut [0; 4]))).unwrap_or_default());
            }
        }
        let key = self.to_key(k)?;
        self.get(base, &key)
    }

    pub fn has_property(&mut self, o: &ObjRef, key: &Key) -> R<bool> {
        let v = Value::Obj(o.clone());
        let mut cur = Some(o.clone());
        while let Some(c) = cur {
            let host = match c.borrow().kind {
                Kind::Host(t, id) => Some((t, id)),
                _ => None,
            };
            if let (Some((t, id)), Some(hook)) = (host, self.host_get) {
                if hook(self, &c, t, id, key)?.is_some() {
                    return Ok(true);
                }
            }
            let b = c.borrow();
            let own = match &b.kind {
                Kind::Array(a) => key.index().is_some_and(|i| i < a.len()) || key.is("length"),
                _ => false,
            };
            if own || b.props.find(key).is_some() || (b.callable() && (key.is("name") || key.is("length"))) {
                return Ok(true);
            }
            cur = b.proto.clone();
        }
        let _ = v;
        Ok(false)
    }

    pub fn put(&mut self, base: &Value, key: Key, v: Value) -> R<()> {
        match base {
            Value::Obj(o) => self.put_obj(o, key, v, base),
            Value::Undef | Value::Null => {
                let k = self.key_text(&key);
                Err(self.type_error(&format!("Cannot set properties of {} (setting '{k}')", if matches!(base, Value::Null) { "null" } else { "undefined" })))
            }
            _ => Ok(()),
        }
    }

    pub fn put_obj(&mut self, o: &ObjRef, key: Key, v: Value, receiver: &Value) -> R<()> {
        let host = match o.borrow().kind {
            Kind::Host(t, id) => Some((t, id)),
            _ => None,
        };
        if let (Some((t, id)), Some(hook)) = (host, self.host_set) {
            if hook(self, o, t, id, &key, &v)? {
                return Ok(());
            }
        }
        let is_array = matches!(o.borrow().kind, Kind::Array(_));
        if is_array && key.is("length") {
            let n = self.to_number(&v)?;
            if n < 0.0 || n != to_int(n) || n > 1e8 {
                return Err(self.range_error("Invalid array length"));
            }
            if let Kind::Array(a) = &mut o.borrow_mut().kind {
                a.resize(n as usize, Value::Undef);
            }
            return Ok(());
        }
        let setter = {
            let mut b = o.borrow_mut();
            if b.frozen {
                return Ok(());
            }
            if let Kind::Array(a) = &mut b.kind {
                if let Some(i) = key.index() {
                    if i < a.len() {
                        a[i] = v;
                        return Ok(());
                    }
                    if i < a.len() + 1_000_000 {
                        a.resize(i, Value::Undef);
                        a.push(v);
                        return Ok(());
                    }
                }
            }
            match b.props.get_mut(&key) {
                Some(p) => match &mut p.slot {
                    Slot::Data(d) => {
                        if p.writable {
                            *d = v;
                        }
                        return Ok(());
                    }
                    Slot::Acc(_, s) => Some(s.clone()),
                },
                None => {
                    drop(b);
                    self.inherited_setter(o, &key)?
                }
            }
        };
        match setter {
            Some(Value::Undef) => Ok(()),
            Some(s) => self.call(&s, receiver.clone(), &[v]).map(|_| ()),
            None => {
                o.borrow_mut().props.insert(key, Prop::data(v));
                Ok(())
            }
        }
    }

    /// A setter up the prototype chain: `Some(setter)`, `Some(Undef)` if the
    /// property can't be written, `None` to make an own property.
    fn inherited_setter(&mut self, o: &ObjRef, key: &Key) -> R<Option<Value>> {
        let mut cur = o.borrow().proto.clone();
        while let Some(c) = cur {
            let host = match c.borrow().kind {
                Kind::Host(t, id) => Some((t, id)),
                _ => None,
            };
            if host.is_some() {
                // Host prototypes answer through the hook on the instance.
                let _ = host;
            }
            let b = c.borrow();
            match b.props.get(key) {
                Some(Prop { slot: Slot::Acc(_, s), .. }) => return Ok(Some(s.clone())),
                Some(p) if !p.writable => return Ok(Some(Value::Undef)),
                Some(_) => return Ok(None),
                None => cur = b.proto.clone(),
            }
        }
        Ok(None)
    }

    pub fn put_v(&mut self, base: &Value, k: &Value, v: Value) -> R<()> {
        if let (Value::Obj(o), Value::Num(n)) = (base, k) {
            if let Kind::Array(a) = &mut o.borrow_mut().kind {
                let i = *n as usize;
                if i as f64 == *n && i < a.len() {
                    a[i] = v;
                    return Ok(());
                }
            }
        }
        let key = self.to_key(k)?;
        self.put(base, key, v)
    }

    pub fn delete(&mut self, base: &Value, key: &Key) -> bool {
        let Value::Obj(o) = base else { return true };
        let mut b = o.borrow_mut();
        if b.frozen {
            return false;
        }
        if let Kind::Array(a) = &mut b.kind {
            if let Some(i) = key.index() {
                if i < a.len() {
                    a[i] = Value::Undef;
                }
                return true;
            }
        }
        b.props.remove(key);
        true
    }

    /// Own enumerable string keys, array indices first.
    pub fn own_keys(&mut self, v: &Value, all: bool) -> Vec<Key> {
        let mut out = Vec::new();
        match v {
            Value::Str(s) => {
                for i in 0..crate::string::len(s) {
                    out.push(Key::Str(Rc::from(num_to_string(i as f64).as_str())));
                }
            }
            Value::Obj(o) => {
                let b = o.borrow();
                let n = match &b.kind {
                    Kind::Array(a) => a.len(),
                    Kind::Prim(Value::Str(s)) => crate::string::len(s),
                    _ => 0,
                };
                for i in 0..n {
                    out.push(Key::Str(Rc::from(num_to_string(i as f64).as_str())));
                }
                if all && matches!(b.kind, Kind::Array(_)) {
                    out.push(Key::str("length"));
                }
                let mut ints: Vec<(usize, Key)> = Vec::new();
                let mut rest = Vec::new();
                for (k, p) in b.props.iter() {
                    if !(p.enumerable || all) || matches!(k, Key::Sym(_)) {
                        continue;
                    }
                    match k.index() {
                        Some(i) => ints.push((i, k.clone())),
                        None => rest.push(k.clone()),
                    }
                }
                ints.sort_by_key(|x| x.0);
                out.extend(ints.into_iter().map(|x| x.1));
                out.extend(rest);
            }
            _ => {}
        }
        out
    }

    fn for_in_keys(&mut self, v: &Value) -> Vec<Rc<str>> {
        let mut out: Vec<Rc<str>> = Vec::new();
        let mut cur = v.clone();
        let mut first = true;
        loop {
            for k in self.own_keys(&cur, false) {
                if let Key::Str(s) = k {
                    if first || !out.contains(&s) {
                        out.push(s);
                    }
                }
            }
            first = false;
            let next = match &cur {
                Value::Obj(o) => o.borrow().proto.clone(),
                _ => None,
            };
            match next {
                Some(p) if !p.same(&self.p.object) => cur = Value::Obj(p),
                _ => break,
            }
        }
        out
    }

    // -- Conversions ------------------------------------------------------

    pub fn to_prim(&mut self, v: &Value, hint: Hint) -> R<Value> {
        let Value::Obj(o) = v else { return Ok(v.clone()) };
        let prim = match &o.borrow().kind {
            Kind::Prim(p) => Some(p.clone()),
            _ => None,
        };
        if let Some(p) = prim {
            return Ok(p);
        }
        let exotic = self.get(v, &Key::Sym(SYM_TO_PRIMITIVE))?;
        if exotic.callable() {
            let h = Value::str(match hint {
                Hint::Default => "default",
                Hint::Num => "number",
                Hint::Str => "string",
            });
            let r = self.call(&exotic, v.clone(), &[h])?;
            if !matches!(r, Value::Obj(_)) {
                return Ok(r);
            }
        }
        let is_date = matches!(o.borrow().kind, Kind::Date(_));
        let order = if hint == Hint::Str || (hint == Hint::Default && is_date) { ["toString", "valueOf"] } else { ["valueOf", "toString"] };
        for m in order {
            let f = self.get(v, &Key::str(m))?;
            if f.callable() {
                let r = self.call(&f, v.clone(), &[])?;
                if !matches!(r, Value::Obj(_)) {
                    return Ok(r);
                }
            }
        }
        Err(self.type_error("Cannot convert object to primitive value"))
    }

    pub fn to_number(&mut self, v: &Value) -> R<f64> {
        Ok(match v {
            Value::Undef => f64::NAN,
            Value::Null => 0.0,
            Value::Bool(b) => f64::from(u8::from(*b)),
            Value::Num(n) => *n,
            Value::Str(s) => str_to_num(s),
            Value::Sym(_) => return Err(self.type_error("Cannot convert a Symbol value to a number")),
            Value::Obj(_) => {
                let p = self.to_prim(v, Hint::Num)?;
                return self.to_number(&p);
            }
        })
    }

    pub fn to_str(&mut self, v: &Value) -> R<Rc<str>> {
        Ok(match v {
            Value::Undef => Rc::from("undefined"),
            Value::Null => Rc::from("null"),
            Value::Bool(b) => Rc::from(if *b { "true" } else { "false" }),
            Value::Num(n) => Rc::from(num_to_string(*n).as_str()),
            Value::Str(s) => s.clone(),
            Value::Sym(_) => return Err(self.type_error("Cannot convert a Symbol value to a string")),
            Value::Obj(_) => {
                let p = self.to_prim(v, Hint::Str)?;
                return self.to_str(&p);
            }
        })
    }

    pub fn to_key(&mut self, v: &Value) -> R<Key> {
        Ok(match v {
            Value::Sym(s) => Key::Sym(*s),
            Value::Str(s) => Key::Str(s.clone()),
            v => Key::Str(self.to_str(v)?),
        })
    }

    pub fn to_object(&mut self, v: &Value) -> R<ObjRef> {
        match v {
            Value::Obj(o) => Ok(o.clone()),
            Value::Undef | Value::Null => Err(self.type_error("Cannot convert undefined or null to object")),
            p => {
                let proto = self.proto_of(p);
                Ok(self.alloc(proto, Kind::Prim(p.clone())))
            }
        }
    }

    pub fn loose_eq(&mut self, a: &Value, b: &Value) -> R<bool> {
        Ok(match (a, b) {
            (Value::Undef | Value::Null, Value::Undef | Value::Null) => true,
            (Value::Undef | Value::Null, _) | (_, Value::Undef | Value::Null) => false,
            (Value::Num(x), Value::Str(s)) | (Value::Str(s), Value::Num(x)) => *x == str_to_num(s),
            (Value::Bool(x), _) => return self.loose_eq(&Value::Num(f64::from(u8::from(*x))), b),
            (_, Value::Bool(y)) => return self.loose_eq(a, &Value::Num(f64::from(u8::from(*y)))),
            (Value::Obj(_), Value::Obj(_)) => a.strict_eq(b),
            (Value::Obj(_), _) => {
                let p = self.to_prim(a, Hint::Default)?;
                return self.loose_eq(&p, b);
            }
            (_, Value::Obj(_)) => {
                let p = self.to_prim(b, Hint::Default)?;
                return self.loose_eq(a, &p);
            }
            _ => a.strict_eq(b),
        })
    }

    /// `a < b`; `None` when either is NaN.
    fn less(&mut self, a: &Value, b: &Value) -> R<Option<bool>> {
        let a = self.to_prim(a, Hint::Num)?;
        let b = self.to_prim(b, Hint::Num)?;
        if let (Value::Str(x), Value::Str(y)) = (&a, &b) {
            return Ok(Some(x < y));
        }
        let x = self.to_number(&a)?;
        let y = self.to_number(&b)?;
        if x.is_nan() || y.is_nan() {
            return Ok(None);
        }
        Ok(Some(x < y))
    }

    pub fn instance_of(&mut self, v: &Value, f: &Value) -> R<bool> {
        if !f.callable() {
            return Err(self.type_error("Right-hand side of 'instanceof' is not callable"));
        }
        let target = match &f.obj().expect("callable").borrow().kind {
            Kind::Bound(b) => Some(b.target.clone()),
            _ => None,
        };
        if let Some(t) = target {
            return self.instance_of(v, &t);
        }
        let Value::Obj(o) = v else { return Ok(false) };
        let proto = self.get(f, &Key::str("prototype"))?;
        let Value::Obj(proto) = proto else { return Ok(false) };
        let mut cur = o.borrow().proto.clone();
        while let Some(c) = cur {
            if c.same(&proto) {
                return Ok(true);
            }
            cur = c.borrow().proto.clone();
        }
        Ok(false)
    }

    pub fn binop(&mut self, op: Op, a: &Value, b: &Value) -> R<Value> {
        if let (Value::Num(x), Value::Num(y)) = (a, b) {
            let (x, y) = (*x, *y);
            match op {
                Op::Add => return Ok(Value::Num(x + y)),
                Op::Sub => return Ok(Value::Num(x - y)),
                Op::Mul => return Ok(Value::Num(x * y)),
                Op::Div => return Ok(Value::Num(x / y)),
                Op::Lt => return Ok(Value::Bool(x < y)),
                Op::Gt => return Ok(Value::Bool(x > y)),
                Op::Le => return Ok(Value::Bool(x <= y)),
                Op::Ge => return Ok(Value::Bool(x >= y)),
                Op::StrictEq | Op::Eq => return Ok(Value::Bool(x == y)),
                Op::StrictNe | Op::Ne => return Ok(Value::Bool(x != y)),
                _ => {}
            }
        }
        let num = |it: &mut Interp, v: &Value| it.to_number(v);
        Ok(match op {
            Op::Add => {
                let pa = self.to_prim(a, Hint::Default)?;
                let pb = self.to_prim(b, Hint::Default)?;
                if matches!(pa, Value::Str(_)) || matches!(pb, Value::Str(_)) {
                    let sa = self.to_str(&pa)?;
                    let sb = self.to_str(&pb)?;
                    let mut s = String::with_capacity(sa.len() + sb.len());
                    s.push_str(&sa);
                    s.push_str(&sb);
                    Value::Str(Rc::from(s.as_str()))
                } else {
                    Value::Num(num(self, &pa)? + num(self, &pb)?)
                }
            }
            Op::Sub => Value::Num(num(self, a)? - num(self, b)?),
            Op::Mul => Value::Num(num(self, a)? * num(self, b)?),
            Op::Div => Value::Num(num(self, a)? / num(self, b)?),
            Op::Mod => Value::Num(num(self, a)? % num(self, b)?),
            Op::Exp => {
                let (x, y) = (num(self, a)?, num(self, b)?);
                Value::Num(if y.is_nan() || (x.abs() == 1.0 && y.is_infinite()) { f64::NAN } else { arcc_core::float::powf(x, y) })
            }
            Op::Shl => Value::Num(f64::from(to_i32(num(self, a)?).wrapping_shl(to_u32(num(self, b)?) & 31))),
            Op::Shr => Value::Num(f64::from(to_i32(num(self, a)?) >> (to_u32(num(self, b)?) & 31))),
            Op::UShr => Value::Num(f64::from(to_u32(num(self, a)?) >> (to_u32(num(self, b)?) & 31))),
            Op::BitAnd => Value::Num(f64::from(to_i32(num(self, a)?) & to_i32(num(self, b)?))),
            Op::BitOr => Value::Num(f64::from(to_i32(num(self, a)?) | to_i32(num(self, b)?))),
            Op::BitXor => Value::Num(f64::from(to_i32(num(self, a)?) ^ to_i32(num(self, b)?))),
            Op::Eq => Value::Bool(self.loose_eq(a, b)?),
            Op::Ne => Value::Bool(!self.loose_eq(a, b)?),
            Op::StrictEq => Value::Bool(a.strict_eq(b)),
            Op::StrictNe => Value::Bool(!a.strict_eq(b)),
            Op::Lt => Value::Bool(self.less(a, b)? == Some(true)),
            Op::Gt => Value::Bool(self.less(b, a)? == Some(true)),
            Op::Le => Value::Bool(self.less(b, a)? == Some(false)),
            Op::Ge => Value::Bool(self.less(a, b)? == Some(false)),
            Op::In => {
                let Value::Obj(o) = b else { return Err(self.type_error("Cannot use 'in' operator to search for a key in a primitive")) };
                let k = self.to_key(a)?;
                Value::Bool(self.has_property(o, &k)?)
            }
            Op::InstanceOf => Value::Bool(self.instance_of(a, b)?),
            Op::And | Op::Or | Op::Nullish => unreachable!("short-circuit operators are evaluated in place"),
        })
    }

    // -- Calls ------------------------------------------------------------

    pub fn call(&mut self, f: &Value, this: Value, args: &[Value]) -> R<Value> {
        let Value::Obj(o) = f else {
            let d = self.display(f);
            return Err(self.type_error(&format!("{d} is not a function")));
        };
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            self.depth -= 1;
            return Err(self.range_error("Maximum call stack size exceeded"));
        }
        let r = self.call_obj(o, this, args);
        self.depth -= 1;
        r
    }

    fn call_obj(&mut self, o: &ObjRef, this: Value, args: &[Value]) -> R<Value> {
        enum C {
            Func(Rc<Closure>),
            Native(Rc<Native>),
            Bound(Rc<Bound>),
            None,
        }
        let c = match &o.borrow().kind {
            Kind::Func(c) => C::Func(c.clone()),
            Kind::Native(n) => C::Native(n.clone()),
            Kind::Bound(b) => C::Bound(b.clone()),
            _ => C::None,
        };
        match c {
            C::Func(c) => {
                if c.class.is_some() {
                    return Err(self.type_error("Class constructor cannot be invoked without 'new'"));
                }
                self.invoke(&c, o, Some(this), args, Value::Undef).map(|r| r.0)
            }
            C::Native(n) => {
                let saved = core::mem::take(&mut self.new_target);
                let r = (n.f)(self, &this, args, &n.data);
                self.new_target = saved;
                r
            }
            C::Bound(b) => {
                let mut a = b.args.clone();
                a.extend_from_slice(args);
                self.call(&b.target, b.this.clone(), &a)
            }
            C::None => Err(self.type_error("not a function")),
        }
    }

    /// Runs a closure; returns its result and the `this` it ended with.
    fn invoke(&mut self, c: &Rc<Closure>, fobj: &ObjRef, this: Option<Value>, args: &[Value], nt: Value) -> R<(Value, Option<Value>)> {
        self.tick()?;
        let def = c.def.clone();
        let sc = self.new_scope(&c.scope);
        if !def.arrow {
            sc.borrow_mut().frame = Some(Box::new(Frame { this, callee: Some(fobj.clone()), home: c.home.clone(), new_target: nt }));
        }
        let r = self.run_body(&def, &sc, args);
        let r = if def.is_async {
            let p = self.new_promise();
            match r {
                Ok(v) => self.resolve_promise(&p, v)?,
                Err(Ctl::Throw(e)) => self.reject_promise(&p, e),
                Err(e) => return Err(e),
            }
            Value::Obj(p)
        } else {
            r?
        };
        let this_after = sc.borrow().frame.as_ref().and_then(|f| f.this.clone());
        Ok((r, this_after))
    }

    fn run_body(&mut self, def: &FuncDef, sc: &ScopeRef, args: &[Value]) -> R<Value> {
        if def.uses_args {
            let a = self.new_array(args.to_vec());
            self.declare(sc, &Rc::from("arguments"), a, false);
        }
        for (i, p) in def.params.iter().enumerate() {
            let v = arg(args, i);
            match p {
                Pat::Id(n) => self.declare(sc, n, v, false),
                p => self.bind(p, v, sc, Bind::Let)?,
            }
        }
        if let Some(r) = &def.rest {
            let rest = args.get(def.params.len()..).unwrap_or(&[]).to_vec();
            let a = self.new_array(rest);
            self.bind(r, a, sc, Bind::Let)?;
        }
        if !def.vars.is_empty() {
            let mut s = sc.borrow_mut();
            for v in &def.vars {
                if !s.vars.iter().any(|b| same_name(&b.name, v)) {
                    s.vars.push(Binding { name: v.clone(), value: Value::Undef, konst: false });
                }
            }
        }
        match &def.body {
            Body::Expr(e) => self.eval(e, sc),
            Body::Block(stmts) => match self.exec_list(stmts, sc) {
                Ok(()) => Ok(Value::Undef),
                Err(Ctl::Return(v)) => Ok(v),
                Err(e) => Err(e),
            },
        }
    }

    pub fn construct(&mut self, f: &Value, args: &[Value], nt: Option<&Value>) -> R<Value> {
        let nt = nt.cloned().unwrap_or_else(|| f.clone());
        let Value::Obj(o) = f else {
            let d = self.display(f);
            return Err(self.type_error(&format!("{d} is not a constructor")));
        };
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            self.depth -= 1;
            return Err(self.range_error("Maximum call stack size exceeded"));
        }
        let r = self.construct_obj(o, f, args, nt);
        self.depth -= 1;
        r
    }

    fn construct_obj(&mut self, o: &ObjRef, f: &Value, args: &[Value], nt: Value) -> R<Value> {
        enum C {
            Func(Rc<Closure>),
            Native(Rc<Native>),
            Bound(Rc<Bound>),
            None,
        }
        let c = match &o.borrow().kind {
            Kind::Func(c) => C::Func(c.clone()),
            Kind::Native(n) => C::Native(n.clone()),
            Kind::Bound(b) => C::Bound(b.clone()),
            _ => C::None,
        };
        match c {
            C::Func(c) => {
                if c.def.arrow || (c.def.method && c.class.is_none()) || c.def.is_async {
                    let d = self.display(f);
                    return Err(self.type_error(&format!("{d} is not a constructor")));
                }
                let derived = c.class.as_ref().is_some_and(|k| k.derived);
                let this = if derived {
                    None
                } else {
                    let proto = self.proto_from(&nt)?;
                    let t = Value::Obj(self.alloc(Some(proto), Kind::Plain));
                    if let Some(k) = &c.class {
                        self.init_fields(&t, k)?;
                    }
                    Some(t)
                };
                let (r, this_after) = self.invoke(&c, o, this, args, nt)?;
                if let Value::Obj(_) = r {
                    return Ok(r);
                }
                match this_after {
                    Some(t) => Ok(t),
                    None => Err(self.reference_error("Must call super constructor in derived class before returning")),
                }
            }
            C::Native(n) => {
                if !n.ctor {
                    let d = self.display(f);
                    return Err(self.type_error(&format!("{d} is not a constructor")));
                }
                let proto = self.proto_from(&nt)?;
                let this = Value::Obj(self.alloc(Some(proto), Kind::Plain));
                let saved = core::mem::replace(&mut self.new_target, nt);
                let r = (n.f)(self, &this, args, &n.data);
                self.new_target = saved;
                let r = r?;
                Ok(if matches!(r, Value::Obj(_)) { r } else { this })
            }
            C::Bound(b) => {
                let mut a = b.args.clone();
                a.extend_from_slice(args);
                let nt = if nt.strict_eq(f) { b.target.clone() } else { nt };
                self.construct(&b.target, &a, Some(&nt))
            }
            C::None => {
                let d = self.display(f);
                Err(self.type_error(&format!("{d} is not a constructor")))
            }
        }
    }

    fn proto_from(&mut self, nt: &Value) -> R<ObjRef> {
        match self.get(nt, &Key::str("prototype"))? {
            Value::Obj(p) => Ok(p),
            _ => Ok(self.p.object.clone()),
        }
    }

    fn init_fields(&mut self, this: &Value, k: &Rc<ClassRt>) -> R<()> {
        if k.fields.is_empty() {
            return Ok(());
        }
        let sc = self.new_scope(&k.scope);
        sc.borrow_mut().frame = Some(Box::new(Frame { this: Some(this.clone()), callee: None, home: Some(k.proto.clone()), new_target: Value::Undef }));
        for (key, idx) in &k.fields {
            let MemberKind::Field(init) = &k.def.members[*idx].kind else { continue };
            let v = match init {
                Some(e) => self.eval(e, &sc)?,
                None => Value::Undef,
            };
            if let Value::Obj(o) = this {
                o.borrow_mut().props.insert(key.clone(), Prop::data(v));
            }
        }
        Ok(())
    }

    pub fn make_function(&mut self, def: &Rc<FuncDef>, sc: &ScopeRef, is_expr: bool) -> Value {
        let named = is_expr && def.name.is_some() && !def.arrow;
        let scope = if named { self.new_scope(sc) } else { sc.clone() };
        let closure = Rc::new(Closure { def: def.clone(), scope: scope.clone(), home: None, class: None });
        let p = self.p.function.clone();
        let f = Value::Obj(self.alloc(Some(p), Kind::Func(closure)));
        if named {
            if let Some(n) = &def.name {
                self.declare(&scope, n, f.clone(), false);
            }
        }
        f
    }

    fn make_method(&mut self, def: &Rc<FuncDef>, sc: &ScopeRef, home: &ObjRef) -> Value {
        let closure = Rc::new(Closure { def: def.clone(), scope: sc.clone(), home: Some(home.clone()), class: None });
        let p = self.p.function.clone();
        Value::Obj(self.alloc(Some(p), Kind::Func(closure)))
    }

    fn define_accessor(o: &ObjRef, key: Key, get: Option<Value>, set: Option<Value>, enumerable: bool) {
        let mut b = o.borrow_mut();
        let (g0, s0) = match b.props.get(&key) {
            Some(Prop { slot: Slot::Acc(g, s), .. }) => (g.clone(), s.clone()),
            _ => (Value::Undef, Value::Undef),
        };
        b.props.insert(key, Prop { slot: Slot::Acc(get.unwrap_or(g0), set.unwrap_or(s0)), enumerable, writable: true });
    }

    fn prop_key(&mut self, k: &PropName, sc: &ScopeRef) -> R<Key> {
        match k {
            PropName::Name(n) => Ok(Key::Str(n.clone())),
            PropName::Computed(e) => {
                let v = self.eval(e, sc)?;
                self.to_key(&v)
            }
        }
    }

    fn eval_class(&mut self, cd: &Rc<ClassDef>, sc: &ScopeRef) -> R<Value> {
        let csc = self.new_scope(sc);
        let parent = match &cd.extends {
            Some(e) => Some(self.eval(e, &csc)?),
            None => None,
        };
        let (proto_parent, ctor_parent) = match &parent {
            None => (Some(self.p.object.clone()), self.p.function.clone()),
            Some(Value::Null) => (None, self.p.function.clone()),
            Some(v @ Value::Obj(o)) if o.borrow().callable() => match self.get(v, &Key::str("prototype"))? {
                Value::Obj(p) => (Some(p), o.clone()),
                Value::Null => (None, o.clone()),
                _ => return Err(self.type_error("Class extends value does not have valid prototype property")),
            },
            Some(v) => {
                let d = self.display(v);
                return Err(self.type_error(&format!("Class extends value {d} is not a constructor or null")));
            }
        };
        let proto = self.alloc(proto_parent, Kind::Plain);
        let mut fields = Vec::new();
        let mut statics = Vec::new();
        let mut methods = Vec::new();
        for (i, m) in cd.members.iter().enumerate() {
            let key = self.prop_key(&m.key, &csc)?;
            match &m.kind {
                MemberKind::Field(_) if !m.is_static => fields.push((key, i)),
                MemberKind::Field(_) | MemberKind::Block(_) => statics.push((key, i)),
                _ => methods.push((key, i)),
            }
        }
        let rt = Rc::new(ClassRt { def: cd.clone(), derived: parent.is_some(), fields, scope: csc.clone(), proto: proto.clone() });
        let closure = Rc::new(Closure { def: cd.ctor.clone(), scope: csc.clone(), home: Some(proto.clone()), class: Some(rt) });
        let ctor = self.alloc(Some(ctor_parent), Kind::Func(closure));
        let cv = Value::Obj(ctor.clone());
        ctor.borrow_mut().props.insert(Key::str("prototype"), Prop { slot: Slot::Data(Value::Obj(proto.clone())), enumerable: false, writable: false });
        proto.borrow_mut().props.insert(Key::str("constructor"), Prop::hidden(cv.clone()));
        if let Some(n) = &cd.name {
            self.declare(&csc, n, cv.clone(), true);
        }
        for (key, i) in methods {
            let m = &cd.members[i];
            let target = if m.is_static { ctor.clone() } else { proto.clone() };
            match &m.kind {
                MemberKind::Method(def) => {
                    let f = self.make_method(def, &csc, &target);
                    target.borrow_mut().props.insert(key, Prop::hidden(f));
                }
                MemberKind::Get(def) => {
                    let f = self.make_method(def, &csc, &target);
                    Self::define_accessor(&target, key, Some(f), None, false);
                }
                MemberKind::Set(def) => {
                    let f = self.make_method(def, &csc, &target);
                    Self::define_accessor(&target, key, None, Some(f), false);
                }
                _ => {}
            }
        }
        if !statics.is_empty() {
            let ssc = self.new_scope(&csc);
            ssc.borrow_mut().frame = Some(Box::new(Frame { this: Some(cv.clone()), callee: None, home: Some(ctor.clone()), new_target: Value::Undef }));
            for (key, i) in statics {
                match &cd.members[i].kind {
                    MemberKind::Field(init) => {
                        let v = match init {
                            Some(e) => self.eval(e, &ssc)?,
                            None => Value::Undef,
                        };
                        ctor.borrow_mut().props.insert(key, Prop::data(v));
                    }
                    MemberKind::Block(def) => {
                        let f = self.make_method(def, &csc, &ctor);
                        self.call(&f, cv.clone(), &[])?;
                    }
                    _ => {}
                }
            }
        }
        Ok(cv)
    }

    // -- Patterns ---------------------------------------------------------

    pub fn bind(&mut self, p: &Pat, v: Value, sc: &ScopeRef, mode: Bind) -> R<()> {
        match p {
            Pat::Id(n) => match mode {
                Bind::Let => {
                    self.declare(sc, n, v, false);
                    Ok(())
                }
                Bind::Const => {
                    self.declare(sc, n, v, true);
                    Ok(())
                }
                Bind::Var | Bind::Assign => self.assign_name(sc, n, v),
            },
            Pat::Target(e) => {
                let r = self.reference(e, sc)?;
                self.ref_set(&r, v, sc)
            }
            Pat::Def(p, d) => {
                let v = if matches!(v, Value::Undef) { self.eval(d, sc)? } else { v };
                self.bind(p, v, sc, mode)
            }
            Pat::Arr(items, rest) => {
                let list = self.iterate(&v)?;
                for (i, item) in items.iter().enumerate() {
                    if let Some(p) = item {
                        self.bind(p, list.get(i).cloned().unwrap_or_default(), sc, mode)?;
                    }
                }
                if let Some(r) = rest {
                    let tail = list.get(items.len()..).unwrap_or(&[]).to_vec();
                    let a = self.new_array(tail);
                    self.bind(r, a, sc, mode)?;
                }
                Ok(())
            }
            Pat::Obj(props, rest) => {
                if v.nullish() {
                    let d = self.display(&v);
                    return Err(self.type_error(&format!("Cannot destructure '{d}' as it is {d}.")));
                }
                let mut used = Vec::new();
                for (k, p) in props {
                    let key = self.prop_key(k, sc)?;
                    let x = self.get(&v, &key)?;
                    used.push(key);
                    self.bind(p, x, sc, mode)?;
                }
                if let Some(r) = rest {
                    let o = self.new_obj();
                    for k in self.own_keys(&v, false) {
                        if !used.contains(&k) {
                            let x = self.get(&v, &k)?;
                            o.borrow_mut().props.insert(k, Prop::data(x));
                        }
                    }
                    self.bind(r, Value::Obj(o), sc, mode)?;
                }
                Ok(())
            }
        }
    }

    fn reference(&mut self, e: &Expr, sc: &ScopeRef) -> R<Ref> {
        Ok(match e {
            Expr::Ident(n) => Ref::Name(n.clone()),
            Expr::Dot(o, n, _) => Ref::Prop(self.eval(o, sc)?, Key::Str(n.clone())),
            Expr::Index(o, k, _) => {
                let base = self.eval(o, sc)?;
                let k = self.eval(k, sc)?;
                Ref::Prop(base, self.to_key(&k)?)
            }
            Expr::SuperDot(n) => Ref::Prop(self.this_val(sc)?, Key::Str(n.clone())),
            Expr::SuperIndex(k) => {
                let t = self.this_val(sc)?;
                let k = self.eval(k, sc)?;
                Ref::Prop(t, self.to_key(&k)?)
            }
            Expr::Chain(inner) => return self.reference(inner, sc),
            _ => return Err(self.syntax_error("Invalid left-hand side in assignment")),
        })
    }

    fn ref_get(&mut self, r: &Ref, sc: &ScopeRef) -> R<Value> {
        match r {
            Ref::Name(n) => self.lookup(sc, n),
            Ref::Prop(b, k) => self.get(b, k),
        }
    }

    fn ref_set(&mut self, r: &Ref, v: Value, sc: &ScopeRef) -> R<()> {
        match r {
            Ref::Name(n) => self.assign_name(sc, n, v),
            Ref::Prop(b, k) => self.put(b, k.clone(), v),
        }
    }

    fn pat_ref(&mut self, p: &Pat, sc: &ScopeRef) -> R<Ref> {
        match p {
            Pat::Id(n) => Ok(Ref::Name(n.clone())),
            Pat::Target(e) => self.reference(e, sc),
            _ => Err(self.syntax_error("Invalid left-hand side in assignment")),
        }
    }

    /// Everything a `for…of` or spread goes through.
    pub fn iterate(&mut self, v: &Value) -> R<Vec<Value>> {
        match v {
            Value::Str(s) => return Ok(s.chars().map(|c| Value::str(c.encode_utf8(&mut [0; 4]))).collect()),
            Value::Obj(o) => {
                enum Fast {
                    List(Vec<Value>),
                    Pairs(Vec<(Value, Value)>),
                    No,
                }
                let fast = {
                    let mut b = o.borrow_mut();
                    match &mut b.kind {
                        Kind::Array(a) => Fast::List(a.clone()),
                        Kind::Set(s) => Fast::List(s.clone()),
                        Kind::Map(m) => Fast::Pairs(m.clone()),
                        Kind::Iter(items, at) => {
                            let rest = items.get(*at..).unwrap_or(&[]).to_vec();
                            *at = items.len();
                            Fast::List(rest)
                        }
                        Kind::Prim(Value::Str(s)) => Fast::List(s.chars().map(|c| Value::str(c.encode_utf8(&mut [0; 4]))).collect()),
                        _ => Fast::No,
                    }
                };
                match fast {
                    Fast::List(l) => return Ok(l),
                    Fast::Pairs(p) => return Ok(p.into_iter().map(|(k, v)| self.new_array(vec![k, v])).collect()),
                    Fast::No => {}
                }
            }
            _ => {}
        }
        let f = if v.nullish() { Value::Undef } else { self.get(v, &Key::Sym(SYM_ITERATOR))? };
        if !f.callable() {
            let d = self.display(v);
            return Err(self.type_error(&format!("{d} is not iterable")));
        }
        let it = self.call(&f, v.clone(), &[])?;
        let next = self.get(&it, &Key::str("next"))?;
        let mut out = Vec::new();
        loop {
            let r = self.call(&next, it.clone(), &[])?;
            if self.get(&r, &Key::str("done"))?.truthy() {
                break;
            }
            out.push(self.get(&r, &Key::str("value"))?);
            if out.len() > 10_000_000 {
                return Err(self.range_error("iterator too long"));
            }
            self.tick()?;
        }
        Ok(out)
    }

    // -- Statements -------------------------------------------------------

    pub fn exec_list(&mut self, list: &[Stmt], sc: &ScopeRef) -> R<()> {
        for s in list {
            if let Stmt::Func(def) = s {
                let f = self.make_function(def, sc, false);
                if let Some(n) = &def.name {
                    self.declare(sc, n, f, false);
                }
            }
        }
        for s in list {
            if !matches!(s, Stmt::Func(_)) {
                self.exec(s, sc)?;
            }
        }
        Ok(())
    }

    /// What a loop does after its body: `Ok(true)` to leave it.
    fn loop_ctl(r: R<()>, labels: &[Name]) -> R<bool> {
        match r {
            Ok(()) | Err(Ctl::Continue(None)) => Ok(false),
            Err(Ctl::Break(None)) => Ok(true),
            Err(Ctl::Break(Some(l))) if labels.contains(&l) => Ok(true),
            Err(Ctl::Continue(Some(l))) if labels.contains(&l) => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn decl_mode(kind: Option<DeclKind>) -> Bind {
        match kind {
            Some(DeclKind::Var) => Bind::Var,
            Some(DeclKind::Let) => Bind::Let,
            Some(DeclKind::Const) => Bind::Const,
            None => Bind::Assign,
        }
    }

    /// A copy of a loop's scope for the next round, so closures keep theirs.
    fn copy_scope(&mut self, sc: &ScopeRef) -> ScopeRef {
        let (vars, parent) = {
            let s = sc.borrow();
            (s.vars.iter().map(|b| Binding { name: b.name.clone(), value: b.value.clone(), konst: b.konst }).collect(), s.parent.clone())
        };
        let n = Rc::new(RefCell::new(Scope { vars, parent, frame: None, global: false }));
        self.scopes.push(Rc::downgrade(&n));
        n
    }

    pub fn exec(&mut self, s: &Stmt, sc: &ScopeRef) -> R<()> {
        self.tick()?;
        match s {
            Stmt::Expr(e) => {
                self.eval(e, sc)?;
                Ok(())
            }
            Stmt::Decl(kind, list) => {
                let mode = Self::decl_mode(Some(*kind));
                for (p, init) in list {
                    let v = match init {
                        Some(e) => self.eval(e, sc)?,
                        None if *kind == DeclKind::Var => continue,
                        None => Value::Undef,
                    };
                    self.bind(p, v, sc, mode)?;
                }
                Ok(())
            }
            Stmt::Func(_) | Stmt::Empty => Ok(()),
            Stmt::Class(cd) => {
                let c = self.eval_class(cd, sc)?;
                if let Some(n) = &cd.name {
                    self.declare(sc, n, c, false);
                }
                Ok(())
            }
            Stmt::Return(e) => {
                let v = match e {
                    Some(e) => self.eval(e, sc)?,
                    None => Value::Undef,
                };
                Err(Ctl::Return(v))
            }
            Stmt::If(c, a, b) => {
                if self.eval(c, sc)?.truthy() {
                    self.exec(a, sc)
                } else if let Some(b) = b {
                    self.exec(b, sc)
                } else {
                    Ok(())
                }
            }
            Stmt::Block(list, lexical) => {
                if *lexical {
                    let b = self.new_scope(sc);
                    self.exec_list(list, &b)
                } else {
                    self.exec_list(list, sc)
                }
            }
            Stmt::For(init, test, update, body) => {
                let labels = core::mem::take(&mut self.pending_labels);
                let per_iter = matches!(init.as_deref(), Some(Stmt::Decl(DeclKind::Let | DeclKind::Const, _)));
                let mut isc = if per_iter { self.new_scope(sc) } else { sc.clone() };
                if let Some(i) = init {
                    self.exec(i, &isc)?;
                }
                loop {
                    if let Some(t) = test {
                        if !self.eval(t, &isc)?.truthy() {
                            break;
                        }
                    }
                    let r = self.exec(body, &isc);
                    if Self::loop_ctl(r, &labels)? {
                        break;
                    }
                    // Closures made in this pass keep its bindings; the
                    // next pass counts on in a copy.
                    if per_iter {
                        isc = self.copy_scope(&isc);
                    }
                    if let Some(u) = update {
                        self.eval(u, &isc)?;
                    }
                }
                Ok(())
            }
            Stmt::ForIn(kind, pat, obj, body) | Stmt::ForOf(kind, pat, obj, body) => {
                let labels = core::mem::take(&mut self.pending_labels);
                let src = self.eval(obj, sc)?;
                let items: Vec<Value> = if matches!(s, Stmt::ForIn(..)) {
                    if src.nullish() {
                        Vec::new()
                    } else {
                        self.for_in_keys(&src).into_iter().map(Value::Str).collect()
                    }
                } else {
                    self.iterate(&src)?
                };
                let lexical = matches!(kind, Some(DeclKind::Let | DeclKind::Const));
                let mode = Self::decl_mode(*kind);
                for v in items {
                    let isc = if lexical { self.new_scope(sc) } else { sc.clone() };
                    self.bind(pat, v, &isc, mode)?;
                    let r = self.exec(body, &isc);
                    if Self::loop_ctl(r, &labels)? {
                        break;
                    }
                }
                Ok(())
            }
            Stmt::While(c, body) => {
                let labels = core::mem::take(&mut self.pending_labels);
                while self.eval(c, sc)?.truthy() {
                    let r = self.exec(body, sc);
                    if Self::loop_ctl(r, &labels)? {
                        break;
                    }
                }
                Ok(())
            }
            Stmt::DoWhile(body, c) => {
                let labels = core::mem::take(&mut self.pending_labels);
                loop {
                    let r = self.exec(body, sc);
                    if Self::loop_ctl(r, &labels)? || !self.eval(c, sc)?.truthy() {
                        break;
                    }
                }
                Ok(())
            }
            Stmt::Break(l) => Err(Ctl::Break(l.clone())),
            Stmt::Continue(l) => Err(Ctl::Continue(l.clone())),
            Stmt::Throw(e) => {
                let v = self.eval(e, sc)?;
                Err(Ctl::Throw(v))
            }
            Stmt::Try(body, catch, finally) => {
                let b = self.new_scope(sc);
                let mut r = self.exec_list(body, &b);
                if let (Err(Ctl::Throw(_)), Some((param, cbody))) = (&r, catch) {
                    let Err(Ctl::Throw(e)) = core::mem::replace(&mut r, Ok(())) else { unreachable!() };
                    let c = self.new_scope(sc);
                    r = match param {
                        Some(p) => self.bind(p, e, &c, Bind::Let),
                        None => Ok(()),
                    };
                    if r.is_ok() {
                        r = self.exec_list(cbody, &c);
                    }
                }
                if let Some(f) = finally {
                    if matches!(r, Err(Ctl::Halt)) {
                        return r;
                    }
                    let fsc = self.new_scope(sc);
                    self.exec_list(f, &fsc)?;
                }
                r
            }
            Stmt::Switch(d, cases) => {
                let v = self.eval(d, sc)?;
                let ssc = self.new_scope(sc);
                let mut start = None;
                for (i, (t, _)) in cases.iter().enumerate() {
                    if let Some(t) = t {
                        if self.eval(t, &ssc)?.strict_eq(&v) {
                            start = Some(i);
                            break;
                        }
                    }
                }
                let start = start.or_else(|| cases.iter().position(|c| c.0.is_none()));
                if let Some(start) = start {
                    for (_, body) in &cases[start..] {
                        match self.exec_list(body, &ssc) {
                            Err(Ctl::Break(None)) => return Ok(()),
                            r => r?,
                        }
                    }
                }
                Ok(())
            }
            Stmt::Label(l, body) => {
                self.pending_labels.push(l.clone());
                if !body.is_loop() && !matches!(**body, Stmt::Label(..)) {
                    self.pending_labels.clear();
                }
                match self.exec(body, sc) {
                    Err(Ctl::Break(Some(x))) if x == *l => Ok(()),
                    r => r,
                }
            }
        }
    }

    // -- Expressions ------------------------------------------------------

    fn eval_args(&mut self, args: &[Expr], sc: &ScopeRef) -> R<Vec<Value>> {
        let mut out = Vec::with_capacity(args.len());
        for a in args {
            if let Expr::Spread(x) = a {
                let v = self.eval(x, sc)?;
                out.extend(self.iterate(&v)?);
            } else {
                out.push(self.eval(a, sc)?);
            }
        }
        Ok(out)
    }

    fn super_get(&mut self, key: &Key, sc: &ScopeRef) -> R<Value> {
        let fs = self.frame_scope(sc);
        let home = fs.borrow().frame.as_ref().and_then(|f| f.home.clone());
        let this = self.this_val(sc)?;
        let Some(home) = home else { return Err(self.syntax_error("'super' keyword unexpected here")) };
        let proto = home.borrow().proto.clone();
        match proto {
            Some(p) => Ok(self.get_opt(&p, key, &this)?.unwrap_or_default()),
            None => Ok(Value::Undef),
        }
    }

    fn not_callable(&mut self, callee: &Expr) -> Ctl {
        self.type_error(&format!("{} is not a function", label(callee)))
    }

    fn eval_call(&mut self, callee: &Expr, args: &[Expr], optional: bool, sc: &ScopeRef) -> R<Value> {
        let (f, this) = match callee {
            Expr::Dot(o, n, opt) => {
                let b = self.eval(o, sc)?;
                if *opt && b.nullish() {
                    return Err(Ctl::Short);
                }
                (self.get(&b, &Key::Str(n.clone()))?, b)
            }
            Expr::Index(o, k, opt) => {
                let b = self.eval(o, sc)?;
                if *opt && b.nullish() {
                    return Err(Ctl::Short);
                }
                let k = self.eval(k, sc)?;
                (self.get_v(&b, &k)?, b)
            }
            Expr::SuperDot(n) => {
                let f = self.super_get(&Key::Str(n.clone()), sc)?;
                (f, self.this_val(sc)?)
            }
            e => (self.eval(e, sc)?, Value::Undef),
        };
        if optional && f.nullish() {
            return Err(Ctl::Short);
        }
        let args = self.eval_args(args, sc)?;
        if !f.callable() {
            return Err(self.not_callable(callee));
        }
        self.call(&f, this, &args)
    }

    fn eval_object(&mut self, props: &[PropDef], sc: &ScopeRef) -> R<Value> {
        let o = self.new_obj();
        for p in props {
            match &p.kind {
                PropKind::Spread => {
                    let v = self.eval(&p.value, sc)?;
                    for k in self.own_keys(&v, false) {
                        let x = self.get(&v, &k)?;
                        o.borrow_mut().props.insert(k, Prop::data(x));
                    }
                }
                PropKind::Init => {
                    if let (PropName::Name(n), false) = (&p.key, matches!(p.value, Expr::Func(_))) {
                        if &**n == "__proto__" {
                            match self.eval(&p.value, sc)? {
                                Value::Obj(pr) => o.borrow_mut().proto = Some(pr),
                                Value::Null => o.borrow_mut().proto = None,
                                _ => {}
                            }
                            continue;
                        }
                    }
                    let key = self.prop_key(&p.key, sc)?;
                    let v = match &p.value {
                        Expr::Func(def) if def.method => self.make_method(def, sc, &o),
                        e => self.eval(e, sc)?,
                    };
                    o.borrow_mut().props.insert(key, Prop::data(v));
                }
                PropKind::Get | PropKind::Set => {
                    let key = self.prop_key(&p.key, sc)?;
                    let Expr::Func(def) = &p.value else { continue };
                    let f = self.make_method(def, sc, &o);
                    if matches!(p.kind, PropKind::Get) {
                        Self::define_accessor(&o, key, Some(f), None, true);
                    } else {
                        Self::define_accessor(&o, key, None, Some(f), true);
                    }
                }
                PropKind::Shorthand(_) => return Err(self.syntax_error("Invalid shorthand property initializer")),
            }
        }
        Ok(Value::Obj(o))
    }

    fn update(&mut self, inc: bool, prefix: bool, target: &Expr, sc: &ScopeRef) -> R<Value> {
        let r = self.reference(target, sc)?;
        let old = self.ref_get(&r, sc)?;
        let old = self.to_number(&old)?;
        let new = if inc { old + 1.0 } else { old - 1.0 };
        self.ref_set(&r, Value::Num(new), sc)?;
        Ok(Value::Num(if prefix { new } else { old }))
    }

    fn eval_assign(&mut self, op: Option<Op>, pat: &Pat, rhs: &Expr, sc: &ScopeRef) -> R<Value> {
        match op {
            None => {
                let v = self.eval(rhs, sc)?;
                self.bind(pat, v.clone(), sc, Bind::Assign)?;
                Ok(v)
            }
            Some(op @ (Op::And | Op::Or | Op::Nullish)) => {
                let r = self.pat_ref(pat, sc)?;
                let cur = self.ref_get(&r, sc)?;
                let go = match op {
                    Op::And => cur.truthy(),
                    Op::Or => !cur.truthy(),
                    _ => cur.nullish(),
                };
                if !go {
                    return Ok(cur);
                }
                let v = self.eval(rhs, sc)?;
                self.ref_set(&r, v.clone(), sc)?;
                Ok(v)
            }
            Some(op) => {
                let r = self.pat_ref(pat, sc)?;
                let cur = self.ref_get(&r, sc)?;
                let x = self.eval(rhs, sc)?;
                let v = self.binop(op, &cur, &x)?;
                self.ref_set(&r, v.clone(), sc)?;
                Ok(v)
            }
        }
    }

    pub fn eval(&mut self, e: &Expr, sc: &ScopeRef) -> R<Value> {
        match e {
            Expr::Num(n) => Ok(Value::Num(*n)),
            Expr::Str(s) => Ok(Value::Str(s.clone())),
            Expr::Ident(n) => self.lookup(sc, n),
            Expr::This => self.this_val(sc),
            Expr::Null => Ok(Value::Null),
            Expr::Bool(b) => Ok(Value::Bool(*b)),
            Expr::Tmpl(parts, exprs) => {
                let mut s = String::from(&*parts[0]);
                for (i, x) in exprs.iter().enumerate() {
                    let v = self.eval(x, sc)?;
                    s.push_str(&self.to_str(&v)?);
                    if let Some(p) = parts.get(i + 1) {
                        s.push_str(p);
                    }
                }
                Ok(Value::Str(Rc::from(s.as_str())))
            }
            Expr::Tagged(tag, parts, exprs) => {
                let (f, this) = match &**tag {
                    Expr::Dot(o, n, _) => {
                        let b = self.eval(o, sc)?;
                        (self.get(&b, &Key::Str(n.clone()))?, b)
                    }
                    t => (self.eval(t, sc)?, Value::Undef),
                };
                let strings = self.new_array(parts.iter().map(|p| Value::Str(p.clone())).collect());
                if let Value::Obj(o) = &strings {
                    let raw = self.new_array(parts.iter().map(|p| Value::Str(p.clone())).collect());
                    o.borrow_mut().props.insert(Key::str("raw"), Prop::hidden(raw));
                }
                let mut args = vec![strings];
                for x in exprs {
                    args.push(self.eval(x, sc)?);
                }
                if !f.callable() {
                    return Err(self.not_callable(tag));
                }
                self.call(&f, this, &args)
            }
            Expr::Regex(p, f) => crate::string::new_regexp(self, p, f),
            Expr::Array(items) => {
                let mut v = Vec::with_capacity(items.len());
                for i in items {
                    match i {
                        None => v.push(Value::Undef),
                        Some(Expr::Spread(x)) => {
                            let s = self.eval(x, sc)?;
                            v.extend(self.iterate(&s)?);
                        }
                        Some(x) => v.push(self.eval(x, sc)?),
                    }
                }
                Ok(self.new_array(v))
            }
            Expr::Object(props) => self.eval_object(props, sc),
            Expr::Func(def) => Ok(self.make_function(def, sc, true)),
            Expr::Class(cd) => self.eval_class(cd, sc),
            Expr::Unary(op, x) => {
                if *op == Unary::TypeOf {
                    if let Expr::Ident(n) = &**x {
                        return match self.lookup(sc, n) {
                            Ok(v) => Ok(Value::str(v.type_of())),
                            Err(Ctl::Throw(_)) => Ok(Value::str("undefined")),
                            Err(e) => Err(e),
                        };
                    }
                }
                if *op == Unary::Delete {
                    return match &**x {
                        Expr::Dot(o, n, _) => {
                            let b = self.eval(o, sc)?;
                            Ok(Value::Bool(self.delete(&b, &Key::Str(n.clone()))))
                        }
                        Expr::Index(o, k, _) => {
                            let b = self.eval(o, sc)?;
                            let k = self.eval(k, sc)?;
                            let k = self.to_key(&k)?;
                            Ok(Value::Bool(self.delete(&b, &k)))
                        }
                        e => {
                            self.eval(e, sc)?;
                            Ok(Value::Bool(true))
                        }
                    };
                }
                let v = self.eval(x, sc)?;
                Ok(match op {
                    Unary::Neg => Value::Num(-self.to_number(&v)?),
                    Unary::Plus => Value::Num(self.to_number(&v)?),
                    Unary::Not => Value::Bool(!v.truthy()),
                    Unary::BitNot => Value::Num(f64::from(!to_i32(self.to_number(&v)?))),
                    Unary::TypeOf => Value::str(v.type_of()),
                    Unary::Void | Unary::Delete => Value::Undef,
                })
            }
            Expr::Update(inc, prefix, target) => self.update(*inc, *prefix, target, sc),
            Expr::Bin(Op::And, a, b) => {
                let x = self.eval(a, sc)?;
                if x.truthy() {
                    self.eval(b, sc)
                } else {
                    Ok(x)
                }
            }
            Expr::Bin(Op::Or, a, b) => {
                let x = self.eval(a, sc)?;
                if x.truthy() {
                    Ok(x)
                } else {
                    self.eval(b, sc)
                }
            }
            Expr::Bin(Op::Nullish, a, b) => {
                let x = self.eval(a, sc)?;
                if x.nullish() {
                    self.eval(b, sc)
                } else {
                    Ok(x)
                }
            }
            Expr::Bin(op, a, b) => {
                let x = self.eval(a, sc)?;
                let y = self.eval(b, sc)?;
                self.binop(*op, &x, &y)
            }
            Expr::Assign(op, pat, rhs) => self.eval_assign(*op, pat, rhs, sc),
            Expr::Cond(c, a, b) => {
                if self.eval(c, sc)?.truthy() {
                    self.eval(a, sc)
                } else {
                    self.eval(b, sc)
                }
            }
            Expr::Call(callee, args, opt) => self.eval_call(callee, args, *opt, sc),
            Expr::New(callee, args) => {
                let f = self.eval(callee, sc)?;
                let args = self.eval_args(args, sc)?;
                if !matches!(f, Value::Obj(_)) {
                    return Err(self.type_error(&format!("{} is not a constructor", label(callee))));
                }
                self.construct(&f, &args, None)
            }
            Expr::Dot(o, n, opt) => {
                let b = self.eval(o, sc)?;
                if *opt && b.nullish() {
                    return Err(Ctl::Short);
                }
                if b.nullish() {
                    return Err(self.type_error(&format!("Cannot read properties of {} (reading '{n}')", if matches!(b, Value::Null) { "null" } else { "undefined" })));
                }
                self.get(&b, &Key::Str(n.clone()))
            }
            Expr::Index(o, k, opt) => {
                let b = self.eval(o, sc)?;
                if *opt && b.nullish() {
                    return Err(Ctl::Short);
                }
                let k = self.eval(k, sc)?;
                self.get_v(&b, &k)
            }
            Expr::Seq(list) => {
                let mut last = Value::Undef;
                for x in list {
                    last = self.eval(x, sc)?;
                }
                Ok(last)
            }
            Expr::Spread(x) => self.eval(x, sc),
            Expr::Chain(inner) => match self.eval(inner, sc) {
                Err(Ctl::Short) => Ok(Value::Undef),
                r => r,
            },
            Expr::Await(x) => {
                let v = self.eval(x, sc)?;
                self.await_value(v)
            }
            Expr::SuperCall(args) => {
                let fs = self.frame_scope(sc);
                let (callee, nt) = {
                    let s = fs.borrow();
                    let f = s.frame.as_ref().expect("function frame");
                    (f.callee.clone(), f.new_target.clone())
                };
                let Some(callee) = callee else { return Err(self.syntax_error("'super' keyword unexpected here")) };
                let parent = callee.borrow().proto.clone().map(Value::Obj).unwrap_or_default();
                let args = self.eval_args(args, sc)?;
                let this = self.construct(&parent, &args, Some(&nt))?;
                if let Some(f) = fs.borrow_mut().frame.as_mut() {
                    f.this = Some(this.clone());
                }
                let rt = match &callee.borrow().kind {
                    Kind::Func(c) => c.class.clone(),
                    _ => None,
                };
                if let Some(k) = rt {
                    self.init_fields(&this, &k)?;
                }
                Ok(Value::Undef)
            }
            Expr::SuperDot(n) => self.super_get(&Key::Str(n.clone()), sc),
            Expr::SuperIndex(k) => {
                let k = self.eval(k, sc)?;
                let k = self.to_key(&k)?;
                self.super_get(&k, sc)
            }
            Expr::NewTarget => {
                let fs = self.frame_scope(sc);
                let nt = fs.borrow().frame.as_ref().map(|f| f.new_target.clone()).unwrap_or_default();
                Ok(nt)
            }
        }
    }

    // -- Promises ---------------------------------------------------------

    pub fn new_promise(&mut self) -> ObjRef {
        let p = self.p.promise.clone();
        self.alloc(Some(p), Kind::Promise(Box::new(Promise { state: PromiseState::Pending, reactions: Vec::new(), handled: false })))
    }

    pub fn promise_state(p: &ObjRef) -> Option<PromiseState> {
        match &p.borrow().kind {
            Kind::Promise(pr) => Some(pr.state.clone()),
            _ => None,
        }
    }

    fn settle_promise(&mut self, p: &ObjRef, ok: bool, v: Value) {
        let reactions = {
            let mut b = p.borrow_mut();
            let Kind::Promise(pr) = &mut b.kind else { return };
            if !matches!(pr.state, PromiseState::Pending) {
                return;
            }
            pr.state = if ok { PromiseState::Fulfilled(v.clone()) } else { PromiseState::Rejected(v.clone()) };
            core::mem::take(&mut pr.reactions)
        };
        for r in reactions {
            self.jobs.push_back(Job::React(r, ok, v.clone()));
        }
    }

    pub fn reject_promise(&mut self, p: &ObjRef, e: Value) {
        self.settle_promise(p, false, e);
    }

    pub fn resolve_promise(&mut self, p: &ObjRef, v: Value) -> R<()> {
        if let Value::Obj(o) = &v {
            if o.same(p) {
                let e = self.type_error("Chaining cycle detected for promise");
                if let Ctl::Throw(e) = e {
                    self.reject_promise(p, e);
                }
                return Ok(());
            }
            if matches!(o.borrow().kind, Kind::Promise(_)) {
                self.promise_then(o, Value::Undef, Value::Undef, Some(p.clone()));
                return Ok(());
            }
            let then = match self.get(&v, &Key::str("then")) {
                Ok(t) => t,
                Err(Ctl::Throw(e)) => {
                    self.reject_promise(p, e);
                    return Ok(());
                }
                Err(e) => return Err(e),
            };
            if then.callable() {
                let (res, rej) = self.resolving_functions(p);
                self.jobs.push_back(Job::Call(then, v.clone(), vec![res, rej]));
                return Ok(());
            }
        }
        self.settle_promise(p, true, v);
        Ok(())
    }

    /// The `resolve` and `reject` handed to an executor; only the first
    /// call of either counts.
    pub fn resolving_functions(&mut self, p: &ObjRef) -> (Value, Value) {
        fn res(it: &mut Interp, _: &Value, args: &[Value], data: &Value) -> R<Value> {
            if let Some((p, fresh)) = take_once(data) {
                if fresh {
                    it.resolve_promise(&p, arg(args, 0))?;
                }
            }
            Ok(Value::Undef)
        }
        fn rej(it: &mut Interp, _: &Value, args: &[Value], data: &Value) -> R<Value> {
            if let Some((p, fresh)) = take_once(data) {
                if fresh {
                    it.reject_promise(&p, arg(args, 0));
                }
            }
            Ok(Value::Undef)
        }
        fn take_once(data: &Value) -> Option<(ObjRef, bool)> {
            let Value::Obj(o) = data else { return None };
            let mut b = o.borrow_mut();
            let Kind::Array(a) = &mut b.kind else { return None };
            let fresh = !a[1].truthy();
            a[1] = Value::Bool(true);
            a[0].obj().cloned().map(|p| (p, fresh))
        }
        let pair = self.new_array(vec![Value::Obj(p.clone()), Value::Bool(false)]);
        let r = self.native("resolve", 1, res, pair.clone(), false);
        let j = self.native("reject", 1, rej, pair, false);
        (Value::Obj(r), Value::Obj(j))
    }

    pub fn promise_then(&mut self, p: &ObjRef, on_ok: Value, on_err: Value, target: Option<ObjRef>) {
        let state = {
            let mut b = p.borrow_mut();
            let Kind::Promise(pr) = &mut b.kind else { return };
            pr.handled = true;
            match &pr.state {
                PromiseState::Pending => {
                    pr.reactions.push(Reaction { on_ok, on_err, target });
                    return;
                }
                s => s.clone(),
            }
        };
        let (ok, v) = match state {
            PromiseState::Fulfilled(v) => (true, v),
            PromiseState::Rejected(v) => (false, v),
            PromiseState::Pending => return,
        };
        self.jobs.push_back(Job::React(Reaction { on_ok, on_err, target }, ok, v));
    }

    fn react(&mut self, r: Reaction, ok: bool, v: Value) -> R<()> {
        let h = if ok { &r.on_ok } else { &r.on_err };
        if h.callable() {
            match self.call(h, Value::Undef, &[v]) {
                Ok(x) => {
                    if let Some(t) = &r.target {
                        self.resolve_promise(t, x)?;
                    }
                }
                Err(Ctl::Throw(e)) => match &r.target {
                    Some(t) => self.reject_promise(t, e),
                    None => {
                        let msg = format!("Uncaught (in promise) {}", self.describe_error(&e));
                        self.log(msg);
                    }
                },
                Err(e) => return Err(e),
            }
        } else if let Some(t) = &r.target {
            if ok {
                self.resolve_promise(t, v)?;
            } else {
                self.reject_promise(t, v);
            }
        }
        Ok(())
    }

    /// `Promise.resolve(v)`.
    pub fn to_promise(&mut self, v: Value) -> R<ObjRef> {
        if let Value::Obj(o) = &v {
            if matches!(o.borrow().kind, Kind::Promise(_)) {
                return Ok(o.clone());
            }
        }
        let p = self.new_promise();
        self.resolve_promise(&p, v)?;
        Ok(p)
    }

    fn await_value(&mut self, v: Value) -> R<Value> {
        let p = self.to_promise(v)?;
        if let Kind::Promise(pr) = &mut p.borrow_mut().kind {
            pr.handled = true;
        }
        for _ in 0..100_000 {
            match Self::promise_state(&p) {
                Some(PromiseState::Fulfilled(v)) => return Ok(v),
                Some(PromiseState::Rejected(e)) => return Err(Ctl::Throw(e)),
                _ => {}
            }
            if !self.jobs.is_empty() {
                if let Some(job) = self.jobs.pop_front() {
                    match job {
                        Job::Call(f, this, args) => {
                            if let Err(c) = self.call(&f, this, &args) {
                                if matches!(c, Ctl::Halt) {
                                    return Err(c);
                                }
                                let msg = self.ctl_message(c);
                                self.log(msg);
                            }
                        }
                        Job::React(r, ok, v) => self.react(r, ok, v)?,
                    }
                }
                continue;
            }
            if !self.run_next_timer()? {
                break;
            }
        }
        Ok(Value::Undef)
    }
}

impl Drop for Interp {
    /// Objects and scopes point at each other in cycles: empty them all so
    /// the memory comes back.
    fn drop(&mut self) {
        self.jobs.clear();
        self.timers.clear();
        self.host = None;
        let scopes = core::mem::take(&mut self.scopes);
        for w in scopes {
            if let Some(s) = w.upgrade() {
                if let Ok(mut s) = s.try_borrow_mut() {
                    s.vars.clear();
                    s.parent = None;
                    s.frame = None;
                }
            }
        }
        if let Ok(mut s) = self.scope.try_borrow_mut() {
            s.vars.clear();
            s.frame = None;
        }
        let objects = core::mem::take(&mut self.objects);
        for w in objects {
            if let Some(o) = w.upgrade() {
                if let Ok(mut o) = o.try_borrow_mut() {
                    o.props.clear();
                    o.proto = None;
                    o.kind = Kind::Plain;
                }
            }
        }
    }
}

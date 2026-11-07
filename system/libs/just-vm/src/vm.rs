//! The JustVM interpreter.
//!
//! * Operand stack + call frames live in two `Vec`s; JustScript recursion
//!   never consumes native stack, so deep recursion is bounded only by
//!   [`Limits::max_frames`].
//! * Each frame owns `n_locals` slots at `stack[base..base + n_locals]`;
//!   temporaries are pushed above them.
//! * Programs are verified before execution ([`Program::verify`]) and every
//!   remaining dynamic check (stack underflow, types, bounds) is a runtime
//!   error, never a panic or undefined behaviour.

use crate::builtins;
use crate::prelude::*;
use crate::ui::{UiBuilder, UiEvent, UiNode, UiValue};
use crate::value::{Bound, Closure, Iter, MapKey, StructObj, Value, VariantObj};
use alloc::rc::Rc;
use alloc::sync::Arc;
use justc_core::bytecode::{Constant, DecodeError, Op, Program, TypeDef, ANY_VARIANT, ERR, NONE, OK, OPTION_TYPE, RESULT_TYPE, SOME};
use justc_core::ui::{Arg, ArgType, Attr, AttrType, Event, RawValue, UiKind};
use core::cell::RefCell;
use core::fmt;
use core::sync::atomic::{AtomicBool, Ordering};

/// What a program reaches outside the VM: the console, the clock, files
/// and the operating system. Only `print` is required; without the rest the
/// standard library reports that the service is not available.
pub trait Host {
    fn print(&mut self, text: &str);

    /// `prompt` (if any) is shown without a newline before reading. Returns
    /// the line without its line ending, or `None` at the end of input.
    /// Hosts without a console have no input.
    fn read_line(&mut self, prompt: Option<&str>) -> Option<String> {
        let _ = prompt;
        None
    }

    /// Seconds since 1970-01-01 00:00 UTC.
    fn now(&mut self) -> f64 {
        0.0
    }

    /// Milliseconds since an arbitrary moment; never goes back.
    fn clock_ms(&mut self) -> u64 {
        0
    }

    fn sleep_ms(&mut self, ms: u64) {
        let _ = ms;
    }

    /// Bits that differ from one run to the next (seeds `random`).
    fn entropy(&mut self) -> u64 {
        self.now().to_bits() ^ self.clock_ms().rotate_left(32)
    }

    fn read_file(&mut self, path: &str) -> Result<Vec<u8>, String> {
        Err(format!("{path}: {NO_FILES}"))
    }

    /// Creates or replaces the file, or adds to its end.
    fn write_file(&mut self, path: &str, data: &[u8], append: bool) -> Result<(), String> {
        let _ = (data, append);
        Err(format!("{path}: {NO_FILES}"))
    }

    /// `Some(true)` for a folder, `Some(false)` for a file, `None` if
    /// there is nothing at `path`.
    fn file_kind(&mut self, path: &str) -> Option<bool> {
        let _ = path;
        None
    }

    /// The names in a folder, in any order.
    fn list_dir(&mut self, path: &str) -> Result<Vec<String>, String> {
        Err(format!("{path}: {NO_FILES}"))
    }

    /// Creates a folder and the missing ones above it.
    fn make_dir(&mut self, path: &str) -> Result<(), String> {
        Err(format!("{path}: {NO_FILES}"))
    }

    /// Removes a file or an empty folder.
    fn remove(&mut self, path: &str) -> Result<(), String> {
        Err(format!("{path}: {NO_FILES}"))
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), String> {
        let _ = to;
        Err(format!("{from}: {NO_FILES}"))
    }

    fn copy_file(&mut self, from: &str, to: &str) -> Result<(), String> {
        let data = self.read_file(from)?;
        self.write_file(to, &data, false)
    }

    fn env(&mut self, name: &str) -> Option<String> {
        let _ = name;
        None
    }

    /// The current folder.
    fn cwd(&mut self) -> String {
        String::new()
    }

    /// Runs a program to its end: its exit code and, where the system can
    /// capture them, its output and error output.
    fn run(&mut self, program: &str, args: &[String]) -> Result<RunOutput, String> {
        let _ = args;
        Err(format!("{program}: this system cannot start programs"))
    }

    /// An HTTP request; any response, 404 included, is `Ok`.
    fn http(&mut self, request: &HttpRequest<'_>) -> Result<HttpResponse, String> {
        let _ = request;
        Err("this runtime has no network support".into())
    }

    /// A desktop notification for app `app_id`; returns its id.
    fn notify(&mut self, app_title: &str, app_id: &str, summary: &str, body: &str) -> Result<u32, String> {
        let _ = (app_title, app_id, summary, body);
        Err("this system has no notifications".into())
    }

    /// `os.name`.
    fn os_name(&self) -> &'static str {
        "unknown"
    }
}

const NO_FILES: &str = "this runtime has no file system";

/// What [`Host::run`] returns.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunOutput {
    pub code: i64,
    pub stdout: String,
    pub stderr: String,
}

pub struct HttpRequest<'a> {
    pub method: &'a str,
    pub url: &'a str,
    pub body: Option<&'a str>,
    pub headers: &'a [(String, String)],
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: i64,
    pub body: String,
    /// Names in lower case.
    pub headers: Vec<(String, String)>,
}

impl<F: FnMut(&str)> Host for F {
    fn print(&mut self, text: &str) {
        self(text)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Maximum instructions per top-level call (`None` = unlimited).
    pub max_steps: Option<u64>,
    pub max_frames: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { max_steps: None, max_frames: 10_000 }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraceEntry {
    pub function: String,
    pub line: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeError {
    pub message: String,
    /// Innermost frame first.
    pub trace: Vec<TraceEntry>,
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "runtime error: {}", self.message)?;
        for t in &self.trace {
            match t.line {
                Some(line) => write!(f, "\n    at {} (line {line})", t.function)?,
                None => write!(f, "\n    at {}", t.function)?,
            }
        }
        Ok(())
    }
}

impl core::error::Error for RuntimeError {}

#[derive(Clone, Debug)]
struct Frame {
    func: u16,
    ip: usize,
    base: usize,
    /// The running closure, for its captured values.
    closure: Option<Rc<Closure>>,
}

pub struct Vm {
    program: Rc<Program>,
    constants: Vec<Value>,
    pub(crate) types: Vec<Rc<TypeDef>>,
    globals: Vec<Value>,
    stack: Vec<Value>,
    frames: Vec<Frame>,
    pub(crate) host: Box<dyn Host>,
    ui: Option<UiBuilder>,
    actions: Vec<Value>,
    limits: Limits,
    interrupt: Option<Arc<AtomicBool>>,
    steps: u64,
    /// `host.clock_ms()` when the VM was made.
    pub(crate) started: u64,
    /// State of the `random` module's generator.
    pub(crate) rng: u64,
    /// What `os.args()` returns.
    pub(crate) args: Vec<String>,
    /// Set by `os.exit`, which stops the program with an error.
    pub(crate) exit_code: Option<i32>,
    /// The app's display name and app id, used by `os.notify`.
    pub(crate) app_title: String,
    pub(crate) app_id: String,
}

impl Vm {
    /// Create a VM whose output is dropped (see [`Vm::with_host`]). The
    /// program is verified first.
    pub fn new(program: Program) -> Result<Vm, DecodeError> {
        Vm::with_host(program, Box::new(|_: &str| {}))
    }

    pub fn with_host(program: Program, mut host: Box<dyn Host>) -> Result<Vm, DecodeError> {
        program.verify()?;
        let constants = program
            .constants
            .iter()
            .map(|c| match c {
                justc_core::bytecode::Constant::Int(v) => Value::Int(*v),
                justc_core::bytecode::Constant::Float(v) => Value::Float(*v),
                justc_core::bytecode::Constant::Str(s) => Value::str(s),
            })
            .collect();
        // Packages from older compilers have no type table.
        let types = if program.types.is_empty() { TypeDef::builtins() } else { program.types.clone() };
        let (started, rng) = (host.clock_ms(), host.entropy());
        Ok(Vm {
            types: types.into_iter().map(Rc::new).collect(),
            globals: vec![Value::Nil; program.globals.len()],
            program: Rc::new(program),
            constants,
            stack: Vec::with_capacity(256),
            frames: Vec::with_capacity(64),
            host,
            ui: None,
            actions: Vec::new(),
            limits: Limits::default(),
            interrupt: None,
            steps: 0,
            started,
            rng,
            args: Vec::new(),
            exit_code: None,
            app_title: "JustScript".into(),
            app_id: justc_core::jpk::app_id("justscript"),
        })
    }

    /// The command-line arguments the program sees through `os.args()`.
    pub fn set_args(&mut self, args: Vec<String>) {
        self.args = args;
    }

    /// The app's display name and app id (`dev.justos.<name>`), which
    /// notifications carry so the desktop groups them per app.
    pub fn set_app(&mut self, title: &str, app_id: &str) {
        self.app_title = title.to_owned();
        self.app_id = app_id.to_owned();
    }

    /// The code passed to `os.exit`, once the program has called it. The
    /// call stops the program like a runtime error, which callers report
    /// only when this is `None`.
    pub fn exit_code(&self) -> Option<i32> {
        self.exit_code
    }

    pub fn program(&self) -> &Program {
        &self.program
    }

    pub fn set_limits(&mut self, limits: Limits) {
        self.limits = limits;
    }

    /// Cooperative cancellation: execution stops with an error once the flag is set.
    pub fn set_interrupt(&mut self, flag: Arc<AtomicBool>) {
        self.interrupt = Some(flag);
    }

    pub fn has_view(&self) -> bool {
        self.program.view.is_some()
    }

    /// Value of a global by name (useful for tests and debugging tools).
    pub fn global(&self, name: &str) -> Option<&Value> {
        let idx = self.program.globals.iter().position(|g| g == name)?;
        self.globals.get(idx)
    }

    /// Execute the top-level code of the program.
    pub fn run_main(&mut self) -> Result<(), RuntimeError> {
        let entry = self.program.entry;
        self.call(Value::Func(entry), Vec::new()).map(drop)
    }

    /// Evaluate the `view` and return the resulting UI tree.
    pub fn render_view(&mut self) -> Result<Vec<UiNode>, RuntimeError> {
        let Some(view) = self.program.view else { return Ok(Vec::new()) };
        self.actions.clear();
        self.ui = Some(UiBuilder::default());
        let result = self.call(Value::Func(view), Vec::new());
        let builder = self.ui.take().unwrap_or_default();
        result.map(|_| builder.roots)
    }

    /// Run the handler for an event on the tree produced by the last
    /// [`render_view`](Self::render_view). Change handlers receive the new
    /// value after their bound arguments.
    pub fn dispatch(&mut self, event: UiEvent) -> Result<(), RuntimeError> {
        let (action, args) = match event {
            UiEvent::Click(action) => (action, Vec::new()),
            UiEvent::Change(action, value) => (action, vec![value.into()]),
        };
        let handler = self
            .actions
            .get(action)
            .cloned()
            .ok_or_else(|| RuntimeError { message: format!("unknown UI action #{action} (view was re-rendered?)"), trace: Vec::new() })?;
        self.call(handler, args).map(drop)
    }

    /// Call any callable value with arguments.
    pub fn call(&mut self, callee: Value, args: Vec<Value>) -> Result<Value, RuntimeError> {
        if self.frames.is_empty() {
            self.steps = 0;
        }
        let mark = self.stack.len();
        let depth = self.frames.len();
        self.stack.push(callee);
        self.stack.extend(args);
        let argc = self.stack.len() - mark - 1;
        match self.call_at(mark, argc, depth) {
            Ok(v) => Ok(v),
            Err(message) => {
                let trace = self.trace(depth);
                self.frames.truncate(depth);
                self.stack.truncate(mark);
                Err(RuntimeError { message, trace })
            }
        }
    }

    fn call_at(&mut self, callee_pos: usize, argc: usize, depth: usize) -> Result<Value, String> {
        let (callee, argc) = self.unwrap_bound(callee_pos, argc)?;
        match callee {
            Value::Func(f) => {
                self.push_frame(f, callee_pos, argc, None)?;
                self.run(depth)
            }
            Value::Closure(c) => {
                self.push_frame(c.func, callee_pos, argc, Some(c))?;
                self.run(depth)
            }
            Value::Builtin(b) => {
                let args = self.stack.split_off(callee_pos + 1);
                self.stack.truncate(callee_pos);
                builtins::call(self, b, args)
            }
            other => Err(format!("value of type {} is not callable", other.type_name())),
        }
    }

    fn trace(&self, depth: usize) -> Vec<TraceEntry> {
        let top = self.frames.len().saturating_sub(1);
        self.frames[depth..]
            .iter()
            .enumerate()
            .rev()
            .map(|(i, fr)| {
                let f = &self.program.functions[fr.func as usize];
                // Caller frames store the return address: look at the call instruction.
                let ip = if depth + i == top { fr.ip } else { fr.ip.saturating_sub(1) };
                TraceEntry { function: f.name.clone(), line: f.line_at(ip) }
            })
            .collect()
    }

    /// Replace a bound callable at `callee_pos` by its target, splicing its
    /// pre-bound arguments in front of the call arguments.
    fn unwrap_bound(&mut self, callee_pos: usize, mut argc: usize) -> Result<(Value, usize), String> {
        loop {
            let callee = self.stack.get(callee_pos).cloned().ok_or("stack underflow")?;
            let Value::Bound(bound) = callee else { return Ok((callee, argc)) };
            self.stack[callee_pos] = bound.callee.clone();
            let at = callee_pos + 1;
            self.stack.splice(at..at, bound.args.iter().cloned());
            argc += bound.args.len();
        }
    }

    /// `Some(value)`.
    pub(crate) fn some(&self, value: Value) -> Value {
        self.variant(OPTION_TYPE, SOME, vec![value])
    }

    /// `None`.
    pub(crate) fn none(&self) -> Value {
        self.variant(OPTION_TYPE, NONE, Vec::new())
    }

    /// `Ok(value)`.
    pub(crate) fn ok(&self, value: Value) -> Value {
        self.variant(RESULT_TYPE, OK, vec![value])
    }

    /// `Err(message)`.
    pub(crate) fn err(&self, message: impl core::fmt::Display) -> Value {
        self.err_value(Value::str(&message.to_string()))
    }

    /// `Err(value)`.
    pub(crate) fn err_value(&self, value: Value) -> Value {
        self.variant(RESULT_TYPE, ERR, vec![value])
    }

    /// Whether the host asked execution to stop.
    pub(crate) fn interrupted(&self) -> bool {
        self.interrupt.as_ref().is_some_and(|f| f.load(Ordering::Relaxed))
    }

    fn variant(&self, ty: u16, tag: u16, values: Vec<Value>) -> Value {
        Value::Variant(Rc::new(VariantObj { ty: Rc::clone(&self.types[ty as usize]), type_index: ty, tag, values }))
    }

    fn push_frame(&mut self, func: u16, callee_pos: usize, argc: usize, closure: Option<Rc<Closure>>) -> Result<(), String> {
        let f = &self.program.functions[func as usize];
        if argc != f.arity as usize {
            return Err(format!("function `{}` expects {} argument(s), got {argc}", f.name, f.arity));
        }
        if self.frames.len() >= self.limits.max_frames {
            return Err(format!("stack overflow (more than {} nested calls)", self.limits.max_frames));
        }
        let extra = f.n_locals as usize - argc;
        self.stack.extend(core::iter::repeat_n(Value::Nil, extra));
        self.frames.push(Frame { func, ip: 0, base: callee_pos + 1, closure });
        Ok(())
    }

    fn check_budget(&mut self) -> Result<(), String> {
        if let Some(flag) = &self.interrupt {
            if flag.load(Ordering::Relaxed) {
                return Err("execution interrupted".into());
            }
        }
        if let Some(max) = self.limits.max_steps {
            if self.steps > max {
                return Err(format!("instruction budget exhausted ({max} steps) — infinite loop?"));
            }
        }
        Ok(())
    }

    /// The interpreter loop. Runs until the frame count drops to `stop_depth`.
    fn run(&mut self, stop_depth: usize) -> Result<Value, String> {
        let program = Rc::clone(&self.program);
        let Frame { func, mut ip, mut base, closure } = self.frames.last().cloned().ok_or("no active frame")?;
        let mut func = func;
        let mut closure = closure;
        let mut code: &[u8] = &program.functions[func as usize].code;

        loop {
            let op_start = ip;
            macro_rules! fail {
                ($e:expr) => {{
                    if let Some(fr) = self.frames.last_mut() {
                        fr.ip = op_start;
                    }
                    return Err(String::from($e));
                }};
            }
            macro_rules! tri {
                ($e:expr) => {
                    match $e {
                        Ok(v) => v,
                        Err(e) => fail!(e),
                    }
                };
            }
            macro_rules! pop {
                () => {
                    match self.stack.pop() {
                        Some(v) => v,
                        None => fail!("stack underflow"),
                    }
                };
            }
            macro_rules! u8_operand {
                () => {{
                    ip += 1;
                    code[ip - 1]
                }};
            }
            macro_rules! u16_operand {
                () => {{
                    ip += 2;
                    u16::from_le_bytes([code[ip - 2], code[ip - 1]]) as usize
                }};
            }
            macro_rules! u32_operand {
                () => {{
                    ip += 4;
                    u32::from_le_bytes([code[ip - 4], code[ip - 3], code[ip - 2], code[ip - 1]]) as usize
                }};
            }
            // Call the callee at `callee_pos` with the `argc` values above it.
            macro_rules! do_call {
                ($callee_pos:expr, $argc:expr) => {{
                    let callee_pos = $callee_pos;
                    let (callee, argc) = tri!(self.unwrap_bound(callee_pos, $argc));
                    let target = match callee {
                        Value::Func(f) => Some((f, None)),
                        Value::Closure(c) => Some((c.func, Some(c))),
                        Value::Builtin(b) => {
                            let args = self.stack.split_off(callee_pos + 1);
                            self.stack.truncate(callee_pos);
                            if let Some(fr) = self.frames.last_mut() {
                                fr.ip = op_start;
                            }
                            let v = tri!(builtins::call(self, b, args));
                            self.stack.push(v);
                            None
                        }
                        other => fail!(format!("value of type {} is not callable", other.type_name())),
                    };
                    if let Some((f, c)) = target {
                        // Save the return address, then switch to the callee.
                        if let Some(fr) = self.frames.last_mut() {
                            fr.ip = ip;
                        }
                        tri!(self.push_frame(f, callee_pos, argc, c.clone()));
                        ip = 0;
                        base = callee_pos + 1;
                        func = f;
                        closure = c;
                        code = &program.functions[f as usize].code;
                    }
                }};
            }
            macro_rules! do_return {
                ($result:expr) => {{
                    let result = $result;
                    let frame = self.frames.pop().expect("return without frame");
                    self.stack.truncate(frame.base - 1);
                    if self.frames.len() <= stop_depth {
                        return Ok(result);
                    }
                    self.stack.push(result);
                    let caller = self.frames.last().expect("caller frame");
                    ip = caller.ip;
                    base = caller.base;
                    func = caller.func;
                    closure = caller.closure.clone();
                    code = &program.functions[caller.func as usize].code;
                }};
            }
            macro_rules! name_at {
                ($i:expr) => {
                    match &program.constants[$i] {
                        Constant::Str(s) => s.as_str(),
                        _ => fail!("name operand is not a string"),
                    }
                };
            }

            self.steps += 1;
            if self.steps & 0x3FF == 0 {
                tri!(self.check_budget());
            }

            // Programs are verified, so the opcode byte is always valid.
            let op = match Op::from_u8(code[ip]) {
                Some(op) => op,
                None => fail!("invalid opcode"),
            };
            ip += 1;

            match op {
                Op::Const => {
                    let i = u16_operand!();
                    self.stack.push(self.constants[i].clone());
                }
                Op::Nil => self.stack.push(Value::Nil),
                Op::True => self.stack.push(Value::Bool(true)),
                Op::False => self.stack.push(Value::Bool(false)),
                Op::Pop => {
                    pop!();
                }
                Op::Dup2 => {
                    let n = self.stack.len();
                    if n < 2 {
                        fail!("stack underflow");
                    }
                    self.stack.extend_from_within(n - 2..);
                }
                Op::GetLocal => {
                    let slot = base + u16_operand!();
                    let v = tri!(self.stack.get(slot).cloned().ok_or("invalid local slot"));
                    self.stack.push(v);
                }
                Op::SetLocal => {
                    let slot = base + u16_operand!();
                    let v = pop!();
                    match self.stack.get_mut(slot) {
                        Some(dst) => *dst = v,
                        None => fail!("invalid local slot"),
                    }
                }
                Op::GetGlobal => {
                    let i = u16_operand!();
                    self.stack.push(self.globals[i].clone());
                }
                Op::SetGlobal => {
                    let i = u16_operand!();
                    self.globals[i] = pop!();
                }
                Op::Func => {
                    let i = u16_operand!();
                    self.stack.push(Value::Func(i as u16));
                }
                Op::Builtin => {
                    let i = u8_operand!();
                    self.stack.push(Value::Builtin(i));
                }
                Op::Add | Op::Sub | Op::Mul | Op::Div | Op::Rem => {
                    let b = pop!();
                    let a = pop!();
                    let v = tri!(arith(op, a, b));
                    self.stack.push(v);
                }
                Op::Neg => {
                    let v = match pop!() {
                        Value::Int(i) => Value::Int(tri!(i.checked_neg().ok_or("integer overflow"))),
                        Value::Float(f) => Value::Float(-f),
                        other => fail!(format!("cannot negate a value of type {}", other.type_name())),
                    };
                    self.stack.push(v);
                }
                Op::Not => match pop!() {
                    Value::Bool(b) => self.stack.push(Value::Bool(!b)),
                    other => fail!(format!("`not` expects a bool, found {}", other.type_name())),
                },
                Op::Eq | Op::Ne => {
                    let b = pop!();
                    let a = pop!();
                    self.stack.push(Value::Bool(a.equals(&b) == (op == Op::Eq)));
                }
                Op::Lt | Op::Le | Op::Gt | Op::Ge => {
                    let b = pop!();
                    let a = pop!();
                    let v = tri!(compare(op, &a, &b));
                    self.stack.push(Value::Bool(v));
                }
                Op::Range => {
                    let b = pop!();
                    let a = pop!();
                    match (a, b) {
                        (Value::Int(a), Value::Int(b)) => self.stack.push(Value::Range(a, b)),
                        (a, b) => fail!(format!("range bounds must be integers, found {} and {}", a.type_name(), b.type_name())),
                    }
                }
                Op::Jump => ip = u32_operand!(),
                Op::JumpIfFalse => {
                    let target = u32_operand!();
                    match pop!() {
                        Value::Bool(true) => {}
                        Value::Bool(false) => ip = target,
                        other => fail!(format!("condition must be a bool, found {}", other.type_name())),
                    }
                }
                Op::JumpIfFalseKeep | Op::JumpIfTrueKeep => {
                    let target = u32_operand!();
                    let jump_on = op == Op::JumpIfTrueKeep;
                    match self.stack.last() {
                        Some(Value::Bool(b)) if *b == jump_on => ip = target,
                        Some(Value::Bool(_)) => {
                            self.stack.pop();
                        }
                        Some(other) => fail!(format!(
                            "operands of `{}` must be bools, found {}",
                            if jump_on { "or" } else { "and" },
                            other.type_name()
                        )),
                        None => fail!("stack underflow"),
                    }
                }
                Op::Call => {
                    let argc = u8_operand!() as usize;
                    let Some(callee_pos) = self.stack.len().checked_sub(argc + 1) else { fail!("stack underflow") };
                    do_call!(callee_pos, argc);
                }
                Op::Method => {
                    let name = name_at!(u16_operand!());
                    let argc = u8_operand!() as usize;
                    let Some(callee_pos) = self.stack.len().checked_sub(argc + 2) else { fail!("stack underflow") };
                    let method = match &self.stack[callee_pos + 1] {
                        Value::Struct(s) => s.ty.method(name),
                        Value::Variant(v) => v.ty.method(name),
                        _ => None,
                    };
                    match method {
                        Some(f) => self.stack[callee_pos] = Value::Func(f),
                        None if matches!(self.stack[callee_pos], Value::Nil) => {
                            fail!(format!("value of type {} has no method `{name}`", self.stack[callee_pos + 1].type_name()))
                        }
                        None => {}
                    }
                    do_call!(callee_pos, argc + 1);
                }
                Op::Return => {
                    let result = pop!();
                    do_return!(result);
                }
                Op::Try => {
                    let value = pop!();
                    match &value {
                        Value::Variant(v) if v.type_index == OPTION_TYPE || v.type_index == RESULT_TYPE => {
                            const _: () = assert!(SOME == OK, "`Some` and `Ok` share a tag");
                            if v.tag == SOME {
                                self.stack.push(v.values[0].clone());
                            } else {
                                do_return!(value);
                            }
                        }
                        other => fail!(format!("`?` expects an Option or Result, found {}", other.type_name())),
                    }
                }
                Op::Dup => {
                    let Some(top) = self.stack.last().cloned() else { fail!("stack underflow") };
                    self.stack.push(top);
                }
                Op::Map => {
                    let n = u16_operand!();
                    let Some(start) = self.stack.len().checked_sub(n * 2) else { fail!("stack underflow") };
                    let items = self.stack.split_off(start);
                    let mut entries = alloc::collections::BTreeMap::new();
                    for pair in items.as_chunks::<2>().0 {
                        entries.insert(tri!(MapKey::from_value(&pair[0])), pair[1].clone());
                    }
                    self.stack.push(Value::map(entries));
                }
                Op::GetField => {
                    let name = name_at!(u16_operand!());
                    let v = match pop!() {
                        Value::Struct(s) => match s.ty.fields().iter().position(|f| f == name) {
                            Some(i) => s.fields.borrow()[i].clone(),
                            None => fail!(format!("`{}` has no field `{name}`", s.ty.name)),
                        },
                        other => fail!(format!("value of type {} has no field `{name}`", other.type_name())),
                    };
                    self.stack.push(v);
                }
                Op::SetField => {
                    let name = name_at!(u16_operand!());
                    let value = pop!();
                    match pop!() {
                        Value::Struct(s) => match s.ty.fields().iter().position(|f| f == name) {
                            Some(i) => s.fields.borrow_mut()[i] = value,
                            None => fail!(format!("`{}` has no field `{name}`", s.ty.name)),
                        },
                        other => fail!(format!("value of type {} has no field `{name}`", other.type_name())),
                    }
                }
                Op::NewStruct => {
                    let t = u16_operand!();
                    let ty = Rc::clone(&self.types[t]);
                    let Some(start) = self.stack.len().checked_sub(ty.fields().len()) else { fail!("stack underflow") };
                    let fields = self.stack.split_off(start);
                    self.stack.push(Value::Struct(Rc::new(StructObj { ty, type_index: t as u16, fields: RefCell::new(fields) })));
                }
                Op::NewVariant => {
                    let t = u16_operand!();
                    let tag = u16_operand!();
                    let arity = self.types[t].variants()[tag].1 as usize;
                    let Some(start) = self.stack.len().checked_sub(arity) else { fail!("stack underflow") };
                    let values = self.stack.split_off(start);
                    let v = self.variant(t as u16, tag as u16, values);
                    self.stack.push(v);
                }
                Op::IsType => {
                    let t = u16_operand!() as u16;
                    let tag = u16_operand!() as u16;
                    let ok = match pop!() {
                        Value::Struct(s) => s.type_index == t && tag == ANY_VARIANT,
                        Value::Variant(v) => v.type_index == t && (tag == ANY_VARIANT || v.tag == tag),
                        _ => false,
                    };
                    self.stack.push(Value::Bool(ok));
                }
                Op::GetItem => {
                    let i = u8_operand!() as usize;
                    let v = match pop!() {
                        Value::Struct(s) => s.fields.borrow().get(i).cloned(),
                        Value::Variant(v) => v.values.get(i).cloned(),
                        _ => None,
                    };
                    match v {
                        Some(v) => self.stack.push(v),
                        None => fail!("pattern item out of range"),
                    }
                }
                Op::InRange => {
                    let hi = pop!();
                    let lo = pop!();
                    let v = pop!();
                    let ok =
                        matches!(v, Value::Int(_) | Value::Float(_)) && tri!(compare(Op::Ge, &v, &lo)) && tri!(compare(Op::Lt, &v, &hi));
                    self.stack.push(Value::Bool(ok));
                }
                Op::Closure => {
                    let f = u16_operand!() as u16;
                    let n = u8_operand!() as usize;
                    let Some(start) = self.stack.len().checked_sub(n) else { fail!("stack underflow") };
                    let captures = self.stack.split_off(start);
                    self.stack.push(Value::Closure(Rc::new(Closure { func: f, captures })));
                }
                Op::GetCapture => {
                    let i = u8_operand!() as usize;
                    match closure.as_ref().and_then(|c| c.captures.get(i)) {
                        Some(v) => self.stack.push(v.clone()),
                        None => fail!("captured value out of range"),
                    }
                }
                Op::Current => {
                    let me = match &closure {
                        Some(c) => Value::Closure(Rc::clone(c)),
                        None => Value::Func(func),
                    };
                    self.stack.push(me);
                }
                Op::List => {
                    let n = u16_operand!();
                    let Some(start) = self.stack.len().checked_sub(n) else { fail!("stack underflow") };
                    let items = self.stack.split_off(start);
                    self.stack.push(Value::list(items));
                }
                Op::Index => {
                    let index = pop!();
                    let target = pop!();
                    let v = tri!(index_get(&target, &index));
                    self.stack.push(v);
                }
                Op::SetIndex => {
                    let value = pop!();
                    let index = pop!();
                    let target = pop!();
                    tri!(index_set(&target, &index, value));
                }
                Op::Bind => {
                    let argc = u8_operand!() as usize;
                    let Some(start) = self.stack.len().checked_sub(argc) else { fail!("stack underflow") };
                    let args = self.stack.split_off(start);
                    let callee = pop!();
                    if !callee.is_callable() {
                        fail!(format!("event handler must be a function, found {}", callee.type_name()));
                    }
                    self.stack.push(Value::Bound(Rc::new(Bound { callee, args })));
                }
                Op::Concat => {
                    use core::fmt::Write as _;
                    let n = u16_operand!();
                    let Some(start) = self.stack.len().checked_sub(n) else { fail!("stack underflow") };
                    let mut s = String::new();
                    for v in self.stack.drain(start..) {
                        let _ = write!(s, "{v}");
                    }
                    self.stack.push(Value::Str(Rc::from(s)));
                }
                Op::IterInit => {
                    let iter = match pop!() {
                        Value::Range(a, b) => Iter::Range { next: a, end: b },
                        Value::List(list) => Iter::List { list, index: 0 },
                        // Maps iterate over a snapshot of their keys.
                        Value::Map(map) => {
                            let keys = map.borrow().keys().map(MapKey::to_value).collect();
                            Iter::List { list: Rc::new(RefCell::new(keys)), index: 0 }
                        }
                        Value::Str(text) => Iter::Str { text, offset: 0 },
                        Value::Iter(it) => {
                            self.stack.push(Value::Iter(it));
                            continue;
                        }
                        other => fail!(format!("cannot iterate over a value of type {}", other.type_name())),
                    };
                    self.stack.push(Value::Iter(Rc::new(RefCell::new(iter))));
                }
                Op::IterNext => {
                    let slot = base + u16_operand!();
                    let exit = u32_operand!();
                    let next = match self.stack.get(slot) {
                        Some(Value::Iter(it)) => it.borrow_mut().next(),
                        _ => fail!("for-loop iterator slot does not hold an iterator"),
                    };
                    match next {
                        Some(v) => self.stack.push(v),
                        None => ip = exit,
                    }
                }
                Op::UiBegin => {
                    let kind = tri!(UiKind::from_u8(u8_operand!()).ok_or("invalid UI element"));
                    let node = tri!(element(kind, pop!()));
                    match self.ui.as_mut() {
                        Some(ui) => tri!(ui.open(node)),
                        None => fail!(UI_OUTSIDE_VIEW),
                    }
                }
                Op::UiAttr => {
                    let attr = tri!(Attr::from_u8(u8_operand!()).ok_or("invalid UI attribute"));
                    let value = pop!();
                    let Some(node) = self.ui.as_mut().and_then(UiBuilder::current_mut) else {
                        fail!(format!("attribute `{}` used outside of an element", attr.name()))
                    };
                    if !attr.applies_to(node.kind) {
                        fail!(attr.not_applicable(node.kind));
                    }
                    let checked = tri!(coerce_attr(attr, node.kind, &value));
                    node.set_attr(attr, checked);
                }
                Op::UiOn => {
                    let handler = pop!();
                    let Some(node) = self.ui.as_mut().and_then(UiBuilder::current_mut) else { fail!(UI_OUTSIDE_VIEW) };
                    if node.kind.event() == Event::None {
                        fail!(format!("`{}` does not accept an event handler", node.kind.name()));
                    }
                    if !handler.is_callable() {
                        fail!(format!("event handler must be a function, found {}", handler.type_name()));
                    }
                    node.action = Some(self.actions.len());
                    self.actions.push(handler);
                }
                // Format 1 opcodes, kept so packages built by older compilers still run.
                Op::UiOpen => {
                    let kind = UiKind::from_u8(u8_operand!()).unwrap_or(UiKind::Column);
                    let arg = pop!();
                    let text = if matches!(arg, Value::Nil) { String::new() } else { arg.to_string() };
                    match self.ui.as_mut() {
                        Some(ui) => tri!(ui.open(UiNode::new(kind, text))),
                        None => fail!(UI_OUTSIDE_VIEW),
                    }
                }
                Op::UiClose => match self.ui.as_mut() {
                    Some(ui) => tri!(ui.close()),
                    None => fail!(UI_OUTSIDE_VIEW),
                },
                Op::UiLabel => {
                    let text = pop!().to_string();
                    match self.ui.as_mut() {
                        Some(ui) => tri!(ui.leaf(UiNode::new(UiKind::Label, text))),
                        None => fail!(UI_OUTSIDE_VIEW),
                    }
                }
                Op::UiButton => {
                    let action = pop!();
                    let text = pop!().to_string();
                    let mut node = UiNode::new(UiKind::Button, text);
                    match action {
                        Value::Nil => {}
                        a if a.is_callable() => {
                            node.action = Some(self.actions.len());
                            self.actions.push(a);
                        }
                        other => fail!(format!("button handler must be a function, found {}", other.type_name())),
                    }
                    match self.ui.as_mut() {
                        Some(ui) => tri!(ui.leaf(node)),
                        None => fail!(UI_OUTSIDE_VIEW),
                    }
                }
            }
        }
    }
}

const UI_OUTSIDE_VIEW: &str = "UI elements can only be used while rendering a `view`";

/// Build an element from its argument, checking the argument's type.
fn element(kind: UiKind, arg: Value) -> Result<UiNode, String> {
    let mut node = UiNode::new(kind, String::new());
    let ty = match (kind.arg(), &arg) {
        (Arg::None, Value::Nil) | (Arg::Optional(_), Value::Nil) => return Ok(node),
        (Arg::None, other) => return Err(format!("`{}` does not take an argument, found {}", kind.name(), other.type_name())),
        (Arg::Optional(ty) | Arg::Required(ty), _) => ty,
    };
    match (ty, arg) {
        (ArgType::Text, v) => node.text = v.to_string(),
        (ArgType::Str, Value::Str(s)) => {
            node.text = s.to_string();
            node.value = Some(UiValue::Text(s.to_string()));
        }
        (ArgType::Bool, Value::Bool(b)) => node.value = Some(UiValue::Bool(b)),
        (ArgType::Number, Value::Int(i)) => node.value = Some(UiValue::Int(i)),
        (ArgType::Number, Value::Float(f)) => node.value = Some(UiValue::Float(f)),
        (ty, other) => return Err(format!("`{}` expects {}, found {}", kind.name(), ty.describe(), other.type_name())),
    }
    Ok(node)
}

fn coerce_attr(attr: Attr, kind: UiKind, value: &Value) -> Result<justc_core::ui::AttrValue, String> {
    // Text attributes show any value exactly like string interpolation does.
    let shown;
    let raw = match value {
        _ if attr.spec().ty == AttrType::Text => {
            shown = value.to_string();
            RawValue::Str(&shown)
        }
        Value::Int(i) => RawValue::Int(*i),
        Value::Float(f) => RawValue::Float(*f),
        Value::Bool(b) => RawValue::Bool(*b),
        Value::Str(s) => RawValue::Str(s),
        other => RawValue::Other(other.kind_name()),
    };
    attr.coerce(kind, raw)
}

impl From<UiValue> for Value {
    fn from(v: UiValue) -> Value {
        match v {
            UiValue::Text(s) => Value::Str(Rc::from(s)),
            UiValue::Bool(b) => Value::Bool(b),
            UiValue::Int(i) => Value::Int(i),
            UiValue::Float(f) => Value::Float(f),
        }
    }
}

fn arith(op: Op, a: Value, b: Value) -> Result<Value, String> {
    use Value::*;
    let overflow = || "integer overflow".to_string();
    Ok(match (a, b) {
        (Int(x), Int(y)) => Int(match op {
            Op::Add => x.checked_add(y).ok_or_else(overflow)?,
            Op::Sub => x.checked_sub(y).ok_or_else(overflow)?,
            Op::Mul => x.checked_mul(y).ok_or_else(overflow)?,
            Op::Div | Op::Rem if y == 0 => return Err("division by zero".into()),
            Op::Div => x.checked_div(y).ok_or_else(overflow)?,
            Op::Rem => x.checked_rem(y).ok_or_else(overflow)?,
            _ => unreachable!(),
        }),
        (Int(x), Float(y)) => float_arith(op, x as f64, y),
        (Float(x), Int(y)) => float_arith(op, x, y as f64),
        (Float(x), Float(y)) => float_arith(op, x, y),
        (Str(x), Str(y)) if op == Op::Add => {
            let mut s = String::with_capacity(x.len() + y.len());
            s.push_str(&x);
            s.push_str(&y);
            Str(Rc::from(s))
        }
        (Str(s), Int(n)) | (Int(n), Str(s)) if op == Op::Mul => {
            if n < 0 || s.len().saturating_mul(n as usize) > 64 << 20 {
                return Err("invalid string repetition count".into());
            }
            Str(Rc::from(s.repeat(n as usize)))
        }
        (List(x), List(y)) if op == Op::Add => {
            let mut items = x.borrow().clone();
            items.extend(y.borrow().iter().cloned());
            Value::list(items)
        }
        (a, b) => {
            let sym = match op {
                Op::Add => "+",
                Op::Sub => "-",
                Op::Mul => "*",
                Op::Div => "/",
                _ => "%",
            };
            return Err(format!("unsupported operand types for `{sym}`: {} and {}", a.type_name(), b.type_name()));
        }
    })
}

fn float_arith(op: Op, x: f64, y: f64) -> Value {
    Value::Float(match op {
        Op::Add => x + y,
        Op::Sub => x - y,
        Op::Mul => x * y,
        Op::Div => x / y,
        _ => x % y,
    })
}

fn compare(op: Op, a: &Value, b: &Value) -> Result<bool, String> {
    use core::cmp::Ordering as O;
    let ord = match (a, b) {
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Int(x), Value::Float(y)) => (*x as f64).partial_cmp(y).unwrap_or(O::Less),
        (Value::Float(x), Value::Int(y)) => x.partial_cmp(&(*y as f64)).unwrap_or(O::Less),
        (Value::Float(x), Value::Float(y)) => match x.partial_cmp(y) {
            Some(o) => o,
            None => return Ok(false), // NaN compares false
        },
        (Value::Str(x), Value::Str(y)) => x.cmp(y),
        _ => return Err(format!("cannot compare {} with {}", a.type_name(), b.type_name())),
    };
    Ok(match op {
        Op::Lt => ord == O::Less,
        Op::Le => ord != O::Greater,
        Op::Gt => ord == O::Greater,
        _ => ord != O::Less,
    })
}

fn normalize_index(index: &Value, len: usize) -> Result<usize, String> {
    let Value::Int(i) = *index else { return Err(format!("index must be an int, found {}", index.type_name())) };
    let resolved = if i < 0 { len as i64 + i } else { i };
    if resolved < 0 || resolved as usize >= len {
        return Err(format!("index {i} out of bounds (length {len})"));
    }
    Ok(resolved as usize)
}

fn index_get(target: &Value, index: &Value) -> Result<Value, String> {
    match target {
        Value::Map(map) => {
            let key = MapKey::from_value(index)?;
            map.borrow().get(&key).cloned().ok_or_else(|| format!("key {} not found in map (use `get` for an Option)", key_repr(index)))
        }
        Value::List(list) => {
            let list = list.borrow();
            Ok(list[normalize_index(index, list.len())?].clone())
        }
        Value::Str(s) => {
            let n = s.chars().count();
            let i = normalize_index(index, n)?;
            let c = s.chars().nth(i).expect("index checked");
            Ok(Value::Str(Rc::from(c.encode_utf8(&mut [0; 4]) as &str)))
        }
        other => Err(format!("cannot index into a value of type {}", other.type_name())),
    }
}

/// A key as written in source: strings quoted.
fn key_repr(v: &Value) -> String {
    match v {
        Value::Str(s) => format!("{:?}", &**s),
        other => other.to_string(),
    }
}

fn index_set(target: &Value, index: &Value, value: Value) -> Result<(), String> {
    match target {
        Value::Map(map) => {
            map.borrow_mut().insert(MapKey::from_value(index)?, value);
            Ok(())
        }
        Value::List(list) => {
            let mut list = list.borrow_mut();
            let i = normalize_index(index, list.len())?;
            list[i] = value;
            Ok(())
        }
        Value::Str(_) => Err("strings are immutable".into()),
        other => Err(format!("cannot index into a value of type {}", other.type_name())),
    }
}

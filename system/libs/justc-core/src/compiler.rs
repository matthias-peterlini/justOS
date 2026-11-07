//! AST → JustVM bytecode compiler.
//!
//! Scoping rules:
//! * `let` at the top level of the file declares a *global*.
//! * `let` anywhere else declares a block-scoped *local* (stack slot).
//! * Top-level functions, structs and enums may be referenced before their
//!   declaration.
//! * Nested functions and lambdas are closures: they capture the variables
//!   of enclosing functions *by value* when they are created, so assigning
//!   to a captured variable is an error (share state through a list, map,
//!   struct or global instead).
//!
//! Name resolution order: locals (innermost first) → the enclosing nested
//! function itself → captured variables → globals → functions → types →
//! `Some`/`None`/`Ok`/`Err` → builtins.

use crate::ast::*;
use crate::bytecode::{
    builtin_index, std_members, Constant, Function, Op, Program, TypeDef, TypeShape, ANY_VARIANT, BUILTINS, ERR, NONE, OK, OPTION_TYPE,
    RESULT_TYPE, SOME, STD_CONSTANTS, STD_MODULES, STD_PROPERTIES,
};
use crate::diagnostic::Diagnostic;
use crate::prelude::*;
use crate::span::{SourceFile, Span};
use crate::suggest::best_match;
use crate::ui::{Arg, ArgType, Attr, Event, RawValue};
use alloc::collections::{BTreeMap as HashMap, BTreeSet as HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompileOptions {
    /// Constant folding and other cheap AST-level optimizations.
    pub optimize: bool,
    /// Emit line tables for runtime stack traces.
    pub debug_info: bool,
}

impl CompileOptions {
    pub const DEBUG: CompileOptions = CompileOptions { optimize: false, debug_info: true };
    pub const RELEASE: CompileOptions = CompileOptions { optimize: true, debug_info: false };
}

impl Default for CompileOptions {
    fn default() -> Self {
        CompileOptions::DEBUG
    }
}

/// A parsed file of a program: the main file (`name: None`) or an
/// imported module.
pub struct Unit<'a> {
    pub name: Option<String>,
    pub module: &'a Module,
}

/// Compile a parsed module. Returns all diagnostics (including warnings) on failure.
pub fn compile(module: &Module, source: &SourceFile, opts: CompileOptions) -> Result<(Program, Vec<Diagnostic>), Vec<Diagnostic>> {
    compile_units(&[Unit { name: None, module }], source, opts)
}

/// Compile a program made of a main file and the modules it imports
/// (main file first). Every `import` must name one of the units.
pub fn compile_units(units: &[Unit<'_>], source: &SourceFile, opts: CompileOptions) -> Result<(Program, Vec<Diagnostic>), Vec<Diagnostic>> {
    let mut c = Compiler::new(source, opts);
    c.compile_program(units);
    if c.diagnostics.iter().any(Diagnostic::is_error) {
        Err(c.diagnostics)
    } else {
        let warnings = core::mem::take(&mut c.diagnostics);
        Ok((c.finish(), warnings))
    }
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum ConstKey {
    Int(i64),
    Float(u64),
    Str(String),
}

struct LoopCtx {
    continue_target: usize,
    breaks: Vec<usize>,
}

/// Per-function code generation state.
struct FnState {
    code: Vec<u8>,
    lines: Vec<(u32, u32)>,
    /// Stack of lexical scopes, each a list of `(name, slot)`.
    scopes: Vec<Vec<(String, u16)>>,
    next_slot: u16,
    max_slots: u16,
    loops: Vec<LoopCtx>,
    /// `true` for `<main>`: top-level `let`s become globals.
    is_main: bool,
    /// `true` for the `view` body.
    is_view: bool,
    /// Elements lexically enclosing the code being compiled, innermost last.
    ui: Vec<UiKind>,
    /// A function body where `?` may return (not `<main>` or the view).
    is_fn: bool,
    /// A nested function or lambda: may capture enclosing locals.
    is_closure: bool,
    /// Name of a nested `fn`, which refers to itself inside its body.
    self_name: Option<String>,
    /// Captured variables: name and where to read it in the enclosing function.
    captures: Vec<(String, Var)>,
}

impl FnState {
    fn new(is_main: bool) -> Self {
        FnState {
            code: Vec::new(),
            lines: Vec::new(),
            scopes: vec![Vec::new()],
            next_slot: 0,
            max_slots: 0,
            loops: Vec::new(),
            is_main,
            is_view: false,
            ui: Vec::new(),
            is_fn: false,
            is_closure: false,
            self_name: None,
            captures: Vec::new(),
        }
    }

    fn function() -> Self {
        FnState { is_fn: true, ..FnState::new(false) }
    }

    fn lookup_local(&self, name: &str) -> Option<u16> {
        self.scopes.iter().rev().flat_map(|s| s.iter().rev()).find(|(n, _)| n == name).map(|&(_, slot)| slot)
    }
}

/// The body of a closure being compiled.
enum ClosureBody<'a> {
    Block(&'a Block),
    Expr(&'a Expr),
}

/// Static information about a struct, for construction and patterns.
struct StructInfo {
    fields: Vec<String>,
    defaults: Vec<Option<Expr>>,
    /// The module declaring it (defaults are compiled there).
    module: Option<String>,
}

/// What an imported name refers to.
#[derive(Clone, Debug)]
enum ImportTarget {
    /// `import utils`: the module, used as `utils.name`.
    Module(String),
    /// `import utils.{name}`: the qualified name `utils.name`.
    Name(String),
}

struct Compiler<'s> {
    source: &'s SourceFile,
    opts: CompileOptions,
    constants: Vec<Constant>,
    const_index: HashMap<ConstKey, u16>,
    functions: Vec<Function>,
    fn_index: HashMap<String, (u16, u8)>,
    globals: Vec<String>,
    global_index: HashMap<String, u16>,
    diagnostics: Vec<Diagnostic>,
    f: FnState,
    entry: u16,
    view: Option<u16>,
    /// Synthetic `<set name>` functions used by two-way bound elements, by global.
    global_setters: HashMap<u16, u16>,
    /// Synthetic `<set index>` function: `(container, index, value)`.
    index_setter: Option<u16>,
    /// Synthetic `<set .name>` functions: `(struct, value)`, by field name.
    field_setters: HashMap<String, u16>,
    /// Enclosing functions of the closure being compiled, outermost first.
    outer: Vec<FnState>,
    types: Vec<TypeDef>,
    type_index: HashMap<String, u16>,
    structs: HashMap<u16, StructInfo>,
    /// Functions declared in a type's block: `(type, name)` → (function, arity).
    assoc: HashMap<(u16, String), (u16, u8)>,
    /// Every field name of every struct, for spotting typos.
    field_names: HashSet<String>,
    /// Every method name (functions taking `self`).
    method_names: HashSet<String>,
    /// Compiling a struct field default: only globals and functions are visible.
    detached: bool,
    /// The module being compiled (`None` for the main file). Its top-level
    /// names are stored qualified: `utils.helper`.
    cur_module: Option<String>,
    /// Names brought into scope by `import`, per importing module.
    imports: HashMap<(Option<String>, String), ImportTarget>,
    /// `<init module>` functions, which run a module's top-level code once.
    module_inits: HashMap<String, u16>,
}

impl<'s> Compiler<'s> {
    fn new(source: &'s SourceFile, opts: CompileOptions) -> Self {
        Compiler {
            source,
            opts,
            constants: Vec::new(),
            const_index: HashMap::new(),
            functions: Vec::new(),
            fn_index: HashMap::new(),
            globals: Vec::new(),
            global_index: HashMap::new(),
            diagnostics: Vec::new(),
            f: FnState::new(true),
            entry: 0,
            view: None,
            global_setters: HashMap::new(),
            index_setter: None,
            field_setters: HashMap::new(),
            outer: Vec::new(),
            type_index: TypeDef::builtins().iter().enumerate().map(|(i, t)| (t.name.clone(), i as u16)).collect(),
            types: TypeDef::builtins(),
            structs: HashMap::new(),
            assoc: HashMap::new(),
            field_names: HashSet::new(),
            method_names: HashSet::new(),
            detached: false,
            cur_module: None,
            imports: HashMap::new(),
            module_inits: HashMap::new(),
        }
    }

    /// A top-level name of the current module, as stored.
    fn q(&self, name: &str) -> String {
        match &self.cur_module {
            Some(m) => format!("{m}.{name}"),
            None => name.to_owned(),
        }
    }

    /// Find a top-level name of the current module, or one imported into it.
    fn find<T: Copy>(&self, map: &HashMap<String, T>, name: &str) -> Option<T> {
        if let Some(v) = map.get(&self.q(name)) {
            return Some(*v);
        }
        match self.imports.get(&(self.cur_module.clone(), name.to_owned())) {
            Some(ImportTarget::Name(full)) => map.get(full).copied(),
            _ => None,
        }
    }

    fn find_fn(&self, name: &str) -> Option<(u16, u8)> {
        self.find(&self.fn_index, name)
    }

    fn find_global(&self, name: &str) -> Option<u16> {
        self.find(&self.global_index, name)
    }

    fn find_type(&self, name: &str) -> Option<u16> {
        // Built-in types are visible everywhere.
        self.find(&self.type_index, name).or_else(|| matches!(name, "Option" | "Result").then(|| self.type_index[name]))
    }

    /// A builtin called by a bare name: `len`, or `sin` after `import math.{sin}`.
    fn find_builtin(&self, name: &str) -> Option<u8> {
        match self.imports.get(&(self.cur_module.clone(), name.to_owned())) {
            Some(ImportTarget::Name(full)) => builtin_index(full).filter(|b| !STD_PROPERTIES.contains(&BUILTINS[*b as usize].name)),
            _ => builtin_index(name),
        }
    }

    /// The member `full` (`math.sin`) of a standard library module.
    fn std_member(full: &str) -> Option<Var> {
        if let Some(&(_, v)) = STD_CONSTANTS.iter().find(|(n, _)| *n == full) {
            return Some(Var::StdConst(v));
        }
        let b = builtin_index(full)?;
        Some(if STD_PROPERTIES.contains(&full) { Var::Property(b) } else { Var::Builtin(b) })
    }

    /// Report `fs.read(...)` written without `import fs`.
    fn unimported_std_module(&mut self, expr: &Expr) -> bool {
        let ExprKind::Ident(name) = &expr.kind else { return false };
        if !STD_MODULES.contains(&name.as_str()) || self.resolve_quiet(name).is_some() || self.module_alias(expr).is_some() {
            return false;
        }
        self.diagnostics.push(
            Diagnostic::error(format!("the standard library module `{name}` is not imported"), expr.span)
                .with_label("not imported")
                .with_note(format!("add `import {name}` at the top of the file")),
        );
        true
    }

    /// The module that `expr` names (`utils` after `import utils`), unless a
    /// variable shadows it.
    fn module_alias(&self, expr: &Expr) -> Option<String> {
        let ExprKind::Ident(name) = &expr.kind else { return None };
        if self.lookup_local(name).is_some() || self.find_global(name).is_some() || self.find_fn(name).is_some() {
            return None;
        }
        match self.imports.get(&(self.cur_module.clone(), name.clone())) {
            Some(ImportTarget::Module(m)) => Some(m.clone()),
            _ => None,
        }
    }

    fn finish(self) -> Program {
        Program {
            constants: self.constants,
            functions: self.functions,
            types: self.types,
            globals: self.globals,
            entry: self.entry,
            view: self.view,
        }
    }

    fn error(&mut self, msg: impl Into<String>, span: Span) {
        self.diagnostics.push(Diagnostic::error(msg, span));
    }

    // ----------------------------------------------------------- declarations

    fn compile_program(&mut self, units: &[Unit<'_>]) {
        // Pass 0: the modules each file imports.
        let known: HashSet<&str> = units.iter().filter_map(|u| u.name.as_deref()).collect();
        for unit in units {
            for stmt in &unit.module.stmts {
                if let StmtKind::Import(import) = &stmt.kind {
                    let m = &import.module.name;
                    if STD_MODULES.contains(&m.as_str()) {
                        self.imports.insert((unit.name.clone(), m.clone()), ImportTarget::Module(m.clone()));
                    } else if !known.contains(m.as_str()) {
                        self.error(format!("cannot find module `{m}`"), import.module.span);
                    } else if unit.name.as_deref() == Some(m) {
                        self.error("a module cannot import itself", import.module.span);
                    } else {
                        self.imports.insert((unit.name.clone(), m.clone()), ImportTarget::Module(m.clone()));
                    }
                }
            }
        }
        for unit in units.iter().filter_map(|u| u.name.as_ref()) {
            let init = self.reserve_function(&format!("<init {unit}>"));
            self.module_inits.insert(unit.clone(), init);
        }

        // Pass 1: declare types, functions, the view and top-level globals of
        // every file so that forward references resolve.
        let mut fn_decls = Vec::new();
        let mut view_body = None;
        for unit in units {
            self.cur_module = unit.name.clone();
            for stmt in &unit.module.stmts {
                if let StmtKind::Struct(decl) = &stmt.kind {
                    self.declare_type(decl, &mut fn_decls);
                }
            }
        }
        for unit in units {
            self.cur_module = unit.name.clone();
            for stmt in &unit.module.stmts {
                match &stmt.kind {
                    StmtKind::Fn(decl) => {
                        let name = self.q(&decl.name.name);
                        if self.fn_index.contains_key(&name) {
                            self.error(format!("function `{}` is defined more than once", decl.name.name), decl.name.span);
                            continue;
                        }
                        if builtin_index(&decl.name.name).is_some() {
                            self.diagnostics
                                .push(Diagnostic::warning(format!("function `{}` shadows a builtin", decl.name.name), decl.name.span));
                        }
                        if self.type_index.contains_key(&name) {
                            self.error(format!("`{}` is already the name of a type", decl.name.name), decl.name.span);
                        }
                        let idx = self.reserve_function(&name);
                        self.fn_index.insert(name, (idx, decl.params.len().min(255) as u8));
                        fn_decls.push((idx, decl, unit.name.clone()));
                    }
                    StmtKind::View(body) => {
                        if unit.name.is_some() {
                            self.error("only the main file can declare a `view`", stmt.span);
                        } else if view_body.is_some() {
                            self.error("only one `view` may be declared per program", stmt.span);
                        } else {
                            view_body = Some(body);
                        }
                    }
                    StmtKind::Let { name, .. } => {
                        self.declare_global(&self.q(&name.name));
                    }
                    _ => {}
                }
            }
        }
        // Names imported directly: `import utils.{a, b}`.
        for unit in units {
            for stmt in &unit.module.stmts {
                let StmtKind::Import(import) = &stmt.kind else { continue };
                let m = &import.module.name;
                if STD_MODULES.contains(&m.as_str()) {
                    for name in &import.names {
                        let full = format!("{m}.{}", name.name);
                        if Self::std_member(&full).is_some() {
                            self.imports.insert((unit.name.clone(), name.name.clone()), ImportTarget::Name(full));
                        } else {
                            self.unknown_module_member(m, name);
                        }
                    }
                    continue;
                }
                if !known.contains(m.as_str()) {
                    continue;
                }
                for name in &import.names {
                    let full = format!("{m}.{}", name.name);
                    if self.fn_index.contains_key(&full) || self.global_index.contains_key(&full) || self.type_index.contains_key(&full) {
                        self.imports.insert((unit.name.clone(), name.name.clone()), ImportTarget::Name(full));
                    } else {
                        let prefix = format!("{m}.");
                        let members: Vec<&str> = self
                            .fn_index
                            .keys()
                            .chain(self.global_index.keys())
                            .chain(self.type_index.keys())
                            .filter_map(|k| k.strip_prefix(&prefix))
                            .filter(|k| !k.contains('.'))
                            .collect();
                        let mut d = Diagnostic::error(format!("module `{m}` has no `{}`", name.name), name.span);
                        if let Some(similar) = best_match(&name.name, members) {
                            d = d.with_note(format!("a similar name exists: `{similar}`"));
                        }
                        self.diagnostics.push(d);
                    }
                }
            }
        }

        // Pass 2: the entry point, then each module's top-level code.
        self.entry = self.reserve_function("<main>");
        for unit in units {
            self.cur_module = unit.name.clone();
            self.f = FnState::new(true);
            let guard = unit.name.as_ref().map(|m| {
                // Run once: `if <loaded m> != nil: return`.
                let g = self.declare_global(&format!("<loaded {m}>"));
                self.emit_u16(Op::GetGlobal, g);
                self.emit(Op::Nil);
                self.emit(Op::Eq);
                let proceed = self.emit_jump(Op::JumpIfFalse);
                self.emit(Op::True);
                self.emit_u16(Op::SetGlobal, g);
                proceed
            });
            for stmt in &unit.module.stmts {
                self.compile_stmt(stmt);
            }
            if let Some(proceed) = guard {
                let done = self.emit_jump(Op::Jump);
                self.patch_jump(proceed);
                self.patch_jump(done);
            }
            self.emit(Op::Nil);
            self.emit(Op::Return);
            let state = core::mem::replace(&mut self.f, FnState::new(false));
            let idx = match &unit.name {
                Some(m) => self.module_inits[m],
                None => self.entry,
            };
            self.store_function(idx, state, 0);
        }

        // Pass 3: functions, methods and the view.
        for (idx, decl, module) in fn_decls {
            self.cur_module = module;
            self.f = FnState::function();
            for p in &decl.params {
                self.declare_local(&p.name);
            }
            self.compile_block_inner(&decl.body);
            self.emit(Op::Nil);
            self.emit(Op::Return);
            let state = core::mem::replace(&mut self.f, FnState::new(false));
            self.store_function(idx, state, decl.params.len() as u8);
        }
        self.cur_module = None;
        if let Some(body) = view_body {
            let idx = self.reserve_function("view");
            self.f = FnState::new(false);
            self.f.is_view = true;
            self.compile_block_inner(body);
            self.emit(Op::Nil);
            self.emit(Op::Return);
            let state = core::mem::replace(&mut self.f, FnState::new(false));
            self.store_function(idx, state, 0);
            self.view = Some(idx);
        }
    }

    /// Register a struct or enum and reserve its methods.
    fn declare_type<'m>(&mut self, decl: &'m TypeDecl, methods: &mut Vec<(u16, &'m FnDecl, Option<String>)>) {
        let name = &decl.name.name;
        let full = self.q(name);
        if matches!(name.as_str(), "Option" | "Result") || self.type_index.contains_key(&full) {
            let what = if matches!(name.as_str(), "Option" | "Result") { "a built-in type" } else { "defined more than once" };
            return self.error(format!("type `{name}` is {what}"), decl.name.span);
        }
        if self.types.len() >= ANY_VARIANT as usize {
            return self.error("too many types", decl.name.span);
        }
        let ty = self.types.len() as u16;
        let shape = match decl.kind {
            TypeKind::Struct => {
                if decl.fields.len() > u8::MAX as usize {
                    self.error("too many fields (max 255)", decl.name.span);
                }
                self.field_names.extend(decl.fields.iter().map(|f| f.name.name.clone()));
                self.structs.insert(
                    ty,
                    StructInfo {
                        fields: decl.fields.iter().map(|f| f.name.name.clone()).collect(),
                        defaults: decl.fields.iter().map(|f| f.default.clone()).collect(),
                        module: self.cur_module.clone(),
                    },
                );
                TypeShape::Struct(decl.fields.iter().map(|f| f.name.name.clone()).collect())
            }
            TypeKind::Enum => {
                let mut variants = Vec::new();
                for v in &decl.variants {
                    if v.fields.len() > u8::MAX as usize {
                        self.error("too many values in a variant (max 255)", v.name.span);
                    }
                    variants.push((v.name.name.clone(), v.fields.len().min(255) as u8));
                }
                TypeShape::Enum(variants)
            }
        };
        self.types.push(TypeDef { name: name.clone(), shape, methods: Vec::new() });
        self.type_index.insert(full.clone(), ty);
        for m in &decl.methods {
            let key = (ty, m.name.name.clone());
            if self.assoc.contains_key(&key) {
                self.error(format!("`{name}` already has a function named `{}`", m.name.name), m.name.span);
                continue;
            }
            if decl.variants.iter().any(|v| v.name.name == m.name.name) {
                self.error(format!("`{}` is already a variant of `{name}`", m.name.name), m.name.span);
                continue;
            }
            let idx = self.reserve_function(&format!("{full}.{}", m.name.name));
            self.assoc.insert(key, (idx, m.params.len().min(255) as u8));
            if m.params.first().is_some_and(|p| p.name == "self") {
                self.types[ty as usize].methods.push((m.name.name.clone(), idx));
                self.method_names.insert(m.name.name.clone());
            }
            methods.push((idx, m, self.cur_module.clone()));
        }
    }

    fn reserve_function(&mut self, name: &str) -> u16 {
        if self.functions.len() >= u16::MAX as usize {
            self.error("too many functions", Span::default());
        }
        self.functions.push(Function { name: name.to_owned(), ..Default::default() });
        (self.functions.len() - 1) as u16
    }

    fn store_function(&mut self, idx: u16, state: FnState, arity: u8) {
        let f = &mut self.functions[idx as usize];
        f.arity = arity;
        f.n_locals = state.max_slots;
        f.code = state.code;
        f.lines = if self.opts.debug_info { state.lines } else { Vec::new() };
    }

    fn declare_global(&mut self, name: &str) -> u16 {
        if let Some(&i) = self.global_index.get(name) {
            return i;
        }
        let i = self.globals.len() as u16;
        self.globals.push(name.to_owned());
        self.global_index.insert(name.to_owned(), i);
        i
    }

    fn declare_local(&mut self, name: &str) -> u16 {
        if self.f.next_slot == u16::MAX {
            self.error("too many local variables in one function", Span::default());
            return 0;
        }
        let slot = self.f.next_slot;
        self.f.next_slot += 1;
        self.f.max_slots = self.f.max_slots.max(self.f.next_slot);
        self.f.scopes.last_mut().unwrap().push((name.to_owned(), slot));
        slot
    }

    fn begin_scope(&mut self) {
        self.f.scopes.push(Vec::new());
    }

    fn end_scope(&mut self) {
        let scope = self.f.scopes.pop().expect("scope underflow");
        self.f.next_slot -= scope.len() as u16;
    }

    fn at_global_scope(&self) -> bool {
        self.f.is_main && self.f.scopes.len() == 1
    }

    // ---------------------------------------------------------------- emitters

    fn mark_line(&mut self, span: Span) {
        let line = self.source.line_of(span.start);
        let off = self.f.code.len() as u32;
        match self.f.lines.last_mut() {
            Some(last) if last.1 == line => {}
            Some(last) if last.0 == off => last.1 = line,
            _ => self.f.lines.push((off, line)),
        }
    }

    fn emit(&mut self, op: Op) {
        self.f.code.push(op as u8);
    }

    fn emit_u8(&mut self, op: Op, v: u8) {
        self.f.code.extend_from_slice(&[op as u8, v]);
    }

    fn emit_u16(&mut self, op: Op, v: u16) {
        self.f.code.push(op as u8);
        self.f.code.extend_from_slice(&v.to_le_bytes());
    }

    /// Emit a jump with a placeholder target; returns the operand offset.
    fn emit_jump(&mut self, op: Op) -> usize {
        self.f.code.push(op as u8);
        let at = self.f.code.len();
        self.f.code.extend_from_slice(&u32::MAX.to_le_bytes());
        at
    }

    fn patch_jump(&mut self, operand_at: usize) {
        let target = self.f.code.len() as u32;
        self.f.code[operand_at..operand_at + 4].copy_from_slice(&target.to_le_bytes());
    }

    fn emit_jump_to(&mut self, op: Op, target: usize) {
        self.f.code.push(op as u8);
        self.f.code.extend_from_slice(&(target as u32).to_le_bytes());
    }

    fn emit_const(&mut self, c: Constant, span: Span) {
        let key = match &c {
            Constant::Int(v) => ConstKey::Int(*v),
            Constant::Float(v) => ConstKey::Float(v.to_bits()),
            Constant::Str(s) => ConstKey::Str(s.clone()),
        };
        let idx = match self.const_index.get(&key) {
            Some(&i) => i,
            None => {
                if self.constants.len() >= u16::MAX as usize {
                    self.error("too many constants in program (max 65535)", span);
                    return;
                }
                let i = self.constants.len() as u16;
                self.constants.push(c);
                self.const_index.insert(key, i);
                i
            }
        };
        self.emit_u16(Op::Const, idx);
    }

    // -------------------------------------------------------------- statements

    fn compile_block(&mut self, block: &Block) {
        self.begin_scope();
        self.compile_block_inner(block);
        self.end_scope();
    }

    fn compile_block_inner(&mut self, block: &Block) {
        for stmt in &block.stmts {
            self.compile_stmt(stmt);
        }
    }

    fn compile_stmt(&mut self, stmt: &Stmt) {
        self.mark_line(stmt.span);
        match &stmt.kind {
            StmtKind::Let { name, value } => {
                self.compile_expr(value);
                if self.at_global_scope() {
                    let g = self.declare_global(&self.q(&name.name));
                    self.emit_u16(Op::SetGlobal, g);
                } else {
                    let slot = self.declare_local(&name.name);
                    self.emit_u16(Op::SetLocal, slot);
                }
            }
            StmtKind::Assign { target, op, value } => self.compile_assign(target, *op, value),
            StmtKind::Expr(e) => {
                self.compile_expr(e);
                self.emit(Op::Pop);
            }
            StmtKind::If { branches, otherwise } => {
                let mut exits = Vec::new();
                for (cond, body) in branches {
                    self.begin_scope();
                    let mut next = Vec::new();
                    match cond {
                        Cond::Expr(e) => {
                            self.compile_expr(e);
                            next.push(self.emit_jump(Op::JumpIfFalse));
                        }
                        Cond::Let(pattern, value) => {
                            self.compile_expr(value);
                            let slot = self.declare_local("<let>");
                            self.emit_u16(Op::SetLocal, slot);
                            self.compile_pattern(pattern, slot, &mut next);
                        }
                    }
                    self.compile_block(body);
                    exits.push(self.emit_jump(Op::Jump));
                    self.end_scope();
                    for j in next {
                        self.patch_jump(j);
                    }
                }
                if let Some(body) = otherwise {
                    self.compile_block(body);
                }
                for e in exits {
                    self.patch_jump(e);
                }
            }
            StmtKind::While { cond, body } => {
                let start = self.f.code.len();
                self.compile_expr(cond);
                let exit = self.emit_jump(Op::JumpIfFalse);
                self.f.loops.push(LoopCtx { continue_target: start, breaks: Vec::new() });
                self.compile_block(body);
                self.emit_jump_to(Op::Jump, start);
                self.patch_jump(exit);
                let ctx = self.f.loops.pop().unwrap();
                for b in ctx.breaks {
                    self.patch_jump(b);
                }
            }
            StmtKind::For { var, iterable, body } => {
                self.compile_expr(iterable);
                self.emit(Op::IterInit);
                self.begin_scope();
                let iter_slot = self.declare_local("<iter>");
                self.emit_u16(Op::SetLocal, iter_slot);
                let var_slot = self.declare_local(&var.name);
                let head = self.f.code.len();
                self.f.code.push(Op::IterNext as u8);
                self.f.code.extend_from_slice(&iter_slot.to_le_bytes());
                let exit = self.f.code.len();
                self.f.code.extend_from_slice(&u32::MAX.to_le_bytes());
                self.emit_u16(Op::SetLocal, var_slot);
                self.f.loops.push(LoopCtx { continue_target: head, breaks: Vec::new() });
                self.compile_block(body);
                self.emit_jump_to(Op::Jump, head);
                self.patch_jump(exit);
                let ctx = self.f.loops.pop().unwrap();
                for b in ctx.breaks {
                    self.patch_jump(b);
                }
                self.end_scope();
            }
            StmtKind::Return(value) => {
                match value {
                    Some(v) => self.compile_expr(v),
                    None => self.emit(Op::Nil),
                }
                self.emit(Op::Return);
            }
            StmtKind::Break => match self.f.loops.is_empty() {
                true => self.error("`break` outside of a loop", stmt.span),
                false => {
                    let j = self.emit_jump(Op::Jump);
                    self.f.loops.last_mut().unwrap().breaks.push(j);
                }
            },
            StmtKind::Continue => match self.f.loops.last() {
                None => self.error("`continue` outside of a loop", stmt.span),
                Some(ctx) => {
                    let target = ctx.continue_target;
                    self.emit_jump_to(Op::Jump, target);
                }
            },
            // Nested functions are closures stored in a local.
            StmtKind::Fn(decl) if !self.at_global_scope() => {
                let slot = self.declare_local(&decl.name.name);
                self.compile_closure(&decl.name.name, Some(&decl.name.name), &decl.params, ClosureBody::Block(&decl.body));
                self.emit_u16(Op::SetLocal, slot);
            }
            // Declared and compiled in `compile_module`.
            StmtKind::Fn(_) | StmtKind::View(_) | StmtKind::Struct(_) => {}
            // Run the module's top-level code (once).
            StmtKind::Import(import) => {
                if let Some(&init) = self.module_inits.get(&import.module.name) {
                    self.emit_u16(Op::Func, init);
                    self.emit_u8(Op::Call, 0);
                    self.emit(Op::Pop);
                }
            }
            StmtKind::Match { subject, arms } => self.compile_match(subject, arms, stmt.span),
            StmtKind::Ui(el) => self.compile_ui(el),
            StmtKind::Attrs(attrs) => {
                for a in attrs {
                    self.compile_attr(a, None);
                }
            }
        }
    }

    fn compile_assign(&mut self, target: &Expr, op: Option<BinaryOp>, value: &Expr) {
        match &target.kind {
            ExprKind::Ident(name) => {
                let Some(var) = self.resolve_var(name, target.span) else { return };
                if let Some(op) = op {
                    self.emit_get(var);
                    self.compile_expr(value);
                    self.emit_binary(op);
                } else {
                    self.compile_expr(value);
                }
                self.emit_set(var, target.span);
            }
            ExprKind::Index { target: container, index } => {
                self.compile_expr(container);
                self.compile_expr(index);
                if let Some(op) = op {
                    self.emit(Op::Dup2);
                    self.emit(Op::Index);
                    self.compile_expr(value);
                    self.emit_binary(op);
                } else {
                    self.compile_expr(value);
                }
                self.emit(Op::SetIndex);
            }
            ExprKind::Field { target: object, name } => {
                if let Some(global) = self.module_global(object, name) {
                    let Some(g) = global else { return };
                    if let Some(op) = op {
                        self.emit_u16(Op::GetGlobal, g);
                        self.compile_expr(value);
                        self.emit_binary(op);
                    } else {
                        self.compile_expr(value);
                    }
                    return self.emit_u16(Op::SetGlobal, g);
                }
                if self.static_type(object).is_some() {
                    return self.error("cannot assign to a type's function or variant", target.span);
                }
                self.check_field_name(name);
                let c = self.name_constant(&name.name, name.span);
                self.compile_expr(object);
                if let Some(op) = op {
                    self.emit(Op::Dup);
                    self.emit_u16(Op::GetField, c);
                    self.compile_expr(value);
                    self.emit_binary(op);
                } else {
                    self.compile_expr(value);
                }
                self.emit_u16(Op::SetField, c);
            }
            _ => self.error("invalid assignment target", target.span),
        }
    }

    // ------------------------------------------------------------------ match

    fn compile_match(&mut self, subject: &Expr, arms: &[MatchArm], span: Span) {
        self.compile_expr(subject);
        self.begin_scope();
        let slot = self.declare_local("<match>");
        self.emit_u16(Op::SetLocal, slot);
        let mut exits = Vec::new();
        for arm in arms {
            self.begin_scope();
            let mut next = Vec::new();
            self.compile_pattern(&arm.pattern, slot, &mut next);
            if let Some(guard) = &arm.guard {
                self.compile_expr(guard);
                next.push(self.emit_jump(Op::JumpIfFalse));
            }
            self.compile_block(&arm.body);
            exits.push(self.emit_jump(Op::Jump));
            self.end_scope();
            for j in next {
                self.patch_jump(j);
            }
        }
        // Even a match covering every variant of an enum can receive a value
        // of another type: only an arm that matches anything makes the
        // fallback unnecessary.
        if !self.check_match_coverage(arms, span) {
            // No arm matched: a runtime error naming the value.
            let panic = builtin_index("panic").expect("panic is a builtin");
            self.emit_u8(Op::Builtin, panic);
            self.emit_const(Constant::Str("no `match` arm matches the value ".into()), span);
            self.emit_u16(Op::GetLocal, slot);
            self.emit_u16(Op::Concat, 2);
            self.emit_u8(Op::Call, 1);
            self.emit(Op::Pop);
        }
        for j in exits {
            self.patch_jump(j);
        }
        self.end_scope();
    }

    /// Whether some arm matches every value. Matches over the variants of
    /// one enum must cover every variant (an error otherwise); other matches
    /// without a catch-all arm get a warning. Either way, a value that
    /// matches no arm stops the program at runtime.
    fn check_match_coverage(&mut self, arms: &[MatchArm], span: Span) -> bool {
        let unguarded = arms.iter().filter(|a| a.guard.is_none());
        if unguarded.clone().any(|a| self.irrefutable(&a.pattern)) {
            return true;
        }
        let mut enum_ty = None;
        let mut covered = HashSet::new();
        let mut bools = HashSet::new();
        let mut only_variants = true;
        let mut only_bools = true;
        for arm in arms {
            for alt in alternatives(&arm.pattern) {
                if let PatternKind::Literal(Expr { kind: ExprKind::Bool(b), .. }) = alt.kind {
                    only_variants = false;
                    if arm.guard.is_none() {
                        bools.insert(b);
                    }
                    continue;
                }
                only_bools = false;
                match self.pattern_variant(alt) {
                    Some((ty, tag, args)) if *enum_ty.get_or_insert(ty) == ty => {
                        if arm.guard.is_none() && args.is_none_or(|args| args.iter().all(|p| self.irrefutable(p))) {
                            covered.insert(tag);
                        }
                    }
                    _ => only_variants = false,
                }
            }
        }
        if only_bools {
            if bools.len() < 2 {
                self.diagnostics.push(Diagnostic::warning("`match` does not cover both `true` and `false`", span));
            }
            return false;
        }
        match enum_ty {
            Some(ty) if only_variants => {
                let missing: Vec<String> = self.types[ty as usize]
                    .variants()
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !covered.contains(&(*i as u16)))
                    .map(|(_, (name, arity))| if *arity == 0 { format!("`{name}`") } else { format!("`{name}(..)`") })
                    .collect();
                if !missing.is_empty() {
                    self.diagnostics.push(
                        Diagnostic::error(format!("`match` does not cover {}", missing.join(", ")), span)
                            .with_label("not every case is handled")
                            .with_note("add an arm for each missing case, or a final `_ => ...` arm"),
                    );
                }
                false
            }
            _ => {
                self.diagnostics.push(
                    Diagnostic::warning("`match` has no catch-all arm", span)
                        .with_note("a value that matches no arm stops the program: add a final `_ => ...` arm"),
                );
                false
            }
        }
    }

    /// Patterns that match every value: `_`, a binding, or an or-pattern
    /// containing one.
    fn irrefutable(&self, pattern: &Pattern) -> bool {
        match &pattern.kind {
            PatternKind::Wildcard | PatternKind::Bind(_) => true,
            PatternKind::Ctor { path, args: None } if path.len() == 1 => self.bare_variant(&path[0].name).is_none(),
            PatternKind::Or(alts) => alts.iter().any(|p| self.irrefutable(p)),
            _ => false,
        }
    }

    /// The enum variant a constructor pattern names, if it is one.
    fn pattern_variant<'p>(&self, pattern: &'p Pattern) -> Option<(u16, u16, Option<&'p [Pattern]>)> {
        let PatternKind::Ctor { path, args } = &pattern.kind else { return None };
        let (ty, tag) = match path.as_slice() {
            [name] => self.bare_variant(&name.name)?,
            _ => {
                let (t, tag) = self.resolve_pattern_path(path).ok()?;
                (tag != ANY_VARIANT).then_some((t, tag))?
            }
        };
        Some((ty, tag, args.as_deref()))
    }

    /// A variant written without its type: `Some`, `None`, `Ok`, `Err`, or a
    /// variant name that only one enum has.
    fn bare_variant(&self, name: &str) -> Option<(u16, u16)> {
        match name {
            "Some" => return Some((OPTION_TYPE, SOME)),
            "None" => return Some((OPTION_TYPE, NONE)),
            "Ok" => return Some((RESULT_TYPE, OK)),
            "Err" => return Some((RESULT_TYPE, ERR)),
            _ => {}
        }
        let mut found = None;
        for (t, def) in self.types.iter().enumerate().skip(2) {
            if let Some(tag) = def.variants().iter().position(|(v, _)| v == name) {
                if found.is_some() {
                    return None;
                }
                found = Some((t as u16, tag as u16));
            }
        }
        found
    }

    /// Test the value in `slot` against `pattern`, binding names as locals.
    /// Jumps whose target is "no match" are added to `fail`.
    fn compile_pattern(&mut self, pattern: &Pattern, slot: u16, fail: &mut Vec<usize>) {
        match &pattern.kind {
            PatternKind::Wildcard => {}
            PatternKind::Bind(name) => self.bind_pattern(name, slot),
            PatternKind::Literal(value) => {
                self.emit_u16(Op::GetLocal, slot);
                self.compile_expr(value);
                self.emit(Op::Eq);
                fail.push(self.emit_jump(Op::JumpIfFalse));
            }
            PatternKind::Range(lo, hi) => {
                for bound in [lo, hi] {
                    if !matches!(bound.kind, ExprKind::Int(_) | ExprKind::Float(_)) {
                        self.error("range patterns need numbers", bound.span);
                    }
                }
                self.emit_u16(Op::GetLocal, slot);
                self.compile_expr(lo);
                self.compile_expr(hi);
                self.emit(Op::InRange);
                fail.push(self.emit_jump(Op::JumpIfFalse));
            }
            PatternKind::Or(alts) => {
                if let Some(binding) = alts.iter().find_map(|p| first_binding(p, self)) {
                    self.diagnostics.push(
                        Diagnostic::error("alternatives in a `|` pattern cannot bind names", binding)
                            .with_note("use a separate arm for each alternative that needs the value"),
                    );
                }
                let mut matched = Vec::new();
                for (i, alt) in alts.iter().enumerate() {
                    let mut alt_fail = Vec::new();
                    self.compile_pattern(alt, slot, &mut alt_fail);
                    if i + 1 == alts.len() {
                        fail.extend(alt_fail);
                    } else {
                        matched.push(self.emit_jump(Op::Jump));
                        for j in alt_fail {
                            self.patch_jump(j);
                        }
                    }
                }
                for j in matched {
                    self.patch_jump(j);
                }
            }
            PatternKind::Ctor { path, args } => self.compile_ctor_pattern(pattern, path, args.as_deref(), slot, fail),
        }
    }

    fn bind_pattern(&mut self, name: &Ident, slot: u16) {
        let local = self.declare_local(&name.name);
        self.emit_u16(Op::GetLocal, slot);
        self.emit_u16(Op::SetLocal, local);
    }

    fn compile_ctor_pattern(&mut self, pattern: &Pattern, path: &[Ident], args: Option<&[Pattern]>, slot: u16, fail: &mut Vec<usize>) {
        let span = pattern.span;
        let (ty, tag) = match (path, args) {
            ([name], None) if self.bare_variant(&name.name).is_none() => {
                if self.find_type(&name.name).is_some() {
                    return self.error(format!("`{}` is a type: write `{}(..)` to match its fields", name.name, name.name), span);
                }
                if name.name.starts_with(char::is_uppercase) && self.variant_owners(&name.name).len() > 1 {
                    return self.ambiguous_variant(&name.name, span);
                }
                return self.bind_pattern(name, slot);
            }
            ([name], _) => match self.bare_variant(&name.name) {
                Some(v) => v,
                None => match self.find_type(&name.name) {
                    Some(t) if self.structs.contains_key(&t) => (t, ANY_VARIANT),
                    _ if self.variant_owners(&name.name).len() > 1 => return self.ambiguous_variant(&name.name, span),
                    _ => return self.unknown_variant(&name.name, None, span),
                },
            },
            _ => match self.resolve_pattern_path(path) {
                Ok(v) => v,
                Err(d) => return self.diagnostics.push(d),
            },
        };
        let def = &self.types[ty as usize];
        let (label, arity) = if tag == ANY_VARIANT {
            (def.name.clone(), def.fields().len())
        } else {
            (def.variants()[tag as usize].0.clone(), def.variants()[tag as usize].1 as usize)
        };
        match args {
            None if arity > 0 => {
                let blanks = vec!["_"; arity].join(", ");
                return self.diagnostics.push(
                    Diagnostic::error(format!("`{label}` has {arity} value(s)"), span).with_note(format!("write `{label}({blanks})`")),
                );
            }
            Some(args) if args.len() != arity => {
                let what = if tag == ANY_VARIANT { "field(s)" } else { "value(s)" };
                return self.error(format!("`{label}` has {arity} {what}, but the pattern has {}", args.len()), span);
            }
            _ => {}
        }
        self.emit_u16(Op::GetLocal, slot);
        self.f.code.push(Op::IsType as u8);
        self.f.code.extend_from_slice(&ty.to_le_bytes());
        self.f.code.extend_from_slice(&tag.to_le_bytes());
        fail.push(self.emit_jump(Op::JumpIfFalse));
        for (i, sub) in args.unwrap_or_default().iter().enumerate() {
            if matches!(sub.kind, PatternKind::Wildcard) {
                continue;
            }
            let item = self.declare_local("<item>");
            self.emit_u16(Op::GetLocal, slot);
            self.emit_u8(Op::GetItem, i as u8);
            self.emit_u16(Op::SetLocal, item);
            self.compile_pattern(sub, item, fail);
        }
    }

    /// `Type.Variant`, `module.Type`, `module.Type.Variant` or `module.Struct`.
    fn resolve_pattern_path(&self, path: &[Ident]) -> Result<(u16, u16), Diagnostic> {
        let first = &path[0];
        let module = self.module_alias(&Expr { kind: ExprKind::Ident(first.name.clone()), span: first.span });
        let (ty_ident, rest) = match &module {
            Some(m) => {
                let ty = path.get(1).ok_or_else(|| Diagnostic::error("expected a type after the module name", first.span))?;
                let full = format!("{m}.{}", ty.name);
                let t = *self
                    .type_index
                    .get(&full)
                    .ok_or_else(|| Diagnostic::error(format!("module `{m}` has no type `{}`", ty.name), ty.span))?;
                ((t, ty), &path[2..])
            }
            None => {
                let t = self
                    .find_type(&first.name)
                    .ok_or_else(|| Diagnostic::error(format!("cannot find type `{}`", first.name), first.span))?;
                ((t, first), &path[1..])
            }
        };
        let (t, ty) = ty_ident;
        match rest {
            [] if self.structs.contains_key(&t) => Ok((t, ANY_VARIANT)),
            [] => Err(Diagnostic::error(format!("`{}` is an enum: name one of its variants", ty.name), ty.span)),
            [variant] => match self.types[t as usize].variants().iter().position(|(v, _)| *v == variant.name) {
                Some(tag) => Ok((t, tag as u16)),
                None => {
                    let mut d = Diagnostic::error(format!("`{}` has no variant `{}`", ty.name, variant.name), variant.span);
                    let names = self.types[t as usize].variants().iter().map(|(v, _)| v.as_str());
                    if let Some(similar) = best_match(&variant.name, names) {
                        d = d.with_note(format!("a variant with a similar name exists: `{similar}`"));
                    }
                    Err(d)
                }
            },
            _ => Err(Diagnostic::error("invalid pattern path", path.last().unwrap().span)),
        }
    }

    /// Enums that have a variant named `name`.
    fn variant_owners(&self, name: &str) -> Vec<&str> {
        self.types.iter().filter(|t| t.variants().iter().any(|(v, _)| v == name)).map(|t| t.name.as_str()).collect()
    }

    fn ambiguous_variant(&mut self, name: &str, span: Span) {
        let owners = self.variant_owners(name).iter().map(|t| format!("`{t}.{name}`")).collect::<Vec<_>>().join(" or ");
        self.diagnostics
            .push(Diagnostic::error(format!("`{name}` is a variant of several enums"), span).with_note(format!("write {owners}")));
    }

    fn unknown_variant(&mut self, name: &str, ty: Option<u16>, span: Span) {
        let mut d = match ty {
            Some(t) => Diagnostic::error(format!("`{}` has no variant `{name}`", self.types[t as usize].name), span),
            None => Diagnostic::error(format!("cannot find variant or struct `{name}`"), span),
        };
        let candidates: Vec<&str> = match ty {
            Some(t) => self.types[t as usize].variants().iter().map(|(v, _)| v.as_str()).collect(),
            None => self.types.iter().flat_map(|t| t.variants().iter().map(|(v, _)| v.as_str())).collect(),
        };
        if let Some(similar) = best_match(name, candidates) {
            d = d.with_note(format!("a variant with a similar name exists: `{similar}`"));
        }
        self.diagnostics.push(d.with_label("unknown variant"));
    }

    fn compile_ui(&mut self, el: &UiElement) {
        let kind = el.kind;
        if self.f.is_main {
            self.diagnostics.push(
                Diagnostic::error(format!("`{}` can only be used inside a `view`", kind.name()), el.kind_span)
                    .with_note("UI elements belong in the `view` block or in functions called from it"),
            );
        }
        if let Some(&parent) = self.f.ui.last() {
            if !parent.is_container() {
                self.diagnostics.push(
                    Diagnostic::error(format!("`{}` cannot contain child elements", parent.name()), el.kind_span)
                        .with_note("only containers (`window`, `column`, `row`, `card`, `grid`, `scroll`, `group`, `split`, `accordion`) can have children"),
                );
            }
        }
        match &el.arg {
            Some(arg) => {
                if let (Arg::Required(ty) | Arg::Optional(ty), Some(raw)) = (kind.arg(), literal(arg)) {
                    self.check_literal_arg(kind, ty, raw, arg.span);
                }
                self.compile_expr(arg);
            }
            None => self.emit(Op::Nil),
        }
        self.emit_u8(Op::UiBegin, kind as u8);
        for a in &el.attrs {
            self.compile_attr(a, Some(kind));
        }
        match (&el.action, kind.event()) {
            (Some(handler), event) => {
                self.compile_handler(handler, event);
                self.emit(Op::UiOn);
            }
            (None, Event::Change) => {
                if let Some(arg) = &el.arg {
                    if self.compile_binding(kind, arg) {
                        self.emit(Op::UiOn);
                    }
                }
            }
            (None, _) => {}
        }
        if let Some(block) = &el.children {
            self.f.ui.push(kind);
            self.compile_block(block);
            self.f.ui.pop();
        }
        self.emit(Op::UiClose);
    }

    fn check_literal_arg(&mut self, kind: UiKind, ty: ArgType, raw: RawValue<'_>, span: Span) {
        let ok = match ty {
            ArgType::Text => true,
            ArgType::Str => matches!(raw, RawValue::Str(_)),
            ArgType::Bool => matches!(raw, RawValue::Bool(_)),
            ArgType::Number => matches!(raw, RawValue::Int(_) | RawValue::Float(_)),
        };
        if !ok {
            let found = match raw {
                RawValue::Int(_) => "int",
                RawValue::Float(_) => "float",
                RawValue::Bool(_) => "bool",
                RawValue::Str(_) => "string",
                RawValue::Other(t) => t,
            };
            self.diagnostics.push(
                Diagnostic::error(format!("`{}` expects {}, found {found}", kind.name(), ty.describe()), span).with_label("wrong type"),
            );
        }
    }

    /// `name: value` on the element `owner`, or on the innermost enclosing
    /// element when `owner` is `None` (attribute lines).
    fn compile_attr(&mut self, a: &AttrInit, owner: Option<UiKind>) {
        let name = a.name.name.as_str();
        let Some(attr) = Attr::from_name(name) else {
            let mut d = Diagnostic::error(format!("unknown attribute `{name}`"), a.name.span).with_label("unknown attribute");
            if UiKind::from_name(name).is_some() {
                d = d.with_note(format!("`{name}` is an element: write `{name} ...` without the `:`"));
            } else if let Some(similar) = Attr::suggest(name) {
                d = d.with_note(format!("an attribute with a similar name exists: `{similar}`"));
            }
            return self.diagnostics.push(d);
        };
        let owner = owner.or(self.f.ui.last().copied());
        match owner {
            Some(kind) if !attr.applies_to(kind) => {
                let valid = kind.attrs().map(Attr::name).collect::<Vec<_>>().join(", ");
                let note = if valid.is_empty() {
                    format!("`{}` has no attributes", kind.name())
                } else {
                    format!("`{}` accepts: {valid}", kind.name())
                };
                return self.diagnostics.push(
                    Diagnostic::error(attr.not_applicable(kind), a.name.span)
                        .with_label("not an attribute of this element")
                        .with_note(note),
                );
            }
            None if self.f.is_main || self.f.is_view => {
                return self.diagnostics.push(
                    Diagnostic::error(format!("attribute `{name}` outside of an element"), a.name.span)
                        .with_note("attributes are written on an element line or in its indented block"),
                );
            }
            _ => {}
        }
        if let Some(raw) = literal(&a.value) {
            // Inside a component function the element is only known at
            // runtime: check against the first element that accepts it.
            let kind = owner.or_else(|| UiKind::ALL.into_iter().find(|k| attr.applies_to(*k)));
            if let Some(kind) = kind {
                if let Err(msg) = attr.coerce(kind, raw) {
                    self.diagnostics.push(Diagnostic::error(msg, a.value.span).with_label("invalid value"));
                }
            }
        }
        self.compile_expr(&a.value);
        self.emit_u8(Op::UiAttr, attr as u8);
    }

    /// Push the callable for `-> handler`. Change handlers receive the new
    /// value after their bound arguments.
    fn compile_handler(&mut self, handler: &Expr, event: Event) {
        let implicit = u8::from(event == Event::Change);
        match &handler.kind {
            // `-> f(a, b)` binds the arguments now and calls later.
            ExprKind::Call { callee, args, named } if named.is_empty() && !self.is_constructor(callee) => {
                self.compile_call_parts(callee, args, handler.span, implicit);
                self.emit_u8(Op::Bind, args.len() as u8);
            }
            // `-> utils.save(a)` / `-> Type.f(a)`: a known function.
            ExprKind::MethodCall { receiver, method, args, named }
                if named.is_empty() && self.static_callee(receiver, method).is_some() =>
            {
                let (func, arity) = self.static_callee(receiver, method).expect("checked");
                if args.len() + implicit as usize != arity as usize {
                    let d = Diagnostic::error(format!("`{}` takes {arity} argument(s)", method.name), handler.span)
                        .with_note("change handlers receive the new value as their last argument");
                    self.diagnostics.push(d);
                }
                self.emit_u16(Op::Func, func);
                for a in args {
                    self.compile_expr(a);
                }
                self.emit_u8(Op::Bind, args.len() as u8);
            }
            // `-> item.toggle()` binds the receiver and arguments now.
            ExprKind::MethodCall { receiver, method, args, named }
                if named.is_empty() && self.static_type(receiver).is_none() && self.module_alias(receiver).is_none() =>
            {
                self.compile_method_handler(receiver, method, args, implicit);
            }
            // `-> f` passes just the implicit arguments.
            _ => {
                self.check_arity(handler, 0, implicit, handler.span);
                self.compile_expr(handler);
            }
        }
    }

    /// Two-way binding of `input name` / `checkbox items[i]` without a
    /// handler: pushes a setter that stores the new value. Returns `false`
    /// (after reporting) when the argument is not bindable.
    fn compile_binding(&mut self, kind: UiKind, arg: &Expr) -> bool {
        let name = kind.name();
        match &arg.kind {
            ExprKind::Ident(var) if self.lookup_local(var).is_some() => {
                self.diagnostics.push(
                    Diagnostic::error(format!("`{name}` cannot be bound to the local variable `{var}`"), arg.span)
                        .with_label("local variable")
                        .with_note("locals are recreated on every render: bind a global, or add a handler such as `-> set_value()`"),
                );
                false
            }
            ExprKind::Ident(var) => match self.find_global(var) {
                Some(g) => {
                    let setter = self.global_setter(g);
                    self.emit_u16(Op::Func, setter);
                    true
                }
                // Unknown names were already reported while compiling the argument.
                None => {
                    if self.find_fn(var).is_some() || self.find_builtin(var).is_some() {
                        self.diagnostics.push(Diagnostic::error(format!("`{name}` cannot be bound to the function `{var}`"), arg.span));
                    }
                    false
                }
            },
            ExprKind::Index { target, index } => {
                let setter = self.index_setter();
                self.emit_u16(Op::Func, setter);
                self.compile_expr(target);
                self.compile_expr(index);
                self.emit_u8(Op::Bind, 2);
                true
            }
            ExprKind::Field { target, name } if self.module_alias(target).is_some() => match self.module_global(target, name) {
                Some(Some(g)) => {
                    let setter = self.global_setter(g);
                    self.emit_u16(Op::Func, setter);
                    true
                }
                _ => false,
            },
            ExprKind::Field { target, name } if self.static_type(target).is_none() => {
                let setter = self.field_setter(&name.name, name.span);
                self.emit_u16(Op::Func, setter);
                self.compile_expr(target);
                self.emit_u8(Op::Bind, 1);
                true
            }
            _ => {
                self.diagnostics.push(
                    Diagnostic::error(format!("`{name}` needs a variable to bind to, or an event handler"), arg.span)
                        .with_label("not assignable")
                        .with_note(format!("write `{name} value` where `value` is a global variable, or `{name} expr -> on_change()`")),
                );
                false
            }
        }
    }

    fn global_setter(&mut self, g: u16) -> u16 {
        if let Some(&f) = self.global_setters.get(&g) {
            return f;
        }
        let name = format!("<set {}>", self.globals[g as usize]);
        let mut code = vec![Op::GetLocal as u8, 0, 0, Op::SetGlobal as u8];
        code.extend_from_slice(&g.to_le_bytes());
        code.extend_from_slice(&[Op::Nil as u8, Op::Return as u8]);
        let f = self.synthetic_function(&name, 1, code);
        self.global_setters.insert(g, f);
        f
    }

    fn index_setter(&mut self) -> u16 {
        if let Some(f) = self.index_setter {
            return f;
        }
        let code = vec![
            Op::GetLocal as u8,
            0,
            0,
            Op::GetLocal as u8,
            1,
            0,
            Op::GetLocal as u8,
            2,
            0,
            Op::SetIndex as u8,
            Op::Nil as u8,
            Op::Return as u8,
        ];
        let f = self.synthetic_function("<set index>", 3, code);
        self.index_setter = Some(f);
        f
    }

    fn field_setter(&mut self, field: &str, span: Span) -> u16 {
        if let Some(&f) = self.field_setters.get(field) {
            return f;
        }
        let c = self.name_constant(field, span);
        let mut code = vec![Op::GetLocal as u8, 0, 0, Op::GetLocal as u8, 1, 0, Op::SetField as u8];
        code.extend_from_slice(&c.to_le_bytes());
        code.extend_from_slice(&[Op::Nil as u8, Op::Return as u8]);
        let f = self.synthetic_function(&format!("<set .{field}>"), 2, code);
        self.field_setters.insert(field.to_owned(), f);
        f
    }

    /// `-> receiver.method(args)`: a closure over the receiver and argument
    /// values that performs the method call when the event fires.
    fn compile_method_handler(&mut self, receiver: &Expr, method: &Ident, args: &[Expr], implicit: u8) {
        if args.len() + 2 > u8::MAX as usize {
            return self.error("too many arguments", method.span);
        }
        self.push_method_fallback(method, args.len() + 1 + implicit as usize);
        self.compile_expr(receiver);
        for a in args {
            self.compile_expr(a);
        }
        let c = self.name_constant(&method.name, method.span);
        let n = args.len() as u8 + 2;
        let mut code = Vec::new();
        for i in 0..n {
            code.extend_from_slice(&[Op::GetCapture as u8, i]);
        }
        if implicit > 0 {
            code.extend_from_slice(&[Op::GetLocal as u8, 0, 0]);
        }
        code.push(Op::Method as u8);
        code.extend_from_slice(&c.to_le_bytes());
        code.extend_from_slice(&[args.len() as u8 + implicit, Op::Return as u8]);
        let f = self.synthetic_function(&format!("<handler .{}>", method.name), implicit, code);
        self.f.code.push(Op::Closure as u8);
        self.f.code.extend_from_slice(&f.to_le_bytes());
        self.f.code.push(n);
    }

    fn synthetic_function(&mut self, name: &str, arity: u8, code: Vec<u8>) -> u16 {
        let idx = self.reserve_function(name);
        let f = &mut self.functions[idx as usize];
        f.arity = arity;
        f.n_locals = arity as u16;
        f.code = code;
        idx
    }

    // ------------------------------------------------------------- expressions

    fn compile_expr(&mut self, expr: &Expr) {
        if self.opts.optimize {
            if let Some(c) = fold(expr) {
                return self.emit_folded(c, expr.span);
            }
        }
        match &expr.kind {
            ExprKind::Int(v) => self.emit_const(Constant::Int(*v), expr.span),
            ExprKind::Float(v) => self.emit_const(Constant::Float(*v), expr.span),
            ExprKind::Str(s) => self.emit_const(Constant::Str(s.clone()), expr.span),
            ExprKind::Bool(true) => self.emit(Op::True),
            ExprKind::Bool(false) => self.emit(Op::False),
            ExprKind::Nil => self.emit(Op::Nil),
            ExprKind::Interp(parts) => {
                for part in parts {
                    match part {
                        InterpPart::Text(t) => self.emit_const(Constant::Str(t.clone()), expr.span),
                        InterpPart::Expr(e) => self.compile_expr(e),
                    }
                }
                if parts.len() > u16::MAX as usize {
                    return self.error("string interpolation has too many parts", expr.span);
                }
                self.emit_u16(Op::Concat, parts.len() as u16);
            }
            ExprKind::Ident(name) => {
                if let Some(var) = self.resolve_var(name, expr.span) {
                    self.emit_value(var, name, expr.span);
                }
            }
            ExprKind::Map(entries) => {
                if entries.len() > u16::MAX as usize {
                    return self.error("map literal has too many entries", expr.span);
                }
                for (k, v) in entries {
                    self.compile_expr(k);
                    self.compile_expr(v);
                }
                self.emit_u16(Op::Map, entries.len() as u16);
            }
            ExprKind::Field { target, name } => self.compile_field(target, name, expr.span),
            ExprKind::MethodCall { receiver, method, args, named } => self.compile_method_call(receiver, method, args, named, expr.span),
            ExprKind::Lambda { params, body } => {
                self.compile_closure("<lambda>", None, params, ClosureBody::Expr(body));
            }
            ExprKind::Try(inner) => {
                if !self.f.is_fn {
                    let where_ = if self.f.is_view { "the `view`" } else { "top-level code" };
                    self.diagnostics.push(
                        Diagnostic::error(format!("`?` cannot be used in {where_}"), expr.span)
                            .with_label("would return from the program")
                            .with_note("`?` returns `None`/`Err` from the enclosing function; use `match`, `unwrap` or `unwrap_or` here"),
                    );
                }
                self.compile_expr(inner);
                self.emit(Op::Try);
            }
            ExprKind::List(items) => {
                if items.len() > u16::MAX as usize {
                    return self.error("list literal has too many elements", expr.span);
                }
                for item in items {
                    self.compile_expr(item);
                }
                self.emit_u16(Op::List, items.len() as u16);
            }
            ExprKind::Unary { op, expr: inner } => {
                self.compile_expr(inner);
                self.emit(match op {
                    UnaryOp::Neg => Op::Neg,
                    UnaryOp::Not => Op::Not,
                });
            }
            ExprKind::Binary { op, lhs, rhs } => {
                self.compile_expr(lhs);
                self.compile_expr(rhs);
                self.emit_binary(*op);
            }
            ExprKind::Logical { op, lhs, rhs } => {
                self.compile_expr(lhs);
                let jump = self.emit_jump(match op {
                    LogicalOp::And => Op::JumpIfFalseKeep,
                    LogicalOp::Or => Op::JumpIfTrueKeep,
                });
                self.compile_expr(rhs);
                self.patch_jump(jump);
            }
            ExprKind::Call { callee, args, named } => {
                if self.compile_constructor(callee, args, named, expr.span) {
                    return;
                }
                if let Some((n, _)) = named.first() {
                    return self.diagnostics.push(
                        Diagnostic::error("named arguments are only used to create structs", n.span)
                            .with_note("pass the arguments in order: `f(a, b)`"),
                    );
                }
                self.compile_call_parts(callee, args, expr.span, 0);
                self.emit_u8(Op::Call, args.len() as u8);
            }
            ExprKind::Index { target, index } => {
                self.compile_expr(target);
                self.compile_expr(index);
                self.emit(Op::Index);
            }
        }
    }

    /// Push callee and arguments, checking arity statically when possible.
    /// `implicit` extra arguments are supplied later by the caller (the new
    /// value passed to change handlers).
    fn compile_call_parts(&mut self, callee: &Expr, args: &[Expr], span: Span, implicit: u8) {
        if args.len() > u8::MAX as usize - implicit as usize {
            self.error("too many arguments (max 255)", span);
        }
        self.check_arity(callee, args.len(), implicit, span);
        self.compile_expr(callee);
        for arg in args {
            self.compile_expr(arg);
        }
    }

    fn check_arity(&mut self, callee: &Expr, explicit: usize, implicit: u8, span: Span) {
        let ExprKind::Ident(name) = &callee.kind else { return };
        if self.lookup_local(name).is_some() || self.find_global(name).is_some() || self.resolve_local_at(self.outer.len(), name).is_some()
        {
            return;
        }
        let expected = if let Some((_, arity)) = self.find_fn(name) {
            Some((arity, Some(arity)))
        } else {
            self.find_builtin(name).map(|b| {
                let spec = &BUILTINS[b as usize];
                (spec.min_args, spec.max_args)
            })
        };
        let Some((min, max)) = expected else { return };
        let n = (explicit + implicit as usize).min(u8::MAX as usize) as u8;
        if n >= min && max.is_none_or(|m| n <= m) {
            return;
        }
        let want = match max {
            Some(m) if m == min => format!("{min}"),
            Some(m) => format!("{min} to {m}"),
            None => format!("at least {min}"),
        };
        let mut d =
            Diagnostic::error(format!("`{name}` takes {want} argument{}, but {n} were supplied", if want == "1" { "" } else { "s" }), span)
                .with_label("wrong number of arguments");
        if implicit > 0 {
            d = d.with_note("change handlers receive the new value as their last argument");
        }
        self.diagnostics.push(d);
    }

    // ------------------------------------------------------- types and closures

    /// The type named by `expr` when it is a bare type name (`Point`, `Shape`).
    fn static_type(&self, expr: &Expr) -> Option<u16> {
        match &expr.kind {
            ExprKind::Ident(name) => {
                if self.lookup_local(name).is_some() || self.find_global(name).is_some() || self.find_fn(name).is_some() {
                    return None;
                }
                self.find_type(name)
            }
            // `module.Type`
            ExprKind::Field { target, name } => {
                let m = self.module_alias(target)?;
                self.type_index.get(&format!("{m}.{}", name.name)).copied()
            }
            _ => None,
        }
    }

    fn is_constructor(&self, callee: &Expr) -> bool {
        match &callee.kind {
            ExprKind::Ident(name) => {
                self.static_type(callee).is_some() || (self.bare_builtin_variant(name).is_some() && self.lookup_local(name).is_none())
            }
            _ => false,
        }
    }

    /// `Some`, `None`, `Ok` and `Err` as expressions.
    fn bare_builtin_variant(&self, name: &str) -> Option<(u16, u16)> {
        match name {
            "Some" | "None" | "Ok" | "Err" => self.bare_variant(name),
            _ => None,
        }
    }

    fn name_constant(&mut self, name: &str, span: Span) -> u16 {
        let key = ConstKey::Str(name.to_owned());
        if let Some(&i) = self.const_index.get(&key) {
            return i;
        }
        if self.constants.len() >= u16::MAX as usize {
            self.error("too many constants in program (max 65535)", span);
            return 0;
        }
        let i = self.constants.len() as u16;
        self.constants.push(Constant::Str(name.to_owned()));
        self.const_index.insert(key, i);
        i
    }

    fn check_field_name(&mut self, name: &Ident) {
        if self.field_names.contains(&name.name) {
            return;
        }
        let mut d = Diagnostic::error(format!("no struct has a field named `{}`", name.name), name.span).with_label("unknown field");
        if let Some(similar) = best_match(&name.name, self.field_names.iter().map(String::as_str)) {
            d = d.with_note(format!("a field with a similar name exists: `{similar}`"));
        } else if self.method_names.contains(&name.name) || self.find_fn(&name.name).is_some() || self.find_builtin(&name.name).is_some() {
            d = d.with_note(format!("to call it, write `.{}()`", name.name));
        }
        self.diagnostics.push(d);
    }

    fn emit_variant(&mut self, ty: u16, tag: u16) {
        self.f.code.push(Op::NewVariant as u8);
        self.f.code.extend_from_slice(&ty.to_le_bytes());
        self.f.code.extend_from_slice(&tag.to_le_bytes());
    }

    fn unknown_module_member(&mut self, module: &str, name: &Ident) {
        let prefix = format!("{module}.");
        let members: Vec<&str> = self
            .fn_index
            .keys()
            .chain(self.global_index.keys())
            .chain(self.type_index.keys())
            .filter_map(|k| k.strip_prefix(&prefix))
            .filter(|k| !k.contains('.'))
            .chain(std_members(module))
            .collect();
        let mut d = Diagnostic::error(format!("module `{module}` has no `{}`", name.name), name.span).with_label("not found");
        if let Some(similar) = best_match(&name.name, members) {
            d = d.with_note(format!("a similar name exists: `{similar}`"));
        }
        self.diagnostics.push(d);
    }

    /// The global `module.name`, for reading or assigning.
    fn module_global(&mut self, target: &Expr, name: &Ident) -> Option<Option<u16>> {
        let m = self.module_alias(target)?;
        let full = format!("{m}.{}", name.name);
        if STD_MODULES.contains(&m.as_str()) {
            match Self::std_member(&full) {
                Some(_) => self.error(format!("`{full}` belongs to the standard library and cannot be changed"), name.span),
                None => self.unknown_module_member(&m, name),
            }
            return Some(None);
        }
        Some(match self.global_index.get(&full) {
            Some(&g) => Some(g),
            None => {
                if self.fn_index.contains_key(&full) || self.type_index.contains_key(&full) {
                    self.error(format!("`{full}` is not a variable"), name.span);
                } else {
                    self.unknown_module_member(&m, name);
                }
                None
            }
        })
    }

    /// `x.name`, `Type.Variant`, `Type.function`, `module.function` or `module.variable`.
    fn compile_field(&mut self, target: &Expr, name: &Ident, span: Span) {
        if self.unimported_std_module(target) {
            return;
        }
        if let Some(m) = self.module_alias(target) {
            let full = format!("{m}.{}", name.name);
            if STD_MODULES.contains(&m.as_str()) {
                return match Self::std_member(&full) {
                    Some(var) => self.emit_get(var),
                    None => self.unknown_module_member(&m, name),
                };
            }
            if let Some(&(func, _)) = self.fn_index.get(&full) {
                return self.emit_u16(Op::Func, func);
            }
            if self.type_index.contains_key(&full) {
                return self.diagnostics.push(Diagnostic::error(format!("`{full}` is a type, not a value"), span));
            }
            if let Some(Some(g)) = self.module_global(target, name) {
                self.emit_u16(Op::GetGlobal, g);
            }
            return;
        }
        if let Some(ty) = self.static_type(target) {
            let def = &self.types[ty as usize];
            if let Some(tag) = def.variants().iter().position(|(v, _)| *v == name.name) {
                let arity = def.variants()[tag].1;
                if arity > 0 {
                    let (t, v) = (def.name.clone(), name.name.clone());
                    let blanks = vec!["..."; arity as usize].join(", ");
                    return self.diagnostics.push(
                        Diagnostic::error(format!("`{t}.{v}` needs {arity} value(s)"), span)
                            .with_note(format!("write `{t}.{v}({blanks})`")),
                    );
                }
                return self.emit_variant(ty, tag as u16);
            }
            if let Some(&(func, _)) = self.assoc.get(&(ty, name.name.clone())) {
                return self.emit_u16(Op::Func, func);
            }
            return self.unknown_member(ty, name);
        }
        self.check_field_name(name);
        self.compile_expr(target);
        let c = self.name_constant(&name.name, name.span);
        self.emit_u16(Op::GetField, c);
    }

    fn unknown_member(&mut self, ty: u16, name: &Ident) {
        let def = &self.types[ty as usize];
        let members: Vec<&str> = def
            .variants()
            .iter()
            .map(|(v, _)| v.as_str())
            .chain(self.assoc.keys().filter(|(t, _)| *t == ty).map(|(_, n)| n.as_str()))
            .collect();
        let mut d = Diagnostic::error(format!("`{}` has no variant or function named `{}`", def.name, name.name), name.span);
        if let Some(similar) = best_match(&name.name, members) {
            d = d.with_note(format!("a similar name exists: `{similar}`"));
        } else if self.structs.contains_key(&ty) {
            d = d.with_note(format!("fields belong to values: create one with `{}(...)` first", def.name));
        }
        self.diagnostics.push(d);
    }

    /// Struct construction and enum variants with values. Returns `false`
    /// when `callee` is an ordinary function.
    fn compile_constructor(&mut self, callee: &Expr, args: &[Expr], named: &[(Ident, Expr)], span: Span) -> bool {
        let ExprKind::Ident(name) = &callee.kind else { return false };
        if let Some(ty) = self.static_type(callee) {
            if self.structs.contains_key(&ty) {
                self.compile_struct_literal(ty, args, named, span);
            } else {
                let t = name.clone();
                let example = self.types[ty as usize].variants().first().map_or(String::new(), |(v, _)| format!(": `{t}.{v}`"));
                self.diagnostics.push(
                    Diagnostic::error(format!("`{t}` is an enum: create one of its variants"), callee.span)
                        .with_note(format!("for example{example}")),
                );
            }
            return true;
        }
        if self.lookup_local(name).is_some() {
            return false;
        }
        match self.bare_builtin_variant(name) {
            Some((ty, tag)) => {
                self.compile_variant_args(ty, tag, args, named, span);
                true
            }
            None => false,
        }
    }

    fn compile_variant_args(&mut self, ty: u16, tag: u16, args: &[Expr], named: &[(Ident, Expr)], span: Span) {
        let (variant, arity) = self.types[ty as usize].variants()[tag as usize].clone();
        if let Some((n, _)) = named.first() {
            return self.error("enum variants take their values in order, without names", n.span);
        }
        if arity == 0 {
            return self.diagnostics.push(
                Diagnostic::error(format!("`{variant}` has no values"), span).with_note(format!("write `{variant}` without parentheses")),
            );
        }
        if args.len() != arity as usize {
            return self.error(format!("`{variant}` takes {arity} value(s), but {} were supplied", args.len()), span);
        }
        for a in args {
            self.compile_expr(a);
        }
        self.emit_variant(ty, tag);
    }

    fn compile_struct_literal(&mut self, ty: u16, args: &[Expr], named: &[(Ident, Expr)], span: Span) {
        let info = &self.structs[&ty];
        let fields = info.fields.clone();
        let defaults = info.defaults.clone();
        let type_name = self.types[ty as usize].name.clone();
        if args.len() > fields.len() {
            return self.error(format!("`{type_name}` has {} field(s), but {} values were supplied", fields.len(), args.len()), span);
        }
        let mut values: Vec<Option<&Expr>> = args.iter().map(Some).collect();
        values.resize(fields.len(), None);
        for (field, value) in named {
            match fields.iter().position(|f| *f == field.name) {
                Some(i) if values[i].is_some() => self.error(format!("field `{}` is given more than once", field.name), field.span),
                Some(i) => values[i] = Some(value),
                None => {
                    let mut d =
                        Diagnostic::error(format!("`{type_name}` has no field `{}`", field.name), field.span).with_label("unknown field");
                    if let Some(similar) = best_match(&field.name, fields.iter().map(String::as_str)) {
                        d = d.with_note(format!("a field with a similar name exists: `{similar}`"));
                    }
                    self.diagnostics.push(d);
                }
            }
        }
        let missing: Vec<String> = fields
            .iter()
            .enumerate()
            .filter(|(i, _)| values[*i].is_none() && defaults[*i].is_none())
            .map(|(_, f)| format!("`{f}`"))
            .collect();
        if !missing.is_empty() {
            return self.diagnostics.push(
                Diagnostic::error(format!("missing field(s) {} in `{type_name}`", missing.join(", ")), span)
                    .with_note(format!("fields: {}", fields.join(", "))),
            );
        }
        for (i, value) in values.iter().enumerate() {
            match value {
                Some(v) => self.compile_expr(v),
                None => {
                    // Defaults see globals and functions, not the caller's locals.
                    let default = defaults[i].clone().expect("checked above");
                    let saved = core::mem::replace(&mut self.detached, true);
                    let module = self.structs[&ty].module.clone();
                    let saved_module = core::mem::replace(&mut self.cur_module, module);
                    self.compile_expr(&default);
                    self.cur_module = saved_module;
                    self.detached = saved;
                }
            }
        }
        self.emit_u16(Op::NewStruct, ty);
    }

    /// The function used when the receiver's type has no such method:
    /// `x.f(a)` calls `f(x, a)`.
    fn push_method_fallback(&mut self, method: &Ident, argc: usize) {
        let name = &method.name;
        let fallback = self.resolve_quiet(name).filter(|v| !matches!(v, Var::Type(_) | Var::Variant(..)));
        match fallback {
            Some(var) => {
                if !self.method_names.contains(name) {
                    let callee = Expr { kind: ExprKind::Ident(name.clone()), span: method.span };
                    self.check_arity(&callee, argc, 0, method.span);
                }
                self.emit_get(var);
            }
            None => {
                if !self.method_names.contains(name) {
                    let mut d = Diagnostic::error(format!("no method or function named `{name}`"), method.span).with_label("not found");
                    let candidates: Vec<String> = self
                        .method_names
                        .iter()
                        .cloned()
                        .chain(self.fn_index.keys().cloned())
                        .chain(BUILTINS.iter().map(|b| b.name.to_owned()))
                        .collect();
                    if let Some(similar) = best_match(name, candidates.iter().map(String::as_str)) {
                        d = d.with_note(format!("a similar name exists: `{similar}`"));
                    } else if self.field_names.contains(name) {
                        d = d.with_note(format!("`{name}` is a field: write `.{name}` without parentheses"));
                    }
                    self.diagnostics.push(d);
                }
                self.emit(Op::Nil);
            }
        }
    }

    /// A function named statically: `module.f` or `Type.f`.
    fn static_callee(&self, receiver: &Expr, method: &Ident) -> Option<(u16, u8)> {
        if let Some(m) = self.module_alias(receiver) {
            return self.fn_index.get(&format!("{m}.{}", method.name)).copied();
        }
        let ty = self.static_type(receiver)?;
        self.assoc.get(&(ty, method.name.clone())).copied()
    }

    /// `x.f(a)`, `Type.f(a)`, `Type.Variant(a)`, `module.f(a)` or `module.Struct(...)`.
    fn compile_method_call(&mut self, receiver: &Expr, method: &Ident, args: &[Expr], named: &[(Ident, Expr)], span: Span) {
        if self.unimported_std_module(receiver) {
            return;
        }
        if let Some(m) = self.module_alias(receiver) {
            let full = format!("{m}.{}", method.name);
            if let Some(&t) = self.type_index.get(&full) {
                if self.structs.contains_key(&t) {
                    return self.compile_struct_literal(t, args, named, span);
                }
                return self.error(format!("`{full}` is an enum: create one of its variants, e.g. `{full}.Variant(...)`"), span);
            }
            if let Some((n, _)) = named.first() {
                return self.error("named arguments are only used to create structs", n.span);
            }
            if STD_MODULES.contains(&m.as_str()) {
                match Self::std_member(&full) {
                    Some(Var::Builtin(b)) => {
                        let spec = &BUILTINS[b as usize];
                        let n = args.len();
                        if n < spec.min_args as usize || spec.max_args.is_some_and(|m| n > m as usize) {
                            let doc = spec.doc;
                            self.diagnostics.push(
                                Diagnostic::error(format!("`{full}` called with {n} argument(s)"), span)
                                    .with_label("wrong number of arguments")
                                    .with_note(doc),
                            );
                        }
                        self.emit_u8(Op::Builtin, b);
                        for a in args {
                            self.compile_expr(a);
                        }
                        self.emit_u8(Op::Call, args.len() as u8);
                    }
                    Some(_) => self.error(format!("`{full}` is a value, not a function: write `{full}` without parentheses"), span),
                    None => self.unknown_module_member(&m, method),
                }
                return;
            }
            let Some(&(func, arity)) = self.fn_index.get(&full) else { return self.unknown_module_member(&m, method) };
            if args.len() != arity as usize {
                self.error(format!("`{full}` takes {arity} argument(s), but {} were supplied", args.len()), span);
            }
            self.emit_u16(Op::Func, func);
            for a in args {
                self.compile_expr(a);
            }
            return self.emit_u8(Op::Call, args.len() as u8);
        }
        if let Some((n, _)) = named.first() {
            return self.error("named arguments are only used to create structs", n.span);
        }
        if let Some(ty) = self.static_type(receiver) {
            if let Some(tag) = self.types[ty as usize].variants().iter().position(|(v, _)| *v == method.name) {
                return self.compile_variant_args(ty, tag as u16, args, &[], span);
            }
            let Some(&(func, arity)) = self.assoc.get(&(ty, method.name.clone())) else { return self.unknown_member(ty, method) };
            if args.len() != arity as usize {
                let t = self.types[ty as usize].name.clone();
                self.error(format!("`{t}.{}` takes {arity} argument(s), but {} were supplied", method.name, args.len()), span);
            }
            self.emit_u16(Op::Func, func);
            for a in args {
                self.compile_expr(a);
            }
            return self.emit_u8(Op::Call, args.len() as u8);
        }
        if args.len() >= u8::MAX as usize {
            return self.error("too many arguments (max 254)", span);
        }
        self.push_method_fallback(method, args.len() + 1);
        self.compile_expr(receiver);
        for a in args {
            self.compile_expr(a);
        }
        let c = self.name_constant(&method.name, method.span);
        self.emit_u16(Op::Method, c);
        self.f.code.push(args.len() as u8);
    }

    /// Compile a nested function or lambda and push it: a plain function
    /// when it captures nothing, otherwise a closure over the captured values.
    fn compile_closure(&mut self, name: &str, self_name: Option<&str>, params: &[Ident], body: ClosureBody<'_>) {
        if params.len() > u8::MAX as usize {
            self.error("too many parameters (max 255)", params[0].span);
        }
        let idx = self.reserve_function(name);
        let state = FnState { is_closure: true, self_name: self_name.map(str::to_owned), ..FnState::function() };
        let parent = core::mem::replace(&mut self.f, state);
        self.outer.push(parent);
        for p in params {
            self.declare_local(&p.name);
        }
        match body {
            ClosureBody::Block(block) => {
                self.compile_block_inner(block);
                self.emit(Op::Nil);
            }
            ClosureBody::Expr(expr) => {
                self.mark_line(expr.span);
                self.compile_expr(expr);
            }
        }
        self.emit(Op::Return);
        let parent = self.outer.pop().expect("closure has a parent");
        let state = core::mem::replace(&mut self.f, parent);
        let captures = state.captures.clone();
        self.store_function(idx, state, params.len().min(255) as u8);
        if captures.is_empty() {
            return self.emit_u16(Op::Func, idx);
        }
        if captures.len() > u8::MAX as usize {
            return self.error("a closure can capture at most 255 variables", Span::default());
        }
        for (_, var) in &captures {
            self.emit_get(*var);
        }
        self.f.code.push(Op::Closure as u8);
        self.f.code.extend_from_slice(&idx.to_le_bytes());
        self.f.code.push(captures.len() as u8);
    }

    fn state(&self, level: usize) -> &FnState {
        if level == self.outer.len() {
            &self.f
        } else {
            &self.outer[level]
        }
    }

    fn state_mut(&mut self, level: usize) -> &mut FnState {
        if level == self.outer.len() {
            &mut self.f
        } else {
            &mut self.outer[level]
        }
    }

    /// Resolve `name` among the locals of the function at `level` and,
    /// for closures, of its enclosing functions (capturing them).
    fn resolve_local_at(&mut self, level: usize, name: &str) -> Option<Var> {
        let st = self.state(level);
        if let Some(slot) = st.lookup_local(name) {
            return Some(Var::Local(slot));
        }
        if st.self_name.as_deref() == Some(name) {
            return Some(Var::Current);
        }
        if let Some(i) = st.captures.iter().position(|(n, _)| n == name) {
            return Some(Var::Capture(i as u8));
        }
        if level == 0 || !st.is_closure {
            return None;
        }
        let from = self.resolve_local_at(level - 1, name)?;
        let st = self.state_mut(level);
        st.captures.push((name.to_owned(), from));
        Some(Var::Capture((st.captures.len() - 1).min(255) as u8))
    }

    fn emit_binary(&mut self, op: BinaryOp) {
        self.emit(match op {
            BinaryOp::Add => Op::Add,
            BinaryOp::Sub => Op::Sub,
            BinaryOp::Mul => Op::Mul,
            BinaryOp::Div => Op::Div,
            BinaryOp::Rem => Op::Rem,
            BinaryOp::Eq => Op::Eq,
            BinaryOp::Ne => Op::Ne,
            BinaryOp::Lt => Op::Lt,
            BinaryOp::Le => Op::Le,
            BinaryOp::Gt => Op::Gt,
            BinaryOp::Ge => Op::Ge,
            BinaryOp::Range => Op::Range,
        });
    }

    fn emit_folded(&mut self, c: Folded, span: Span) {
        match c {
            Folded::Int(v) => self.emit_const(Constant::Int(v), span),
            Folded::Float(v) => self.emit_const(Constant::Float(v), span),
            Folded::Str(s) => self.emit_const(Constant::Str(s), span),
            Folded::Bool(true) => self.emit(Op::True),
            Folded::Bool(false) => self.emit(Op::False),
        }
    }

    // --------------------------------------------------------------- resolving

    fn lookup_local(&self, name: &str) -> Option<u16> {
        if self.detached {
            return None;
        }
        self.f.lookup_local(name)
    }

    /// Resolve without reporting unknown names.
    fn resolve_quiet(&mut self, name: &str) -> Option<Var> {
        if !self.detached {
            if let Some(var) = self.resolve_local_at(self.outer.len(), name) {
                return Some(var);
            }
        }
        if let Some(g) = self.find_global(name) {
            return Some(Var::Global(g));
        }
        if let Some((f, _)) = self.find_fn(name) {
            return Some(Var::Func(f));
        }
        if let Some(t) = self.find_type(name) {
            return Some(Var::Type(t));
        }
        if let Some((t, v)) = self.bare_builtin_variant(name) {
            return Some(Var::Variant(t, v));
        }
        if let Some(ImportTarget::Name(full)) = self.imports.get(&(self.cur_module.clone(), name.to_owned())) {
            if let Some(var) = Self::std_member(full) {
                return Some(var);
            }
        }
        builtin_index(name).map(Var::Builtin)
    }

    fn resolve_var(&mut self, name: &str, span: Span) -> Option<Var> {
        if let Some(var) = self.resolve_quiet(name) {
            return Some(var);
        }
        let mut d = Diagnostic::error(format!("cannot find value `{name}` in this scope"), span).with_label("not found");
        if let Some(similar) = self.suggest(name) {
            d = d.with_note(format!("a name with a similar spelling exists: `{similar}`"));
        } else if UiKind::from_name(name).is_some() {
            d = d.with_note(format!("`{name}` is a UI element: write `{name} value` on its own line, without parentheses"));
        }
        if matches!(self.imports.get(&(self.cur_module.clone(), name.to_owned())), Some(ImportTarget::Module(_))) {
            d = Diagnostic::error(format!("`{name}` is a module, not a value"), span)
                .with_note(format!("use its members: `{name}.function(...)`"));
        }
        self.diagnostics.push(d);
        None
    }

    fn suggest(&self, name: &str) -> Option<String> {
        let candidates = self
            .f
            .scopes
            .iter()
            .flat_map(|s| s.iter().map(|(n, _)| n.as_str()))
            .chain(self.globals.iter().map(String::as_str))
            .chain(self.fn_index.keys().map(String::as_str))
            .chain(BUILTINS.iter().map(|b| b.name))
            .filter(|c| !c.starts_with('<') && !c.contains('.'));
        best_match(name, candidates).map(str::to_owned)
    }

    fn emit_get(&mut self, var: Var) {
        match var {
            Var::Local(s) => self.emit_u16(Op::GetLocal, s),
            Var::Global(g) => self.emit_u16(Op::GetGlobal, g),
            Var::Func(f) => self.emit_u16(Op::Func, f),
            Var::Builtin(b) => self.emit_u8(Op::Builtin, b),
            Var::Capture(i) => self.emit_u8(Op::GetCapture, i),
            Var::Current => self.emit(Op::Current),
            // Reported by `emit_value`; push something to keep the stack balanced.
            Var::Type(_) => self.emit(Op::Nil),
            Var::Variant(t, v) => self.emit_variant(t, v),
            Var::StdConst(v) => self.emit_const(Constant::Float(v), Span::default()),
            Var::Property(b) => {
                self.emit_u8(Op::Builtin, b);
                self.emit_u8(Op::Call, 0);
            }
        }
    }

    /// A name used as a value.
    fn emit_value(&mut self, var: Var, name: &str, span: Span) {
        match var {
            Var::Type(t) => {
                let note = if self.structs.contains_key(&t) {
                    format!("create a value with `{name}(...)`")
                } else {
                    let first = self.types[t as usize].variants().first().map_or("Variant".to_owned(), |(v, _)| v.clone());
                    format!("use one of its variants, e.g. `{name}.{first}`")
                };
                self.diagnostics.push(Diagnostic::error(format!("`{name}` is a type, not a value"), span).with_note(note));
            }
            Var::Variant(t, v) if self.types[t as usize].variants()[v as usize].1 > 0 => {
                self.diagnostics
                    .push(Diagnostic::error(format!("`{name}` needs a value"), span).with_note(format!("write `{name}(value)`")));
            }
            _ => self.emit_get(var),
        }
    }

    fn emit_set(&mut self, var: Var, span: Span) {
        match var {
            Var::Local(s) => self.emit_u16(Op::SetLocal, s),
            Var::Global(g) => self.emit_u16(Op::SetGlobal, g),
            Var::Func(f) => {
                let name = self.functions[f as usize].name.clone();
                self.error(format!("cannot assign to function `{name}`"), span);
            }
            Var::Builtin(b) => self.error(format!("cannot assign to builtin `{}`", BUILTINS[b as usize].name), span),
            Var::Capture(i) => {
                let name = self.f.captures.get(i as usize).map_or(String::new(), |(n, _)| n.clone());
                self.diagnostics.push(
                    Diagnostic::error(format!("cannot assign to `{name}`, captured from the enclosing function"), span)
                        .with_label("captured by value")
                        .with_note("closures capture copies of variables; share state through a list, map, struct or global"),
                );
            }
            Var::Current => self.error("cannot assign to the function itself", span),
            Var::Type(_) | Var::Variant(..) => self.error("cannot assign to a type or variant", span),
            Var::StdConst(_) | Var::Property(_) => self.error("cannot assign to a standard library value", span),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Var {
    Local(u16),
    Global(u16),
    Func(u16),
    Builtin(u8),
    /// A value captured by the running closure.
    Capture(u8),
    /// The running nested function itself.
    Current,
    Type(u16),
    /// `None` (or another variant without values).
    Variant(u16, u16),
    /// A standard library constant (`math.pi`).
    StdConst(f64),
    /// A standard library value computed at runtime (`os.name`).
    Property(u8),
}

/// The alternatives of a pattern (itself unless it is an or-pattern).
fn alternatives(pattern: &Pattern) -> Vec<&Pattern> {
    match &pattern.kind {
        PatternKind::Or(alts) => alts.iter().flat_map(alternatives).collect(),
        _ => vec![pattern],
    }
}

/// The span of the first name a pattern binds.
fn first_binding(pattern: &Pattern, c: &Compiler<'_>) -> Option<Span> {
    match &pattern.kind {
        PatternKind::Bind(name) => Some(name.span),
        PatternKind::Ctor { path, args: None } if path.len() == 1 => {
            (c.bare_variant(&path[0].name).is_none() && c.find_type(&path[0].name).is_none()).then_some(pattern.span)
        }
        PatternKind::Ctor { args: Some(args), .. } => args.iter().find_map(|p| first_binding(p, c)),
        PatternKind::Or(alts) => alts.iter().find_map(|p| first_binding(p, c)),
        _ => None,
    }
}

// ------------------------------------------------------------------- folding

enum Folded {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
}

/// Evaluate constant expressions at compile time. Anything that could fail at
/// runtime (overflow, division by zero, type errors) is left to the VM so the
/// error is reported identically in debug and release builds.
fn fold(expr: &Expr) -> Option<Folded> {
    match &expr.kind {
        ExprKind::Binary { op, lhs, rhs } => {
            let (a, b) = (fold_leaf(lhs)?, fold_leaf(rhs)?);
            Some(match (a, b) {
                (Folded::Int(a), Folded::Int(b)) => match op {
                    BinaryOp::Add => Folded::Int(a.checked_add(b)?),
                    BinaryOp::Sub => Folded::Int(a.checked_sub(b)?),
                    BinaryOp::Mul => Folded::Int(a.checked_mul(b)?),
                    BinaryOp::Div if b != 0 => Folded::Int(a.checked_div(b)?),
                    BinaryOp::Rem if b != 0 => Folded::Int(a.checked_rem(b)?),
                    BinaryOp::Eq => Folded::Bool(a == b),
                    BinaryOp::Ne => Folded::Bool(a != b),
                    BinaryOp::Lt => Folded::Bool(a < b),
                    BinaryOp::Le => Folded::Bool(a <= b),
                    BinaryOp::Gt => Folded::Bool(a > b),
                    BinaryOp::Ge => Folded::Bool(a >= b),
                    _ => return None,
                },
                (Folded::Float(a), Folded::Float(b)) => match op {
                    BinaryOp::Add => Folded::Float(a + b),
                    BinaryOp::Sub => Folded::Float(a - b),
                    BinaryOp::Mul => Folded::Float(a * b),
                    BinaryOp::Div => Folded::Float(a / b),
                    _ => return None,
                },
                (Folded::Str(a), Folded::Str(b)) if *op == BinaryOp::Add => Folded::Str(a + &b),
                _ => return None,
            })
        }
        ExprKind::Unary { op: UnaryOp::Not, expr } => match fold_leaf(expr)? {
            Folded::Bool(b) => Some(Folded::Bool(!b)),
            _ => None,
        },
        _ => None,
    }
}

fn fold_leaf(expr: &Expr) -> Option<Folded> {
    match &expr.kind {
        ExprKind::Int(v) => Some(Folded::Int(*v)),
        ExprKind::Float(v) => Some(Folded::Float(*v)),
        ExprKind::Str(s) => Some(Folded::Str(s.clone())),
        ExprKind::Bool(b) => Some(Folded::Bool(*b)),
        _ => fold(expr),
    }
}

/// The value of a literal expression, for compile-time attribute checks.
fn literal(expr: &Expr) -> Option<RawValue<'_>> {
    Some(match &expr.kind {
        ExprKind::Int(v) => RawValue::Int(*v),
        ExprKind::Float(v) => RawValue::Float(*v),
        ExprKind::Str(s) => RawValue::Str(s),
        ExprKind::Bool(b) => RawValue::Bool(*b),
        ExprKind::Nil => RawValue::Other("nil"),
        ExprKind::List(_) => RawValue::Other("list"),
        ExprKind::Map(_) => RawValue::Other("map"),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    fn compile_src(src: &str, opts: CompileOptions) -> Result<Program, Vec<Diagnostic>> {
        let parsed = parse(src);
        assert!(parsed.diagnostics.iter().all(|d| !d.is_error()), "{:#?}", parsed.diagnostics);
        compile(&parsed.module, &SourceFile::new("test", src), opts).map(|(p, _)| p)
    }

    #[test]
    fn produces_verified_program() {
        let src = "let total = 0\nfn add(n)\n    total += n\nfor i in 0..5\n    add(i)\nprint(\"total = {total}\")\n";
        let p = compile_src(src, CompileOptions::DEBUG).unwrap();
        p.verify().unwrap();
        assert_eq!(p.globals, vec!["total"]);
        assert_eq!(p.functions.len(), 2);
        assert!(!p.functions[p.entry as usize].lines.is_empty());
        assert!(p.disassemble().contains("IterNext"));
    }

    #[test]
    fn folds_constants_in_release() {
        let p = compile_src("let x = 2 * 3 + 4\n", CompileOptions::RELEASE).unwrap();
        assert!(p.constants.contains(&Constant::Int(10)));
        assert!(!p.constants.contains(&Constant::Int(2)));
        assert!(p.functions[0].lines.is_empty());
    }

    #[test]
    fn reports_semantic_errors() {
        let errs =
            compile_src("fn f(a)\n    return a\nf(1, 2)\nprint(undefined_thing)\nlet count = 1\nprint(cuont)\n", CompileOptions::DEBUG)
                .unwrap_err();
        assert!(errs.iter().any(|d| d.message.contains("takes 1 argument")), "{errs:#?}");
        assert!(errs.iter().any(|d| d.message.contains("undefined_thing")));
        assert!(errs.iter().any(|d| d.notes.iter().any(|n| n.contains("`count`"))));
    }

    #[test]
    fn block_locals_reuse_slots() {
        let src = "fn f()\n    if true\n        let a = 1\n    if true\n        let b = 2\n        let c = 3\n";
        let p = compile_src(src, CompileOptions::DEBUG).unwrap();
        assert_eq!(p.functions[0].n_locals, 2);
    }

    fn errors_of(src: &str) -> Vec<Diagnostic> {
        let parsed = parse(src);
        let mut all = parsed.diagnostics.clone();
        if let Err(e) = compile(&parsed.module, &SourceFile::new("test", src), CompileOptions::DEBUG) {
            all.extend(e);
        }
        all.into_iter().filter(Diagnostic::is_error).collect()
    }

    fn assert_error(src: &str, needle: &str) {
        let errs = errors_of(src);
        assert!(errs.iter().any(|d| d.message.contains(needle) || d.notes.iter().any(|n| n.contains(needle))), "{src:?}\n{errs:#?}");
    }

    #[test]
    fn ui_elements_compile_with_bindings() {
        let src = "\
let name = \"\"
let flags = [false, true]
let volume = 5
fn on_volume(v)
    volume = v
fn tag(t)
    label t
        color: \"secondary\"
view
    window \"App\" theme: \"light\", accent: \"#6750a4\"
        input name placeholder: \"Name\"
        checkbox flags[1] text: \"Second\"
        slider volume -> on_volume
        tag(\"x\")
        button \"Go\" icon: \"send\", style: \"filled\" -> on_volume(1)
";
        let p = compile_src(src, CompileOptions::DEBUG).unwrap();
        p.verify().unwrap();
        let names: Vec<_> = p.functions.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"<set name>") && names.contains(&"<set index>"), "{names:?}");
        assert!(!names.contains(&"<set volume>"), "a handler suppresses the binding");
        let listing = p.disassemble();
        assert!(listing.contains("UiAttr          placeholder"), "{listing}");
        assert!(listing.contains("UiBegin         checkbox"));
    }

    #[test]
    fn ui_static_checks() {
        assert_error("view\n    label \"x\" paddin: 3\n", "`padding`");
        assert_error("view\n    label \"x\" placeholder: \"y\"\n", "`label` has no attribute `placeholder`");
        assert_error("view\n    card padding: \"big\"\n", "`padding` expects a number");
        assert_error("view\n    button \"x\" icon: \"hom\" -> f()\nfn f()\n    return\n", "did you mean \"home\"");
        assert_error("view\n    label \"x\"\n        label \"y\"\n", "`label` cannot contain child elements");
        assert_error("view\n    accordion\n        label \"y\"\n", "`accordion` requires an argument");
        assert_error("view\n    group open: true\n", "`group` has no attribute `open`");
        assert_error("view\n    button \"x\" style: \"glassy\" -> f()\nfn f()\n    return\n", "\"frosted\"");
        assert_error("view\n    padding: 4\n", "outside of an element");
        assert_error("label \"x\"\n", "can only be used inside a `view`");
        assert_error("view\n    checkbox \"yes\"\n", "`checkbox` expects a bool");
        assert_error("view\n    input \"fixed\"\n", "needs a variable to bind to");
        assert_error("fn f()\n    let t = \"\"\n    input t\nview\n    f()\n", "local variable");
        assert_error("fn set(v)\n    return\nview\n    slider 1 -> set(2)\n", "receive the new value");
        assert_error("view\n    window\n        column: 1\n", "is an element");
        assert_error("view\n    label(\"x\")\n", "is a UI element");
    }
}

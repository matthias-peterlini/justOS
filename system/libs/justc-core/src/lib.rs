//! # justc-core
//!
//! The JustScript compiler front- and middle-end:
//!
//! ```text
//! source ──lexer──▶ tokens ──parser──▶ AST ──compiler──▶ Program ──package──▶ .jpk
//! ```
//!
//! Everything is implemented by hand without parser generators. All stages
//! are error-tolerant and report [`Diagnostic`]s instead of panicking, which
//! makes the same code usable by the CLI (`justc`) and by the IDE.
//!
//! In justOS the crate is `no_std` + `alloc` (it comes from the JustScript
//! repository, where it uses `std`): `deflate` replaces the `miniz_oxide`
//! crate, `float` the `f64` functions of `std`; executable bundles are
//! left out.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

/// What `std`'s prelude would bring in.
pub(crate) mod prelude {
    pub use alloc::borrow::ToOwned;
    pub use alloc::boxed::Box;
    pub use alloc::format;
    pub use alloc::string::{String, ToString};
    pub use alloc::vec;
    pub use alloc::vec::Vec;

    pub use crate::float::F64Ext;
}

use prelude::*;

pub mod ast;
pub mod bytecode;
pub mod compiler;
pub mod crc32;
pub mod deflate;
pub mod diagnostic;
pub mod float;
pub mod highlight;
pub mod lexer;
pub mod jpk;
pub mod parser;
pub mod span;
pub mod suggest;
pub mod token;
pub mod ui;

pub use bytecode::Program;
pub use compiler::CompileOptions;
pub use diagnostic::{Diagnostic, Severity};
pub use jpk::Jpk;
pub use span::{SourceFile, Span};

/// Compiler version string embedded into package manifests.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Successful compilation result.
#[derive(Debug, Clone)]
pub struct CompileOutput {
    pub program: Program,
    /// Non-fatal diagnostics.
    pub warnings: Vec<Diagnostic>,
    pub source: SourceFile,
}

/// Failed compilation: every diagnostic plus the source for rendering.
#[derive(Debug, Clone)]
pub struct CompileFailure {
    pub diagnostics: Vec<Diagnostic>,
    pub source: SourceFile,
}

impl CompileFailure {
    pub fn render(&self) -> String {
        diagnostic::render_all(&self.diagnostics, &self.source)
    }
}

impl core::fmt::Display for CompileFailure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.render())
    }
}

impl core::error::Error for CompileFailure {}

/// Loads the source of an imported module by name (`import utils` asks
/// for `"utils"`): returns the file name shown in diagnostics and its text.
pub type ModuleLoader<'a> = dyn FnMut(&str) -> Result<(String, String), String> + 'a;

/// Run the full pipeline: lex, parse, compile and verify. Programs that
/// `import` modules need [`compile_with_modules`].
pub fn compile_source(name: &str, src: &str, opts: CompileOptions) -> Result<CompileOutput, CompileFailure> {
    compile_with_modules(name, src, opts, &mut |module| {
        Err(format!("cannot load module `{module}` here: save the file and build it with `justc`"))
    })
}

/// Like [`compile_source`], loading imported modules with `load`.
pub fn compile_with_modules(
    name: &str,
    src: &str,
    opts: CompileOptions,
    load: &mut ModuleLoader<'_>,
) -> Result<CompileOutput, CompileFailure> {
    let mut source = SourceFile::new(name, src);
    let parsed = parser::parse(src);
    let mut diagnostics = parsed.diagnostics;
    let mut modules: Vec<(Option<String>, ast::Module)> = vec![(None, parsed.module)];
    // Load every module reachable through `import`, each once.
    let mut i = 0;
    while i < modules.len() {
        let imports: Vec<ast::Ident> = modules[i]
            .1
            .stmts
            .iter()
            .filter_map(|s| match &s.kind {
                ast::StmtKind::Import(import) => Some(import.module.clone()),
                _ => None,
            })
            .collect();
        for module in imports {
            // Standard library modules are built in.
            let std = bytecode::STD_MODULES.contains(&module.name.as_str());
            if std || modules.iter().any(|(n, _)| n.as_deref() == Some(module.name.as_str())) {
                continue;
            }
            match load(&module.name) {
                Ok((file, text)) => {
                    let base = source.add(file, text.clone());
                    let parsed = parser::parse_at(&text, base);
                    diagnostics.extend(parsed.diagnostics);
                    modules.push((Some(module.name.clone()), parsed.module));
                }
                Err(message) => diagnostics.push(Diagnostic::error(message, module.span).with_label("module not found")),
            }
        }
        i += 1;
    }
    let (errors, mut warnings): (Vec<_>, Vec<_>) = diagnostics.into_iter().partition(Diagnostic::is_error);
    if !errors.is_empty() {
        let mut diagnostics = errors;
        diagnostics.extend(warnings);
        diagnostics.sort_by_key(|d| d.span.start);
        return Err(CompileFailure { diagnostics, source });
    }
    let units: Vec<compiler::Unit<'_>> = modules.iter().map(|(name, module)| compiler::Unit { name: name.clone(), module }).collect();
    match compiler::compile_units(&units, &source, opts) {
        Ok((program, more)) => {
            debug_assert!(program.verify().is_ok(), "compiler emitted unverifiable bytecode: {:?}", program.verify());
            warnings.extend(more);
            Ok(CompileOutput { program, warnings, source })
        }
        Err(mut diagnostics) => {
            diagnostics.extend(warnings);
            diagnostics.sort_by_key(|d| d.span.start);
            Err(CompileFailure { diagnostics, source })
        }
    }
}

/// All diagnostics for `src` (parse + semantic), for live editor feedback.
/// Diagnostics located in imported modules are left out.
pub fn check(src: &str) -> Vec<Diagnostic> {
    check_with_modules(src, &mut |module| Err(format!("cannot find module `{module}`")))
}

/// Like [`check`], loading imported modules with `load`.
pub fn check_with_modules(src: &str, load: &mut ModuleLoader<'_>) -> Vec<Diagnostic> {
    let all = match compile_with_modules("<check>", src, CompileOptions::DEBUG, load) {
        Ok(out) => out.warnings,
        Err(fail) => fail.diagnostics,
    };
    all.into_iter().filter(|d| (d.span.start as usize) <= src.len()).collect()
}

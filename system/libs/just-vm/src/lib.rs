//! # just-vm
//!
//! The JustScript runtime: a compact stack-based bytecode interpreter with
//! reference-counted values and a reactive declarative UI layer.
//!
//! ```ignore
//! use justc_core::{compile_source, CompileOptions};
//! use just_vm::Vm;
//!
//! let out = compile_source("demo.jt", "print(\"hi\")\n", CompileOptions::DEBUG).unwrap();
//! let mut vm = Vm::with_host(out.program, Box::new(|line: &str| show(line))).unwrap();
//! vm.run_main().unwrap();
//! ```
//!
//! In justOS the crate is `no_std` + `alloc` (it comes from the JustScript
//! repository, where it uses `std`): files, the clock, the environment and
//! the network are reached through [`Host`], which each system implements.
//! The window renderer, its themes and the desktop launcher are left out;
//! the reactive UI tree is kept.

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

    pub use justc_core::float::F64Ext;
}

mod builtins;
mod stdlib;
pub mod ui;
pub mod value;
pub mod vm;

pub use ui::{ActionId, UiEvent, UiNode, UiValue};
pub use value::Value;
pub use vm::{Host, HttpRequest, HttpResponse, Limits, RunOutput, RuntimeError, Vm};

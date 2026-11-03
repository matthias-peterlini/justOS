//! Run-time library for arcOS programs.
//!
//! A program is a `#![no_std]`, `#![no_main]` binary:
//!
//! ```ignore
//! #![no_std]
//! #![no_main]
//! use arcrt::println;
//!
//! arcrt::main!(main);
//!
//! fn main(args: &str) -> i32 {
//!     println!("hello, {args}");
//!     0
//! }
//! ```
//!
//! `alloc` works (`String`, `Vec`, `format!`); [`fs`], [`console`],
//! [`time`], [`process`], [`ipc`] (messages between programs) and [`app`]
//! (the app's sandbox, the file picker, permissions) wrap the system calls.

#![no_std]

extern crate alloc;

pub mod app;
pub mod console;
pub mod fs;
mod heap;
pub mod ipc;
pub mod process;
pub mod sys;
pub mod time;

pub use arcdk::syscall::Error;
pub use console::Key;

pub type Result<T> = core::result::Result<T, Error>;

/// Declares the program's `main(args: &str) -> i32`.
#[macro_export]
macro_rules! main {
    ($main:path) => {
        #[no_mangle]
        fn __arc_main(args: &str) -> i32 {
            $main(args)
        }
    };
}

extern "Rust" {
    fn __arc_main(args: &str) -> i32;
}

/// Entry point: the kernel passes the argument string.
#[no_mangle]
unsafe extern "C" fn _start(args: *const u8, len: usize) -> ! {
    // SAFETY: the kernel copied `len` bytes of UTF-8 to the top of the stack.
    let args = unsafe { core::str::from_utf8_unchecked(core::slice::from_raw_parts(args, len)) };
    // SAFETY: provided by the program through `main!`.
    let code = unsafe { __arc_main(args) };
    process::exit(code)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    eprintln!("panic: {info}");
    process::exit(101)
}

/// Prints to the console.
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => { $crate::console::print_fmt(format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($($arg:tt)*) => { $crate::console::print_fmt(format_args!("{}\n", format_args!($($arg)*))) };
}

/// Prints an error in red.
#[macro_export]
macro_rules! eprintln {
    ($($arg:tt)*) => { $crate::console::print_fmt(format_args!("\x1b[91m{}\x1b[0m\n", format_args!($($arg)*))) };
}

/// Splits an argument string into words; "double quotes" group words.
pub fn split_args(args: &str) -> alloc::vec::Vec<alloc::string::String> {
    let mut out = alloc::vec::Vec::new();
    let mut cur = alloc::string::String::new();
    let (mut quoted, mut any) = (false, false);
    for c in args.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any || !cur.is_empty() {
                    out.push(core::mem::take(&mut cur));
                    any = false;
                }
            }
            c => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

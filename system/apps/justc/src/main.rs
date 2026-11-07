//! `justc`: the JustScript compiler of justOS. Builds `.jt` programs and
//! projects into `.jpk` packages, checks and runs them (see `cli`).

#![no_std]
#![no_main]

extern crate alloc;

mod cli;
mod host;
mod project;

justrt::main!(main);

fn main(args: &str) -> i32 {
    cli::main(args, cli::Tool::Compiler)
}

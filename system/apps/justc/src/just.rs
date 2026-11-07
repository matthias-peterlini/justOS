//! `just`: the JustScript runner of justOS. `just main.jt` and
//! `just app.jpk` run a program; it is the system's runner for `.jt` and
//! `.jpk` files (see `cli`).

#![no_std]
#![no_main]

extern crate alloc;

mod cli;
mod host;
mod project;

justrt::main!(main);

fn main(args: &str) -> i32 {
    cli::main(args, cli::Tool::Runner)
}

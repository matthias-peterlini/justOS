//! Runs Android apps and Java programs on JustOS.
//!
//! `dalvik` reads the DEX code of an `.apk`, a `.jar` built with `d8`/`dx`
//! or a bare `.dex` file and runs it with the [`justdex`] interpreter. A
//! program with `public static void main(String[])` runs in the terminal;
//! an app with an `Activity` gets a window that looks like a phone screen,
//! where its views are drawn and clicks and typing reach its listeners.

#![no_std]
#![no_main]

extern crate alloc;

mod screen;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use justdex::{Host, Vm};
use justrt::{eprintln, println};

justrt::main!(main);

/// The interpreter's way out to JustOS.
struct JustHost;

impl Host for JustHost {
    fn write(&mut self, s: &str, _err: bool) {
        justrt::console::write(s);
    }
    fn read_line(&mut self) -> Option<String> {
        justrt::console::read_line()
    }
    fn time_ms(&mut self) -> u64 {
        justrt::time::now() * 1000 + justrt::time::uptime_ms() % 1000
    }
    fn uptime_ms(&mut self) -> u64 {
        justrt::time::uptime_ms()
    }
    fn sleep(&mut self, ms: u64) {
        justrt::time::sleep_ms(ms);
    }
}

fn main(args: &str) -> i32 {
    let mut parts = justrt::split_args(args.trim());
    let mut mode = None;
    while let Some(first) = parts.first() {
        match first.as_str() {
            "--window" | "--app" => mode = Some(true),
            "--console" => mode = Some(false),
            _ => break,
        }
        parts.remove(0);
    }
    if parts.is_empty() || parts[0] == "--help" {
        println!("usage: dalvik [--app|--console] <app.apk | program.dex | program.jar> [arguments]");
        println!("runs an Android app in a window, or a Java program in the terminal.");
        return if parts.is_empty() { 2 } else { 0 };
    }
    let path = parts.remove(0);
    let data = match justrt::fs::read(&path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("dalvik: {path}: {}", e.as_str());
            return 1;
        }
    };
    let dexes = match justdex::load(&data) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("dalvik: {path}: {e}");
            return 1;
        }
    };
    drop(data);
    let mut vm = Vm::new(dexes, Box::new(JustHost));
    let apk = path.to_ascii_lowercase().ends_with(".apk");
    let main_class = vm.main_class();
    let activity = vm.activity_class();
    let window = match (mode, &main_class, &activity) {
        (Some(w), _, _) => w,
        (None, Some(_), Some(_)) => apk,
        (None, _, Some(_)) => true,
        _ => false,
    };
    if window {
        let Some(class) = activity else {
            eprintln!("dalvik: {path}: no Activity to show");
            return 1;
        };
        let name = path.rsplit('/').next().unwrap_or(&path);
        let name = name.rsplit_once('.').map(|(n, _)| n).unwrap_or(name);
        return screen::run(&mut vm, &class, name);
    }
    let Some(class) = main_class else {
        eprintln!("dalvik: {path}: no class has a main(String[]) method");
        return 1;
    };
    let args: Vec<String> = parts;
    vm.run_main(&class, &args)
}

//! A system overview with memory bars.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use justrt::console::color::*;
use justrt::println;

justrt::main!(main);

fn size(bytes: u64) -> String {
    if bytes >= 1 << 30 {
        alloc::format!("{}.{} GiB", bytes >> 30, ((bytes % (1 << 30)) * 10) >> 30)
    } else {
        alloc::format!("{}.{} MiB", bytes >> 20, ((bytes % (1 << 20)) * 10) >> 20)
    }
}

fn bar(used: u64, total: u64, width: usize) -> String {
    let filled = if total == 0 { 0 } else { (used as u128 * width as u128 / total as u128) as usize };
    let mut s = String::from(ACCENT);
    for i in 0..width {
        s.push(if i < filled { '█' } else { '░' });
    }
    s.push_str(RESET);
    s
}

fn main(_: &str) -> i32 {
    let i = justrt::process::sysinfo();
    let used = i.memory_total - i.memory_free;
    let up = i.uptime_ms / 1000;
    println!();
    println!("  {WHITE}JustOS{ACCENT}█{RESET}   {DIM}system overview{RESET}");
    println!();
    println!("  memory    {}  {} of {}", bar(used, i.memory_total, 30), size(used), size(i.memory_total));
    println!("  devices   {}  {} of {} with a driver", bar(i.devices_with_driver, i.devices, 30), i.devices_with_driver, i.devices);
    println!("  disks     {}", i.disks);
    println!("  programs  {} running, {} threads", i.programs, i.threads);
    println!("  uptime    {}h {:02}m {:02}s", up / 3600, up / 60 % 60, up % 60);
    println!();
    let (cols, rows) = justrt::console::size();
    println!("  {DIM}console {cols}x{rows} · folder {}{RESET}", justrt::fs::current_dir());
    println!();
    0
}

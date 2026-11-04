//! Fixed hardware of a PC that no bus enumerates.
//!
//! The kernel owns the 8042 PS/2 controller (both keyboard and mouse sit
//! behind it) and publishes its ports as `ps2-keyboard` / `ps2-mouse`
//! platform devices for driver modules. Timer, interrupt controller, clock
//! and serial port are handled by the kernel itself and listed as built-in.

use crate::arch::cpu::{inb, outb};
use alloc::vec::Vec;
use arcdk::{DeviceInfo, Resource, BUS_PLATFORM, IRQ_NONE, RES_IO};
use core::sync::atomic::{AtomicU8, Ordering};

const DATA: u16 = 0x60;
const STATUS: u16 = 0x64;

fn wait_write() -> bool {
    for _ in 0..100_000 {
        if inb(STATUS) & 2 == 0 {
            return true;
        }
    }
    false
}

fn wait_read() -> bool {
    for _ in 0..100_000 {
        if inb(STATUS) & 1 != 0 {
            return true;
        }
    }
    false
}

fn command(cmd: u8) {
    wait_write();
    outb(STATUS, cmd);
}

fn command_read(cmd: u8) -> Option<u8> {
    command(cmd);
    wait_read().then(|| inb(DATA))
}

fn write_config(v: u8) {
    command(0x60);
    wait_write();
    outb(DATA, v);
}

struct Ps2 {
    keyboard: bool,
    mouse: bool,
}

/// 0 = not probed yet; otherwise 0x80 | mouse << 1 | keyboard.
static PS2: AtomicU8 = AtomicU8::new(0);

/// Initialises the 8042 once and reports which ports work.
fn ps2() -> Ps2 {
    let v = PS2.load(Ordering::Relaxed);
    if v != 0 {
        return Ps2 { keyboard: v & 1 != 0, mouse: v & 2 != 0 };
    }
    let r = init_8042();
    PS2.store(0x80 | (r.mouse as u8) << 1 | r.keyboard as u8, Ordering::Relaxed);
    r
}

/// After sleep: the controller is set up again (before the keyboard and
/// mouse drivers wake up their devices).
pub fn resume_ps2() {
    if PS2.load(Ordering::Relaxed) & 3 != 0 {
        init_8042();
    }
}

fn init_8042() -> Ps2 {
    let none = Ps2 { keyboard: false, mouse: false };
    if inb(STATUS) == 0xFF {
        return none; // no controller (floating bus)
    }
    command(0xAD); // disable port 1
    command(0xA7); // disable port 2
    for _ in 0..16 {
        if inb(STATUS) & 1 == 0 {
            break;
        }
        inb(DATA);
    }
    let Some(mut config) = command_read(0x20) else { return none };
    config &= !0b11; // no interrupts while testing
    write_config(config);
    if command_read(0xAA) != Some(0x55) {
        return none;
    }
    write_config(config); // the self-test may reset it
                          // A second port exists if enabling it clears the "port 2 clock off" bit.
    command(0xA8);
    let dual = command_read(0x20).is_some_and(|c| c & 0x20 == 0);
    command(0xA7);
    let keyboard = command_read(0xAB) == Some(0);
    let mouse = dual && command_read(0xA9) == Some(0);
    if keyboard {
        command(0xAE);
        config |= 1;
    }
    if mouse {
        command(0xA8);
        config |= 2;
    }
    config |= 0x40; // translate to scan code set 1
    write_config(config);
    Ps2 { keyboard, mouse }
}

fn device(name: &str, ports: &[u16], irq: u8) -> DeviceInfo {
    let mut info = DeviceInfo::empty();
    info.bus = BUS_PLATFORM;
    info.set_name(name);
    info.irq = irq;
    for (i, &p) in ports.iter().enumerate().take(6) {
        info.resources[i] = Resource { kind: RES_IO, flags: 0, base: p as u64, size: 1 };
    }
    info
}

/// Platform devices with the kernel component that handles them, if any.
pub fn detect() -> Vec<(DeviceInfo, Option<&'static str>)> {
    let mut out = Vec::new();
    out.push((device("pic", &[0x20, 0xA0], IRQ_NONE), Some("kernel: interrupts")));
    out.push((device("pit", &[0x40, 0x43], 0), Some("kernel: timer")));
    out.push((device("rtc", &[0x70, 0x71], IRQ_NONE), Some("kernel: clock")));
    if crate::console::serial::present() {
        out.push((device("com1", &[0x3F8], 4), Some("kernel: debug console")));
    }
    out.push((device("display", &[], IRQ_NONE), Some("kernel: console")));
    if let Some(line) = crate::acpi::sci_line() {
        out.push((device("acpi", &[], line), Some("kernel: ACPI")));
    }
    let ps2 = ps2();
    if ps2.keyboard {
        out.push((device("ps2-keyboard", &[DATA, STATUS], 1), None));
    }
    if ps2.mouse {
        out.push((device("ps2-mouse", &[DATA, STATUS], 12), None));
    }
    out
}

pub fn describe(name: &str) -> &'static str {
    match name {
        "pic" => "8259 interrupt controller",
        "pit" => "8254 system timer",
        "rtc" => "real-time clock",
        "com1" => "serial port COM1",
        "display" => "boot display",
        "acpi" => "ACPI events (power button, battery, lid)",
        "ps2-keyboard" => "PS/2 keyboard",
        "ps2-mouse" => "PS/2 mouse",
        _ => "platform device",
    }
}

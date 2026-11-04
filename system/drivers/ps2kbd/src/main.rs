//! PS/2 keyboard driver.
//!
//! The kernel initialises the 8042 controller with scan code set 1
//! translation; this driver turns scan codes into USB HID usages and reports
//! presses, repeats and releases.

#![no_std]
#![no_main]

use arcdrv::port;
use arcdrv::{DeviceInfo, Status, E_NODEV, KEY_PRESSED, KEY_RELEASED, KEY_REPEAT, OK};
use core::ffi::c_void;

arcdrv::driver! {
    name: "ps2kbd",
    version: "1.1.0",
    probe: probe,
    remove: remove,
}

const DATA: u16 = 0x60;
const STATUS: u16 = 0x64;

/// Scan code set 1 (make codes 0x00..0x58) to HID usage.
const NORMAL: [u8; 0x59] = [
    0x00, 0x29, 0x1E, 0x1F, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x2D, 0x2E, 0x2A, 0x2B, // 00
    0x14, 0x1A, 0x08, 0x15, 0x17, 0x1C, 0x18, 0x0C, 0x12, 0x13, 0x2F, 0x30, 0x28, 0xE0, 0x04, 0x16, // 10
    0x07, 0x09, 0x0A, 0x0B, 0x0D, 0x0E, 0x0F, 0x33, 0x34, 0x35, 0xE1, 0x31, 0x1D, 0x1B, 0x06, 0x19, // 20
    0x05, 0x11, 0x10, 0x36, 0x37, 0x38, 0xE5, 0x55, 0xE2, 0x2C, 0x39, 0x3A, 0x3B, 0x3C, 0x3D, 0x3E, // 30
    0x3F, 0x40, 0x41, 0x42, 0x43, 0x53, 0x47, 0x5F, 0x60, 0x61, 0x56, 0x5C, 0x5D, 0x5E, 0x57, 0x59, // 40
    0x5A, 0x5B, 0x62, 0x63, 0x00, 0x00, 0x64, 0x44, 0x45, // 50
];

/// Codes after the 0xE0 prefix.
fn extended(code: u8) -> u8 {
    match code {
        0x1C => 0x58, // keypad Enter
        0x1D => 0xE4, // right Ctrl
        0x35 => 0x54, // keypad /
        0x37 => 0x46, // Print Screen (after the fake shift E0 2A)
        0x38 => 0xE6, // right Alt (AltGr)
        0x47 => 0x4A, // Home
        0x48 => 0x52, // Up
        0x49 => 0x4B, // Page Up
        0x4B => 0x50, // Left
        0x4D => 0x4F, // Right
        0x4F => 0x4D, // End
        0x50 => 0x51, // Down
        0x51 => 0x4E, // Page Down
        0x52 => 0x49, // Insert
        0x53 => 0x4C, // Delete
        0x5B => 0xE3, // left GUI
        0x5C => 0xE7, // right GUI
        0x5D => 0x65, // menu
        _ => 0,
    }
}

struct State {
    device: u32,
    prefix: bool,
    pressed: [u64; 4],
}

static STATE: arcdrv::IrqLock<State> = arcdrv::IrqLock::new(State { device: 0, prefix: false, pressed: [0; 4] });

fn handle(code: u8) {
    let mut s = STATE.lock();
    if code == 0xE0 {
        s.prefix = true;
        return;
    }
    if code == 0xE1 || code == 0xFA || code == 0xFE || code == 0x00 || code == 0xFF {
        return; // Pause prefix, acknowledgements, errors
    }
    let release = code & 0x80 != 0;
    let make = code & 0x7F;
    let usage = if core::mem::replace(&mut s.prefix, false) { extended(make) } else { NORMAL.get(make as usize).copied().unwrap_or(0) };
    if usage == 0 {
        return;
    }
    let (word, bit) = ((usage / 64) as usize, 1u64 << (usage % 64));
    let value = if release {
        s.pressed[word] &= !bit;
        KEY_RELEASED
    } else if s.pressed[word] & bit != 0 {
        KEY_REPEAT
    } else {
        s.pressed[word] |= bit;
        KEY_PRESSED
    };
    let device = s.device;
    drop(s);
    arcdrv::input_event(device, arcdrv::INPUT_KEY, usage as u32, value);
}

unsafe extern "C" fn irq(_: *mut c_void) {
    // Read everything the controller holds for the keyboard port.
    for _ in 0..16 {
        let status = port::inb(STATUS);
        if status & 1 == 0 {
            break;
        }
        let data = port::inb(DATA);
        if status & 0x20 == 0 {
            handle(data);
        }
    }
}

fn send(byte: u8) -> bool {
    if !arcdrv::wait_until(50, || port::inb(STATUS) & 2 == 0) {
        return false;
    }
    port::outb(DATA, byte);
    arcdrv::wait_until(100, || port::inb(STATUS) & 1 != 0) && port::inb(DATA) == 0xFA
}

fn probe(dev: &DeviceInfo) -> Status {
    if dev.name() != "ps2-keyboard" {
        return E_NODEV;
    }
    while port::inb(STATUS) & 1 != 0 {
        port::inb(DATA);
    }
    // Enable scanning; a missing acknowledgement is not fatal (some
    // emulated controllers stay silent) but is worth a note.
    if !send(0xF4) {
        arcdrv::log(arcdrv::LOG_DEBUG, format_args!("no acknowledgement from the keyboard"));
    }
    STATE.lock().device = dev.id;
    let st = arcdrv::irq_attach(dev.id, dev.irq, irq, core::ptr::null_mut());
    if st != OK {
        return st;
    }
    arcdrv::power_hooks(dev.id, core::ptr::null_mut(), None, resume);
    arcdrv::info!("keyboard ready on IRQ {}", dev.irq);
    OK
}

/// After sleep (the kernel has set up the controller again): keys held
/// when the computer went to sleep are let go, and the keyboard scans
/// again.
unsafe extern "C" fn resume(_: *mut c_void) -> Status {
    let (device, held) = {
        let mut s = STATE.lock();
        s.prefix = false;
        (s.device, core::mem::take(&mut s.pressed))
    };
    for (word, bits) in held.iter().enumerate() {
        for bit in 0..64 {
            if bits & (1 << bit) != 0 {
                arcdrv::input_event(device, arcdrv::INPUT_KEY, (word * 64 + bit) as u32, KEY_RELEASED);
            }
        }
    }
    // The answers must not reach the interrupt handler.
    arcdrv::irq_detach(device, 1);
    while port::inb(STATUS) & 1 != 0 {
        port::inb(DATA);
    }
    send(0xF4);
    arcdrv::irq_attach(device, 1, irq, core::ptr::null_mut());
    OK
}

fn remove(device: u32) {
    arcdrv::irq_detach(device, 1);
}

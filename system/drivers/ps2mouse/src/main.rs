//! PS/2 mouse driver: three-byte packets, plus the wheel when the mouse
//! speaks the IntelliMouse protocol.

#![no_std]
#![no_main]

use justdrv::{port, DeviceInfo, IrqLock, Status, AXIS_WHEEL, AXIS_X, AXIS_Y, E_IO, E_NODEV, INPUT_BUTTON, INPUT_REL, OK};
use core::ffi::c_void;

justdrv::driver! {
    name: "ps2mouse",
    version: "1.1.0",
    probe: probe,
    remove: remove,
}

const DATA: u16 = 0x60;
const STATUS: u16 = 0x64;

struct State {
    device: u32,
    packet: [u8; 4],
    len: usize,
    size: usize,
    buttons: u8,
}

static STATE: IrqLock<State> = IrqLock::new(State { device: 0, packet: [0; 4], len: 0, size: 3, buttons: 0 });

fn write_mouse(byte: u8) -> Option<u8> {
    justdrv::wait_until(50, || port::inb(STATUS) & 2 == 0);
    port::outb(STATUS, 0xD4); // next data byte goes to the second port
    justdrv::wait_until(50, || port::inb(STATUS) & 2 == 0);
    port::outb(DATA, byte);
    justdrv::wait_until(100, || port::inb(STATUS) & 1 != 0).then(|| port::inb(DATA))
}

fn read_byte() -> Option<u8> {
    justdrv::wait_until(100, || port::inb(STATUS) & 1 != 0).then(|| port::inb(DATA))
}

fn packet(s: &mut State) {
    let p = s.packet;
    if p[0] & 0xC0 != 0 {
        return; // overflow: drop the packet
    }
    let dx = p[1] as i32 - if p[0] & 0x10 != 0 { 256 } else { 0 };
    let dy = p[2] as i32 - if p[0] & 0x20 != 0 { 256 } else { 0 };
    let dev = s.device;
    if dx != 0 {
        justdrv::input_event(dev, INPUT_REL, AXIS_X, dx);
    }
    if dy != 0 {
        justdrv::input_event(dev, INPUT_REL, AXIS_Y, -dy); // PS/2 counts up, screens count down
    }
    if s.size == 4 && p[3] != 0 {
        justdrv::input_event(dev, INPUT_REL, AXIS_WHEEL, (p[3] as i8 as i32).clamp(-8, 7));
    }
    let buttons = p[0] & 7;
    for (bit, code) in [(1u8, 0u32), (2, 1), (4, 2)] {
        if (buttons ^ s.buttons) & bit != 0 {
            justdrv::input_event(dev, INPUT_BUTTON, code, (buttons & bit != 0) as i32);
        }
    }
    s.buttons = buttons;
}

unsafe extern "C" fn irq(_: *mut c_void) {
    for _ in 0..16 {
        let status = port::inb(STATUS);
        if status & 1 == 0 {
            break;
        }
        let byte = port::inb(DATA);
        if status & 0x20 == 0 {
            continue; // keyboard data
        }
        let mut s = STATE.lock();
        if s.len == 0 && byte & 0x08 == 0 {
            continue; // out of sync: the first byte always has bit 3 set
        }
        let n = s.len;
        s.packet[n] = byte;
        s.len += 1;
        if s.len == s.size {
            s.len = 0;
            packet(&mut s);
        }
    }
}

/// Defaults, the wheel if the mouse has one, reporting on. The packet
/// size, or None if no mouse answers.
fn setup() -> Option<usize> {
    if write_mouse(0xF6) != Some(0xFA) {
        return None;
    }
    // IntelliMouse detection: sample rates 200, 100, 80, then read the id.
    let mut size = 3;
    if [200u8, 100, 80].iter().all(|&r| write_mouse(0xF3) == Some(0xFA) && write_mouse(r) == Some(0xFA))
        && write_mouse(0xF2) == Some(0xFA)
        && read_byte() == Some(3)
    {
        size = 4;
    }
    write_mouse(0xF4); // start reporting
    Some(size)
}

fn probe(dev: &DeviceInfo) -> Status {
    if dev.name() != "ps2-mouse" {
        return E_NODEV;
    }
    let Some(size) = setup() else {
        return E_IO; // no mouse answers on the port
    };
    {
        let mut s = STATE.lock();
        s.device = dev.id;
        s.size = size;
    }
    let st = justdrv::irq_attach(dev.id, dev.irq, irq, core::ptr::null_mut());
    if st != OK {
        return st;
    }
    justdrv::power_hooks(dev.id, core::ptr::null_mut(), None, resume);
    justdrv::info!("mouse ready on IRQ {}{}", dev.irq, if size == 4 { ", with wheel" } else { "" });
    OK
}

/// After sleep (the kernel has set up the controller again): the mouse is
/// set up as at the start, with its interrupt off meanwhile so the answers
/// come here.
unsafe extern "C" fn resume(_: *mut c_void) -> Status {
    let device = STATE.lock().device;
    justdrv::irq_detach(device, 12);
    while port::inb(STATUS) & 1 != 0 {
        port::inb(DATA);
    }
    let size = setup();
    {
        let mut s = STATE.lock();
        s.len = 0;
        if let Some(size) = size {
            s.size = size;
        }
        if s.buttons != 0 {
            for (bit, code) in [(1u8, 0u32), (2, 1), (4, 2)] {
                if s.buttons & bit != 0 {
                    justdrv::input_event(device, INPUT_BUTTON, code, 0);
                }
            }
            s.buttons = 0;
        }
    }
    justdrv::irq_attach(device, 12, irq, core::ptr::null_mut());
    if size.is_some() {
        OK
    } else {
        E_IO
    }
}

fn remove(device: u32) {
    write_mouse(0xF5); // stop reporting
    justdrv::irq_detach(device, 12);
}

//! USB HID driver: keyboards, mice, trackballs, touchpads and tablets.
//!
//! Reads the device's report descriptor (`justdk::hid`) to learn where keys,
//! buttons and axes sit in its reports, then has the host controller poll
//! the interrupt endpoint and turns every report into input events. USB
//! keyboards do not repeat keys by themselves: a small thread per keyboard
//! does it (500 ms delay, 30 per second).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;
use justdrv::hid::{self, Field};
use justdrv::usb::*;
use justdrv::{DeviceInfo, IrqLock, Status, ABS_MAX, AXIS_WHEEL, AXIS_X, AXIS_Y, E_NODEV, INPUT_ABS, INPUT_BUTTON, INPUT_KEY, INPUT_REL};
use justdrv::{KEY_PRESSED, KEY_RELEASED, KEY_REPEAT, OK};
use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, Ordering};

justdrv::driver! {
    name: "usbhid",
    version: "1.0.0",
    probe: probe,
    remove: remove,
}

const REQ_SET_IDLE: u8 = 0x0A;
const REQ_SET_PROTOCOL: u8 = 0x0B;
const REPEAT_DELAY_MS: u64 = 500;
const REPEAT_EVERY_MS: u64 = 33;

/// Report descriptors of the boot protocol, for devices whose own
/// descriptor cannot be used.
const BOOT_KEYBOARD: &[u8] = &[
    0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0x75, 0x01, 0x95, 0x08, 0x05, 0x07, 0x19, 0xe0, 0x29, 0xe7, 0x15, 0x00, 0x25, 0x01, 0x81, 0x02,
    0x95, 0x01, 0x75, 0x08, 0x81, 0x01, 0x95, 0x06, 0x75, 0x08, 0x15, 0x00, 0x25, 0xff, 0x05, 0x07, 0x19, 0x00, 0x29, 0xff, 0x81, 0x00,
    0xc0,
];
const BOOT_MOUSE: &[u8] = &[
    0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x09, 0x01, 0xa1, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29, 0x03, 0x15, 0x00, 0x25, 0x01, 0x95, 0x03,
    0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x05, 0x81, 0x01, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x81, 0x25, 0x7f, 0x75, 0x08,
    0x95, 0x02, 0x81, 0x06, 0xc0, 0xc0,
];

#[derive(Default)]
struct Keys {
    held: Vec<u8>,
    buttons: u8,
    /// Key being repeated and when its next repeat is due.
    repeat: Option<(u8, u64)>,
}

struct Hid {
    device: u32,
    iface: &'static UsbInterface,
    endpoint: u8,
    fields: Vec<Field>,
    keys: IrqLock<Keys>,
    keyboard: bool,
    /// Set by `remove`; the repeat thread then ends and sets `stopped`.
    dead: AtomicBool,
    stopped: AtomicBool,
}

static DEVICES: IrqLock<Vec<(u32, usize)>> = IrqLock::new(Vec::new());

unsafe extern "C" fn report(ctx: *mut c_void, data: *const u8, len: u32) {
    // SAFETY: the Hid this driver passed to poll_start; alive until remove
    // returned, which waits for the poll to stop.
    let h = unsafe { &*(ctx as *const Hid) };
    // SAFETY: the controller passes `len` bytes.
    let data = unsafe { core::slice::from_raw_parts(data, len as usize) };
    h.handle(data);
}

impl Hid {
    fn handle(&self, data: &[u8]) {
        let r = hid::decode(&self.fields, data, ABS_MAX);
        let dev = self.device;
        if r.has_keys {
            let mut k = self.keys.lock();
            let now = justdrv::time_ms();
            let pressed: Vec<u8> = r.keys.iter().copied().filter(|c| !k.held.contains(c)).collect();
            let released: Vec<u8> = k.held.iter().copied().filter(|c| !r.keys.contains(c)).collect();
            for &c in &pressed {
                if c < 0xE0 {
                    k.repeat = Some((c, now + REPEAT_DELAY_MS));
                }
            }
            if k.repeat.is_some_and(|(c, _)| released.contains(&c)) {
                k.repeat = None;
            }
            k.held = r.keys.clone();
            drop(k);
            for c in released {
                justdrv::input_event(dev, INPUT_KEY, c as u32, KEY_RELEASED);
            }
            for c in pressed {
                justdrv::input_event(dev, INPUT_KEY, c as u32, KEY_PRESSED);
            }
        }
        if r.has_pointer {
            if r.dx != 0 {
                justdrv::input_event(dev, INPUT_REL, AXIS_X, r.dx);
            }
            if r.dy != 0 {
                justdrv::input_event(dev, INPUT_REL, AXIS_Y, r.dy);
            }
            if r.wheel != 0 {
                // HID wheels count up away from the user; screens scroll down.
                justdrv::input_event(dev, INPUT_REL, AXIS_WHEEL, -r.wheel);
            }
            if let Some(x) = r.abs_x {
                justdrv::input_event(dev, INPUT_ABS, AXIS_X, x);
            }
            if let Some(y) = r.abs_y {
                justdrv::input_event(dev, INPUT_ABS, AXIS_Y, y);
            }
            let changed = {
                let mut k = self.keys.lock();
                let c = k.buttons ^ r.buttons;
                k.buttons = r.buttons;
                c
            };
            for b in 0..3 {
                if changed & (1 << b) != 0 {
                    justdrv::input_event(dev, INPUT_BUTTON, b, (r.buttons >> b & 1) as i32);
                }
            }
        }
    }

    fn control(&self, request: u8, value: u16) -> Status {
        let setup = Setup::new(TYPE_CLASS | RECIP_INTERFACE, request, value, self.iface.number as u16, 0);
        // SAFETY: the interface of a present device.
        unsafe { (self.iface.control)(self.iface.ctx, &setup, core::ptr::null_mut(), 1000) }
    }
}

unsafe extern "C" fn repeater(ctx: *mut c_void) {
    // SAFETY: the Hid of a keyboard; it stays alive until `stopped` is set.
    let h = unsafe { &*(ctx as *const Hid) };
    while !h.dead.load(Ordering::Acquire) {
        justdrv::sleep_ms(10);
        let now = justdrv::time_ms();
        let due = {
            let mut k = h.keys.lock();
            match k.repeat {
                Some((c, at)) if now >= at && k.held.contains(&c) => {
                    k.repeat = Some((c, now + REPEAT_EVERY_MS));
                    Some(c)
                }
                _ => None,
            }
        };
        if let Some(c) = due {
            justdrv::input_event(h.device, INPUT_KEY, c as u32, KEY_REPEAT);
        }
    }
    h.stopped.store(true, Ordering::Release);
}

fn probe(dev: &DeviceInfo) -> Status {
    if dev.interface == 0 || dev.class != CLASS_HID {
        return E_NODEV;
    }
    // SAFETY: the host controller driver keeps the interface alive while
    // the device exists.
    let iface: &'static UsbInterface = unsafe { &*(dev.interface as *const UsbInterface) };
    // SAFETY: as above.
    let config = unsafe { iface.config() };
    let number = iface.number;
    let Some(ep) = endpoints(config, number, 0).find(|e| e.is_in() && e.kind() == EP_INTERRUPT) else { return E_NODEV };
    let desc_len = interface_descriptors(config, number, 0)
        .find_map(|d| match d {
            Descriptor::Other(DESC_HID, b) if b.len() >= 9 && b[6] == DESC_REPORT => Some(u16::from_le_bytes([b[7], b[8]])),
            _ => None,
        })
        .unwrap_or(0);

    let mut hid = Hid {
        device: dev.id,
        iface,
        endpoint: ep.address,
        fields: Vec::new(),
        keys: IrqLock::new(Keys::default()),
        keyboard: false,
        dead: AtomicBool::new(false),
        stopped: AtomicBool::new(false),
    };
    // Reports only when something changes.
    hid.control(REQ_SET_IDLE, 0);

    let boot = dev.subclass == 1;
    if desc_len > 0 {
        let mut desc = alloc::vec![0u8; desc_len.min(4096) as usize];
        let get = Setup::new(DIR_IN | RECIP_INTERFACE, REQ_GET_DESCRIPTOR, (DESC_REPORT as u16) << 8, number as u16, desc.len() as u16);
        // SAFETY: the interface's own control function, with a buffer of
        // `length` bytes.
        let n = unsafe { (iface.control)(iface.ctx, &get, desc.as_mut_ptr(), 2000) };
        if n > 0 {
            if boot {
                hid.control(REQ_SET_PROTOCOL, 1); // report protocol
            }
            hid.fields = hid::parse(&desc[..n as usize]);
        }
    }
    let usable = |f: &[Field]| {
        f.iter().any(|x| x.page == hid::PAGE_KEYBOARD || x.page == hid::PAGE_BUTTON || (x.page == hid::PAGE_DESKTOP && x.variable))
    };
    if !usable(&hid.fields) {
        // Fall back to the boot protocol, whose reports are fixed.
        match (boot, dev.prog_if) {
            (true, 1) => hid.fields = hid::parse(BOOT_KEYBOARD),
            (true, 2) => hid.fields = hid::parse(BOOT_MOUSE),
            _ => return E_NODEV, // nothing the input layer understands
        }
        hid.control(REQ_SET_PROTOCOL, 0);
    }
    hid.keyboard = hid.fields.iter().any(|f| f.page == hid::PAGE_KEYBOARD || f.usage(0).0 == hid::PAGE_KEYBOARD);
    let absolute = hid.fields.iter().any(|f| f.page == hid::PAGE_DESKTOP && !f.relative && f.usage(0).1 == hid::USAGE_X);
    let pointer = hid.fields.iter().any(|f| f.page == hid::PAGE_BUTTON || (f.page == hid::PAGE_DESKTOP && f.usage(0).1 == hid::USAGE_X));

    let h: &'static Hid = Box::leak(Box::new(hid));
    let ctx = h as *const Hid as *mut c_void;
    // SAFETY: the interface's own function; `h` outlives the poll.
    let st = unsafe { (iface.poll_start)(iface.ctx, h.endpoint, report, ctx) };
    if st != OK {
        // SAFETY: never shared: the poll did not start.
        drop(unsafe { Box::from_raw(ctx as *mut Hid) });
        return st;
    }
    if h.keyboard {
        justdrv::spawn("usb-keyrepeat", repeater, ctx);
    } else {
        h.stopped.store(true, Ordering::Release);
    }
    DEVICES.lock().push((dev.id, ctx as usize));
    let what = match (h.keyboard, pointer, absolute) {
        (true, true, _) => "keyboard and pointer",
        (true, false, _) => "keyboard",
        (false, _, true) => "tablet",
        (false, _, false) => "mouse",
    };
    justdrv::info!("{what} ready ({} report fields)", h.fields.len());
    OK
}

fn remove(device: u32) {
    let ptr = {
        let mut d = DEVICES.lock();
        let Some(i) = d.iter().position(|(id, _)| *id == device) else { return };
        d.remove(i).1
    };
    // SAFETY: the Hid leaked in probe.
    let h = unsafe { &*(ptr as *const Hid) };
    h.dead.store(true, Ordering::Release);
    // SAFETY: the interface stays valid during remove; stopping waits for a
    // running callback.
    unsafe { (h.iface.poll_stop)(h.iface.ctx, h.endpoint) };
    while !h.stopped.load(Ordering::Acquire) {
        justdrv::sleep_ms(5);
    }
    // Keys still held are released, so nothing stays stuck.
    let held = core::mem::take(&mut h.keys.lock().held);
    for c in held {
        justdrv::input_event(device, INPUT_KEY, c as u32, KEY_RELEASED);
    }
    // SAFETY: no callback or thread uses it any more.
    drop(unsafe { Box::from_raw(ptr as *mut Hid) });
}

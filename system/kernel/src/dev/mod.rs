//! Devices: everything the kernel found on the machine, and which driver
//! owns each one.
//!
//! Devices come from bus scans (PCI), from platform detection (PS/2
//! controller and other fixed hardware) and from drivers: a USB controller
//! driver adds the devices plugged into it as its children. Drivers are
//! attached to them by [`crate::driver`], automatically.
//!
//! Device ids are never reused: an unplugged device keeps its slot, marked
//! gone, so a stale id can never reach a newer device.

pub mod audio;
pub mod block;
pub mod display;
pub mod input;
pub mod irq;
pub mod keymap;
pub mod net;
pub mod pci;
pub mod platform;
pub mod pm;

use crate::sync::IrqLock;
use alloc::string::String;
use alloc::vec::Vec;
use justdk::{DeviceInfo, BUS_PCI, BUS_USB};

#[derive(Clone)]
pub struct Device {
    pub info: DeviceInfo,
    /// Driver module bound to the device.
    pub driver: Option<String>,
    /// Kernel component that handles the device itself (no module needed).
    pub builtin: Option<&'static str>,
    /// Why the last attempt to find a driver failed, if it did.
    pub problem: Option<String>,
    /// What the parent driver called it (USB product name).
    pub name: Option<String>,
    /// The parent's driver handles the device itself (USB hub).
    pub owned: bool,
    /// Unplugged.
    pub gone: bool,
}

impl Device {
    pub fn description(&self) -> String {
        if let Some(n) = &self.name {
            return n.clone();
        }
        if self.info.bus == BUS_USB {
            return String::from(usb_class_name(self.info.class, self.info.subclass, self.info.prog_if));
        }
        if self.info.bus == BUS_PCI {
            let class = pci::class_name(self.info.class, self.info.subclass, self.info.prog_if);
            match pci::vendor_name(self.info.vendor) {
                Some(v) => alloc::format!("{v} {class}"),
                None => String::from(class),
            }
        } else {
            String::from(platform::describe(self.info.name()))
        }
    }

    /// "pci 00:1f.2", "usb 3-1.2:0" or "platform ps2-keyboard"
    pub fn location(&self) -> String {
        match self.info.bus {
            BUS_PCI => {
                let l = self.info.location;
                alloc::format!("pci {:02x}:{:02x}.{}", l >> 8, (l >> 3) & 0x1F, l & 7)
            }
            BUS_USB => alloc::format!("usb {}", self.info.name()),
            justdk::BUS_SD => alloc::format!("sd {}", self.info.name()),
            _ => alloc::format!("platform {}", self.info.name()),
        }
    }

    pub fn has_driver(&self) -> bool {
        self.driver.is_some() || self.builtin.is_some() || self.owned
    }

    /// Bridges and other devices that need no driver.
    pub fn needs_driver(&self) -> bool {
        !(self.info.bus == BUS_PCI
            && (self.info.class == 0x06 || (self.info.class == 0x05) || (self.info.class == 0x08 && self.info.subclass == 0x80)))
    }
}

/// Name of a USB interface class.
pub fn usb_class_name(class: u8, subclass: u8, protocol: u8) -> &'static str {
    match (class, subclass, protocol) {
        (0x01, _, _) => "USB audio",
        (0x02, _, _) => "USB communications",
        (0x03, 0x01, 0x01) => "USB keyboard",
        (0x03, 0x01, 0x02) => "USB mouse",
        (0x03, _, _) => "USB input device",
        (0x06, _, _) => "USB camera (still image)",
        (0x07, _, _) => "USB printer",
        (0x08, _, _) => "USB storage",
        (0x09, _, _) => "USB hub",
        (0x0A, _, _) => "USB data interface",
        (0x0B, _, _) => "USB smart card reader",
        (0x0E, _, _) => "USB video",
        (0xE0, 0x01, 0x01) => "Bluetooth adapter",
        (0xE0, _, _) => "USB wireless controller",
        (0xFF, _, _) => "USB vendor-specific device",
        _ => "USB device",
    }
}

static DEVICES: IrqLock<Vec<Device>> = IrqLock::new(Vec::new());

/// Adds a device unless one with the same bus location already exists.
/// Returns its id and whether it is new.
pub fn add(info: DeviceInfo, builtin: Option<&'static str>) -> (u32, bool) {
    add_full(info, builtin, None, false)
}

fn add_full(mut info: DeviceInfo, builtin: Option<&'static str>, name: Option<String>, owned: bool) -> (u32, bool) {
    let mut devs = DEVICES.lock();
    if let Some(d) = devs.iter().find(|d| {
        !d.gone && d.info.bus == info.bus && d.info.parent == info.parent && d.info.location == info.location && d.info.name == info.name
    }) {
        return (d.info.id, false);
    }
    info.id = devs.len() as u32 + 1;
    let id = info.id;
    devs.push(Device { info, driver: None, builtin, problem: None, name, owned, gone: false });
    (id, true)
}

/// A device a driver found (see `KernelApi::device_add`).
pub fn add_child(parent: u32, mut info: DeviceInfo, name: Option<String>, owned: bool) -> Option<u32> {
    get(parent)?;
    info.parent = parent;
    let (id, _) = add_full(info, None, name, owned);
    Some(id)
}

/// Marks a device unplugged. Returns its children, which go with it.
pub fn mark_gone(id: u32) -> Vec<u32> {
    let mut devs = DEVICES.lock();
    if let Some(d) = id.checked_sub(1).and_then(|i| devs.get_mut(i as usize)) {
        d.gone = true;
        d.driver = None;
    }
    devs.iter().filter(|d| !d.gone && d.info.parent == id && id != 0).map(|d| d.info.id).collect()
}

/// Present children of a device.
pub fn children(id: u32) -> Vec<u32> {
    DEVICES.lock().iter().filter(|d| !d.gone && d.info.parent == id && id != 0).map(|d| d.info.id).collect()
}

/// A present device.
pub fn get(id: u32) -> Option<Device> {
    DEVICES.lock().get(id.checked_sub(1)? as usize).filter(|d| !d.gone).cloned()
}

pub fn info(id: u32) -> Option<DeviceInfo> {
    get(id).map(|d| d.info)
}

pub fn set_driver(id: u32, driver: Option<&str>) {
    if let Some(d) = DEVICES.lock().get_mut(id as usize - 1).filter(|d| !d.gone) {
        d.driver = driver.map(String::from);
        if driver.is_some() {
            d.problem = None;
        }
    }
}

pub fn set_problem(id: u32, problem: Option<String>) {
    if let Some(d) = DEVICES.lock().get_mut(id as usize - 1) {
        d.problem = problem;
    }
}

/// Every present device.
pub fn list() -> Vec<Device> {
    DEVICES.lock().iter().filter(|d| !d.gone).cloned().collect()
}

/// Finds hardware: PCI functions and platform devices. Returns the ids of
/// devices seen for the first time.
pub fn scan() -> Vec<u32> {
    let mut new = Vec::new();
    for info in pci::scan() {
        let (id, fresh) = add(info, None);
        if fresh {
            new.push(id);
        }
    }
    for (info, builtin) in platform::detect() {
        let (id, fresh) = add(info, builtin);
        if fresh {
            new.push(id);
        }
    }
    new
}

//! USB: what a host controller driver offers the drivers of the devices it
//! finds, plus descriptor parsing shared by both sides.
//!
//! A host controller driver (xhci, ehci, uhci, ohci) enumerates the devices
//! on its ports and hubs and adds every **interface** of every device to the
//! kernel as a [`crate::BUS_USB`] device. Its `DeviceInfo::interface` points
//! at a [`UsbInterface`]: the configuration descriptor and the transfer
//! functions the interface's driver uses. Hubs are handled by the host
//! controller driver itself.

use crate::abi::Status;
use core::ffi::c_void;

pub const SPEED_LOW: u8 = 1;
pub const SPEED_FULL: u8 = 2;
pub const SPEED_HIGH: u8 = 3;
pub const SPEED_SUPER: u8 = 4;

pub fn speed_name(speed: u8) -> &'static str {
    match speed {
        SPEED_LOW => "1.5 Mbit/s",
        SPEED_FULL => "12 Mbit/s",
        SPEED_HIGH => "480 Mbit/s",
        SPEED_SUPER => "5 Gbit/s",
        _ => "?",
    }
}

// Descriptor types
pub const DESC_DEVICE: u8 = 1;
pub const DESC_CONFIG: u8 = 2;
pub const DESC_STRING: u8 = 3;
pub const DESC_INTERFACE: u8 = 4;
pub const DESC_ENDPOINT: u8 = 5;
pub const DESC_HID: u8 = 0x21;
pub const DESC_REPORT: u8 = 0x22;
pub const DESC_HUB: u8 = 0x29;
pub const DESC_SS_HUB: u8 = 0x2A;

// Standard requests
pub const REQ_GET_STATUS: u8 = 0;
pub const REQ_CLEAR_FEATURE: u8 = 1;
pub const REQ_SET_FEATURE: u8 = 3;
pub const REQ_SET_ADDRESS: u8 = 5;
pub const REQ_GET_DESCRIPTOR: u8 = 6;
pub const REQ_SET_CONFIGURATION: u8 = 9;
pub const REQ_SET_INTERFACE: u8 = 11;

// bmRequestType
pub const DIR_IN: u8 = 0x80;
pub const TYPE_CLASS: u8 = 0x20;
pub const TYPE_VENDOR: u8 = 0x40;
pub const RECIP_DEVICE: u8 = 0;
pub const RECIP_INTERFACE: u8 = 1;
pub const RECIP_ENDPOINT: u8 = 2;
pub const RECIP_OTHER: u8 = 3;

pub const CLASS_HID: u8 = 3;
pub const CLASS_STORAGE: u8 = 8;
pub const CLASS_HUB: u8 = 9;

/// Endpoint transfer types (bits 0-1 of bmAttributes).
pub const EP_CONTROL: u8 = 0;
pub const EP_ISOCH: u8 = 1;
pub const EP_BULK: u8 = 2;
pub const EP_INTERRUPT: u8 = 3;

/// A SETUP packet.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Setup {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub length: u16,
}

impl Setup {
    pub const fn new(request_type: u8, request: u8, value: u16, index: u16, length: u16) -> Setup {
        Setup { request_type, request, value, index, length }
    }

    pub fn to_bytes(self) -> [u8; 8] {
        let v = self.value.to_le_bytes();
        let i = self.index.to_le_bytes();
        let l = self.length.to_le_bytes();
        [self.request_type, self.request, v[0], v[1], i[0], i[1], l[0], l[1]]
    }

    pub fn is_in(&self) -> bool {
        self.request_type & DIR_IN != 0
    }

    /// GET_DESCRIPTOR of the device.
    pub const fn get_descriptor(kind: u8, index: u8, length: u16) -> Setup {
        Setup::new(DIR_IN, REQ_GET_DESCRIPTOR, (kind as u16) << 8 | index as u16, 0, length)
    }
}

/// The device descriptor.
#[derive(Clone, Copy, Debug, Default)]
pub struct DeviceDescriptor {
    pub usb_version: u16,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub max_packet0: u8,
    pub vendor: u16,
    pub product: u16,
    pub release: u16,
    pub manufacturer_string: u8,
    pub product_string: u8,
    pub serial_string: u8,
    pub configurations: u8,
}

impl DeviceDescriptor {
    pub fn parse(b: &[u8]) -> Option<DeviceDescriptor> {
        if b.len() < 18 || b[1] != DESC_DEVICE {
            return None;
        }
        let w = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
        Some(DeviceDescriptor {
            usb_version: w(2),
            class: b[4],
            subclass: b[5],
            protocol: b[6],
            max_packet0: b[7],
            vendor: w(8),
            product: w(10),
            release: w(12),
            manufacturer_string: b[14],
            product_string: b[15],
            serial_string: b[16],
            configurations: b[17],
        })
    }
}

/// An interface descriptor inside a configuration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InterfaceDescriptor {
    pub number: u8,
    pub alternate: u8,
    pub endpoints: u8,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub string: u8,
}

/// An endpoint descriptor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EndpointDescriptor {
    /// Number in bits 0-3, direction in bit 7 (set = IN).
    pub address: u8,
    pub attributes: u8,
    pub max_packet: u16,
    pub interval: u8,
}

impl EndpointDescriptor {
    pub fn kind(&self) -> u8 {
        self.attributes & 3
    }

    pub fn is_in(&self) -> bool {
        self.address & 0x80 != 0
    }

    pub fn number(&self) -> u8 {
        self.address & 0x0F
    }

    /// Packet size without the high-bandwidth multiplier bits.
    pub fn packet_size(&self) -> u16 {
        self.max_packet & 0x7FF
    }
}

/// One descriptor of a configuration, as found while walking it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Descriptor<'a> {
    Interface(InterfaceDescriptor),
    Endpoint(EndpointDescriptor),
    /// Anything else: (type, the whole descriptor).
    Other(u8, &'a [u8]),
}

/// Walks the descriptors of a configuration (after the 9-byte header).
pub fn descriptors(config: &[u8]) -> impl Iterator<Item = Descriptor<'_>> {
    let mut at = if config.len() >= 9 && config[1] == DESC_CONFIG { config[0] as usize } else { config.len() };
    core::iter::from_fn(move || {
        if at + 2 > config.len() {
            return None;
        }
        let len = config[at] as usize;
        if len < 2 || at + len > config.len() {
            return None;
        }
        let d = &config[at..at + len];
        at += len;
        Some(match d[1] {
            DESC_INTERFACE if len >= 9 => Descriptor::Interface(InterfaceDescriptor {
                number: d[2],
                alternate: d[3],
                endpoints: d[4],
                class: d[5],
                subclass: d[6],
                protocol: d[7],
                string: d[8],
            }),
            DESC_ENDPOINT if len >= 7 => Descriptor::Endpoint(EndpointDescriptor {
                address: d[2],
                attributes: d[3],
                max_packet: u16::from_le_bytes([d[4], d[5]]),
                interval: d[6],
            }),
            t => Descriptor::Other(t, d),
        })
    })
}

/// `bConfigurationValue` of a configuration descriptor.
pub fn config_value(config: &[u8]) -> u8 {
    if config.len() >= 9 {
        config[5]
    } else {
        0
    }
}

/// The interfaces of a configuration (alternate setting 0 only).
pub fn interfaces(config: &[u8]) -> impl Iterator<Item = InterfaceDescriptor> + '_ {
    descriptors(config).filter_map(|d| match d {
        Descriptor::Interface(i) if i.alternate == 0 => Some(i),
        _ => None,
    })
}

/// The descriptors that belong to an interface (alternate setting `alt`):
/// everything between its interface descriptor and the next one.
pub fn interface_descriptors(config: &[u8], number: u8, alt: u8) -> impl Iterator<Item = Descriptor<'_>> {
    let mut inside = false;
    descriptors(config).filter(move |d| match d {
        Descriptor::Interface(i) => {
            inside = i.number == number && i.alternate == alt;
            false
        }
        _ => inside,
    })
}

/// The endpoints of an interface.
pub fn endpoints(config: &[u8], number: u8, alt: u8) -> impl Iterator<Item = EndpointDescriptor> + '_ {
    interface_descriptors(config, number, alt).filter_map(|d| match d {
        Descriptor::Endpoint(e) => Some(e),
        _ => None,
    })
}

/// Decodes a string descriptor (UTF-16LE) into `out`; returns the length.
pub fn decode_string(desc: &[u8], out: &mut [u8]) -> usize {
    if desc.len() < 2 || desc[1] != DESC_STRING {
        return 0;
    }
    let len = (desc[0] as usize).min(desc.len());
    let units = desc[2..len].as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c));
    let mut n = 0;
    for c in char::decode_utf16(units) {
        let c = c.unwrap_or('?');
        let mut buf = [0u8; 4];
        let s = c.encode_utf8(&mut buf);
        if n + s.len() > out.len() {
            break;
        }
        out[n..n + s.len()].copy_from_slice(s.as_bytes());
        n += s.len();
    }
    // Trim trailing spaces and NULs some devices pad with.
    while n > 0 && (out[n - 1] == b' ' || out[n - 1] == 0) {
        n -= 1;
    }
    n
}

// ----------------------------------------------------------------------------
// The interface a host controller driver offers

/// Control transfer on endpoint 0. `data` holds `setup.length` bytes.
/// Returns the bytes transferred or an error.
pub type UsbControl = unsafe extern "C" fn(ctx: *mut c_void, setup: *const Setup, data: *mut u8, timeout_ms: u32) -> Status;
/// Bulk (or interrupt) transfer on `endpoint` (address with direction bit).
/// Returns the bytes transferred or an error.
pub type UsbTransfer = unsafe extern "C" fn(ctx: *mut c_void, endpoint: u8, data: *mut u8, len: u32, timeout_ms: u32) -> Status;
/// Called with every report an interrupt IN endpoint delivers.
pub type UsbReport = unsafe extern "C" fn(cb_ctx: *mut c_void, data: *const u8, len: u32);
/// Keeps polling an interrupt IN endpoint, calling `report` with each
/// packet (from the controller's thread), until `poll_stop`.
pub type UsbPollStart = unsafe extern "C" fn(ctx: *mut c_void, endpoint: u8, report: UsbReport, cb_ctx: *mut c_void) -> Status;
pub type UsbEndpointOp = unsafe extern "C" fn(ctx: *mut c_void, endpoint: u8) -> Status;

/// One interface of a USB device, as handed to its driver.
#[repr(C)]
pub struct UsbInterface {
    /// The host controller's handle for the device.
    pub ctx: *mut c_void,
    /// `SPEED_*`.
    pub speed: u8,
    /// `bInterfaceNumber`.
    pub number: u8,
    /// Address on the bus.
    pub address: u8,
    pub _pad: u8,
    /// The whole configuration descriptor (all interfaces and endpoints).
    pub config: *const u8,
    pub config_len: u32,
    pub _pad2: u32,
    pub control: UsbControl,
    pub transfer: UsbTransfer,
    pub poll_start: UsbPollStart,
    pub poll_stop: UsbEndpointOp,
    /// Clears a halted (stalled) endpoint on both sides.
    pub clear_halt: UsbEndpointOp,
}

// SAFETY: the host controller driver serialises access behind `ctx`.
unsafe impl Send for UsbInterface {}
unsafe impl Sync for UsbInterface {}

impl UsbInterface {
    /// The configuration descriptor.
    ///
    /// # Safety
    /// The interface must still belong to a present device.
    pub unsafe fn config(&self) -> &[u8] {
        // SAFETY: guaranteed by the caller; the host driver keeps it alive.
        unsafe { core::slice::from_raw_parts(self.config, self.config_len as usize) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A boot keyboard: config, interface 0 (HID boot keyboard), HID
    // descriptor, interrupt IN endpoint 1.
    const CONFIG: [u8; 34] = [
        9, 2, 34, 0, 1, 1, 0, 0xA0, 50, // configuration
        9, 4, 0, 0, 1, 3, 1, 1, 0, // interface
        9, 0x21, 0x11, 1, 0, 1, 0x22, 63, 0, // HID
        7, 5, 0x81, 3, 8, 0, 10, // endpoint
    ];

    #[test]
    fn walks_config() {
        assert_eq!(config_value(&CONFIG), 1);
        let ifs: Vec<_> = interfaces(&CONFIG).collect();
        assert_eq!(ifs.len(), 1);
        assert_eq!((ifs[0].class, ifs[0].subclass, ifs[0].protocol), (3, 1, 1));
        let eps: Vec<_> = endpoints(&CONFIG, 0, 0).collect();
        assert_eq!(eps.len(), 1);
        assert!(eps[0].is_in());
        assert_eq!(eps[0].kind(), EP_INTERRUPT);
        assert_eq!(eps[0].packet_size(), 8);
        let hid = interface_descriptors(&CONFIG, 0, 0).find(|d| matches!(d, Descriptor::Other(DESC_HID, _)));
        assert!(hid.is_some());
        assert_eq!(endpoints(&CONFIG, 1, 0).count(), 0);
    }

    #[test]
    fn strings() {
        let d = [10, 3, b'Q', 0, b'E', 0, b'M', 0, b' ', 0];
        let mut out = [0u8; 16];
        let n = decode_string(&d, &mut out);
        assert_eq!(&out[..n], b"QEM");
    }

    #[test]
    fn setup_bytes() {
        let s = Setup::get_descriptor(DESC_DEVICE, 0, 18);
        assert_eq!(s.to_bytes(), [0x80, 6, 0, 1, 0, 0, 18, 0]);
    }
}

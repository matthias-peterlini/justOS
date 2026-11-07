//! The USB core shared by justOS host controller drivers.
//!
//! A host controller driver implements [`Host`] (ports, addressing,
//! transfers) and hands it to [`start`]. The core then does everything that
//! is the same on every controller:
//!
//! * watches the root ports and the ports of every hub, every 250 ms and
//!   whenever the controller reports a port change;
//! * resets and enumerates new devices: address, descriptors, strings,
//!   configuration;
//! * runs hubs (USB 2 and USB 3), so devices behind any number of hubs work;
//! * adds every interface of every device to the kernel as a USB device,
//!   whose `interface` is a [`UsbInterface`]: the kernel then installs the
//!   right driver (keyboard, disk...) from its manifests;
//! * removes devices (and everything behind a hub) when they are unplugged.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use justdrv::usb::*;
use justdrv::{DeviceInfo, Event, IrqLock, Mutex, Status, BUS_USB, DEVICE_OWNED, E_GONE, E_INVAL, E_IO, OK};
use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, Ordering};

pub use justdrv::usb;

/// A host controller's handle for one device.
pub type Handle = u32;

/// Where a new device sits.
#[derive(Clone, Copy, Debug, Default)]
pub struct Attach {
    pub speed: u8,
    /// Root port (1-based) the device or its hub chain hangs off.
    pub root_port: u8,
    /// xHCI route string: one nibble per hub below the root port.
    pub route: u32,
    /// Hubs between the device and the root port.
    pub depth: u8,
    /// The hub the device is plugged into, and its port.
    pub hub: Option<(Handle, u8)>,
    /// For a low/full-speed device behind a high-speed hub: that hub's
    /// transaction translator (hub handle, port, multi-TT).
    pub tt: Option<(Handle, u8, bool)>,
}

/// What `configure` needs to know about a hub.
#[derive(Clone, Copy, Debug)]
pub struct HubInfo {
    pub ports: u8,
    /// TT think time, 0-3 (8 to 32 FS bit times).
    pub think_time: u8,
    pub multi_tt: bool,
}

/// State of a root port.
#[derive(Clone, Copy, Debug, Default)]
pub struct PortStatus {
    pub connected: bool,
    pub enabled: bool,
    /// `SPEED_*` of the attached device (valid when connected).
    pub speed: u8,
    /// Something changed since the last `port_clear_changes`.
    pub changed: bool,
}

/// What a host controller driver provides.
pub trait Host: Send + Sync + 'static {
    /// Number of root ports.
    fn ports(&self) -> u8;
    fn port_status(&self, port: u8) -> PortStatus;
    fn port_clear_changes(&self, port: u8);
    /// Resets and enables a root port; returns the device's speed.
    fn port_reset(&self, port: u8) -> Result<u8, Status>;
    /// Whether `add_device` gives the device its address itself (xHCI).
    fn assigns_addresses(&self) -> bool {
        false
    }
    /// Sets up a new device that answers at address 0 with `mps0` bytes
    /// per control packet. A controller that assigns addresses addresses it.
    fn add_device(&self, at: &Attach, mps0: u16) -> Result<Handle, Status>;
    /// The core sent SET_ADDRESS: talk to the device at `address` from now.
    fn set_address(&self, _dev: Handle, _address: u8) {}
    /// The bus address of the device.
    fn address(&self, dev: Handle) -> u8;
    /// The real packet size of endpoint 0, from the device descriptor.
    fn set_mps0(&self, dev: Handle, mps0: u16) -> Result<(), Status>;
    /// Prepares the endpoints of a configuration (before SET_CONFIGURATION).
    fn configure(&self, dev: Handle, config: &[u8], hub: Option<HubInfo>) -> Result<(), Status>;
    /// Control transfer on endpoint 0; returns the bytes transferred.
    fn control(&self, dev: Handle, setup: Setup, data: &mut [u8], timeout_ms: u32) -> Result<usize, Status>;
    /// Bulk or interrupt transfer; returns the bytes transferred.
    fn transfer(&self, dev: Handle, endpoint: u8, data: &mut [u8], timeout_ms: u32) -> Result<usize, Status>;
    /// Starts polling an interrupt IN endpoint (see `UsbInterface`).
    fn poll_start(&self, dev: Handle, endpoint: u8, report: UsbReport, ctx: *mut c_void) -> Status;
    fn poll_stop(&self, dev: Handle, endpoint: u8);
    /// Resets the controller's side of a halted endpoint (data toggle,
    /// queue); the core then clears the halt on the device.
    fn reset_endpoint(&self, dev: Handle, endpoint: u8) -> Status;
    /// The device is gone: free everything the controller holds for it.
    fn remove_device(&self, dev: Handle);
    /// The controller can follow the machine into sleep: [`Host::stop`] and
    /// [`Host::restart`] work.
    const SLEEPS: bool = false;
    /// Before sleep, once every device is removed: the controller stops.
    fn stop(&self) {}
    /// After sleep the controller has lost its setup: reset it and start
    /// it again as at the start, with no devices.
    fn restart(&self) -> Status {
        justdrv::E_UNSUPPORTED
    }
}

// ----------------------------------------------------------------------------
// Hub requests

const HUB_GET_STATUS: Setup = Setup::new(DIR_IN | TYPE_CLASS | RECIP_OTHER, REQ_GET_STATUS, 0, 0, 4);
const PORT_RESET: u16 = 4;
const PORT_POWER: u16 = 8;
const C_PORT_CONNECTION: u16 = 16;
const C_PORT_ENABLE: u16 = 17;
const C_PORT_OVER_CURRENT: u16 = 19;
const C_PORT_RESET: u16 = 20;
const C_PORT_LINK_STATE: u16 = 25;
const C_PORT_CONFIG_ERROR: u16 = 26;
const C_BH_PORT_RESET: u16 = 29;
const REQ_SET_HUB_DEPTH: u8 = 12;

fn port_feature(set: bool, feature: u16, port: u8) -> Setup {
    Setup::new(TYPE_CLASS | RECIP_OTHER, if set { REQ_SET_FEATURE } else { REQ_CLEAR_FEATURE }, feature, port as u16, 0)
}

// ----------------------------------------------------------------------------
// Bus state

struct Hub {
    ports: u8,
    /// USB 3 hub (other port status layout).
    superspeed: bool,
    /// The device on each port (index into `devices`), by port - 1.
    children: Vec<Option<Handle>>,
    /// Kernel id of the hub device.
    kernel_id: u32,
}

/// The context behind a `UsbInterface` this core handed out.
struct IfaceCtx {
    bus: &'static dyn BusOps,
    dev: Handle,
    gone: AtomicBool,
}

struct Device {
    handle: Handle,
    /// "1.4.2": root port 1, hub port 4, hub port 2.
    path: String,
    speed: u8,
    /// Hub this device is plugged into (None: a root port).
    parent: Option<Handle>,
    attach: Attach,
    hub: Option<Hub>,
    /// Kernel ids of the interfaces.
    kernel_ids: Vec<u32>,
    contexts: Vec<&'static IfaceCtx>,
}

struct State {
    devices: Vec<Device>,
    /// The device on each root port, by port - 1.
    root: Vec<Option<Handle>>,
    addresses: [bool; 128],
}

/// The bus of one controller.
pub struct Bus<H: Host> {
    host: &'static H,
    /// Kernel id of the controller; also the bus number in port paths.
    controller: u32,
    state: Mutex<State>,
    wake: Event,
    /// Held while the bus thread looks at the ports.
    busy: Mutex<()>,
    /// Asleep: the ports are left alone.
    paused: AtomicBool,
}

/// The part of a bus the interface trampolines need (no generics there).
trait BusOps: Sync {
    fn control(&self, dev: Handle, setup: Setup, data: &mut [u8], timeout_ms: u32) -> Result<usize, Status>;
    fn transfer(&self, dev: Handle, endpoint: u8, data: &mut [u8], timeout_ms: u32) -> Result<usize, Status>;
    fn poll_start(&self, dev: Handle, endpoint: u8, report: UsbReport, ctx: *mut c_void) -> Status;
    fn poll_stop(&self, dev: Handle, endpoint: u8);
    fn clear_halt(&self, dev: Handle, endpoint: u8) -> Status;
}

impl<H: Host> BusOps for Bus<H> {
    fn control(&self, dev: Handle, setup: Setup, data: &mut [u8], timeout_ms: u32) -> Result<usize, Status> {
        self.host.control(dev, setup, data, timeout_ms)
    }

    fn transfer(&self, dev: Handle, endpoint: u8, data: &mut [u8], timeout_ms: u32) -> Result<usize, Status> {
        self.host.transfer(dev, endpoint, data, timeout_ms)
    }

    fn poll_start(&self, dev: Handle, endpoint: u8, report: UsbReport, ctx: *mut c_void) -> Status {
        self.host.poll_start(dev, endpoint, report, ctx)
    }

    fn poll_stop(&self, dev: Handle, endpoint: u8) {
        self.host.poll_stop(dev, endpoint)
    }

    fn clear_halt(&self, dev: Handle, endpoint: u8) -> Status {
        let st = self.host.reset_endpoint(dev, endpoint);
        if st != OK {
            return st;
        }
        let clear = Setup::new(RECIP_ENDPOINT, REQ_CLEAR_FEATURE, 0, endpoint as u16, 0);
        match self.host.control(dev, clear, &mut [], 1000) {
            Ok(_) => OK,
            Err(e) => e,
        }
    }
}

/// Wakes the bus thread: a port changed (call it from the controller's
/// interrupt or event handling).
pub fn port_changed<H: Host>(bus: &Bus<H>) {
    bus.wake.signal();
}

/// Starts the core for a controller: a thread that watches the ports and
/// manages devices. `controller` is the controller's kernel device id.
pub fn start<H: Host>(host: &'static H, controller: u32, name: &str) -> &'static Bus<H> {
    let bus: &'static Bus<H> = Box::leak(Box::new(Bus {
        host,
        controller,
        state: Mutex::new(State { devices: Vec::new(), root: vec![None; host.ports() as usize], addresses: [false; 128] }),
        wake: Event::new(),
        busy: Mutex::new(()),
        paused: AtomicBool::new(false),
    }));
    unsafe extern "C" fn run<H: Host>(ctx: *mut c_void) {
        // SAFETY: the leaked bus passed below.
        let bus = unsafe { &*(ctx as *const Bus<H>) };
        bus.run();
    }
    let ctx = bus as *const Bus<H> as *mut c_void;
    justdrv::spawn(name, run::<H>, ctx);
    if H::SLEEPS {
        justdrv::power_hooks(controller, ctx, Some(sleep::<H>), wake::<H>);
    }
    bus
}

/// Before sleep: the devices are let go as if unplugged (their drivers
/// stop cleanly while the controller still works), then the controller
/// stops.
unsafe extern "C" fn sleep<H: Host>(ctx: *mut c_void) -> Status {
    // SAFETY: the leaked bus.
    let bus = unsafe { &*(ctx as *const Bus<H>) };
    bus.paused.store(true, Ordering::Release);
    let _busy = bus.busy.lock();
    bus.remove_all();
    bus.host.stop();
    OK
}

/// After sleep: the controller starts again and the devices are found
/// again as if plugged in.
unsafe extern "C" fn wake<H: Host>(ctx: *mut c_void) -> Status {
    // SAFETY: the leaked bus.
    let bus = unsafe { &*(ctx as *const Bus<H>) };
    let st = bus.host.restart();
    bus.paused.store(false, Ordering::Release);
    bus.wake.signal();
    st
}

impl<H: Host> Bus<H> {
    fn run(&'static self) {
        // Give devices that were plugged in at power-on time to settle.
        justdrv::sleep_ms(50);
        loop {
            if !self.paused.load(Ordering::Acquire) {
                let _busy = self.busy.lock();
                if !self.paused.load(Ordering::Acquire) {
                    self.check_root_ports();
                    self.check_hubs();
                }
            }
            self.wake.wait(250);
        }
    }

    // ------------------------------------------------------------------
    // Root ports

    fn check_root_ports(&'static self) {
        for port in 1..=self.host.ports() {
            let st = self.host.port_status(port);
            if st.changed {
                self.host.port_clear_changes(port);
            }
            let present = self.state.lock().root[port as usize - 1];
            match (st.connected, present) {
                (false, Some(h)) => self.unplug(h),
                (true, Some(h)) if st.changed && !self.host.port_status(port).enabled => {
                    // Reconnected or disabled by an error: start over.
                    self.unplug(h);
                    self.plug_root(port);
                }
                (true, None) => self.plug_root(port),
                _ => {}
            }
        }
    }

    fn plug_root(&'static self, port: u8) {
        justdrv::sleep_ms(100); // connect debounce
        if !self.host.port_status(port).connected {
            return;
        }
        let speed = match self.host.port_reset(port) {
            Ok(s) => s,
            Err(e) => {
                justdrv::debug!("port {port}: reset failed ({})", justdrv::status_name(e));
                return;
            }
        };
        let at = Attach { speed, root_port: port, route: 0, depth: 0, hub: None, tt: None };
        let path = format!("{port}");
        if let Some(h) = self.enumerate(at, path, None) {
            self.state.lock().root[port as usize - 1] = Some(h);
        }
    }

    // ------------------------------------------------------------------
    // Hubs

    fn check_hubs(&'static self) {
        let hubs: Vec<(Handle, u8, bool)> =
            self.state.lock().devices.iter().filter_map(|d| d.hub.as_ref().map(|h| (d.handle, h.ports, h.superspeed))).collect();
        for (hub, ports, ss) in hubs {
            for port in 1..=ports {
                if !self.present(hub) {
                    break;
                }
                self.check_hub_port(hub, port, ss);
            }
        }
    }

    fn present(&self, h: Handle) -> bool {
        self.state.lock().devices.iter().any(|d| d.handle == h)
    }

    /// (wPortStatus, wPortChange) of a hub port.
    fn hub_port_status(&self, hub: Handle, port: u8) -> Option<(u16, u16)> {
        let mut st = [0u8; 4];
        let mut setup = HUB_GET_STATUS;
        setup.index = port as u16;
        self.host.control(hub, setup, &mut st, 500).ok()?;
        Some((u16::from_le_bytes([st[0], st[1]]), u16::from_le_bytes([st[2], st[3]])))
    }

    fn check_hub_port(&'static self, hub: Handle, port: u8, ss: bool) {
        let Some((status, change)) = self.hub_port_status(hub, port) else { return };
        // Acknowledge every change bit.
        let clears: &[(u16, u16)] = if ss {
            &[
                (1, C_PORT_CONNECTION),
                (8, C_PORT_OVER_CURRENT),
                (16, C_PORT_RESET),
                (32, C_BH_PORT_RESET),
                (64, C_PORT_LINK_STATE),
                (128, C_PORT_CONFIG_ERROR),
            ]
        } else {
            &[(1, C_PORT_CONNECTION), (2, C_PORT_ENABLE), (8, C_PORT_OVER_CURRENT), (16, C_PORT_RESET)]
        };
        for &(bit, feature) in clears {
            if change & bit != 0 {
                let _ = self.host.control(hub, port_feature(false, feature, port), &mut [], 500);
            }
        }
        let connected = status & 1 != 0;
        let present = self.child(hub, port);
        match (connected, present) {
            (false, Some(h)) => self.unplug(h),
            (true, Some(h)) if change & 1 != 0 => {
                self.unplug(h);
                self.plug_hub(hub, port, ss);
            }
            (true, None) => self.plug_hub(hub, port, ss),
            _ => {}
        }
    }

    fn child(&self, hub: Handle, port: u8) -> Option<Handle> {
        let s = self.state.lock();
        let d = s.devices.iter().find(|d| d.handle == hub)?;
        *d.hub.as_ref()?.children.get(port as usize - 1)?
    }

    fn plug_hub(&'static self, hub: Handle, port: u8, ss: bool) {
        justdrv::sleep_ms(100);
        let Some((status, _)) = self.hub_port_status(hub, port) else { return };
        if status & 1 == 0 {
            return;
        }
        // USB 3 ports enable themselves; USB 2 ports need a reset.
        let mut status = status;
        if !ss || status & 2 == 0 {
            if self.host.control(hub, port_feature(true, PORT_RESET, port), &mut [], 500).is_err() {
                return;
            }
            let mut done = false;
            for _ in 0..50 {
                justdrv::sleep_ms(10);
                if let Some((s, c)) = self.hub_port_status(hub, port) {
                    if c & 16 != 0 || (s & 16 == 0 && s & 2 != 0) {
                        status = s;
                        done = true;
                        break;
                    }
                }
            }
            let _ = self.host.control(hub, port_feature(false, C_PORT_RESET, port), &mut [], 500);
            if !done || status & 2 == 0 {
                justdrv::debug!("hub port {port}: reset failed");
                return;
            }
            justdrv::sleep_ms(10); // reset recovery
        }
        let speed = if ss {
            SPEED_SUPER
        } else if status & (1 << 9) != 0 {
            SPEED_LOW
        } else if status & (1 << 10) != 0 {
            SPEED_HIGH
        } else {
            SPEED_FULL
        };
        let (parent_at, parent_path, parent_speed) = {
            let s = self.state.lock();
            let Some(d) = s.devices.iter().find(|d| d.handle == hub) else { return };
            (d.attach, d.path.clone(), d.speed)
        };
        if parent_at.depth >= 5 {
            justdrv::warn!("hub port {port}: too many hubs in a row");
            return;
        }
        // Low/full-speed devices behind a high-speed hub talk through its
        // transaction translator; deeper down they inherit it.
        let tt = if speed < SPEED_HIGH {
            if parent_speed == SPEED_HIGH {
                Some((hub, port, false))
            } else {
                parent_at.tt
            }
        } else {
            None
        };
        let at = Attach {
            speed,
            root_port: parent_at.root_port,
            route: parent_at.route | (port.min(15) as u32) << (parent_at.depth as u32 * 4),
            depth: parent_at.depth + 1,
            hub: Some((hub, port)),
            tt,
        };
        let path = format!("{parent_path}.{port}");
        if let Some(h) = self.enumerate(at, path, Some(hub)) {
            let mut s = self.state.lock();
            if let Some(hub) = s.devices.iter_mut().find(|d| d.handle == hub).and_then(|d| d.hub.as_mut()) {
                if let Some(slot) = hub.children.get_mut(port as usize - 1) {
                    *slot = Some(h);
                }
            }
        }
    }

    /// Prepares a new hub: descriptor, depth, port power.
    fn start_hub(&self, h: Handle, speed: u8) -> Option<(HubInfo, bool, u16)> {
        let ss = speed >= SPEED_SUPER;
        let kind = if ss { DESC_SS_HUB } else { DESC_HUB };
        let mut d = [0u8; 16];
        let get = Setup::new(DIR_IN | TYPE_CLASS, REQ_GET_DESCRIPTOR, (kind as u16) << 8, 0, d.len() as u16);
        let n = self.host.control(h, get, &mut d, 1000).ok()?;
        if n < 7 {
            return None;
        }
        let ports = d[2].min(15);
        let characteristics = u16::from_le_bytes([d[3], d[4]]);
        let power_good = d[5] as u16 * 2;
        let info = HubInfo { ports, think_time: ((characteristics >> 5) & 3) as u8, multi_tt: false };
        Some((info, ss, power_good))
    }

    fn power_hub(&self, h: Handle, info: &HubInfo, ss: bool, depth: u8, power_good: u16) {
        if ss {
            let set_depth = Setup::new(TYPE_CLASS, REQ_SET_HUB_DEPTH, depth as u16, 0, 0);
            let _ = self.host.control(h, set_depth, &mut [], 1000);
        }
        for port in 1..=info.ports {
            let _ = self.host.control(h, port_feature(true, PORT_POWER, port), &mut [], 1000);
        }
        justdrv::sleep_ms(power_good.max(100) as u64);
    }

    // ------------------------------------------------------------------
    // Enumeration

    fn get_descriptor(&self, h: Handle, kind: u8, index: u8, lang: u16, buf: &mut [u8]) -> Result<usize, Status> {
        let mut setup = Setup::get_descriptor(kind, index, buf.len() as u16);
        setup.index = lang;
        self.host.control(h, setup, buf, 1000)
    }

    fn string(&self, h: Handle, index: u8, lang: u16) -> Option<String> {
        if index == 0 || lang == 0 {
            return None;
        }
        let mut buf = [0u8; 255];
        let n = self.get_descriptor(h, DESC_STRING, index, lang, &mut buf).ok()?;
        let mut out = [0u8; 128];
        let len = decode_string(&buf[..n], &mut out);
        let s = core::str::from_utf8(&out[..len]).ok()?.trim();
        (!s.is_empty()).then(|| String::from(s))
    }

    fn free_address(&self, h: Handle) {
        let addr = self.host.address(h) as usize;
        if addr > 0 && addr < 128 {
            self.state.lock().addresses[addr] = false;
        }
    }

    /// Takes a freshly reset device from address 0 to configured, and
    /// publishes its interfaces. Returns its handle.
    fn enumerate(&'static self, at: Attach, path: String, parent: Option<Handle>) -> Option<Handle> {
        for attempt in 0..2 {
            match self.try_enumerate(&at, &path, parent) {
                Ok(h) => return Some(h),
                Err(e) if attempt == 0 => {
                    justdrv::debug!("{}-{path}: {}; retrying", self.controller, justdrv::status_name(e));
                    justdrv::sleep_ms(100);
                }
                Err(e) => justdrv::warn!("device on port {}-{path} does not respond ({})", self.controller, justdrv::status_name(e)),
            }
        }
        None
    }

    fn try_enumerate(&'static self, at: &Attach, path: &str, parent: Option<Handle>) -> Result<Handle, Status> {
        let mps0 = match at.speed {
            SPEED_LOW => 8,
            SPEED_FULL => 64,
            SPEED_HIGH => 64,
            _ => 512,
        };
        let h = self.host.add_device(at, mps0)?;
        let result = self.setup_device(h, at, path, parent);
        if result.is_err() {
            self.free_address(h);
            self.host.remove_device(h);
        }
        result.map(|_| h)
    }

    fn setup_device(&'static self, h: Handle, at: &Attach, path: &str, parent: Option<Handle>) -> Result<(), Status> {
        // The first 8 bytes say how big endpoint 0's packets are.
        let mut d = [0u8; 18];
        let n = self.get_descriptor(h, DESC_DEVICE, 0, 0, &mut d[..8])?;
        if n < 8 {
            return Err(E_IO);
        }
        let mps0 = if at.speed >= SPEED_SUPER { 1u16 << d[7].min(9) } else { d[7].max(8) as u16 };
        self.host.set_mps0(h, mps0)?;
        if !self.host.assigns_addresses() {
            let addr = {
                let mut s = self.state.lock();
                let a = (1..128).find(|&a| !s.addresses[a]).ok_or(justdrv::E_BUSY)?;
                s.addresses[a] = true;
                a as u8
            };
            let set = Setup::new(0, REQ_SET_ADDRESS, addr as u16, 0, 0);
            if let Err(e) = self.host.control(h, set, &mut [], 1000) {
                self.state.lock().addresses[addr as usize] = false;
                return Err(e);
            }
            justdrv::sleep_ms(5); // SET_ADDRESS recovery
            self.host.set_address(h, addr);
        }
        if self.get_descriptor(h, DESC_DEVICE, 0, 0, &mut d)? < 18 {
            return Err(E_IO);
        }
        let desc = DeviceDescriptor::parse(&d).ok_or(E_IO)?;

        // Configuration 0, all of it.
        let mut head = [0u8; 9];
        self.get_descriptor(h, DESC_CONFIG, 0, 0, &mut head)?;
        let total = (u16::from_le_bytes([head[2], head[3]]) as usize).clamp(9, 4096);
        let mut config = vec![0u8; total];
        let n = self.get_descriptor(h, DESC_CONFIG, 0, 0, &mut config)?;
        config.truncate(n);
        if n < 9 {
            return Err(E_IO);
        }

        // Names, in the first language the device offers.
        let mut langs = [0u8; 4];
        let lang = match self.get_descriptor(h, DESC_STRING, 0, 0, &mut langs) {
            Ok(n) if n >= 4 => u16::from_le_bytes([langs[2], langs[3]]),
            _ => 0,
        };
        let product = self.string(h, desc.product_string, lang);
        let maker = self.string(h, desc.manufacturer_string, lang);
        let name = match (&maker, &product) {
            (Some(m), Some(p)) if !p.starts_with(m.as_str()) => format!("{m} {p}"),
            (_, Some(p)) => p.clone(),
            (Some(m), None) => format!("{m} device"),
            (None, None) => String::new(),
        };

        let is_hub = desc.class == CLASS_HUB || interfaces(&config).any(|i| i.class == CLASS_HUB);
        let hub = if is_hub { Some(self.start_hub(h, at.speed).ok_or(E_IO)?) } else { None };
        self.host.configure(h, &config, hub.map(|x| x.0))?;
        let set = Setup::new(0, REQ_SET_CONFIGURATION, config_value(&config) as u16, 0, 0);
        self.host.control(h, set, &mut [], 1000)?;

        let parent_kernel = parent
            .and_then(|p| self.state.lock().devices.iter().find(|d| d.handle == p).and_then(|d| d.hub.as_ref().map(|h| h.kernel_id)))
            .unwrap_or(self.controller);
        let location = format!("{}-{path}", self.controller);
        let label = if name.is_empty() { format!("{:04x}:{:04x}", desc.vendor, desc.product) } else { name.clone() };
        justdrv::info!("{location}: {label} ({}, {})", speed_name(at.speed), if is_hub { "hub" } else { "device" });

        let mut device = Device {
            handle: h,
            path: String::from(path),
            speed: at.speed,
            parent,
            attach: *at,
            hub: None,
            kernel_ids: Vec::new(),
            contexts: Vec::new(),
        };
        // The configuration stays alive for as long as the interfaces do.
        let config: &'static [u8] = Box::leak(config.into_boxed_slice());

        if let Some((info, ss, power_good)) = hub {
            let mut di = self.device_info(&desc, None, &location, parent_kernel, 0);
            di.class = CLASS_HUB;
            let label = if name.is_empty() { String::from(if ss { "USB 3 hub" } else { "USB hub" }) } else { name.clone() };
            let id = justdrv::device_add(parent_kernel, &di, &label, DEVICE_OWNED);
            self.power_hub(h, &info, ss, at.depth, power_good);
            device.hub =
                Some(Hub { ports: info.ports, superspeed: ss, children: vec![None; info.ports as usize], kernel_id: id.max(0) as u32 });
            self.state.lock().devices.push(device);
            return Ok(());
        }

        // Every interface becomes a kernel device.
        let ifaces: Vec<InterfaceDescriptor> = interfaces(config).collect();
        self.state.lock().devices.push(device);
        for i in ifaces {
            let ctx: &'static IfaceCtx = Box::leak(Box::new(IfaceCtx { bus: self, dev: h, gone: AtomicBool::new(false) }));
            let iface: &'static UsbInterface = Box::leak(Box::new(UsbInterface {
                ctx: ctx as *const IfaceCtx as *mut c_void,
                speed: at.speed,
                number: i.number,
                address: self.host.address(h),
                _pad: 0,
                config: config.as_ptr(),
                config_len: config.len() as u32,
                _pad2: 0,
                control: tramp_control,
                transfer: tramp_transfer,
                poll_start: tramp_poll_start,
                poll_stop: tramp_poll_stop,
                clear_halt: tramp_clear_halt,
            }));
            let mut di = self.device_info(&desc, Some(&i), &location, parent_kernel, iface as *const UsbInterface as u64);
            di.location = (h << 8) | i.number as u32;
            let id = justdrv::device_add(parent_kernel, &di, &name, 0);
            let mut s = self.state.lock();
            if let Some(d) = s.devices.iter_mut().find(|d| d.handle == h) {
                d.contexts.push(ctx);
                if id > 0 {
                    d.kernel_ids.push(id as u32);
                }
            }
        }
        Ok(())
    }

    fn device_info(&self, desc: &DeviceDescriptor, i: Option<&InterfaceDescriptor>, location: &str, parent: u32, iface: u64) -> DeviceInfo {
        let mut di = DeviceInfo::empty();
        di.bus = BUS_USB;
        di.vendor = desc.vendor;
        di.device = desc.product;
        di.revision = (desc.release >> 8) as u8;
        // Class 0 on the device means "see the interfaces".
        let (class, subclass, protocol) = match i {
            Some(i) if i.class != 0 => (i.class, i.subclass, i.protocol),
            _ => (desc.class, desc.subclass, desc.protocol),
        };
        di.class = class;
        di.subclass = subclass;
        di.prog_if = protocol;
        di.parent = parent;
        di.interface = iface;
        match i {
            Some(i) => di.set_name(&format!("{location}:{}", i.number)),
            None => di.set_name(location),
        }
        di
    }

    // ------------------------------------------------------------------
    // Removal

    /// A device was unplugged: everything behind it goes too.
    fn unplug(&'static self, h: Handle) {
        let (children, kernel_ids, hub_id, contexts, parent, path) = {
            let s = self.state.lock();
            let Some(d) = s.devices.iter().find(|d| d.handle == h) else { return };
            let children: Vec<Handle> = d.hub.as_ref().map(|hub| hub.children.iter().flatten().copied().collect()).unwrap_or_default();
            (children, d.kernel_ids.clone(), d.hub.as_ref().map(|h| h.kernel_id), d.contexts.clone(), d.parent, d.path.clone())
        };
        for c in children {
            self.unplug(c);
        }
        for ctx in &contexts {
            ctx.gone.store(true, Ordering::Release);
        }
        for id in kernel_ids {
            justdrv::device_remove(id);
        }
        if let Some(id) = hub_id {
            justdrv::device_remove(id);
        }
        justdrv::info!("{}-{path}: unplugged", self.controller);
        self.free_address(h);
        self.host.remove_device(h);
        let mut s = self.state.lock();
        s.devices.retain(|d| d.handle != h);
        for slot in s.root.iter_mut() {
            if *slot == Some(h) {
                *slot = None;
            }
        }
        if let Some(p) = parent {
            if let Some(hub) = s.devices.iter_mut().find(|d| d.handle == p).and_then(|d| d.hub.as_mut()) {
                for slot in hub.children.iter_mut() {
                    if *slot == Some(h) {
                        *slot = None;
                    }
                }
            }
        }
    }

    /// The controller is going away: every device is removed.
    pub fn remove_all(&'static self) {
        let roots: Vec<Handle> = self.state.lock().root.iter().flatten().copied().collect();
        for h in roots {
            self.unplug(h);
        }
    }
}

// ----------------------------------------------------------------------------
// The functions behind `UsbInterface`

fn ctx<'a>(ctx: *mut c_void) -> Option<&'a IfaceCtx> {
    // SAFETY: a context this core leaked for a UsbInterface; never freed.
    let c = unsafe { (ctx as *const IfaceCtx).as_ref()? };
    (!c.gone.load(Ordering::Acquire)).then_some(c)
}

fn result(r: Result<usize, Status>) -> Status {
    match r {
        Ok(n) => n as Status,
        Err(e) => e,
    }
}

unsafe extern "C" fn tramp_control(c: *mut c_void, setup: *const Setup, data: *mut u8, timeout_ms: u32) -> Status {
    let Some(c) = ctx(c) else { return E_GONE };
    if setup.is_null() {
        return E_INVAL;
    }
    // SAFETY: the driver passes a setup packet and `length` bytes of data.
    let setup = unsafe { *setup };
    let buf: &mut [u8] = if setup.length == 0 || data.is_null() {
        &mut []
    } else {
        // SAFETY: as above.
        unsafe { core::slice::from_raw_parts_mut(data, setup.length as usize) }
    };
    result(c.bus.control(c.dev, setup, buf, timeout_ms))
}

unsafe extern "C" fn tramp_transfer(c: *mut c_void, endpoint: u8, data: *mut u8, len: u32, timeout_ms: u32) -> Status {
    let Some(c) = ctx(c) else { return E_GONE };
    if data.is_null() && len != 0 {
        return E_INVAL;
    }
    let buf: &mut [u8] = if len == 0 {
        &mut []
    } else {
        // SAFETY: the driver passes `len` bytes.
        unsafe { core::slice::from_raw_parts_mut(data, len as usize) }
    };
    result(c.bus.transfer(c.dev, endpoint, buf, timeout_ms))
}

unsafe extern "C" fn tramp_poll_start(c: *mut c_void, endpoint: u8, report: UsbReport, cb: *mut c_void) -> Status {
    let Some(c) = ctx(c) else { return E_GONE };
    c.bus.poll_start(c.dev, endpoint, report, cb)
}

unsafe extern "C" fn tramp_poll_stop(c: *mut c_void, endpoint: u8) -> Status {
    // Stopping is allowed while the device goes away.
    // SAFETY: as in ctx().
    if let Some(c) = unsafe { (c as *const IfaceCtx).as_ref() } {
        c.bus.poll_stop(c.dev, endpoint);
    }
    OK
}

unsafe extern "C" fn tramp_clear_halt(c: *mut c_void, endpoint: u8) -> Status {
    let Some(c) = ctx(c) else { return E_GONE };
    c.bus.clear_halt(c.dev, endpoint)
}

// ----------------------------------------------------------------------------
// Helpers for host controller drivers

/// xHCI-style polling interval of an endpoint, as a power of two in
/// 125 µs units (3 = 1 ms), from its descriptor and the device speed.
pub fn interval_exponent(ep: &EndpointDescriptor, speed: u8) -> u8 {
    let b = ep.interval.max(1);
    match (ep.kind(), speed) {
        (EP_INTERRUPT | EP_ISOCH, s) if s >= SPEED_HIGH => (b.min(16) - 1).min(15),
        (EP_ISOCH, _) => (b.min(16) - 1 + 3).min(15),
        (EP_INTERRUPT, _) => {
            // bInterval in milliseconds: largest 2^n x 125 µs that fits.
            let micro = b as u32 * 8;
            (31 - micro.leading_zeros()).clamp(3, 10) as u8
        }
        _ => 0,
    }
}

/// Interval in milliseconds of an interrupt endpoint (at least 1).
pub fn interval_ms(ep: &EndpointDescriptor, speed: u8) -> u32 {
    if speed >= SPEED_HIGH {
        ((1u32 << (ep.interval.clamp(1, 16) - 1)) / 8).max(1)
    } else {
        ep.interval.max(1) as u32
    }
}

/// The endpoints a controller has to prepare for a configuration: bulk and
/// interrupt endpoints of alternate setting 0 of every interface, each once.
pub fn config_endpoints(config: &[u8]) -> Vec<EndpointDescriptor> {
    let mut out: Vec<EndpointDescriptor> = Vec::new();
    for i in interfaces(config) {
        for e in endpoints(config, i.number, 0) {
            let kind = e.kind();
            if (kind == EP_BULK || kind == EP_INTERRUPT) && e.number() != 0 && !out.iter().any(|o| o.address == e.address) {
                out.push(e);
            }
        }
    }
    out
}

/// A small table that maps keys to values, for host drivers that keep
/// per-device state behind a lock.
pub struct Table<T> {
    items: IrqLock<Vec<(Handle, T)>>,
}

impl<T: Clone> Table<T> {
    pub const fn new() -> Table<T> {
        Table { items: IrqLock::new(Vec::new()) }
    }

    pub fn insert(&self, h: Handle, v: T) {
        self.items.lock().push((h, v));
    }

    pub fn get(&self, h: Handle) -> Option<T> {
        self.items.lock().iter().find(|(k, _)| *k == h).map(|(_, v)| v.clone())
    }

    pub fn remove(&self, h: Handle) -> Option<T> {
        let mut items = self.items.lock();
        let i = items.iter().position(|(k, _)| *k == h)?;
        Some(items.remove(i).1)
    }
}

impl<T: Clone> Default for Table<T> {
    fn default() -> Self {
        Self::new()
    }
}

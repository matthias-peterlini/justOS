//! Batteries, the power adapter, the lid and the thermal zones, as the
//! firmware's AML reports them (ACPI 6.5, 10.2, 10.3, 9.4 and 11). The AML
//! task keeps a copy of their state that anyone can read.

use super::aml::Aml;
use crate::sync::IrqLock;
use alloc::string::String;
use alloc::vec::Vec;
use justaml::{NodeId, ObjectType, Value};

#[derive(Clone, Debug, Default)]
pub struct Battery {
    pub path: String,
    pub present: bool,
    /// Capacities and rates in mWh and mW (or mAh and mA if not `watts`).
    pub watts: bool,
    pub design: Option<u32>,
    pub full: Option<u32>,
    pub remaining: Option<u32>,
    pub rate: Option<u32>,
    /// mV
    pub voltage: Option<u32>,
    pub cycles: Option<u32>,
    pub model: String,
    pub kind: String,
    pub vendor: String,
    /// `_BST` state: bit 0 discharging, 1 charging, 2 critically low.
    pub state: u32,
}

impl Battery {
    pub fn charging(&self) -> bool {
        self.state & 2 != 0
    }

    pub fn discharging(&self) -> bool {
        self.state & 1 != 0
    }

    pub fn percent(&self) -> Option<u8> {
        let full = self.full.or(self.design).filter(|&f| f > 0)?;
        Some((self.remaining? as u64 * 100 / full as u64).min(100) as u8)
    }

    /// Minutes until empty (discharging) or full (charging).
    pub fn minutes_left(&self) -> Option<u32> {
        let rate = self.rate.filter(|&r| r > 0)? as u64;
        let left = if self.discharging() {
            self.remaining? as u64
        } else if self.charging() {
            (self.full? as u64).saturating_sub(self.remaining? as u64)
        } else {
            return None;
        };
        Some((left * 60 / rate).min(48 * 60) as u32)
    }

    /// The wear: last full charge against design capacity.
    pub fn health(&self) -> Option<u8> {
        let d = self.design.filter(|&d| d > 0)?;
        Some((self.full? as u64 * 100 / d as u64).min(100) as u8)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Thermal {
    pub path: String,
    /// Tenths of a degree Celsius.
    pub temp: i32,
    /// When the firmware wants the computer turned off.
    pub critical: Option<i32>,
}

#[derive(Clone, Debug, Default)]
pub struct State {
    pub batteries: Vec<Battery>,
    /// On mains power (None: no adapter is described).
    pub ac: Option<bool>,
    pub lid_open: Option<bool>,
    pub thermal: Vec<Thermal>,
}

static STATE: IrqLock<State> = IrqLock::new(State { batteries: Vec::new(), ac: None, lid_open: None, thermal: Vec::new() });

/// The latest state of batteries, adapter, lid and temperatures.
pub fn state() -> State {
    STATE.lock().clone()
}

/// The charge of the batteries together, and whether they charge (or the
/// computer runs on mains): None without a battery.
pub fn battery() -> Option<(u8, bool)> {
    let s = STATE.lock();
    let present: Vec<&Battery> = s.batteries.iter().filter(|b| b.present).collect();
    if present.is_empty() {
        return None;
    }
    let (mut rem, mut full) = (0u64, 0u64);
    for b in &present {
        if let (Some(r), Some(f)) = (b.remaining, b.full.or(b.design)) {
            rem += r as u64;
            full += f as u64;
        }
    }
    let pct = if full > 0 { (rem * 100 / full).min(100) as u8 } else { present[0].percent()? };
    let plugged = s.ac == Some(true) || present.iter().any(|b| b.charging());
    Some((pct, plugged))
}

/// The hottest thermal zone, in tenths of °C.
pub fn hottest() -> Option<i32> {
    STATE.lock().thermal.iter().map(|t| t.temp).max()
}

/// The devices found in the namespace.
#[derive(Default)]
pub(super) struct Devices {
    batteries: Vec<NodeId>,
    adapters: Vec<NodeId>,
    lids: Vec<NodeId>,
    zones: Vec<NodeId>,
    /// Static battery information, read again when a battery is inserted
    /// or the firmware says it changed.
    info: Vec<Option<Battery>>,
    last_batteries: u64,
    last_thermal: u64,
    /// Readings above the critical temperature in a row.
    hot: u32,
    warned: u8,
}

/// An integer that is not "unknown" (all ones).
fn known(v: Option<&Value>) -> Option<u32> {
    let x = v?.as_integer()?;
    (x != 0xFFFF_FFFF && x != u64::MAX && x <= u32::MAX as u64).then_some(x as u32)
}

fn text(v: Option<&Value>) -> String {
    let s: String = match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Buffer(b)) => b.iter().take_while(|&&c| c != 0).map(|&c| c as char).collect(),
        _ => String::new(),
    };
    s.trim().chars().filter(|c| !c.is_control()).collect()
}

pub(super) fn discover(aml: &mut Aml) -> Devices {
    let mut d = Devices {
        batteries: aml.find_devices(&["PNP0C0A"]),
        adapters: aml.find_devices(&["ACPI0003"]),
        lids: aml.find_devices(&["PNP0C0D"]),
        ..Devices::default()
    };
    // Devices the firmware says are absent and not functioning stay out.
    for list in [&mut d.batteries, &mut d.adapters, &mut d.lids] {
        list.retain(|&n| aml.status(n) & 0x09 != 0);
    }
    d.zones = aml.nodes_of(ObjectType::ThermalZone).into_iter().filter(|&z| aml.child(z, "_TMP").is_some()).collect();
    d.info = alloc::vec![None; d.batteries.len()];
    refresh_all(aml, &mut d);
    d
}

pub(super) fn refresh_all(aml: &mut Aml, d: &mut Devices) {
    refresh_batteries(aml, d);
    refresh_lid(aml, d);
    refresh_thermal(aml, d);
}

fn battery_info(aml: &mut Aml, n: NodeId) -> Option<Battery> {
    let mut b = Battery { path: aml.path(n), ..Battery::default() };
    if let Ok(Some(Value::Package(p))) = aml.evaluate_child(n, "_BIX", &[]) {
        if p.len() >= 20 {
            b.watts = known(p.get(1)) == Some(0);
            b.design = known(p.get(2));
            b.full = known(p.get(3));
            b.voltage = known(p.get(5));
            b.cycles = known(p.get(8)).filter(|&c| c > 0);
            b.model = text(p.get(16));
            b.kind = text(p.get(18));
            b.vendor = text(p.get(19));
            return Some(b);
        }
    }
    let Ok(Some(Value::Package(p))) = aml.evaluate_child(n, "_BIF", &[]) else { return None };
    if p.len() < 13 {
        return None;
    }
    b.watts = known(p.first()) == Some(0);
    b.design = known(p.get(1));
    b.full = known(p.get(2));
    b.voltage = known(p.get(4));
    b.model = text(p.get(9));
    b.kind = text(p.get(11));
    b.vendor = text(p.get(12));
    Some(b)
}

/// Reads the batteries and the adapter again.
pub(super) fn refresh_batteries(aml: &mut Aml, d: &mut Devices) {
    let mut out = Vec::new();
    for (i, &n) in d.batteries.iter().enumerate() {
        // _STA bit 4: a battery is in the slot.
        let present = aml.status(n) & 0x10 != 0;
        if !present {
            d.info[i] = None;
            out.push(Battery { path: aml.path(n), ..Battery::default() });
            continue;
        }
        if d.info[i].is_none() {
            d.info[i] = battery_info(aml, n);
        }
        let mut b = d.info[i].clone().unwrap_or_else(|| Battery { path: aml.path(n), ..Battery::default() });
        b.present = true;
        if let Ok(Some(Value::Package(p))) = aml.evaluate_child(n, "_BST", &[]) {
            b.state = known(p.first()).unwrap_or(0);
            b.rate = known(p.get(1));
            b.remaining = known(p.get(2));
            if let Some(v) = known(p.get(3)) {
                b.voltage = Some(v);
            }
        }
        out.push(b);
    }
    let ac = d.adapters.first().map(|&a| aml.integer(a, "_PSR").unwrap_or(0) != 0);
    let mut s = STATE.lock();
    s.batteries = out;
    s.ac = ac;
}

/// The firmware says a battery's information changed (notification 0x81).
pub(super) fn forget_info(d: &mut Devices, n: NodeId) {
    if let Some(i) = d.batteries.iter().position(|&b| b == n) {
        d.info[i] = None;
    }
}

pub(super) fn refresh_lid(aml: &mut Aml, d: &mut Devices) {
    let open = d.lids.first().and_then(|&l| aml.integer(l, "_LID")).map(|v| v != 0);
    STATE.lock().lid_open = open;
}

pub(super) fn refresh_thermal(aml: &mut Aml, d: &mut Devices) {
    let mut out = Vec::new();
    for &z in &d.zones {
        // Tenths of a kelvin; nonsense readings (sensor missing) are left out.
        let Some(t) = aml.integer(z, "_TMP") else { continue };
        if !(2000..5000).contains(&t) {
            continue;
        }
        let critical = aml.integer(z, "_CRT").filter(|c| (3000..5000).contains(c)).map(|c| c as i32 - 2732);
        out.push(Thermal { path: aml.path(z), temp: t as i32 - 2732, critical });
    }
    STATE.lock().thermal = out;
}

pub(super) fn is_battery(d: &Devices, n: NodeId) -> bool {
    d.batteries.contains(&n) || d.adapters.contains(&n)
}

pub(super) fn is_lid(d: &Devices, n: NodeId) -> bool {
    d.lids.contains(&n)
}

pub(super) fn is_zone(d: &Devices, n: NodeId) -> bool {
    d.zones.contains(&n)
}

/// Every so often: batteries every 30 s, temperatures every 10 s. Acts on
/// a critical battery or temperature.
pub(super) fn poll(aml: &mut Aml, d: &mut Devices, now: u64) {
    if !d.batteries.is_empty() || !d.adapters.is_empty() {
        if now >= d.last_batteries + 30_000 {
            d.last_batteries = now;
            refresh_batteries(aml, d);
        }
    }
    if !d.zones.is_empty() && now >= d.last_thermal + 10_000 {
        d.last_thermal = now;
        refresh_thermal(aml, d);
        let s = state();
        let over = s.thermal.iter().find(|t| t.critical.is_some_and(|c| t.temp >= c));
        match over {
            Some(t) => {
                d.hot += 1;
                crate::warn!("acpi", "{} is at {}.{} °C, above its critical {} °C", t.path, t.temp / 10, t.temp % 10, t.critical.unwrap_or(0) / 10);
                if d.hot >= 3 {
                    crate::gui::notify("Temperatura critica", "Il computer è troppo caldo: si spegne per proteggersi.", "monitor");
                    super::request_power_off("critical temperature");
                }
            }
            None => d.hot = 0,
        }
    }
    check_battery(d);
}

fn check_battery(d: &mut Devices) {
    let Some((pct, plugged)) = battery() else { return };
    if plugged {
        d.warned = 0;
        return;
    }
    let critical = STATE.lock().batteries.iter().any(|b| b.present && b.discharging() && b.state & 4 != 0);
    if (critical || pct <= 2) && d.warned < 3 {
        d.warned = 3;
        crate::warn!("acpi", "battery critically low ({pct}%): turning off");
        crate::gui::notify("Batteria scarica", "La batteria è quasi vuota: il computer si spegne.", "about");
        super::request_power_off("battery empty");
    } else if pct <= 5 && d.warned < 2 {
        d.warned = 2;
        crate::gui::notify("Batteria quasi scarica", &alloc::format!("Resta il {pct}%: collega l'alimentatore."), "about");
    } else if pct <= 10 && d.warned < 1 {
        d.warned = 1;
        crate::gui::notify("Batteria in esaurimento", &alloc::format!("Resta il {pct}% della carica."), "about");
    }
}

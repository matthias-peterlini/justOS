//! ACPI: the tables, power off and restart, and the firmware's AML, run by
//! the AML task (see `aml`), for batteries, lid, temperatures, buttons and
//! the embedded controller of laptops.

mod aml;
pub mod ec;
mod gpe;
pub mod power;

pub use aml::{ec_info, eval, ready as aml_ready, summary as aml_summary, tree};

use crate::arch::cpu::{self, inw, outb, outw};
use crate::boot::MemoryRegion;
use crate::mem::phys_to_virt;
use crate::sync::IrqLock;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Copy, Default)]
struct Fadt {
    smi_cmd: u32,
    acpi_enable: u8,
    pm1a_cnt: u32,
    pm1b_cnt: u32,
    dsdt: u64,
    reset_space: u8,
    reset_addr: u64,
    reset_value: u8,
    reset_supported: bool,
    /// Interrupt line of the SCI (System Control Interrupt).
    sci: u16,
    pm1a_evt: u32,
    pm1b_evt: u32,
    pm1_evt_len: u8,
    /// The power button is a fixed feature (not a control method one).
    fixed_power_button: bool,
    /// The general-purpose event blocks (I/O ports; lengths in bytes).
    gpe0_blk: u32,
    gpe1_blk: u32,
    gpe0_len: u8,
    gpe1_len: u8,
    gpe1_base: u8,
}

struct Acpi {
    tables: Vec<([u8; 4], u64)>,
    fadt: Option<Fadt>,
    oem: String,
    /// The firmware's memory map: what is RAM (reachable cached through the
    /// direct map) and what is not (device registers).
    memory: &'static [MemoryRegion],
}

static ACPI: IrqLock<Acpi> = IrqLock::new(Acpi { tables: Vec::new(), fadt: None, oem: String::new(), memory: &[] });

/// RAM, ACPI tables or ACPI NVS: memory, not device registers.
fn is_ram(addr: u64) -> bool {
    let top = crate::mem::frame::top().max(4 << 30);
    addr < top && ACPI.lock().memory.iter().any(|r| matches!(r.kind, 1 | 3 | 4) && addr >= r.base && addr - r.base < r.len)
}

/// The tables the root table lists, by signature and physical address.
fn tables() -> Vec<([u8; 4], u64)> {
    ACPI.lock().tables.clone()
}

fn dsdt() -> Option<u64> {
    ACPI.lock().fadt.map(|f| f.dsdt).filter(|&d| d != 0)
}

fn bytes(phys: u64, len: usize) -> &'static [u8] {
    // SAFETY: ACPI tables live in RAM or reserved memory, both reachable
    // through the direct map.
    unsafe { core::slice::from_raw_parts(phys_to_virt(phys) as *const u8, len) }
}

fn le16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn le64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

fn checksum_ok(b: &[u8]) -> bool {
    b.iter().fold(0u8, |a, &x| a.wrapping_add(x)) == 0
}

fn find_rsdp() -> Option<u64> {
    let scan = |start: u64, len: u64| -> Option<u64> {
        (start..start + len).step_by(16).find(|&a| bytes(a, 8) == b"RSD PTR " && checksum_ok(bytes(a, 20)))
    };
    let ebda = (le16(bytes(0x40E, 2), 0) as u64) << 4;
    if ebda != 0 {
        if let Some(a) = scan(ebda, 1024) {
            return Some(a);
        }
    }
    scan(0xE0000, 0x20000)
}

/// A table, if its length is sane and its checksum right.
fn table(phys: u64) -> Option<&'static [u8]> {
    let header = bytes(phys, 36);
    let len = le32(header, 4) as usize;
    if !(36..=1 << 20).contains(&len) {
        return None;
    }
    let t = bytes(phys, len);
    checksum_ok(t).then_some(t)
}

pub fn init(rsdp_hint: u64, memory: &'static [MemoryRegion]) {
    ACPI.lock().memory = memory;
    let Some(rsdp) = (if rsdp_hint != 0 { Some(rsdp_hint) } else { find_rsdp() }) else {
        crate::warn!("acpi", "no ACPI tables found");
        return;
    };
    let r = bytes(rsdp, 36);
    let revision = r[15];
    let oem = String::from_utf8_lossy(&r[9..15]).trim().into();
    let (root, entry_size) = if revision >= 2 && le64(r, 24) != 0 { (le64(r, 24), 8) } else { (le32(r, 16) as u64, 4) };
    let Some(root_table) = table(root) else {
        crate::warn!("acpi", "the root table is damaged");
        return;
    };
    let mut acpi = ACPI.lock();
    acpi.oem = oem;
    let count = (root_table.len() - 36) / entry_size;
    for i in 0..count {
        let at = 36 + i * entry_size;
        let phys = if entry_size == 8 { le64(root_table, at) } else { le32(root_table, at) as u64 };
        let Some(t) = table(phys) else { continue };
        let sig: [u8; 4] = t[..4].try_into().unwrap();
        acpi.tables.push((sig, phys));
        if &sig == b"FACP" && t.len() >= 116 {
            let mut f = Fadt {
                smi_cmd: le32(t, 48),
                acpi_enable: t[52],
                pm1a_cnt: le32(t, 64),
                pm1b_cnt: le32(t, 68),
                dsdt: le32(t, 40) as u64,
                ..Fadt::default()
            };
            f.sci = le16(t, 46);
            f.pm1a_evt = le32(t, 56);
            f.pm1b_evt = le32(t, 60);
            f.pm1_evt_len = t[88];
            f.gpe0_blk = le32(t, 80);
            f.gpe1_blk = le32(t, 84);
            f.gpe0_len = t[92];
            f.gpe1_len = t[93];
            f.gpe1_base = t[94];
            f.fixed_power_button = le32(t, 112) & (1 << 4) == 0;
            if t.len() >= 129 {
                f.reset_supported = le32(t, 112) & (1 << 10) != 0;
                f.reset_space = t[116];
                f.reset_addr = le64(t, 120);
                f.reset_value = t[128];
            }
            if t.len() >= 148 && le64(t, 140) != 0 {
                f.dsdt = le64(t, 140);
            }
            acpi.fadt = Some(f);
        }
    }
    let names: Vec<&str> = acpi.tables.iter().map(|(s, _)| core::str::from_utf8(s).unwrap_or("?")).collect();
    crate::debug!("acpi", "revision {revision}, tables: {}", names.join(" "));
}

/// The local APIC ids of the processors that can be started, from the MADT
/// ("APIC" table): enabled or online-capable local APIC entries (type 0)
/// and x2APIC entries (type 9) with an id the xAPIC can address.
pub fn processors() -> Vec<u32> {
    let phys = ACPI.lock().tables.iter().find(|(s, _)| s == b"APIC").map(|&(_, p)| p);
    let Some(t) = phys.and_then(table) else { return Vec::new() };
    let mut ids = Vec::new();
    let mut at = 44;
    while at + 2 <= t.len() {
        let (kind, len) = (t[at], t[at + 1] as usize);
        if len < 2 || at + len > t.len() {
            break;
        }
        match kind {
            0 if len >= 8 && le32(t, at + 4) & 3 != 0 => ids.push(t[at + 3] as u32),
            9 if len >= 16 && le32(t, at + 8) & 3 != 0 && le32(t, at + 4) < 255 => ids.push(le32(t, at + 4)),
            _ => {}
        }
        at += len;
    }
    ids.dedup();
    ids
}

/// Signatures of the ACPI tables, with the OEM id.
pub fn summary() -> (String, Vec<String>) {
    let a = ACPI.lock();
    (a.oem.clone(), a.tables.iter().map(|(s, _)| String::from_utf8_lossy(s).into()).collect())
}

/// SLP_TYPa/b for the S5 (soft off) state from the DSDT's `_S5_` package.
fn s5_sleep_type(dsdt: u64) -> Option<(u16, u16)> {
    let t = table(dsdt)?;
    let at = t.windows(4).position(|w| w == b"_S5_")?;
    let mut i = at + 4;
    if *t.get(i)? != 0x12 {
        return None; // PackageOp
    }
    i += 1;
    i += ((t[i] >> 6) & 3) as usize + 1; // PkgLength
    i += 1; // NumElements
    let read = |i: &mut usize| -> Option<u16> {
        let v = match *t.get(*i)? {
            0x0A => {
                *i += 1;
                *t.get(*i)? as u16
            } // BytePrefix
            0x00 => 0,
            0x01 => 1,
            x => x as u16,
        };
        *i += 1;
        Some(v)
    };
    let a = read(&mut i)?;
    let b = read(&mut i).unwrap_or(0);
    Some((a, b))
}

/// Switches the chipset from legacy (SMI) to ACPI mode if the firmware left
/// it in the former.
fn enable_acpi_mode(f: &Fadt) {
    if f.pm1a_cnt == 0 || inw(f.pm1a_cnt as u16) & 1 != 0 || f.smi_cmd == 0 || f.acpi_enable == 0 {
        return;
    }
    outb(f.smi_cmd as u16, f.acpi_enable);
    for _ in 0..1_000_000 {
        if inw(f.pm1a_cnt as u16) & 1 != 0 {
            break;
        }
    }
}

/// PM1 status: the power button was pressed.
const PWRBTN: u16 = 1 << 8;

/// Set when the computer should turn itself off: the power button, or the
/// AML task (battery empty, too hot).
static POWER_BUTTON: crate::task::Event = crate::task::Event::new();
static POWER_REASON: IrqLock<&'static str> = IrqLock::new("power button pressed");

/// The PM1 status and enable registers of the first and second block.
fn pm1_regs(f: &Fadt) -> impl Iterator<Item = (u16, u16)> {
    let half = (f.pm1_evt_len / 2) as u32;
    [f.pm1a_evt, f.pm1b_evt].into_iter().filter(|&b| b != 0 && b <= 0xFFFF).map(move |b| (b as u16, (b + half) as u16))
}

/// The SCI: the fixed power button, and the GPEs, handed to the AML task.
fn sci() {
    let Some(f) = ACPI.lock().fadt else { return };
    for (status, enable) in pm1_regs(&f) {
        let pending = inw(status) & inw(enable);
        if pending != 0 {
            outw(status, pending); // write 1 to clear
            if pending & PWRBTN != 0 {
                *POWER_REASON.lock() = "power button pressed";
                POWER_BUTTON.signal();
            }
        }
    }
    if gpe::sci() {
        aml::WAKE.signal();
    }
}

/// The interrupt line of ACPI events (for the device list), if there is one.
pub fn sci_line() -> Option<u8> {
    ACPI.lock().fadt.filter(|f| f.sci < 16 && f.sci != 2 && (f.pm1a_evt != 0 || f.gpe0_blk != 0)).map(|f| f.sci as u8)
}

/// Asks the power task to turn the computer off cleanly.
pub(crate) fn request_power_off(reason: &'static str) {
    *POWER_REASON.lock() = reason;
    POWER_BUTTON.signal();
}

/// Switches the chipset to ACPI mode, listens for the power button and the
/// GPEs, and starts the AML task. Pressing the power button turns the
/// computer off cleanly (the file systems are written out first).
pub fn start_events() {
    let Some(f) = ACPI.lock().fadt else { return };
    let Some(line) = sci_line() else {
        crate::debug!("acpi", "no SCI: ACPI events are off");
        return;
    };
    enable_acpi_mode(&f);
    gpe::init(f.gpe0_blk, f.gpe0_len, f.gpe1_blk, f.gpe1_len, f.gpe1_base);
    if f.pm1_evt_len >= 4 {
        let button = if f.fixed_power_button { PWRBTN } else { 0 };
        for (status, enable) in pm1_regs(&f) {
            outw(status, 0xFFFF);
            outw(enable, button);
        }
    }
    crate::dev::irq::attach_kernel(line, sci);
    crate::task::spawn("power", || loop {
        POWER_BUTTON.wait(None);
        let reason = *POWER_REASON.lock();
        crate::info!("acpi", "{reason}: turning off");
        power_off();
    });
    crate::debug!("acpi", "SCI on line {line}, {} GPEs", gpe::count());
    aml::start();
}

/// Writes SLP_TYP and SLP_EN: the chipset enters the sleep state.
fn enter_sleep(f: &Fadt, (a, b): (u16, u16)) {
    // Wake events that are pending would wake the computer at once.
    for (status, _) in pm1_regs(f) {
        outw(status, 0xFFFF);
    }
    gpe::disable_all();
    for (reg, typ) in [(f.pm1a_cnt, a), (f.pm1b_cnt, b)] {
        if reg != 0 && reg <= 0xFFFF {
            let v = inw(reg as u16) & !(7 << 10 | 1 << 13);
            outw(reg as u16, v | (typ & 7) << 10);
        }
    }
    for (reg, typ) in [(f.pm1a_cnt, a), (f.pm1b_cnt, b)] {
        if reg != 0 && reg <= 0xFFFF {
            let v = inw(reg as u16) & !(7 << 10 | 1 << 13);
            outw(reg as u16, v | (typ & 7) << 10 | 1 << 13);
        }
    }
}

/// Turns the machine off. Returns only if every method failed.
pub fn power_off() {
    crate::fs::sync_all();
    // The firmware prepares (_PTS: lights, fans, the embedded controller)
    // and says which sleep type S5 is.
    let from_aml = aml::with(3000, |aml| {
        if aml.find("\\_PTS").is_some() {
            let _ = aml.evaluate("\\_PTS", &[arcaml::Value::Integer(5)]);
        }
        aml.sleep_type(5)
    })
    .flatten();
    cpu::disable_irq();
    let fadt = ACPI.lock().fadt;
    if let Some(f) = fadt {
        if f.pm1a_cnt != 0 {
            enable_acpi_mode(&f);
            let slp = from_aml.map(|(a, b)| (a as u16, b as u16)).or_else(|| s5_sleep_type(f.dsdt));
            if let Some(slp) = slp {
                enter_sleep(&f, slp);
            }
        }
    }
    // Emulators: QEMU (new and old), VirtualBox.
    outw(0x604, 0x2000);
    outw(0xB004, 0x2000);
    outw(0x4004, 0x3400);
    // Power-off takes effect asynchronously; give it a moment.
    crate::time::spin_us(500_000);
}

/// Restarts the machine.
pub fn reboot() -> ! {
    crate::fs::sync_all();
    cpu::disable_irq();
    let fadt = ACPI.lock().fadt;
    if let Some(f) = fadt {
        if f.reset_supported && f.reset_addr != 0 {
            match f.reset_space {
                1 => outb(f.reset_addr as u16, f.reset_value),
                0 => {
                    if let Some(v) = crate::mem::region::map_device(f.reset_addr, 1, crate::mem::paging::NO_CACHE) {
                        // SAFETY: the FADT names this register for resets.
                        unsafe { (v as *mut u8).write_volatile(f.reset_value) };
                    }
                }
                _ => {}
            }
            crate::time::spin_us(50_000);
        }
    }
    // The keyboard controller's reset line.
    for _ in 0..10 {
        if cpu::inb(0x64) & 2 == 0 {
            break;
        }
    }
    outb(0x64, 0xFE);
    crate::time::spin_us(50_000);
    // Triple fault: an empty IDT and an exception.
    #[repr(C, packed)]
    struct Empty {
        limit: u16,
        base: u64,
    }
    let empty = Empty { limit: 0, base: 0 };
    // SAFETY: deliberately crashes the CPU into a reset.
    unsafe { core::arch::asm!("lidt [{}]", "int3", in(reg) &empty) };
    cpu::halt_forever();
}

//! Sleep (suspend to RAM): the whole machine stops with its memory kept and
//! starts again, a second later, where it was.
//!
//! Going to sleep, each step undone if a later one fails:
//! 1. checks: the firmware offers S3 and every device's driver can follow
//!    (see `dev::pm`);
//! 2. the file systems are written out, in case the battery runs out;
//! 3. the firmware is told (`_TTS`), disk transfers are held back, the PCI
//!    configuration is saved and the drivers stop their devices;
//! 4. the devices that may wake the computer are armed and the firmware
//!    prepares (`_PTS`); the other processors stop;
//! 5. the chipset cuts the power (see `acpi::sleep`).
//!
//! Waking up, on the first processor with interrupts off: the clock, the
//! PCI configuration (the chipset's included), the interrupt controllers,
//! the system timer, the serial port and the chipset's events; then with
//! interrupts on: message interrupts, the other processors, the PS/2
//! controller, the drivers' devices, the held-back I/O, `_WAK`, a redraw.

use crate::acpi::sleep::{self as acpi_sleep, Outcome};
use crate::arch::{apic, cpu, pic, pit, smp};
use crate::dev::{self, pm};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// A sleep is under way.
static BUSY: AtomicBool = AtomicBool::new(false);
/// How many times the machine slept, and when it last woke up.
static COUNT: AtomicU64 = AtomicU64::new(0);

/// Whether the machine can sleep now: Err with the reasons if not.
pub fn available() -> Result<(), String> {
    acpi_sleep::supported().map_err(String::from)?;
    let unready = pm::unready();
    if !unready.is_empty() {
        return Err(format!("these devices cannot sleep: {}", unready.join("; ")));
    }
    Ok(())
}

/// How many times the machine slept since it started.
pub fn count() -> u64 {
    COUNT.load(Ordering::Relaxed)
}

/// Puts the machine to sleep until a wake event (power button, lid,
/// keyboard on some machines) or, with `wake_after`, until that many
/// seconds have passed. Returns the seconds it slept.
pub fn suspend(wake_after: Option<u64>) -> Result<u64, String> {
    if BUSY.swap(true, Ordering::AcqRel) {
        return Err(String::from("the computer is already going to sleep"));
    }
    let r = run(wake_after);
    BUSY.store(false, Ordering::Release);
    if let Err(e) = &r {
        crate::warn!("sleep", "{e}");
    }
    r
}

/// Sleep asked for by an event (sleep button, lid, the menu): on a thread
/// of its own, so the caller goes on.
pub fn request(reason: &'static str) {
    if BUSY.load(Ordering::Acquire) {
        return;
    }
    crate::task::spawn("sleep", move || {
        crate::info!("sleep", "{reason}");
        if let Err(e) = suspend(None) {
            crate::gui::notify("Sospensione non riuscita", &e, "about");
        }
    });
}

fn run(wake_after: Option<u64>) -> Result<u64, String> {
    available()?;
    crate::info!("sleep", "going to sleep");
    if crate::gui::active() {
        // The lock screen goes up first (if there is a password), so it
        // is what the screen shows on waking up.
        crate::gui::before_sleep();
        crate::task::sleep_ms(300);
    }
    crate::fs::sync_all();
    acpi_sleep::will_sleep();
    pm::pause_io();
    let mut pci: Vec<u32> = dev::list().iter().filter(|d| d.info.bus == justdk::BUS_PCI).map(|d| d.info.location).collect();
    pci.sort_unstable();
    pci.dedup();
    let saved = dev::pci::save_config(&pci);
    if let Err(e) = pm::suspend_devices() {
        pm::resume_io();
        acpi_sleep::finish();
        return Err(e);
    }
    let plan = match acpi_sleep::prepare() {
        Ok(p) => p,
        Err(e) => {
            wake_devices();
            acpi_sleep::finish();
            return Err(String::from(e));
        }
    };
    if !smp::park() {
        wake_devices();
        acpi_sleep::finish();
        return Err(String::from("a processor did not stop"));
    }
    pic::suspend();
    if let Some(secs) = wake_after {
        crate::time::set_alarm(secs);
    }
    let outcome = acpi_sleep::enter(&plan, wake_after.is_some());

    // Interrupts are off; only this processor runs.
    let slept = crate::time::resync();
    if wake_after.is_some() {
        crate::time::clear_alarm();
    }
    dev::pci::restore_config(&saved);
    apic::resume();
    pic::resume();
    pit::init();
    crate::console::serial::resume();
    acpi_sleep::after_wake(&plan);
    cpu::enable_irq();

    dev::irq::restore_msi();
    if outcome == Outcome::Slept {
        smp::resume();
    } else {
        smp::unpark();
    }
    dev::platform::resume_ps2();
    wake_devices();
    acpi_sleep::finish();
    dev::display::after_sleep();
    crate::gui::after_sleep();
    match outcome {
        Outcome::Slept => {
            COUNT.fetch_add(1, Ordering::Relaxed);
            crate::info!("sleep", "awake after {slept} s");
            Ok(slept)
        }
        Outcome::WokeAtOnce => Err(String::from("the computer woke up at once (a wake event was waiting)")),
        Outcome::Refused => Err(String::from("the firmware did not put the computer to sleep")),
    }
}

/// The drivers start their devices again and the held-back I/O goes on.
fn wake_devices() {
    pm::resume_devices();
    pm::resume_io();
}

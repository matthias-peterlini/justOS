//! Devices across sleep (suspend to RAM).
//!
//! While the machine sleeps every device loses power. Drivers say how their
//! devices stop and start again with [`PowerHooks`] (see
//! `KernelApi::power_hooks`); the kernel restores the PCI configuration
//! itself (see [`super::pci::save_config`]) and holds back the I/O that
//! would reach a device while it is down: disk transfers wait, network
//! frames are dropped, sound waits.

use crate::sync::IrqLock;
use crate::task::WaitQueue;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use arcdk::{PowerHooks, Status, E_INVAL, OK};
use core::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy)]
struct Entry {
    device: u32,
    hooks: PowerHooks,
}

static HOOKS: IrqLock<Vec<Entry>> = IrqLock::new(Vec::new());
/// Set from before the devices stop until after they run again.
static ASLEEP: AtomicBool = AtomicBool::new(false);
static AWAKE: WaitQueue = WaitQueue::new();

/// A driver's hooks for one of its devices (replacing earlier ones).
pub fn register(device: u32, hooks: PowerHooks) -> Status {
    if super::get(device).is_none() {
        return E_INVAL;
    }
    let mut h = HOOKS.lock();
    h.retain(|e| e.device != device);
    h.push(Entry { device, hooks });
    OK
}

/// The device's driver let go of it.
pub fn forget(device: u32) {
    HOOKS.lock().retain(|e| e.device != device);
}

/// The devices whose drivers cannot follow the machine into sleep, as
/// "description (location, driver)". Devices found by drivers (USB
/// devices) are their parent's business.
pub fn unready() -> Vec<String> {
    let hooked: Vec<u32> = HOOKS.lock().iter().map(|e| e.device).collect();
    super::list()
        .into_iter()
        .filter(|d| d.info.parent == 0 && !hooked.contains(&d.info.id))
        .filter_map(|d| d.driver.as_ref().map(|drv| format!("{} ({}, {drv})", d.description(), d.location())))
        .collect()
}

fn describe(device: u32) -> String {
    super::get(device).map(|d| format!("{} ({})", d.description(), d.location())).unwrap_or_else(|| format!("device {device}"))
}

/// Stops every device with hooks, the ones found last (children) first.
/// If one refuses, those already stopped start again and the error says
/// which device refused.
pub fn suspend_devices() -> Result<(), String> {
    let mut list: Vec<Entry> = HOOKS.lock().clone();
    list.sort_by_key(|e| core::cmp::Reverse(e.device));
    for (i, e) in list.iter().enumerate() {
        let Some(suspend) = e.hooks.suspend else { continue };
        // SAFETY: the driver's own hook with its own context.
        let st = unsafe { suspend(e.hooks.ctx) };
        if st != OK {
            for done in list[..i].iter().rev() {
                // SAFETY: as above.
                unsafe { (done.hooks.resume)(done.hooks.ctx) };
            }
            return Err(format!("{} cannot stop: {}", describe(e.device), arcdk::status_name(st)));
        }
    }
    Ok(())
}

/// Starts every device with hooks again, parents first.
pub fn resume_devices() {
    let mut list: Vec<Entry> = HOOKS.lock().clone();
    list.sort_by_key(|e| e.device);
    for e in list {
        let started = crate::time::uptime_ms();
        // SAFETY: the driver's own hook with its own context.
        let st = unsafe { (e.hooks.resume)(e.hooks.ctx) };
        if st != OK {
            crate::warn!("sleep", "{} did not wake up: {}", describe(e.device), arcdk::status_name(st));
        } else if crate::time::uptime_ms() - started > 500 {
            crate::debug!("sleep", "{} took {} ms to wake up", describe(e.device), crate::time::uptime_ms() - started);
        }
    }
}

/// Holds back device I/O and waits for the disk transfers under way.
pub fn pause_io() {
    ASLEEP.store(true, Ordering::Release);
    for d in super::block::list() {
        d.wait_idle();
    }
}

/// Lets device I/O through again.
pub fn resume_io() {
    ASLEEP.store(false, Ordering::Release);
    AWAKE.notify_all();
}

/// The devices are up (not on the way into or out of sleep).
pub fn awake() -> bool {
    !ASLEEP.load(Ordering::Acquire)
}

/// Waits until the devices are up.
pub fn wait_awake() {
    if awake() {
        return;
    }
    AWAKE.wait_for(|| awake().then_some(()));
}

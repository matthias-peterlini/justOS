//! Display controllers registered by drivers: the screen resolutions they
//! offer and switching between them.
//!
//! Without a driver the screen stays in the mode the loader chose. With one,
//! the graphical interface can pick another mode: the driver programs the
//! controller and describes the new framebuffer, which replaces the old one.

use crate::console::FbInfo;
use crate::sync::IrqLock;
use alloc::string::String;
use alloc::vec::Vec;
use justdk::{DisplayDesc, DisplayMode, Framebuffer, Status};

struct Controller {
    device: u32,
    name: String,
    desc: *const DisplayDesc,
}

// SAFETY: the descriptor belongs to the driver; calls come from the
// interface thread (and the shell), one at a time.
unsafe impl Send for Controller {}

static CONTROLLERS: IrqLock<Vec<Option<Controller>>> = IrqLock::new(Vec::new());
/// The controller whose framebuffer is on the screen, if it needs to be told
/// about changes.
static FLUSH: IrqLock<Option<DescPtr>> = IrqLock::new(None);

#[derive(Clone, Copy, PartialEq, Eq)]
struct DescPtr(*const DisplayDesc);
// SAFETY: see Controller.
unsafe impl Send for DescPtr {}

pub fn register(device: u32, desc: *const DisplayDesc) -> Status {
    // SAFETY: the driver keeps the descriptor alive.
    let d = unsafe { &*desc };
    let len = d.name.iter().position(|&b| b == 0).unwrap_or(d.name.len());
    let name = String::from(core::str::from_utf8(&d.name[..len]).unwrap_or("display"));
    let mut c = CONTROLLERS.lock();
    let n = c.len();
    crate::info!("display", "display{n}: {name}");
    c.push(Some(Controller { device, name, desc }));
    n as Status
}

pub fn remove_device(device: u32) {
    let mut c = CONTROLLERS.lock();
    for slot in c.iter_mut() {
        if let Some(ctl) = slot.as_ref().filter(|ctl| ctl.device == device) {
            let mut f = FLUSH.lock();
            if *f == Some(DescPtr(ctl.desc)) {
                *f = None;
            }
            *slot = None;
        }
    }
}

fn first() -> Option<(*const DisplayDesc, String)> {
    CONTROLLERS.lock().iter().flatten().next().map(|c| (c.desc, c.name.clone()))
}

/// Name of the display controller, if a driver handles it.
pub fn controller() -> Option<String> {
    first().map(|(_, n)| n)
}

/// The modes the controller offers, largest first.
pub fn modes() -> Vec<DisplayMode> {
    let Some((desc, _)) = first() else { return Vec::new() };
    let mut out = alloc::vec![DisplayMode::default(); 64];
    // SAFETY: the driver's own function with a buffer of 64 modes.
    let n = unsafe { ((*desc).modes)((*desc).ctx, out.as_mut_ptr(), out.len()) }.min(out.len());
    out.truncate(n);
    out.retain(|m| m.width >= 640 && m.height >= 480);
    out.sort_by_key(|m| core::cmp::Reverse((m.width * m.height, m.width)));
    out.dedup();
    out
}

/// Switches the screen to `mode`. Returns the new framebuffer, already
/// installed for the console.
pub fn set_mode(mode: DisplayMode) -> Result<FbInfo, &'static str> {
    let (desc, _) = first().ok_or("no display driver")?;
    if !modes().contains(&mode) {
        return Err("the display does not offer this resolution");
    }
    let mut fb = Framebuffer::default();
    // SAFETY: the driver's own function with a valid out pointer.
    let status = unsafe { ((*desc).set_mode)((*desc).ctx, mode, &mut fb) };
    if status < 0 || fb.width < 640 || fb.height < 480 || fb.pitch < fb.width * 4 {
        return Err("the display refused the resolution");
    }
    let virt = if fb.virt != 0 {
        fb.virt
    } else {
        let size = fb.pitch as u64 * fb.height as u64;
        crate::mem::region::map_device(fb.phys, size, crate::mem::paging::WRITE_COMBINE).ok_or("cannot map the framebuffer")?
    };
    let info =
        FbInfo { virt, width: fb.width as usize, height: fb.height as usize, pitch: fb.pitch as usize, shift: (fb.red, fb.green, fb.blue) };
    // SAFETY: the driver's descriptor stays valid while it is registered.
    *FLUSH.lock() = unsafe { (*desc).flush.is_some() }.then_some(DescPtr(desc));
    crate::console::set_framebuffer(info);
    DRIVER_MODE.store(true, core::sync::atomic::Ordering::Release);
    crate::info!("display", "{}x{}", fb.width, fb.height);
    Ok(info)
}

/// A driver set the mode (the screen is no longer in the firmware's).
static DRIVER_MODE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// After sleep. Drivers set their own mode again, but not one the firmware
/// chose at boot, and the firmware does not set it up again on waking: the
/// driver is asked for the same resolution.
pub fn after_sleep() {
    if DRIVER_MODE.load(core::sync::atomic::Ordering::Acquire) || first().is_none() {
        return;
    }
    let Some((w, h)) = crate::console::pixel_size() else { return };
    let mode = DisplayMode { width: w as u32, height: h as u32 };
    if let Err(e) = set_mode(mode) {
        crate::warn!("display", "{w}x{h} after sleep: {e}");
    }
}

/// Tells the controller that a rectangle of the screen changed (only
/// controllers that need it).
pub fn flush(x: i32, y: i32, w: i32, h: i32) {
    let Some(DescPtr(desc)) = *FLUSH.lock() else { return };
    if w <= 0 || h <= 0 || !super::pm::awake() {
        return;
    }
    // SAFETY: the driver's own function, while it is registered.
    unsafe {
        if let Some(f) = (*desc).flush {
            f((*desc).ctx, x.max(0) as u32, y.max(0) as u32, w as u32, h as u32);
        }
    }
}

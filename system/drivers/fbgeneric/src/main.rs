//! The display driver for every graphics card without a driver of its own:
//! Intel, AMD and NVIDIA cards on real computers, Hyper-V, ASPEED and Matrox
//! server chips, and any other PCI display controller.
//!
//! It keeps the framebuffer the firmware (UEFI GOP or VESA) set up before
//! justOS started, at the resolution the loader picked; without a native
//! driver the card cannot change it. What the driver adds is the card itself:
//! the screen belongs to a named device, so Settings and `display` show the
//! real card, and the screen keeps working when a better driver is missing.
//!
//! It matches the whole display class with the lowest score, so any specific
//! driver (bochsvga, vmsvga, virtiogpu, cirrus) wins when there is one.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::format;
use justdrv::{DeviceInfo, DisplayDesc, DisplayMode, Framebuffer, Status, E_INVAL, E_NODEV, OK, RES_MEMORY};
use core::ffi::c_void;

justdrv::driver! {
    name: "fbgeneric",
    version: "1.0.0",
    probe: probe,
}

fn vendor_name(vendor: u16) -> &'static str {
    match vendor {
        0x8086 => "Intel",
        0x1002 => "AMD",
        0x10DE => "NVIDIA",
        0x102B => "Matrox",
        0x1A03 => "ASPEED",
        0x1414 => "Microsoft Hyper-V",
        0x15AD => "VMware",
        0x80EE => "VirtualBox",
        0x1234 => "QEMU",
        0x1AF4 => "virtio",
        0x1B36 => "Red Hat",
        0x5333 => "S3",
        0x1013 => "Cirrus Logic",
        0x18CA => "XGI",
        0x1039 => "SiS",
        0x126F => "Silicon Motion",
        0x5143 | 0x17CB => "Qualcomm",
        0x1EB1 => "VeriSilicon",
        0x1E50 | 0x1D17 => "Zhaoxin",
        0x1ED5 => "Moore Threads",
        _ => "",
    }
}

unsafe extern "C" fn modes(ctx: *mut c_void, out: *mut DisplayMode, max: usize) -> usize {
    // SAFETY: ctx is the leaked framebuffer description.
    let fb = unsafe { &*(ctx as *const Framebuffer) };
    if max == 0 {
        return 0;
    }
    // SAFETY: out holds at least one mode.
    unsafe { out.write(DisplayMode { width: fb.width, height: fb.height }) };
    1
}

unsafe extern "C" fn set_mode(ctx: *mut c_void, mode: DisplayMode, out: *mut Framebuffer) -> Status {
    // SAFETY: ctx is the leaked description; out is valid for the call.
    let fb = unsafe { &*(ctx as *const Framebuffer) };
    if (mode.width, mode.height) != (fb.width, fb.height) {
        return E_INVAL;
    }
    // SAFETY: as above.
    unsafe { *out = *fb };
    OK
}

fn probe(dev: &DeviceInfo) -> Status {
    let Some(fb) = justdrv::boot_framebuffer() else {
        justdrv::info!("the firmware left no framebuffer");
        return E_NODEV;
    };
    // With two cards (a laptop's integrated and dedicated graphics), only
    // the one whose memory holds the framebuffer is driving the screen.
    let size = fb.pitch as u64 * fb.height as u64;
    let holds = dev.resources.iter().any(|r| r.kind == RES_MEMORY && fb.phys >= r.base && fb.phys + size <= r.base + r.size);
    let has_memory = dev.resources.iter().any(|r| r.kind == RES_MEMORY && r.size >= size);
    if !holds && has_memory {
        justdrv::info!("{:04x}:{:04x} is not the card on the screen", dev.vendor, dev.device);
        return E_NODEV;
    }
    let vendor = vendor_name(dev.vendor);
    let name = if vendor.is_empty() {
        format!("Graphics {:04x}:{:04x}", dev.vendor, dev.device)
    } else {
        format!("{vendor} graphics {:04x}", dev.device)
    };
    justdrv::info!("{name}: firmware framebuffer {}x{}", fb.width, fb.height);
    let fb: &'static Framebuffer = Box::leak(Box::new(fb));
    let desc: &'static DisplayDesc = Box::leak(Box::new(DisplayDesc {
        ctx: fb as *const Framebuffer as *mut c_void,
        modes,
        set_mode,
        flush: None,
        name: justdrv::model_name(&name),
    }));
    let n = justdrv::display_register(dev.id, desc);
    if n < 0 {
        return n;
    }
    OK
}

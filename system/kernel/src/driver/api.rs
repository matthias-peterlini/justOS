//! The [`KernelApi`] table handed to every driver module.

use crate::dev;
use crate::mem::{self, frame, paging, region, PAGE_SIZE};
use alloc::alloc::Layout;
use arcdk::*;
use core::ffi::c_void;

unsafe extern "C" fn api_log(level: u32, msg: *const u8, len: usize) {
    // SAFETY: the module passes a valid buffer.
    let bytes = unsafe { core::slice::from_raw_parts(msg, len.min(1024)) };
    let text = core::str::from_utf8(bytes).unwrap_or("<invalid UTF-8>");
    let (source, text) = text.split_once(": ").unwrap_or(("driver", text));
    let level = match level {
        LOG_ERROR => crate::console::LEVEL_ERROR,
        LOG_WARN => crate::console::LEVEL_WARN,
        LOG_INFO => crate::console::LEVEL_INFO,
        _ => crate::console::LEVEL_DEBUG,
    };
    crate::console::log(level, source, format_args!("{text}"));
}

unsafe extern "C" fn api_alloc(size: usize, align: usize) -> *mut u8 {
    match Layout::from_size_align(size.max(1), align.max(1)) {
        // SAFETY: non-zero size.
        Ok(l) => unsafe { alloc::alloc::alloc(l) },
        Err(_) => core::ptr::null_mut(),
    }
}

unsafe extern "C" fn api_free(ptr: *mut u8, size: usize, align: usize) {
    if ptr.is_null() {
        return;
    }
    if let Ok(l) = Layout::from_size_align(size.max(1), align.max(1)) {
        // SAFETY: the module frees what api_alloc returned with this layout.
        unsafe { alloc::alloc::dealloc(ptr, l) };
    }
}

unsafe extern "C" fn api_map_mmio(phys: u64, size: usize) -> *mut u8 {
    region::map_device(phys, size as u64, paging::NO_CACHE).map(|v| v as *mut u8).unwrap_or(core::ptr::null_mut())
}

unsafe extern "C" fn api_dma_alloc(size: usize, phys: *mut u64) -> *mut u8 {
    let pages = mem::align_up(size.max(1) as u64, PAGE_SIZE) / PAGE_SIZE;
    match frame::alloc_contiguous(pages, 4 << 30) {
        Some(p) => {
            if !phys.is_null() {
                // SAFETY: the module passes a valid out pointer.
                unsafe { *phys = p };
            }
            mem::phys_to_virt(p) as *mut u8
        }
        None => core::ptr::null_mut(),
    }
}

unsafe extern "C" fn api_dma_free(ptr: *mut u8, size: usize) {
    if ptr.is_null() {
        return;
    }
    let pages = mem::align_up(size.max(1) as u64, PAGE_SIZE) / PAGE_SIZE;
    frame::free_contiguous(ptr as u64 - mem::HHDM, pages);
}

unsafe extern "C" fn api_virt_to_phys(ptr: *const u8) -> u64 {
    mem::virt_to_phys(ptr as u64).unwrap_or(0)
}

fn pci_location(device: u32) -> Option<u32> {
    let info = dev::info(device)?;
    (info.bus == BUS_PCI).then_some(info.location)
}

unsafe extern "C" fn api_pci_read(device: u32, offset: u16, width: u8) -> u32 {
    match pci_location(device) {
        Some(loc) if offset < 256 => dev::pci::read(loc, offset, width),
        _ => u32::MAX,
    }
}

unsafe extern "C" fn api_pci_write(device: u32, offset: u16, width: u8, value: u32) {
    if let Some(loc) = pci_location(device) {
        if offset < 256 {
            dev::pci::write(loc, offset, width, value);
        }
    }
}

unsafe extern "C" fn api_irq_attach(device: u32, irq: u8, handler: IrqHandler, ctx: *mut c_void) -> Status {
    if dev::irq::attach_driver(device, irq, handler, ctx) {
        OK
    } else {
        E_INVAL
    }
}

unsafe extern "C" fn api_irq_detach(device: u32, irq: u8) {
    dev::irq::detach(device, Some(irq));
}

unsafe extern "C" fn api_time_ms() -> u64 {
    crate::time::uptime_ms()
}

unsafe extern "C" fn api_sleep_ms(ms: u64) {
    crate::task::sleep_ms(ms);
}

unsafe extern "C" fn api_yield() {
    crate::task::yield_now();
}

unsafe extern "C" fn api_spawn(name: *const u8, len: usize, entry: ThreadEntry, ctx: *mut c_void) -> Status {
    // SAFETY: the module passes a valid name.
    let name = unsafe { core::str::from_utf8(core::slice::from_raw_parts(name, len.min(64))).unwrap_or("driver") };
    match crate::task::spawn_raw(name, entry, ctx) {
        Some(tid) => tid as Status,
        None => E_NOMEM,
    }
}

unsafe extern "C" fn api_block_register(device: u32, desc: *const BlockDesc) -> Status {
    if desc.is_null() {
        return E_INVAL;
    }
    dev::block::register(device, desc)
}

unsafe extern "C" fn api_input_event(device: u32, kind: u32, code: u32, value: i32) {
    dev::input::driver_event(device, kind, code, value);
}

unsafe extern "C" fn api_net_register(device: u32, desc: *const NetDesc) -> Status {
    if desc.is_null() {
        return E_INVAL;
    }
    dev::net::register(device, desc)
}

unsafe extern "C" fn api_net_receive(iface: u32, frame: *const u8, len: usize) {
    if frame.is_null() || len > 65536 {
        return;
    }
    // SAFETY: the driver passes a valid frame.
    dev::net::receive(iface, unsafe { core::slice::from_raw_parts(frame, len) });
}

unsafe extern "C" fn api_irq_attach_msi(device: u32, handler: IrqHandler, ctx: *mut c_void) -> Status {
    match dev::irq::attach_msi(device, handler, ctx) {
        Ok(v) => v as Status,
        Err(e) => e,
    }
}

unsafe extern "C" fn api_event_new() -> Event {
    alloc::boxed::Box::into_raw(alloc::boxed::Box::new(crate::task::Event::new())) as Event
}

unsafe extern "C" fn api_event_free(event: Event) {
    if !event.is_null() {
        // SAFETY: created by api_event_new; the module frees it once.
        drop(unsafe { alloc::boxed::Box::from_raw(event as *mut crate::task::Event) });
    }
}

unsafe extern "C" fn api_event_signal(event: Event) {
    // SAFETY: a live event of the module.
    if let Some(e) = unsafe { (event as *const crate::task::Event).as_ref() } {
        e.signal();
    }
}

unsafe extern "C" fn api_event_wait(event: Event, timeout_ms: u64) -> Status {
    // SAFETY: a live event of the module.
    let Some(e) = (unsafe { (event as *const crate::task::Event).as_ref() }) else { return E_INVAL };
    if e.wait((timeout_ms != WAIT_FOREVER).then_some(timeout_ms)) {
        OK
    } else {
        E_TIMEOUT
    }
}

unsafe extern "C" fn api_device_add(parent: u32, info: *const DeviceInfo, description: Str, flags: u32) -> Status {
    if info.is_null() {
        return E_INVAL;
    }
    // SAFETY: the module passes a valid device and string.
    let (info, name) = unsafe { (*info, description.as_str()) };
    let name = (!name.is_empty()).then(|| alloc::string::String::from(&name[..name.len().min(64)]));
    let owned = flags & DEVICE_OWNED != 0;
    match dev::add_child(parent, info, name, owned) {
        Some(id) => {
            if !owned {
                super::device_added();
            }
            id as Status
        }
        None => E_INVAL,
    }
}

unsafe extern "C" fn api_device_remove(device: u32) -> Status {
    super::device_removed(device)
}

unsafe extern "C" fn api_audio_register(device: u32, desc: *const AudioDesc) -> Status {
    if desc.is_null() {
        return E_INVAL;
    }
    dev::audio::register(device, desc)
}

unsafe extern "C" fn api_display_register(device: u32, desc: *const DisplayDesc) -> Status {
    if desc.is_null() {
        return E_INVAL;
    }
    dev::display::register(device, desc)
}

unsafe extern "C" fn api_map_framebuffer(phys: u64, size: usize) -> *mut u8 {
    region::map_device(phys, size as u64, paging::WRITE_COMBINE).map(|v| v as *mut u8).unwrap_or(core::ptr::null_mut())
}

unsafe extern "C" fn api_boot_framebuffer(out: *mut Framebuffer) -> Status {
    let Some((phys, fb)) = crate::console::boot_framebuffer() else { return E_NODEV };
    if out.is_null() {
        return E_INVAL;
    }
    let (red, green, blue) = fb.shift;
    // SAFETY: the driver passes a place for the description.
    unsafe {
        *out = Framebuffer {
            phys,
            virt: fb.virt,
            width: fb.width as u32,
            height: fb.height as u32,
            pitch: fb.pitch as u32,
            red,
            green,
            blue,
            _pad: 0,
        };
    }
    OK
}

unsafe extern "C" fn api_power_hooks(device: u32, hooks: *const PowerHooks) -> Status {
    if hooks.is_null() {
        return E_INVAL;
    }
    // SAFETY: the module passes valid hooks for the call's duration.
    dev::pm::register(device, unsafe { *hooks })
}

pub static KERNEL_API: KernelApi = KernelApi {
    abi_version: ABI_VERSION,
    size: core::mem::size_of::<KernelApi>() as u32,
    log: api_log,
    alloc: api_alloc,
    free: api_free,
    map_mmio: api_map_mmio,
    dma_alloc: api_dma_alloc,
    dma_free: api_dma_free,
    virt_to_phys: api_virt_to_phys,
    pci_read: api_pci_read,
    pci_write: api_pci_write,
    irq_attach: api_irq_attach,
    irq_detach: api_irq_detach,
    time_ms: api_time_ms,
    sleep_ms: api_sleep_ms,
    yield_now: api_yield,
    spawn: api_spawn,
    block_register: api_block_register,
    input_event: api_input_event,
    net_register: api_net_register,
    net_receive: api_net_receive,
    irq_attach_msi: api_irq_attach_msi,
    event_new: api_event_new,
    event_free: api_event_free,
    event_signal: api_event_signal,
    event_wait: api_event_wait,
    device_add: api_device_add,
    device_remove: api_device_remove,
    audio_register: api_audio_register,
    display_register: api_display_register,
    map_framebuffer: api_map_framebuffer,
    boot_framebuffer: api_boot_framebuffer,
    power_hooks: api_power_hooks,
};
